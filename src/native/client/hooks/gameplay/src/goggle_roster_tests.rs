//! Source-bound same-match roster transitions; no engine, network, or timing stubs.
use crate::ats_marks::Role;
use crate::goggle_exposure::{ActualDetections, Source};
use crate::goggle_feedback::{Context, Person, Publisher, Receiver, Report};

fn context(local: u64) -> Context {
    Context {
        scope: [1, 2, 3, 4], match_key: 50, now: 1000, local,
        session_peers: vec![7, 20, 21],
        people: vec![
            Person { actor: 10, peer: 7, team: 1, role: Role::Merc },
            Person { actor: 20, peer: 20, team: 2, role: Role::Spy },
            Person { actor: 21, peer: 21, team: 2, role: Role::Spy },
        ],
    }
}
fn report(seq: u32) -> Report {
    Report { match_key: 50, actor: 10, seq, detector: Source::Rfd, targets: vec![20,21] }
}
fn detected(rx: &mut Receiver, c: &Context) -> bool {
    rx.detections(c) != ActualDetections::default()
}

pub(crate) fn observations() -> Vec<(&'static str, bool)> {
    let mut out=Vec::new();
    let mut c=context(20); let mut rx=Receiver::default();
    assert!(rx.receive_report(&c,7,report(100)).is_ok());
    out.push(("valid_enemy_initially_detected",detected(&mut rx,&c)));
    c.people[1].team=1; c.now+=1;
    out.push(("target_team_change_withdraws_detection",!detected(&mut rx,&c)));
    c.people[1].team=2; c.now+=1;
    out.push(("old_detection_does_not_revive_after_team_return",!detected(&mut rx,&c)));
    out.push(("team_change_keeps_same_match_replay_watermark",rx.receive_report(&c,7,report(100)).is_err()));
    out.push(("fresh_report_after_team_return_is_accepted",rx.receive_report(&c,7,report(101)).is_ok() && detected(&mut rx,&c)));

    let mut c=context(20); let mut rx=Receiver::default();
    assert!(rx.receive_report(&c,7,report(100)).is_ok());
    c.people[1].role=Role::Merc; c.now+=1;
    out.push(("merc_local_has_no_spy_warning",!detected(&mut rx,&c)));
    c.people[1].role=Role::Spy; c.now+=1;
    out.push(("old_detection_does_not_revive_after_role_return",!detected(&mut rx,&c)));

    let mut c=context(20); let mut rx=Receiver::default();
    assert!(rx.receive_report(&c,7,report(100)).is_ok());
    c.people[0].team=2; c.now+=1;
    out.push(("detector_team_change_withdraws_detection",!detected(&mut rx,&c)));

    for (kind,label) in [(0,"partial_merge_excludes_new_teammate"),(1,"partial_merge_excludes_new_merc"),(2,"partial_merge_excludes_departed_actor")] {
        let mut c=context(10); let mut tx=Publisher::default();
        tx.sample(&c,Source::Ats,&[20],true).unwrap();
        match kind { 0=>c.people[1].team=1,1=>c.people[1].role=Role::Merc,_=>{c.people.remove(1);} }
        c.now+=100;
        let next=tx.sample(&c,Source::Ats,&[21],true).unwrap();
        let mut recipient=c.clone(); recipient.local=21;
        let mut rx=Receiver::default();
        out.push((label,next.targets==vec![21] && rx.receive_report(&recipient,7,next).is_ok()));
    }

    let mut c=context(10); let mut tx=Publisher::default();
    let first=tx.sample(&c,Source::Bms,&[20],true).unwrap();
    c.people[0].role=Role::Spy; c.now+=1;
    assert!(tx.sample(&c,Source::Bms,&[],true).is_none());
    c.people[0].role=Role::Merc; c.now+=99;
    out.push(("local_role_return_needs_new_detection",tx.sample(&c,Source::Bms,&[],true).is_none()));
    c.now+=100;
    let next=tx.sample(&c,Source::Bms,&[21],true).unwrap();
    out.push(("local_role_change_preserves_sequence",next.seq>first.seq));

    // Both local roles may receive and preserve authenticated remote events.
    let mut c=context(10); let mut rx=Receiver::default();
    assert!(rx.receive_report(&c,7,report(10)).is_ok());
    out.push(("local_merc_does_not_skip_remote_receipt",!detected(&mut rx,&c)));
    c.local=21;
    out.push(("unaffected_opponent_report_is_retained",detected(&mut rx,&c)));
    out
}

#[test]
fn current_roster_prunes_cached_positives_without_losing_watermarks() {
    let checks=observations();
    for (name,pass) in &checks { println!("{}={}",name,pass); }
    assert!(checks.iter().all(|(_,pass)|*pass),"same-match roster regression");
}
