// Isolated SQLite fault injection; never opens an account or game database.
fn maintenance_cleanup_retry_fault(kind: &str) {
    let storage = create_test_storage(&format!("cleanup-retry-{kind}"));
    insert_users(&storage, 4);
    let first = storage.create_game_session(1, 1, "113 => 0;3 => 8;4 => 0".to_string()).unwrap();
    let second = storage.create_game_session(3, 1, "113 => 0;3 => 8;4 => 0".to_string()).unwrap();
    let old = Instant::now() - Duration::from_secs(60);
    match kind {
        "member" => {
            for (room, user) in [(first, 2), (second, 4)] {
                storage.record_pending_quick_match_member_leave(1, room, user);
                storage.pending_quick_match_member_leaves.lock().unwrap().get_mut(&(1, room, user)).unwrap().recorded_at = old;
            }
        }
        "migration" => {
            for room in [first, second] {
                storage.note_native_host_migration(1, room);
                storage.native_host_migrations.lock().unwrap().get_mut(&(1, room)).unwrap().started_at = old;
            }
        }
        "lobby" => {
            for (room, user) in [(first, 2), (second, 4)] {
                storage.lobby_restore_split_transitions.lock().unwrap().insert((1, room, user), LobbyRestoreSplitTransition {generation: storage.next_cleanup_generation(), started_at: old, removal_deferred: true});
            }
        }
        _ => unreachable!(),
    }
    // Renaming back is a recoverable schema fault in this owned fixture. Every
    // expiry path must perform an actual SQLite read before it can finish.
    run(sqlx::query("ALTER TABLE participants RENAME TO retry_fault_participants").execute(&storage.pool)).unwrap().unwrap();
    let result = match kind {
        "member" => storage.expire_pending_quick_match_member_leaves(),
        "migration" => storage.expire_native_host_migrations(),
        "lobby" => storage.expire_lobby_restore_split_transitions(),
        _ => unreachable!(),
    };
    let pending = match kind {
        "member" => storage.pending_quick_match_member_leaves.lock().unwrap().len(),
        "migration" => storage.native_host_migrations.lock().unwrap().len(),
        "lobby" => storage.lobby_restore_split_transitions.lock().unwrap().len(),
        _ => unreachable!(),
    };
    run(sqlx::query("ALTER TABLE retry_fault_participants RENAME TO participants").execute(&storage.pool)).unwrap().unwrap();
    println!("CLEANUP_RETRY kind={kind} db_error={} pending_after_error={pending} expected=2", result.is_err());
    assert!(result.is_err(), "the actual database failure must reach the caller");
    assert_eq!(pending, 2, "neither failed nor unvisited work may be lost");
    let drain = || match kind {
        "member" => storage.expire_pending_quick_match_member_leaves(),
        "migration" => storage.expire_native_host_migrations(),
        "lobby" => storage.expire_lobby_restore_split_transitions(),
        _ => unreachable!(),
    };
    drain().expect("recovered database must allow retry");
    let remaining = match kind {
        "member" => storage.pending_quick_match_member_leaves.lock().unwrap().len(),
        "migration" => storage.native_host_migrations.lock().unwrap().len(),
        "lobby" => storage.lobby_restore_split_transitions.lock().unwrap().len(),
        _ => unreachable!(),
    };
    assert_eq!(remaining, 0);
    assert_eq!(drain().unwrap(), 0);
    println!("CLEANUP_RETRY kind={kind} recovered_pending=0 repeat=0");
}
#[test]
fn maintenance_cleanup_retry_member_database_failure() { maintenance_cleanup_retry_fault("member"); }
#[test]
fn maintenance_cleanup_retry_migration_database_failure() { maintenance_cleanup_retry_fault("migration"); }
#[test]
fn maintenance_cleanup_retry_lobby_database_failure() { maintenance_cleanup_retry_fault("lobby"); }

#[test]
fn maintenance_cleanup_retry_cancellation_and_replacement_are_not_resurrected() {
    let storage = create_test_storage("cleanup-generation-ownership");
    storage.note_native_host_migration(1, 10);
    let old = storage.native_host_migrations.lock().unwrap()[&(1, 10)].generation;
    storage.clear_native_host_migration(1, 10);
    complete_cleanup(&storage.native_host_migrations, &(1, 10), old, |item| item.generation);
    assert!(storage.native_host_migrations.lock().unwrap().is_empty());
    storage.note_native_host_migration(1, 10);
    let new = storage.native_host_migrations.lock().unwrap()[&(1, 10)].generation;
    assert_ne!(old, new);
    complete_cleanup(&storage.native_host_migrations, &(1, 10), old, |item| item.generation);
    assert_eq!(storage.native_host_migrations.lock().unwrap()[&(1, 10)].generation, new);
    assert!(!cleanup_is_current(&storage.native_host_migrations, &(1, 10), old, |item| item.generation));
    println!("CLEANUP_GENERATION canceled_stays_absent=true replacement_survives_old_ack=true");
}

#[test]
fn maintenance_cleanup_retry_one_worker_does_not_block_another_poll() {
    let storage = Arc::new(create_test_storage("cleanup-worker-overlap"));
    let worker = storage.begin_cleanup().unwrap().unwrap();
    let other = Arc::clone(&storage);
    let (send, receive) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        send.send((other.expire_pending_quick_match_member_leaves().unwrap(), other.expire_native_host_migrations().unwrap(), other.expire_lobby_restore_split_transitions().unwrap())).unwrap();
    });
    assert_eq!(receive.recv_timeout(Duration::from_secs(2)).unwrap(), (0, 0, 0));
    drop(worker);
    thread.join().unwrap();
    assert!(storage.begin_cleanup().unwrap().is_some());
    println!("CLEANUP_WORKER competing_poll_returns=true gate_released=true");
}

#[test]
fn maintenance_cleanup_retry_guard_runs_after_writer_admission() {
    let storage = Arc::new(create_test_storage("cleanup-guard-writer-order"));
    insert_users(&storage, 2);
    let room = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
    storage.add_participants(1, 1, room, vec![2], Vec::new()).unwrap();
    for path in ["remove", "delete", "leave"] {
        let blocker = run(storage.begin_write()).unwrap().unwrap();
        let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (sent, seen) = std::sync::mpsc::channel();
        let (done, result) = std::sync::mpsc::channel();
        let other = Arc::clone(&storage); let cancel = Arc::clone(&canceled);
        let thread = std::thread::spawn(move || {
            let allow = || { let value = !cancel.load(Ordering::SeqCst); sent.send(value).unwrap(); value };
            match path {
                "remove" => assert!(!run(other.remove_participants_checked_async(1, 1, room, vec![2], allow)).unwrap().unwrap()),
                "delete" => assert_eq!(other.delete_game_session_checked(1, 1, room, allow).unwrap(), DeleteGameSessionOutcome::NotFound),
                "leave" => other.leave_game_session_checked(2, 1, room, false, allow).unwrap(),
                _ => unreachable!(),
            }
            done.send(()).unwrap();
        });
        if path != "remove" { assert!(seen.recv_timeout(Duration::from_secs(2)).unwrap()); }
        // The transaction is held, so final admission cannot have happened.
        assert!(seen.recv_timeout(Duration::from_millis(80)).is_err());
        canceled.store(true, Ordering::SeqCst);
        run(blocker.rollback()).unwrap().unwrap();
        assert!(!seen.recv_timeout(Duration::from_secs(2)).unwrap());
        result.recv_timeout(Duration::from_secs(2)).unwrap(); thread.join().unwrap();
        assert_eq!(storage.game_session_diagnostics(1, room).unwrap().unwrap().2, vec![1,2]);
        println!("CLEANUP_GUARD path={path} canceled_while_waiting=true members_unchanged=true");
    }
}
    #[test]
    fn maintenance_cleanup_retry_native_partial_database_failure() {
        let storage = create_test_storage("native-migration-expired-fallback");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();
        storage.abandon_game_session(1, 1, room_id).unwrap();

        storage
            .native_host_migrations
            .lock()
            .unwrap()
            .get_mut(&(1, room_id)).unwrap().started_at = Instant::now() - NATIVE_HOST_MIGRATION_TTL - Duration::from_secs(1);
        assert!(!storage.native_host_migration_is_active(1, room_id));
        assert!(storage.native_host_migration_cleanup_due());
        run(sqlx::query("CREATE TRIGGER retry_participant_fault BEFORE DELETE ON participants WHEN OLD.user_id=3 BEGIN SELECT RAISE(ABORT, 'owned-cleanup-fault'); END").execute(&storage.pool)).unwrap().unwrap();
        assert!(storage.expire_native_host_migrations().is_err());
        assert!(storage.native_host_migration_cleanup_due());
        println!("CLEANUP_PARTIAL after_error={:?}", storage.game_session_diagnostics(1, room_id).unwrap());
        run(sqlx::query("DROP TRIGGER retry_participant_fault").execute(&storage.pool)).unwrap().unwrap();
        assert_eq!(storage.expire_native_host_migrations().unwrap(), 1);
        assert!(!storage.game_session_is_active(1, room_id).unwrap());

        run(sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE receiver = 2 AND kind = ?")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .execute(&storage.pool))
        .unwrap()
        .unwrap();
        let leader_restore = run(storage.take_invite_async(2)).unwrap().unwrap().expect("N-1 leader restore");
        assert_eq!(leader_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert!(run(storage.accept_invite_async(2, leader_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, leader_restore.session_id).unwrap());

        run(sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE receiver = 3 AND kind = ?")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .execute(&storage.pool))
        .unwrap()
        .unwrap();
        let restore = run(storage.take_invite_async(3)).unwrap().unwrap().expect("N-1 follower restore");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert!(run(storage.accept_invite_async(3, restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, restore.session_id).unwrap());

        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);
    }

