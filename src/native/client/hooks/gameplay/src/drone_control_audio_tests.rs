use super::*;
fn starts(edges: [Option<WorldEdge>; 2]) -> [Option<bool>; 2] {
    edges.map(|edge| edge.map(|edge| edge.start))
}
#[test]
fn native_activation_state_zero_is_recordable_before_control_but_dead_remote_unknown_are_not() {
    let mut body = vec![0u32; 0xc00 / 4];
    let mut owner = vec![0u32; 0x100 / 4];
    let mut node = vec![0u32; 0x80 / 4];
    body[0] = VT;
    body[0x4c / 4] = 1 << 8;
    body[0x5dc / 4] = 600;
    owner[0] = 0x2ad2abc;
    body[0x9fc / 4] = owner.as_ptr() as u32;
    body[0x8c / 4] = owner.as_ptr() as u32;
    body[0x2c0 / 4] = node.as_ptr() as u32;
    node[0x70 / 4] = 7;
    node[0x74 / 4] = 9;
    let p = body.as_ptr() as usize;
    unsafe {
        for state in 0..=2 {
            body[0x910 / 4] = state;
            assert_eq!(identity(p).unwrap().source, [7, 9]);
        }
        body[0x910 / 4] = 3;
        assert!(identity(p).is_none());
        body[0x910 / 4] = 1;
        body[0x4c / 4] = 2 << 8;
        assert!(identity(p).is_none());
        body[0x4c / 4] = 1 << 8;
        body[0x5dc / 4] = 0;
        assert!(identity(p).is_none());
        body[0x5dc / 4] = 600;
        body[0x4b0 / 4] = 0x80000000;
        assert!(identity(p).is_none());
        body[0x4b0 / 4] = 0;
        body[0x8c / 4] = 0;
        assert!(identity(p).is_none());
        body[0x8c / 4] = owner.as_ptr() as u32;
        node[0x70 / 4] = 0;
        node[0x74 / 4] = 0;
        assert!(identity(p).is_none());
    }
}
fn id(source: u32) -> Identity {
    Identity {
        owner: 100,
        source: [source, 2],
    }
}
fn request(event: usize) -> Request {
    Request {
        event,
        slot: 7,
        scalar: 0.25,
        a: 13,
        b: 29,
    }
}
#[test]
fn park_stops_once_and_resume_replays_exact_native_parameters_once() {
    let mut b = Bank::default();
    let r = request(17);
    assert!(b.event(1, id(1), 0, true, r));
    assert!(b.park(1, id(1)));
    assert!(!b.park(1, id(1)));
    assert!(!b.event(1, id(1), 0, true, r));
    assert_eq!(b.resume(1, id(1)), [Some(r), None, None]);
    assert_eq!(b.resume(1, id(1)), [None; 3]);
    assert!(b.event(1, id(1), 0, true, r));
}
#[test]
fn damage_and_out_of_range_while_parked_do_not_restart_operator_noise() {
    let mut b = Bank::default();
    b.event(1, id(1), 0, true, request(10));
    b.park(1, id(1));
    assert!(b.event(1, id(1), 0, false, request(11)));
    assert!(!b.event(1, id(1), 1, true, request(20)));
    assert!(!b.event(1, id(1), 2, true, request(30)));
    assert!(b.event(1, id(1), 2, false, request(31)));
    assert_eq!(b.resume(1, id(1)), [None, Some(request(20)), None]);
}
#[test]
fn other_body_and_new_network_identity_never_inherit_mute_or_replay() {
    let mut b = Bank::default();
    b.event(1, id(1), 0, true, request(10));
    b.park(1, id(1));
    assert!(b.event(2, id(2), 0, true, request(10)));
    assert_eq!(b.resume(1, id(3)), [None; 3]);
    assert!(b.event(1, id(3), 0, true, request(10)));
    assert_eq!(b.resume(1, id(3)), [None; 3]);
}
#[test]
fn validated_park_without_observed_start_stops_and_capacity_is_bounded() {
    let mut b = Bank::default();
    assert!(b.park(1, id(1)));
    assert_eq!(b.resume(1, id(1)), [None; 3]); // no guessed restart
    for body in 1..=40 {
        assert!(b.event(body, id(1), 0, true, request(10)));
    }
    assert_eq!(b.records.len(), 32);
    assert!(!b.park(40, id(1)));
    assert!(b.event(40, id(1), 0, true, request(10)));
}
#[test]
fn missed_early_start_is_remembered_without_double_play_then_reentry_restores_it() {
    let mut b = Bank::default();
    let normal = request(10);
    b.remember_native_start(1, id(1), normal);
    assert_eq!(b.resume(1, id(1)), [None; 3]); // first native entry already played
    assert!(b.park(1, id(1)));
    assert!(!b.park(1, id(1)));
    b.remember_native_start(1, id(1), normal);
    assert_eq!(b.resume(1, id(1)), [Some(normal), None, None]);
    assert_eq!(b.resume(1, id(1)), [None; 3]);
}
#[test]
fn control_boundary_does_not_overwrite_damage_or_native_stop() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.event(1, id(1), 0, false, request(11));
    b.event(1, id(1), 1, true, request(20));
    b.park(1, id(1));
    b.remember_native_start(1, id(1), request(10));
    assert_eq!(b.resume(1, id(1)), [None, Some(request(20)), None]);
    b.event(1, id(1), 1, false, request(21));
    b.park(1, id(1));
    b.remember_native_start(1, id(1), request(10));
    assert_eq!(b.resume(1, id(1)), [None; 3]);
}
#[test]
fn new_identity_discards_previous_control_history() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    b.remember_native_start(1, id(2), request(40));
    assert_eq!(b.resume(1, id(2)), [None; 3]);
    assert!(b.park(1, id(2)));
    assert_eq!(b.resume(1, id(2)), [Some(request(40)), None, None]);
}
#[test]
fn native_stop_always_passes_and_prevents_stale_restart() {
    let mut b = Bank::default();
    for i in 0..3 {
        assert!(b.event(1, id(1), i, true, request(i + 1)));
    }
    b.park(1, id(1));
    for i in 0..3 {
        assert!(b.event(1, id(1), i, false, request(i + 10)));
    }
    assert_eq!(b.resume(1, id(1)), [None; 3]);
}
#[test]
fn retail_loop_offsets_exclude_world_one_shots_and_native_3d_audio() {
    let used: Vec<_> = FIELDS.iter().flat_map(|&(a, b, _)| [a, b]).collect();
    for keep in [
        0xb6c, 0xb78, 0xb7c, 0xb80, 0xb84, 0xb90, 0xb94, 0xb98, 0xb9c, 0xba0, 0xba4, 0xba8, 0xbac,
        0xbb0,
    ] {
        assert!(!used.contains(&keep));
    }
}
#[test]
fn same_input_audio_transaction() {
    let mut b = Bank::default();
    let mut audible = [false; 3];
    let r = request(10);
    if b.event(1, id(1), 0, true, r) {
        audible[0] = true;
    }
    if b.park(1, id(1)) {
        audible = [false; 3];
    }
    // A damage event may legitimately start its own loop AFTER voluntary exit.
    if b.event(1, id(1), 1, true, request(20)) {
        audible[1] = true;
    }
    assert_eq!(audible, [false; 3]);
    let parked = audible.iter().filter(|v| **v).count();
    for (i, r) in b.resume(1, id(1)).iter().enumerate() {
        if r.is_some() {
            audible[i] = true;
        }
    }
    println!(
        "C20_AUDIO parked_operator_loops={} reentry_restored={} world3d_untouched=true",
        parked,
        audible[0] && audible[1]
    );
}

#[test]
fn owner_park_switches_to_world_once_and_reentry_stops_before_player_resume() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    assert_eq!(starts(b.world_edges(1, id(1), true)), [None; 2]);
    b.park(1, id(1));
    assert_eq!(starts(b.world_edges(1, id(1), true)), [Some(true), None]);
    assert_eq!(starts(b.world_edges(1, id(1), true)), [None; 2]);
    assert_eq!(starts(b.world_edges(1, id(1), false)), [Some(false), None]);
    assert_eq!(b.resume(1, id(1)), [Some(request(10)), None, None]);
    assert_eq!(starts(b.world_edges(1, id(1), true)), [None; 2]);
}
#[test]
fn parked_damage_replaces_normal_world_and_never_starts_range_warning_in_world() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    b.world_edges(1, id(1), true);
    b.event(1, id(1), 0, false, request(11));
    assert_eq!(starts(b.world_edges(1, id(1), true)), [Some(false), None]);
    assert!(!b.event(1, id(1), 1, true, request(20)));
    assert_eq!(starts(b.world_edges(1, id(1), true)), [None, Some(true)]);
    assert!(!b.event(1, id(1), 2, true, request(30)));
    assert_eq!(starts(b.world_edges(1, id(1), true)), [None; 2]);
}
#[test]
fn forced_terminal_world_cleanup_is_once_and_does_not_restart_player() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    b.world_edges(1, id(1), true);
    assert_eq!(starts(b.world_edges(1, id(1), false)), [Some(false), None]);
    assert_eq!(starts(b.world_edges(1, id(1), false)), [None; 2]);
    b.records.remove(&1);
    assert_eq!(b.resume(1, id(1)), [None; 3]);
}
#[test]
fn world_ownership_never_leaks_to_other_drone_or_reused_source() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    b.world_edges(1, id(1), true);
    assert_eq!(starts(b.world_edges(2, id(1), false)), [None; 2]);
    assert_eq!(starts(b.world_edges(1, id(2), false)), [None; 2]);
    b.remember_native_start(1, id(2), request(40));
    assert_eq!(starts(b.world_edges(1, id(2), false)), [None; 2]);
}
#[test]
fn unknown_native_loop_history_does_not_invent_world_sound() {
    let mut b = Bank::default();
    b.park(1, id(1));
    assert_eq!(starts(b.world_edges(1, id(1), true)), [None; 2]);
}
#[test]
fn world_events_are_only_two_native_body_loops_and_explicit_spatial_channel() {
    assert_eq!(
        WORLD_FIELDS,
        [(0xb94, 0xb98, "Loop"), (0xbac, 0xbb0, "DamagedLoop")]
    );
    assert_eq!(WORLD_SLOT, 1);
    for (a, b, _) in WORLD_FIELDS {
        assert!(!FIELDS.iter().any(|&(x, y, _)| x == a || y == b));
    }
}

#[test]
fn rejected_far_world_start_retries_at_two_hz_and_never_duplicates_success() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    let edge = b.world_edges(1, id(1), true)[0].clone().unwrap();
    assert!(edge.start);
    b.world_rejected(1, id(1), 0, &edge);
    assert!(b.world_retry_due(1, id(1), 10.));
    let edge = b.world_edges(1, id(1), true)[0].clone().unwrap();
    assert!(edge.start);
    b.world_rejected(1, id(1), 0, &edge);
    assert!(!b.world_retry_due(1, id(1), 10.2));
    assert!(!b.world_retry_due(1, id(1), f32::NAN));
    assert!(b.world_retry_due(1, id(1), 10.5));
    let edge = b.world_edges(1, id(1), true)[0].clone().unwrap();
    assert!(edge.start);
    assert!(!b.world_retry_due(1, id(1), 11.));
}
#[test]
fn stopped_record_and_identity_mismatch_do_not_schedule_world_retry() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    assert!(!b.world_retry_due(1, id(2), 1.));
    b.world_edges(1, id(1), true);
    b.world_edges(1, id(1), false);
    b.resume(1, id(1));
    assert!(!b.world_retry_due(1, id(1), 2.));
}

#[test]
fn stale_submission_cannot_undo_recreated_or_newer_world_start() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    let old = b.world_edges(1, id(1), true)[0].clone().unwrap();
    b.world_edges(1, id(1), false);
    let new = b.world_edges(1, id(1), true)[0].clone().unwrap();
    b.world_rejected(1, id(1), 0, &old);
    assert!(b.world_current(1, id(1), 0, &new));
    b.records.clear();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    let recreated = b.world_edges(1, id(1), true)[0].clone().unwrap();
    b.world_rejected(1, id(1), 0, &new);
    assert!(b.world_current(1, id(1), 0, &recreated));
    assert!(b.records[&1].world[0]);
}
#[test]
fn pending_start_loses_authority_on_resume_cancel_or_identity_change() {
    for change in 0..3 {
        let mut b = Bank::default();
        b.remember_native_start(1, id(1), request(10));
        b.park(1, id(1));
        let edge = b.world_edges(1, id(1), true)[0].clone().unwrap();
        match change {
            0 => { b.resume(1, id(1)); }
            1 => { b.records.clear(); }
            _ => { b.remember_native_start(1, id(2), request(20)); }
        }
        assert!(!b.world_current(1, id(1), 0, &edge));
        b.world_rejected(1, id(1), 0, &edge);
    }
}
#[test]
fn retry_clock_rewind_recovers_and_remains_half_second_limited() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    assert!(b.world_retry_due(1, id(1), 10.));
    assert!(!b.world_retry_due(1, id(1), 10.2));
    assert!(!b.world_retry_due(1, id(1), f32::NAN));
    assert!(b.world_retry_due(1, id(1), 1.));
    assert!(!b.world_retry_due(1, id(1), 1.2));
    assert!(b.world_retry_due(1, id(1), 1.5));
    assert!(!b.world_retry_due(1, id(1), f32::INFINITY));
}
#[test]
fn unchanged_world_edges_keep_the_same_ticket_without_new_allocation() {
    let mut b = Bank::default();
    b.remember_native_start(1, id(1), request(10));
    b.park(1, id(1));
    let edge = b.world_edges(1, id(1), true)[0].clone().unwrap();
    for _ in 0..1000 {
        assert!(b.world_edges(1, id(1), true).iter().all(Option::is_none));
        assert!(std::sync::Arc::ptr_eq(b.records[&1].world_tokens[0].as_ref().unwrap(), &edge.token));
    }
}
