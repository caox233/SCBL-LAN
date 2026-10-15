// Append-only test module for the existing frozen batch6 secure-close fixture.
// Reuses its real WirePeer, schema, ticket issuance and SQL snapshot helpers.
// No production patch. No live service: synchronous dispatch + loopback ACKs only.
mod host_presence_pending_ticket {
    use super::*;
    use quazal::rmc::basic::FromStream;
    use crate::protocols::secure_connection_service::secure_connection_protocol::{RegisterRequest, RegisterResponse};

    #[derive(Clone, Copy, Debug)]
    enum Sweep { None, Users, GameReady }

    fn presence_server<'a>(ctx: &'a Context, logger: &'a slog::Logger, storage: &'a Arc<Storage>) -> NativeServer<'a> {
        let mut handler = quazal::rmc::RVSecHandler::<()>::new(logger.clone());
        handler.register_protocol(crate::secure::new_protocol(Arc::clone(storage)));
        let mut registry = StreamHandlerRegistry::new(logger.clone());
        registry.register(VPort { port: 1, stream_type: StreamType::RVSec }, Box::new(handler));
        let mut transport = NativeServer::new(logger.clone(), ctx, registry);
        configure_secure_close_handler(&mut transport, logger, storage);
        let mut real_close = transport.close_handler.take().unwrap();
        transport.close_handler = Some(Box::new(move |ci, reason| {
            let committed = real_close(ci, reason);
            println!("F-SRV-004 close_callback user={:?} reason={reason:?} may_forget_owner={committed}", ci.user_id);
            committed
        }));
        transport.bind("127.0.0.1:0").unwrap();
        transport
    }

    // Dispatch into the actual transport owner, RVSec handler and Secure Register.
    // Do not manufacture a replacement ClientInfo or call mark_secure_ticket directly.
    fn register(peer: &WirePeer, transport: &mut NativeServer<'_>, ctx: &Context, signature: u32, sequence: u16) -> Result<u32, u32> {
        let request = quazal::rmc::Request { protocol_id: 11, call_id: u32::from(sequence), method_id: 1,
            parameters: RegisterRequest { vec_my_urls: vec![] }.to_bytes() };
        peer.dispatch(transport, ctx, QPacket { signature, session_id: 7, sequence,
            fragment_id: Some(0), // Data wire field: standalone/final payload, not absent.
            flags: PacketFlag::HasSize | PacketFlag::NeedAck,
            payload: request.to_bytes(), ..WirePeer::packet(PacketType::Data) });
        let mut buffer = [0_u8; 4096];
        let mut saw_ack = false;
        for _ in 0..3 {
            let (n, source) = peer.socket.recv_from(&mut buffer).expect("real Register response on fixture loopback");
            assert!(source.ip().is_loopback());
            let (packet, _) = QPacket::from_bytes(ctx, &buffer[..n]).unwrap();
            packet.validate(ctx, &buffer[..n]).unwrap();
            assert_eq!(packet.packet_type, PacketType::Data);
            if packet.flags.contains(PacketFlag::Ack) { saw_ack = true; continue; }
            assert!(saw_ack, "the real request ACK preceded the real RMC response");
            let response = quazal::rmc::Response::from_bytes(&packet.payload).unwrap();
            assert_eq!(response.protocol_id, 11);
            return match response.result {
                Ok(data) => {
                    assert_eq!(data.call_id, u32::from(sequence));
                    assert_eq!(data.method_id, 1);
                    let result = RegisterResponse::from_bytes(&data.data).unwrap();
                    assert_eq!(result.url_public.port, peer.address().port());
                    println!("F-SRV-004 Register call={} result=Ok connection_id={}", sequence, result.pid_connection_id);
                    Ok(result.pid_connection_id)
                }
                Err(error) => {
                    assert_eq!(error.call_id, u32::from(sequence));
                    println!("F-SRV-004 Register call={} error=0x{:08x}", sequence, error.error_code);
                    Err(error.error_code)
                }
            };
        }
        panic!("no complete Register response");
    }

    fn snapshot(label: &str, pool: &SqlitePool, session: u32) {
        let tickets: Vec<(i64, String, i64, i64)> = run(sqlx::query_as(
            "SELECT user_id,ticket_hash,game_run_id,native_connected FROM user_sessions ORDER BY user_id,ticket_hash").fetch_all(pool)).unwrap();
        let users: Vec<(i64, i64, i64)> = run(sqlx::query_as(
            "SELECT id,is_online,CASE WHEN last_api_seen_at>=datetime('now','-15 seconds') THEN 1 ELSE 0 END FROM users WHERE id IN(1000,1001) ORDER BY id").fetch_all(pool)).unwrap();
        let room: Vec<(i64, i64, i64)> = run(sqlx::query_as(
            "SELECT id,creator_id,CASE WHEN destroyed_at IS NULL THEN 0 ELSE 1 END FROM game_sessions WHERE id=?").bind(session).fetch_all(pool)).unwrap();
        let participants: Vec<(i64,)> = run(sqlx::query_as(
            "SELECT user_id FROM participants WHERE game_id=? ORDER BY user_id").bind(session).fetch_all(pool)).unwrap();
        let urls: Vec<(i64, String)> = run(sqlx::query_as(
            "SELECT user_id,url FROM station_urls ORDER BY user_id,url").fetch_all(pool)).unwrap();
        let runs: Vec<(i64, i64, i64, i64)> = run(sqlx::query_as(
            "SELECT id,user_id,CASE WHEN retired_at IS NULL THEN 0 ELSE 1 END,CASE WHEN seen_at>=datetime('now','-90 seconds') THEN 1 ELSE 0 END FROM game_runs ORDER BY id").fetch_all(pool)).unwrap();
        println!("F-SRV-004 SNAPSHOT {}", serde_json::json!({"at":label,"tickets":tickets,"users":users,"room":room,"participants":participants,"urls":urls,"runs":runs}));
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Mutation {
        None, Expired, Retired, WrongRelease, WrongUserRun,
        OrphanExpired, OrphanValid, ReplaceRun, ReplaceIp,
        FreshApiExpired, OnlineRegisteredExpired,
    }

    // The original six calls retain their exact input and production path.
    fn pending_case(name: &str, subject_is_host: bool, sweep: Sweep, register_before_close: bool, fresh_api: bool) {
        pending_case_with_mutation(name, subject_is_host, sweep, register_before_close, fresh_api, Mutation::None);
    }

    fn pending_case_with_mutation(name: &str, subject_is_host: bool, sweep: Sweep, register_before_close: bool, fresh_api: bool, mutation: Mutation) {
        let (storage, pool) = fixture();
        let ctx = Context::splinter_cell_blacklist();
        let logger = slog::Logger::root(slog::Discard, slog::o!());
        let mut transport = presence_server(&ctx, &logger, &storage);
        let old_fp = client(&storage, 1000, 201, false).ticket_fingerprint.unwrap();
        let peer_fp = client(&storage, 1001, 203, false).ticket_fingerprint.unwrap();
        let old = WirePeer::new(); let old_sig = old.syn(&mut transport, &ctx).unwrap();
        assert!(old.connect(&mut transport, &ctx, old_sig, 1000, 201));
        assert!(register(&old, &mut transport, &ctx, old_sig, 2).is_ok());
        let peer = WirePeer::new(); let peer_sig = peer.syn(&mut transport, &ctx).unwrap();
        assert!(peer.connect(&mut transport, &ctx, peer_sig, 1001, 203));
        assert!(register(&peer, &mut transport, &ctx, peer_sig, 2).is_ok());
        let creator = if subject_is_host { 1000 } else { 1001 };
        let follower = if subject_is_host { 1001 } else { 1000 };
        let session = storage.create_game_session(creator, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(follower, 1, session).unwrap());
        let initial_room = storage.game_session_diagnostics(1, session).unwrap().unwrap();
        assert_eq!(initial_room.0, creator); assert_eq!(initial_room.2, vec![1000, 1001]);
        let new_fp = client(&storage, 1000, 202, false).ticket_fingerprint.unwrap();
        let new = WirePeer::new(); let new_sig = new.syn(&mut transport, &ctx).unwrap();
        assert!(new.connect(&mut transport, &ctx, new_sig, 1000, 202));
        assert_eq!(ticket_count(&pool, new_fp), 1);
        if register_before_close { assert!(register(&new, &mut transport, &ctx, new_sig, 2).is_ok()); }
        if fresh_api { run(storage.touch_api_presence_async(1000)).unwrap(); }
        println!("F-SRV-004 CASE {name} host={subject_is_host} sweep={sweep:?} pre_registered={register_before_close} api_fresh={fresh_api}");
        snapshot("before-old-close", &pool, session);
        // Sequence 2 was Register. Use a distinct actual Disconnect sequence.
        old.dispatch(&mut transport, &ctx, QPacket { signature: old_sig, session_id: 7, sequence: 3,
            ..WirePeer::packet(PacketType::Disconnect) });
        assert!(old.ack(&ctx, PacketType::Disconnect).is_some());
        assert!(transport.maintenance_owner(old_sig).is_none());
        assert_eq!(ticket_count(&pool, old_fp), 0);
        assert_eq!(ticket_count(&pool, new_fp), 1, "v7 exact close must still preserve new unregistered ticket");
        let after_close = storage.game_session_diagnostics(1, session).unwrap().unwrap();
        assert_eq!(after_close.0, creator);
        assert!(after_close.2.contains(&1001));
        if subject_is_host { assert_eq!(after_close.2, vec![1000, 1001]); }
        else { assert_eq!(after_close.2, vec![1001]); }
        snapshot("after-exact-close", &pool, session);
        if matches!(mutation, Mutation::OrphanExpired | Mutation::OrphanValid) {
            // Real host-migration API removes the old host's last participant.
            // No synthetic DELETE stands in for the production transition.
            assert!(storage.migrate_game_session_host(1001, 1, session).unwrap());
            let migrated = storage.game_session_diagnostics(1, session).unwrap().unwrap();
            assert_eq!(migrated.0, 1001);
            assert_eq!(migrated.2, vec![1001]);
            assert_eq!(count(&pool, "SELECT COUNT(*) FROM participants WHERE user_id=1000"), 0);
            assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 0);
            assert_eq!(ticket_count(&pool, new_fp), 1);
        }
        // Mutations affect synthetic fixture lease/identity only. Cleanup,
        // Register, migration and run revocation still execute actual source.
        match mutation {
            Mutation::Expired | Mutation::OrphanExpired | Mutation::FreshApiExpired | Mutation::OnlineRegisteredExpired => {
                run(sqlx::query("UPDATE game_runs SET seen_at=datetime('now','-120 seconds') WHERE user_id=1000 AND retired_at IS NULL").execute(&pool)).unwrap();
            },
            Mutation::Retired => {
                run(sqlx::query("UPDATE game_runs SET retired_at=CURRENT_TIMESTAMP WHERE user_id=1000 AND retired_at IS NULL").execute(&pool)).unwrap();
            },
            Mutation::WrongRelease => {
                // Release identity is immutable in the actual schema. Retire
                // the former approved run, insert a CURRENT/FRESH same-user run
                // with only the release gate invalid, and bind the pending ticket
                // to that run. Do not disable/drop the production trigger.
                run(async {
                    let mut tx = pool.begin().await.unwrap();
                    sqlx::query("UPDATE game_runs SET retired_at=CURRENT_TIMESTAMP WHERE user_id=1000 AND retired_at IS NULL").execute(&mut *tx).await.unwrap();
                    sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip,release_identity) VALUES(1000,'fixture-nonapproved-current',2,'127.0.0.1','fixture-nonapproved-release')").execute(&mut *tx).await.unwrap();
                    sqlx::query("UPDATE user_sessions SET game_run_id=(SELECT id FROM game_runs WHERE user_id=1000 AND retired_at IS NULL) WHERE ticket_hash=?")
                        .bind(sodiumoxide::hex::encode(new_fp)).execute(&mut *tx).await.unwrap();
                    tx.commit().await.unwrap();
                });
                assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions t JOIN game_runs r ON r.id=t.game_run_id WHERE t.user_id=1000 AND r.user_id=t.user_id AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds') AND r.release_identity='fixture-nonapproved-release'"), 1);
            },
            Mutation::WrongUserRun => {
                run(sqlx::query("UPDATE user_sessions SET game_run_id=(SELECT id FROM game_runs WHERE user_id=1001 AND retired_at IS NULL) WHERE ticket_hash=?")
                    .bind(sodiumoxide::hex::encode(new_fp)).execute(&pool)).unwrap();
            },
            Mutation::ReplaceRun | Mutation::ReplaceIp => {
                let _mode_guard = storage.mode_gate.blocking_write();
                let instance = if mutation == Mutation::ReplaceRun { "0000000000000000000000000000abcd".to_string() } else { format!("{:032x}", 1000) };
                let source = if mutation == Mutation::ReplaceIp { "127.0.0.2" } else { "127.0.0.1" };
                run(storage.begin_game_run(1000, &instance, 2, source.parse().unwrap())).unwrap();
                assert_eq!(ticket_count(&pool, new_fp), 0, "original run/IP revocation must happen before presence");
                assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1000"), 0);
            },
            Mutation::None | Mutation::OrphanValid => {},
        }
        if mutation == Mutation::FreshApiExpired {
            assert_eq!(count(&pool, "SELECT CASE WHEN last_api_seen_at>=datetime('now','-15 seconds') THEN 1 ELSE 0 END FROM users WHERE id=1000"), 1);
            assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
        }
        if mutation == Mutation::OnlineRegisteredExpired {
            assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 1);
            assert_eq!(count(&pool, "SELECT native_connected FROM user_sessions WHERE user_id=1000"), 1);
        }
        let expected_room = storage.game_session_diagnostics(1, session).unwrap().unwrap();
        snapshot("after-mutation-before-presence", &pool, session);
        match sweep {
            Sweep::None => {},
            Sweep::Users => { let users=run(storage.list_users_async()).unwrap();println!("F-SRV-004 list_users returned={}", users.len()); },
            Sweep::GameReady => { let users=run(storage.list_game_ready_users_async()).unwrap();println!("F-SRV-004 list_game_ready_users returned={}", users.len()); },
        }
        snapshot("after-presence-before-Register", &pool, session);
        assert_eq!(ticket_count(&pool, peer_fp), 1, "observer/surviving companion ticket changed");
        assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1001"), 1);
        let after_sweep = storage.game_session_diagnostics(1, session).unwrap().unwrap();
        assert_eq!(after_sweep, expected_room, "presence sweep changed remaining room/companion state");
        let new_ticket_before_register = ticket_count(&pool, new_fp);
        let sequence = if register_before_close { 3 } else { 2 };
        let result = register(&new, &mut transport, &ctx, new_sig, sequence);
        snapshot("after-Register", &pool, session);
        let fresh_run:i64 = count(&pool,"SELECT COUNT(*) FROM game_runs WHERE user_id=1000 AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds')");
        println!("F-SRV-004 VERDICT {name} fresh_run={fresh_run} pending_ticket_before_register={new_ticket_before_register} register={result:?} companion_unchanged=true");
        if matches!(mutation, Mutation::None | Mutation::OrphanValid) {
            assert_eq!(fresh_run, 1, "test lease expired: invalid reproduction");
            assert!(result.is_ok(), "fresh approved-run ticket was accepted by real CONNECT but rejected by actual Register after presence sweep: {result:?}");
            assert_eq!(new_ticket_before_register, 1);
        } else {
            let keep = matches!(mutation, Mutation::FreshApiExpired | Mutation::OnlineRegisteredExpired);
            println!("F-SRV-004 CONTROL {name} mutation={mutation:?} actual_ticket={new_ticket_before_register} expected_ticket={} register={result:?}", i64::from(keep));
            assert_eq!(result, Err(0x8001_0006), "invalid/revoked ticket must fail actual Register");
            assert_eq!(new_ticket_before_register, i64::from(keep), "pending pruning/revocation scope differs from contract");
        }
    }

    #[test] fn fsrv004_host_pending_ticket_survives_list_users() { pending_case("host-users", true, Sweep::Users, false, false); }
    #[test] fn fsrv004_host_pending_ticket_survives_game_ready_roster() { pending_case("host-game-ready", true, Sweep::GameReady, false, false); }
    #[test] fn fsrv004_control_no_presence_query() { pending_case("no-query", true, Sweep::None, false, false); }
    #[test] fn fsrv004_control_non_host() { pending_case("non-host", false, Sweep::Users, false, false); }
    #[test] fn fsrv004_control_already_registered() { pending_case("already-registered", true, Sweep::Users, true, false); }
    #[test] fn fsrv004_control_fresh_api_heartbeat() { pending_case("fresh-api", true, Sweep::Users, false, true); }

    #[test] fn fsrv004_expired_pending_is_pruned() { pending_case_with_mutation("expired", true, Sweep::Users, false, false, Mutation::Expired); }
    #[test] fn fsrv004_retired_pending_is_pruned() { pending_case_with_mutation("retired", true, Sweep::Users, false, false, Mutation::Retired); }
    #[test] fn fsrv004_nonapproved_pending_is_pruned() { pending_case_with_mutation("wrong-release", true, Sweep::Users, false, false, Mutation::WrongRelease); }
    #[test] fn fsrv004_wrong_user_run_pending_is_pruned() { pending_case_with_mutation("wrong-user-run", true, Sweep::Users, false, false, Mutation::WrongUserRun); }
    #[test] fn fsrv004_orphan_expired_pending_is_pruned_after_real_migration() { pending_case_with_mutation("orphan-expired", true, Sweep::Users, false, false, Mutation::OrphanExpired); }
    #[test] fn fsrv004_orphan_valid_pending_survives_real_migration() { pending_case_with_mutation("orphan-valid", true, Sweep::Users, false, false, Mutation::OrphanValid); }
    #[test] fn fsrv004_new_run_still_revokes_all_old_tickets() { pending_case_with_mutation("new-run", true, Sweep::Users, false, false, Mutation::ReplaceRun); }
    #[test] fn fsrv004_source_ip_change_still_revokes_all_old_tickets() { pending_case_with_mutation("new-ip", true, Sweep::Users, false, false, Mutation::ReplaceIp); }
    #[test] fn fsrv004_fresh_api_excludes_invalid_pending_prune() { pending_case_with_mutation("fresh-api-expired", true, Sweep::Users, false, true, Mutation::FreshApiExpired); }
    #[test] fn fsrv004_online_registered_invalid_ticket_is_not_swept() { pending_case_with_mutation("online-registered-expired", true, Sweep::Users, true, false, Mutation::OnlineRegisteredExpired); }
}
