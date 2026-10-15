// Additional controls; the original four RED tests stay separately frozen.
fn coverage2_join_control_setup(label: &str, mode: i64) -> (Storage, u32, u32) {
    let storage = create_test_storage(&format!("join-order-{label}-mode-{mode}"));
    insert_accounts(&storage, 4);
    for user in 1..=4 {
        maintenance_begin_run(&storage, user, &format!("d{mode:01x}{user:030x}"), mode);
        storage.register_urls(user, vec![format!("prudp:/address=192.0.2.{user};port=13000;sid=35;type=2")]).unwrap();
    }
    storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
    let departed = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_owned()).unwrap();
    storage.add_participants(1, 1, departed, vec![2], Vec::new()).unwrap();
    let target = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
    storage.leave_game_session(2, 1, departed).unwrap();
    assert!(storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)].accepted_invite.is_none());
    (storage, departed, target)
}

fn coverage2_join_control_invite(storage: &Storage, target: u32, accept: bool) -> i64 {
    let invite = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, target)).unwrap().unwrap().unwrap();
    if accept { assert!(run(storage.accept_invite_async(2, invite)).unwrap().unwrap().is_some()); }
    invite
}

fn coverage2_join_control_roster(storage: &Storage, user: u32) -> (u32, Vec<u32>) {
    let (leader, roster) = run(storage.find_party_roster_async(user)).unwrap().unwrap().unwrap();
    let mut ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
    ids.sort_unstable(); (leader, ids)
}

fn coverage2_join_control_expire_invite(storage: &Storage, invite: i64) {
    run(sqlx::query("UPDATE invites SET expires_at=datetime('now','-1 second') WHERE id=?")
        .bind(invite).execute(&storage.pool)).unwrap().unwrap();
}

#[test]
fn coverage2_join_control_sql_rollback_keeps_pending_then_expires() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("rollback", mode);
        let invite = coverage2_join_control_invite(&storage, target, true);
        let before = coverage2_join_control_roster(&storage, 2);
        run(sqlx::query("CREATE TRIGGER join_order_consume_fault BEFORE UPDATE OF consumed_at ON invites WHEN NEW.id = OLD.id AND OLD.receiver=2 AND OLD.consumed_at IS NULL AND NEW.consumed_at IS NOT NULL BEGIN SELECT RAISE(ABORT, 'synthetic join consume failure'); END")
            .execute(&storage.pool)).unwrap().unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, target).is_err());
        run(sqlx::query("DROP TRIGGER join_order_consume_fault").execute(&storage.pool)).unwrap().unwrap();
        assert_eq!(coverage2_join_control_roster(&storage, 2), before, "real SQL rollback restores the old Party");
        assert_eq!(storage.game_session_diagnostics(1, target).unwrap().unwrap().2, vec![3]);
        let consumed: i64 = run(sqlx::query_scalar("SELECT consumed_at IS NOT NULL FROM invites WHERE id=?").bind(invite).fetch_one(&storage.pool)).unwrap().unwrap();
        assert_eq!(consumed, 0);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0, "uncommitted Join is only pending, not completed");
        assert_eq!(storage.pending_quick_match_member_leaves(1, departed), vec![2]);
        coverage2_join_control_expire_invite(&storage, invite);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1, "failed admission must not shield cleanup forever");
        assert!(storage.pending_quick_match_member_leaves(1, departed).is_empty());
        assert_eq!(coverage2_join_control_roster(&storage, 2), (2, vec![2]));
        println!("JOIN_CONTROL mode={mode} case=rollback actual_sql_fault=true rollback_verified=true pending_then_expired=true");
    }
}

#[test]
fn coverage2_join_control_unaccepted_invite_does_not_shield_departure() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("not-accepted", mode);
        coverage2_join_control_invite(&storage, target, false);
        let old = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert!(storage.join_game_session_unbounded(2, 1, target).unwrap());
        let pending = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert_eq!(pending.generation, old.generation);
        assert_eq!(pending.accepted_invite, None);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);
        assert_eq!(coverage2_join_control_roster(&storage, 2), (2, vec![2]));
        println!("JOIN_CONTROL mode={mode} case=not-accepted ordinary_solo_cleanup=true");
    }
}

#[test]
fn coverage2_join_control_wrong_type_missing_and_nonowner_admission_do_not_mark() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("denied", mode);
        let invite = coverage2_join_control_invite(&storage, target, true);
        let old = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert!(!storage.join_game_session_unbounded(2, 2, target).unwrap());
        assert!(!storage.join_game_session_unbounded(2, 1, u32::MAX).unwrap());
        // Non-owner AddParticipants is acknowledged without mutation by contract.
        storage.add_participants(1, 1, target, vec![2], Vec::new()).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, target).unwrap().unwrap().2, vec![3]);
        let pending = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert_eq!(pending.generation, old.generation);
        assert_eq!(pending.accepted_invite, None);
        coverage2_join_control_expire_invite(&storage, invite);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);
        println!("JOIN_CONTROL mode={mode} case=denied wrong_type=true missing=true nonowner=true cleanup_converged=true");
    }
}

#[test]
fn coverage2_join_control_owner_add_participants_uses_same_witness() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("owner-admission", mode);
        coverage2_join_control_invite(&storage, target, true);
        storage.add_participants(3, 1, target, vec![2], Vec::new()).unwrap();
        assert_eq!(coverage2_join_control_roster(&storage, 2), (3, vec![2, 3]));
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0);
        assert_eq!(coverage2_join_control_roster(&storage, 2), (3, vec![2, 3]));
        assert!(storage.pending_quick_match_member_leaves(1, departed).is_empty());
        println!("JOIN_CONTROL mode={mode} case=owner-admission completed_party_preserved=true");
    }
}

#[test]
fn coverage2_join_control_other_user_type_and_old_snapshot_are_isolated() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("scope", mode);
        storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_owned()).unwrap();
        storage.add_participants(1, 1, departed, vec![4], Vec::new()).unwrap();
        storage.leave_game_session(4, 1, departed).unwrap();
        let other_type = storage.create_game_session(1, 2, "113 => 0;3 => 12;4 => 0".to_owned()).unwrap();
        storage.add_participants(1, 2, other_type, vec![2], Vec::new()).unwrap();
        storage.leave_game_session(2, 2, other_type).unwrap();
        let (old, other_user, other_scope) = {
            let map = storage.pending_quick_match_member_leaves.lock().unwrap();
            (map[&(1, departed, 2)], map[&(1, departed, 4)], map[&(2, other_type, 2)])
        };
        let invite = coverage2_join_control_invite(&storage, target, true);
        assert!(storage.join_game_session_unbounded(2, 1, target).unwrap());
        let current = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert_ne!(old.generation, current.generation, "old expiry snapshot must be invalidated");
        assert_eq!(old.recorded_at, current.recorded_at, "witness must not renew departure TTL");
        assert_eq!(current.accepted_invite, Some(PendingAcceptedNormalInvite { invite_id: invite, target_session_id: target }));
        // This is the actual shared generation guard, not an entire threaded worker test.
        assert!(!cleanup_is_current(&storage.pending_quick_match_member_leaves, &(1, departed, 2), old.generation, |item| item.generation));
        complete_cleanup(&storage.pending_quick_match_member_leaves, &(1, departed, 2), old.generation, |item| item.generation);
        let map = storage.pending_quick_match_member_leaves.lock().unwrap();
        assert_eq!(map[&(1, departed, 2)].generation, current.generation);
        assert_eq!(map[&(1, departed, 4)].generation, other_user.generation);
        assert_eq!(map[&(1, departed, 4)].accepted_invite, other_user.accepted_invite);
        assert_eq!(map[&(2, other_type, 2)].generation, other_scope.generation);
        assert_eq!(map[&(2, other_type, 2)].accepted_invite, other_scope.accepted_invite);
        println!("JOIN_CONTROL mode={mode} case=scope other_user_unchanged=true other_type_unchanged=true old_snapshot_rejected=true");
    }
}

#[test]
fn coverage2_join_control_later_real_departure_replaces_old_witness() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_control_setup("new-generation", mode);
        coverage2_join_control_invite(&storage, target, true);
        assert!(storage.join_game_session_unbounded(2, 1, target).unwrap());
        let old = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert!(storage.join_game_session_unbounded(2, 1, departed).unwrap());
        storage.leave_game_session(2, 1, departed).unwrap();
        let new = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert_ne!(old.generation, new.generation);
        assert_eq!(new.accepted_invite, None, "new leave cannot inherit an earlier Join witness");
        complete_cleanup(&storage.pending_quick_match_member_leaves, &(1, departed, 2), old.generation, |item| item.generation);
        assert_eq!(storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)].generation, new.generation);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);
        assert_eq!(coverage2_join_control_roster(&storage, 2), (2, vec![2]));
        println!("JOIN_CONTROL mode={mode} case=new-generation actual_join_leave=true old_ack_cannot_remove=true new_leave_cleanup=true");
    }
}
