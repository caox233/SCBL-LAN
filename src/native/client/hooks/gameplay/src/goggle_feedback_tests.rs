use super::*;
fn context(local: u64) -> Context {
    Context {
        scope: [1, 2, local, 3],
        match_key: 50,
        now: 100,
        local,
        session_peers: vec![7, 8, 9],
        people: vec![
            Person {
                actor: 10,
                peer: 7,
                team: 1,
                role: Role::Merc,
            },
            Person {
                actor: 20,
                peer: 8,
                team: 2,
                role: Role::Spy,
            },
            Person {
                actor: 30,
                peer: 9,
                team: 1,
                role: Role::Merc,
            },
        ],
    }
}
fn report(seq: u32) -> Report {
    Report {
        match_key: 50,
        actor: 10,
        seq,
        detector: Source::Rfd,
        targets: vec![20],
    }
}
fn detected(c: &Context, r: &mut Receiver) -> bool {
    r.detections(c) != ActualDetections::default()
}
#[test]
fn codec_roundtrips_all_sources_at_limit_without_chat_overflow() {
    for detector in [Source::Rfd, Source::Ats, Source::Bms] {
        for n in 0..=MAX_TARGETS {
            let mut r = report(u32::MAX);
            r.detector = detector;
            r.targets = (100..100 + n as u64).collect();
            let text = r.encode().unwrap();
            assert!(text.len() <= 144);
            assert_eq!(Report::decode(&text), Some(r));
        }
    }
}
#[test]
fn codec_rejects_invalid_ids_duplicates_unicode_truncation_and_trailing_groups() {
    let r = report(1);
    let good = r.encode().unwrap();
    for n in 0..good.len() {
        assert!(Report::decode(&good[..n]).is_none());
    }
    for tail in [0u16, 65, 0x4e00, 0xffff] {
        let mut s = good.clone();
        s.push(tail);
        assert!(Report::decode(&s).is_none());
    }
    for ids in [vec![0], vec![20, 20], vec![1; 9]] {
        let mut x = r.clone();
        x.targets = ids;
        assert!(x.encode().is_none());
    }
    let mut zero = r;
    zero.actor = 0;
    assert!(zero.encode().is_none());
    assert!(Report::decode(&vec![65; 145]).is_none());
}
#[test]
fn native_source_match_role_team_and_roster_are_all_required() {
    let c = context(20);
    let mut receiver = Receiver::default();
    for from in [0, 8, 9, u32::MAX] {
        assert!(!receiver.receive(&c, from, report(1)));
    }
    let mut bad = report(1);
    bad.match_key = 51;
    assert!(!receiver.receive(&c, 7, bad));
    let mut bad = report(1);
    bad.actor = 20;
    assert!(!receiver.receive(&c, 8, bad));
    let mut bad = report(1);
    bad.targets = vec![30];
    assert!(!receiver.receive(&c, 7, bad));
    let mut bad = report(1);
    bad.targets = vec![999];
    assert!(!receiver.receive(&c, 7, bad));
    assert!(!detected(&c, &mut receiver));
    assert!(receiver.receive(&c, 7, report(1)));
    assert!(detected(&c, &mut receiver));
}
#[test]
fn duplicate_or_unknown_native_owners_fail_closed() {
    let mut c = context(20);
    let mut r = Receiver::default();
    c.people[2].peer = 7;
    assert!(!c.valid());
    assert!(!r.receive(&c, 7, report(1)));
    c.people[2].peer = 9;
    c.people[2].actor = 10;
    assert!(!c.valid());
    c.people[2].actor = 30;
    c.people[2].team = 255;
    assert!(!c.valid());
    c.people[2].team = 1;
    c.people[2].peer = u32::MAX;
    assert!(!c.valid());
}
#[test]
fn duplicate_old_reports_do_not_extend_lease_and_timeout_is_exact() {
    let mut c = context(20);
    let mut r = Receiver::default();
    assert!(r.receive(&c, 7, report(10)));
    c.now = 499;
    assert!(detected(&c, &mut r));
    assert!(!r.receive(&c, 7, report(10)));
    assert!(!r.receive(&c, 7, report(9)));
    c.now = 500;
    assert!(!detected(&c, &mut r));
    assert!(!r.receive(&c, 7, report(10)));
    assert!(r.receive(&c, 7, report(11)));
    assert!(detected(&c, &mut r));
}
#[test]
fn withdrawal_is_source_specific_other_merc_or_detector_survives() {
    let c = context(20);
    let mut r = Receiver::default();
    assert!(r.receive(&c, 7, report(1)));
    let mut other = report(1);
    other.actor = 30;
    other.detector = Source::Bms;
    assert!(r.receive(&c, 9, other.clone()));
    let mut clear = report(2);
    clear.targets.clear();
    assert!(r.receive(&c, 7, clear));
    assert!(detected(&c, &mut r));
    other.seq = 2;
    other.targets.clear();
    assert!(r.receive(&c, 9, other));
    assert!(!detected(&c, &mut r));
}
#[test]
fn source_death_disconnect_scene_or_clock_reset_remove_receipts() {
    for kind in 0..4 {
        let mut c = context(20);
        let mut r = Receiver::default();
        assert!(r.receive(&c, 7, report(1)));
        match kind {
            0 => {
                c.people.remove(0);
            }
            1 => c.scope[0] += 1,
            2 => c.scope[3] += 1,
            _ => c.now -= 1,
        }
        assert!(!detected(&c, &mut r));
    }
}
#[test]
fn sequence_wrap_is_forward_but_half_range_is_not() {
    let c = context(20);
    let mut r = Receiver::default();
    assert!(r.receive(&c, 7, report(u32::MAX)));
    assert!(r.receive(&c, 7, report(0)));
    assert!(!r.receive(&c, 7, report(0x80000000)));
}
#[test]
fn transient_publisher_reset_does_not_reuse_wire_sequences() {
    for detector in [Source::Rfd, Source::Ats, Source::Bms] {
        for reset in 0..4 {
            let mut merc = context(10);
            let mut spy = context(20);
            let mut sender = Publisher::default();
            let mut receiver = Receiver::default();
            for _ in 0..20 {
                let r = sender.sample(&merc, detector, &[20], false).unwrap();
                spy.now = merc.now;
                assert!(receiver.receive(&spy, 7, r));
                merc.now += 100;
            }
            match reset {
                0 => {
                    let mut invalid = merc.clone();
                    invalid.people[2].peer = 7;
                    assert!(sender.sample(&invalid, detector, &[20], false).is_none());
                }
                1 => assert!(sender.sample(&merc, detector, &[20; MAX_TARGETS + 1], false).is_none()),
                2 => merc.scope[3] += 1,
                _ => merc.now -= 200,
            }
            // Sender invalidation must clear detections, not reset its wire
            // sequence behind a still-live receiver's anti-replay watermark.
            assert!(sender.sample(&merc, detector, &[], true).is_none());
            let fresh = sender.sample(&merc, detector, &[20], false).unwrap();
            assert_eq!(fresh.seq, 21, "detector={detector:?} reset={reset}");
            spy.now += 100; // receiver clock did not reset with sender
            assert!(receiver.receive(&spy, 7, fresh.clone()));
            assert!(detected(&spy, &mut receiver));
            assert!(!receiver.receive(&spy, 7, fresh)); // replay still rejected
            let mut old = report(20);
            old.detector = detector;
            assert!(!receiver.receive(&spy, 7, old));
        }
    }
}
#[test]
fn publisher_rate_limit_and_complete_withdrawal_are_bounded() {
    let mut c = context(10);
    let mut p = Publisher::default();
    assert!(p.sample(&c, Source::Rfd, &[], false).is_none());
    assert_eq!(
        p.sample(&c, Source::Rfd, &[20], false).unwrap().targets,
        vec![20]
    );
    c.now = 199;
    assert!(p.sample(&c, Source::Rfd, &[20], false).is_none());
    c.now = 200;
    assert!(p
        .sample(&c, Source::Rfd, &[], false)
        .unwrap()
        .targets
        .is_empty());
    c.now = 300;
    assert!(p.sample(&c, Source::Rfd, &[], false).is_none());
}
#[test]
fn ats_alternate_parity_retains_only_recent_actual_positive() {
    let mut c = context(10);
    let mut p = Publisher::default();
    assert!(p.sample(&c, Source::Ats, &[20], true).is_some());
    c.now = 200;
    assert_eq!(
        p.sample(&c, Source::Ats, &[], true).unwrap().targets,
        vec![20]
    );
    c.now = 300;
    assert!(p
        .sample(&c, Source::Ats, &[], true)
        .unwrap()
        .targets
        .is_empty());
    c.now = 400;
    assert!(p.sample(&c, Source::Ats, &[20], true).is_some());
    c.now = 500;
    assert!(p
        .sample(&c, Source::Ats, &[], false)
        .unwrap()
        .targets
        .is_empty());
}
#[test]
fn integrated_two_peer_codec_receipt_warning_priority_and_expiry() {
    use crate::goggle_exposure::{Frame, Life, Presentation, Warning};
    let merc = context(10);
    let mut spy = context(20);
    let mut p = Publisher::default();
    let mut r = Receiver::default();
    let mut ui = Warning::default();
    let wire = p
        .sample(&merc, Source::Bms, &[20], false)
        .unwrap()
        .encode()
        .unwrap();
    assert!(r.receive(&spy, 7, Report::decode(&wire).unwrap()));
    let mut frame = Frame {
        life: Life { scene: 1, spawn: 1 },
        now_ms: 100,
        dx11_mod: true,
        local_alive_spy: true,
        tactical_goggles_on: true,
        native_marked: Some(false),
        detections: r.detections(&spy),
    };
    assert_eq!(ui.update(frame), Presentation::Exposed);
    spy.now = 500;
    frame.now_ms = 500;
    frame.detections = r.detections(&spy);
    assert_eq!(ui.update(frame), Presentation::Exposed); // requested minimum two seconds, not infinite receipt
    frame.now_ms = 2100;
    assert_eq!(ui.update(frame), Presentation::Hidden);
    frame.native_marked = Some(true);
    assert_eq!(ui.update(frame), Presentation::Native);
}

#[test]
fn maintenance_receiver_behavior_compatibility() {
    for detector in [Source::Rfd,Source::Ats,Source::Bms] {
        for case in 0..12 {
            let mut c=context(20);let mut receiver=Receiver::default();let mut first=report(10);first.detector=detector;
            assert!(receiver.receive(&c,7,first));c.now=200;
            let mut next=report(11);next.detector=detector;let mut from=7;
            match case {
                1=>next.seq=10,2=>next.seq=9,3=>next.seq=0x8000000a,4=>next.match_key=51,
                5=>from=9,6=>next.targets=vec![30],7=>next.targets=vec![999],8=>next.targets.clear(),
                9=>c.people[2].peer=7,10=>next.targets=vec![20,20],11=>next.actor=99,_=>(),
            }
            let accepted=receiver.receive(&c,from,next);let visible=detected(&c,&mut receiver);
            assert_eq!(accepted,matches!(case,0|8));assert_eq!(visible,!matches!(case,8|9));
            println!("RECEIPT_BEHAVIOR source={detector:?} case={case} accepted={accepted} receipt_active={visible}");
        }
    }
}

#[test]
fn receipt_diagnostics_identify_each_existing_rejection_gate() {
    for detector in [Source::Rfd,Source::Ats,Source::Bms] {
        for reason in [ReceiptRejection::Context,ReceiptRejection::Report,ReceiptRejection::Match,
            ReceiptRejection::Source,ReceiptRejection::Targets,ReceiptRejection::Sequence,ReceiptRejection::Capacity] {
            let mut c=context(20);let mut r=report(11);r.detector=detector;let mut receiver=Receiver::default();let mut from=7;
            match reason {
                ReceiptRejection::Context=>c.scope[0]=0,
                ReceiptRejection::Report=>r.targets=vec![20,20],
                ReceiptRejection::Match=>r.match_key=51,
                ReceiptRejection::Source=>from=9,
                ReceiptRejection::Targets=>r.targets=vec![30],
                ReceiptRejection::Sequence=>assert_eq!(receiver.receive_report(&c,7,r.clone()),Ok(())),
                ReceiptRejection::Capacity=>{
                    // Defensive impossible-from-public-API internal saturation fixture;
                    // do not call this an attainable 96-peer multiplayer configuration.
                    let mut other=report(1);other.actor=30;other.detector=Source::Bms;
                    receiver.scope=Some(c.cache_scope());receiver.last=c.now;
                    receiver.entries=(0..96).map(|_|Received{from:9,report:other.clone(),at:c.now}).collect();
                },
            }
            assert_eq!(receiver.receive_report(&c,from,r),Err(reason));
            println!("RECEIPT_REASON source={detector:?} rejected={} identity_values_logged=false",reason.label());
        }
    }
}
#[test]
fn receipt_diagnostics_preserve_acceptance_expiry_and_replay_rejection() {
    let mut c=context(20);let mut r=Receiver::default();
    assert_eq!(r.receive_report(&c,7,report(u32::MAX)),Ok(()));
    assert_eq!(r.receive_report(&c,7,report(0)),Ok(()));
    c.now=499;assert!(detected(&c,&mut r));
    assert_eq!(r.receive_report(&c,7,report(0)),Err(ReceiptRejection::Sequence));
    c.now=500;assert!(!detected(&c,&mut r));
    assert_eq!(r.receive_report(&c,7,report(0)),Err(ReceiptRejection::Sequence));
    assert_eq!(r.receive_report(&c,7,report(1)),Ok(()));
    let mut empty=report(2);empty.targets.clear();assert_eq!(r.receive_report(&c,7,empty),Ok(()));
    assert!(!detected(&c,&mut r));
    println!("RECEIPT_DIAGNOSTICS accepted_wrap=true expired_replay_denied=true complete_withdrawal=true");
}
