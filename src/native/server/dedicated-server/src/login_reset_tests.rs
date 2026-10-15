//! F-SRV-002: real Users handlers, SQLite, signed claims, and complete native
//! handler cores. Synthetic fixture only; no sockets, listeners or live accounts.
//! Current reset contract rotates credentials/JWT versions; it does not eject native sessions.
use super::*;
use server_api::gameplay_contract::{ACCOUNT_PRODUCT, GAME_INSTANCE_HEADER, GameplayMode};
use sqlx::sqlite::SqlitePool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;
const NAME: &str = "reset-fixture";
const OTHER: &str = "reset-other";
const OLD: &str = "synthetic-old-password";
const NEW: &str = "synthetic-new-password";
const RECOVERY: &str = "AAAAAAAAAAAAAAAAAAAAAAAA";
const NONCE: &str = "11111111111111111111111111111111";
const NEW_NONCE: &str = "22222222222222222222222222222222";
fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap() }
fn logger() -> Logger { Logger::root(slog::Discard, slog::o!()) }
fn config() -> AuthTokenConfig { AuthTokenConfig { release: test_release_identity(), key: hmacsha256::Key([0x52;32]), ttl_seconds: DEFAULT_AUTH_TOKEN_TTL_SECONDS } }
fn service(storage: Arc<Storage>) -> MyUsers { MyUsers { logger: logger(), token_config: config(), storage, overlay_network: LanClientNetwork } }
fn source<T>(value: T) -> Request<T> {
    let mut request = Request::new(value);
    request.extensions_mut().insert(tonic::transport::server::TcpConnectInfo { local_addr: Some("127.0.0.1:50051".parse().unwrap()), remote_addr: Some("127.0.0.1:30500".parse().unwrap()) });
    request
}
fn login_request(name: &str, password: &str, product: &str, nonce: &str) -> Request<users::LoginRequest> {
    let mut request = source(users::LoginRequest { client_product: product.into(), username: name.into(), password: password.into(), package_identity: Some(test_package_identity()) });
    if product != ACCOUNT_PRODUCT { request.metadata_mut().insert(GAME_INSTANCE_HEADER, nonce.parse().unwrap()); }
    request
}
fn reset_request() -> Request<users::ResetPasswordRequest> {
    Request::new(users::ResetPasswordRequest { username: NAME.into(), recovery_code: RECOVERY.into(), new_password: NEW.into() })
}
async fn seed(storage: &Storage, pool: &SqlitePool, hashed: bool) {
    if hashed {
        storage.register_user_with_recovery_async(NAME, OLD, Some("reset-account"), Some(&hash_recovery_code(RECOVERY))).await.unwrap();
    } else {
        sqlx::query("INSERT INTO users(id,username,password,ubi_id,recovery_code_hash) VALUES(1000,?,?,?,?)")
            .bind(NAME).bind(OLD).bind("reset-account").bind(hash_recovery_code(RECOVERY)).execute(pool).await.unwrap();
    }
    sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(1001,?,?,?)")
        .bind(OTHER).bind(OLD).bind("other-account").execute(pool).await.unwrap();
}
async fn counts(pool: &SqlitePool, user: u32) -> (i64,i64,i64) {
    let active = sqlx::query_scalar("SELECT COUNT(*) FROM game_runs WHERE user_id=? AND retired_at IS NULL").bind(user).fetch_one(pool).await.unwrap();
    let retired = sqlx::query_scalar("SELECT COUNT(*) FROM game_runs WHERE user_id=? AND retired_at IS NOT NULL").bind(user).fetch_one(pool).await.unwrap();
    let tickets = sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE user_id=?").bind(user).fetch_one(pool).await.unwrap();
    (active,retired,tickets)
}
async fn token_valid(storage: &Arc<Storage>, token: &str) -> bool {
    let mut request = source(()); request.metadata_mut().insert("authorization", token.parse().unwrap());
    check_token(&logger(), &config(), storage, LanClientNetwork, request).await.is_ok()
}
fn pause(storage: &Storage, stage: &'static str) -> (Receiver<()>, SyncSender<()>) {
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    storage.set_login_checkpoint(stage, Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(60)).expect("test controller did not release credential checkpoint");
    }));
    (entered_rx, release_tx)
}
fn ticket(storage: Arc<Storage>, method: &str, name: &str, password: &str, user: u32, bound: Option<u32>) -> (quazal::rmc::Result<()>, Option<u32>) {
    crate::ticket::maintenance_ticket_core(storage, method, name, password, user, bound)
}

#[test]
fn fsrv002_api_reset_between_snapshot_verification_and_publication() {
    let mut failures = Vec::new();
    for hashed in [false, true] {
        for stage in ["snapshot", "verified", "publication"] {
            for product in [ACCOUNT_PRODUCT, GameplayMode::Official.product(), GameplayMode::Mod.product()] {
                let (storage,pool) = crate::storage::release_test_support::fixture();
                runtime().block_on(seed(&storage,&pool,hashed));
                let (entered,release) = pause(&storage,stage);
                let login_storage = storage.clone();
                let product_owned = product.to_string();
                let worker = std::thread::spawn(move || runtime().block_on(service(login_storage).login(login_request(NAME,OLD,&product_owned,NONCE))));
                entered.recv_timeout(Duration::from_secs(60)).expect("real handler did not reach checkpoint");
                let reset = runtime().block_on(service(storage.clone()).reset_password(reset_request()));
                release.send(()).unwrap();
                let result = worker.join().unwrap();
                assert!(reset.is_ok(), "real reset failed: {reset:?}");
                let observed = runtime().block_on(counts(&pool,1000));
                let rejected = matches!(&result, Err(status) if status.code() == tonic::Code::Unauthenticated);
                let usable = result.as_ref().ok().map(|response| runtime().block_on(token_valid(&storage,&response.get_ref().token))).unwrap_or(false);
                println!("FSRV002 API hashed={hashed} stage={stage} product={product} rejected={rejected} usable_token={usable} state={observed:?}");
                if !rejected || usable || observed != (0,0,0) { failures.push(format!("{hashed}/{stage}/{product}")); }
            }
        }
    }
    assert!(failures.is_empty(), "stale credential API publications: {failures:?}");
}

#[test]
fn fsrv002_native_old_snapshot_cannot_borrow_new_password_run() {
    let mut failures = Vec::new();
    for hashed in [false,true] {
        for stage in ["snapshot","verified","publication"] {
            let (storage,pool) = crate::storage::release_test_support::fixture();
            runtime().block_on(async {
                seed(&storage,&pool,hashed).await;
                service(storage.clone()).login(login_request(NAME,OLD,GameplayMode::Mod.product(),NONCE)).await.unwrap();
            });
            let (entered,release) = pause(&storage,stage);
            let worker_storage = storage.clone();
            let worker = std::thread::spawn(move || ticket(worker_storage,"LoginEx",NAME,OLD,1000,Some(1001)));
            entered.recv_timeout(Duration::from_secs(60)).expect("native credential flow did not reach checkpoint");
            let reset = runtime().block_on(service(storage.clone()).reset_password(reset_request()));
            // Install a new, legitimately authenticated run before the stale
            // LoginEx continues. Merely checking any current run is insufficient.
            let new_login = runtime().block_on(service(storage.clone()).login(login_request(NAME,NEW,GameplayMode::Mod.product(),NEW_NONCE)));
            release.send(()).unwrap();
            let (result,bound) = worker.join().unwrap();
            assert!(reset.is_ok()); assert!(new_login.is_ok());
            let observed = runtime().block_on(counts(&pool,1000));
            let rejected = matches!(result,Err(quazal::rmc::Error::AccessDenied));
            println!("FSRV002 NATIVE hashed={hashed} stage={stage} rejected={rejected} preserved_binding={} state={observed:?}",bound==Some(1001));
            if !rejected || bound != Some(1001) || observed != (1,1,0) { failures.push(format!("{hashed}/{stage}")); }
            let accepted = ticket(storage,"LoginEx",NAME,NEW,1000,None);
            assert!(accepted.0.is_ok()); assert_eq!(accepted.1,Some(1000));
        }
    }
    assert!(failures.is_empty(),"stale credential native publications: {failures:?}");
}

#[test]
fn fsrv002_reset_before_auth_rejects_old_and_normal_new_password_succeeds() {
    let (storage,pool) = crate::storage::release_test_support::fixture();
    runtime().block_on(async {
        seed(&storage,&pool,true).await;
        let users = service(storage.clone());
        users.reset_password(reset_request()).await.unwrap();
        let replay = users.reset_password(reset_request()).await.unwrap_err();
        assert_eq!(replay.code(),tonic::Code::Unauthenticated);
        for product in [ACCOUNT_PRODUCT,GameplayMode::Official.product(),GameplayMode::Mod.product()] {
            assert_eq!(users.login(login_request(NAME,OLD,product,NONCE)).await.unwrap_err().code(),tonic::Code::Unauthenticated);
        }
        assert_eq!(counts(&pool,1000).await,(0,0,0));
        let account = users.login(login_request(NAME,NEW,ACCOUNT_PRODUCT,NONCE)).await.unwrap().into_inner();
        assert!(token_valid(&storage,&account.token).await);
        assert_eq!(counts(&pool,1000).await,(0,0,0));
        users.login(login_request(NAME,NEW,GameplayMode::Mod.product(),NEW_NONCE)).await.unwrap();
    });
    for method in ["Login","LoginEx","RequestTicket"] {
        let (result,bound) = ticket(storage.clone(),method,NAME,NEW,1000,Some(1000));
        assert!(result.is_ok(),"ordinary new-password {method} failed"); assert_eq!(bound,Some(1000));
    }
    assert_eq!(runtime().block_on(counts(&pool,1000)),(1,0,3));
    println!("FSRV002 BEFORE old_rejected=true replay_rejected=true new_account_game_native_success=true");
}

#[test]
fn fsrv002_reset_after_publication_preserves_native_state_and_invalidates_only_old_jwt() {
    let (storage,pool) = crate::storage::release_test_support::fixture();
    let (old_token,other_token) = runtime().block_on(async {
        seed(&storage,&pool,false).await;
        let users = service(storage.clone());
        let old = users.login(login_request(NAME,OLD,GameplayMode::Mod.product(),NONCE)).await.unwrap().into_inner().token;
        let other = users.login(login_request(OTHER,OLD,GameplayMode::Mod.product(),NONCE)).await.unwrap().into_inner().token;
        (old,other)
    });
    assert!(ticket(storage.clone(),"LoginEx",NAME,OLD,1000,None).0.is_ok());
    assert!(ticket(storage.clone(),"LoginEx",OTHER,OLD,1001,None).0.is_ok());
    runtime().block_on(async {
        service(storage.clone()).reset_password(reset_request()).await.unwrap();
        assert!(!token_valid(&storage,&old_token).await);
        assert!(token_valid(&storage,&other_token).await);
        assert_eq!(counts(&pool,1000).await,(1,0,1));
        assert_eq!(counts(&pool,1001).await,(1,0,1));
        assert!(service(storage.clone()).login(login_request(NAME,OLD,ACCOUNT_PRODUCT,NONCE)).await.is_err());
        service(storage.clone()).login(login_request(NAME,NEW,GameplayMode::Mod.product(),NONCE)).await.unwrap();
    });
    assert!(ticket(storage.clone(),"LoginEx",NAME,OLD,1000,None).0.is_err());
    for method in ["Login","LoginEx","RequestTicket"] {
        assert!(ticket(storage.clone(),method,NAME,NEW,1000,Some(1000)).0.is_ok());
    }
    println!("FSRV002 AFTER old_jwt_invalid=true native_state_preserved=true other_unchanged=true new_password_ok=true");
}

#[test]
fn fsrv002_reset_update_failure_keeps_credentials_and_allows_same_code_retry() {
    let (storage,pool) = crate::storage::release_test_support::fixture();
    runtime().block_on(async {
        seed(&storage,&pool,true).await;
        let users = service(storage.clone());
        let token=users.login(login_request(NAME,OLD,GameplayMode::Mod.product(),NONCE)).await.unwrap().into_inner().token;
        sqlx::query("CREATE TRIGGER reset_fixture_abort BEFORE UPDATE OF auth_version ON users WHEN NEW.id=1000 BEGIN SELECT RAISE(ABORT,'owned reset fixture failure'); END").execute(&pool).await.unwrap();
        assert_eq!(users.reset_password(reset_request()).await.unwrap_err().code(),tonic::Code::Internal);
        assert!(token_valid(&storage,&token).await);
        assert!(users.login(login_request(NAME,OLD,ACCOUNT_PRODUCT,NONCE)).await.is_ok());
        assert!(users.login(login_request(NAME,NEW,ACCOUNT_PRODUCT,NONCE)).await.is_err());
        assert_eq!(counts(&pool,1000).await,(1,0,0));
        sqlx::query("DROP TRIGGER reset_fixture_abort").execute(&pool).await.unwrap();
        users.reset_password(reset_request()).await.unwrap();
        assert!(!token_valid(&storage,&token).await);
        assert!(users.login(login_request(NAME,NEW,ACCOUNT_PRODUCT,NONCE)).await.is_ok());
        assert!(users.reset_password(reset_request()).await.is_err());
    });
    println!("FSRV002 UPDATE injected_failure_keeps_credentials=true same_code_retry=true replay_rejected=true");
}

include!("LIFECYCLE-EXTRA.rs");
