// Included only in storage::tests by the root's reversible source overlay.
// Actual production storage methods/SQLite; no service, socket, game or account data.
fn coverage2_join_order_case(mode: i64, join_before_first_expiry: bool) {
    let case = if join_before_first_expiry { "join-first" } else { "expire-first-control" };
    let storage = if let Some(root) = std::env::var_os("SCBL_JOIN_ORDER_TEST_DIR") {
        let root = std::path::PathBuf::from(root);
        assert!(root.is_absolute() && root.is_dir());
        let path = root.join(format!("mode-{mode}-{case}.db"));
        assert!(!path.exists(), "never reuse an existing fixture database");
        Storage::init_with_database_url(Logger::root(slog::Discard, slog::o!()), &format!("sqlite://{}?mode=rwc", path.display())).unwrap()
    } else {
        create_test_storage(&format!("join-order-mode-{mode}-{case}"))
    };
    insert_accounts(&storage, 3);
    for user in 1..=3 {
        maintenance_begin_run(&storage, user, &format!("c{mode:01x}{user:030x}"), mode);
        storage.register_urls(user, vec![format!("prudp:/address=192.0.2.{user};port=13000;sid=35;type=2")]).unwrap();
    }
    // Same supported setup as accepted_normal_invite_suppresses_quick_match_lobby_restore_until_join_completes.
    storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
    let departed = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_owned()).unwrap();
    storage.add_participants(1, 1, departed, vec![2], Vec::new()).unwrap();
    let target = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
    storage.leave_game_session(2, 1, departed).unwrap();
    let old_owner = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
    assert!(old_owner.accepted_invite.is_none());
    let invite = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, target)).unwrap().unwrap().unwrap();
    assert!(run(storage.accept_invite_async(2, invite)).unwrap().unwrap().is_some());
    if !join_before_first_expiry {
        // Positive control retains the old test's critical ordering unchanged.
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0);
        assert_eq!(storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)].accepted_invite,
            Some(PendingAcceptedNormalInvite { invite_id: invite, target_session_id: target }));
    }
    assert!(storage.join_game_session_unbounded(2, 1, target).unwrap());
    let (before_leader, before_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
    let mut before_ids = before_roster.into_iter().map(|user| user.id).collect::<Vec<_>>();
    before_ids.sort_unstable();
    assert_eq!((before_leader, before_ids), (3, vec![2, 3]), "setup must complete the actual new Party join");
    let consumed: i64 = run(sqlx::query_scalar("SELECT consumed_at IS NOT NULL FROM invites WHERE id=?").bind(invite).fetch_one(&storage.pool)).unwrap().unwrap();
    assert_eq!(consumed, 1, "the actual Join must consume the accepted invite");
    let expired = expire_pending_quick_match_member_leaves_for_test(&storage);
    let (leader, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
    let mut ids = roster.into_iter().map(|user| user.id).collect::<Vec<_>>();
    ids.sort_unstable();
    let restore_count: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver=2 AND kind=? AND consumed_at IS NULL")
        .bind(INVITE_KIND_LOBBY_RESTORE).fetch_one(&storage.pool)).unwrap().unwrap();
    let target_members = storage.game_session_diagnostics(1, target).unwrap().unwrap().2;
    let old_members = storage.game_session_diagnostics(1, departed).unwrap().unwrap().2;
    let pending = storage.pending_quick_match_member_leaves(1, departed);
    // Emit every side effect before assertions, so a first failure cannot hide the others.
    println!("JOIN_ORDER mode={mode} case={case} actual_storage=true join_before_first_expiry={join_before_first_expiry} consumed={consumed} expired={expired} leader={leader} expected_leader=3 roster={ids:?} expected_roster=[2, 3] kind4_count={restore_count} expected_kind4_count=0 target_members={target_members:?} old_members={old_members:?} pending={pending:?}");
    assert_eq!(old_members, vec![1], "the unrelated old room owner must be unchanged");
    assert_eq!(target_members, vec![2, 3], "the new target's native roster must survive");
    assert!(pending.is_empty(), "the obsolete departure should converge");
    assert_eq!(leader, 3, "old departure must not replace the completed new Party leader");
    assert_eq!(ids, vec![2, 3], "old departure must not split the completed new Party");
    assert_eq!(restore_count, 0, "old departure must not publish a competing kind-4 restore");
    assert_eq!(expired, 0, "completed new join needs no solo restore");
}

#[test]
fn coverage2_join_order_official_join_before_first_expiry() { coverage2_join_order_case(1, true); }
#[test]
fn coverage2_join_order_mod_join_before_first_expiry() { coverage2_join_order_case(2, true); }
#[test]
fn coverage2_join_order_official_expire_before_join_control() { coverage2_join_order_case(1, false); }
#[test]
fn coverage2_join_order_mod_expire_before_join_control() { coverage2_join_order_case(2, false); }

// Included only in storage::tests by the root's reversible source overlay.
// Actual production storage methods/SQLite; no service, socket, game or account data.
fn coverage4_invite_cleanup_case(mode: i64, cleanup: u8) {
    let join_before_first_expiry = true;
    let case = format!("cleanup-{cleanup}");
    let storage = if let Some(root) = std::env::var_os("SCBL_JOIN_ORDER_TEST_DIR") {
        let root = std::path::PathBuf::from(root);
        assert!(root.is_absolute() && root.is_dir());
        let path = root.join(format!("mode-{mode}-{case}.db"));
        assert!(!path.exists(), "never reuse an existing fixture database");
        Storage::init_with_database_url(Logger::root(slog::Discard, slog::o!()), &format!("sqlite://{}?mode=rwc", path.display())).unwrap()
    } else {
        create_test_storage(&format!("join-order-mode-{mode}-{case}"))
    };
    insert_accounts(&storage, 3);
    for user in 1..=3 {
        maintenance_begin_run(&storage, user, &format!("c{mode:01x}{user:030x}"), mode);
        storage.register_urls(user, vec![format!("prudp:/address=192.0.2.{user};port=13000;sid=35;type=2")]).unwrap();
    }
    // Same supported setup as accepted_normal_invite_suppresses_quick_match_lobby_restore_until_join_completes.
    storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
    let departed = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_owned()).unwrap();
    storage.add_participants(1, 1, departed, vec![2], Vec::new()).unwrap();
    let target = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
    storage.leave_game_session(2, 1, departed).unwrap();
    let old_owner = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
    assert!(old_owner.accepted_invite.is_none());
    let invite = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, target)).unwrap().unwrap().unwrap();
    assert!(run(storage.accept_invite_async(2, invite)).unwrap().unwrap().is_some());
    if !join_before_first_expiry {
        // Positive control retains the old test's critical ordering unchanged.
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0);
        assert_eq!(storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)].accepted_invite,
            Some(PendingAcceptedNormalInvite { invite_id: invite, target_session_id: target }));
    }
    assert!(storage.join_game_session_unbounded(2, 1, target).unwrap());
    let (before_leader, before_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
    let mut before_ids = before_roster.into_iter().map(|user| user.id).collect::<Vec<_>>();
    before_ids.sort_unstable();
    assert_eq!((before_leader, before_ids), (3, vec![2, 3]), "setup must complete the actual new Party join");
    let consumed: i64 = run(sqlx::query_scalar("SELECT consumed_at IS NOT NULL FROM invites WHERE id=?").bind(invite).fetch_one(&storage.pool)).unwrap().unwrap();
    assert_eq!(consumed, 1, "the actual Join must consume the accepted invite");
    // A normal next poll or unrelated invitation cleans consumed records before old departure expiry.
    match cleanup {
        0 => {},
        1 => { assert!(run(storage.take_invite_async(1)).unwrap().unwrap().is_none()); },
        2 => { assert!(run(storage.add_invite_async(3, 1, INVITE_KIND_LOBBY_PARTY, 1, target)).unwrap().unwrap().is_some()); },
        _ => unreachable!(),
    }
    let rows: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE id=?").bind(invite).fetch_one(&storage.pool)).unwrap().unwrap();
    println!("CLEANUP_INPUT mode={mode} cleanup={cleanup} invite_rows={rows}");
    assert_eq!(rows, if cleanup == 0 {1} else {0}, "the real public cleanup must remove the consumed invite");
    let expired = expire_pending_quick_match_member_leaves_for_test(&storage);
    let (leader, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
    let mut ids = roster.into_iter().map(|user| user.id).collect::<Vec<_>>();
    ids.sort_unstable();
    let restore_count: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver=2 AND kind=? AND consumed_at IS NULL")
        .bind(INVITE_KIND_LOBBY_RESTORE).fetch_one(&storage.pool)).unwrap().unwrap();
    let target_members = storage.game_session_diagnostics(1, target).unwrap().unwrap().2;
    let old_members = storage.game_session_diagnostics(1, departed).unwrap().unwrap().2;
    let pending = storage.pending_quick_match_member_leaves(1, departed);
    // Emit every side effect before assertions, so a first failure cannot hide the others.
    println!("JOIN_ORDER mode={mode} case={case} actual_storage=true join_before_first_expiry={join_before_first_expiry} consumed={consumed} expired={expired} leader={leader} expected_leader=3 roster={ids:?} expected_roster=[2, 3] kind4_count={restore_count} expected_kind4_count=0 target_members={target_members:?} old_members={old_members:?} pending={pending:?}");
    assert_eq!(old_members, vec![1], "the unrelated old room owner must be unchanged");
    assert_eq!(target_members, vec![2, 3], "the new target's native roster must survive");
    assert!(pending.is_empty(), "the obsolete departure should converge");
    assert_eq!(leader, 3, "old departure must not replace the completed new Party leader");
    assert_eq!(ids, vec![2, 3], "old departure must not split the completed new Party");
    assert_eq!(restore_count, 0, "old departure must not publish a competing kind-4 restore");
    assert_eq!(expired, 0, "completed new join needs no solo restore");
}

#[test]
fn coverage2_join_order_cleanup_official_control() { coverage4_invite_cleanup_case(1, 0); }
#[test]
fn coverage2_join_order_cleanup_official_poll() { coverage4_invite_cleanup_case(1, 1); }
#[test]
fn coverage2_join_order_cleanup_official_add() { coverage4_invite_cleanup_case(1, 2); }
#[test]
fn coverage2_join_order_cleanup_mod_control() { coverage4_invite_cleanup_case(2, 0); }
#[test]
fn coverage2_join_order_cleanup_mod_poll() { coverage4_invite_cleanup_case(2, 1); }
#[test]
fn coverage2_join_order_cleanup_mod_add() { coverage4_invite_cleanup_case(2, 2); }

#[test]
fn coverage4_invite_cleanup_completed_witness_boundaries() {
    for mode in [1, 2] {
        for case in ["joined", "expired-invite", "no-witness", "wrong-user", "wrong-type", "wrong-target", "same-departed", "destroyed", "left-target"] {
            let (storage, departed, target) = coverage2_join_control_setup(&format!("cleanup-boundary-{case}"), mode);
            let invite = coverage2_join_control_invite(&storage, target, true);
            assert!(storage.join_game_session_unbounded(2, 1, target).unwrap());
            if case == "expired-invite" { coverage2_join_control_expire_invite(&storage, invite); }
            else { assert!(run(storage.take_invite_async(1)).unwrap().unwrap().is_none()); }
            if case == "destroyed" {
                run(sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?").bind(target).execute(&storage.pool)).unwrap().unwrap();
            }
            if case == "left-target" {
                run(sqlx::query("DELETE FROM participants WHERE game_id=? AND user_id=2").bind(target).execute(&storage.pool)).unwrap().unwrap();
            }
            let expected = if case == "no-witness" {None} else {Some(PendingAcceptedNormalInvite {
                invite_id: invite, target_session_id: if case == "wrong-target" {u32::MAX} else {target},
            })};
            let is_joined = run(async {
                let mut tx = storage.begin_write().await?;
                let state = Storage::accepted_normal_invite_transition_async(&mut tx,
                    if case == "wrong-user" {4} else {2}, if case == "wrong-type" {2} else {1},
                    if case == "same-departed" {target} else {departed}, expected).await?;
                tx.rollback().await?;
                Ok::<bool, sqlx::Error>(matches!(state, Some(AcceptedNormalInviteTransition::Joined)))
            }).unwrap().unwrap();
            let want = matches!(case, "joined" | "expired-invite");
            println!("CLEANUP_BOUNDARY mode={mode} case={case} joined={is_joined} expected={want}");
            assert_eq!(is_joined, want, "wrong completion scope: {case}");
        }
    }
}

#[test]
fn coverage4_invite_cleanup_join_commit_failure_is_not_completion() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("cleanup-join-commit-failure", mode);
        let invite = coverage2_join_control_invite(&storage, target, true);
        run(sqlx::query("CREATE TABLE coverage4_join_commit_guard (value INTEGER REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED)").execute(&storage.pool)).unwrap().unwrap();
        run(sqlx::query("CREATE TRIGGER coverage4_join_commit_fault AFTER UPDATE OF consumed_at ON invites WHEN OLD.receiver=2 AND OLD.consumed_at IS NULL AND NEW.consumed_at IS NOT NULL BEGIN INSERT INTO coverage4_join_commit_guard VALUES(2147483647); END").execute(&storage.pool)).unwrap().unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, target).is_err(), "the real Join commit must fail");
        run(sqlx::query("DROP TRIGGER coverage4_join_commit_fault").execute(&storage.pool)).unwrap().unwrap();
        assert_eq!(storage.game_session_diagnostics(1, target).unwrap().unwrap().2, vec![3]);
        assert!(storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)].accepted_invite.is_some());
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0, "failed Join is pending, not committed");
        assert_eq!(storage.pending_quick_match_member_leaves(1, departed), vec![2]);
        coverage2_join_control_expire_invite(&storage, invite);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);
        assert_eq!(coverage2_join_control_roster(&storage, 2), (2, vec![2]));
        println!("CLEANUP_COMMIT_CONTROL mode={mode} actual_commit_failed=true membership_rolled_back=true pending_then_expired=true");
    }
}
