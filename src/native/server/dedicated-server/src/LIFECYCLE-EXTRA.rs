// Include at the bottom of api::login_reset_tests: include!("LIFECYCLE-EXTRA.rs");
// Narrow credential contract only: reset does not retire prior native runs.
mod lifecycle_extra {
    use super::*;
    use std::time::Instant;

    #[test]
    fn fsrv002_extra_queued_reset_writer_precedes_old_api_and_native_publication() {
        for native in [false,true] {
            let (storage,pool)=crate::storage::release_test_support::fixture();
            runtime().block_on(async {
                seed(&storage,&pool,false).await;
                if native {service(storage.clone()).login(login_request(NAME,OLD,GameplayMode::Mod.product(),NONCE)).await.unwrap();}
            });
            let (entered,release)=pause(&storage,"publication");
            let worker_storage=storage.clone();
            let old=std::thread::spawn(move || {
                if native {
                    let (response,bound)=ticket(worker_storage,"LoginEx",NAME,OLD,1000,Some(1001));
                    (matches!(response,Err(quazal::rmc::Error::AccessDenied)),bound==Some(1001))
                } else {
                    let response=runtime().block_on(service(worker_storage).login(login_request(NAME,OLD,GameplayMode::Mod.product(),NONCE)));
                    (matches!(response,Err(ref status) if status.code()==tonic::Code::Unauthenticated),true)
                }
            });
            entered.recv_timeout(Duration::from_secs(60)).expect("verified old login did not reach publication seam");
            let queued=runtime().block_on(async {
                let reader=storage.mode_gate.read().await;
                let reset_storage=storage.clone();
                let reset=tokio::spawn(async move {service(reset_storage).reset_password(reset_request()).await});
                let deadline=Instant::now()+Duration::from_secs(60);
                // While this reader is held, try_read failing is evidence of a
                // queued writer, not a writer that has already committed reset.
                let queued=loop {
                    if storage.mode_gate.try_read().is_err() {break true;}
                    if reset.is_finished() || Instant::now()>=deadline {break false;}
                    tokio::task::yield_now().await;
                };
                release.send(()).unwrap();
                drop(reader);
                reset.await.unwrap().unwrap();
                queued
            });
            let (rejected,binding_preserved)=old.join().unwrap();
            let state=runtime().block_on(counts(&pool,1000));
            println!("FSRV002 EXTRA queued_writer native={native} queued={queued} old_rejected={rejected} binding_preserved={binding_preserved} state={state:?}");
            assert!(queued,"real reset returned without queuing behind mode_gate reader");
            assert!(rejected && binding_preserved);
            assert_eq!(state,if native {(1,0,0)} else {(0,0,0)});
        }
    }

    #[test]
    fn fsrv002_extra_same_recovery_code_concurrent_reset_has_exactly_one_winner() {
        let (storage,pool)=crate::storage::release_test_support::fixture();
        runtime().block_on(async {
            seed(&storage,&pool,false).await;
            let before=sqlx::query_as::<_,(String,i64)>("SELECT password,auth_version FROM users WHERE id=1001").fetch_one(&pool).await.unwrap();
            let start=Arc::new(tokio::sync::Barrier::new(3));
            let mut tasks=Vec::new();
            for password in ["synthetic-reset-racer-a","synthetic-reset-racer-b"] {
                let s=storage.clone();let gate=start.clone();
                tasks.push(tokio::spawn(async move {
                    gate.wait().await;
                    let result=service(s).reset_password(Request::new(users::ResetPasswordRequest{username:NAME.into(),recovery_code:RECOVERY.into(),new_password:password.into()})).await;
                    (password,result)
                }));
            }
            start.wait().await;
            let mut winners=Vec::new();let mut losers=Vec::new();
            for task in tasks {
                let (password,result)=task.await.unwrap();
                match result {Ok(response)=>winners.push((password,response.into_inner().recovery_code)),Err(status)=>{assert_eq!(status.code(),tonic::Code::Unauthenticated);losers.push(password);}}
            }
            assert_eq!(winners.len(),1);assert_eq!(losers.len(),1);
            let (version,hash):(i64,String)=sqlx::query_as("SELECT auth_version,recovery_code_hash FROM users WHERE id=1000").fetch_one(&pool).await.unwrap();
            assert_eq!(version,2,"one recovery code must increment credential version exactly once");
            assert_eq!(hash,hash_recovery_code(&winners[0].1));
            let users=service(storage.clone());
            assert!(users.login(login_request(NAME,winners[0].0,ACCOUNT_PRODUCT,NONCE)).await.is_ok());
            for wrong in [OLD,losers[0]] {assert_eq!(users.login(login_request(NAME,wrong,ACCOUNT_PRODUCT,NONCE)).await.unwrap_err().code(),tonic::Code::Unauthenticated);}
            assert_eq!(users.reset_password(reset_request()).await.unwrap_err().code(),tonic::Code::Unauthenticated);
            assert_eq!(sqlx::query_as::<_,(String,i64)>("SELECT password,auth_version FROM users WHERE id=1001").fetch_one(&pool).await.unwrap(),before);
            println!("FSRV002 EXTRA concurrent_recovery success=1 rejected=1 version_increment=1 only_winner_password=true other_user_unchanged=true");
        });
    }
}
