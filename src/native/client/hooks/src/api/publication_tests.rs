    use super::*;
    use server_api::misc::*;
    use std::sync::{Arc, atomic::{AtomicBool,AtomicUsize,Ordering}};
    use std::time::Duration;
    use tonic::{Request,Response,Status};
    #[derive(Default)]
    struct State {
        failures:AtomicUsize, sets:AtomicUsize, clears:AtomicUsize, events:AtomicUsize,
        active:AtomicUsize, maximum:AtomicUsize, hold:AtomicBool,
        remote:Mutex<Option<UplayGameSession>>,
    }
    #[derive(Clone)]
    struct Fixture(Arc<State>);
    impl Fixture {
        async fn attempt(&self)->Result<(),Status> {
            let s=&self.0;
            let count=s.active.fetch_add(1,Ordering::SeqCst)+1;s.maximum.fetch_max(count,Ordering::SeqCst);
            while s.hold.load(Ordering::SeqCst){tokio::time::sleep(Duration::from_millis(5)).await;}
            tokio::time::sleep(Duration::from_millis(5)).await;
            let failed=s.failures.fetch_update(Ordering::SeqCst,Ordering::SeqCst,|n|n.checked_sub(1)).is_ok();
            s.active.fetch_sub(1,Ordering::SeqCst);
            if failed{Err(Status::unavailable("synthetic publication-only failure"))}else{Ok(())}
        }
        fn auth<T>(r:&Request<T>){assert_eq!(r.metadata().get("authorization").unwrap(),"fixture-token");}
    }
    #[tonic::async_trait]
    impl misc_server::Misc for Fixture {
        async fn event(&self,r:Request<EventRequest>)->Result<Response<EventResponse>,Status>{
            Self::auth(&r);self.0.events.fetch_add(1,Ordering::SeqCst);
            Ok(Response::new(EventResponse{invite:Some(InviteEvent{id:7,kind:3,force_join:true,..Default::default()}),party:None}))
        }
        async fn set_game_session(&self,r:Request<SetGameSessionRequest>)->Result<Response<SetGameSessionResponse>,Status>{
            Self::auth(&r);self.0.sets.fetch_add(1,Ordering::SeqCst);self.attempt().await?;let r=r.into_inner();
            *self.0.remote.lock().unwrap()=Some(UplayGameSession{id:r.id,data:r.data,flags:r.flags,invite_only:r.invite_only});
            Ok(Response::new(SetGameSessionResponse{}))
        }
        async fn clear_game_session(&self,r:Request<ClearGameSessionRequest>)->Result<Response<ClearGameSessionResponse>,Status>{
            Self::auth(&r);self.0.clears.fetch_add(1,Ordering::SeqCst);self.attempt().await?;
            *self.0.remote.lock().unwrap()=None;Ok(Response::new(ClearGameSessionResponse{}))
        }
        async fn accept_invite(&self,_:Request<AcceptInviteRequest>)->Result<Response<AcceptInviteResponse>,Status>{Err(Status::unimplemented("fixture"))}
        async fn test_p2p(&self,_:Request<TestP2pRequest>)->Result<Response<TestP2pResponse>,Status>{Err(Status::unimplemented("fixture"))}
    }
    fn session(id:u64)->UplayGameSession{UplayGameSession{id,data:vec![0x5a;496],flags:7,invite_only:true}}
    async fn settle() {
        let _lock=GAME_SESSION_PUBLISH_LOCK.get_or_init(||tokio::sync::Mutex::new(())).lock().await;
    }
    fn reset(s:&State,remote:Option<UplayGameSession>,failures:usize) {
        *GAME_SESSION_PUBLISH_STATE.lock().unwrap()=GameSessionPublishState::default();
        *s.remote.lock().unwrap()=remote;s.failures.store(failures,Ordering::SeqCst);
        for n in [&s.sets,&s.clears,&s.events,&s.active,&s.maximum]{n.store(0,Ordering::SeqCst);}
    }
    async fn polls(s:&State,count:usize) {
        for _ in 0..count{
            let response=tokio::time::timeout(Duration::from_secs(2),event()).await.expect("event delivery blocked by publication").unwrap();
            let invite=response.invite.unwrap();assert_eq!(invite.kind,3);assert!(invite.force_join);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(s.events.load(Ordering::SeqCst),count);
    }
    fn report(case:&str,s:&State,expected:Option<UplayGameSession>) {
        let state=GAME_SESSION_PUBLISH_STATE.lock().unwrap();let pending=state.pending().is_some();
        let remote=s.remote.lock().unwrap();
        println!("PUBLICATION case={case} pending={pending} remote_matches_latest={} set_calls={} clear_calls={} event_success={} max_inflight={} actual_client_rpc=true scripted_service=true game=false",
            *remote==expected,s.sets.load(Ordering::SeqCst),s.clears.load(Ordering::SeqCst),s.events.load(Ordering::SeqCst),s.maximum.load(Ordering::SeqCst));
    }
    #[test]
    fn deferred_publication_recovers_without_stalling_events() {
        let s=Arc::new(State::default());let service=Fixture(s.clone());
        let (ready_tx,ready_rx)=std::sync::mpsc::sync_channel(1);let(stop_tx,stop_rx)=tokio::sync::oneshot::channel();
        let server=std::thread::spawn(move||tokio::runtime::Runtime::new().unwrap().block_on(async move{
            let incoming=tonic::transport::server::TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
            ready_tx.send(incoming.local_addr().unwrap()).unwrap();
            tonic::transport::Server::builder().add_service(misc_server::MiscServer::new(service))
                .serve_with_incoming_shutdown(incoming,async{let _=stop_rx.await;}).await.unwrap();
        }));
        let addr=ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        crate::config::URL.set(format!("http://{addr}").parse().unwrap()).unwrap();
        *TOKEN.lock().unwrap()=Some("fixture-token".parse().unwrap());
        runtime().unwrap().block_on(async{
            for case in ["set_transient","clear_transient","coalesced_latest","healthy_no_pending","persistent_failure"] {
                settle().await;
                reset(&s,if case=="clear_transient"{Some(session(1))}else{None},if case=="persistent_failure"{10}else{1});
                if case!="healthy_no_pending"{
                    GAME_SESSION_PUBLISH_STATE.lock().unwrap().update(if case=="clear_transient"{None}else{Some(session(1))});
                    synchronize_game_session("fixture_initial").await;
                    assert!(GAME_SESSION_PUBLISH_STATE.lock().unwrap().pending().is_some());
                }
                if case=="coalesced_latest"{
                    let mut state=GAME_SESSION_PUBLISH_STATE.lock().unwrap();state.update(None);state.update(Some(session(3)));
                }
                polls(&s,3).await;settle().await;
                let expected=match case{"clear_transient"|"healthy_no_pending"=>None,"coalesced_latest"=>Some(session(3)),_=>Some(session(1))};
                report(case,&s,expected.clone());
                if case=="persistent_failure" {
                    assert!(GAME_SESSION_PUBLISH_STATE.lock().unwrap().pending().is_some());
                    assert_eq!(s.sets.load(Ordering::SeqCst),4);
                } else {
                    assert!(GAME_SESSION_PUBLISH_STATE.lock().unwrap().pending().is_none());
                    assert_eq!(*s.remote.lock().unwrap(),expected);
                    let calls=s.sets.load(Ordering::SeqCst)+s.clears.load(Ordering::SeqCst);
                    assert_eq!(calls,if case=="healthy_no_pending"{0}else{2});
                }
            }
            reset(&s,None,0);s.hold.store(true,Ordering::SeqCst);
            GAME_SESSION_PUBLISH_STATE.lock().unwrap().update(Some(session(9)));
            let task=tokio::spawn(synchronize_game_session("fixture_slow"));
            for _ in 0..200{if s.active.load(Ordering::SeqCst)==1{break;}tokio::time::sleep(Duration::from_millis(5)).await;}
            assert_eq!(s.active.load(Ordering::SeqCst),1);
            polls(&s,3).await;assert_eq!(s.sets.load(Ordering::SeqCst),1);assert_eq!(s.maximum.load(Ordering::SeqCst),1);
            s.hold.store(false,Ordering::SeqCst);task.await.unwrap();settle().await;
            report("slow_inflight_no_duplicate",&s,Some(session(9)));
        });
        let _=stop_tx.send(());server.join().unwrap();
    }
