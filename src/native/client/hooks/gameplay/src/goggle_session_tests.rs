//! Same live object addresses/actors, different logical match. No engine or transport stubs.
use crate::ats_marks::Role;
use crate::goggle_exposure::Source;
use crate::goggle_feedback::{Context, Person, Publisher, Receiver, Report};
use crate::goggle_feedback_wire::{packets, Inbox};
use crate::intel_rfd_wire;

fn context(local: u64) -> Context {
    Context {
        scope: [1, 2, 3, 4], match_key: 50, now: 1000, local,
        session_peers: std::iter::once(7).chain(20..28).collect(),
        people: std::iter::once(Person { actor: 10, peer: 7, team: 1, role: Role::Merc })
            .chain((20..28).map(|actor| Person { actor, peer: actor as u32, team: 2, role: Role::Spy }))
            .collect(),
    }
}
fn report(c: &Context, seq: u32) -> Report {
    Report { match_key: c.match_key, actor: 10, seq, detector: Source::Rfd, targets: vec![20] }
}

pub(crate) fn observations() -> Vec<(&'static str, bool)> {
    let mut out = Vec::new();
    let mut c = context(20);
    let mut rx = Receiver::default();
    assert!(rx.receive_report(&c, 7, report(&c, 100)).is_ok());
    out.push(("same_match_duplicate_rejected", rx.receive_report(&c, 7, report(&c, 100)).is_err()));
    c.match_key += 1;
    c.now += 1;
    out.push(("new_match_low_sequence_accepted", rx.receive_report(&c, 7, report(&c, 1)).is_ok()));
    let mut stale = report(&c, 101);
    stale.match_key -= 1;
    out.push(("old_match_report_rejected", rx.receive_report(&c, 7, stale).is_err()));
    out.push(("spoofed_source_rejected", rx.receive_report(&c, 20, report(&c, 102)).is_err()));

    let mut c = context(10);
    let mut tx = Publisher::default();
    let first = tx.sample(&c, Source::Ats, &[20], true).unwrap();
    c.match_key += 1;
    c.now += 1;
    let next = tx.sample(&c, Source::Ats, &[21], true);
    out.push(("new_match_first_sample_not_throttled", next.is_some()));
    out.push(("new_match_sequence_remains_monotonic", next.as_ref().is_some_and(|r| r.seq == first.seq + 1)));
    c.now += 100;
    let next = tx.sample(&c, Source::Ats, &[21], true);
    out.push(("new_match_partial_sample_drops_old_targets", next.as_ref().is_some_and(|r| r.targets == vec![21])));

    let mut c = context(20);
    let mut inbox = Inbox::default();
    let old = Report { targets: (20..28).collect(), ..report(&c, 9) };
    let old_parts = packets(&old).unwrap();
    assert!(old_parts.len() > 1);
    assert!(inbox.receive(&c, 7, &old_parts[0]).is_none());
    c.match_key += 1;
    c.now += 1;
    let fresh = Report { match_key: c.match_key, ..old };
    let parts = packets(&fresh).unwrap();
    let received = parts.iter().filter_map(|p| inbox.receive(&c, 7, p)).collect::<Vec<_>>();
    out.push(("new_match_fragment_sequence_can_restart", received == vec![fresh]));

    let mut c = context(10);
    let mut intel = intel_rfd_wire::State::default();
    let old = intel_rfd_wire::Report { match_key: c.match_key, actor: 20, seq: 100, active: true };
    assert!(intel.receive(&c, 20, old));
    c.match_key += 1;
    c.now += 1;
    out.push(("intel_new_match_low_sequence_accepted", intel.receive(&c, 20, intel_rfd_wire::Report { match_key: c.match_key, seq: 1, ..old })));
    out.push(("intel_old_match_rejected", !intel.receive(&c, 20, old)));

    let mut c = context(20);
    let mut intel = intel_rfd_wire::State::default();
    let first = intel.publish(&c, true).unwrap();
    c.match_key += 1;
    c.now += 1;
    let next = intel.publish(&c, true);
    out.push(("intel_new_match_not_throttled", next.is_some()));
    out.push(("intel_new_match_sequence_remains_monotonic", next.is_some_and(|r| r.seq == first.seq + 1)));
    out
}

#[test]
fn match_identity_separates_all_feedback_caches() {
    let observations = observations();
    for (name, passed) in &observations { println!("{}={}", name, passed); }
    assert!(observations.iter().all(|(_, passed)| *passed), "session transition regression");
}
