// Actual Storage/SQLite calls, not a listener/packet concurrency simulation.
// Root's maintain.py uses --test-threads=1; hooks are also scoped by Storage address.
type Coverage2LeaveCommitHook = (usize, bool, u32, u32, u32, std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
static COVERAGE2_LEAVE_COMMIT_HOOKS: std::sync::Mutex<Vec<Coverage2LeaveCommitHook>> = std::sync::Mutex::new(Vec::new());

pub(super) fn coverage2_join_leave_commit_checkpoint(storage: &Storage, committed: bool, user: u32, ty: u32, session: u32) {
    let identity = storage as *const Storage as usize;
    let hook = {
        let mut hooks = COVERAGE2_LEAVE_COMMIT_HOOKS.lock().unwrap();
        hooks.iter().position(|(owner, ok, u, t, s, _, _)| (*owner, *ok, *u, *t, *s) == (identity, committed, user, ty, session))
            .map(|index| hooks.remove(index))
    };
    if let Some((_, _, _, _, _, arrived, release)) = hook {
        arrived.send(()).unwrap();
        // Failure watchdog only: continuation is controlled by the paired channel.
        release.recv_timeout(Duration::from_secs(10)).expect("commit checkpoint was not released");
    }
}

fn coverage2_join_arm_commit(storage: &Storage, committed: bool, departed: u32) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (arrived, seen) = std::sync::mpsc::channel();
    let (resume, release) = std::sync::mpsc::channel();
    COVERAGE2_LEAVE_COMMIT_HOOKS.lock().unwrap().push((storage as *const Storage as usize, committed, 2, 1, departed, arrived, release));
    (seen, resume)
}

fn coverage2_join_publication_setup(label: &str, mode: i64) -> (Arc<Storage>, u32, u32) {
    let (storage, departed, target) = coverage2_join_control_setup(label, mode);
    assert!(storage.join_game_session_unbounded(2, 1, departed).unwrap());
    assert!(storage.pending_quick_match_member_leaves(1, departed).is_empty());
    (Arc::new(storage), departed, target)
}

fn coverage2_join_install_commit_fault(storage: &Storage, departed: u32) {
    run(sqlx::query("CREATE TABLE coverage2_join_commit_guard (value INTEGER REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED)")
        .execute(&storage.pool)).unwrap().unwrap();
    let sql = format!("CREATE TRIGGER coverage2_join_commit_fault AFTER DELETE ON participants WHEN OLD.user_id=2 AND OLD.game_id={departed} BEGIN INSERT INTO coverage2_join_commit_guard VALUES(2147483647); END");
    run(sqlx::query(&sql).execute(&storage.pool)).unwrap().unwrap();
}

#[test]
fn coverage2_join_publication_postcommit_concurrent_join_preserves_party() {
    for mode in [1, 2] {
        let (storage, departed, target) = coverage2_join_publication_setup("publication-concurrent", mode);
        let invite = coverage2_join_control_invite(&storage, target, true);
        let (seen, resume) = coverage2_join_arm_commit(&storage, true, departed);
        let (joined, left, at_commit) = std::thread::scope(|scope| {
            let owner = Arc::clone(&storage);
            let worker = scope.spawn(move || owner.leave_game_session(2, 1, departed));
            let ready = seen.recv_timeout(Duration::from_secs(5));
            let at_commit = storage.pending_quick_match_member_leaves(1, departed);
            let joined = if ready.is_ok() { storage.join_game_session_unbounded(2, 1, target) } else { Ok(false) };
            let _ = resume.send(());
            let left = worker.join();
            assert!(ready.is_ok(), "actual successful COMMIT checkpoint was not reached");
            (joined, left.unwrap(), at_commit)
        });
        assert!(joined.unwrap()); left.unwrap();
        let witness = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)].accepted_invite;
        let expired = expire_pending_quick_match_member_leaves_for_test(&storage);
        let (leader, roster) = coverage2_join_control_roster(&storage, 2);
        let kind4: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver=2 AND kind=? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE).fetch_one(&storage.pool)).unwrap().unwrap();
        println!("JOIN_PUBLICATION mode={mode} case=postcommit concurrent_storage=true same_listener=false at_commit={at_commit:?} witness={witness:?} expired={expired} leader={leader} roster={roster:?} kind4={kind4}");
        assert_eq!(leader, 3, "old Leave must not replace the completed Party after concurrent Join");
        assert_eq!(roster, vec![2, 3]); assert_eq!(kind4, 0); assert_eq!(expired, 0);
        assert_eq!(witness, Some(PendingAcceptedNormalInvite { invite_id: invite, target_session_id: target }));
        assert!(storage.pending_quick_match_member_leaves(1, departed).is_empty());
        assert_eq!(storage.game_session_diagnostics(1, departed).unwrap().unwrap().2, vec![1]);
        assert_eq!(storage.game_session_diagnostics(1, target).unwrap().unwrap().2, vec![2, 3]);
    }
}

#[test]
fn coverage2_join_publication_commit_error_restores_previous_marker_and_sql() {
    for mode in [1, 2] {
        for had_previous in [false, true] {
            let (storage, departed, _) = coverage2_join_publication_setup(&format!("publication-rollback-{had_previous}"), mode);
            // Explicit marker-state control, not an assertion that this seed arose
            // from another listener. SQL mutation and failed COMMIT remain real.
            if had_previous { storage.record_pending_quick_match_member_leave(1, departed, 2); }
            let before = storage.pending_quick_match_member_leaves.lock().unwrap().get(&(1, departed, 2)).copied();
            let party_before = coverage2_join_control_roster(&storage, 2);
            coverage2_join_install_commit_fault(&storage, departed);
            let (seen, resume) = coverage2_join_arm_commit(&storage, false, departed);
            let (left, staged) = std::thread::scope(|scope| {
                let owner = Arc::clone(&storage);
                let worker = scope.spawn(move || owner.leave_game_session(2, 1, departed));
                let ready = seen.recv_timeout(Duration::from_secs(5));
                let staged = storage.pending_quick_match_member_leaves.lock().unwrap().get(&(1, departed, 2)).copied();
                let _ = resume.send(());
                let left = worker.join();
                assert!(ready.is_ok(), "deferred FK must reach actual failed COMMIT, not an earlier SQL error");
                (left.unwrap(), staged)
            });
            let error = left.expect_err("deferred foreign key must reject COMMIT");
            assert!(format!("{error:?}").contains("FOREIGN KEY constraint failed"), "unexpected actual SQL error: {error:?}");
            let after = storage.pending_quick_match_member_leaves.lock().unwrap().get(&(1, departed, 2)).copied();
            assert_eq!(after.map(|p| (p.generation, p.recorded_at, p.accepted_invite)), before.map(|p| (p.generation, p.recorded_at, p.accepted_invite)), "failed publication must restore the exact previous marker or absence");
            assert_eq!(storage.game_session_diagnostics(1, departed).unwrap().unwrap().2, vec![1, 2]);
            assert_eq!(coverage2_join_control_roster(&storage, 2), party_before);
            let guards: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM coverage2_join_commit_guard").fetch_one(&storage.pool)).unwrap().unwrap();
            assert_eq!(guards, 0, "SQL effects of failed COMMIT must roll back too");
            println!("JOIN_PUBLICATION mode={mode} case=commit-error had_previous={had_previous} staged_generation={:?} restored_generation={:?} sql_rolled_back=true", staged.map(|p| p.generation), after.map(|p| p.generation));
        }
    }
}

#[test]
fn coverage2_join_publication_failed_old_leave_cannot_rewind_new_leave_generation() {
    for mode in [1, 2] {
        let (storage, departed, _) = coverage2_join_publication_setup("publication-new-generation", mode);
        coverage2_join_install_commit_fault(&storage, departed);
        let (seen, resume) = coverage2_join_arm_commit(&storage, false, departed);
        let (left, newer, old_generation, new_generation) = std::thread::scope(|scope| {
            let owner = Arc::clone(&storage);
            let worker = scope.spawn(move || owner.leave_game_session(2, 1, departed));
            let ready = seen.recv_timeout(Duration::from_secs(5));
            let old_generation = storage.pending_quick_match_member_leaves.lock().unwrap().get(&(1, departed, 2)).map(|p| p.generation);
            // The failed transaction has returned its error. The second actual
            // writer waits for SQLx's rollback, then performs a valid new Leave.
            let newer = if ready.is_ok() {
                run(sqlx::query("DROP TRIGGER coverage2_join_commit_fault").execute(&storage.pool)).unwrap()
                    .map(|_| ()).map_err(eyre::Error::from)
                    .and_then(|_| storage.leave_game_session(2, 1, departed))
            } else { Ok(()) };
            let new_generation = storage.pending_quick_match_member_leaves.lock().unwrap().get(&(1, departed, 2)).map(|p| p.generation);
            let _ = resume.send(());
            let left = worker.join();
            assert!(ready.is_ok(), "failed COMMIT checkpoint was not reached");
            (left.unwrap(), newer, old_generation, new_generation)
        });
        let error = left.expect_err("old Leave must retain its actual COMMIT error");
        assert!(format!("{error:?}").contains("FOREIGN KEY constraint failed"));
        newer.unwrap();
        let after = storage.pending_quick_match_member_leaves.lock().unwrap()[&(1, departed, 2)];
        assert_eq!(Some(after.generation), new_generation, "old rollback compensation cannot delete/replace a newer publication");
        assert_ne!(new_generation, old_generation);
        assert_eq!(after.accepted_invite, None);
        assert_eq!(storage.game_session_diagnostics(1, departed).unwrap().unwrap().2, vec![1]);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);
        assert_eq!(coverage2_join_control_roster(&storage, 2), (2, vec![2]));
        assert!(storage.pending_quick_match_member_leaves(1, departed).is_empty());
        println!("JOIN_PUBLICATION mode={mode} case=rollback-new-generation old_generation={old_generation:?} new_generation={new_generation:?} actual_second_leave=true newer_preserved=true cleanup_converged=true");
    }
}
