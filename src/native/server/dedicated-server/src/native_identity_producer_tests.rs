//! Real native production handler cores, no socket and no invented log messages.
use super::*;
use crate::api::diagnostic_identity_tests::*;
use quazal::rmc::basic::ToStream;
use quazal::rmc::types::{Any,Data};
fn login_ex_request()->LoginExRequest {
    let data=UbiAuthenticationLoginCustomData { data:Data,user_name:NAME.into(),online_key:"DPA_SECRET_PASSWORD".into(),password:PASSWORD.into() };
    LoginExRequest { str_user_name:ACCOUNT.into(),o_extra_data:Any::new("UbiAuthenticationLoginCustomData".into(),data.to_bytes()) }
}
fn plain_producer(kind:&str) {
        let (capture,logger)=Capture::new(kind);
        let (storage,pool)=fixture(logger.clone());
        let rt=runtime();
        if kind!="missing" {
            rt.block_on(storage.register_user_with_recovery_async(NAME,PASSWORD,Some(ACCOUNT),None)).unwrap();
            let id=storage.find_user_id_by_name(NAME).unwrap().unwrap();
            rt.block_on(approved_run(&storage,id));
        }
        let service=TicketGrantingProtocolServerImpl {storage:storage.clone()};
        let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
        let result=service.login_core(&logger,&Context::splinter_cell_blacklist(),&mut ci,LoginRequest {str_user_name:NAME.into()});
        if kind=="missing" {assert!(matches!(result,Err(quazal::rmc::Error::AccessDenied)));assert_eq!(ci.user_id,None);}
        else {assert!(result.is_ok());assert!(ci.user_id.is_some());}
        rt.block_on(pool.close());
        drop(service);drop(storage);drop(pool);drop(logger);
        capture.finish(&[if kind=="missing" {"Native login account not found"} else {"Native login has no legacy password"}],&[]);
}
#[test]
fn dpa_identity_native_actual_plain_missing_producer() { plain_producer("missing"); }
#[test]
fn dpa_identity_native_actual_plain_no_password_producer() { plain_producer("no-plain-password"); }
#[test]
fn dpa_identity_native_actual_login_ex_missing_producers() {
    let (capture,logger)=Capture::new("native-loginex-missing");
    let (storage,pool)=fixture(logger.clone());
    let service=TicketGrantingProtocolServerImpl {storage:storage.clone()};
    let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    let result=service.login_ex_core(&logger,&Context::splinter_cell_blacklist(),&mut ci,login_ex_request());
    assert!(matches!(result,Err(quazal::rmc::Error::AccessDenied)));assert_eq!(ci.user_id,None);
    runtime().block_on(pool.close());
    drop(service);drop(storage);drop(pool);drop(logger);
    capture.finish(&["Native LoginEx attempt","Authentication account not found","Native LoginEx authentication failed"],&[]);
}
#[test]
fn dpa_identity_native_actual_login_ex_success_producers() {
    let (capture,logger)=Capture::new("native-loginex-success");
    let (storage,pool)=fixture(logger.clone());
    let rt=runtime();rt.block_on(seed_plain(&pool));rt.block_on(approved_run(&storage,ID));
    let service=TicketGrantingProtocolServerImpl {storage:storage.clone()};
    let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    let result=service.login_ex_core(&logger,&Context::splinter_cell_blacklist(),&mut ci,login_ex_request());
    assert!(result.is_ok());assert_eq!(ci.user_id,Some(ID));
    rt.block_on(pool.close());
    drop(service);drop(storage);drop(pool);drop(logger);
    capture.finish(&["Native LoginEx attempt","Verifying legacy password","Native LoginEx authentication succeeded"],&[]);
}
#[test]
fn dpa_identity_native_actual_ticket_denial_has_no_principals() {
    let (capture,logger)=Capture::new("native-ticket-denied");
    let (storage,pool)=fixture(logger.clone());
    let service=TicketGrantingProtocolServerImpl {storage:storage.clone()};
    let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());ci.user_id=Some(ID+1);
    let result=service.request_ticket_core(&logger,&Context::splinter_cell_blacklist(),&mut ci,RequestTicketRequest {id_source:ID,id_target:SERVER_PID});
    assert!(matches!(result,Err(quazal::rmc::Error::AccessDenied)));assert_eq!(ci.user_id,Some(ID+1));
    runtime().block_on(pool.close());
    drop(service);drop(storage);drop(pool);drop(logger);
    capture.finish(&["Native ticket request denied: transport identity mismatch"],&[]);
}

fn invalid_credential_state(both:bool) {
    let (capture,logger)=Capture::new(if both {"credential-both"} else {"credential-neither"});
    let (storage,pool)=fixture(logger.clone());let rt=runtime();
    rt.block_on(async {
        seed_plain(&pool).await;
        if both { sqlx::query("UPDATE users SET password_hash='synthetic-unused-hash' WHERE id=?").bind(ID).execute(&pool).await.unwrap(); }
        else { sqlx::query("UPDATE users SET password=NULL WHERE id=?").bind(ID).execute(&pool).await.unwrap(); }
    });
    let service=TicketGrantingProtocolServerImpl {storage:storage.clone()};
    let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
    let result=service.login_ex_core(&logger,&Context::splinter_cell_blacklist(),&mut ci,login_ex_request());
    assert!(matches!(result,Err(quazal::rmc::Error::InternalError)));assert_eq!(ci.user_id,None);
    rt.block_on(pool.close());drop(service);drop(storage);drop(pool);drop(logger);
    capture.finish(&["Native LoginEx attempt", if both {"Error finding user password: account credential state has both password forms"}
        else {"Error finding user password: account credential state has no password material"}],&[]);
}
#[test]
fn dpa_identity_native_actual_missing_credential_error_has_no_principal() { invalid_credential_state(false); }
#[test]
fn dpa_identity_native_actual_ambiguous_credential_error_has_no_principal() { invalid_credential_state(true); }
