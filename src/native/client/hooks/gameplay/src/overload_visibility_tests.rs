use super::*;

fn id(actor: usize, key: u32, root: usize) -> Token {
    Token { actor, key, world: 7, root }
}
#[test]
fn exact_root_only_changes_native_false_to_true() {
    assert_eq!(visibility_arg(0, true, Some(0)), 1);
    assert_eq!(visibility_arg(1, true, Some(0)), 1);
    assert_eq!(visibility_arg(0, false, Some(0)), 0); // Spy/unknown/ordinary.
    assert_eq!(visibility_arg(0, true, Some(1)), 0); // Native exception bit.
    assert_eq!(visibility_arg(0, true, None), 0); // Unknown memory fails native.
    assert_eq!(visibility_arg(0x100, true, Some(0)), 1); // Exact boolean, not bitwise OR.
}
#[test]
fn queue_dedupe_and_generation_reset() {
    let mut entries = [None; LIMIT];
    assert!(reserve(&mut entries, id(10, 1, 20)));
    assert!(!reserve(&mut entries, id(10, 1, 20)));
    assert!(reserve(&mut entries, id(10, 2, 20))); // Same address, new serial.
    assert_eq!(entries.iter().flatten().filter(|i| i.actor == 10).count(), 1);
    assert!(reserve(&mut entries, id(10, 2, 21))); // Same actor, rebuilt root.
    assert_eq!(entries.iter().flatten().find(|i| i.actor == 10).unwrap().root, 21);
}
#[test]
fn capacity_does_not_evict_other_live_actor() {
    let mut entries = [None; LIMIT];
    for actor in 1..=LIMIT { assert!(reserve(&mut entries, id(actor, actor as u32, actor + 1000))); }
    assert!(!reserve(&mut entries, id(LIMIT + 1, 1, 2000)));
    assert_eq!(entries.iter().flatten().count(), LIMIT);
}
#[test]
fn release_scope_captures_only_inside_and_fails_native_on_overflow() {
    let mut p = Pending::new();
    p.note(10);
    assert_eq!(p.len, 0);
    p.depth = 1;
    p.note(10); p.note(10); p.note(11);
    assert_eq!(&p.actors[..p.len], &[10, 11]);
    p.note(12); p.note(13); p.note(14);
    assert!(p.overflow);
    assert_eq!(p.len, 0);
}
#[test]
fn closure_called_once_and_scope_cleared() {
    let mut calls = 0;
    let result = unsafe { with_release(|| { calls += 1; 42 }) };
    assert_eq!(result, 42);
    assert_eq!(calls, 1);
    PENDING.with(|p| assert_eq!(p.borrow().depth, 0));
}
#[test]
fn unregistered_reset_never_touches_other_particle_roots() {
    let mut checked = 0;
    let mut retired = 0;
    let mut mismatch_retired = 0;
    before_reset(false, || { checked += 1; false }, || retired += 1, || mismatch_retired += 1);
    assert_eq!((checked, retired, mismatch_retired), (0, 0, 0));
    before_reset(true, || { checked += 1; false }, || retired += 1, || mismatch_retired += 1);
    assert_eq!((checked, retired, mismatch_retired), (1, 1, 1));
    before_reset(true, || { checked += 1; true }, || retired += 1, || mismatch_retired += 1);
    assert_eq!((checked, retired, mismatch_retired), (2, 2, 1));
}
