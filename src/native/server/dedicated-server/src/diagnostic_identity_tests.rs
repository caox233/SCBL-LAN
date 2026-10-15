//! Actual authentication producers -> duplicate real sloggers JSON/Compact
//! formatters. All credentials/SQLite rows are synthetic; no listener or socket.
use super::*;
use slog::Drain;
use sloggers::Build;
use std::path::PathBuf;
pub(crate) const NAME: &str = "DPA_NAME_HEAD not found\r\nDPA_NAME_TAIL";
pub(crate) const ACCOUNT: &str = "DPA_ACCOUNT_HEAD\r\nDPA_ACCOUNT_TAIL";
pub(crate) const PASSWORD: &str = "DPA_SECRET_PASSWORD\r\nDPA_SECRET_TAIL";
pub(crate) const NEW_PASSWORD: &str = "DPA_NEW_PASSWORD\r\nDPA_NEW_TAIL";
pub(crate) const ID: u32 = 734501001;
pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}
fn config() -> AuthTokenConfig {
    AuthTokenConfig { release:test_release_identity(), key:hmacsha256::Key([0x52;32]), ttl_seconds:DEFAULT_AUTH_TOKEN_TTL_SECONDS }
}
pub(crate) struct Capture { pub(crate) directory: PathBuf }
impl Capture {
    pub(crate) fn new(label: &str) -> (Self, Logger) {
        let root = PathBuf::from(std::env::var_os("SCBL_IDENTITY_TEST_DIR").expect("root must supply an owned SCBL_IDENTITY_TEST_DIR"));
        let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory=root.join(format!("{label}-{}-{nonce}",std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let json=sloggers::file::FileLoggerBuilder::new(directory.join("producer.jsonl"))
            .level(sloggers::types::Severity::Info).format(sloggers::types::Format::Json).build().unwrap();
        let compact=sloggers::file::FileLoggerBuilder::new(directory.join("producer.compact.txt"))
            .level(sloggers::types::Severity::Info).format(sloggers::types::Format::Compact).build().unwrap();
        let logger=Logger::root(slog::Duplicate(json,compact).fuse(),slog::o!());
        (Self{directory},logger)
    }
    // Call only after every clone of the async logger (including Storage) drops.
    pub(crate) fn finish(self, expected: &[&str], secrets: &[String]) {
        let json=std::fs::read_to_string(self.directory.join("producer.jsonl")).unwrap();
        let compact=std::fs::read_to_string(self.directory.join("producer.compact.txt")).unwrap();
        let rows:Vec<serde_json::Value>=json.lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(rows.len(),expected.len(),"lost or extra actual producer records: {}",self.directory.display());
        assert!(!compact.is_empty());
        let forbidden=["DPA_NAME_HEAD","DPA_NAME_TAIL","DPA_ACCOUNT_HEAD","DPA_ACCOUNT_TAIL",
            "DPA_SECRET_PASSWORD","DPA_SECRET_TAIL","DPA_NEW_PASSWORD","DPA_NEW_TAIL","734501001","734501002"];
        let leaked:Vec<_>=forbidden.iter().filter(|value|json.contains(**value)||compact.contains(**value)).copied().collect();
        println!("DPA_PRODUCER path={} records={} compact_physical_lines={} identity_canary_present={}",
            self.directory.display(),rows.len(),compact.lines().count(),!leaked.is_empty());
        assert!(leaked.is_empty(),"real producer leaked synthetic identity fragments: {leaked:?}; raw files retained");
        for secret in secrets { assert!(!json.contains(secret)&&!compact.contains(secret),"generated synthetic secret reached a drain"); }
        let messages:Vec<_>=rows.iter().map(|row|row["msg"].as_str().unwrap()).collect();
        assert_eq!(messages,expected,"fixed diagnostic events must not disappear");
        assert!(compact_matches_records(&compact,&rows),"unexpected Compact metadata, message or continuation: {}",self.directory.display());
    }
}
pub(crate) fn source<T>(value:T)->Request<T> {
    let mut request=Request::new(value);
    request.extensions_mut().insert(tonic::transport::server::TcpConnectInfo {
        local_addr:Some("127.0.0.1:50051".parse().unwrap()),remote_addr:Some("127.0.0.1:30500".parse().unwrap()) });
    request
}
pub(crate) fn login_request(password:&str)->Request<users::LoginRequest> {
    source(users::LoginRequest {client_product:server_api::gameplay_contract::ACCOUNT_PRODUCT.into(),
        username:NAME.into(),password:password.into(),package_identity:Some(test_package_identity())})
}
pub(crate) fn fixture(logger:Logger)->(Arc<Storage>,sqlx::SqlitePool) {
    crate::storage::release_test_support::fixture_with_logger(logger)
}
pub(crate) async fn seed_plain(pool:&sqlx::SqlitePool) {
    sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(?,?,?,?)")
        .bind(ID).bind(NAME).bind(PASSWORD).bind(ACCOUNT).execute(pool).await.unwrap();
}
pub(crate) async fn approved_run(storage:&Storage,user:u32) {
    let _guard=storage.mode_gate.write().await;
    storage.begin_game_run_with_release(user,"11111111111111111111111111111111",2,
        "127.0.0.1".parse().unwrap(),&test_release_identity()).await.unwrap();
}

fn storage_producer(kind:&str) {
        let (capture,logger)=Capture::new(kind);
        let (storage,pool)=fixture(logger.clone());
        let rt=runtime();
        rt.block_on(async {
            match kind {
                "plain"=>seed_plain(&pool).await,
                "hash"=>storage.register_user_with_recovery_async(NAME,PASSWORD,Some(ACCOUNT),None).await.unwrap(),
                _=>(),
            }
            let result=storage.authenticate_user_async(NAME,PASSWORD).await.unwrap();
            if kind=="missing" { assert!(matches!(result,Err(LoginError::NotFound))); }
            else { let Ok(identity)=result else {panic!("synthetic authentication unexpectedly rejected")};assert_eq!(identity.username,NAME);assert_eq!(identity.account_id,ACCOUNT); }
            pool.close().await;
        });
        drop(pool);drop(storage);drop(logger);
        capture.finish(&[match kind {"missing"=>"Authentication account not found", "plain"=>"Verifying legacy password", _=>"Verifying password hash"}],&[]);
}
#[test]
fn dpa_identity_storage_actual_missing_producer() { storage_producer("missing"); }
#[test]
fn dpa_identity_storage_actual_plain_producer() { storage_producer("plain"); }
#[test]
fn dpa_identity_storage_actual_hash_producer() { storage_producer("hash"); }

#[test]
fn dpa_identity_api_actual_registration_login_reset_producers() {
    let (capture,logger)=Capture::new("api-register-login-reset");
    let (storage,pool)=fixture(logger.clone());
    let service=MyUsers {logger:logger.clone(),token_config:config(),storage:storage.clone(),overlay_network:LanClientNetwork};
    let rt=runtime();
    let secrets=rt.block_on(async {
        assert!(valid_lan_registration_identity(NAME,ACCOUNT));
        let response=service.register(source(users::RegisterRequest {
            client_product:server_api::gameplay_contract::ACCOUNT_PRODUCT.into(),username:NAME.into(),password:PASSWORD.into(),ubi_id:ACCOUNT.into(),package_identity:Some(test_package_identity())
        })).await.unwrap().into_inner();
        assert!(response.error.is_empty());
        let first=service.login(login_request(PASSWORD)).await.unwrap().into_inner();
        assert_eq!(first.user.as_ref().unwrap().username,NAME);
        assert_eq!(first.user.as_ref().unwrap().id,ACCOUNT);
        let reset=service.reset_password(Request::new(users::ResetPasswordRequest {
            username:NAME.into(),recovery_code:response.recovery_code.clone(),new_password:NEW_PASSWORD.into()
        })).await.unwrap().into_inner();
        let second=service.login(login_request(NEW_PASSWORD)).await.unwrap().into_inner();
        assert_eq!(second.user.as_ref().unwrap().username,NAME);
        pool.close().await;
        vec![response.recovery_code,reset.recovery_code,first.token,second.token]
    });
    drop(service);drop(pool);drop(storage);drop(logger);
    capture.finish(&["Account registration completed","Verifying password hash","Account login succeeded",
        "Account password reset completed","Verifying password hash","Account login succeeded"],&secrets);
}

// This fixture logger has exactly one inherited KV: sloggers' default module.
// Match it to the corresponding real JSON record, never skip arbitrary lines.
fn compact_matches_records(compact:&str, rows:&[serde_json::Value])->bool {
    if !compact.ends_with('\n') || compact.contains('\r') { return false; }
    let mut lines=compact.split_terminator('\n');
    let mut previous_module=None;
    for row in rows {
        let Some(module)=row["module"].as_str() else {return false};
        let Some((path,number))=module.rsplit_once(':') else {return false};
        if !matches!(path,"dedicated_server::api"|"dedicated_server::storage::accounts"|"dedicated_server::ticket")
            || number.is_empty() || !number.bytes().all(|b|b.is_ascii_digit())
            || !number.parse::<u32>().is_ok_and(|n|n>0) {return false;}
        if previous_module!=Some(module) {
            let header=format!("module: {module}");
            if lines.next()!=Some(header.as_str()) {return false;}
            previous_module=Some(module);
        }
        let Some(line)=lines.next().and_then(|line|line.strip_prefix(' ')) else {return false};
        let Some(timestamp)=line.get(..19) else {return false};
        let t=timestamp.as_bytes();
        if !matches!(&t[..3],b"Jan"|b"Feb"|b"Mar"|b"Apr"|b"May"|b"Jun"|b"Jul"|b"Aug"|b"Sep"|b"Oct"|b"Nov"|b"Dec")
            || t[3]!=b' ' || t[6]!=b' ' || t[9]!=b':' || t[12]!=b':' || t[15]!=b'.'
            || ![4,5,7,8,10,11,13,14,16,17,18].iter().all(|&i|t[i].is_ascii_digit()) {return false;}
        let pair=|i:usize|(t[i]-b'0')*10+t[i+1]-b'0';
        if !(1..=31).contains(&pair(4)) || pair(7)>23 || pair(10)>59 || pair(13)>60 {return false;}
        let (Some(level),Some(message))=(row["level"].as_str(),row["msg"].as_str()) else {return false};
        if !matches!(level,"INFO"|"WARN"|"ERRO") || message.contains(['\r','\n']) {return false;}
        let record=format!(" {level} {message}");
        if line.get(19..)!=Some(record.as_str()) {return false;}
    }
    lines.next().is_none()
}

#[test]
fn dpa_identity_fixture_compact_record_boundaries_are_strict() {
    // Parser-only negative controls, not substituted producer evidence.
    let row=serde_json::json!({"module":"dedicated_server::ticket:367","level":"WARN","msg":"fixed event"});
    let header="module: dedicated_server::ticket:367\n";
    let event=" Oct 12 06:48:04.489 WARN fixed event\n";
    let valid=format!("{header}{event}");
    assert!(compact_matches_records(&valid,&[row.clone()]));
    assert!(compact_matches_records(&format!("{valid}{event}"),&[row.clone(),row.clone()]));
    for invalid in [
        format!("{valid}unknown continuation\n"),
        format!("{header}unknown continuation\n{event}"),
        format!("{header}{header}{event}"),
        format!("{valid}{header}"),
        event.to_owned(),
        valid.replace("::ticket:","::unreviewed:"),
        valid.replace("fixed event","fixed event, unknown: value"),
        valid.replace("fixed event","fixed event\n module: injected"),
        valid.replace("Oct 12","Oct AA"),
        valid.replace(" WARN "," INFO "),
        valid.replace('\n',"\r\n"),
    ] {assert!(!compact_matches_records(&invalid,&[row.clone()]),"unknown or mismatched Compact content was accepted");}
    assert!(!compact_matches_records(&format!("{valid}{header}{event}"),&[row.clone(),row]));
}
