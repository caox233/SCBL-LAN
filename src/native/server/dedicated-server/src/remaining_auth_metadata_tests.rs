//! Additional permanent coverage for the two versioned dispatcher inputs.
//! Real production new_protocol/handle and typed wire serialization, no mock
//! handlers. The frozen seven tests separately cover real JSON/Compact bytes.
use crate::api::diagnostic_identity_tests::{approved_run, fixture, runtime, seed_plain, ACCOUNT, ID, NAME, PASSWORD};
use crate::protocols::authentication_foundation::ticket_granting_protocol as auth;
use crate::protocols::ubi_account_management_service::ubi_account_management_protocol as ubi;
use crate::protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
use quazal::rmc::basic::{FromStream, ToStream};
use quazal::rmc::types::{Any, Data, QResult};
use quazal::rmc::{Error, Protocol, Request};
use slog::{Drain, KV};
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq)]
struct Event { level: slog::Level, message: String, fields: Vec<(String, String)> }
struct Fields(Vec<(String, String)>);
impl slog::Serializer for Fields {
    fn emit_arguments(&mut self, key: slog::Key, value: &std::fmt::Arguments<'_>) -> slog::Result {
        self.0.push((key.to_string(), value.to_string()));
        Ok(())
    }
}
struct Trace(Arc<Mutex<Vec<Event>>>);
impl Drain for Trace {
    type Ok = ();
    type Err = std::convert::Infallible;
    fn log(&self, record: &slog::Record<'_>, values: &slog::OwnedKVList) -> Result<(), Self::Err> {
        // Capture every severity/field from the actual generated producer,
        // including its default-method warning, not only the safe Debug text.
        if record.module().starts_with("sc_bl_protocols::protocols::") {
            let mut fields = Fields(Vec::new());
            record.kv().serialize(record, &mut fields).unwrap();
            values.serialize(record, &mut fields).unwrap();
            self.0.lock().unwrap().push(Event {
                level: record.level(), message: record.msg().to_string(), fields: fields.0,
            });
        }
        Ok(())
    }
}
fn expected(protocol: &str, method: &str, default_method: Option<&str>) -> Vec<Event> {
    let mut rows = vec![Event { level: slog::Level::Debug,
        message: format!("{protocol}.{method} request"), fields: vec![] }];
    if let Some(name) = default_method {
        rows.push(Event { level: slog::Level::Warning,
            message: format!("Method {protocol}.{name} not implemented"), fields: vec![] });
    }
    // Current successful methods return outer RMC Ok; all remaining defaults
    // are the already-documented UnimplementedMethod = 0x80010002.
    let success = default_method.is_none();
    let error_code: u32 = if success { 0 } else { 0x8001_0002 };
    rows.push(Event { level: slog::Level::Debug,
        message: format!("{protocol}.{method} response success={success} error_code={error_code}"), fields: vec![] });
    rows
}
struct Case { id: u32, method: &'static str, default_method: Option<&'static str>, parameters: Vec<u8> }
fn case(id: u32, method: &'static str, default_method: Option<&'static str>, parameters: Vec<u8>) -> Case {
    Case { id, method, default_method, parameters }
}
fn custom() -> Any<Data, String> {
    let data = UbiAuthenticationLoginCustomData { data: Data, user_name: NAME.into(),
        online_key: "DPA_ONLINE_KEY_SYNTHETIC".into(), password: PASSWORD.into() };
    Any::new("UbiAuthenticationLoginCustomData".into(), data.to_bytes())
}
fn auth_cases() -> Vec<Case> {
    vec![
        case(1, "Login", None, auth::LoginRequest { str_user_name: NAME.into() }.to_bytes()),
        case(2, "LoginEx", None, auth::LoginExRequest { str_user_name: ACCOUNT.into(), o_extra_data: custom() }.to_bytes()),
        case(3, "RequestTicket", None, auth::RequestTicketRequest { id_source: ID, id_target: crate::SERVER_PID }.to_bytes()),
        case(4, "GetPid", Some("get_pid"), auth::GetPidRequest { str_user_name: NAME.into() }.to_bytes()),
        case(5, "GetName", Some("get_name"), auth::GetNameRequest { id: ID }.to_bytes()),
        case(6, "LoginWithContext", Some("login_with_context"), auth::LoginWithContextRequest { login_data: custom() }.to_bytes()),
    ]
}
fn ubi_cases() -> Vec<Case> {
    vec![
        case(1, "CreateAccount", Some("create_account"), ubi::CreateAccountRequest.to_bytes()),
        case(2, "UpdateAccount", Some("update_account"), ubi::UpdateAccountRequest.to_bytes()),
        case(3, "GetAccount", Some("get_account"), ubi::GetAccountRequest.to_bytes()),
        case(4, "LinkAccount", Some("link_account"), ubi::LinkAccountRequest { ubi_account_username: NAME.into(), ubi_account_password: PASSWORD.into() }.to_bytes()),
        case(5, "GetTos", Some("get_tos"), ubi::GetTosRequest { country_code: NAME.into(), language_code: ACCOUNT.into(), html_version: false }.to_bytes()),
        case(6, "ValidateUsername", Some("validate_username"), ubi::ValidateUsernameRequest { username: NAME.into() }.to_bytes()),
        case(7, "ValidatePassword", Some("validate_password"), ubi::ValidatePasswordRequest { username: NAME.into(), password: PASSWORD.into() }.to_bytes()),
        case(8, "ValidateEmail", Some("validate_email"), ubi::ValidateEmailRequest { email: "DPA_EMAIL_SYNTHETIC@example.invalid".into() }.to_bytes()),
        case(9, "GetCountryList", Some("get_country_list"), ubi::GetCountryListRequest { language_code: NAME.into() }.to_bytes()),
        case(10, "ForgetPassword", Some("forget_password"), ubi::ForgetPasswordRequest { username_or_email: NAME.into() }.to_bytes()),
        case(11, "LookupPrincipalIds", None, ubi::LookupPrincipalIdsRequest { ubi_account_ids: vec![ACCOUNT.into()] }.to_bytes()),
        case(12, "LookupUbiAccountIDsByPids", None, ubi::LookupUbiAccountIDsByPidsRequest { pids: vec![ID] }.to_bytes()),
        case(13, "LookupUbiAccountIDsByUsernames", Some("lookup_ubi_account_ids_by_usernames"), ubi::LookupUbiAccountIDsByUsernamesRequest { usernames: vec![NAME.into()] }.to_bytes()),
        case(14, "LookupUsernamesByUbiAccountIDs", Some("lookup_usernames_by_ubi_account_ids"), ubi::LookupUsernamesByUbiAccountIDsRequest { ubi_account_ids: vec![ACCOUNT.into()] }.to_bytes()),
        case(15, "LookupUbiAccountIDsByUsernameSubString", Some("lookup_ubi_account_ids_by_username_sub_string"), ubi::LookupUbiAccountIDsByUsernameSubStringRequest { username_sub_string: NAME.into() }.to_bytes()),
        case(16, "UserHasPlayed", Some("user_has_played"), ubi::UserHasPlayedRequest { ubi_account_ids: vec![ACCOUNT.into()] }.to_bytes()),
        case(17, "IsUserPlaying", Some("is_user_playing"), ubi::IsUserPlayingRequest { ubi_account_ids: vec![ACCOUNT.into()] }.to_bytes()),
        case(18, "LookupUbiAccountIDsByUsernamesGlobal", Some("lookup_ubi_account_ids_by_usernames_global"), ubi::LookupUbiAccountIDsByUsernamesGlobalRequest { usernames: vec![NAME.into()] }.to_bytes()),
        case(19, "LookupUbiAccountIDsByEmailsGlobal", Some("lookup_ubi_account_ids_by_emails_global"), ubi::LookupUbiAccountIDsByEmailsGlobalRequest { emails: vec!["DPA_EMAIL_SYNTHETIC@example.invalid".into()] }.to_bytes()),
        case(20, "LookupUsernamesByUbiAccountIDsGlobal", Some("lookup_usernames_by_ubi_account_ids_global"), ubi::LookupUsernamesByUbiAccountIDsGlobalRequest { ubi_account_ids: vec![ACCOUNT.into()] }.to_bytes()),
        case(21, "GetTosEx", Some("get_tos_ex"), ubi::GetTosExRequest { country_code: NAME.into(), language_code: ACCOUNT.into(), html_version: false }.to_bytes()),
        case(22, "HasAcceptedLatestTos", None, ubi::HasAcceptedLatestTosRequest.to_bytes()),
        case(23, "AcceptLatestTos", Some("accept_latest_tos"), ubi::AcceptLatestTosRequest.to_bytes()),
    ]
}
fn matrix(auth_protocol: bool) {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let logger = slog::Logger::root(Trace(trace.clone()).fuse(), slog::o!());
    let (storage, pool) = fixture(logger.clone());
    let rt = runtime();
    rt.block_on(seed_plain(&pool));
    if auth_protocol { rt.block_on(approved_run(&storage, ID)); }
    let (protocol, cases, name): (Box<dyn Protocol<()>>, _, _) = if auth_protocol {
        (crate::ticket::new_protocol(storage.clone()), auth_cases(), "TicketGrantingProtocol")
    } else {
        (crate::ubi_acc_mgmt::new_protocol(storage.clone()), ubi_cases(), "UbiAccountManagementProtocol")
    };
    // Regeneration adding/removing a method requires an explicit updated matrix.
    assert_eq!(usize::try_from(protocol.num_methods()).unwrap(), cases.len());
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut ctx = quazal::Context::splinter_cell_blacklist();
    ctx.listen = socket.local_addr().unwrap();
    ctx.secure_server_addr = None;
    let clients = quazal::prudp::ClientRegistry::<()>::default();
    let mut observed = Vec::new();
    for case in cases {
        let mut ci = quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
        if !auth_protocol || case.id == 3 { ci.user_id = Some(ID); }
        let request = Request { protocol_id: protocol.id(), call_id: case.id, method_id: case.id, parameters: case.parameters };
        let result = protocol.handle(&logger, &ctx, &mut ci, &request, &clients, &socket);
        if case.default_method.is_some() {
            assert!(matches!(result, Err(Error::UnimplementedMethod)), "{}", case.method);
        } else {
            let bytes = result.unwrap();
            assert_eq!(ci.user_id, Some(ID));
            match (auth_protocol, case.id) {
                (true, 1) => { let r = auth::LoginResponse::from_bytes(&bytes).unwrap(); assert!(matches!(r.return_value, QResult::Ok)); assert_eq!(r.pid_principal, ID); assert!(!r.pbuf_response.is_empty()); },
                (true, 2) => { let r = auth::LoginExResponse::from_bytes(&bytes).unwrap(); assert!(matches!(r.return_value, QResult::Ok)); assert_eq!(r.pid_principal, ID); assert!(!r.pbuf_response.is_empty()); },
                (true, 3) => { let r = auth::RequestTicketResponse::from_bytes(&bytes).unwrap(); assert!(matches!(r.return_value, QResult::Ok)); assert!(!r.buf_response.is_empty()); },
                (false, 11) => { let r = ubi::LookupPrincipalIdsResponse::from_bytes(&bytes).unwrap(); assert_eq!(r.pids.get(ACCOUNT), Some(&ID)); },
                (false, 12) => { let r = ubi::LookupUbiAccountIDsByPidsResponse::from_bytes(&bytes).unwrap(); assert_eq!(r.ubiaccount_ids.get(&ID).map(String::as_str), Some(ACCOUNT)); },
                (false, 22) => { let r = ubi::HasAcceptedLatestTosResponse::from_bytes(&bytes).unwrap(); assert!(r.has_accepted); assert!(r.failed_reasons.is_empty()); },
                _ => unreachable!(),
            }
        }
        let events = std::mem::take(&mut *trace.lock().unwrap());
        observed.push((case.method, events, expected(name, case.method, case.default_method)));
    }
    // Release owned runtime/storage resources before any diagnostic assertion.
    rt.block_on(pool.close());
    drop(socket); drop(protocol); drop(storage); drop(pool); drop(logger);
    for (method, actual, expected) in observed {
        assert_eq!(actual, expected, "versioned {name}.{method} must retain only safe fixed event metadata");
    }
}
#[test]
fn dpa_remaining_metadata_ticket_all_six_actual_methods() { matrix(true); }
#[test]
fn dpa_remaining_metadata_ubi_all_twenty_three_actual_methods() { matrix(false); }
