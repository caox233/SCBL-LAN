use super::*;
fn buf(n: usize) -> Vec<u32> {
    vec![0; (n + 3) / 4]
}
struct Fixture {
    info: Vec<u32>,
    world: Vec<u32>,
    wi: Vec<u32>,
    manager: Vec<u32>,
    net: Vec<u32>,
    session: Vec<u32>,
    nodes: Vec<Vec<u32>>,
    peers: Vec<Vec<u32>>,
    pcs: Vec<Vec<u32>>,
    pawns: Vec<Vec<u32>>,
    actors: Vec<Vec<u32>>,
    records: Vec<Vec<u32>>,
    record_array: Vec<u32>,
    local_array: Vec<u32>,
    pcvt: Vec<u32>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            info: buf(0x600),
            world: buf(0x700),
            wi: buf(0x400),
            manager: buf(0x300),
            net: buf(0x100),
            session: buf(0x600),
            nodes: (0..2).map(|_| buf(16)).collect(),
            peers: (0..2).map(|_| buf(16)).collect(),
            pcs: (0..2).map(|_| buf(0x700)).collect(),
            pawns: (0..2).map(|_| buf(0x1c00)).collect(),
            actors: (0..2).map(|_| buf(0x100)).collect(),
            records: (0..2).map(|_| buf(0x60)).collect(),
            record_array: vec![0; 2],
            local_array: vec![0; 2],
            pcvt: buf(0x440),
        };
        f.info[0x144 / 4] = f.world.as_ptr() as u32;
        f.info[0x330 / 4] = 0x10;
        f.info[0x448 / 4] = f.record_array.as_ptr() as u32;
        f.info[0x44c / 4] = 2;
        f.info[0x450 / 4] = 2;
        f.world[0x6d0 / 4] = f.wi.as_ptr() as u32;
        f.world[0x5a0 / 4] = 1f32.to_bits();
        f.wi[0x378 / 4] = f.manager.as_ptr() as u32;
        f.manager[0x2c0 / 4] = f.net.as_ptr() as u32;
        f.net[0] = 0x2af70fc;
        f.net[0x88 / 4] = f.manager.as_ptr() as u32;
        f.net[0x50 / 4] = f.session.as_ptr() as u32;
        f.net[0x70 / 4] = 50;
        f.wi[0x36c / 4] = f.local_array.as_ptr() as u32;
        f.wi[0x370 / 4] = 2;
        f.wi[0x374 / 4] = 2;
        f.pcvt[0x430 / 4] = 0xe5c960;
        for i in 0..2 {
            f.local_array[i] = f.pcs[i].as_ptr() as u32;
            f.pcs[i][0x144 / 4] = f.world.as_ptr() as u32;
            f.pcs[i][0x60 / 4] = 0x60000;
            f.pcs[i][0x4c / 4] = if i == 1 { 1 << 8 } else { 2 << 8 };
        }
        f.session[0x588 / 4] = f.nodes[0].as_ptr() as u32;
        for i in 0..2 {
            f.nodes[i][1] = if i == 0 {
                f.nodes[1].as_ptr() as u32
            } else {
                f.session.as_ptr() as u32 + 0x584
            };
            f.nodes[i][2] = f.peers[i].as_ptr() as u32;
            f.peers[i][0] = 7 + i as u32;
            f.record_array[i] = f.records[i].as_ptr() as u32;
            f.records[i][0x48 / 4] = f.pcs[i].as_ptr() as u32;
            f.records[i][0x2c / 4] = 1 + i as u32;
            f.pcs[i][0] = f.pcvt.as_ptr() as u32;
            f.pcs[i][0x6dc / 4] = 1 + i as u32;
            f.pcs[i][0x520 / 4] = f.pawns[i].as_ptr() as u32;
            f.pawns[i][0] = if i == 0 { 0x2ad2abc } else { 0x2ad3234 };
            f.pawns[i][0x374 / 4] = if i == 0 { 0x80000 } else { 0x40000 };
            f.pawns[i][0x144 / 4] = f.world.as_ptr() as u32;
            f.pawns[i][0x20 / 4] = 100 + i as u32;
            f.pawns[i][0x2c0 / 4] = f.actors[i].as_ptr() as u32;
            f.actors[i][0] = 0x2b1cd1c;
            f.actors[i][0x88 / 4] = f.pawns[i].as_ptr() as u32;
            f.actors[i][0x50 / 4] = f.session.as_ptr() as u32;
            f.actors[i][0x18 / 4] = 7 + i as u32;
            f.actors[i][0x70 / 4] = 10 + 10 * i as u32;
        }
        f
    }
    unsafe fn frame(&self) -> Option<Frame> {
        from_info(self.info.as_ptr() as usize)
    }
}
#[test]
fn native_current_match_maps_object_identity_to_separate_envelope_owner() {
    let f = Fixture::new();
    let x = unsafe { f.frame().unwrap() };
    assert_eq!(x.context.local, 20);
    assert_eq!(x.context.match_key, 50);
    assert_eq!(x.context.source(10, 7).unwrap().actor, 10);
    assert!(x.context.source(10, 10).is_none());
}
#[test]
fn native_peer_mapping_rejects_loops_missing_owners_duplicate_owners_and_wrong_session() {
    for kind in 0..6 {
        let mut f = Fixture::new();
        match kind {
            0 => f.nodes[1][1] = f.nodes[0].as_ptr() as u32,
            1 => f.actors[0][0x18 / 4] = 99,
            2 => f.actors[0][0x18 / 4] = 8,
            3 => f.actors[0][0x50 / 4] = f.world.as_ptr() as u32,
            4 => f.actors[0][0x88 / 4] = f.pawns[1].as_ptr() as u32,
            _ => f.info[0x330 / 4] = 0,
        }
        assert!(unsafe { f.frame() }.is_none(), "{kind}");
    }
}
#[test]
fn native_death_and_local_respawn_do_not_reuse_old_scope() {
    let mut f = Fixture::new();
    let before = unsafe { f.frame().unwrap() };
    f.pawns[0][0x4b0 / 4] = 0x80000000;
    let after = unsafe { f.frame().unwrap() };
    assert!(after.context.source(10, 7).is_none());
    f.pawns[1][0x20 / 4] += 1;
    assert_ne!(
        unsafe { f.frame().unwrap() }.context.scope,
        before.context.scope
    );
    f.pawns[1][0xc50 / 4] = 1;
    assert!(unsafe { f.frame() }.is_none());
}
#[test]
fn native_text_read_is_bounded_and_never_writes_terminator() {
    let mut msg = buf(0x68);
    let mut b = buf(0xc + 16);
    b[1] = 4;
    b[2] = 4;
    msg[0x40 / 4] = b.as_ptr() as u32;
    unsafe {
        let p = (b.as_mut_ptr() as usize + 0xc) as *mut u16;
        for (i, v) in [65u16, 66, 67, 68, 0xffff].iter().enumerate() {
            p.add(i).write(*v);
        }
        let before = b.clone();
        assert_eq!(text(msg.as_ptr() as usize), Some(vec![65, 66, 67, 68]));
        assert_eq!(b, before);
        b[1] = 145;
        assert!(text(msg.as_ptr() as usize).is_none());
        b[1] = 4;
        b[2] = 3;
        assert!(text(msg.as_ptr() as usize).is_none());
    }
}

#[test]
fn actual_spy_merc_manager_and_two_controller_scene_keep_native_identity_gates() {
    let mut f=Fixture::new();
    let a=unsafe {f.frame().unwrap()};
    f.local_array.swap(0,1);
    let b=unsafe {f.frame().unwrap()};
    assert_eq!(a.context.local,b.context.local);
    assert_eq!(a.context.match_key,b.context.match_key);
    f.net[0]=0x2a06bdc; // base class is not the actual multiplayer manager
    assert!(unsafe {f.frame()}.is_none());
    f.net[0]=0x2af70fc;
    f.pcs[0][0x4c/4]=1<<8; // two actual local controllers remain ambiguous
    assert!(unsafe {f.frame()}.is_none());
}

impl Fixture {
    fn three_players() -> Self {
        let mut f = Self {
            info: buf(0x600),
            world: buf(0x700),
            wi: buf(0x400),
            manager: buf(0x300),
            net: buf(0x100),
            session: buf(0x600),
            nodes: (0..3).map(|_| buf(16)).collect(),
            peers: (0..3).map(|_| buf(16)).collect(),
            pcs: (0..3).map(|_| buf(0x700)).collect(),
            pawns: (0..3).map(|_| buf(0x1c00)).collect(),
            actors: (0..3).map(|_| buf(0x100)).collect(),
            records: (0..3).map(|_| buf(0x60)).collect(),
            record_array: vec![0; 3],
            local_array: vec![0; 3],
            pcvt: buf(0x440),
        };
        f.info[0x144 / 4] = f.world.as_ptr() as u32;
        f.info[0x330 / 4] = 0x10;
        f.info[0x448 / 4] = f.record_array.as_ptr() as u32;
        f.info[0x44c / 4] = 3;
        f.info[0x450 / 4] = 3;
        f.world[0x6d0 / 4] = f.wi.as_ptr() as u32;
        f.world[0x5a0 / 4] = 1f32.to_bits();
        f.wi[0x378 / 4] = f.manager.as_ptr() as u32;
        f.manager[0x2c0 / 4] = f.net.as_ptr() as u32;
        f.net[0] = 0x2af70fc;
        f.net[0x88 / 4] = f.manager.as_ptr() as u32;
        f.net[0x50 / 4] = f.session.as_ptr() as u32;
        f.net[0x70 / 4] = 50;
        f.wi[0x36c / 4] = f.local_array.as_ptr() as u32;
        f.wi[0x370 / 4] = 3;
        f.wi[0x374 / 4] = 3;
        f.pcvt[0x430 / 4] = 0xe5c960;
        for i in 0..3 {
            f.local_array[i] = f.pcs[i].as_ptr() as u32;
            f.pcs[i][0x144 / 4] = f.world.as_ptr() as u32;
            f.pcs[i][0x60 / 4] = 0x60000;
            f.pcs[i][0x4c / 4] = if i == 1 { 1 << 8 } else { 2 << 8 };
        }
        f.session[0x588 / 4] = f.nodes[0].as_ptr() as u32;
        for i in 0..3 {
            f.nodes[i][1] = if i < 2 {
                f.nodes[i + 1].as_ptr() as u32
            } else {
                f.session.as_ptr() as u32 + 0x584
            };
            f.nodes[i][2] = f.peers[i].as_ptr() as u32;
            f.peers[i][0] = 7 + i as u32;
            f.record_array[i] = f.records[i].as_ptr() as u32;
            f.records[i][0x48 / 4] = f.pcs[i].as_ptr() as u32;
            f.records[i][0x2c / 4] = if i == 0 { 1 } else { 2 };
            f.pcs[i][0] = f.pcvt.as_ptr() as u32;
            f.pcs[i][0x6dc / 4] = if i == 0 { 1 } else { 2 };
            f.pcs[i][0x520 / 4] = f.pawns[i].as_ptr() as u32;
            f.pawns[i][0] = if i == 0 { 0x2ad2abc } else { 0x2ad3234 };
            f.pawns[i][0x374 / 4] = if i == 0 { 0x80000 } else { 0x40000 };
            f.pawns[i][0x144 / 4] = f.world.as_ptr() as u32;
            f.pawns[i][0x20 / 4] = 100 + i as u32;
            f.pawns[i][0x2c0 / 4] = f.actors[i].as_ptr() as u32;
            f.actors[i][0] = 0x2b1cd1c;
            f.actors[i][0x88 / 4] = f.pawns[i].as_ptr() as u32;
            f.actors[i][0x50 / 4] = f.session.as_ptr() as u32;
            f.actors[i][0x18 / 4] = 7 + i as u32;
            f.actors[i][0x70 / 4] = 10 + 10 * i as u32;
        }
        f
    }
}

#[test]
fn maintenance_pending_remote_proxy_preserves_other_peers_and_real_receipts() {
    use crate::goggle_feedback::{Receiver, Report};
    use crate::goggle_exposure::{ActualDetections, Source};
    unsafe {
        for detector in [Source::Rfd, Source::Ats, Source::Bms] {
            let mut f = Fixture::three_players();
            f.pawns[2][0x2c0 / 4] = 0;
            let before = f.pawns.clone();
            let frame = f.frame().expect("unrelated pending remote proxy must not discard live local/merc pair");
            assert_eq!(frame.context.people.len(), 2);
            assert!(frame.context.person(30).is_none());
            assert!(frame.context.source(10, 7).is_some());
            let mut receiver = Receiver::default();
            assert_eq!(receiver.detections(&frame.context), ActualDetections::default());
            let report = Report {match_key:50, actor:10, seq:1, detector, targets:vec![20]};
            assert!(!receiver.receive(&frame.context, 9, report.clone()));
            assert!(receiver.receive(&frame.context, 7, report.clone()));
            assert!(!receiver.receive(&frame.context, 7, report));
            assert_ne!(receiver.detections(&frame.context), ActualDetections::default());
            let mut later = frame.context.clone();later.now += 400;
            assert_eq!(receiver.detections(&later), ActualDetections::default());
            assert_eq!(f.pawns, before);
        }
    }
}
#[test]
fn maintenance_pending_proxy_does_not_accept_malformed_or_local_missing_identity() {
    unsafe {
        for kind in 0..8 {
            let mut f = Fixture::three_players();
            match kind {
                0 => f.pawns[1][0x2c0 / 4] = 0,
                1 => f.pawns[2][0x2c0 / 4] = 1,
                2 => f.actors[2][0x50 / 4] = 0,
                3 => f.actors[2][0x18 / 4] = 99,
                4 => f.actors[2][0] = 0,
                5 => f.actors[2][0x88 / 4] = f.pawns[0].as_ptr() as u32,
                6 => f.actors[2][0x70 / 4] = 10,
                _ => f.actors[2][0x18 / 4] = 7,
            }
            assert!(f.frame().is_none(), "malformed identity {kind}");
        }
    }
}
#[test]
fn maintenance_pending_proxy_rejoins_only_from_current_native_mapping() {
    unsafe {
        let mut f = Fixture::three_players();
        let before = f.frame().unwrap();
        f.pawns[2][0x2c0 / 4] = 0;
        let pending = f.frame().unwrap();
        assert_eq!(before.context.scope, pending.context.scope);
        assert!(pending.context.person(30).is_none());
        f.pawns[2][0x2c0 / 4] = f.actors[2].as_ptr() as u32;
        let joined = f.frame().unwrap();
        assert_eq!(joined.context.people.len(), 3);
        assert_eq!(joined.context.person(30).unwrap().peer, 9);
        f.pawns[1][0x2c0 / 4] = 0;
        assert!(f.frame().is_none());
    }
}


fn frame_case(case: usize) -> (Fixture, bool) {
    let mut f = if matches!(case,19..=21) {Fixture::three_players()} else {Fixture::new()};
    match case {
        0 => (), 1 => f.info[0x144/4]=0, 2 => f.world[0x6d0/4]=0,
        3 => f.wi[0x378/4]=0, 4 => f.manager[0x2c0/4]=0,
        5 => f.net[0]=0, 6 => f.net[0x50/4]=0,
        7 => f.nodes[1][1]=f.nodes[0].as_ptr() as u32,
        8 => f.net[0x70/4]=0,
        9 => f.pcs[0][0x4c/4]=1<<8,
        10 => f.info[0x330/4]=0,
        11 => f.pawns[1][0x2c0/4]=0,
        12 => f.actors[0][0]=0,
        13 => f.actors[0][0x18/4]=99,
        14 => f.actors[0][0x70/4]=0,
        15 => f.pawns[1][0xc50/4]=1,
        16 => f.world[0x5a0/4]=f32::NAN.to_bits(),
        17 => f.pawns[1][0x20/4]=0,
        18 => (),19 => f.pawns[2][0x2c0/4]=0,
        20 => f.pawns[2][0xc50/4]=1,21=>(),
        22 => {f.pcs[0][0x4c/4]=1<<8;f.pcs[1][0x4c/4]=2<<8;},
        _ => unreachable!(),
    }
    (f,case!=0)
}

#[test]
fn native_context_rejections_preserve_each_gate_and_valid_controls() {
    let expected=[
        FrameRejection::Information,FrameRejection::World,FrameRejection::WorldInfo,
        FrameRejection::Manager,FrameRejection::Net,FrameRejection::NetIdentity,
        FrameRejection::Session,FrameRejection::PeerList,FrameRejection::MatchKey,
        FrameRejection::LocalController,FrameRejection::Roster,FrameRejection::ActorPointer,
        FrameRejection::ActorBinding,FrameRejection::ActorPeer,FrameRejection::ActorIdentity,
        FrameRejection::LocalIdentity,FrameRejection::Time,FrameRejection::Context,
    ];
    for case in 0..23 {
        let (f,present)=frame_case(case);
        let result=unsafe{inspect_pointer(present.then_some(f.info.as_ptr() as usize))};
        if case<18 {assert_eq!(result.err(),Some(expected[case]),"case={case}");}
        else {assert!(result.is_ok(),"valid control {case}");}
    }
}
#[test]
fn native_context_log_is_bounded_and_non_identifying() {
    use std::sync::{Arc,Mutex};
    struct Sink(Arc<Mutex<Vec<String>>>);
    struct Visitor(String);
    impl tracing::field::Visit for Visitor {
        fn record_debug(&mut self,field:&tracing::field::Field,value:&dyn std::fmt::Debug){
            use std::fmt::Write;
            write!(&mut self.0,"{}={value:?};",field.name()).unwrap();
        }
    }
    impl tracing::Subscriber for Sink {
        fn enabled(&self,_:&tracing::Metadata<'_>)->bool{true}
        fn new_span(&self,_:&tracing::span::Attributes<'_>)->tracing::span::Id{tracing::span::Id::from_u64(1)}
        fn record(&self,_:&tracing::span::Id,_:&tracing::span::Record<'_>){}
        fn record_follows_from(&self,_:&tracing::span::Id,_:&tracing::span::Id){}
        fn event(&self,event:&tracing::Event<'_>){let mut v=Visitor(String::new());event.record(&mut v);self.0.lock().unwrap().push(v.0);}
        fn enter(&self,_:&tracing::span::Id){}
        fn exit(&self,_:&tracing::span::Id){}
    }
    let captured=Arc::new(Mutex::new(Vec::new()));let bits=AtomicU32::new(0);
    tracing::subscriber::with_default(Sink(captured.clone()),||{
        for case in 0..18 {
            let(f,present)=frame_case(case);
            let reason=unsafe{inspect_pointer(present.then_some(f.info.as_ptr() as usize))}.err().unwrap();
            for _ in 0..1000 {log_rejection(&bits,reason);}
        }
    });
    let rows=captured.lock().unwrap();assert_eq!(rows.len(),18);assert_eq!(bits.load(Ordering::Relaxed),(1<<18)-1);
    for row in rows.iter(){
        assert!(row.contains("SCBL_MOD_EXPOSURE_FRAME rejected_native_context"));
        assert!(row.contains("reason="));
        assert!(!row.contains("actor=")&&!row.contains("peer=")&&!row.contains("0x"));
        println!("NATIVE_FRAME_LOG {row}");
    }
    println!("NATIVE_FRAME_LOG_BOUND attempts=18000 emitted=18 identifiers=false");
}


// Append to the exact current goggle_peer_frame_tests.rs: reuses Fixture and
// from_info -> inspect -> feedback_people -> Receiver, never a model/clone.
#[test]
fn maintenance_goggle_lifetime_expired_replay_after_pending_proxy_is_rejected() {
    use crate::goggle_exposure::{ActualDetections, Source};
    use crate::goggle_feedback::{ReceiptRejection, Receiver, Report};
    let mut observations = Vec::new();
    for detector in [Source::Rfd, Source::Ats, Source::Bms] {
        let mut f = Fixture::new();
        let original_pawns = f.pawns.clone();
        let initial = unsafe { f.frame() }.expect("existing live Merc + local Spy native fixture");
        assert_eq!(initial.context.now, 1000);
        assert_eq!(initial.context.local, 20);
        let source = initial.context.source(10, 7).unwrap().clone();
        let report = Report { match_key: initial.context.match_key, actor: source.actor,
            seq: 100, detector, targets: vec![initial.context.local] };
        let mut receiver = Receiver::default();
        assert_eq!(receiver.receive_report(&initial.context, source.peer, report.clone()), Ok(()));
        assert_ne!(receiver.detections(&initial.context), ActualDetections::default());

        // Exact binary-representable 1.5 seconds avoids float boundary ambiguity.
        // 500 elapsed ms exceeds the real 400 ms receipt lease; no wall sleep.
        f.world[0x5a0 / 4] = 1.5_f32.to_bits();
        let expired = unsafe { f.frame() }.unwrap();
        assert_eq!(expired.context.now, 1500);
        assert_eq!(receiver.detections(&expired.context), ActualDetections::default());
        assert_eq!(receiver.receive_report(&expired.context, source.peer, report.clone()), Err(ReceiptRejection::Sequence),
            "control: expiry alone must not lose the same-match watermark");

        // The accepted pending-proxy rule omits this exact-null REMOTE actor,
        // while native session peer 7, the local Spy and the scope remain live.
        f.pawns[0][0x2c0 / 4] = 0;
        let pending = unsafe { f.frame() }.expect("missing remote proxy must not invalidate the local frame");
        assert_eq!(pending.context.cache_scope(), initial.context.cache_scope());
        assert_eq!(pending.context.local, initial.context.local);
        assert!(pending.context.source(source.actor, source.peer).is_none());
        assert_eq!(pending.context.people.len(), initial.context.people.len() - 1);
        assert_eq!(receiver.detections(&pending.context), ActualDetections::default());

        // Restore the exact original pointer/key/peer, not a new owner or life.
        f.pawns[0][0x2c0 / 4] = f.actors[0].as_ptr() as u32;
        assert_eq!(f.pawns, original_pawns);
        let restored = unsafe { f.frame() }.unwrap();
        assert_eq!(restored.context.cache_scope(), initial.context.cache_scope());
        assert_eq!(restored.context.people, initial.context.people);
        assert_eq!(restored.context.source(source.actor, source.peer), Some(&source));
        assert_eq!(receiver.detections(&restored.context), ActualDetections::default(),
            "returning identity must not spontaneously restore old positives");

        let replay = receiver.receive_report(&restored.context, source.peer, report.clone());
        let replay_detected = receiver.detections(&restored.context) != ActualDetections::default();
        let fresh = Report { seq: 101, ..report };
        let fresh_result = receiver.receive_report(&restored.context, source.peer, fresh);
        let fresh_detected = receiver.detections(&restored.context) != ActualDetections::default();
        println!("GOGGLE_LIFETIME source={detector:?} elapsed_ms=500 same_scope_match_actor_peer_life=true expired_control_rejected=true pending_valid=true restored_before_replay_empty=true old_replay={replay:?} old_replay_detected={replay_detected} fresh_seq={fresh_result:?} fresh_detected={fresh_detected}");
        observations.push((detector, replay, replay_detected, fresh_result, fresh_detected));
    }
    // Observe new-sequence recovery for all sources before the expected RED.
    assert!(observations.iter().all(|(_, _, _, fresh, detected)| fresh.is_ok() && *detected),
        "new authenticated detections after proxy restoration must still work: {observations:?}");
    assert!(observations.iter().all(|(_, replay, detected, _, _)|
        *replay == Err(ReceiptRejection::Sequence) && !*detected),
        "an expired same-match report must not become a new detection solely because its remote proxy was pending: {observations:?}");
}

// Additional same-input controls. Uses the production native frame reader and
// Receiver/Inbox APIs on owned buffers; never requires a new Context field.
fn lifetime_fixture(count: usize, all_mercs: bool) -> Fixture {
    assert!((2..=32).contains(&count));
    let mut f = Fixture::new();
    f.nodes = (0..count).map(|_| buf(16)).collect();
    f.peers = (0..count).map(|_| buf(16)).collect();
    f.pcs = (0..count).map(|_| buf(0x700)).collect();
    f.pawns = (0..count).map(|_| buf(0x1c00)).collect();
    f.actors = (0..count).map(|_| buf(0x100)).collect();
    f.records = (0..count).map(|_| buf(0x60)).collect();
    f.record_array = vec![0; count];
    f.local_array = vec![0; count];
    f.info[0x448 / 4] = f.record_array.as_ptr() as u32;
    f.info[0x44c / 4] = count as u32;
    f.info[0x450 / 4] = count as u32;
    f.wi[0x36c / 4] = f.local_array.as_ptr() as u32;
    f.wi[0x370 / 4] = count as u32;
    f.wi[0x374 / 4] = count as u32;
    f.session[0x588 / 4] = f.nodes[0].as_ptr() as u32;
    for i in 0..count {
        let merc = all_mercs || i == 0;
        let team = if merc { 1 } else { 2 };
        f.local_array[i] = f.pcs[i].as_ptr() as u32;
        f.pcs[i][0x144 / 4] = f.world.as_ptr() as u32;
        f.pcs[i][0x60 / 4] = 0x60000;
        f.pcs[i][0x4c / 4] = if i == 1 { 1 << 8 } else { 2 << 8 };
        f.nodes[i][1] = if i + 1 < count { f.nodes[i + 1].as_ptr() as u32 }
            else { f.session.as_ptr() as u32 + 0x584 };
        f.nodes[i][2] = f.peers[i].as_ptr() as u32;
        f.peers[i][0] = 7 + i as u32;
        f.record_array[i] = f.records[i].as_ptr() as u32;
        f.records[i][0x48 / 4] = f.pcs[i].as_ptr() as u32;
        f.records[i][0x2c / 4] = team;
        f.pcs[i][0] = f.pcvt.as_ptr() as u32;
        f.pcs[i][0x6dc / 4] = team;
        f.pcs[i][0x520 / 4] = f.pawns[i].as_ptr() as u32;
        f.pawns[i][0] = if merc { 0x2ad2abc } else { 0x2ad3234 };
        f.pawns[i][0x374 / 4] = if merc { 0x80000 } else { 0x40000 };
        f.pawns[i][0x144 / 4] = f.world.as_ptr() as u32;
        f.pawns[i][0x20 / 4] = 100 + i as u32;
        f.pawns[i][0x2c0 / 4] = f.actors[i].as_ptr() as u32;
        f.actors[i][0] = 0x2b1cd1c;
        f.actors[i][0x88 / 4] = f.pawns[i].as_ptr() as u32;
        f.actors[i][0x50 / 4] = f.session.as_ptr() as u32;
        f.actors[i][0x18 / 4] = 7 + i as u32;
        f.actors[i][0x70 / 4] = 10 + 10 * i as u32;
    }
    f
}

#[test]
fn maintenance_goggle_lifetime_unexpired_pending_proxy_and_wrap_keep_watermarks() {
    use crate::goggle_feedback::{Receiver, Report, ReceiptRejection};
    use crate::goggle_exposure::{ActualDetections, Source};
    let mut outcomes = Vec::new();
    unsafe {
        for detector in [Source::Rfd, Source::Ats, Source::Bms] {
            let mut f = Fixture::new();
            let mut receiver = Receiver::default();
            let first = f.frame().unwrap();
            let mut report = Report { match_key:50, actor:10, seq:u32::MAX, detector, targets:vec![20] };
            assert_eq!(receiver.receive_report(&first.context, 7, report.clone()), Ok(()));
            f.world[0x5a0 / 4] = 1.125f32.to_bits(); // exact 125ms; still inside 400ms lease
            f.pawns[0][0x2c0 / 4] = 0;
            let pending = f.frame().unwrap();
            assert_eq!(pending.context.cache_scope(), first.context.cache_scope());
            assert_eq!(receiver.detections(&pending.context), ActualDetections::default());
            assert_eq!(receiver.receive_report(&pending.context, 7, report.clone()), Err(ReceiptRejection::Source));
            f.pawns[0][0x2c0 / 4] = f.actors[0].as_ptr() as u32;
            let restored = f.frame().unwrap();
            assert_eq!(receiver.detections(&restored.context), ActualDetections::default());
            let replay = receiver.receive_report(&restored.context, 7, report.clone());
            let replay_visible = receiver.detections(&restored.context) != ActualDetections::default();
            report.seq = 0;
            let fresh = receiver.receive_report(&restored.context, 7, report.clone());
            assert_eq!(fresh, Ok(()));
            assert_ne!(receiver.detections(&restored.context), ActualDetections::default());
            // Role loss keeps no positives and does not authenticate a Merc report.
            f.pawns[0][0] = 0x2ad3234;
            f.pawns[0][0x374 / 4] = 0x40000;
            let spy = f.frame().unwrap();
            assert_eq!(receiver.detections(&spy.context), ActualDetections::default());
            assert_eq!(receiver.receive_report(&spy.context, 7, report.clone()), Err(ReceiptRejection::Source));
            f.pawns[0][0] = 0x2ad2abc;
            f.pawns[0][0x374 / 4] = 0x80000;
            let merc = f.frame().unwrap();
            assert_eq!(receiver.detections(&merc.context), ActualDetections::default());
            let role_replay = receiver.receive_report(&merc.context, 7, report.clone());
            report.seq = 1;
            assert_eq!(receiver.receive_report(&merc.context, 7, report), Ok(()));
            println!("GOGGLE_LIFETIME_UNEXPIRED source={detector:?} elapsed_ms=125 replay={replay:?} replay_visible={replay_visible} wrapped_fresh={fresh:?} role_return_replay={role_replay:?}");
            outcomes.push((replay, replay_visible, role_replay));
        }
    }
    assert!(outcomes.iter().all(|(r, visible, role)| *r == Err(ReceiptRejection::Sequence)
        && !visible && *role == Err(ReceiptRejection::Sequence)), "{outcomes:?}");
}

#[test]
fn maintenance_goggle_lifetime_identity_changes_retire_only_exact_old_watermarks() {
    use crate::goggle_feedback::{Receiver, Report, ReceiptRejection};
    use crate::goggle_exposure::{ActualDetections, Source};
    unsafe {
        for detector in [Source::Rfd, Source::Ats, Source::Bms] {
            for kind in 0..3 {
                let mut f = Fixture::new();
                let first = f.frame().unwrap();
                let mut receiver = Receiver::default();
                let report = Report { match_key:50, actor:10, seq:100, detector, targets:vec![20] };
                assert_eq!(receiver.receive_report(&first.context, 7, report.clone()), Ok(()));
                if kind == 0 {
                    // Actual native peer departure, not just the missing actor proxy.
                    f.pawns[0][0x2c0 / 4] = 0;
                    f.session[0x588 / 4] = f.nodes[1].as_ptr() as u32;
                    let left = f.frame().unwrap();
                    assert_eq!(left.context.people.len(), 1);
                    assert_eq!(left.context.cache_scope(), first.context.cache_scope());
                    assert_eq!(receiver.detections(&left.context), ActualDetections::default());
                    f.session[0x588 / 4] = f.nodes[0].as_ptr() as u32;
                    f.pawns[0][0x2c0 / 4] = f.actors[0].as_ptr() as u32;
                    // Same identity rejoining after an observed true departure may start low.
                } else if kind == 1 {
                    f.actors[0][0x70 / 4] = 11; // same From, replacement actor
                } else {
                    f.peers[0][0] = 9;
                    f.actors[0][0x18 / 4] = 9; // same actor, replacement From
                }
                let changed = f.frame().unwrap();
                assert_eq!(changed.context.cache_scope(), first.context.cache_scope());
                assert_eq!(receiver.detections(&changed.context), ActualDetections::default());
                if kind != 0 {
                    assert_eq!(receiver.receive_report(&changed.context, 7, report.clone()), Err(ReceiptRejection::Source));
                }
                let actor = if kind == 1 {11} else {10};
                let from = if kind == 2 {9} else {7};
                let fresh = Report { actor, seq:1, ..report };
                assert_eq!(receiver.receive_report(&changed.context, from, fresh.clone()), Ok(()));
                assert_eq!(receiver.receive_report(&changed.context, from, fresh), Err(ReceiptRejection::Sequence));
                assert_ne!(receiver.detections(&changed.context), ActualDetections::default());
                println!("GOGGLE_LIFETIME_IDENTITY source={detector:?} kind={kind} same_scope=true low_fresh_accepted=true duplicate_rejected=true");
            }
        }
    }
}

#[test]
fn maintenance_goggle_lifetime_session_peer_churn_stays_within_96_receipts() {
    use crate::goggle_feedback::{Receiver, Report, ReceiptRejection};
    use crate::goggle_exposure::Source;
    unsafe {
        let mut f = lifetime_fixture(32, true);
        let frame = f.frame().unwrap();
        assert_eq!(frame.context.people.len(), 32);
        let mut receiver = Receiver::default();
        for person in &frame.context.people {
            for detector in [Source::Rfd, Source::Ats, Source::Bms] {
                let report = Report {match_key:50, actor:person.actor, seq:100, detector, targets:vec![]};
                assert_eq!(receiver.receive_report(&frame.context, person.peer, report), Ok(()));
            }
        }
        for round in 0..192u32 {
            // First observe a real departure with all other 31 owners unchanged.
            f.pawns[0][0x2c0 / 4] = 0;
            f.session[0x588 / 4] = f.nodes[1].as_ptr() as u32;
            let left = f.frame().unwrap();
            receiver.detections(&left.context);
            f.actors[0][0x70 / 4] = 10000 + round;
            f.peers[0][0] = 1000 + round;
            f.actors[0][0x18 / 4] = 1000 + round;
            f.pawns[0][0x2c0 / 4] = f.actors[0].as_ptr() as u32;
            f.session[0x588 / 4] = f.nodes[0].as_ptr() as u32;
            let joined = f.frame().unwrap();
            assert_eq!(joined.context.cache_scope(), frame.context.cache_scope());
            for detector in [Source::Rfd, Source::Ats, Source::Bms] {
                let report = Report {match_key:50, actor:(10000 + round) as u64, seq:1, detector, targets:vec![]};
                assert_eq!(receiver.receive_report(&joined.context, 1000 + round, report.clone()), Ok(()), "churn round {round}");
                assert_eq!(receiver.receive_report(&joined.context, 1000 + round, report), Err(ReceiptRejection::Sequence));
            }
        }
        let joined = f.frame().unwrap();
        for person in joined.context.people.iter().skip(1) {
            for detector in [Source::Rfd, Source::Ats, Source::Bms] {
                let report = Report {match_key:50, actor:person.actor, seq:100, detector, targets:vec![]};
                assert_eq!(receiver.receive_report(&joined.context, person.peer, report), Err(ReceiptRejection::Sequence));
            }
        }
        println!("GOGGLE_LIFETIME_CAPACITY initial_receipts=96 departed_rejoined=192 fresh_receipts=576 other_owner_watermarks_preserved=93 capacity_errors=0");
    }
}

#[test]
fn maintenance_goggle_lifetime_wire_inbox_single_and_fragmented_replays_are_rejected() {
    use crate::goggle_feedback::{Receiver, Report, ReceiptRejection};
    use crate::goggle_feedback_wire::{packets, Inbox};
    use crate::goggle_exposure::{ActualDetections, Source};
    let mut outcomes = Vec::new();
    unsafe {
        for detector in [Source::Rfd, Source::Ats, Source::Bms] {
            for fragmented in [false, true] {
                let mut f = lifetime_fixture(9, false);
                let frame = f.frame().unwrap();
                let mut receiver = Receiver::default();
                let mut inbox = Inbox::default();
                let targets = if fragmented {(20..=90).step_by(10).collect()} else {vec![20]};
                let report = Report {match_key:50, actor:10, seq:100, detector, targets};
                let old_packets = packets(&report).unwrap();
                assert_eq!(old_packets.len(), if fragmented {3} else {1});
                let decoded: Vec<_> = old_packets.iter().filter_map(|p| inbox.receive(&frame.context, 7, p)).collect();
                assert_eq!(decoded, vec![report.clone()]);
                assert_eq!(receiver.receive_report(&frame.context, 7, decoded[0].clone()), Ok(()));
                f.world[0x5a0 / 4] = 1.5f32.to_bits();
                f.pawns[0][0x2c0 / 4] = 0;
                let pending = f.frame().unwrap();
                assert_eq!(receiver.detections(&pending.context), ActualDetections::default());
                assert!(old_packets.iter().all(|p| inbox.receive(&pending.context, 7, p).is_none()));
                f.pawns[0][0x2c0 / 4] = f.actors[0].as_ptr() as u32;
                let restored = f.frame().unwrap();
                assert_eq!(receiver.detections(&restored.context), ActualDetections::default());
                let replay_decoded: Vec<_> = old_packets.iter().filter_map(|p| inbox.receive(&restored.context, 7, p)).collect();
                assert_eq!(replay_decoded, vec![report.clone()]); // Inbox is not an anti-replay cache.
                let replay = receiver.receive_report(&restored.context, 7, replay_decoded[0].clone());
                let visible = receiver.detections(&restored.context) != ActualDetections::default();
                let fresh = Report {seq:101, ..report};
                let fresh_packets = packets(&fresh).unwrap();
                let fresh_decoded: Vec<_> = fresh_packets.iter().filter_map(|p| inbox.receive(&restored.context, 7, p)).collect();
                assert_eq!(fresh_decoded, vec![fresh]);
                assert_eq!(receiver.receive_report(&restored.context, 7, fresh_decoded[0].clone()), Ok(()));
                assert_ne!(receiver.detections(&restored.context), ActualDetections::default());
                println!("GOGGLE_LIFETIME_WIRE source={detector:?} fragments={} identical_old_packets=true replay={replay:?} replay_visible={visible} fresh_accepted=true", old_packets.len());
                outcomes.push((replay, visible));
            }
        }
    }
    assert!(outcomes.iter().all(|(r, visible)| *r == Err(ReceiptRejection::Sequence) && !visible), "{outcomes:?}");
}
