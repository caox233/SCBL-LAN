
    include!("retry_tests.rs");
    include!("search_tests.rs");
    #[test]
    fn maintenance_hex_ticket_storage_observation() {
        let fixtures:[([u8;16],&str,&str);3]=[([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"00000000000000000000000000000000","374708fff7719dd5979ec875d56cd2286f6d3cf7ec317a3b25632aab28ec37bb"),
([255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255],"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF","5ac6a5945f16500911219129984ba8b387a06f24fe383ce4e81a73294065461b"),
([10,27,40,57,78,95,108,125,130,147,160,177,198,215,228,245],"0A1B28394E5F6C7D8293A0B1C6D7E4F5","aad6398ac058a9189d003a4f3c959b3cc4a0ff7155606176a89f1752960e4a6c")];
        for mode in [1,2] {
            for (case,(key,expected_key,expected_hash)) in fixtures.iter().enumerate() {
                let s=create_test_storage("hex-case-observation");insert_users(&s,2);
                maintenance_begin_run(&s,1,"11111111111111111111111111111111",mode);
                let ip="127.0.0.1".parse().unwrap();
                s.create_bound_user_session(1,key,ip).unwrap();
                let fingerprint=quazal::kerberos::ticket_fingerprint(key);
                let row:(String,String,i64)=run(sqlx::query_as("SELECT id,ticket_hash,native_connected FROM user_sessions WHERE user_id=1").fetch_one(&s.pool)).unwrap().unwrap();
                assert_eq!(row.0,*expected_key);assert_eq!(row.1,*expected_hash);assert_eq!(row.2,0);
                assert!(s.native_game_run(1,Some(fingerprint),ip).unwrap().is_none());
                assert!(!s.mark_secure_ticket(1,None).unwrap());
                assert!(!s.mark_secure_ticket(2,Some(fingerprint)).unwrap());
                assert!(s.mark_secure_ticket(1,Some(fingerprint)).unwrap());
                assert_eq!(s.native_game_run(1,Some(fingerprint),ip).unwrap().unwrap().mode,mode);
                assert!(s.native_game_run(2,Some(fingerprint),ip).unwrap().is_none());
                assert!(s.native_game_run(1,Some(fingerprint),"127.0.0.2".parse().unwrap()).unwrap().is_none());
                s.close_secure_ticket(1,Some(fingerprint)).unwrap();
                assert!(s.native_game_run(1,Some(fingerprint),ip).unwrap().is_none());
                let remaining:i64=run(sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE user_id=1").fetch_one(&s.pool)).unwrap().unwrap();assert_eq!(remaining,0);
                println!("HEX_OBSERVATION mode={mode} case={case} key_upper_exact=true hash_lower_exact=true connection_gate=true user_source_gate=true close_removed=true");
            }
        }
    }

    use std::sync::Arc;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use super::*;

    fn create_test_storage(name: &str) -> Storage {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("5th-echelon-{name}-{}-{unique}.db", std::process::id()));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        Storage::init_with_database_url(Logger::root(slog::Discard, slog::o!()), &url).unwrap()
    }

    #[test]
    fn fresh_database_keeps_tracking_service_principal_but_no_development_players() {
        let storage = create_test_storage("no-demo-seed-data");
        run(async {
            let demo_users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username IN ('Foo', 'AAAABBBB', 'sam_the_fisher')")
                .fetch_one(&storage.pool)
                .await?;
            let tracking_principal: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = 105 AND username = 'Tracking' AND password = 'JaDe!' AND password_hash IS NULL AND ubi_id IS NULL")
                    .fetch_one(&storage.pool)
                    .await?;
            let demo_sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM game_sessions WHERE id = 1 AND creator_id = 1000")
                .fetch_one(&storage.pool)
                .await?;
            assert_eq!(demo_users, 0);
            assert_eq!(tracking_principal, 1);
            assert_eq!(demo_sessions, 0);
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();
    }

    #[test]
    fn fresh_player_registration_uses_the_proven_non_service_principal_range() {
        let storage = create_test_storage("player-principal-range");
        run(async {
            storage
                .register_user_with_recovery_async("first-player", "password", Some("first-account"), Some("first-recovery"))
                .await?;
            storage
                .register_user_with_recovery_async("second-player", "password", Some("second-account"), Some("second-recovery"))
                .await?;

            let principals: Vec<(String, u32)> = sqlx::query_as("SELECT username, id FROM users WHERE username IN ('first-player', 'second-player') ORDER BY id")
                .fetch_all(&storage.pool)
                .await?;

            assert_eq!(principals, vec![("first-player".to_string(), 1000), ("second-player".to_string(), 1001)]);
            assert!(principals.iter().all(|(_, id)| !matches!(*id, 1 | 2 | 105 | 0x1000)));
            Ok::<(), eyre::Report>(())
        })
        .unwrap()
        .unwrap();
    }





    fn insert_accounts(storage: &Storage, count: u32) {
        run(async {
            for user_id in 1..=count {
                sqlx::query("INSERT OR REPLACE INTO users (id, username, password_hash, ubi_id) VALUES (?, ?, ?, ?)")
                    .bind(user_id)
                    .bind(format!("user-{user_id}"))
                    .bind("test-password-hash")
                    .bind(format!("ubi-{user_id}"))
                    .execute(&storage.pool)
                    .await?;
            }
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();
    }

    fn insert_users(storage: &Storage, count: u32) {
        insert_accounts(storage, count);
        // Lifecycle fixtures model authenticated game players, not legacy accounts.
        // Mode-specific tests deliberately replace this distinct empty run first.
        for user_id in 1..=count {
            maintenance_begin_run(storage, user_id, &format!("f{user_id:031x}"), 1);
        }
    }

    fn expire_pending_quick_match_member_leaves_for_test(storage: &Storage) -> usize {
        for pending in storage.pending_quick_match_member_leaves.lock().unwrap().values_mut() {
            pending.recorded_at = Instant::now() - GAME_ROOM_DEPARTURE_CORRELATION_TTL - Duration::from_millis(1);
        }
        assert!(storage.pending_quick_match_member_leave_cleanup_due());
        storage.expire_pending_quick_match_member_leaves().unwrap()
    }

    #[test]
    fn sqlite_pool_applies_safety_pragmas_to_every_connection() {
        let storage = create_test_storage("pool-safety-pragmas");
        run(async {
            // Keep the acquisitions alive together so the pool must provide
            // distinct physical connections instead of reusing one handle.
            let mut connections = Vec::new();
            for _ in 0..3 {
                connections.push(storage.pool.acquire().await?);
            }

            for connection in &mut connections {
                let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(&mut **connection).await?;
                let busy_timeout_ms: i64 = sqlx::query_scalar("PRAGMA busy_timeout").fetch_one(&mut **connection).await?;
                let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode").fetch_one(&mut **connection).await?;

                assert_eq!(foreign_keys, 1);
                assert_eq!(busy_timeout_ms, 5_000);
                assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
            }
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();
    }

    #[test]
    fn immediate_write_admission_waits_for_the_current_writer() {
        let storage = Arc::new(create_test_storage("immediate-write-admission"));
        insert_users(&storage, 2);
        let session_id = storage.create_game_session(1, 1, String::new()).unwrap();

        let mut blocker = run(storage.begin_write()).unwrap().unwrap();
        run(async {
            sqlx::query("UPDATE users SET last_api_seen_at = CURRENT_TIMESTAMP WHERE id = 1")
                .execute(&mut *blocker)
                .await?;
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();

        let worker_storage = Arc::clone(&storage);
        let worker = std::thread::spawn(move || worker_storage.add_participants(1, 1, session_id, Vec::new(), vec![2]));
        std::thread::sleep(Duration::from_millis(100));
        run(blocker.commit()).unwrap().unwrap();
        worker.join().expect("writer thread must not panic").expect("second writer must wait and then commit");

        let (_, _, participants) = storage.game_session_diagnostics(1, session_id).unwrap().unwrap();
        assert_eq!(participants, vec![1, 2]);
    }

    #[test]
    fn join_session_records_twelve_participants_without_capacity_gate() {
        let storage = create_test_storage("join-twelve-participants");
        insert_users(&storage, 12);
        for user_id in 1..=12 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=14;type=2")])
                .unwrap();
        }

        let session_id = storage.create_game_session(1, 42, "101 => 1".to_string()).unwrap();
        for user_id in 2..=12 {
            assert!(storage.join_game_session_unbounded(user_id, 42, session_id).unwrap());
        }
        // A repeated JoinSession and a later AddParticipants callback must not
        // inflate the participant count.
        assert!(storage.join_game_session_unbounded(12, 42, session_id).unwrap());
        storage.add_participants(1, 42, session_id, Vec::new(), vec![12]).unwrap();

        let (_, _, participant_ids) = storage.game_session_diagnostics(42, session_id).unwrap().unwrap();
        assert_eq!(participant_ids, (1..=12).collect::<Vec<_>>());
        let sessions_with_twelfth = storage.search_sessions_with_participants(42, &[12]).unwrap();
        assert_eq!(sessions_with_twelfth.len(), 1);
        assert_eq!(sessions_with_twelfth[0].participants.len(), 12);

        assert!(!storage.join_game_session_unbounded(12, 42, session_id + 10_000).unwrap());
    }

    #[test]
    fn supports_twelve_participants_without_capacity_gate() {
        let storage = create_test_storage("twelve-participants");
        insert_users(&storage, 12);
        for user_id in 1..=12 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=14;type=2")])
                .unwrap();
        }

        let session_id = storage.create_game_session(1, 42, "101 => 1".to_string()).unwrap();
        for user_id in 2..=12 {
            storage.add_participants(1, 42, session_id, Vec::new(), vec![user_id]).unwrap();
        }

        let (_, _, participant_ids) = storage.game_session_diagnostics(42, session_id).unwrap().unwrap();
        assert_eq!(participant_ids, (1..=12).collect::<Vec<_>>());
        assert!(storage.game_session_is_active(42, session_id).unwrap());

        let all_sessions = storage.search_sessions_with_participants(42, &[]).unwrap();
        assert_eq!(all_sessions.len(), 1);
        assert_eq!(all_sessions[0].participants.len(), 12);
        let sessions_with_twelfth = storage.search_sessions_with_participants(42, &[12]).unwrap();
        assert_eq!(sessions_with_twelfth.len(), 1);
    }

    #[test]
    fn leave_waits_for_each_native_migration_and_final_leave_destroys_session() {
        let storage = create_test_storage("host-migration");
        insert_users(&storage, 3);
        let session_id = storage.create_game_session(1, 7, String::new()).unwrap();
        storage.add_participants(1, 7, session_id, Vec::new(), vec![2, 3]).unwrap();

        storage.leave_game_session(1, 7, session_id).unwrap();
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(2, 7, session_id).unwrap());

        storage.leave_game_session(2, 7, session_id).unwrap();
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 2);
        assert_eq!(participant_ids, vec![2, 3]);
        assert!(storage.migrate_game_session_host(3, 7, session_id).unwrap());

        storage.leave_game_session(3, 7, session_id).unwrap();
        assert!(!storage.game_session_is_active(7, session_id).unwrap());
        assert!(storage.game_session_diagnostics(7, session_id).unwrap().is_none());
    }

    #[test]
    fn only_current_session_owner_can_remove_participants() {
        let storage = create_test_storage("remove-participants-owner");
        insert_users(&storage, 4);
        let session_id = storage.create_game_session(1, 7, String::new()).unwrap();
        storage.add_participants(1, 7, session_id, Vec::new(), vec![2, 3]).unwrap();

        assert!(!storage.remove_participants(2, 7, session_id, vec![3]).unwrap());
        assert!(!storage.remove_participants(4, 7, session_id, vec![3]).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3]);

        assert!(storage.remove_participants(1, 7, session_id, vec![3, 3]).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2]);

        assert!(!storage.remove_participants(1, 7, session_id + 1, vec![2]).unwrap());
    }

    #[test]
    fn native_owner_migration_revokes_previous_owner_removal_rights() {
        let storage = create_test_storage("remove-participants-after-owner-migration");
        insert_users(&storage, 3);
        let session_id = storage.create_game_session(1, 7, String::new()).unwrap();
        storage.add_participants(1, 7, session_id, Vec::new(), vec![2, 3]).unwrap();

        storage.leave_game_session(1, 7, session_id).unwrap();
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(2, 7, session_id).unwrap());

        assert!(!storage.remove_participants(1, 7, session_id, vec![3]).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 2);
        assert_eq!(participant_ids, vec![2, 3]);

        assert!(storage.remove_participants(2, 7, session_id, vec![3]).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(7, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 2);
        assert_eq!(participant_ids, vec![2]);
    }

    #[test]
    fn split_restores_callers_own_just_abandoned_session() {
        let storage = create_test_storage("split-restore-abandoned");
        insert_users(&storage, 2);
        let session_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        storage.leave_game_session(1, 1, session_id).unwrap();
        assert!(!storage.game_session_is_active(1, session_id).unwrap());
        assert!(!storage.restore_abandoned_game_session_for_split(2, 1, session_id).unwrap());
        assert!(storage.restore_abandoned_game_session_for_split(1, 1, session_id).unwrap());

        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, session_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1]);
    }

    #[test]
    fn deleting_hosted_game_preserves_remaining_players_for_native_migration() {
        let storage = create_test_storage("host-delete-native-migration");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, game_id).unwrap());
        assert!(storage.join_game_session_unbounded(3, 1, game_id).unwrap());

        assert_eq!(
            storage.delete_game_session(1, 1, game_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, game_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3]);
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());

        // SplitSession is the first and only ownership decision.
        assert!(storage.migrate_game_session_host(3, 1, game_id).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, game_id).unwrap().unwrap();
        assert_eq!(creator_id, 3);
        assert_eq!(participant_ids, vec![2, 3]);
    }

    #[test]
    fn successful_four_player_host_migration_keeps_match_and_suppresses_lobby_regroup() {
        let storage = create_test_storage("four-player-host-migration-solo-fallbacks");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let mut solo_lobbies = Vec::new();
        for receiver in 2..=4 {
            solo_lobbies.push((receiver, storage.create_game_session(receiver, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap()));
        }
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();

        for (receiver, lobby_id) in &solo_lobbies {
            let invite_id = run(storage.add_invite_async(1, *receiver, INVITE_KIND_PRIVATE_ROOM, 1, game_id)).unwrap().unwrap().unwrap();
            assert_eq!(run(storage.take_invite_async(*receiver)).unwrap().unwrap().unwrap().id, invite_id);
            assert!(run(storage.accept_invite_async(*receiver, invite_id)).unwrap().unwrap().is_some());

            // Retail sends both AbandonSession and LeaveSession for the old
            // solo lobby while it is crossing into the private match.
            storage.leave_game_session(*receiver, 1, *lobby_id).unwrap();
            storage.leave_game_session(*receiver, 1, *lobby_id).unwrap();
            assert!(storage.game_session_is_active(1, *lobby_id).unwrap());
        }

        storage.add_participants(1, 1, game_id, vec![2, 3, 4], Vec::new()).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2, 3, 4]);

        for (receiver, lobby_id) in &solo_lobbies {
            // The accepted invite is consumed after admission. A late retail
            // lobby Leave is still harmless because room membership now owns
            // the fallback lifetime.
            storage.leave_game_session(*receiver, 1, *lobby_id).unwrap();
            assert!(storage.game_session_is_active(1, *lobby_id).unwrap());
        }

        assert_eq!(
            storage.delete_game_session(1, 1, game_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 3 }
        );
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2, 3, 4]);
        for (_, lobby_id) in &solo_lobbies {
            assert!(storage.game_session_is_active(1, *lobby_id).unwrap());
        }

        // A member may leave after the migration without affecting the two
        // players who remain in the migrated match.
        storage.leave_game_session(4, 1, game_id).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(3, 1, game_id).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, game_id).unwrap().unwrap();
        assert_eq!(creator_id, 3);
        assert_eq!(participant_ids, vec![2, 3]);
        // SplitSession itself is the migration contract. The server must not
        // inject a second invitation state machine while retail is migrating.
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());

        // StateLeaveWithoutParty may create a fresh solo lobby. That creation
        // retires only this player's preserved predecessor, preventing two
        // personal lobbies without touching the migrated match or teammates.
        let old_user_four_lobby = solo_lobbies[2].1;
        let replacement = storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(!storage.game_session_is_active(1, old_user_four_lobby).unwrap());
        assert!(storage.game_session_is_active(1, replacement).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![2, 3]);
    }

    #[test]
    fn two_player_host_migration_completes_after_the_only_survivor_becomes_host() {
        let storage = create_test_storage("two-player-host-migration-confirmation");
        insert_users(&storage, 2);
        for user_id in 1..=2 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, game_id).unwrap());
        assert!(matches!(
            storage.delete_game_session(1, 1, game_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 1 }
        ));
        assert!(storage.migrate_game_session_host(2, 1, game_id).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![2]);
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
    }

    #[test]
    fn deleting_hosted_game_does_not_restore_player_who_already_left() {
        let storage = create_test_storage("host-delete-no-restore-after-leave");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, game_id).unwrap());
        storage.leave_game_session(2, 1, game_id).unwrap();

        assert_eq!(storage.delete_game_session(1, 1, game_id).unwrap(), DeleteGameSessionOutcome::Destroyed);
        assert!(!storage.game_session_is_active(1, game_id).unwrap());
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
    }

    #[test]
    fn deleting_session_rejects_non_owner_and_missing_session() {
        let storage = create_test_storage("host-delete-ownership");
        insert_users(&storage, 2);
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, game_id).unwrap());

        assert_eq!(storage.delete_game_session(2, 1, game_id).unwrap(), DeleteGameSessionOutcome::NotFound);
        assert_eq!(storage.delete_game_session(1, 1, game_id + 1).unwrap(), DeleteGameSessionOutcome::NotFound);
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2]);
    }

    #[test]
    fn registering_urls_replaces_stale_endpoints() {
        let storage = create_test_storage("replace-urls");
        insert_users(&storage, 1);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.5;port=13000;sid=14;type=2".to_string()]).unwrap();
        storage.register_urls(1, vec!["prudp:/address=10.66.0.6;port=13000;sid=14;type=2".to_string()]).unwrap();
        let urls = run(storage.list_urls(1)).unwrap().unwrap();
        assert_eq!(urls, vec!["prudp:/address=10.66.0.6;port=13000;sid=14;type=2"]);
    }

    #[test]
    fn private_participant_add_persists_private_membership() {
        let storage = create_test_storage("private-participant-membership");
        insert_users(&storage, 3);
        let session_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();

        storage.add_participants(1, 1, session_id, vec![2], Vec::new()).unwrap();
        let (_, _, participant_ids) = storage.game_session_diagnostics(1, session_id).unwrap().unwrap();
        assert_eq!(participant_ids, vec![1, 2]);

        storage.add_participants(1, 1, session_id, Vec::new(), vec![3]).unwrap();
        let (_, _, participant_ids) = storage.game_session_diagnostics(1, session_id).unwrap().unwrap();
        assert_eq!(participant_ids, vec![1, 2, 3]);
    }

    #[test]
    fn private_room_invite_survives_delivery_acceptance_and_repeated_search_until_join() {
        let storage = create_test_storage("private-room-invite");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let private_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        let host_sessions = run(storage.find_invitable_sessions_async(1)).unwrap().unwrap();
        assert_eq!(host_sessions.iter().map(|session| session.session_id).collect::<Vec<_>>(), vec![private_id, lobby_id]);

        let invite_id = run(storage.add_invite_async(1, 2, 1, 1, private_id)).unwrap().unwrap().unwrap();
        let delivered = run(storage.take_invite_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(delivered.id, invite_id);
        assert_eq!(delivered.kind, 1);
        assert_eq!(delivered.delivery_count, 1);
        assert!(run(storage.find_accepted_invited_session_async(2, 1)).unwrap().unwrap().is_none());
        let unrelated_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();

        let accepted = run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().unwrap();
        assert_eq!(accepted.receiver, 2);
        let (_, _, authorized_participants) = storage.game_session_diagnostics(1, private_id).unwrap().unwrap();
        assert_eq!(authorized_participants, vec![1]);
        let first = run(storage.find_accepted_invited_session_async(2, 1)).unwrap().unwrap().unwrap();
        let second = run(storage.find_accepted_invited_session_async(2, 1)).unwrap().unwrap().unwrap();
        assert_eq!(first.invite_id, invite_id);
        assert_eq!(first.kind, 1);
        assert_eq!(first.session.session_id, private_id);
        assert_eq!(first.session.participants.iter().map(|participant| participant.user_id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(second.session.session_id, private_id);
        assert_eq!(session_attribute_value(&first.session.attributes, 113), Some(1));
        assert_eq!(session_attribute_value(&second.session.attributes, 113), Some(1));
        assert_eq!(session_attribute_value(&first.session.attributes, 3), Some(0));
        assert_eq!(session_attribute_value(&second.session.attributes, 3), Some(0));
        assert_eq!(session_attribute_value(&first.session.attributes, 4), Some(8));
        assert_eq!(session_attribute_value(&second.session.attributes, 4), Some(8));

        let unrelated_consumed: (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE id = ? AND consumed_at IS NOT NULL")
            .bind(unrelated_invite)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(unrelated_consumed.0, 1);
        let recovery_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        storage.add_participants(1, 1, private_id, vec![2], Vec::new()).unwrap();
        let (_, _, deferred_participants) = storage.game_session_diagnostics(1, private_id).unwrap().unwrap();
        assert_eq!(deferred_participants, vec![1, 2]);
        assert!(run(storage.find_accepted_invited_session_async(2, 1)).unwrap().unwrap().is_none());
        let remaining: (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE receiver = 2 AND consumed_at IS NULL").fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        assert_eq!(remaining.0, 0, "successful admission must clear every pending or accepted invite");
        let recovery_consumed: (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE id = ? AND consumed_at IS NOT NULL")
            .bind(recovery_invite)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(recovery_consumed.0, 1);

        let (_, stored_attributes, stored_participants) = storage.game_session_diagnostics(1, private_id).unwrap().unwrap();
        assert_eq!(session_attribute_value(&stored_attributes, 113), Some(0));
        assert_eq!(session_attribute_value(&stored_attributes, 3), Some(0));
        assert_eq!(session_attribute_value(&stored_attributes, 4), Some(8));
        assert_eq!(stored_participants, vec![1, 2]);
        assert!(run(storage.find_accepted_invited_session_async(3, 1)).unwrap().unwrap().is_none());

        assert!(storage.join_game_session_unbounded(2, 1, private_id).unwrap());
        let (_, _, participant_ids) = storage.game_session_diagnostics(1, private_id).unwrap().unwrap();
        assert_eq!(participant_ids, vec![1, 2]);
    }

    #[test]
    fn session_member_can_invite_a_friend_into_the_real_hosts_session() {
        let storage = create_test_storage("member-invites-friend");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let member_sessions = run(storage.find_invitable_sessions_async(2)).unwrap().unwrap();
        assert_eq!(member_sessions.iter().map(|session| session.session_id).collect::<Vec<_>>(), vec![lobby_id]);
        assert_eq!(member_sessions[0].creator_id, 1);
        assert!(run(storage.find_invitable_sessions_async(3)).unwrap().unwrap().is_empty());

        let invite_id = run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, lobby_id))
            .unwrap()
            .unwrap()
            .expect("a current session member may invite another friend");
        let delivered = run(storage.take_invite_async(3)).unwrap().unwrap().expect("member invitation should remain valid");
        assert_eq!(delivered.id, invite_id);
        assert_eq!(delivered.sender, 2);
        assert_eq!(delivered.session_id, lobby_id);
        assert!(run(storage.accept_invite_async(3, invite_id)).unwrap().unwrap().is_some());
        let target = run(storage.find_accepted_invited_session_async(3, 1)).unwrap().unwrap().unwrap();
        assert_eq!(target.session.session_id, lobby_id);
        assert_eq!(target.session.creator_id, 1);

        assert!(run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().is_none());
    }

    #[test]
    fn migrated_shared_lobby_outranks_a_newer_preserved_solo_fallback_for_invites() {
        let storage = create_test_storage("migrated-party-invite-versus-solo-fallback");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, shared_lobby).unwrap());

        // Retail keeps this newer personal lobby alive while the player is
        // crossing through a private room, so it remains a valid fallback.
        let preserved_solo = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let private_room = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, private_room).unwrap());
        storage.leave_game_session(2, 1, preserved_solo).unwrap();
        assert!(storage.game_session_is_active(1, preserved_solo).unwrap());

        storage.leave_game_session(2, 1, private_room).unwrap();
        assert_eq!(storage.delete_game_session(1, 1, private_room).unwrap(), DeleteGameSessionOutcome::Destroyed);

        let candidates = run(storage.find_invitable_sessions_async(2)).unwrap().unwrap();
        assert_eq!(candidates.iter().map(|session| session.session_id).collect::<Vec<_>>(), vec![shared_lobby, preserved_solo]);

        let invite_id = run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, candidates[0].session_id))
            .unwrap()
            .unwrap()
            .expect("the migrated shared lobby remains joinable");
        let delivered = run(storage.take_invite_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(delivered.id, invite_id);
        assert_eq!(delivered.session_id, shared_lobby);
    }

    #[test]
    fn owner_admits_non_host_invitee_and_hands_off_every_room_member() {
        let storage = create_test_storage("member-invite-owner-admission-handoff");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let member_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, member_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());
        let friend_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_some());
        assert!(run(storage.accept_invite_async(3, friend_invite)).unwrap().unwrap().is_some());

        // A non-owner may send the invitation but cannot self-authorize the
        // resulting membership record.
        storage.add_participants(2, 1, room_id, vec![3], Vec::new()).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1, 2]);

        // The real game sends AddParticipants from the room owner after P2P
        // succeeds; the invited friend does not necessarily send JoinSession.
        storage.add_participants(1, 1, room_id, vec![3], Vec::new()).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1, 2, 3]);
        let (party_leader, party_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(party_leader, 1);
        assert_eq!(party_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);

        // Model the private-room transition: followers are no longer members
        // of the old lobby. Owner departure must preserve their shared room
        // for the game's native migration instead of pulling them backwards.
        storage.leave_game_session(2, 1, lobby_id).unwrap();
        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(2, 1, room_id).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![2, 3]);
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());
    }

    #[test]
    fn party_member_can_invite_during_lobby_to_private_membership_lag() {
        let storage = create_test_storage("party-member-transition-invite");
        insert_users(&storage, 4);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, party_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        // The host has already published the private room, but the follower's
        // Quazal AddParticipants has not reached the server yet. This is the
        // exact transition window observed in the three-client diagnostics.
        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        let member_sessions = run(storage.find_invitable_sessions_async(2)).unwrap().unwrap();
        assert_eq!(member_sessions[0].session_id, room_id);
        assert_eq!(member_sessions[0].creator_id, 1);
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1]);
        assert!(run(storage.find_invitable_sessions_async(4)).unwrap().unwrap().is_empty());

        let invite_id = run(storage.add_invite_async(2, 3, INVITE_KIND_PRIVATE_ROOM, 1, room_id))
            .unwrap()
            .unwrap()
            .expect("a persistent party member may invite during participant-capture lag");
        let delivered = run(storage.take_invite_async(3)).unwrap().unwrap().expect("transition invite must survive cleanup");
        assert_eq!(delivered.id, invite_id);
        assert_eq!(delivered.sender, 2);
        assert_eq!(run(storage.find_active_session_creator_async(1, room_id)).unwrap().unwrap(), Some(1));
        assert!(run(storage.accept_invite_async(3, invite_id)).unwrap().unwrap().is_some());
        let target = run(storage.find_accepted_invited_session_async(3, 1)).unwrap().unwrap().unwrap();
        assert_eq!(target.session.session_id, room_id);
        assert_eq!(target.session.creator_id, 1);

        assert!(run(storage.add_invite_async(4, 3, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().is_none());
    }

    #[test]
    fn room_member_private_invites_collapse_to_one_target_generation() {
        let storage = create_test_storage("member-private-invite-race");
        insert_users(&storage, 4);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());
        assert!(storage.join_game_session_unbounded(3, 1, room_id).unwrap());

        let member_invite = run(storage.add_invite_async(2, 4, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        let repeated_member_invite = run(storage.add_invite_async(2, 4, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        assert_eq!(repeated_member_invite, member_invite);

        let second_member_invite = run(storage.add_invite_async(3, 4, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        assert_eq!(second_member_invite, member_invite);
        let pending: Vec<(i64, u32)> =
            run(sqlx::query_as("SELECT id, sender FROM invites WHERE receiver = 4 AND accepted_at IS NULL AND consumed_at IS NULL ORDER BY sender").fetch_all(&storage.pool))
                .unwrap()
                .unwrap();
        assert_eq!(pending, vec![(member_invite, 2)]);

        assert!(run(storage.accept_invite_async(4, member_invite)).unwrap().unwrap().is_some());
        let repeated_after_accept = run(storage.add_invite_async(3, 4, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        assert_eq!(
            repeated_after_accept, member_invite,
            "an accepted target must not create another native join event before admission"
        );
        let target = run(storage.find_accepted_invited_session_async(4, 1)).unwrap().unwrap().unwrap();
        assert_eq!(target.session.session_id, room_id);
        assert_eq!(target.session.creator_id, 1);
    }

    #[test]
    fn lobby_host_and_member_invites_collapse_to_one_target_generation() {
        let storage = create_test_storage("lobby-host-member-invite-race");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let host_invite = run(storage.add_invite_async(1, 3, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        let member_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert_eq!(member_invite, host_invite);
        let delivered = run(storage.take_invite_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(delivered.id, host_invite);
        assert_eq!(delivered.sender, 1);

        assert!(run(storage.accept_invite_async(3, host_invite)).unwrap().unwrap().is_some());
        let repeated_after_accept = run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert_eq!(repeated_after_accept, host_invite);
        let target = run(storage.find_accepted_invited_session_async(3, 1)).unwrap().unwrap().unwrap();
        assert_eq!(target.session.session_id, lobby_id);
        assert_eq!(target.session.creator_id, 1);
    }

    #[test]
    fn accepting_one_legacy_equivalent_invite_retires_its_sibling() {
        let storage = create_test_storage("legacy-equivalent-invite-race");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let host_invite = run(storage.add_invite_async(1, 3, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        let legacy_sibling = run(sqlx::query(
            "INSERT INTO invites (sender, receiver, kind, session_type, session_id, expires_at) VALUES (?, ?, ?, ?, ?, datetime('now', '+30 seconds'))",
        )
        .bind(2_u32)
        .bind(3_u32)
        .bind(INVITE_KIND_LOBBY_PARTY)
        .bind(1_u32)
        .bind(lobby_id)
        .execute(&storage.pool))
        .unwrap()
        .unwrap()
        .last_insert_rowid();

        assert!(run(storage.accept_invite_async(3, host_invite)).unwrap().unwrap().is_some());
        let (consumed_at,): (Option<String>,) = run(sqlx::query_as("SELECT consumed_at FROM invites WHERE id = ?").bind(legacy_sibling).fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        assert!(consumed_at.is_some());
        assert!(run(storage.accept_invite_async(3, legacy_sibling)).unwrap().unwrap().is_none());
    }

    #[test]
    fn accepting_one_party_retires_the_current_competing_invite_batch() {
        let storage = create_test_storage("competing-party-invite-race");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();
        let first_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let second_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let first_invite = run(storage.add_invite_async(1, 3, INVITE_KIND_LOBBY_PARTY, 1, first_lobby)).unwrap().unwrap().unwrap();
        let second_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, second_lobby)).unwrap().unwrap().unwrap();
        assert_ne!(first_invite, second_invite);

        assert!(run(storage.accept_invite_async(3, first_invite)).unwrap().unwrap().is_some());
        assert!(run(storage.accept_invite_async(3, second_invite)).unwrap().unwrap().is_none());
        let recovery_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, second_lobby)).unwrap().unwrap().unwrap();
        assert_ne!(recovery_invite, second_invite, "a fresh invitation after a failed join must be immediately available");

        assert!(storage.join_game_session_unbounded(3, 1, first_lobby).unwrap());
        assert!(run(storage.add_invite_async(2, 3, INVITE_KIND_LOBBY_PARTY, 1, second_lobby)).unwrap().unwrap().is_some());
    }

    #[test]
    fn repeated_invite_keeps_one_generation_and_the_original_thirty_second_expiry() {
        let storage = create_test_storage("invite-fixed-thirty-second-window");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();

        let invite_id = run(storage.add_invite_async(1, 2, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        let lifetime_seconds: (i64,) = run(sqlx::query_as("SELECT strftime('%s', expires_at) - strftime('%s', created) FROM invites WHERE id = ?")
            .bind(invite_id)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(lifetime_seconds.0, 30);

        run(sqlx::query("UPDATE invites SET expires_at = datetime('now', '+20 seconds') WHERE id = ?")
            .bind(invite_id)
            .execute(&storage.pool))
        .unwrap()
        .unwrap();
        let before: (String,) = run(sqlx::query_as("SELECT expires_at FROM invites WHERE id = ?").bind(invite_id).fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        let repeated = run(storage.add_invite_async(1, 2, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        let after: (String,) = run(sqlx::query_as("SELECT expires_at FROM invites WHERE id = ?").bind(invite_id).fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        assert_eq!(repeated, invite_id);
        assert_eq!(after, before, "a repeated click must not extend the original expiry");
    }

    #[test]
    fn lobby_invites_allow_many_receivers_to_join_the_same_party_session() {
        let storage = create_test_storage("lobby-party-invite");
        insert_users(&storage, 4);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        for receiver in 2..=4 {
            let invite_id = run(storage.add_invite_async(1, receiver, 2, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert_eq!(run(storage.take_invite_async(receiver)).unwrap().unwrap().unwrap().kind, 2);
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            let (_, _, participants_after_accept) = storage.game_session_diagnostics(1, lobby_id).unwrap().unwrap();
            assert!(!participants_after_accept.contains(&receiver));
            let target = run(storage.find_accepted_invited_session_async(receiver, 1)).unwrap().unwrap().unwrap();
            assert_eq!(target.kind, 2);
            assert_eq!(target.session.session_id, lobby_id);
            assert!(!target.session.participants.iter().any(|participant| participant.user_id == receiver));
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        let (_, _, participant_ids) = storage.game_session_diagnostics(1, lobby_id).unwrap().unwrap();
        assert_eq!(participant_ids, vec![1, 2, 3, 4]);

        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3, 4]);

        storage.leave_game_session(1, 1, lobby_id).unwrap();
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3, 4]);

        assert!(storage.migrate_game_session_host(3, 1, lobby_id).unwrap());
        let (leader_id, roster) = run(storage.find_party_roster_async(4)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3, 2, 4]);
    }

    #[test]
    fn lobby_host_migration_detaches_old_leader_even_while_private_room_is_active() {
        let storage = create_test_storage("party-migration-with-active-room");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());
        assert!(storage.join_game_session_unbounded(3, 1, room_id).unwrap());

        storage.leave_game_session(1, 1, lobby_id).unwrap();
        assert!(storage.migrate_game_session_host(2, 1, lobby_id).unwrap());

        assert!(run(storage.find_party_roster_async(1)).unwrap().unwrap().is_none());
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn direct_private_room_host_exit_regroups_exact_n_minus_one_cohort_in_successor_lobby() {
        let storage = create_test_storage("direct-private-room-party-handoff");
        insert_users(&storage, 4);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        storage.register_urls(3, vec!["prudp:/address=10.66.0.3;port=13000;sid=14;type=2".to_string()]).unwrap();

        // Start with the same split shape seen in the real trace: the room
        // host already has one Party member, while the late invitees are not
        // necessarily part of that Party yet.
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let lobby_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, lobby_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());
        let successor_lobby_id = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3, 4], Vec::new()).unwrap();
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3]);

        // A genuinely earlier voluntary departure is not part of the exact
        // N-1 cohort, even if that player remains in the social Party.
        storage.leave_game_session(2, 1, room_id).unwrap();
        storage
            .recent_game_room_departures
            .lock()
            .unwrap()
            .insert((1, room_id, 2), Instant::now() - GAME_ROOM_DEPARTURE_CORRELATION_TTL - Duration::from_millis(1));
        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );
        assert!(run(storage.find_party_roster_async(1)).unwrap().unwrap().is_none());
        let (departed_leader_id, departed_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader_id, 2);
        assert_eq!(departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        let (leader_id, roster) = run(storage.find_party_roster_async(4)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3, 4]);

        // Phase one is sent only to the successor. Followers cannot race the
        // old-room unwind or an unpublished successor lobby.
        assert!(run(storage.take_invite_async(4)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());
        storage.leave_game_session(4, 1, room_id).unwrap();
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());
        storage.leave_game_session(3, 1, room_id).unwrap();

        let leader_restore = run(storage.take_invite_async(3))
            .unwrap()
            .unwrap()
            .expect("the successor should restore its own lobby first");
        assert_eq!(leader_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(leader_restore.sender, 3);
        assert_eq!(leader_restore.receiver, 3);
        assert_eq!(leader_restore.session_id, successor_lobby_id);
        assert!(run(storage.accept_invite_async(3, leader_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, successor_lobby_id).unwrap());

        let follower_restore = run(storage.take_invite_async(4))
            .unwrap()
            .unwrap()
            .expect("the follower should be invited only after the successor lobby is restored");
        assert_eq!(follower_restore.sender, 3);
        assert_eq!(follower_restore.session_id, successor_lobby_id);
        assert!(run(storage.accept_invite_async(4, follower_restore.id)).unwrap().unwrap().is_some());
        let target = run(storage.find_accepted_invited_session_async(4, 1)).unwrap().unwrap().unwrap();
        assert_eq!(target.session.session_id, successor_lobby_id);
        assert!(storage.join_game_session_unbounded(4, 1, successor_lobby_id).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, successor_lobby_id).unwrap().unwrap().2, vec![3, 4]);
    }

    #[test]
    fn leave_with_party_restores_original_lobby_and_includes_late_room_guest() {
        let storage = create_test_storage("private-room-leave-with-original-party");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let original_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let original_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, original_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, original_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, original_lobby).unwrap());
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.record_published_game_session(1, u64::from(room_id));
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();

        let (late_leader, late_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(late_leader, 3);
        assert_eq!(late_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3]);

        storage.record_published_game_session(1, u64::from(original_lobby));
        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );

        let (original_leader, original_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(original_leader, 1);
        assert_eq!(original_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        let (late_leader, late_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(late_leader, 1);
        assert_eq!(late_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());

        storage.leave_game_session(3, 1, room_id).unwrap();
        storage.leave_game_session(2, 1, room_id).unwrap();
        let late_restore = run(storage.take_invite_async(3)).unwrap().unwrap().expect("late room member should return with the Party");
        assert_eq!(late_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(late_restore.sender, 1);
        assert_eq!(late_restore.session_id, original_lobby);
        assert!(run(storage.accept_invite_async(3, late_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, original_lobby).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, original_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        let (original_leader, original_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(original_leader, 1);
        assert_eq!(original_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn native_migration_preserves_late_private_room_guests_pending_lobby_restore() {
        let storage = create_test_storage("native-migration-preserves-late-guest-restore");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        // Users 1 and 2 own the published native Party lobby. User 3 later
        // joins only the private Game Session, matching the three-machine
        // reproduction where TEST2 had left the Party and was re-invited.
        let published_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, published_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, party_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, published_lobby).unwrap());
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.record_published_game_session(1, u64::from(room_id));
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();
        storage.record_published_game_session(1, u64::from(published_lobby));

        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );

        let (restore_id, restore_session_id): (i64, u32) = run(sqlx::query_as("SELECT id, session_id FROM invites WHERE receiver = 3 AND kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(restore_session_id, published_lobby);

        // User 2 is elected by retail SplitSession. Its successful migration
        // must not consume user 3's still-undelivered restore event.
        assert!(storage.migrate_game_session_host(2, 1, room_id).unwrap());
        let restore_still_pending: i64 = run(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM invites WHERE id = ? AND consumed_at IS NULL)")
            .bind(restore_id)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(restore_still_pending, 1);

        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn private_leave_with_party_cancels_member_restore_that_precedes_host_delete() {
        let storage = create_test_storage("private-room-member-leaves-before-host-party-return");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let published_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [2, 3] {
            let invite = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, published_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, published_lobby).unwrap());
        }

        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();
        storage.record_published_game_session(1, u64::from(published_lobby));

        // Native private-room unwind observed in the three-client trace:
        // one follower leaves first, then the host confirms a Party return.
        storage.leave_game_session(3, 1, room_id).unwrap();
        let pending_restore = run(sqlx::query_as::<_, (i64, u32)>(
            "SELECT id, session_id FROM invites WHERE receiver = 3 AND kind = ? AND consumed_at IS NULL ORDER BY id DESC LIMIT 1",
        )
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_ne!(pending_restore.1, published_lobby);
        assert!(storage.game_session_is_active(1, pending_restore.1).unwrap());

        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 1 }
        );

        let (pending_count,): (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE receiver = 3 AND kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_count, 0);
        assert!(!storage.game_session_is_active(1, pending_restore.1).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, published_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn late_private_room_guest_leaves_to_its_existing_personal_party() {
        let storage = create_test_storage("private-room-late-guest-personal-party");
        insert_users(&storage, 2);
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=2;type=2".to_string()]).unwrap();
        let host_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let guest_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2], Vec::new()).unwrap();

        storage.leave_game_session(2, 1, room_id).unwrap();
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(storage.game_session_diagnostics(1, guest_lobby).unwrap().unwrap().2, vec![2]);
        let restore = run(storage.take_invite_async(2))
            .unwrap()
            .unwrap()
            .expect("departed member should restore its personal lobby");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(restore.sender, 2);
        assert_eq!(restore.receiver, 2);
        assert_eq!(restore.session_id, guest_lobby);
        let (host_leader, host_roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(host_leader, 1);
        assert_eq!(host_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(storage.game_session_diagnostics(1, host_lobby).unwrap().unwrap().2, vec![1]);
    }

    #[test]
    fn departed_private_room_member_without_personal_lobby_gets_a_seeded_restore_target() {
        let storage = create_test_storage("private-room-member-seeded-lobby-restore");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=1;type=2".to_string()]).unwrap();
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=2;type=2".to_string()]).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        let invite_id = run(storage.add_invite_async(1, 2, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());

        storage.leave_game_session(2, 1, room_id).unwrap();
        let restore = run(storage.take_invite_async(2)).unwrap().unwrap().expect("a missing personal lobby should be seeded");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(restore.sender, 2);
        assert_eq!(restore.receiver, 2);
        assert_ne!(restore.session_id, room_id);
        let (creator_id, attributes, participants) = storage.game_session_diagnostics(1, restore.session_id).unwrap().unwrap();
        assert_eq!(creator_id, 2);
        assert_eq!(session_attribute_value(&attributes, 113), Some(1));
        assert_eq!(participants, vec![2]);
    }

    #[test]
    fn owner_remove_then_member_leave_queues_internal_personal_lobby_restore() {
        let storage = create_test_storage("private-room-owner-remove-before-member-leave");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=1;type=2".to_string()]).unwrap();
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=2;type=2".to_string()]).unwrap();
        storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let guest_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        let invite_id = run(storage.add_invite_async(1, 2, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());

        // This is the retail wire order: the owner removes the member first,
        // then the departing client sends its own LeaveSession milliseconds
        // later. RemoveParticipants alone must not emit a restore transition.
        assert!(storage.remove_participants(1, 1, room_id, vec![2]).unwrap());
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        storage.leave_game_session(2, 1, room_id).unwrap();

        // Party projection is already a singleton, but the retail UI still
        // needs the internal friend-search transition to create its native
        // personal-lobby Session.
        let restore = run(storage.take_invite_async(2))
            .unwrap()
            .unwrap()
            .expect("owner-remove ordering must still restore the native personal lobby");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(restore.sender, 2);
        assert_eq!(restore.receiver, 2);
        assert_eq!(restore.session_id, guest_lobby);
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        let fresh_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert_ne!(fresh_lobby, guest_lobby);
        assert_eq!(storage.game_session_diagnostics(1, fresh_lobby).unwrap().unwrap().2, vec![2]);
    }

    #[test]
    fn quick_match_owner_remove_then_member_leave_defers_then_restores_personal_lobby() {
        let storage = create_test_storage("quick-match-owner-remove-before-member-leave");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=1;type=2".to_string()]).unwrap();
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=2;type=2".to_string()]).unwrap();
        storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let guest_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265;112 => 6".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2], Vec::new()).unwrap();

        // Retail quick match uses the same owner-remove/member-leave ordering
        // as a private room. The correlation must therefore be based on the
        // game-session class, not on private-room matchmaking attributes.
        assert!(storage.remove_participants(1, 1, quick_match, vec![2]).unwrap());
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        storage.leave_game_session(2, 1, quick_match).unwrap();

        // Owner-remove is also the first half of "leave with Party". Keep the
        // social Party intact during the bounded correlation window; if no
        // leader transaction follows, the expiry path performs the original
        // one-player restore exactly once.
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        assert_eq!(storage.pending_quick_match_member_leaves(1, quick_match), vec![2]);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);

        let restore = run(storage.take_invite_async(2))
            .unwrap()
            .unwrap()
            .expect("quick-match owner-remove ordering must restore the native personal lobby");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(restore.sender, 2);
        assert_eq!(restore.receiver, 2);
        assert_eq!(restore.session_id, guest_lobby);
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn private_room_host_exit_correlates_follower_leave_immediately_before_delete() {
        let storage = create_test_storage("private-room-host-exit-order");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        // User 2 is numerically first but has no surviving lobby. Repeated
        // native migration left user 3 as the authoritative owner of the only
        // N-1 lobby, matching the production Session 1430/1434 ordering.
        let successor_lobby_id = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();

        // Retail unwinds one follower a few milliseconds before the owner's
        // DeleteSession. The transient LeaveSession must not shrink N-1.
        storage.leave_game_session(2, 1, room_id).unwrap();
        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 1 }
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3, 2]);

        storage.leave_game_session(3, 1, room_id).unwrap();
        let leader_restore = run(storage.take_invite_async(3)).unwrap().unwrap().expect("correlated successor restore");
        assert_eq!(leader_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(leader_restore.session_id, successor_lobby_id);
        assert!(run(storage.accept_invite_async(3, leader_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, successor_lobby_id).unwrap());
        let follower_restore = run(storage.take_invite_async(2)).unwrap().unwrap().expect("correlated follower restore");
        assert_eq!(follower_restore.session_id, successor_lobby_id);
    }

    #[test]
    fn repeated_private_room_host_exit_retires_old_generation_and_restores_immediately() {
        let storage = create_test_storage("repeated-private-room-party-handoff");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        let lobby_1 = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let lobby_2 = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let room_1 = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_1, vec![2, 3], Vec::new()).unwrap();
        assert_eq!(
            storage.delete_game_session(1, 1, room_1).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );
        storage.leave_game_session(2, 1, room_1).unwrap();
        storage.leave_game_session(3, 1, room_1).unwrap();
        assert!(!storage.game_session_is_active(1, room_1).unwrap());

        let first_restore = run(storage.take_invite_async(2))
            .unwrap()
            .unwrap()
            .expect("first successor restore must be immediately deliverable");
        assert_eq!(first_restore.session_id, lobby_2);
        assert!(run(storage.accept_invite_async(2, first_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_2).unwrap());
        let first_follower = run(storage.take_invite_async(3)).unwrap().unwrap().expect("first follower restore");
        assert!(run(storage.accept_invite_async(3, first_follower.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, lobby_2).unwrap());

        // A second cycle deliberately chooses the previous room owner as the
        // successor. Before pending-host retirement, room_1 remained active
        // with user 1 alone and blocked this restore indefinitely.
        let room_2 = storage.create_game_session(2, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(2, 1, room_2, vec![1, 3], Vec::new()).unwrap();
        assert_eq!(
            storage.delete_game_session(2, 1, room_2).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );
        storage.leave_game_session(1, 1, room_2).unwrap();
        storage.leave_game_session(3, 1, room_2).unwrap();
        assert!(!storage.game_session_is_active(1, room_2).unwrap());

        let second_restore = run(storage.take_invite_async(1))
            .unwrap()
            .unwrap()
            .expect("second successor restore must not be blocked by room_1");
        assert_eq!(second_restore.session_id, lobby_1);
        assert!(run(storage.accept_invite_async(1, second_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(1, 1, lobby_1).unwrap());
        let second_follower = run(storage.take_invite_async(3)).unwrap().unwrap().expect("second follower restore");
        assert!(run(storage.accept_invite_async(3, second_follower.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, lobby_1).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, lobby_1).unwrap().unwrap().2, vec![1, 3]);
    }

    #[test]
    fn unmarked_solo_private_room_is_not_retired_as_stale_host_state() {
        let storage = create_test_storage("live-solo-private-room");
        insert_users(&storage, 1);
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();

        assert!(storage.remove_participants(1, 1, room_id, Vec::new()).unwrap());
        assert!(storage.game_session_is_active(1, room_id).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1]);
    }

    #[test]
    fn four_player_private_room_host_exit_preserves_three_player_party_and_two_restore_targets() {
        let storage = create_test_storage("four-player-private-room-party-handoff");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let superseded_successor_lobby_id = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3, 4], Vec::new()).unwrap();

        assert_eq!(
            storage.delete_game_session(1, 1, room_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 3 }
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(4)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3, 4]);
        let (restore_count,): (i64,) = run(
            sqlx::query_as("SELECT COUNT(*) FROM invites WHERE kind = ? AND session_id = ? AND sender = receiver AND consumed_at IS NULL")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .bind(superseded_successor_lobby_id)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_eq!(restore_count, 1);

        for user_id in 2..=4 {
            storage.leave_game_session(user_id, 1, room_id).unwrap();
        }
        run(sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE receiver = 2 AND kind = ?")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .execute(&storage.pool))
        .unwrap()
        .unwrap();
        let leader_restore = run(storage.take_invite_async(2)).unwrap().unwrap().expect("successor restore event");
        assert_eq!(leader_restore.sender, 2);
        assert_eq!(leader_restore.receiver, 2);
        assert!(run(storage.accept_invite_async(2, leader_restore.id)).unwrap().unwrap().is_some());

        // Retail may host a fresh lobby instead of rejoining the preserved
        // one. Retiring the old lobby must not erase the in-flight Party.
        let successor_lobby_id = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(!storage.game_session_is_active(1, superseded_successor_lobby_id).unwrap());
        let (leader_id, roster) = run(storage.find_party_roster_async(4)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3, 4]);

        run(
            sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE receiver IN (3, 4) AND kind = ?")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .execute(&storage.pool),
        )
        .unwrap()
        .unwrap();
        for receiver in [3, 4] {
            let restore = run(storage.take_invite_async(receiver))
                .unwrap()
                .unwrap()
                .expect("each N-1 follower should receive one restore target");
            assert_eq!(restore.sender, 2);
            assert_eq!(restore.session_id, successor_lobby_id);
            assert!(run(storage.accept_invite_async(receiver, restore.id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, successor_lobby_id).unwrap());
        }
        assert_eq!(storage.game_session_diagnostics(1, successor_lobby_id).unwrap().unwrap().2, vec![2, 3, 4]);
    }

    #[test]
    fn successful_native_private_match_migration_cancels_lobby_restore_lane() {
        let storage = create_test_storage("native-migration-cancels-lobby-restore");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let match_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, match_id, vec![2, 3], Vec::new()).unwrap();

        assert_eq!(
            storage.delete_game_session(1, 1, match_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 2 }
        );
        let pending_before: i64 = run(sqlx::query_scalar("SELECT host_leave_pending FROM game_sessions WHERE id = ?")
            .bind(match_id)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_before, 1);
        assert!(storage.migrate_game_session_host(2, 1, match_id).unwrap());
        let pending_after: i64 = run(sqlx::query_scalar("SELECT host_leave_pending FROM game_sessions WHERE id = ?")
            .bind(match_id)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_after, 0);
        let (pending_restore_count,): (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_restore_count, 0);
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn native_migration_preserves_survivor_cleanup_leaves_until_split_session() {
        let storage = create_test_storage("native-migration-survivor-cleanup");
        insert_users(&storage, 3);
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();

        // The owner begins native migration via AbandonSession/LeaveSession.
        storage.abandon_game_session(1, 1, room_id).unwrap();
        let pending: i64 = run(sqlx::query_scalar("SELECT host_leave_pending FROM game_sessions WHERE id = ?")
            .bind(room_id)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending, 1);

        // Retail then sends cleanup LeaveSession calls from the surviving
        // clients before the elected host submits SplitSession. Membership
        // must remain intact so either survivor can be elected.
        storage.leave_game_session(2, 1, room_id).unwrap();
        storage.leave_game_session(3, 1, room_id).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1, 2, 3]);

        assert!(storage.migrate_game_session_host(2, 1, room_id).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, room_id).unwrap().unwrap();
        assert_eq!(creator_id, 2);
        assert_eq!(participant_ids, vec![2, 3]);
    }

    #[test]
    fn private_native_migration_detaches_old_party_leader_before_split_session() {
        let storage = create_test_storage("native-migration-party-leader-preparation");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=4 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        for receiver in 2..=4 {
            assert!(storage.join_game_session_unbounded(receiver, 1, room_id).unwrap());
        }

        storage.abandon_game_session(1, 1, room_id).unwrap();

        // The social Party must stop following the departing host immediately,
        // while the private game Session remains untouched for native election.
        // The old host gets an independent single-member Party and one silent
        // personal-lobby restore instead of an impossible zero-member UI.
        let (departed_leader_id, departed_roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader_id, 1);
        assert_eq!(departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1]);
        let (provisional_leader_id, provisional_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(provisional_leader_id, 2);
        assert_eq!(provisional_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3, 4]);
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, room_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3, 4]);
        let (pending_restore_count,): (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_restore_count, 1);
        assert!(run(storage.take_invite_async(1)).unwrap().unwrap().is_none());

        // Cleanup Leaves are transport unwind and must not remove survivors.
        for survivor_id in 2..=4 {
            storage.leave_game_session(survivor_id, 1, room_id).unwrap();
        }
        assert_eq!(storage.game_session_diagnostics(1, room_id).unwrap().unwrap().2, vec![1, 2, 3, 4]);

        // The game-selected SplitSession remains authoritative and may correct
        // the provisional Party successor without creating a second pathway.
        assert!(storage.migrate_game_session_host(4, 1, room_id).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, room_id).unwrap().unwrap();
        assert_eq!(creator_id, 4);
        assert_eq!(participant_ids, vec![2, 3, 4]);
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 4);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![4, 2, 3]);
        let departed_restore = run(storage.take_invite_async(1))
            .unwrap()
            .unwrap()
            .expect("departing host should restore a personal lobby after native migration completes");
        assert_eq!(departed_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(departed_restore.sender, 1);
        assert_eq!(departed_restore.receiver, 1);
        assert_ne!(departed_restore.session_id, lobby_id);
        assert_eq!(storage.game_session_diagnostics(1, departed_restore.session_id).unwrap().unwrap().2, vec![1]);
    }

    #[test]
    fn quick_match_host_abandon_restores_only_the_departing_host_lobby() {
        let storage = create_test_storage("quick-host-personal-lobby-restore");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, shared_lobby).unwrap());
        }
        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2, 3], Vec::new()).unwrap();

        storage.abandon_game_session(1, 1, quick_match).unwrap();

        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, quick_match).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3]);
        let (departed_leader, departed_roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader, 1);
        assert_eq!(departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1]);
        let (survivor_leader, survivor_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(survivor_leader, 2);
        assert_eq!(survivor_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);

        assert!(run(storage.take_invite_async(1)).unwrap().unwrap().is_none());

        // Retail follows AbandonSession with LeaveSession. That cleanup must
        // not queue a second restore or alter the still-migrating roster.
        storage.leave_game_session(1, 1, quick_match).unwrap();
        let pending_restores: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver = 1 AND kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_restores, 1);
        assert_eq!(storage.game_session_diagnostics(1, quick_match).unwrap().unwrap().2, vec![1, 2, 3]);

        assert!(storage.migrate_game_session_host(3, 1, quick_match).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, quick_match).unwrap().unwrap();
        assert_eq!(creator_id, 3);
        assert_eq!(participant_ids, vec![2, 3]);
        for survivor in [2, 3] {
            let candidates = run(storage.find_invitable_session_candidates_async(survivor)).unwrap().unwrap();
            assert!(candidates
                .iter()
                .any(|candidate| candidate.session.session_id == quick_match && candidate.session.creator_id == 3 && candidate.direct_member));
        }
        let restore = run(storage.take_invite_async(1))
            .unwrap()
            .unwrap()
            .expect("departing quick-match host should restore a personal lobby after native migration completes");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(restore.sender, 1);
        assert_eq!(restore.receiver, 1);
        assert_ne!(restore.session_id, shared_lobby);
        assert_ne!(restore.session_id, quick_match);
        assert_eq!(storage.game_session_diagnostics(1, restore.session_id).unwrap().unwrap().2, vec![1]);
    }

    #[test]
    fn expired_native_migration_uses_existing_n_minus_one_lobby_fallback() {
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

    #[test]
    fn expired_private_migration_keeps_an_already_complete_native_party_lobby() {
        let storage = create_test_storage("native-migration-complete-party-lobby");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let survivor_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(3, 1, survivor_lobby).unwrap());
        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();
        storage.abandon_game_session(1, 1, room_id).unwrap();

        storage
            .native_host_migrations
            .lock()
            .unwrap()
            .get_mut(&(1, room_id)).unwrap().started_at = Instant::now() - NATIVE_HOST_MIGRATION_TTL - Duration::from_secs(1);
        assert_eq!(storage.expire_native_host_migrations().unwrap(), 1);

        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(storage.game_session_diagnostics(1, survivor_lobby).unwrap().unwrap().2, vec![2, 3]);
        let (restore_count,): (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(restore_count, 0);
    }

    #[test]
    fn removing_party_member_while_private_room_is_active_detaches_membership_and_allows_reinvite() {
        let storage = create_test_storage("party-remove-with-active-room");
        insert_users(&storage, 4);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=4 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());
        assert!(storage.join_game_session_unbounded(3, 1, room_id).unwrap());
        assert!(storage.join_game_session_unbounded(4, 1, room_id).unwrap());

        assert!(storage.remove_participants(1, 1, lobby_id, vec![2]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, lobby_id).unwrap().unwrap().2, vec![1, 3, 4]);
        let (departed_leader_id, departed_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader_id, 2);
        assert_eq!(departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 3, 4]);

        let reinvite_id = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id))
            .unwrap()
            .unwrap()
            .expect("a remaining Party member must be able to reinvite the departed player");
        let delivered = run(storage.take_invite_async(2))
            .unwrap()
            .unwrap()
            .expect("reinvite must be delivered instead of AlreadyExists");
        assert_eq!(delivered.id, reinvite_id);
        assert_eq!(delivered.sender, 3);
    }

    #[test]
    fn invite_generations_are_monotonic_and_stale_accept_cannot_hit_a_new_lifecycle() {
        const LEGACY_GENERATION_HIGH_WATER: i64 = 1_000_000_000;

        let storage = create_test_storage("invite-generation-monotonicity");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();

        let first_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let old_generation = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, first_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, old_generation)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, first_lobby).unwrap());

        let second_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let new_generation = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, second_lobby)).unwrap().unwrap().unwrap();

        assert!(old_generation > LEGACY_GENERATION_HIGH_WATER);
        assert!(new_generation > old_generation);
        assert!(u32::try_from(new_generation).is_ok());
        assert!(run(storage.accept_invite_async(2, old_generation)).unwrap().unwrap().is_none());

        let (new_accepted_at,): (Option<String>,) = run(sqlx::query_as("SELECT accepted_at FROM invites WHERE id = ?").bind(new_generation).fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        assert!(new_accepted_at.is_none());
        let delivered = run(storage.take_invite_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(delivered.id, new_generation);
        assert_eq!(delivered.session_id, second_lobby);
    }

    #[test]
    fn invite_generation_migration_preserves_existing_rows_and_advances_sequence() {
        run(async {
            let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await?;
            sqlx::raw_sql(
                r"
                PRAGMA foreign_keys = ON;
                CREATE TABLE users (id INTEGER PRIMARY KEY);
                INSERT INTO users(id) VALUES (1), (2);
                CREATE TABLE invites (
                    sender INTEGER NOT NULL REFERENCES users(id),
                    receiver INTEGER NOT NULL REFERENCES users(id),
                    created DATETIME DEFAULT CURRENT_TIMESTAMP,
                    session_type INTEGER,
                    session_id INTEGER,
                    delivered_at DATETIME,
                    consumed_at DATETIME,
                    expires_at DATETIME,
                    kind INTEGER NOT NULL DEFAULT 1,
                    accepted_at DATETIME,
                    delivery_count INTEGER NOT NULL DEFAULT 0,
                    party_id INTEGER
                );
                INSERT INTO invites (
                    rowid, sender, receiver, created, session_type, session_id,
                    delivered_at, expires_at, kind, delivery_count, party_id
                ) VALUES (
                    42, 1, 2, '2026-08-09 00:00:00', 1, 501,
                    '2026-08-09 00:00:03', '2099-01-01 00:00:00', 2, 7, 9
                );
                ",
            )
            .execute(&pool)
            .await?;

            sqlx::raw_sql(include_str!("migrations/20260809180000_invite_generation_ids.fixture")).execute(&pool).await?;

            let preserved: (i64, u32, u32, i32, u32, i64, Option<i64>) = sqlx::query_as("SELECT id, sender, receiver, kind, session_id, delivery_count, party_id FROM invites")
                .fetch_one(&pool)
                .await?;
            assert_eq!(preserved, (42, 1, 2, INVITE_KIND_LOBBY_PARTY, 501, 7, Some(9)));

            let sequence: i64 = sqlx::query_scalar("SELECT seq FROM sqlite_sequence WHERE name = 'invites'").fetch_one(&pool).await?;
            assert_eq!(sequence, 1_000_000_000);
            let next = sqlx::query("INSERT INTO invites (sender, receiver) VALUES (1, 2)")
                .execute(&pool)
                .await?
                .last_insert_rowid();
            assert_eq!(next, 1_000_000_001);
            assert!(u32::try_from(next).is_ok());

            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();
    }

    #[test]
    fn invite_rejects_wrong_host_and_expires_safely() {
        let storage = create_test_storage("invite-validation");
        insert_users(&storage, 3);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();
        storage.register_urls(3, vec!["prudp:/address=10.66.0.4;port=13000;sid=14;type=2".to_string()]).unwrap();
        let other_host_session = storage.create_game_session(3, 1, "113 => 0".to_string()).unwrap();
        assert!(run(storage.add_invite_async(1, 2, 1, 1, other_host_session)).unwrap().unwrap().is_none());

        let own_session = storage.create_game_session(1, 1, "113 => 0".to_string()).unwrap();
        let invite_id = run(storage.add_invite_async(1, 2, 1, 1, own_session)).unwrap().unwrap().unwrap();
        run(async {
            sqlx::query("UPDATE invites SET expires_at = datetime('now', '-1 second') WHERE receiver = 2")
                .execute(&storage.pool)
                .await
        })
        .unwrap()
        .unwrap();
        assert!(run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().is_none());
        assert!(run(storage.find_accepted_invited_session_async(2, 1)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
    }

    #[test]
    fn api_heartbeat_preserves_idle_secure_session_until_heartbeat_stops() {
        let storage = create_test_storage("api-heartbeat");
        insert_users(&storage, 1);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();
        let lobby_id = storage.create_game_session(1, 1, "113 => 1".to_string()).unwrap();
        run(storage.touch_api_presence_async(1)).unwrap().unwrap();

        assert!(storage.close_secure_session(1).unwrap());
        assert!(storage.game_session_is_active(1, lobby_id).unwrap());
        assert!(!run(storage.list_urls(1)).unwrap().unwrap().is_empty());
        let users = run(storage.list_users_async()).unwrap().unwrap();
        assert!(users.iter().any(|user| user.id == 1 && user.is_online));

        run(async {
            sqlx::query("UPDATE users SET last_api_seen_at = datetime('now', '-30 seconds') WHERE id = 1")
                .execute(&storage.pool)
                .await?;
            storage.cleanup_stale_api_sessions_async().await?;
            Ok::<(), eyre::Error>(())
        })
        .unwrap()
        .unwrap();
        assert!(!storage.game_session_is_active(1, lobby_id).unwrap());
        assert!(run(storage.list_urls(1)).unwrap().unwrap().is_empty());
    }

    #[test]
    fn forced_host_disconnect_waits_for_game_selected_successor() {
        let storage = create_test_storage("forced-host-disconnect-native-migration");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage.create_user_session(user_id, format!("forced-host-{user_id}").as_bytes()).unwrap();
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        assert!(!storage.close_secure_session(1).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, lobby_id).unwrap().unwrap();
        assert_eq!(creator_id, 1);
        assert_eq!(participant_ids, vec![1, 2, 3]);
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);

        assert!(storage.migrate_game_session_host(3, 1, lobby_id).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, lobby_id).unwrap().unwrap();
        assert_eq!(creator_id, 3);
        assert_eq!(participant_ids, vec![2, 3]);
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3, 2]);
    }

    #[test]
    fn friend_presence_ignores_login_only_state_until_game_session_is_ready() {
        let storage = create_test_storage("friend-presence-login-only");
        insert_users(&storage, 1);

        run(async {
            // Model the old Launcher-login side effect and an API heartbeat.
            // Neither is sufficient without a secure session or station URL.
            sqlx::query("UPDATE users SET is_online = 1, last_login = CURRENT_TIMESTAMP, last_api_seen_at = CURRENT_TIMESTAMP WHERE id = 1")
                .execute(&storage.pool)
                .await?;
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();

        let users = run(storage.list_game_ready_users_async()).unwrap().unwrap();
        assert!(users.iter().any(|user| user.id == 1 && !user.is_online));

        storage.create_user_session(1, b"friend-presence-secure").unwrap();
        let users = run(storage.list_game_ready_users_async()).unwrap().unwrap();
        assert!(users.iter().any(|user| user.id == 1 && user.is_online));
    }

    #[test]
    fn successful_authentication_does_not_publish_gameplay_presence() {
        let storage = create_test_storage("authentication-is-not-presence");
        run(async {
            sqlx::query("INSERT INTO users (id, username, password, ubi_id) VALUES (2001, 'launcher-user', 'password', 'ubi-launcher')")
                .execute(&storage.pool)
                .await?;
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();

        let authenticated = run(storage.login_user_async("launcher-user", "password")).unwrap().unwrap();
        assert!(matches!(authenticated, Ok(2001)));
        let (has_last_login, is_online): (i64, bool) = run(async {
            Ok::<_, sqlx::Error>(
                sqlx::query_as("SELECT last_login IS NOT NULL, is_online FROM users WHERE id = 2001")
                    .fetch_one(&storage.pool)
                    .await?,
            )
        })
        .unwrap()
        .unwrap();
        assert_eq!((has_last_login, is_online), (1, false));
    }

    #[test]
    fn friend_presence_survives_transient_secure_disconnect_with_game_endpoint() {
        let storage = create_test_storage("friend-presence-transient-disconnect");
        insert_users(&storage, 1);
        storage.create_user_session(1, b"friend-presence-transient").unwrap();
        storage.register_urls(1, vec!["prudp:/address=10.66.0.2;port=13000;sid=14;type=2".to_string()]).unwrap();
        run(storage.touch_api_presence_async(1)).unwrap().unwrap();

        assert!(storage.close_secure_session(1).unwrap());
        let users = run(storage.list_game_ready_users_async()).unwrap().unwrap();
        assert!(users.iter().any(|user| user.id == 1 && user.is_online));
    }

    #[test]
    fn lobby_party_follows_leader_into_created_private_room() {
        let storage = create_test_storage("party-follow-private");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=14;type=2")])
                .unwrap();
        }
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert!(run(storage.take_invite_async(receiver)).unwrap().unwrap().is_some());
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        let private_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        run(async {
            sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE kind = ?")
                .bind(INVITE_KIND_PARTY_FOLLOW)
                .execute(&storage.pool)
                .await
        })
        .unwrap()
        .unwrap();
        for receiver in 2..=3 {
            let follow = run(storage.take_invite_async(receiver)).unwrap().unwrap().unwrap();
            assert_eq!(follow.kind, INVITE_KIND_PARTY_FOLLOW);
            assert!(run(storage.accept_invite_async(receiver, follow.id)).unwrap().unwrap().is_some());
            let target = run(storage.find_accepted_invited_session_async(receiver, 1)).unwrap().unwrap().unwrap();
            assert_eq!(target.kind, INVITE_KIND_PARTY_FOLLOW);
            assert_eq!(target.session.session_id, private_id);
            storage.add_participants(1, 1, private_id, vec![receiver], Vec::new()).unwrap();
            assert!(run(storage.find_accepted_invited_session_async(receiver, 1)).unwrap().unwrap().is_none());
            assert!(storage.join_game_session_unbounded(receiver, 1, private_id).unwrap());
        }
        let (_, _, participant_ids) = storage.game_session_diagnostics(1, private_id).unwrap().unwrap();
        assert_eq!(participant_ids, vec![1, 2, 3]);
    }

    #[test]
    fn private_match_member_leave_detaches_stale_party_but_keeps_remaining_member_invitable() {
        let storage = create_test_storage("private-member-reinvite");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=21;type=2")])
                .unwrap();
        }

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
            assert!(run(storage.take_invite_async(receiver)).unwrap().unwrap().is_some());
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, lobby_id).unwrap());
        }

        let room_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".to_string()).unwrap();
        storage.add_participants(1, 1, room_id, vec![2, 3], Vec::new()).unwrap();

        storage.leave_game_session(2, 1, room_id).unwrap();

        let (departed_leader_id, departed_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader_id, 2);
        assert_eq!(departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 3]);

        let remaining_candidates = run(storage.find_invitable_session_candidates_async(3)).unwrap().unwrap();
        assert!(remaining_candidates
            .iter()
            .any(|candidate| candidate.session.session_id == room_id && candidate.direct_member));
        let departed_candidates = run(storage.find_invitable_session_candidates_async(2)).unwrap().unwrap();
        assert!(departed_candidates.iter().all(|candidate| candidate.session.session_id != room_id));

        assert!(run(storage.add_invite_async(3, 2, INVITE_KIND_PRIVATE_ROOM, 1, room_id)).unwrap().unwrap().is_some());
    }

    #[test]
    fn public_match_member_leave_projects_one_player_lobby_and_preserves_remaining_party() {
        let storage = create_test_storage("public-member-leave-solo");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=22;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, shared_lobby).unwrap());
        }

        let public_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, public_match, vec![2, 3], Vec::new()).unwrap();
        storage.leave_game_session(2, 1, public_match).unwrap();
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);

        let (solo_leader, solo_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(solo_leader, 2);
        assert_eq!(solo_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        let (remaining_leader, remaining_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(remaining_leader, 1);
        assert_eq!(remaining_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 3]);

        storage.leave_game_session(2, 1, shared_lobby).unwrap();
        let (restored_leader, restored_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(restored_leader, 2);
        assert_eq!(restored_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);

        let personal_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(storage.game_session_is_active(1, personal_lobby).unwrap());
        let (personal_leader, personal_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(personal_leader, 2);
        assert_eq!(personal_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn social_party_leader_leaving_another_players_match_hands_remaining_party_to_a_survivor() {
        let storage = create_test_storage("party-leader-leaves-foreign-match");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=23;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, shared_lobby).unwrap());
        }

        let public_match = storage.create_game_session(4, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(4, 1, public_match, vec![1, 2, 3], Vec::new()).unwrap();
        storage.leave_game_session(1, 1, public_match).unwrap();
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);

        let (departed_leader, departed_roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader, 1);
        assert_eq!(departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1]);
        let (remaining_leader, remaining_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(remaining_leader, 2);
        assert_eq!(remaining_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn direct_quick_match_invitee_joins_the_current_party_after_admission() {
        let storage = create_test_storage("quick-match-late-invite-party");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=24;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let member_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, member_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, shared_lobby).unwrap());

        let quick_match = storage.create_game_session(4, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(1, 1, quick_match).unwrap());
        assert!(storage.join_game_session_unbounded(2, 1, quick_match).unwrap());
        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let late_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_PRIVATE_ROOM, 1, quick_match)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(3, late_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, quick_match).unwrap());

        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        let (original_leader_id, original_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(original_leader_id, 1);
        assert_eq!(original_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());
    }

    #[test]
    fn direct_quick_match_invite_reconciles_the_native_published_party() {
        let storage = create_test_storage("quick-match-native-party-reconcile");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=241;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let member_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, member_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, shared_lobby).unwrap());

        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, quick_match).unwrap());

        // Model the observed retail ordering: the native lobby still contains
        // users 1 and 2, while an earlier leave/split path has temporarily
        // detached user 2 into a one-person persistent Party.
        run(async {
            let mut transaction = storage.begin_write().await?;
            Storage::detach_party_member_to_solo_async(&mut transaction, 2, quick_match).await?;
            transaction.commit().await
        })
        .unwrap()
        .unwrap();
        let (stale_leader_id, stale_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(stale_leader_id, 2);
        assert_eq!(stale_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
        storage.record_published_game_session(2, u64::from(shared_lobby));

        storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let late_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_PRIVATE_ROOM, 1, quick_match)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(3, late_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, quick_match).unwrap());

        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn leave_with_party_during_late_quick_match_join_restores_the_complete_party() {
        let storage = create_test_storage("quick-match-leave-with-late-invite");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=25;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let member_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, member_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, shared_lobby).unwrap());

        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, quick_match).unwrap());
        let late_invite = run(storage.add_invite_async(2, 3, INVITE_KIND_PRIVATE_ROOM, 1, quick_match)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(3, late_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, quick_match).unwrap());

        storage.record_published_game_session(1, u64::from(shared_lobby));
        assert!(storage.remove_participants(1, 1, quick_match, vec![2]).unwrap());
        storage.abandon_game_session(1, 1, quick_match).unwrap();

        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 1 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_ne!(restored_lobby, shared_lobby);
        assert!(!storage.game_session_is_active(1, shared_lobby).unwrap());
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (1, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3])
        );
        assert!(run(storage.take_invite_async(1)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());

        run(
            sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds'), delivered_at = datetime('now', '-4 seconds') WHERE receiver = 1 AND kind = ?")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .execute(&storage.pool),
        )
        .unwrap()
        .unwrap();
        let leader_restore = run(storage.take_invite_async(1)).unwrap().unwrap().expect("Party leader must restore first");
        assert_eq!(leader_restore.session_id, restored_lobby);
        assert!(run(storage.accept_invite_async(1, leader_restore.id)).unwrap().unwrap().is_some());
        let active_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        let restore = run(storage.take_invite_async(3))
            .unwrap()
            .unwrap()
            .expect("late quick-match invitee must follow the restored Party leader");
        assert_eq!(restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(restore.session_id, active_lobby);
    }

    #[test]
    fn quick_match_restore_waits_for_leader_before_delivering_followers() {
        let storage = create_test_storage("quick-match-two-phase-party-restore");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=43;type=2")])
                .unwrap();
        }
        let published_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [2, 3] {
            let invite = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, published_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, published_lobby).unwrap());
        }

        let party_id = run(sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = 1").fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        let match_session = storage.create_game_session(3, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(1, 1, match_session).unwrap());
        assert!(storage.join_game_session_unbounded(2, 1, match_session).unwrap());
        let snapshot = QuickMatchPartyDeparture {
            captured_at: Instant::now(),
            party_id,
            leader_id: 1,
            room_host_id: 3,
            lobby_session_id: published_lobby,
            members: vec![1, 2, 3],
            departed_members: [1, 2, 3].into_iter().collect(),
        };

        let (seed_lobby, member_count, initial_restore_count) = run(async {
            let mut transaction = storage.begin_write().await?;
            let restored = storage
                .restore_quick_match_party_snapshot_async(&mut transaction, &snapshot, 1, match_session)
                .await?
                .expect("quick-match Party should enter the restore lane");
            transaction.commit().await?;
            Ok::<_, sqlx::Error>(restored)
        })
        .unwrap()
        .unwrap();
        assert_eq!(member_count, 3);
        assert_eq!(initial_restore_count, 1);
        assert_eq!(storage.game_session_diagnostics(1, seed_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());

        run(sqlx::query(
            "UPDATE invites SET created = datetime('now', '-3 seconds'), delivered_at = datetime('now', '-4 seconds') WHERE receiver = 1 AND kind = ? AND session_id = ?",
        )
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .bind(seed_lobby)
        .execute(&storage.pool))
        .unwrap()
        .unwrap();
        let leader_restore = run(storage.take_invite_async(1)).unwrap().unwrap().expect("leader restore must be delivered first");
        assert_eq!(leader_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(leader_restore.session_id, seed_lobby);
        assert!(run(storage.accept_invite_async(1, leader_restore.id)).unwrap().unwrap().is_some());

        let active_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert_ne!(active_lobby, seed_lobby);
        assert!(!storage.game_session_is_active(1, seed_lobby).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, active_lobby).unwrap().unwrap().2, vec![1]);
        run(sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE kind = ? AND session_id = ?")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(active_lobby)
            .execute(&storage.pool))
        .unwrap()
        .unwrap();

        let ordinary_follower_restore = run(storage.take_invite_async(2))
            .unwrap()
            .unwrap()
            .expect("ordinary follower must follow the leader immediately");
        assert_eq!(ordinary_follower_restore.session_id, active_lobby);
        assert!(run(storage.accept_invite_async(2, ordinary_follower_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, active_lobby).unwrap());

        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());
        let former_host_personal_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(former_host_personal_lobby > seed_lobby);
        let former_host_restore = run(storage.take_invite_async(3))
            .unwrap()
            .unwrap()
            .expect("former transport host must restore after its newer personal lobby exists");
        assert_eq!(former_host_restore.kind, INVITE_KIND_LOBBY_RESTORE);
        assert_eq!(former_host_restore.sender, 1);
        assert_eq!(former_host_restore.session_id, active_lobby);
        assert!(run(storage.accept_invite_async(3, former_host_restore.id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(3, 1, active_lobby).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, active_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn quick_match_member_first_leave_waits_for_the_owner_party_transaction() {
        let storage = create_test_storage("quick-match-member-first-party-return");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=32;type=2")])
                .unwrap();
        }

        // Retail may transfer native Party-lobby ownership independently of
        // the public quick-match host. User 3 owns the Party lobby while user
        // 2 owns the matchmaking Session, matching the three-client trace.
        let shared_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 11;4 => 0".to_string()).unwrap();
        let member_invite = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, member_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, shared_lobby).unwrap());

        let quick_match = storage.create_game_session(2, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(3, 1, quick_match).unwrap());
        storage.record_published_game_session(2, u64::from(shared_lobby));
        storage.record_published_game_session(3, u64::from(shared_lobby));

        let late_invite = run(storage.add_invite_async(3, 1, INVITE_KIND_PRIVATE_ROOM, 1, quick_match)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(1, late_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(1, 1, quick_match).unwrap());

        // Actual retail ordering: a Party member leaves first; 157ms later the
        // quick-match owner abandons. The first call must not detach user 3 to
        // a solo Party before the coordinated transaction becomes visible.
        storage.leave_game_session(3, 1, quick_match).unwrap();
        let (leader_before_owner, roster_before_owner) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_before_owner, 3);
        let mut members_before_owner = roster_before_owner.into_iter().map(|member| member.id).collect::<Vec<_>>();
        members_before_owner.sort_unstable();
        assert_eq!(members_before_owner, vec![1, 2, 3]);

        storage.abandon_game_session(2, 1, quick_match).unwrap();

        assert!(!storage.game_session_is_active(1, quick_match).unwrap());
        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 3 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_ne!(restored_lobby, shared_lobby);
        assert!(!storage.game_session_is_active(1, shared_lobby).unwrap());
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (3, "113 => 1;3 => 11;4 => 0".to_string(), vec![1, 2, 3])
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        let mut member_ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        member_ids.sort_unstable();
        assert_eq!(member_ids, vec![1, 2, 3]);
        assert!(storage.pending_quick_match_member_leaves.lock().unwrap().is_empty());
    }

    #[test]
    fn quick_match_split_before_old_host_abandon_restores_the_departing_party() {
        let storage = create_test_storage("quick-match-split-before-old-host-abandon");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=34;type=2")])
                .unwrap();
        }

        // User 1 is the late member re-invited by successor/Party leader 2;
        // user 3 is the other current member. All clients therefore project
        // the same authoritative three-player Party before the exit begins.
        let successor_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [3, 1] {
            let party_invite = run(storage.add_invite_async(2, receiver, INVITE_KIND_LOBBY_PARTY, 1, successor_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, party_invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, successor_lobby).unwrap());
        }

        let quick_match = storage.create_game_session(2, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(2, 1, quick_match, vec![1, 3], Vec::new()).unwrap();
        for user_id in 1..=3 {
            storage.record_published_game_session(user_id, u64::from(successor_lobby));
        }

        // Actual failing ordering: follower 3 is removed and leaves first; late
        // member 1 submits Split next; 12ms later current leader 2 submits
        // Abandon. The first leave freezes all three members, while only the
        // final leader Abandon authorizes one shared lobby transition.
        assert!(storage.remove_participants(2, 1, quick_match, vec![3]).unwrap());
        storage.leave_game_session(3, 1, quick_match).unwrap();
        let (departed_leader, departed_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(departed_leader, 2);
        let mut departed_members = departed_roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        departed_members.sort_unstable();
        assert_eq!(departed_members, vec![1, 2, 3]);
        assert_eq!(storage.pending_quick_match_member_leaves(1, quick_match), vec![3]);
        assert!(storage.migrate_game_session_host(1, 1, quick_match).unwrap());
        storage.abandon_game_session(2, 1, quick_match).unwrap();

        assert!(!storage.game_session_is_active(1, quick_match).unwrap());
        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 2 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_ne!(restored_lobby, successor_lobby);
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (2, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3])
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        let mut member_ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        member_ids.sort_unstable();
        assert_eq!(member_ids, vec![1, 2, 3]);
        assert!(storage.pending_quick_match_member_leaves.lock().unwrap().is_empty());
    }

    #[test]
    fn quick_match_original_leader_rejoins_successor_party_before_coordinated_return() {
        let storage = create_test_storage("quick-match-original-leader-rejoins-successor-party");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=36;type=2")])
                .unwrap();
        }

        // Exact retail scenario: user 1 creates the lobby and invites users 2
        // and 3 before entering public matchmaking.
        let original_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [2, 3] {
            let party_invite = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, original_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, party_invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, original_lobby).unwrap());
        }
        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2, 3], Vec::new()).unwrap();
        for user_id in 1..=3 {
            storage.record_published_game_session(user_id, u64::from(original_lobby));
        }

        // User 1 leaves alone. Retail moves user 1 to a personal lobby while
        // user 2 takes the quick-match/Party role. The old lobby separately
        // migrates to user 3; that transport migration must not overwrite the
        // Party leader already elected by the public-match departure.
        storage.abandon_game_session(1, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(2, 1, quick_match).unwrap());
        storage.abandon_game_session(1, 1, original_lobby).unwrap();
        assert!(storage.migrate_game_session_host(3, 1, original_lobby).unwrap());
        let personal_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.record_published_game_session(1, u64::from(personal_lobby));
        storage.record_published_game_session(2, u64::from(original_lobby));
        storage.record_published_game_session(3, u64::from(original_lobby));

        let (successor_id, successor_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(successor_id, 2);
        assert_eq!(successor_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2, 3]);

        // A current Party member directly invites user 1 back into the public
        // room. Joining that accepted generation must reattach user 1 to the
        // successor Party before any coordinated leave begins.
        let rejoin_invite = run(storage.add_invite_async(2, 1, INVITE_KIND_PRIVATE_ROOM, 1, quick_match)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(1, rejoin_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(1, 1, quick_match).unwrap());
        let (rejoined_leader, rejoined_roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(rejoined_leader, 2);
        let mut rejoined_ids = rejoined_roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        rejoined_ids.sort_unstable();
        assert_eq!(rejoined_ids, vec![1, 2, 3]);

        // Exact final command ordering from the three-client trace: follower 3
        // leaves first, rejoined user 1 submits Split, then leader 2 abandons.
        assert!(storage.remove_participants(2, 1, quick_match, vec![3]).unwrap());
        storage.leave_game_session(3, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(1, 1, quick_match).unwrap());
        storage.abandon_game_session(2, 1, quick_match).unwrap();

        assert!(!storage.game_session_is_active(1, quick_match).unwrap());
        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 2 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        let (_, _, restored_members) = storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap();
        assert_eq!(restored_members, vec![1, 2, 3]);
        let (restored_leader, restored_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(restored_leader, 2);
        let mut restored_ids = restored_roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        restored_ids.sort_unstable();
        assert_eq!(restored_ids, vec![1, 2, 3]);
    }

    #[test]
    fn quick_match_successor_invites_a_new_member_without_reclaiming_the_original_leader() {
        let storage = create_test_storage("quick-match-successor-invites-new-member");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=37;type=2")])
                .unwrap();
        }

        let original_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [2, 3] {
            let invite = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, original_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, original_lobby).unwrap());
        }
        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2, 3], Vec::new()).unwrap();
        for user_id in 1..=3 {
            storage.record_published_game_session(user_id, u64::from(original_lobby));
        }

        storage.abandon_game_session(1, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(2, 1, quick_match).unwrap());
        storage.abandon_game_session(1, 1, original_lobby).unwrap();
        assert!(storage.migrate_game_session_host(3, 1, original_lobby).unwrap());
        let original_leader_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.record_published_game_session(1, u64::from(original_leader_lobby));
        storage.record_published_game_session(2, u64::from(original_lobby));
        storage.record_published_game_session(3, u64::from(original_lobby));

        let newcomer_lobby = storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.record_published_game_session(4, u64::from(newcomer_lobby));
        let newcomer_invite = run(storage.add_invite_async(2, 4, INVITE_KIND_PRIVATE_ROOM, 1, quick_match)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(4, newcomer_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(4, 1, quick_match).unwrap());

        let (current_leader, current_roster) = run(storage.find_party_roster_async(4)).unwrap().unwrap().unwrap();
        assert_eq!(current_leader, 2);
        let mut current_ids = current_roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        current_ids.sort_unstable();
        assert_eq!(current_ids, vec![2, 3, 4]);
        assert_eq!(run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap().1.len(), 1);

        assert!(storage.remove_participants(2, 1, quick_match, vec![3]).unwrap());
        storage.leave_game_session(3, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(4, 1, quick_match).unwrap());
        storage.abandon_game_session(2, 1, quick_match).unwrap();

        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 2 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![2, 3, 4]);
        let (restored_leader, restored_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(restored_leader, 2);
        let mut restored_ids = restored_roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        restored_ids.sort_unstable();
        assert_eq!(restored_ids, vec![2, 3, 4]);
        assert_eq!(run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap().1.len(), 1);
    }

    #[test]
    fn quick_match_split_before_leader_abandon_restores_the_complete_n_member_party() {
        let storage = create_test_storage("quick-match-split-before-leader-abandon-n-members");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=35;type=2")])
                .unwrap();
        }

        let successor_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [3, 4, 1] {
            let party_invite = run(storage.add_invite_async(2, receiver, INVITE_KIND_LOBBY_PARTY, 1, successor_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, party_invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, successor_lobby).unwrap());
        }

        let quick_match = storage.create_game_session(2, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(2, 1, quick_match, vec![1, 3, 4], Vec::new()).unwrap();
        for user_id in 1..=4 {
            storage.record_published_game_session(user_id, u64::from(successor_lobby));
        }

        assert!(storage.remove_participants(2, 1, quick_match, vec![4]).unwrap());
        storage.leave_game_session(4, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(1, 1, quick_match).unwrap());
        storage.abandon_game_session(2, 1, quick_match).unwrap();

        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 2 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (2, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3, 4])
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        let mut member_ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        member_ids.sort_unstable();
        assert_eq!(member_ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn quick_match_solo_member_leave_is_restored_after_the_coordination_window() {
        let storage = create_test_storage("quick-match-solo-member-leave");
        insert_users(&storage, 2);
        for user_id in 1..=2 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=33;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, shared_lobby).unwrap());
        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, quick_match).unwrap());

        storage.leave_game_session(2, 1, quick_match).unwrap();
        assert_eq!(run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap().0, 1);
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);

        let (leader_id, roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn accepted_normal_invite_suppresses_quick_match_lobby_restore_until_join_completes() {
        let storage = create_test_storage("accepted-invite-suppresses-quick-match-restore");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=35;type=2")])
                .unwrap();
        }

        storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let departed_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, departed_match, vec![2], Vec::new()).unwrap();
        let target_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        storage.leave_game_session(2, 1, departed_match).unwrap();
        let invite_id = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, target_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().is_some());

        // The creation entry point has the same guard as the expiry path, so
        // another caller cannot inject a competing kind-4 event mid-join.
        let direct_restore = run(async {
            let mut transaction = storage.begin_write().await?;
            let restore = Storage::queue_departed_member_lobby_restore_async(&mut transaction, 2, 1, departed_match).await?;
            transaction.rollback().await?;
            Ok::<Option<(u32, bool, u64)>, sqlx::Error>(restore)
        })
        .unwrap()
        .unwrap();
        assert!(direct_restore.is_none());

        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0);
        assert_eq!(storage.pending_quick_match_member_leaves(1, departed_match), vec![2]);
        let pending_restores: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver = 2 AND kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_restores, 0);

        assert!(storage.join_game_session_unbounded(2, 1, target_lobby).unwrap());
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0);
        assert!(storage.pending_quick_match_member_leaves(1, departed_match).is_empty());
        let pending_restores: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver = 2 AND kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_restores, 0);
    }

    #[test]
    fn invalidated_normal_invite_releases_exactly_one_quick_match_lobby_restore() {
        let storage = create_test_storage("invalidated-invite-releases-quick-match-restore");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=36;type=2")])
                .unwrap();
        }

        storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let departed_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, departed_match, vec![2], Vec::new()).unwrap();
        let target_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();

        storage.leave_game_session(2, 1, departed_match).unwrap();
        let invite_id = run(storage.add_invite_async(3, 2, INVITE_KIND_LOBBY_PARTY, 1, target_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().is_some());
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 0);

        run(sqlx::query("UPDATE game_sessions SET destroyed_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(target_lobby)
            .execute(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(expire_pending_quick_match_member_leaves_for_test(&storage), 1);

        let pending_restores: i64 = run(sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE receiver = 2 AND kind = ? AND consumed_at IS NULL")
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(pending_restores, 1);
        assert!(storage.pending_quick_match_member_leaves(1, departed_match).is_empty());
    }

    #[test]
    fn quick_match_leave_with_party_reverse_order_restores_the_authoritative_party() {
        let storage = create_test_storage("quick-match-reverse-party-return");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=26;type=2")])
                .unwrap();
        }

        let shared_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, shared_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, shared_lobby).unwrap());
        }
        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2, 3], Vec::new()).unwrap();
        storage.record_published_game_session(1, u64::from(shared_lobby));

        // Retail sends the owner Abandon first. The elected host then removes
        // the Party members in later calls, which is the ordering seen in the
        // three-client quick-match trace.
        storage.abandon_game_session(1, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(3, 1, quick_match).unwrap());
        assert!(storage.remove_participants(3, 1, quick_match, vec![1]).unwrap());
        assert!(storage.remove_participants(3, 1, quick_match, vec![2]).unwrap());

        assert!(!storage.game_session_is_active(1, quick_match).unwrap());
        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 1 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_ne!(restored_lobby, shared_lobby);
        assert!(!storage.game_session_is_active(1, shared_lobby).unwrap());
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (1, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3])
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn quick_match_party_return_does_not_merge_another_party_in_the_same_room() {
        let storage = create_test_storage("quick-match-multiple-party-return");
        insert_users(&storage, 5);
        for user_id in 1..=5 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=27;type=2")])
                .unwrap();
        }

        let party_a_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in 2..=3 {
            let invite_id = run(storage.add_invite_async(1, receiver, INVITE_KIND_LOBBY_PARTY, 1, party_a_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite_id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, party_a_lobby).unwrap());
        }
        let party_b_lobby = storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_b_invite = run(storage.add_invite_async(4, 5, INVITE_KIND_LOBBY_PARTY, 1, party_b_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(5, party_b_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(5, 1, party_b_lobby).unwrap());

        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2, 3, 4, 5], Vec::new()).unwrap();
        storage.record_published_game_session(1, u64::from(party_a_lobby));
        storage.abandon_game_session(1, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(4, 1, quick_match).unwrap());
        assert!(storage.remove_participants(4, 1, quick_match, vec![1]).unwrap());
        assert!(storage.remove_participants(4, 1, quick_match, vec![2]).unwrap());

        let (creator_id, _, remaining_match_members) = storage.game_session_diagnostics(1, quick_match).unwrap().unwrap();
        assert_eq!(creator_id, 4);
        assert_eq!(remaining_match_members, vec![4, 5]);
        let (party_a_leader, party_a_roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(party_a_leader, 1);
        assert_eq!(party_a_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        let restored_party_a_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 1 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_ne!(restored_party_a_lobby, party_a_lobby);
        assert_eq!(storage.game_session_diagnostics(1, restored_party_a_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        assert_eq!(storage.game_session_diagnostics(1, party_b_lobby).unwrap().unwrap().2, vec![4, 5]);
        let (party_b_leader, party_b_roster) = run(storage.find_party_roster_async(5)).unwrap().unwrap().unwrap();
        assert_eq!(party_b_leader, 4);
        assert_eq!(party_b_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![4, 5]);
    }

    #[test]
    fn quick_match_host_leaving_alone_transfers_only_their_own_party_leadership() {
        let storage = create_test_storage("quick-match-host-leaves-own-party");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=31;type=2")])
                .unwrap();
        }

        let party_a_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_a_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, party_a_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, party_a_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, party_a_lobby).unwrap());

        let party_b_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_b_invite = run(storage.add_invite_async(3, 4, INVITE_KIND_LOBBY_PARTY, 1, party_b_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(4, party_b_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(4, 1, party_b_lobby).unwrap());

        let quick_match = storage.create_game_session(1, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(1, 1, quick_match, vec![2, 3, 4], Vec::new()).unwrap();
        storage.record_published_game_session(1, u64::from(party_a_lobby));

        // User 1 is both the quick-match host and Party A leader, but leaves
        // alone. The game may elect user 3 as the room host; Party A must
        // independently elect its own remaining member, user 2.
        storage.abandon_game_session(1, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(3, 1, quick_match).unwrap());
        assert!(storage.remove_participants(3, 1, quick_match, vec![1]).unwrap());

        let (room_host, _, room_members) = storage.game_session_diagnostics(1, quick_match).unwrap().unwrap();
        assert_eq!(room_host, 3);
        assert_eq!(room_members, vec![2, 3, 4]);

        let (party_a_leader, party_a_roster) = run(storage.find_party_roster_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(party_a_leader, 2);
        assert_eq!(party_a_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![2]);

        let (party_b_leader, party_b_roster) = run(storage.find_party_roster_async(4)).unwrap().unwrap().unwrap();
        assert_eq!(party_b_leader, 3);
        assert_eq!(party_b_roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![3, 4]);
    }

    #[test]
    fn lobby_party_follows_leader_into_quick_match_hosted_by_another_user() {
        let storage = create_test_storage("party-follow-quick-match");
        insert_users(&storage, 4);
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=14;type=2")])
                .unwrap();
        }
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let invite_id = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_some());
        assert!(run(storage.accept_invite_async(2, invite_id)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let match_id = storage.create_game_session(4, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(1, 1, match_id).unwrap());
        run(async {
            sqlx::query("UPDATE invites SET created = datetime('now', '-3 seconds') WHERE kind = ?")
                .bind(INVITE_KIND_PARTY_FOLLOW)
                .execute(&storage.pool)
                .await
        })
        .unwrap()
        .unwrap();

        let follow = run(storage.take_invite_async(2)).unwrap().unwrap().unwrap();
        assert_eq!(follow.kind, INVITE_KIND_PARTY_FOLLOW);
        assert_eq!(follow.sender, 1);
        assert!(run(storage.accept_invite_async(2, follow.id)).unwrap().unwrap().is_some());
        let target = run(storage.find_accepted_invited_session_async(2, 1)).unwrap().unwrap().unwrap();
        assert_eq!(target.session.session_id, match_id);
        assert_eq!(target.session.creator_id, 4);
        assert!(storage.join_game_session_unbounded(2, 1, match_id).unwrap());

        let (_, _, participant_ids) = storage.game_session_diagnostics(1, match_id).unwrap().unwrap();
        assert_eq!(participant_ids, vec![1, 2, 4]);
    }

    #[test]
    fn native_party_follow_suppresses_redundant_fallback_invite() {
        let storage = create_test_storage("party-follow-native-suppression");
        insert_users(&storage, 2);
        for user_id in 1..=2 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=14;type=2")])
                .unwrap();
        }
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let lobby_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, lobby_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let private_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, private_id).unwrap());
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());

        let (remaining,): (i64,) = run(sqlx::query_as("SELECT COUNT(*) FROM invites WHERE receiver = 2 AND kind = ?")
            .bind(INVITE_KIND_PARTY_FOLLOW)
            .fetch_one(&storage.pool))
        .unwrap()
        .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn manual_and_party_follow_invites_use_independent_lanes() {
        let storage = create_test_storage("invite-lane-isolation");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=14;type=2")])
                .unwrap();
        }

        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let lobby_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, lobby_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());

        let friend_room = storage.create_game_session(3, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(run(storage.add_invite_async(3, 2, INVITE_KIND_PRIVATE_ROOM, 1, friend_room)).unwrap().unwrap().is_some());

        let leader_room = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        let pending =
            run(sqlx::query_as::<_, (i32, u32)>("SELECT kind, session_id FROM invites WHERE receiver = 2 AND consumed_at IS NULL ORDER BY kind").fetch_all(&storage.pool))
                .unwrap()
                .unwrap();
        assert_eq!(pending, vec![(INVITE_KIND_PRIVATE_ROOM, friend_room), (INVITE_KIND_PARTY_FOLLOW, leader_room)]);

        assert!(run(storage.add_invite_async(3, 2, INVITE_KIND_PRIVATE_ROOM, 1, friend_room)).unwrap().unwrap().is_some());
        let pending =
            run(sqlx::query_as::<_, (i32, u32)>("SELECT kind, session_id FROM invites WHERE receiver = 2 AND consumed_at IS NULL ORDER BY kind").fetch_all(&storage.pool))
                .unwrap()
                .unwrap();
        assert_eq!(pending, vec![(INVITE_KIND_PRIVATE_ROOM, friend_room), (INVITE_KIND_PARTY_FOLLOW, leader_room)]);
    }

    #[test]
    fn duplicate_invite_target_detects_same_party_and_same_room() {
        let storage = create_test_storage("duplicate-invite-target");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=14;type=2".to_string()]).unwrap();
        let lobby_id = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let lobby_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, lobby_id)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, lobby_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, lobby_id).unwrap());
        assert!(run(storage.invite_target_already_contains_async(2, INVITE_KIND_LOBBY_PARTY, lobby_id)).unwrap().unwrap());

        let room_id = storage.create_game_session(1, 1, "113 => 0;103 => 0;112 => 4".to_string()).unwrap();
        assert!(!run(storage.invite_target_already_contains_async(2, INVITE_KIND_PRIVATE_ROOM, room_id)).unwrap().unwrap());
        assert!(storage.join_game_session_unbounded(2, 1, room_id).unwrap());
        assert!(run(storage.invite_target_already_contains_async(2, INVITE_KIND_PRIVATE_ROOM, room_id)).unwrap().unwrap());
    }

    #[test]
    fn restored_lobby_remove_before_split_keeps_the_authoritative_party() {
        let storage = create_test_storage("restored-lobby-remove-before-split");
        insert_users(&storage, 3);
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=41;type=2")])
                .unwrap();
        }

        let original_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [2, 1] {
            let invite = run(storage.add_invite_async(3, receiver, INVITE_KIND_LOBBY_PARTY, 1, original_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, original_lobby).unwrap());
        }

        let restored_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_id = run(sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = 3").fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        for receiver in [2, 1] {
            let restore = run(sqlx::query(
                "INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at) \
                     VALUES (3, ?, ?, 1, ?, ?, datetime('now', '+5 minutes'))",
            )
            .bind(receiver)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(restored_lobby)
            .bind(party_id)
            .execute(&storage.pool))
            .unwrap()
            .unwrap()
            .last_insert_rowid();
            assert!(run(storage.accept_invite_async(receiver, restore)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, restored_lobby).unwrap());
        }

        assert!(storage.remove_participants(3, 1, restored_lobby, vec![1]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(1, 1, restored_lobby).unwrap());
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (3, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3])
        );
        assert!(!storage.remove_participants(1, 1, restored_lobby, vec![2, 3]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        let mut roster_ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        roster_ids.sort_unstable();
        assert_eq!(roster_ids, vec![1, 2, 3]);
    }

    fn maintenance_restore_expiry_case(migration_query: bool) {
        let storage = create_test_storage(if migration_query { "restore-expiry-query" } else { "restore-expiry-defer" });
        insert_users(&storage, 4);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=42;type=2".to_string()]).unwrap();
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=42;type=2".to_string()]).unwrap();
        let original_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, original_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, party_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, original_lobby).unwrap());

        let restored_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_id = run(sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = 1").fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        let restore = run(sqlx::query(
            "INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at) \
                 VALUES (1, 2, ?, 1, ?, ?, datetime('now', '+5 minutes'))",
        )
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .bind(restored_lobby)
        .bind(party_id)
        .execute(&storage.pool))
        .unwrap()
        .unwrap()
        .last_insert_rowid();
        assert!(run(storage.accept_invite_async(2, restore)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, restored_lobby).unwrap());
        assert!(storage.remove_participants(1, 1, restored_lobby, vec![2]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1, 2]);

        storage.lobby_restore_split_transitions.lock().unwrap().get_mut(&(1, restored_lobby, 2)).unwrap().started_at =
            Instant::now() - LOBBY_RESTORE_SPLIT_TTL - Duration::from_millis(1);
        let other_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        storage.add_participants(3, 1, other_lobby, vec![4], Vec::new()).unwrap();
        if migration_query {
            assert!(storage.migrate_game_session_host(4, 1, other_lobby).unwrap());
        } else {
            assert!(storage.remove_participants(3, 1, other_lobby, vec![4]).unwrap());
        }
        let due = storage.lobby_restore_split_cleanup_due();
        let resolved = storage.expire_lobby_restore_split_transitions().unwrap();
        let remaining = storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2;
        println!("RESTORE_EXPIRY migration_query={migration_query} due={due} resolved={resolved} remaining={remaining:?}");
        assert!(due, "an unrelated room lookup must not discard pending removal");
        assert_eq!(resolved, 1);
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1]);
        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1]);
        assert!(run(storage.find_party_roster_async(2)).unwrap().unwrap().is_none());
    }

    #[test]
    fn maintenance_restore_expiry_survives_unrelated_migration_query() {
        maintenance_restore_expiry_case(true);
    }
    #[test]
    fn maintenance_restore_expiry_survives_unrelated_removal_query() {
        maintenance_restore_expiry_case(false);
    }

    #[test]
    fn restored_lobby_deferred_removal_falls_back_to_normal_party_removal_without_split() {
        let storage = create_test_storage("restored-lobby-deferred-removal-expiry");
        insert_users(&storage, 2);
        storage.register_urls(1, vec!["prudp:/address=10.66.0.1;port=13000;sid=42;type=2".to_string()]).unwrap();
        storage.register_urls(2, vec!["prudp:/address=10.66.0.2;port=13000;sid=42;type=2".to_string()]).unwrap();
        let original_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_invite = run(storage.add_invite_async(1, 2, INVITE_KIND_LOBBY_PARTY, 1, original_lobby)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(2, party_invite)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, original_lobby).unwrap());

        let restored_lobby = storage.create_game_session(1, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_id = run(sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = 1").fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        let restore = run(sqlx::query(
            "INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at) \
                 VALUES (1, 2, ?, 1, ?, ?, datetime('now', '+5 minutes'))",
        )
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .bind(restored_lobby)
        .bind(party_id)
        .execute(&storage.pool))
        .unwrap()
        .unwrap()
        .last_insert_rowid();
        assert!(run(storage.accept_invite_async(2, restore)).unwrap().unwrap().is_some());
        assert!(storage.join_game_session_unbounded(2, 1, restored_lobby).unwrap());
        assert!(storage.remove_participants(1, 1, restored_lobby, vec![2]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1, 2]);

        storage.lobby_restore_split_transitions.lock().unwrap().get_mut(&(1, restored_lobby, 2)).unwrap().started_at =
            Instant::now() - LOBBY_RESTORE_SPLIT_TTL - Duration::from_millis(1);
        assert!(storage.lobby_restore_split_cleanup_due());
        assert_eq!(storage.expire_lobby_restore_split_transitions().unwrap(), 1);
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1]);
        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 1);
        assert_eq!(roster.into_iter().map(|member| member.id).collect::<Vec<_>>(), vec![1]);
        assert!(run(storage.find_party_roster_async(2)).unwrap().unwrap().is_none());
    }

    #[test]
    fn manual_invitation_search_intent_can_be_armed_and_cleared() {
        let storage = create_test_storage("manual-invite-search-intent");
        assert!(!storage.manual_invitation_search_is_active(2, 1));
        storage.note_manual_invitation_search(2, 1);
        assert!(storage.manual_invitation_search_is_active(2, 1));
        storage.clear_manual_invitation_search(2, 1);
        assert!(!storage.manual_invitation_search_is_active(2, 1));
    }

    #[test]
    fn password_reset_increments_auth_version_atomically() {
        let storage = create_test_storage("password-reset-auth-version");
        run(async {
            sqlx::query("INSERT INTO users (id, username, password_hash, ubi_id, recovery_code_hash) VALUES (424242, 'player', 'old-hash', 'account-1', 'recovery-v1')")
                .execute(&storage.pool)
                .await?;
            Ok::<(), sqlx::Error>(())
        })
        .unwrap()
        .unwrap();

        assert!(run(storage.reset_password_with_recovery_async("player", "recovery-v1", "new-password", "recovery-v2"))
            .unwrap()
            .unwrap());
        let identity = run(storage.find_auth_identity_by_user_id_async(424242)).unwrap().unwrap().unwrap();
        assert_eq!(identity.auth_version, 2);
        assert!(!run(storage.reset_password_with_recovery_async("player", "recovery-v1", "another-password", "recovery-v3"))
            .unwrap()
            .unwrap());
        let identity = run(storage.find_auth_identity_by_user_id_async(424242)).unwrap().unwrap().unwrap();
        assert_eq!(identity.auth_version, 2);
    }
    fn maintenance_native_client(storage:&Storage,user:u32)->quazal::ClientInfo<()> {
        let key:[u8;16]=rand::random();
        storage.create_bound_user_session(user,&key,"127.0.0.1".parse().unwrap()).unwrap();
        let mut ci=quazal::ClientInfo::new("127.0.0.1:30500".parse().unwrap());
        ci.user_id=Some(user);ci.ticket_fingerprint=Some(quazal::kerberos::ticket_fingerprint(&key));
        assert!(storage.mark_secure_ticket(user,ci.ticket_fingerprint).unwrap());ci
    }
    fn maintenance_begin_run(storage:&Storage,user:u32,instance:&str,mode:i64) {
        run(async {let _guard=storage.mode_gate.write().await;storage.begin_game_run(user,instance,mode,"127.0.0.1".parse().unwrap()).await}).unwrap().unwrap();
    }
    #[test]
    fn maintenance_mode_run_ticket_and_source_binding() {
        let s=create_test_storage("mode-tickets");insert_users(&s,2);
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        let ci=maintenance_native_client(&s,1);
        let first=s.native_game_run(1,ci.ticket_fingerprint,ci.address().ip()).unwrap().unwrap();
        assert_eq!(first.mode,1);
        assert!(s.native_game_run(2,ci.ticket_fingerprint,ci.address().ip()).unwrap().is_none());
        assert!(s.native_game_run(1,ci.ticket_fingerprint,"127.0.0.2".parse().unwrap()).unwrap().is_none());
        assert!(s.native_game_run(1,None,ci.address().ip()).unwrap().is_none());
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        assert_eq!(s.native_game_run(1,ci.ticket_fingerprint,ci.address().ip()).unwrap(),Some(first));
    }
    #[test]
    fn maintenance_mode_switch_retires_old_token_ticket_and_preserves_room_mode() {
        let s=create_test_storage("mode-switch");insert_users(&s,1);
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        let old=maintenance_native_client(&s,1);
        let room=s.create_game_session(1,1,"113 => 0;3 => 4;4 => 0".into()).unwrap();
        maintenance_begin_run(&s,1,"22222222222222222222222222222222",2);
        assert!(s.native_game_run(1,old.ticket_fingerprint,old.address().ip()).unwrap().is_none());
        assert!(!run(s.touch_game_run(1,"11111111111111111111111111111111",1,"127.0.0.1".parse().unwrap())).unwrap().unwrap());
        assert!(run(s.begin_game_run(1,"11111111111111111111111111111111",1,"127.0.0.1".parse().unwrap())).unwrap().is_err());
        assert!(s.room_matches_mode(1,room,1).unwrap());assert!(!s.room_matches_mode(1,room,2).unwrap());
        let new=maintenance_native_client(&s,1);
        assert_eq!(s.native_game_run(1,new.ticket_fingerprint,new.address().ip()).unwrap().unwrap().mode,2);
        assert!(s.close_secure_ticket(1,old.ticket_fingerprint).unwrap());
        assert!(s.native_game_run(1,new.ticket_fingerprint,new.address().ip()).unwrap().is_some());
    }
    #[test]
    fn maintenance_old_connection_close_does_not_delete_reconnected_ticket() {
        let s=create_test_storage("mode-reconnect");insert_users(&s,1);
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",2);
        let old=maintenance_native_client(&s,1);let current=maintenance_native_client(&s,1);
        assert!(s.close_secure_ticket(1,old.ticket_fingerprint).unwrap());
        assert!(s.native_game_run(1,old.ticket_fingerprint,old.address().ip()).unwrap().is_none());
        assert!(s.native_game_run(1,current.ticket_fingerprint,current.address().ip()).unwrap().is_some());
        assert!(s.close_secure_ticket(1,None).unwrap());
        assert!(s.native_game_run(1,current.ticket_fingerprint,current.address().ip()).unwrap().is_some());
    }
    #[test]
    fn maintenance_mode_lease_expires_and_same_run_can_renew() {
        let s=create_test_storage("mode-lease");insert_users(&s,1);maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        let ci=maintenance_native_client(&s,1);
        run(sqlx::query("UPDATE game_runs SET seen_at=datetime('now','-91 seconds')").execute(&s.pool)).unwrap().unwrap();
        assert!(s.native_game_run(1,ci.ticket_fingerprint,ci.address().ip()).unwrap().is_none());
        assert_eq!(run(s.current_game_mode(1)).unwrap().unwrap(),None);
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        assert!(s.native_game_run(1,ci.ticket_fingerprint,ci.address().ip()).unwrap().is_some());
    }
    #[test]
    fn maintenance_mode_room_and_participant_constraints_are_transactional() {
        let s=create_test_storage("mode-rows");insert_users(&s,3);
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        maintenance_begin_run(&s,2,"22222222222222222222222222222222",1);
        maintenance_begin_run(&s,3,"33333333333333333333333333333333",2);
        let room=s.create_game_session(1,1,"113 => 0;3 => 4;4 => 0".into()).unwrap();
        assert!(s.join_game_session_unbounded(2,1,room).unwrap());
        assert!(s.join_game_session_unbounded(3,1,room).is_err());
        let members:Vec<(u32,)>=run(sqlx::query_as("SELECT user_id FROM participants WHERE game_id=? ORDER BY user_id").bind(room).fetch_all(&s.pool)).unwrap().unwrap();
        assert_eq!(members,vec![(1,),(2,)]);
        assert!(run(sqlx::query("UPDATE game_sessions SET gameplay_mode=2 WHERE id=?").bind(room).execute(&s.pool)).unwrap().is_err());
        assert!(run(sqlx::query("UPDATE game_sessions SET creator_id=3 WHERE id=?").bind(room).execute(&s.pool)).unwrap().is_err());
        run(sqlx::query("UPDATE game_sessions SET creator_id=2 WHERE id=?").bind(room).execute(&s.pool)).unwrap().unwrap();
        assert!(s.room_matches_mode(1,room,1).unwrap());
    }
    #[test]
    fn maintenance_server_restart_allows_same_instance_with_new_ticket_only() {
        let s=create_test_storage("mode-restart");insert_users(&s,1);maintenance_begin_run(&s,1,"11111111111111111111111111111111",2);
        let old=maintenance_native_client(&s,1);s.invalidate_sessions().unwrap();
        maintenance_begin_run(&s,1,"11111111111111111111111111111111",2);
        assert!(s.native_game_run(1,old.ticket_fingerprint,old.address().ip()).unwrap().is_none());
        let new=maintenance_native_client(&s,1);assert!(s.native_game_run(1,new.ticket_fingerprint,new.address().ip()).unwrap().is_some());
    }

    #[test]
    fn maintenance_signed_mode_login_to_native_admission_fixed() {
        use quazal::rmc::basic::{FromStream,ToStream};
        use quazal::rmc::Request;
        use quazal::rmc::types::{QList,Property};
        use crate::protocols::game_session_service::game_session_protocol::{CreateSessionRequest,CreateSessionResponse,JoinSessionRequest};
        use crate::protocols::game_session_service::types::{GameSession as NativeSession,GameSessionKey,GameSessionQuery};
        use crate::protocols::game_session_ex_service::game_session_ex_protocol::{SearchSessionsRequest,SearchSessionsResponse};
        use server_api::gameplay_contract::GameplayMode;
        let storage=Arc::new(create_test_storage("mode-admission-gap"));
        for name in ["official-fixture","mod-host-fixture","mod-guest-fixture"] {
            run(storage.register_user_with_recovery_async(name,"fixture-only-password",Some(&format!("ubi-{name}")),None)).unwrap().unwrap();
        }
        let official=crate::api::maintenance_authenticated_game_login(storage.clone(),"official-fixture",GameplayMode::Official.product(),"11111111111111111111111111111111");
        let mod_host=crate::api::maintenance_authenticated_game_login(storage.clone(),"mod-host-fixture",GameplayMode::Mod.product(),"22222222222222222222222222222222");
        let guest=crate::api::maintenance_authenticated_game_login(storage.clone(),"mod-guest-fixture",GameplayMode::Mod.product(),"33333333333333333333333333333333");
        for id in [official,mod_host,guest] {storage.register_urls(id,vec![format!("prudp:/address=127.0.0.1;port={};sid=42;type=2",13000+id)]).unwrap();}
        let native=crate::game_session::new_protocol::<()>(storage.clone());
        let search=crate::game_session_ex::new_protocol::<()>(storage.clone());
        let logger=Logger::root(slog::Discard,slog::o!());let context=quazal::Context::splinter_cell_blacklist();
        let clients=quazal::prudp::ClientRegistry::<()>::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
        let mut create=|id| {
            ci=maintenance_native_client(&storage,id);
            let parameters=CreateSessionRequest{game_session:NativeSession{type_id:1,attributes:QList(vec![Property{id:113,value:0},Property{id:3,value:4},Property{id:4,value:0},Property{id:103,value:2165463540},Property{id:105,value:0},Property{id:112,value:8}])}}.to_bytes();
            let bytes=native.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:1,method_id:1,parameters},&clients,&socket).unwrap();
            CreateSessionResponse::from_bytes(&bytes).unwrap().game_session_key.session_id
        };
        let official_room=create(official);let mod_room=create(mod_host);
        ci=maintenance_native_client(&storage,guest);
        let query=SearchSessionsRequest{game_session_query:GameSessionQuery{type_id:1,query_id:0,parameters:QList(vec![Property{id:103,value:2165463540},Property{id:113,value:0},Property{id:105,value:0},Property{id:112,value:2}])}};
        let bytes=search.handle(&logger,&context,&mut ci,&Request{protocol_id:123,call_id:2,method_id:1,parameters:query.to_bytes()},&clients,&socket).unwrap();
        let found=SearchSessionsResponse::from_bytes(&bytes).unwrap().search_results.0.into_iter().map(|v|v.game_session_search_result.session_key.session_id).collect::<Vec<_>>();
        let same_visible=found.contains(&mod_room);let cross_visible=found.contains(&official_room);
        assert!(same_visible);
        let join=JoinSessionRequest{game_session_key:GameSessionKey{type_id:1,session_id:official_room}};
        let cross_join=native.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:3,method_id:22,parameters:join.to_bytes()},&clients,&socket).is_ok();
        println!("MODE_ADMISSION signed_modes_verified=true same_mode_visible={same_visible} cross_mode_visible={cross_visible} cross_mode_join_recorded={cross_join} live_game=false");
        // Same input as the baseline, now incompatible search/join must fail.
        assert!(!cross_visible);assert!(!cross_join);
        let same=JoinSessionRequest{game_session_key:GameSessionKey{type_id:1,session_id:mod_room}};
        assert!(native.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:4,method_id:22,parameters:same.to_bytes()},&clients,&socket).is_ok());
        println!("MODE_ADMISSION same_mode_join_recorded=true private_filters_preserved=true");
    }

    #[test]
    fn maintenance_mode_actual_search_private_invitation_matrix() {
        use quazal::rmc::basic::{FromStream,ToStream};
        use quazal::rmc::{Request,Error};
        use quazal::rmc::types::{QList,Property};
        use crate::protocols::game_session_ex_service::game_session_ex_protocol::{SearchSessionsRequest,SearchSessionsResponse};
        use crate::protocols::game_session_service::types::GameSessionQuery;
        let storage=Arc::new(create_test_storage("maintenance-actual-search"));
        insert_users(&storage,4);
        for id in 1..=4 {maintenance_begin_run(&storage,id,&format!("{id:032x}"),1);}
        for id in 1..=3 {storage.register_urls(id,vec![format!("prudp:/address=127.0.0.{};port=13000;sid=42;type=2",id)]).unwrap();}
        let public=storage.create_game_session(1,1,"113 => 0;3 => 4;4 => 0;103 => 2165463540;105 => 0;112 => 8".into()).unwrap();
        let private=storage.create_game_session(2,1,"113 => 0;3 => 0;4 => 8;103 => 2165463540;105 => 0;112 => 8".into()).unwrap();
        let no_host_url=storage.create_game_session(4,1,"113 => 0;3 => 4;4 => 0;103 => 2165463540;105 => 0;112 => 8".into()).unwrap();
        let protocol=crate::game_session_ex::new_protocol::<()>(storage.clone());
        let logger=Logger::root(slog::Discard,slog::o!());
        let ctx=quazal::Context::splinter_cell_blacklist();
        let registry=quazal::prudp::ClientRegistry::<()>::default();
        let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut ci=maintenance_native_client(&storage,3);
        let query=|ci:&mut quazal::ClientInfo<()>,query_id:u32| {
            let request=SearchSessionsRequest { game_session_query:GameSessionQuery{type_id:1,query_id,parameters:QList(vec![Property{id:103,value:2165463540},Property{id:113,value:0},Property{id:105,value:0},Property{id:112,value:2}])}};
            let rpc=Request{protocol_id:123,call_id:1,method_id:1,parameters:request.to_bytes()};
            protocol.handle(&logger,&ctx,ci,&rpc,&registry,&socket).map(|bytes|SearchSessionsResponse::from_bytes(&bytes).unwrap())
        };
        let ids=|response:SearchSessionsResponse|response.search_results.0.into_iter().map(|r|{assert!(!r.game_session_search_result.host_urls.is_empty());r.game_session_search_result.session_key.session_id}).collect::<Vec<_>>();
        let found=ids(query(&mut ci,0).unwrap());assert_eq!(found,vec![public]);assert!(!found.contains(&private));assert!(!found.contains(&no_host_url));
        println!("SEARCH_PROTOCOL quick_public=true private_hidden=true host_without_url_hidden=true legacy_capacity_ignored=true");
        let invitation=run(storage.add_invite_async(2,3,INVITE_KIND_PRIVATE_ROOM,1,private)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(3,invitation)).unwrap().unwrap().is_some());
        assert_eq!(ids(query(&mut ci,8).unwrap()),vec![private]);assert!(storage.manual_invitation_search_is_active(3,1));
        assert_eq!(ids(query(&mut ci,0).unwrap()),vec![public]);assert!(!storage.manual_invitation_search_is_active(3,1));
        println!("SEARCH_PROTOCOL accepted_invite_exact_private=true quick_after_invite_public_only=true invite_intent_cleared=true");
        ci.user_id=None;assert!(matches!(query(&mut ci,0),Err(Error::AccessDenied)));
        println!("SEARCH_PROTOCOL unauthenticated_rejected=true live_game=false remote_server=false");
    }

    #[test]
    fn maintenance_mode_actual_api_invitation_matrix() {
        let storage=Arc::new(create_test_storage("mode-api-invites"));
        for name in ["mode-host","mode-peer","mode-other"] {run(storage.register_user_with_recovery_async(name,"fixture-only-password",Some(name),None)).unwrap().unwrap();}
        let host=storage.find_user_id_by_name("mode-host").unwrap().unwrap();
        let peer=storage.find_user_id_by_name("mode-peer").unwrap().unwrap();
        let other=storage.find_user_id_by_name("mode-other").unwrap().unwrap();
        for (id,mode) in [(host,2),(peer,2),(other,1)] {
            maintenance_begin_run(&storage,id,&format!("{id:032x}"),mode);
            storage.register_urls(id,vec![format!("prudp:/address=127.0.0.1;port={};sid=42;type=2",13000+id)]).unwrap();
        }
        let room=storage.create_game_session(host,1,"113 => 1;3 => 8;4 => 0;105 => 0".into()).unwrap();
        crate::api::maintenance_api_invitation_checks(storage,host,peer,other,room);
    }
    #[test]
    fn maintenance_ticket_issuance_alone_is_not_a_connected_game() {
        let s=create_test_storage("mode-issued-not-connected");insert_users(&s,1);maintenance_begin_run(&s,1,"11111111111111111111111111111111",1);
        let key=[0x56;16];s.create_bound_user_session(1,&key,"127.0.0.1".parse().unwrap()).unwrap();
        let fp=Some(quazal::kerberos::ticket_fingerprint(&key));
        assert!(s.native_game_run(1,fp,"127.0.0.1".parse().unwrap()).unwrap().is_none());
        assert!(s.mark_secure_ticket(1,fp).unwrap());
        assert!(s.native_game_run(1,fp,"127.0.0.1".parse().unwrap()).unwrap().is_some());
    }

    #[test]
    fn maintenance_mode_native_ticket_before_game_login_observation() {
        let storage=create_test_storage("native-before-api-mode");insert_accounts(&storage,1);
        let key=[0x57;16];let fingerprint=Some(quazal::kerberos::ticket_fingerprint(&key));let source="127.0.0.1".parse().unwrap();
        assert!(storage.create_bound_user_session(1,&key,source).is_err());
        assert!(!storage.mark_secure_ticket(1,fingerprint).unwrap());
        let before=storage.native_game_run(1,fingerprint,source).unwrap().is_some();
        let state:(i64,bool)=run(sqlx::query_as("SELECT (SELECT COUNT(*) FROM user_sessions),is_online FROM users WHERE id=1").fetch_one(&storage.pool)).unwrap().unwrap();assert_eq!(state,(0,false));
        maintenance_begin_run(&storage,1,"11111111111111111111111111111111",1);
        let rebound=storage.native_game_run(1,fingerprint,source).unwrap().is_some();
        let fresh=maintenance_native_client(&storage,1);
        let ready=storage.native_game_run(1,fresh.ticket_fingerprint,fresh.address().ip()).unwrap().is_some();
        assert!(!before);assert!(!rebound);assert!(ready);
        println!("STARTUP_ORDER unbound_ticket_rejected=true ticket_before_login_admitted={before} old_ticket_rebound={rebound} new_ticket_after_login_admitted={ready} live_game=false");
        // Native authentication must wait for the approved signed game run.
        // No pending/unbound ticket is upgraded when that run later appears.
    }

    #[test]
    fn maintenance_mode_lifecycle_successful_four_player_host_migration_keeps_match_and_suppresses_lobby_regroup() {
        let storage = create_test_storage("four-player-host-migration-solo-fallbacks");
        insert_users(&storage, 4);
        for id in 1..=4 {maintenance_begin_run(&storage,id,&format!("{id:032x}"),2);}
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }

        let mut solo_lobbies = Vec::new();
        for receiver in 2..=4 {
            solo_lobbies.push((receiver, storage.create_game_session(receiver, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap()));
        }
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();

        for (receiver, lobby_id) in &solo_lobbies {
            let invite_id = run(storage.add_invite_async(1, *receiver, INVITE_KIND_PRIVATE_ROOM, 1, game_id)).unwrap().unwrap().unwrap();
            assert_eq!(run(storage.take_invite_async(*receiver)).unwrap().unwrap().unwrap().id, invite_id);
            assert!(run(storage.accept_invite_async(*receiver, invite_id)).unwrap().unwrap().is_some());

            // Retail sends both AbandonSession and LeaveSession for the old
            // solo lobby while it is crossing into the private match.
            storage.leave_game_session(*receiver, 1, *lobby_id).unwrap();
            storage.leave_game_session(*receiver, 1, *lobby_id).unwrap();
            assert!(storage.game_session_is_active(1, *lobby_id).unwrap());
        }

        storage.add_participants(1, 1, game_id, vec![2, 3, 4], Vec::new()).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2, 3, 4]);

        for (receiver, lobby_id) in &solo_lobbies {
            // The accepted invite is consumed after admission. A late retail
            // lobby Leave is still harmless because room membership now owns
            // the fallback lifetime.
            storage.leave_game_session(*receiver, 1, *lobby_id).unwrap();
            assert!(storage.game_session_is_active(1, *lobby_id).unwrap());
        }

        assert_eq!(
            storage.delete_game_session(1, 1, game_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 3 }
        );
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2, 3, 4]);
        for (_, lobby_id) in &solo_lobbies {
            assert!(storage.game_session_is_active(1, *lobby_id).unwrap());
        }

        // A member may leave after the migration without affecting the two
        // players who remain in the migrated match.
        storage.leave_game_session(4, 1, game_id).unwrap();
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(3, 1, game_id).unwrap());
        let (creator_id, _, participant_ids) = storage.game_session_diagnostics(1, game_id).unwrap().unwrap();
        assert_eq!(creator_id, 3);
        assert_eq!(participant_ids, vec![2, 3]);
        // SplitSession itself is the migration contract. The server must not
        // inject a second invitation state machine while retail is migrating.
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
        assert!(run(storage.take_invite_async(3)).unwrap().unwrap().is_none());

        // StateLeaveWithoutParty may create a fresh solo lobby. That creation
        // retires only this player's preserved predecessor, preventing two
        // personal lobbies without touching the migrated match or teammates.
        let old_user_four_lobby = solo_lobbies[2].1;
        let replacement = storage.create_game_session(4, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        assert!(!storage.game_session_is_active(1, old_user_four_lobby).unwrap());
        assert!(storage.game_session_is_active(1, replacement).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![2, 3]);
    }


    #[test]
    fn maintenance_mode_lifecycle_two_player_host_migration_completes_after_the_only_survivor_becomes_host() {
        let storage = create_test_storage("two-player-host-migration-confirmation");
        insert_users(&storage, 2);
        for id in 1..=2 {maintenance_begin_run(&storage,id,&format!("{id:032x}"),2);}
        for user_id in 1..=2 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid={user_id};type=2")])
                .unwrap();
        }
        let game_id = storage.create_game_session(1, 1, "113 => 0;3 => 0;4 => 8;112 => 4".to_string()).unwrap();
        assert!(storage.join_game_session_unbounded(2, 1, game_id).unwrap());
        assert!(matches!(
            storage.delete_game_session(1, 1, game_id).unwrap(),
            DeleteGameSessionOutcome::MigrationPending { remaining_participants: 1 }
        ));
        assert!(storage.migrate_game_session_host(2, 1, game_id).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, game_id).unwrap().unwrap().2, vec![2]);
        assert!(run(storage.take_invite_async(2)).unwrap().unwrap().is_none());
    }


    #[test]
    fn maintenance_mode_lifecycle_quick_match_split_before_leader_abandon_restores_the_complete_n_member_party() {
        let storage = create_test_storage("quick-match-split-before-leader-abandon-n-members");
        insert_users(&storage, 4);
        for id in 1..=4 {maintenance_begin_run(&storage,id,&format!("{id:032x}"),2);}
        for user_id in 1..=4 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=35;type=2")])
                .unwrap();
        }

        let successor_lobby = storage.create_game_session(2, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [3, 4, 1] {
            let party_invite = run(storage.add_invite_async(2, receiver, INVITE_KIND_LOBBY_PARTY, 1, successor_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, party_invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, successor_lobby).unwrap());
        }

        let quick_match = storage.create_game_session(2, 1, "113 => 0;3 => 12;4 => 0;103 => 4008726265".to_string()).unwrap();
        storage.add_participants(2, 1, quick_match, vec![1, 3, 4], Vec::new()).unwrap();
        for user_id in 1..=4 {
            storage.record_published_game_session(user_id, u64::from(successor_lobby));
        }

        assert!(storage.remove_participants(2, 1, quick_match, vec![4]).unwrap());
        storage.leave_game_session(4, 1, quick_match).unwrap();
        assert!(storage.migrate_game_session_host(1, 1, quick_match).unwrap());
        storage.abandon_game_session(2, 1, quick_match).unwrap();

        let restored_lobby = run(
            sqlx::query_scalar::<_, u32>("SELECT session_id FROM invites WHERE receiver = 2 AND kind = ? ORDER BY id DESC LIMIT 1")
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .fetch_one(&storage.pool),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (2, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3, 4])
        );
        let (leader_id, roster) = run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 2);
        let mut member_ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        member_ids.sort_unstable();
        assert_eq!(member_ids, vec![1, 2, 3, 4]);
    }


    #[test]
    fn maintenance_mode_lifecycle_restored_lobby_remove_before_split_keeps_the_authoritative_party() {
        let storage = create_test_storage("restored-lobby-remove-before-split");
        insert_users(&storage, 3);
        for id in 1..=3 {maintenance_begin_run(&storage,id,&format!("{id:032x}"),2);}
        for user_id in 1..=3 {
            storage
                .register_urls(user_id, vec![format!("prudp:/address=10.66.0.{user_id};port=13000;sid=41;type=2")])
                .unwrap();
        }

        let original_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        for receiver in [2, 1] {
            let invite = run(storage.add_invite_async(3, receiver, INVITE_KIND_LOBBY_PARTY, 1, original_lobby))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(run(storage.accept_invite_async(receiver, invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, original_lobby).unwrap());
        }

        let restored_lobby = storage.create_game_session(3, 1, "113 => 1;3 => 8;4 => 0".to_string()).unwrap();
        let party_id = run(sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = 3").fetch_one(&storage.pool))
            .unwrap()
            .unwrap();
        for receiver in [2, 1] {
            let restore = run(sqlx::query(
                "INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at) \
                     VALUES (3, ?, ?, 1, ?, ?, datetime('now', '+5 minutes'))",
            )
            .bind(receiver)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(restored_lobby)
            .bind(party_id)
            .execute(&storage.pool))
            .unwrap()
            .unwrap()
            .last_insert_rowid();
            assert!(run(storage.accept_invite_async(receiver, restore)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(receiver, 1, restored_lobby).unwrap());
        }

        assert!(storage.remove_participants(3, 1, restored_lobby, vec![1]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        assert!(storage.migrate_game_session_host(1, 1, restored_lobby).unwrap());
        assert_eq!(
            storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap(),
            (3, "113 => 1;3 => 8;4 => 0".to_string(), vec![1, 2, 3])
        );
        assert!(!storage.remove_participants(1, 1, restored_lobby, vec![2, 3]).unwrap());
        assert_eq!(storage.game_session_diagnostics(1, restored_lobby).unwrap().unwrap().2, vec![1, 2, 3]);
        let (leader_id, roster) = run(storage.find_party_roster_async(1)).unwrap().unwrap().unwrap();
        assert_eq!(leader_id, 3);
        let mut roster_ids = roster.into_iter().map(|member| member.id).collect::<Vec<_>>();
        roster_ids.sort_unstable();
        assert_eq!(roster_ids, vec![1, 2, 3]);
    }


    #[test]
    fn maintenance_real_native_login_ex_secure_registration_matrix() {
        use quazal::rmc::basic::{FromStream,ToStream};
        use quazal::rmc::{Request,Error};
        use quazal::rmc::types::{Any,Data};
        use crate::protocols::authentication_foundation::ticket_granting_protocol::{LoginExRequest,LoginExResponse};
        use crate::protocols::ubi_authentication::types::UbiAuthenticationLoginCustomData;
        use crate::protocols::secure_connection_service::secure_connection_protocol::{RegisterRequest,RegisterResponse};
        let storage=Arc::new(create_test_storage("real-native-login-register"));
        run(storage.register_user_with_recovery_async("native-fixture","fixture-only-password",Some("native-fixture-id"),None)).unwrap().unwrap();
        let user=storage.find_user_id_by_name("native-fixture").unwrap().unwrap();
        let logger=Logger::root(slog::Discard,slog::o!());let ctx=quazal::Context::splinter_cell_blacklist();
        let clients=quazal::prudp::ClientRegistry::<()>::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let ticket=crate::ticket::new_protocol::<()>(storage.clone());
        let secure=crate::secure::new_protocol::<()>(storage.clone());
        let issue=|password:&str| {
            let data=UbiAuthenticationLoginCustomData{data:Data,user_name:"native-fixture".into(),online_key:"fixture-only-online-key".into(),password:password.into()};
            let login=LoginExRequest{str_user_name:"native-fixture".into(),o_extra_data:Any::new("UbiAuthenticationLoginCustomData".into(),data.to_bytes())};
            let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
            ticket.handle(&logger,&ctx,&mut ci,&Request{protocol_id:10,call_id:1,method_id:2,parameters:login.to_bytes()},&clients,&socket)
                .map(|bytes|LoginExResponse::from_bytes(&bytes).unwrap())
        };
        assert!(matches!(issue("wrong-fixture-password"),Err(Error::AccessDenied)));
        // Native credential login cannot create a NULL-bound game ticket.
        assert!(matches!(issue("fixture-only-password"),Err(Error::InternalError)));
        let early_state:(i64,bool)=run(sqlx::query_as("SELECT (SELECT COUNT(*) FROM user_sessions WHERE user_id=?),is_online FROM users WHERE id=?").bind(user).bind(user).fetch_one(&storage.pool)).unwrap().unwrap();
        assert_eq!(early_state,(0,false));
        let connect_identity=|ticket:&quazal::kerberos::KerberosTicket| {
            // Actual encrypted CONNECT assignment is independently covered by
            // quazal::maintenance_connect_tests. This matrix composes the native
            // LoginEx and Secure Register RMCs using that verified ticket identity.
            let mut ci=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());
            ci.user_id=Some(ticket.internal.principle_id);ci.connection_id=Some("99".parse().unwrap());
            ci.ticket_fingerprint=Some(quazal::kerberos::ticket_fingerprint(&ticket.internal.session_key));ci
        };
        let register=|ci:&mut quazal::ClientInfo<()>| {
            secure.handle(&logger,&ctx,ci,&Request{protocol_id:11,call_id:2,method_id:1,parameters:RegisterRequest{vec_my_urls:vec![]}.to_bytes()},&clients,&socket)
                .map(|bytes|RegisterResponse::from_bytes(&bytes).unwrap())
        };
        let logged=crate::api::maintenance_authenticated_game_login(storage.clone(),"native-fixture","scbl-lan/2/game/mod","aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");assert_eq!(logged,user);
        let fresh=issue("fixture-only-password").unwrap();let fresh_ticket=quazal::kerberos::KerberosTicket::from_bytes(&fresh.pbuf_response,user,None,&ctx.ticket_key).unwrap();
        let mut ci=connect_identity(&fresh_ticket);assert!(storage.native_game_run(user,ci.ticket_fingerprint,ci.address().ip()).unwrap().is_none());
        let response=register(&mut ci).unwrap();assert_eq!(response.pid_connection_id,99);assert_eq!(response.url_public.port,30500);
        assert_eq!(storage.native_game_run(user,ci.ticket_fingerprint,ci.address().ip()).unwrap().unwrap().mode,2);
        let mut forged=quazal::ClientInfo::<()>::new("127.0.0.1:30500".parse().unwrap());forged.user_id=Some(user);forged.connection_id=Some("100".parse().unwrap());
        assert!(matches!(register(&mut forged),Err(Error::AccessDenied)));
        println!("NATIVE_LOGIN bad_password_rejected=true issued_ticket_decrypted=true registration_marks_connected=true missing_ticket_rejected=true early_unbound_ticket_rejected_without_presence=true mode_after_signed_login=2 live_game=false");
    }

    #[test]
    fn maintenance_search_endpoint_serialization_observer() {
        use quazal::rmc::basic::{FromStream,ToStream};
        use quazal::rmc::{Request,Error};
        use quazal::rmc::types::{QList,Property};
        use crate::protocols::game_session_ex_service::game_session_ex_protocol::{SearchSessionsRequest,SearchSessionsResponse};
        use crate::protocols::game_session_service::types::GameSessionQuery;
        let storage=Arc::new(create_test_storage("search-endpoint-boundary"));
        insert_users(&storage,2);
        for id in 1..=2 {maintenance_begin_run(&storage,id,&format!("{id:032x}"),1);}
        let valid="prudp:/address=127.0.0.1;port=13000;sid=42;type=2";
        storage.register_urls(1,vec![valid.to_owned()]).unwrap();
        let room=storage.create_game_session(1,1,"113 => 0;3 => 4;4 => 0;103 => 2165463540;105 => 0;112 => 8".into()).unwrap();
        let protocol=crate::game_session_ex::new_protocol::<()>(storage.clone());
        let logger=Logger::root(slog::Discard,slog::o!());
        let ctx=quazal::Context::splinter_cell_blacklist();
        let registry=quazal::prudp::ClientRegistry::<()>::default();
        let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut ci=maintenance_native_client(&storage,2);
        let query=|ci:&mut quazal::ClientInfo<()>| {
            let request=SearchSessionsRequest{game_session_query:GameSessionQuery{type_id:1,query_id:0,parameters:QList(vec![Property{id:103,value:2165463540},Property{id:113,value:0}])}};
            protocol.handle(&logger,&ctx,ci,&Request{protocol_id:123,call_id:1,method_id:1,parameters:request.to_bytes()},&registry,&socket)
                .map(|bytes|SearchSessionsResponse::from_bytes(&bytes).unwrap())
        };
        for (name,urls) in [
            ("valid",vec![valid.to_owned()]),
            ("none",vec![]),
            ("malformed_only",vec!["not-a-station-url".to_owned()]),
            ("mixed",vec!["not-a-station-url".to_owned(),valid.to_owned()]),
            ("valid_restored",vec![valid.to_owned()]),
        ] {
            storage.register_urls(1,urls).unwrap();
            let response=query(&mut ci).unwrap();
            let count=response.search_results.0.len();
            let endpoints=response.search_results.0.iter().map(|row| {
                assert_eq!(row.game_session_search_result.session_key.session_id,room);
                row.game_session_search_result.host_urls.0.len()
            }).sum::<usize>();
            let participants=response.search_results.0.iter().map(|row|row.participants.0.len()).sum::<usize>();
            println!("ENDPOINT_SEARCH case={name} rooms={count} host_endpoints={endpoints} participants={participants} stored_fixture_only=true");
            if name=="valid" || name=="valid_restored" {assert_eq!((count,endpoints,participants),(1,1,1));}
            if name=="none" {assert_eq!(count,0);}
        }
        ci.user_id=None;
        assert!(matches!(query(&mut ci),Err(Error::AccessDenied)));
        println!("ENDPOINT_SEARCH unauthenticated_rejected=true live_game=false network_packets_sent=false");
    }

    #[test]
    fn maintenance_session_update_authority_observer() {
        use quazal::rmc::basic::ToStream;
        use quazal::rmc::{Request,Error};
        use quazal::rmc::types::{QList,Property};
        use crate::protocols::game_session_service::game_session_protocol::UpdateSessionRequest;
        use crate::protocols::game_session_service::types::{GameSessionKey,GameSessionUpdate};
        let mut violations=0;
        for private in [false,true] {
            for case in ["owner","owner_noop","member","outsider","cross_mode","anonymous","former_owner","migrated_owner","destroyed","missing"] {
                let s=Arc::new(create_test_storage("session-update-authority"));insert_users(&s,4);
                for id in 1..=4 {maintenance_begin_run(&s,id,&format!("{id:032x}"),if id==4 {2}else{1});}
                let original=if private {"113 => 0;3 => 0;4 => 12;103 => 0;112 => 5"}else{"113 => 0;3 => 12;4 => 0;103 => 4008726265;112 => 6"}.to_string();
                    assert_eq!(is_private_room_attributes(&original),private);
                let room=s.create_game_session(1,1,original.clone()).unwrap();
                assert!(s.join_game_session_unbounded(2,1,room).unwrap());
                if case=="former_owner" || case=="migrated_owner" {assert!(s.migrate_game_session_host(2,1,room).unwrap());}
                if case=="destroyed" {run(sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?").bind(room).execute(&s.pool)).unwrap().unwrap();}
                let caller=match case {"member"|"migrated_owner"=>2,"outsider"=>3,"cross_mode"=>4,_=>1};
                let mut ci=maintenance_native_client(&s,caller);if case=="anonymous" {ci.user_id=None;}
                let mut input_attributes=original.split(';').map(|entry| {
                    let (id,value)=entry.split_once("=>").unwrap();
                    Property{id:id.trim().parse().unwrap(),value:value.trim().parse().unwrap()}
                }).collect::<Vec<_>>();
                if case!="owner_noop" {
                    input_attributes.iter_mut().find(|item| item.id==if private {4}else{3}).unwrap().value=6;
                }
                let request=UpdateSessionRequest{game_session_update:GameSessionUpdate{session_key:GameSessionKey{type_id:1,session_id:if case=="missing" {u32::MAX}else{room}},attributes:QList(input_attributes)}};
                let protocol=crate::game_session::new_protocol::<()>(s.clone());
                let logger=Logger::root(slog::Discard,slog::o!());let context=quazal::Context::splinter_cell_blacklist();
                let registry=quazal::prudp::ClientRegistry::<()>::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let result=protocol.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:1,method_id:2,parameters:request.to_bytes()},&registry,&socket);
                let (owner,attributes,mode):(u32,String,i64)=run(sqlx::query_as("SELECT creator_id,attributes,gameplay_mode FROM game_sessions WHERE id=?").bind(room).fetch_one(&s.pool)).unwrap().unwrap();
                let allowed=result.is_ok();let denied=matches!(result,Err(Error::AccessDenied));let changed=attributes!=original;
                let expected=matches!(case,"owner"|"owner_noop"|"migrated_owner");
                if allowed!=expected || (!expected && changed) {violations+=1;}
                assert_eq!(owner,if matches!(case,"former_owner"|"migrated_owner") {2}else{1});assert_eq!(mode,1);
                assert!(allowed||denied,"Unexpected error in authority fixture");
                if expected {assert!(allowed);assert_eq!(changed,case!="owner_noop");}
                if matches!(case,"cross_mode"|"anonymous"|"missing") {assert!(denied);assert!(!changed);}
                println!("SESSION_UPDATE private={private} case={case} allowed={allowed} denied={denied} attributes_changed={changed} host_preserved=true mode_preserved=true");
            }
        }
        println!("SESSION_UPDATE_SUMMARY cases=20 authority_violations={violations} actual_protected_handler=true live_game=false");
        assert_eq!(violations,0,"Only the current host of an active room may update its attributes");
    }

    #[test]
    fn maintenance_session_update_rechecks_after_queued_writer() {
        use quazal::rmc::basic::ToStream;
        use quazal::rmc::{Request,Error};
        use quazal::rmc::types::{QList,Property};
        use crate::protocols::game_session_service::game_session_protocol::UpdateSessionRequest;
        use crate::protocols::game_session_service::types::{GameSessionKey,GameSessionUpdate};
        for destroy in [false,true] {
            let s=Arc::new(create_test_storage("session-update-queued-writer"));insert_users(&s,2);
            for id in 1..=2 {maintenance_begin_run(&s,id,&format!("{id:032x}"),1);}
            let original="113 => 0;3 => 4;4 => 0";
            let room=s.create_game_session(1,1,original.into()).unwrap();
            assert!(s.join_game_session_unbounded(2,1,room).unwrap());
            let mut ci=maintenance_native_client(&s,1);
            // Hold an actual SQLite writer with a pending ownership/liveness change.
            // This isolates the commit race;full native migration was checked separately.
            let mut tx=run(s.begin_write()).unwrap().unwrap();
            run(async {
                let sql=if destroy {"UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?"} else {"UPDATE game_sessions SET creator_id=2 WHERE id=?"};
                sqlx::query(sql).bind(room).execute(&mut *tx).await
            }).unwrap().unwrap();
            let worker_storage=s.clone();
            let (started_tx,started_rx)=std::sync::mpsc::channel();let (done_tx,done_rx)=std::sync::mpsc::channel();
            let worker=std::thread::spawn(move || {
                let protocol=crate::game_session::new_protocol::<()>(worker_storage);
                let logger=Logger::root(slog::Discard,slog::o!());let context=quazal::Context::splinter_cell_blacklist();
                let registry=quazal::prudp::ClientRegistry::<()>::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let request=UpdateSessionRequest{game_session_update:GameSessionUpdate{session_key:GameSessionKey{type_id:1,session_id:room},attributes:QList(vec![Property{id:113,value:0},Property{id:3,value:6},Property{id:4,value:0}])}};
                started_tx.send(()).unwrap();
                let response=protocol.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:1,method_id:2,parameters:request.to_bytes()},&registry,&socket);
                done_tx.send((response.is_ok(),matches!(response,Err(Error::AccessDenied)))).unwrap();
            });
            started_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            let blocked=matches!(done_rx.recv_timeout(std::time::Duration::from_millis(150)),Err(std::sync::mpsc::RecvTimeoutError::Timeout));
            run(tx.commit()).unwrap().unwrap();
            let (allowed,denied)=done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();worker.join().unwrap();
            let (owner,attributes,dead):(u32,String,bool)=run(sqlx::query_as("SELECT creator_id,attributes,destroyed_at IS NOT NULL FROM game_sessions WHERE id=?").bind(room).fetch_one(&s.pool)).unwrap().unwrap();
            println!("SESSION_UPDATE_QUEUED destroy={destroy} blocked_before_commit={blocked} allowed={allowed} denied={denied} attributes_preserved={}",attributes==original);
            assert!(blocked);assert_eq!(owner,if destroy {1}else{2});assert_eq!(dead,destroy);
            assert!(denied);assert!(!allowed);assert_eq!(attributes,original);
        }
    }

    fn maintenance_add_protocol(s: Arc<Storage>, caller: u32, room: u32, private_list: bool) -> bool {
        use quazal::rmc::basic::ToStream;
        use quazal::rmc::Request;
        use quazal::rmc::types::QList;
        use crate::protocols::game_session_service::game_session_protocol::AddParticipantsRequest;
        use crate::protocols::game_session_service::types::GameSessionKey;
        let mut ci=maintenance_native_client(&s,caller);
        let protocol=crate::game_session::new_protocol::<()>(s);
        let logger=Logger::root(slog::Discard,slog::o!());let context=quazal::Context::splinter_cell_blacklist();
        let registry=quazal::prudp::ClientRegistry::<()>::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let req=AddParticipantsRequest{game_session_key:GameSessionKey{type_id:1,session_id:room},
            private_participant_ids:QList(if private_list {vec![5]}else{vec![]}),
            public_participant_ids:QList(if private_list {vec![]}else{vec![5]})};
        protocol.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:1,method_id:8,parameters:req.to_bytes()},&registry,&socket).is_ok()
    }

    #[test]
    fn maintenance_add_authority_matrix() {
        let mut violations=0;
        for private_room in [false,true] {
            for private_list in [false,true] {
                for invite_state in ["none","accepted","expired","consumed"] {
                    for caller in [1,2,3,5] {
                        let s=Arc::new(create_test_storage("add-authority-matrix"));insert_users(&s,5);
                        for id in 1..=5 {maintenance_begin_run(&s,id,&format!("{id:032x}"),1);}
                        s.register_urls(1,vec!["prudp:/address=127.0.0.1;port=13000;sid=1;type=2".into()]).unwrap();
                        let attributes=if private_room {"113 => 0;3 => 0;4 => 12;103 => 0;112 => 5"}else{"113 => 0;3 => 12;4 => 0;103 => 4008726265;112 => 6"};
                        assert_eq!(is_private_room_attributes(attributes),private_room);
                        let room=s.create_game_session(1,1,attributes.into()).unwrap();assert!(s.join_game_session_unbounded(2,1,room).unwrap());
                        if invite_state!="none" {
                            let id=run(s.add_invite_async(2,5,INVITE_KIND_PRIVATE_ROOM,1,room)).unwrap().unwrap().unwrap();
                            assert!(run(s.accept_invite_async(5,id)).unwrap().unwrap().is_some());
                            if invite_state!="accepted" {
                                let sql=if invite_state=="expired" {"UPDATE invites SET expires_at=datetime('now','-1 minute') WHERE id=?"}else{"UPDATE invites SET consumed_at=CURRENT_TIMESTAMP WHERE id=?"};
                                run(sqlx::query(sql).bind(id).execute(&s.pool)).unwrap().unwrap();
                            }
                        }
                        let snapshot=|| run(async {
                            let state:(u32,String,bool,i64,i64)=sqlx::query_as("SELECT creator_id,attributes,destroyed_at IS NOT NULL,gameplay_mode,host_leave_pending FROM game_sessions WHERE id=?").bind(room).fetch_one(&s.pool).await?;
                            let roster:Vec<u32>=sqlx::query_scalar("SELECT user_id FROM participants WHERE game_id=? ORDER BY user_id").bind(room).fetch_all(&s.pool).await?;
                            let invites:Vec<(i64,Option<String>,Option<String>,Option<String>)>=sqlx::query_as("SELECT id,accepted_at,consumed_at,expires_at FROM invites ORDER BY id").fetch_all(&s.pool).await?;
                            let parties:Vec<(u32,i64)>=sqlx::query_as("SELECT user_id,party_id FROM party_members ORDER BY user_id").fetch_all(&s.pool).await?;
                            Ok::<_,sqlx::Error>((state,roster,invites,parties))
                        }).unwrap().unwrap();
                        let before=snapshot();let ok=maintenance_add_protocol(s.clone(),caller,room,private_list);let after=snapshot();
                        assert!(ok,"Native non-owner report retains idempotent success");
                        let admitted=after.1.contains(&5);let unchanged=before==after;
                        let valid=if caller==1 {admitted&&before.0.0==after.0.0&&before.0.3==after.0.3}else{unchanged};
                        if !valid {violations+=1;}
                        println!("ADD_AUTH private_room={private_room} private_list={private_list} invite={invite_state} caller={caller} response_ok={ok} admitted={admitted} unchanged={unchanged} valid={valid}");
                    }
                }
            }
        }
        println!("ADD_AUTH_SUMMARY cases=64 violations={violations} actual_private_classifier=true");
        assert_eq!(violations,0,"Only the current room owner may persist AddParticipants admission; guests still use JoinSession");
    }

    #[test]
    fn maintenance_add_authority_queued_writer() {
        let mut violations=0;
        for destroy in [false,true] {
            let s=Arc::new(create_test_storage("add-authority-queued"));insert_users(&s,5);
            for id in 1..=5 {maintenance_begin_run(&s,id,&format!("{id:032x}"),1);}
            let room=s.create_game_session(1,1,"113 => 0;3 => 12;4 => 0".into()).unwrap();assert!(s.join_game_session_unbounded(2,1,room).unwrap());
            let mut tx=run(s.begin_write()).unwrap().unwrap();
            let sql=if destroy {"UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?"}else{"UPDATE game_sessions SET creator_id=2 WHERE id=?"};
            run(sqlx::query(sql).bind(room).execute(&mut *tx)).unwrap().unwrap();
            let worker_s=s.clone();let (started_tx,started_rx)=std::sync::mpsc::channel();let (done_tx,done_rx)=std::sync::mpsc::channel();
            let worker=std::thread::spawn(move || {started_tx.send(()).unwrap();done_tx.send(maintenance_add_protocol(worker_s,1,room,false)).unwrap();});
            started_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            let blocked=matches!(done_rx.recv_timeout(std::time::Duration::from_millis(150)),Err(std::sync::mpsc::RecvTimeoutError::Timeout));
            run(tx.commit()).unwrap().unwrap();let ok=done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();worker.join().unwrap();
            let roster:Vec<u32>=run(sqlx::query_scalar("SELECT user_id FROM participants WHERE game_id=? ORDER BY user_id").bind(room).fetch_all(&s.pool)).unwrap().unwrap();
            let unchanged=roster==vec![1,2];if !unchanged {violations+=1;}
            println!("ADD_AUTH_QUEUED destroy={destroy} blocked={blocked} response_ok={ok} roster_preserved={unchanged}");assert!(blocked&&ok);
            if !destroy {assert!(maintenance_add_protocol(s.clone(),2,room,false));assert!(s.game_session_diagnostics(1,room).unwrap().unwrap().2.contains(&5));}
        }
        println!("ADD_AUTH_QUEUED_SUMMARY cases=2 violations={violations}");assert_eq!(violations,0);
    }

    #[test]
    fn maintenance_delete_complete_state_observer() {
        use quazal::rmc::basic::ToStream;
        use quazal::rmc::{Request,Error};
        use crate::protocols::game_session_service::game_session_protocol::DeleteSessionRequest;
        use crate::protocols::game_session_service::types::GameSessionKey;
        let mut unexpected_marker_clears=0;
        for private in [false,true] {
            for role in ["host","member","outsider","cross_mode","anonymous"] {
                let s=Arc::new(create_test_storage("delete-complete-state"));insert_users(&s,5);
                for id in 1..=5 {maintenance_begin_run(&s,id,&format!("{id:032x}"),if id==4 {2}else{1});}
                let attributes=if private {"113 => 0;3 => 0;4 => 12;103 => 0;112 => 5"}else{"113 => 0;3 => 12;4 => 0;103 => 4008726265;112 => 6"};
                assert_eq!(is_private_room_attributes(attributes),private);
                let room=s.create_game_session(1,1,attributes.into()).unwrap();assert!(s.join_game_session_unbounded(2,1,room).unwrap());
                // Represents a recent native departure, not a live player capture.
                s.record_game_room_departure(1,room,5);
                let caller=match role {"member"=>2,"outsider"=>3,"cross_mode"=>4,_=>1};
                let mut ci=maintenance_native_client(&s,caller);if role=="anonymous" {ci.user_id=None;}
                let snapshot=|| {
                    let persistent=run(async {
                        let state:(u32,String,bool,i64,i64)=sqlx::query_as("SELECT creator_id,attributes,destroyed_at IS NOT NULL,gameplay_mode,host_leave_pending FROM game_sessions WHERE id=?").bind(room).fetch_one(&s.pool).await?;
                        let members:Vec<u32>=sqlx::query_scalar("SELECT user_id FROM participants WHERE game_id=? ORDER BY user_id").bind(room).fetch_all(&s.pool).await?;
                        Ok::<_,sqlx::Error>((state,members))
                    }).unwrap().unwrap();
                    let mut markers=s.recent_game_room_departures(1,room);markers.sort_unstable();(persistent,markers)
                };
                let before=snapshot();let protocol=crate::game_session::new_protocol::<()>(s.clone());
                let logger=Logger::root(slog::Discard,slog::o!());let context=quazal::Context::splinter_cell_blacklist();
                let registry=quazal::prudp::ClientRegistry::<()>::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let req=DeleteSessionRequest{game_session_key:GameSessionKey{type_id:1,session_id:room}};
                let response=protocol.handle(&logger,&context,&mut ci,&Request{protocol_id:42,call_id:1,method_id:3,parameters:req.to_bytes()},&registry,&socket);
                let after=snapshot();let db_changed=before.0!=after.0;let markers_changed=before.1!=after.1;
                if role!="host" && markers_changed {unexpected_marker_clears+=1;}
                println!("DELETE_STATE private={private} role={role} response_ok={} denied={} db_changed={db_changed} markers_changed={markers_changed} host_leave_pending={} markers_after={:?}",response.is_ok(),matches!(response,Err(Error::AccessDenied)),(after.0).0.4,after.1);
                if matches!(role,"cross_mode"|"anonymous") {assert_eq!(before,after);}
            }
        }
        println!("DELETE_STATE_SUMMARY cases=10 unexpected_marker_clears={unexpected_marker_clears} actual_private_classifier=true");
        assert_eq!(unexpected_marker_clears,0);
    }

    #[test]
    fn maintenance_delete_preserves_correlated_private_restore() {
        let storage=create_test_storage("delete-noop-preserves-private-restore");insert_users(&storage,3);
        for id in 1..=3 {storage.register_urls(id,vec![format!("prudp:/address=10.66.0.{id};port=13000;sid={id};type=2")]).unwrap();}
        storage.create_game_session(1,1,"113 => 1;3 => 8;4 => 0".into()).unwrap();
        let successor=storage.create_game_session(3,1,"113 => 1;3 => 8;4 => 0".into()).unwrap();
        let room=storage.create_game_session(1,1,"113 => 0;3 => 0;4 => 12;103 => 0;112 => 5".into()).unwrap();
        storage.add_participants(1,1,room,vec![2,3],Vec::new()).unwrap();
        storage.leave_game_session(2,1,room).unwrap();assert_eq!(storage.recent_game_room_departures(1,room),vec![2]);
        // A member's no-op deletion must not erase the actual recent follower leave.
        assert_eq!(storage.delete_game_session(3,1,room).unwrap(),DeleteGameSessionOutcome::NotFound);
        let marker_preserved=storage.recent_game_room_departures(1,room)==vec![2];
        assert_eq!(storage.delete_game_session(1,1,room).unwrap(),DeleteGameSessionOutcome::MigrationPending{remaining_participants:1});
        let (leader,roster)=run(storage.find_party_roster_async(3)).unwrap().unwrap().unwrap();
        let ids=roster.into_iter().map(|x| x.id).collect::<Vec<_>>();let cohort_preserved=leader==3 && ids==vec![3,2];
        storage.leave_game_session(3,1,room).unwrap();
        let leader_restore=run(storage.take_invite_async(3)).unwrap().unwrap();
        let leader_restored=if let Some(leader_restore)=leader_restore {
            assert_eq!(leader_restore.kind,INVITE_KIND_LOBBY_RESTORE);assert_eq!(leader_restore.session_id,successor);
            assert!(run(storage.accept_invite_async(3,leader_restore.id)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(3,1,successor).unwrap());true
        } else {false};
        println!("DELETE_RESTORE_LEADER available={leader_restored}");
        let follower_restore=run(storage.take_invite_async(2)).unwrap().unwrap();
        let follower_target=follower_restore.as_ref().map(|invite| invite.session_id);let restored=follower_target==Some(successor);
        println!("DELETE_RESTORE marker_preserved={marker_preserved} cohort_preserved={cohort_preserved} follower_restored={restored} leader={leader} roster={ids:?}");
        assert!(marker_preserved && cohort_preserved && restored);
    }

    #[test]
    fn maintenance_search_slot_query_observation() {
        use quazal::rmc::basic::{FromStream, ToStream};
        use quazal::rmc::{Request, Error};
        use quazal::rmc::types::{QList, Property};
        use crate::protocols::game_session_ex_service::game_session_ex_protocol::{SearchSessionsRequest, SearchSessionsResponse};
        use crate::protocols::game_session_service::types::GameSessionQuery;
        let mut different_positive_missing = 0;
        let mut quick_cases = 0;
        let mut controls = 0;
        for mode in [1, 2] {
            for host_slots in [4, 8, 12] {
                let storage = Arc::new(create_test_storage("search-slot-query"));
                insert_users(&storage, 4);
                for id in 1..=4 {
                    maintenance_begin_run(&storage, id, &format!("{id:032x}"), if id == 4 { 3 - mode } else { mode });
                    storage.register_urls(id, vec![format!("prudp:/address=127.0.0.{id};port=13000;sid={id};type=2")]).unwrap();
                }
                let public = storage.create_game_session(1, 1, format!("113 => 0;3 => {host_slots};4 => 0;103 => 2165463540;105 => 0;112 => 8")).unwrap();
                let private = storage.create_game_session(3, 1, format!("113 => 0;3 => 0;4 => {host_slots};103 => 2165463540;105 => 0;112 => 8")).unwrap();
                let protocol = crate::game_session_ex::new_protocol::<()>(storage.clone());
                let logger = Logger::root(slog::Discard, slog::o!());
                let context = quazal::Context::splinter_cell_blacklist();
                let registry = quazal::prudp::ClientRegistry::<()>::default();
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let query = |client: &mut quazal::ClientInfo<()>, query_id, public_slots: Option<u32>| {
                    let mut properties = vec![Property { id: 103, value: 2_165_463_540 }, Property { id: 113, value: 0 }, Property { id: 105, value: 0 }, Property { id: 112, value: 2 }, Property { id: 4, value: 0 }];
                    if let Some(value) = public_slots { properties.push(Property { id: 3, value }); }
                    let request = SearchSessionsRequest { game_session_query: GameSessionQuery { type_id: 1, query_id, parameters: QList(properties) } };
                    protocol.handle(&logger, &context, client, &Request { protocol_id: 123, call_id: 1, method_id: 1, parameters: request.to_bytes() }, &registry, &socket)
                        .map(|bytes| { println!("SEARCH_SLOT_WIRE mode={mode} host_slots={host_slots} query={query_id} requested={public_slots:?} bytes={bytes:02x?}"); SearchSessionsResponse::from_bytes(&bytes).unwrap() })
                };
                let ids = |response: SearchSessionsResponse| response.search_results.0.into_iter().map(|row| {
                    assert!(!row.game_session_search_result.host_urls.is_empty());
                    assert!(row.participants.0.iter().any(|peer| peer.pid == row.game_session_search_result.host_pid));
                    row.game_session_search_result.session_key.session_id
                }).collect::<Vec<_>>();
                let mut client = maintenance_native_client(&storage, 2);
                for requested in [None, Some(0), Some(4), Some(8), Some(12)] {
                    let found = ids(query(&mut client, 0, requested).unwrap());
                    assert!(!found.contains(&private));
                    assert!(found.iter().all(|id| *id == public));
                    if requested.is_none() || requested == Some(host_slots) { assert_eq!(found, vec![public]); }
                    if requested == Some(0) { assert!(found.is_empty()); }
                    if requested.is_some_and(|value| value > 0 && value != host_slots) && found.is_empty() { different_positive_missing += 1; }
                    quick_cases += 1;
                    println!("SEARCH_SLOTS mode={mode} host_slots={host_slots} requested={requested:?} visible={} private_hidden=true", found.len());
                }
                let mut other_mode = maintenance_native_client(&storage, 4);
                let cross_hidden = match query(&mut other_mode, 0, None) { Ok(response) => ids(response).is_empty(), Err(Error::AccessDenied) => true, Err(e) => panic!("unexpected error:{e:?}") };
                assert!(cross_hidden);
                let invitation = run(storage.add_invite_async(3, 2, INVITE_KIND_PRIVATE_ROOM, 1, private)).unwrap().unwrap().unwrap();
                assert!(run(storage.accept_invite_async(2, invitation)).unwrap().unwrap().is_some());
                assert_eq!(ids(query(&mut client, 8, Some(4)).unwrap()), vec![private]);
                assert!(storage.manual_invitation_search_is_active(2, 1));
                assert_eq!(ids(query(&mut client, 0, None).unwrap()), vec![public]);
                assert!(!storage.manual_invitation_search_is_active(2, 1));
                client.user_id = None;
                assert!(matches!(query(&mut client, 0, None), Err(Error::AccessDenied)));
                controls += 4;
                println!("SEARCH_SLOT_CONTROLS mode={mode} host_slots={host_slots} cross_mode_hidden=true accepted_invite_exact=true quick_after_invite_public=true anonymous_denied=true");
            }
        }
        println!("SEARCH_SLOT_SUMMARY quick_cases={quick_cases} controls={controls} different_positive_missing={different_positive_missing} actual_handler=true stored_fixture=true game_request_capture=false");
    }


    include!("coverage2_join_order_tests.rs");
include!("coverage2_join_order_controls.rs");

    include!("coverage2_join_publication_tests.rs");
