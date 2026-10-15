//! In-process real Users handlers, real SQLite transactions, real signed claims.
//! Only synthetic fixtures; never starts a listener or launches the game.
use super::*;
use server_api::gameplay_contract::{ACCOUNT_PRODUCT, GAME_INSTANCE_HEADER, GameplayMode};
use sqlx::sqlite::SqlitePool;
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const OLD_TIME: &str = "2000-01-01 00:00:00";
fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap() }
fn config() -> AuthTokenConfig { AuthTokenConfig {release:test_release_identity(),key:hmacsha256::Key([0x52;32]),ttl_seconds:DEFAULT_AUTH_TOKEN_TTL_SECONDS} }
fn logger() -> Logger { Logger::root(slog::Discard,slog::o!()) }
fn source<T>(value:T)->Request<T> {
    let mut r=Request::new(value);
    r.extensions_mut().insert(tonic::transport::server::TcpConnectInfo{local_addr:Some("127.0.0.1:50051".parse().unwrap()),remote_addr:Some("127.0.0.1:30500".parse().unwrap())});r
}
fn login(product:&str,identity:Option<users::PackageIdentity>)->Request<users::LoginRequest> {
    let mut r=source(users::LoginRequest{client_product:product.into(),username:"release-fixture".into(),password:"fixture-only".into(),package_identity:identity});
    if product!=ACCOUNT_PRODUCT { r.metadata_mut().insert(GAME_INSTANCE_HEADER,NONCE.parse().unwrap()); }r
}
async fn seed(pool:&SqlitePool) {
    sqlx::query("INSERT INTO users(id,username,password,ubi_id,last_login) VALUES(1000,'release-fixture','fixture-only','release-account',?)").bind(OLD_TIME).execute(pool).await.unwrap();
}
async fn snapshot(pool:&SqlitePool)->(Option<String>,i64,i64,i64) {
    let last=sqlx::query_scalar("SELECT last_login FROM users WHERE id=1000").fetch_one(pool).await.unwrap();
    let users=sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(pool).await.unwrap();
    let runs=sqlx::query_scalar("SELECT COUNT(*) FROM game_runs").fetch_one(pool).await.unwrap();
    let sessions=sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions").fetch_one(pool).await.unwrap();
    (last,users,runs,sessions)
}
fn service(storage:Arc<Storage>)->MyUsers { MyUsers{logger:logger(),token_config:config(),storage,overlay_network:LanClientNetwork} }

#[test]
fn missing_wrong_and_mixed_release_reject_before_real_credential_and_registration_writes() {
    let (storage,pool)=crate::storage::release_test_support::fixture();
    runtime().block_on(async {
        seed(&pool).await;let users=service(storage);let before=snapshot(&pool).await;
        let mut wrong=test_package_identity();wrong.manifest_sha256="b".repeat(64);
        let mut mixed=test_package_identity();mixed.gameplay_dll=None;
        for (declared,reason) in [(None,"PACKAGE_IDENTITY_REQUIRED"),(Some(wrong),"PACKAGE_CATALOG_MISMATCH"),(Some(mixed),"PACKAGE_MIXED")] {
            for product in [ACCOUNT_PRODUCT,GameplayMode::Official.product(),GameplayMode::Mod.product()] {
                let error=users.login(login(product,declared.clone())).await.unwrap_err();
                assert_eq!(error.code(),tonic::Code::FailedPrecondition);assert!(error.message().starts_with(reason));
                assert_eq!(snapshot(&pool).await,before,"rejected login must not touch credential last_login/run/ticket/account state");
            }
            let request=source(users::RegisterRequest{client_product:ACCOUNT_PRODUCT.into(),username:"never-created".into(),password:"fixture-only".into(),ubi_id:"never-created-id".into(),package_identity:declared});
            let error=users.register(request).await.unwrap_err();assert_eq!(error.code(),tonic::Code::FailedPrecondition);assert!(error.message().starts_with(reason));
            assert_eq!(snapshot(&pool).await,before,"rejected register cannot create an account");
        }
        println!("RELEASE_HANDLER denied=missing,catalog,mixed credential_last_login_unchanged=true run_ticket_account_unchanged=true");
    });
}

#[test]
fn exact_release_account_and_game_are_separate_and_signed_identity_roundtrips() {
    let (storage,pool)=crate::storage::release_test_support::fixture();
    runtime().block_on(async {
        seed(&pool).await;let users=service(storage.clone());
        for product in [ACCOUNT_PRODUCT,GameplayMode::Official.product(),GameplayMode::Mod.product()] {
            let mut request=login(product,Some(test_package_identity()));
            if product==GameplayMode::Mod.product(){request.metadata_mut().insert(GAME_INSTANCE_HEADER,"ffffffffffffffffffffffffffffffff".parse().unwrap());}
            let response=users.login(request).await.unwrap().into_inner();assert!(!response.token.is_empty());
            let claims=verify_auth_token(&config(),&response.token,"127.0.0.1".parse().unwrap(),unix_time_seconds().unwrap()).unwrap();
            assert_eq!(claims.release,Some(test_release_identity()));
            let mut r=source(());r.metadata_mut().insert("authorization",response.token.parse().unwrap());
            let verified=check_token(&logger(),&config(),&storage,LanClientNetwork,r).await.unwrap();
            if product==ACCOUNT_PRODUCT {
                assert!(claims.game_run.is_none());assert_eq!(snapshot(&pool).await.2,0);
                assert_eq!(authenticated_game_mode(&storage,&verified).await.unwrap_err().code(),tonic::Code::FailedPrecondition);
            } else {
                assert_eq!(claims.game_run.unwrap().product,product);
                assert_eq!(authenticated_game_mode(&storage,&verified).await.unwrap(),if product==GameplayMode::Official.product(){1}else{2});
            }
        }
        println!("RELEASE_HANDLER exact_identity_accepted=true account_has_no_game_capability=true official_mod_distinct=true signed_roundtrip=true");
    });
}

#[test]
fn signed_release_tamper_legacy_and_other_catalog_reject_before_heartbeat() {
    let (storage,pool)=crate::storage::release_test_support::fixture();
    runtime().block_on(async {
        seed(&pool).await;let users=service(storage.clone());
        let token=users.login(login(GameplayMode::Mod.product(),Some(test_package_identity()))).await.unwrap().into_inner().token;
        let now=unix_time_seconds().unwrap();let accepted=verify_auth_token(&config(),&token,"127.0.0.1".parse().unwrap(),now).unwrap();
        sqlx::query("UPDATE game_runs SET seen_at=? WHERE user_id=1000").bind(OLD_TIME).execute(&pool).await.unwrap();
        let parts:Vec<_>=token.split('.').collect();
        let changed=String::from_utf8(decode_base64url(parts[1]).unwrap()).unwrap().replace("fixture-release-2","fixture-release-1");
        let unsigned=format!("{}.{}.{}",parts[0],encode_base64url(changed.as_bytes()),parts[2]);
        let mut legacy=accepted.clone();legacy.release=None;
        let mut wrong=accepted;wrong.release=Some(ReleaseIdentity::new(1,"fixture-release-2","fixture-compat-1",&"b".repeat(64)).unwrap());
        for invalid in [unsigned,issue_auth_token(&config(),&legacy).unwrap(),issue_auth_token(&config(),&wrong).unwrap()] {
            assert_eq!(verify_auth_token(&config(),&invalid,"127.0.0.1".parse().unwrap(),now),Err(AuthTokenError::Invalid));
            let mut r=source(());r.metadata_mut().insert("authorization",invalid.parse().unwrap());
            r.metadata_mut().insert("x-scbl-release-id","fixture-release-2".parse().unwrap());
            assert_eq!(check_token(&logger(),&config(),&storage,LanClientNetwork,r).await.unwrap_err().code(),tonic::Code::Unauthenticated);
            let seen:String=sqlx::query_scalar("SELECT seen_at FROM game_runs WHERE user_id=1000").fetch_one(&pool).await.unwrap();assert_eq!(seen,OLD_TIME);
        }
        println!("RELEASE_SIGNED tamper_legacy_other_catalog_rejected=true raw_metadata_cannot_upgrade=true lease_unchanged=true");
    });
}

#[test]
fn same_nonce_different_or_null_identity_does_not_delete_tickets_or_update_source() {
    let (storage,pool)=crate::storage::release_test_support::fixture();
    runtime().block_on(async {
        seed(&pool).await;let original=test_release_identity();
        {let _guard=storage.mode_gate.write().await;storage.begin_game_run_with_release(1000,NONCE,2,"127.0.0.1".parse().unwrap(),&original).await.unwrap();}
        let run:i64=sqlx::query_scalar("SELECT id FROM game_runs WHERE user_id=1000").fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO user_sessions(id,user_id,game_run_id,ticket_hash,native_connected) VALUES('fixture-ticket',1000,?,'fixture-hash',0)").bind(run).execute(&pool).await.unwrap();
        let before:(String,String,Option<String>)=sqlx::query_as("SELECT source_ip,seen_at,release_identity FROM game_runs WHERE id=?").bind(run).fetch_one(&pool).await.unwrap();
        let other=ReleaseIdentity::new(1,"fixture-release-2","fixture-compat-1",&"b".repeat(64)).unwrap();
        {let _guard=storage.mode_gate.write().await;assert!(storage.begin_game_run_with_release(1000,NONCE,2,"127.0.0.2".parse().unwrap(),&other).await.is_err());}
        let after:(String,String,Option<String>)=sqlx::query_as("SELECT source_ip,seen_at,release_identity FROM game_runs WHERE id=?").bind(run).fetch_one(&pool).await.unwrap();assert_eq!(before,after);
        let tickets:i64=sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE user_id=1000").fetch_one(&pool).await.unwrap();assert_eq!(tickets,1);
        // The database guard independently prohibits backfilling/overwriting identity.
        assert!(sqlx::query("UPDATE game_runs SET release_identity=NULL WHERE id=?").bind(run).execute(&pool).await.is_err());
        sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(1001,'legacy-fixture','fixture-only','legacy-account')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip) VALUES(1001,?,2,'127.0.0.1')").bind(NONCE).execute(&pool).await.unwrap();
        {let _guard=storage.mode_gate.write().await;assert!(storage.begin_game_run_with_release(1001,NONCE,2,"127.0.0.2".parse().unwrap(),&original).await.is_err());}
        let legacy:(String,Option<String>)=sqlx::query_as("SELECT source_ip,release_identity FROM game_runs WHERE user_id=1001").fetch_one(&pool).await.unwrap();assert_eq!(legacy,("127.0.0.1".into(),None));
        assert!(sqlx::query("UPDATE game_runs SET release_identity=? WHERE user_id=1001").bind(original.storage_key()).execute(&pool).await.is_err());
        // Same tuple + same nonce is an ordinary renewal, not forced run retirement.
        {let _guard=storage.mode_gate.write().await;storage.begin_game_run_with_release(1000,NONCE,2,"127.0.0.1".parse().unwrap(),&original).await.unwrap();}
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT id FROM game_runs WHERE user_id=1000 AND retired_at IS NULL").fetch_one(&pool).await.unwrap(),run);
        println!("RELEASE_RUN changed_and_null_rejected=true source_seen_release_unchanged=true ticket_preserved=true same_identity_renewal=true database_immutable=true");
    });
}