//! Prepared, NOT compiled/run by the audit agent. Mount as a cfg(test) child
//! of dedicated_server's main.rs. No copied dispatcher or fabricated logs.
//! The only socket operation is a short-lived 127.0.0.1:0 bind required by
//! Protocol::handle; inspected methods neither send nor receive on that socket.
use crate::api::diagnostic_identity_tests::{approved_run, fixture, runtime, seed_plain, ACCOUNT, ID, NAME, PASSWORD};
use crate::protocols::authentication_foundation::ticket_granting_protocol::{LoginExRequest, LoginExResponse};
use crate::protocols::ubi_account_management_service::ubi_account_management_protocol::*;
use crate::protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::{FromStream, ToStream};
use quazal::rmc::types::{Any, Data, QResult};
use quazal::rmc::{Protocol, Request};
use slog::Drain;
use sloggers::Build;
use sloggers::types::{Format, Severity};
use std::net::UdpSocket;
use std::path::PathBuf;

struct Capture { directory: PathBuf }
impl Capture {
    fn new(label: &str, debug: bool) -> (Self, slog::Logger) {
        // No implicit temp/root fallback: the future root runner owns both dirs.
        let root = PathBuf::from(std::env::var_os("SCBL_IDENTITY_TEST_DIR").expect("owned capture dir required"));
        let db = PathBuf::from(std::env::var_os("SCBL_RELEASE_TEST_DIR").expect("owned synthetic SQLite dir required"));
        assert!(root.is_absolute() && db.is_absolute());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&db).unwrap();
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = root.join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        // Same actual sloggers JSON + Compact formatters as batch16.
        // Debug cases deliberately raise BOTH fixture drains for aligned records;
        // production's file drain stays Info (only its terminal can emit Debug).
        let json = sloggers::file::FileLoggerBuilder::new(directory.join("producer.jsonl"))
            .level(if debug { Severity::Debug } else { Severity::Info }).format(Format::Json).build().unwrap();
        let compact = sloggers::file::FileLoggerBuilder::new(directory.join("producer.compact.txt"))
            .level(if debug { Severity::Debug } else { Severity::Info }).format(Format::Compact).build().unwrap();
        (Self { directory }, slog::Logger::root(slog::Duplicate(json, compact).fuse(), slog::o!()))
    }

    // Call AFTER dropping every logger clone (including protocol and Storage).
    fn finish(self, expected_records: usize, secret_probes: &[(&str, String)]) {
        let json = std::fs::read_to_string(self.directory.join("producer.jsonl")).unwrap();
        let compact = std::fs::read_to_string(self.directory.join("producer.compact.txt")).unwrap();
        let rows: Vec<serde_json::Value> = json.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(rows.len(), expected_records, "unexpected producer event count; raw files retained at {}", self.directory.display());
        assert!(!compact.is_empty(), "Compact drain missing");
        let mut probes = vec![
            ("name-head", "DPA_NAME_HEAD".to_owned()), ("name-tail", "DPA_NAME_TAIL".to_owned()),
            ("account-head", "DPA_ACCOUNT_HEAD".to_owned()), ("account-tail", "DPA_ACCOUNT_TAIL".to_owned()),
            ("password-head", "DPA_SECRET_PASSWORD".to_owned()), ("password-tail", "DPA_SECRET_TAIL".to_owned()),
            ("principal", ID.to_string()),
        ];
        probes.extend(secret_probes.iter().cloned());
        let mut leaked = Vec::new();
        for (label, value) in probes {
            assert!(!value.is_empty());
            let in_json = json.contains(&value);
            let in_compact = compact.contains(&value);
            println!("DPA_REMAINING canary={label} json={in_json} compact={in_compact}");
            if in_json || in_compact { leaked.push(label); }
        }
        println!("DPA_REMAINING path={} events={} compact_physical_lines={} identity_canary_present={}",
            self.directory.display(), rows.len(), compact.lines().count(), !leaked.is_empty());
        // Scan the WHOLE bytes above, including metadata/continuations. No
        // ignore-continuation parser and no false physical-lines==events rule.
        assert!(leaked.is_empty(), "actual producer leaked synthetic canaries: {leaked:?}");
        for row in rows {
            let message = row["msg"].as_str().unwrap();
            assert!(!message.is_empty() && !message.contains(['\r', '\n']));
            assert!(compact.contains(message), "Compact lost a genuine JSON message");
        }
    }
}

fn socket() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    assert!(socket.local_addr().unwrap().ip().is_loopback());
    socket
}
fn decimal_bytes(value: &[u8]) -> String {
    value.iter().map(u8::to_string).collect::<Vec<_>>().join(", ")
}
fn dispatch(protocol: &dyn Protocol<()>, logger: &slog::Logger, ci: &mut quazal::ClientInfo<()>,
    method: u32, parameters: Vec<u8>, socket: &UdpSocket) -> quazal::rmc::Result<Vec<u8>> {
    let request = Request { protocol_id: protocol.id(), call_id: 1, method_id: method, parameters };
    let mut context = quazal::Context::splinter_cell_blacklist();
    context.listen = socket.local_addr().unwrap();
    context.secure_server_addr = None;
    protocol.handle(logger, &context, ci, &request, &ClientRegistry::default(), socket)
}

fn lookup(by_pid: bool, debug: bool) {
    let (capture, logger) = Capture::new(if by_pid { "lookup-pids" } else { "lookup-accounts" }, debug);
    let (storage, pool) = fixture(logger.clone());
    let rt = runtime();
    rt.block_on(seed_plain(&pool));
    let protocol = crate::ubi_acc_mgmt::new_protocol::<()>(storage.clone());
    let mut ci = quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    // The production lookup's admission contract is Some(user_id); this fixture
    // does not claim to cover a transport handshake or account authentication.
    ci.user_id = Some(ID);
    let socket = socket();
    let bytes = if by_pid {
        dispatch(protocol.as_ref(), &logger, &mut ci, UbiAccountManagementProtocolMethod::LookupUbiAccountIDsByPids as u32,
            LookupUbiAccountIDsByPidsRequest { pids: vec![ID] }.to_bytes(), &socket).unwrap()
    } else {
        dispatch(protocol.as_ref(), &logger, &mut ci, UbiAccountManagementProtocolMethod::LookupPrincipalIds as u32,
            LookupPrincipalIdsRequest { ubi_account_ids: vec![ACCOUNT.into()] }.to_bytes(), &socket).unwrap()
    };
    if by_pid {
        let response = LookupUbiAccountIDsByPidsResponse::from_bytes(&bytes).unwrap();
        assert_eq!(response.ubiaccount_ids.len(), 1);
        assert_eq!(response.ubiaccount_ids.get(&ID).map(String::as_str), Some(ACCOUNT));
    } else {
        let response = LookupPrincipalIdsResponse::from_bytes(&bytes).unwrap();
        assert_eq!(response.pids.len(), 1);
        assert_eq!(response.pids.get(ACCOUNT), Some(&ID));
    }
    assert_eq!(ci.user_id, Some(ID));
    rt.block_on(pool.close());
    drop(socket); drop(protocol); drop(storage); drop(pool); drop(logger);
    capture.finish((if by_pid { 2 } else { 1 }) + if debug { 2 } else { 0 }, &[]);
}

#[test]
fn dpa_remaining_lookup_accounts_info_actual_dispatch() { lookup(false, false); }
#[test]
fn dpa_remaining_lookup_pids_info_actual_dispatch() { lookup(true, false); }
#[test]
fn dpa_remaining_lookup_accounts_debug_actual_dispatch() { lookup(false, true); }
#[test]
fn dpa_remaining_lookup_pids_debug_actual_dispatch() { lookup(true, true); }

#[test]
fn dpa_remaining_login_ex_debug_actual_credentials_and_ticket() {
    let (capture, logger) = Capture::new("loginex-debug", true);
    let (storage, pool) = fixture(logger.clone());
    let rt = runtime();
    rt.block_on(seed_plain(&pool));
    rt.block_on(approved_run(&storage, ID));
    let protocol = crate::ticket::new_protocol::<()>(storage.clone());
    let mut ci = quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    let socket = socket();
    const ONLINE_KEY: &str = "DPA_ONLINE_KEY_SYNTHETIC";
    let custom = UbiAuthenticationLoginCustomData { data: Data, user_name: NAME.into(),
        online_key: ONLINE_KEY.into(), password: PASSWORD.into() };
    let parameters = LoginExRequest { str_user_name: ACCOUNT.into(),
        o_extra_data: Any::new("UbiAuthenticationLoginCustomData".into(), custom.to_bytes()) }.to_bytes();
    let bytes = dispatch(protocol.as_ref(), &logger, &mut ci, 2, parameters, &socket).unwrap();
    let response = LoginExResponse::from_bytes(&bytes).unwrap();
    assert!(matches!(response.return_value, QResult::Ok));
    assert_eq!(response.pid_principal, ID);
    assert_eq!(ci.user_id, Some(ID));
    assert!(!response.pbuf_response.is_empty());
    let probes = [
        ("password-decimal-bytes", decimal_bytes(PASSWORD.as_bytes())),
        ("online-key-decimal-bytes", decimal_bytes(ONLINE_KEY.as_bytes())),
        ("real-returned-ticket-bytes", format!("{:?}", response.pbuf_response)),
    ];
    rt.block_on(pool.close());
    drop(socket); drop(protocol); drop(storage); drop(pool); drop(logger);
    capture.finish(5, &probes);
}

fn unimplemented_password_method(link: bool) {
    let (capture, logger) = Capture::new(if link { "link-debug" } else { "validate-password-debug" }, true);
    let (storage, pool) = fixture(logger.clone());
    let protocol = crate::ubi_acc_mgmt::new_protocol::<()>(storage.clone());
    let mut ci = quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    ci.user_id = Some(ID);
    let socket = socket();
    let (method, parameters) = if link {
        (UbiAccountManagementProtocolMethod::LinkAccount as u32,
            LinkAccountRequest { ubi_account_username: NAME.into(), ubi_account_password: PASSWORD.into() }.to_bytes())
    } else {
        (UbiAccountManagementProtocolMethod::ValidatePassword as u32,
            ValidatePasswordRequest { password: PASSWORD.into(), username: NAME.into() }.to_bytes())
    };
    let result = dispatch(protocol.as_ref(), &logger, &mut ci, method, parameters, &socket);
    assert!(matches!(result, Err(quazal::rmc::Error::UnimplementedMethod)));
    assert_eq!(ci.user_id, Some(ID));
    runtime().block_on(pool.close());
    drop(socket); drop(protocol); drop(storage); drop(pool); drop(logger);
    capture.finish(3, &[]);
}
#[test]
fn dpa_remaining_validate_password_debug_even_when_unimplemented() { unimplemented_password_method(false); }
#[test]
fn dpa_remaining_link_account_debug_even_when_unimplemented() { unimplemented_password_method(true); }
