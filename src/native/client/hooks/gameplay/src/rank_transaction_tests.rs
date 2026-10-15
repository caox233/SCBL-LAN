use super::*;
fn patch(bytes: &mut [u8; 5]) -> CallPatch {
    (bytes.as_mut_ptr() as usize, [0xe8, 1, 2, 3, 4], [0xe8, 5, 6, 7, 8])
}
unsafe fn copy(address: usize, _: &[u8; 5], bytes: &[u8; 5]) -> bool {
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, 5);
    true
}
#[test]
fn failed_restore_retains_record_then_recovers() {
    let mut bytes = [0xe8, 5, 6, 7, 8];
    let mut undo = vec![patch(&mut bytes)];
    unsafe {
        assert!(!restore_calls_with(&mut undo, |_, _, _| false, |_| true));
        assert_eq!(undo.len(), 1);
        assert_eq!(bytes, [0xe8, 5, 6, 7, 8]);
        assert!(restore_calls_with(&mut undo, |a, e, n| copy(a, e, n), |_| true));
    }
    assert!(undo.is_empty());
    assert_eq!(bytes, [0xe8, 1, 2, 3, 4]);
}
#[test]
fn foreign_site_is_retained_without_writes() {
    let mut bytes = [0x90, 5, 6, 7, 8];
    let mut undo = vec![patch(&mut bytes)];
    unsafe {
        assert!(!restore_calls_with(&mut undo, |_, _, _| panic!("foreign write"), |_| panic!("foreign flush")));
    }
    assert_eq!(undo.len(), 1);
    assert_eq!(bytes, [0x90, 5, 6, 7, 8]);
    bytes.copy_from_slice(&[0xe8, 5, 6, 7, 8]);
    unsafe { assert!(restore_calls_with(&mut undo, |a, e, n| copy(a, e, n), |_| true)); }
    assert_eq!(bytes, [0xe8, 1, 2, 3, 4]);
}
#[test]
fn original_bytes_require_successful_cache_sync_before_releasing_undo() {
    let mut bytes = [0xe8, 5, 6, 7, 8];
    let mut undo = vec![patch(&mut bytes)];
    unsafe {
        assert!(!restore_calls_with(&mut undo, |a, e, n| { copy(a, e, n); false }, |_| true));
        assert_eq!(bytes, [0xe8, 1, 2, 3, 4]);
        assert_eq!(undo.len(), 1);
        assert!(!restore_calls_with(&mut undo, |_, _, _| panic!("duplicate write"), |_| false));
        assert_eq!(undo.len(), 1);
        assert!(restore_calls_with(&mut undo, |_, _, _| panic!("duplicate write"), |_| true));
    }
    assert!(undo.is_empty());
}
#[test]
fn reverse_restore_continues_past_failed_and_unreadable_sites() {
    let mut a = [0xe8, 5, 6, 7, 8];
    let mut b = a;
    let mut c = a;
    let mut undo = vec![patch(&mut a), patch(&mut b), (0, [0; 5], [1; 5]), patch(&mut c)];
    let failed = undo[1].0;
    let order = vec![undo[3].0, undo[1].0, undo[0].0];
    let mut calls = Vec::new();
    unsafe {
        assert!(!restore_calls_with(&mut undo, |p, e, n| {
            calls.push(p);
            p != failed && copy(p, e, n)
        }, |_| true));
    }
    assert_eq!(calls, order);
    assert_eq!(undo.len(), 2);
    assert_eq!(undo[0].0, failed);
    assert_eq!(undo[1].0, 0);
    assert_eq!(a, [0xe8, 1, 2, 3, 4]);
    assert_eq!(b, [0xe8, 5, 6, 7, 8]);
    assert_eq!(c, [0xe8, 1, 2, 3, 4]);
}