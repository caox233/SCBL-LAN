use super::*;

#[test]
fn generation_exhaustion_is_closed_and_clear_never_reuses_old_completions() {
    let flow = ActionFlow::default();
    let first = flow.reserve(123, Phase::Busy).unwrap();
    assert_eq!(first, 1);
    flow.clear();
    let second = flow.reserve(123, Phase::Busy).unwrap();
    assert_eq!(second, 2);
    flow.finish(123, first, None);
    assert!(flow.is_current(123, second));
    flow.finish(123, first, Some(Phase::Busy));
    assert!(flow.is_current(123, second));
    assert_eq!(flow.reserve(123, Phase::Busy), None);
    assert_eq!(flow.state.lock().unwrap().generation, 2);
    flow.clear();
    flow.finish(123, second, Some(Phase::Busy));
    assert!(flow.state.lock().unwrap().abilities.is_empty());
    // The counter remains monotonically increasing even across full teardown.
    flow.state.lock().unwrap().generation = u64::MAX - 1;
    let last = flow.reserve(123, Phase::Busy).unwrap();
    assert_eq!(last, u64::MAX);
    assert_eq!(flow.reserve(456, Phase::Busy), None);
    assert!(flow.is_current(123, last));
    assert_eq!(flow.state.lock().unwrap().abilities.len(), 1);
    flow.cancel(123);
    assert_eq!(flow.reserve(123, Phase::Busy), None);
    flow.clear();
    assert_eq!(flow.reserve(123, Phase::Busy), None);
    flow.finish(123, last, Some(Phase::Busy));
    let state = flow.state.lock().unwrap();
    assert!(state.abilities.is_empty());
    assert_eq!(state.generation, u64::MAX);
    println!("FLOW_BOUNDARY: clear=monotonic duplicate=unchanged stale=ignored exhaustion=closed teardown=no_resurrection");
}
