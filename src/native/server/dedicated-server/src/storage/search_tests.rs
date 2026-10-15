// Isolated WAL databases only. Pause the real discovery path after its first read;
// another thread commits actual public update/leave/join/URL operations.
impl Storage {
    pub(super) fn discovery_checkpoint(&self) {
        let callback = self.discovery_checkpoint.lock().unwrap().take();
        if let Some(callback) = callback { callback(); }
    }
}

fn search_snapshot_case(lane: &str) {
    let storage = Arc::new(create_test_storage("discovery-snapshot"));
    insert_users(&storage, 4);
    let before = "113 => 1;3 => 8;4 => 0;101 => 1";
    let after = "113 => 1;3 => 8;4 => 0;101 => 2";
    let old_url = "prudp:/address=127.0.0.1;port=13000;sid=1;type=2";
    let new_url = "prudp:/address=127.0.0.1;port=14000;sid=2;type=2";
    storage.register_urls(1, vec![old_url.into()]).unwrap();
    let room = storage.create_game_session(1, 42, before.into()).unwrap();
    assert!(storage.join_game_session_unbounded(2, 42, room).unwrap());
    if lane == "invitation" {
        let invite = run(storage.add_invite_async(1, 4, INVITE_KIND_LOBBY_PARTY, 42, room)).unwrap().unwrap().unwrap();
        assert!(run(storage.accept_invite_async(4, invite)).unwrap().unwrap().is_some());
    }
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer_storage = storage.clone();
    let writer = std::thread::spawn(move || {
        start_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // No committed state has old attributes and new members/endpoints.
        assert!(writer_storage.update_game_session(1, 42, room, after.into()).unwrap());
        writer_storage.leave_game_session(2, 42, room).unwrap();
        assert!(writer_storage.join_game_session_unbounded(3, 42, room).unwrap());
        writer_storage.register_urls(1, vec![new_url.into()]).unwrap();
        done_tx.send(()).unwrap();
    });
    *storage.discovery_checkpoint.lock().unwrap() = Some(Box::new(move || {
        start_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    let sessions = match lane {
        "quick" => storage.search_sessions(42, Some(4)).unwrap(),
        "participants" => storage.search_sessions_with_participants(42, &[2]).unwrap(),
        "list" => run(storage.list_game_sessions_async()).unwrap().unwrap(),
        "invitation" => vec![storage.find_accepted_invited_session(4, 42).unwrap().unwrap().session],
        _ => unreachable!(),
    };
    writer.join().unwrap();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    let mut members = session.participants.iter().map(|p| p.user_id).collect::<Vec<_>>();members.sort_unstable();
    let urls = &session.participants.iter().find(|p| p.user_id == 1).unwrap().station_urls;
    println!("SEARCH_SNAPSHOT lane={lane} attributes={} members={members:?} host_urls={urls:?}", session.attributes);
    assert_eq!(session.attributes, before);
    assert_eq!(members, vec![1, 2], "a response must not mix pre-write attributes with post-write members");
    assert_eq!(urls, &vec![old_url.to_string()]);
    // The next request must see the completed writes, not a cached old snapshot.
    let fresh = storage.search_sessions(42, Some(4)).unwrap();
    assert_eq!(fresh[0].attributes, after);
    assert_eq!(fresh[0].participants.iter().map(|p| p.user_id).collect::<std::collections::HashSet<_>>(), [1, 3].into_iter().collect());
    assert_eq!(fresh[0].participants.iter().find(|p| p.user_id == 1).unwrap().station_urls, vec![new_url.to_string()]);
    // A discovered room is not a join authorization or liveness guarantee.
    run(storage.delete_game_session_by_id_async(room)).unwrap().unwrap();
    assert!(!storage.join_game_session_unbounded(4, 42, room).unwrap());
    println!("SEARCH_SNAPSHOT lane={lane} fresh_request_updated=true destroyed_join_rejected=true writer_completed_during_read=true");
}
#[test] fn maintenance_search_snapshot_quick() { search_snapshot_case("quick"); }
#[test] fn maintenance_search_snapshot_participants() { search_snapshot_case("participants"); }
#[test] fn maintenance_search_snapshot_invitation() { search_snapshot_case("invitation"); }
#[test] fn maintenance_search_snapshot_list() { search_snapshot_case("list"); }
