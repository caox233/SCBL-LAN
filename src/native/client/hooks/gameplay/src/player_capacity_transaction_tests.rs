use super::*;
fn patch(bytes: &mut [u8]) -> AppliedPatch {
    AppliedPatch { address: bytes.as_mut_ptr() as usize,
        original: vec![0x83, 0xf8, 1], replacement: vec![0x83, 0xf8, 12] }
}
unsafe fn copy(address: usize, bytes: &[u8]) -> bool {
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len()); true
}
#[test]
fn restore_failure_keeps_exact_undo_and_retry_succeeds() {
    let mut bytes=[0x83,0xf8,12];let mut undo=vec![patch(&mut bytes)];
    unsafe {
        assert!(!restore_patches_with(&mut undo,|_,_|false,|_,_|true));
        assert_eq!(undo.len(),1);assert_eq!(bytes,[0x83,0xf8,12]);
        assert!(restore_patches_with(&mut undo,|a,b|copy(a,b),|_,_|true));
    }
    assert!(undo.is_empty());assert_eq!(bytes,[0x83,0xf8,1]);
}
#[test]
fn foreign_bytes_are_not_written_and_do_not_lose_undo() {
    let mut bytes=[0x90,0xf8,12];let mut undo=vec![patch(&mut bytes)];
    unsafe{assert!(!restore_patches_with(&mut undo,|_,_|panic!("foreign write"),|_,_|panic!("foreign flush")));}
    assert_eq!(undo.len(),1);assert_eq!(bytes,[0x90,0xf8,12]);
    bytes.copy_from_slice(&[0x83,0xf8,1]);
    unsafe{assert!(restore_patches_with(&mut undo,|_,_|panic!("already original"),|_,_|true));}
    assert!(undo.is_empty());
}
#[test]
fn failed_cache_sync_retains_original_bytes_until_confirmed() {
    let mut bytes=[0x83,0xf8,12];let mut undo=vec![patch(&mut bytes)];
    unsafe {
        assert!(!restore_patches_with(&mut undo,|a,b|{copy(a,b);false},|_,_|true));
        assert_eq!(bytes,[0x83,0xf8,1]);assert_eq!(undo.len(),1);
        assert!(!restore_patches_with(&mut undo,|_,_|panic!("unneeded copy"),|_,_|false));
        assert_eq!(undo.len(),1);
        assert!(restore_patches_with(&mut undo,|_,_|panic!("unneeded copy"),|_,_|true));
    }
}
#[test]
fn restore_continues_reverse_order_past_one_failed_site() {
    let mut a=[0x83,0xf8,12];let mut b=a;let mut c=a;
    let mut undo=vec![patch(&mut a),patch(&mut b),patch(&mut c)];
    let failed=undo[1].address;let order:Vec<_>=undo.iter().rev().map(|p|p.address).collect();let mut calls=Vec::new();
    unsafe { assert!(!restore_patches_with(&mut undo,|p,bytes|{calls.push(p);p!=failed&&copy(p,bytes)},|_,_|true)); }
    assert_eq!(calls,order);assert_eq!(undo.len(),1);assert_eq!(undo[0].address,failed);
    assert_eq!(a,[0x83,0xf8,1]);assert_eq!(b,[0x83,0xf8,12]);assert_eq!(c,[0x83,0xf8,1]);
}
#[test]
fn actual_writer_writes_flushes_and_restores_exact_bytes() {
    let mut bytes: [u8; 3]=[0x83,0xf8,1];unsafe {
        assert!(write_bytes(bytes.as_mut_ptr() as usize,&[0x83,0xf8,12]));
        assert_eq!(bytes,[0x83,0xf8,12]);
        assert!(write_bytes(bytes.as_mut_ptr() as usize,&[0x83,0xf8,1]));
        assert!(flush_code(bytes.as_ptr() as usize,bytes.len()));
    }
    assert_eq!(bytes,[0x83,0xf8,1]);
}
