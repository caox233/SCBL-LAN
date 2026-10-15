use super::*;
#[test]
fn core_scope_uses_local_pc_pawn_world_independent_of_role() {
    let mut pc = vec![0u32; 0x150 / 4];
    let mut pawn = vec![0u32; 0x378 / 4];
    let valid = |pc: &[u32], pawn: &[u32]| unsafe {
        super::super::raw_mouse_native::local_world_pair(
            pc.as_ptr() as usize,
            pawn.as_ptr() as usize,
        )
    };
    pc[0x144 / 4] = 0x20000;
    pawn[0x144 / 4] = 0x20000;
    for role in [0, 0x40000, 0x80000] {
        pawn[0x374 / 4] = role;
        assert!(valid(&pc, &pawn)); // campaign/co-op, spy and merc role fixtures
    }
    pawn[0x144 / 4] = 0x30000;
    assert!(!valid(&pc, &pawn)); // old/foreign world
    pawn[0x144 / 4] = 0;
    assert!(!valid(&pc, &pawn));
    pc[0x144 / 4] = 0;
    assert!(!valid(&pc, &pawn));
    unsafe {
        assert!(!super::super::raw_mouse_native::local_world_pair(0, 0));
    }
}
fn id() -> Identity {
    Identity {
        viewport: 1,
        window: 2,
        pawn: 3,
        mode: 4,
    }
}
fn active() -> Accumulator {
    let mut s = Accumulator::new();
    assert_eq!(s.frame(id(), true), None);
    s
}
#[test]
fn initial_native_fallback() {
    let s = Accumulator::new();
    assert!(!s.suppress_legacy(1, true));
}
#[test]
fn packet_partition_invariant() {
    for n in [1, 8, 125, 1000, 8000] {
        let mut s = active();
        for _ in 0..n {
            s.add(0, 8000 / n, -16000 / n);
        }
        assert_eq!(s.frame(id(), true), Some((8000.0, -16000.0, n as u32)));
    }
}
#[test]
fn no_extra_dt_or_gain() {
    let mut s = active();
    s.add(0, 47, -21);
    assert_eq!(s.frame(id(), true), Some((47.0, -21.0, 1)));
}
#[test]
fn consumed_once() {
    let mut s = active();
    s.add(0, 13, 8);
    s.frame(id(), true);
    assert_eq!(s.frame(id(), true), Some((0.0, 0.0, 0)));
}
#[test]
fn zero_and_sign_cancellation() {
    let mut s = active();
    s.add(0, 0, 0);
    s.add(0, 11, 8);
    s.add(8, -11, -8);
    assert_eq!(s.frame(id(), true), Some((0.0, 0.0, 2)));
}
#[test]
fn focus_loss_drops_pending() {
    let mut s = active();
    s.add(0, 15, 2);
    s.suspend();
    assert!(!s.suppress_legacy(1, true));
    assert_eq!(s.frame(id(), true), None);
    assert_eq!(s.frame(id(), true), Some((0.0, 0.0, 0)));
}
#[test]
fn menu_discards_pending() {
    let mut s = active();
    s.add(0, 500, 4);
    assert_eq!(s.frame(id(), false), None);
    assert_eq!(s.frame(id(), true), None);
    assert_eq!(s.frame(id(), true), Some((0.0, 0.0, 0)));
}
#[test]
fn inactive_never_accumulates() {
    let mut s = Accumulator::new();
    s.add(0, 777, 4);
    assert_eq!(s.frame(id(), true), None);
    assert_eq!(s.frame(id(), true), Some((0.0, 0.0, 0)));
}
#[test]
fn respawn_and_mode_change_drop_old_motion() {
    for k in 0..4 {
        let mut s = active();
        s.add(0, 777, 4);
        let mut i = id();
        match k {
            0 => i.pawn += 1,
            1 => i.mode += 1,
            2 => i.viewport += 1,
            _ => i.window += 1,
        };
        assert_eq!(s.frame(i, true), None);
        assert_eq!(s.frame(i, true), Some((0.0, 0.0, 0)));
    }
}
#[test]
fn legacy_scope() {
    let s = active();
    assert!(s.suppress_legacy(1, true));
    assert!(!s.suppress_legacy(8, true));
    assert!(!s.suppress_legacy(1, false));
}
#[test]
fn absolute_falls_back() {
    let mut s = active();
    s.add(1, 30000, 60000);
    assert!(!s.failed);
    assert!(s.recovering);
    assert!(!s.suppress_legacy(1, true));
    assert_eq!(s.frame(id(), true), None);
}
#[test]
fn excessive_input_falls_back_without_jump() {
    let mut s = active();
    s.add(0, i32::MAX, i32::MIN);
    assert!(s.failed);
    assert_eq!(s.frame(id(), true), None);
}
#[test]
fn cumulative_limit_falls_back() {
    let mut s = active();
    for _ in 0..9 {
        s.add(0, 1_000_000, 0);
    }
    assert!(s.failed);
    assert_eq!(s.frame(id(), true), None);
}
fn raw() -> [u8; 40] {
    let mut b = [0; 40];
    b[4..8].copy_from_slice(&40u32.to_le_bytes());
    b[28..32].copy_from_slice(&(-7i32).to_le_bytes());
    b[32..36].copy_from_slice(&19i32.to_le_bytes());
    b
}
#[test]
fn x86_raw_layout() {
    assert_eq!(decode_x86(&raw()), Some((0, -7, 19)));
}
#[test]
fn raw_nonmouse_rejected() {
    let mut b = raw();
    b[0] = 1;
    assert_eq!(decode_x86(&b), None);
}
#[test]
fn raw_short_and_bad_size_rejected() {
    let b = raw();
    assert_eq!(decode_x86(&b[..39]), None);
    for n in [0u32, 39, 41, u32::MAX] {
        let mut b = raw();
        b[4..8].copy_from_slice(&n.to_le_bytes());
        assert_eq!(decode_x86(&b), None);
    }
}
#[test]
fn buttons_not_interpreted_as_movement() {
    let mut b = raw();
    b[20..28].fill(0xff);
    assert_eq!(decode_x86(&b), Some((0, -7, 19)));
}
#[test]
fn absolute_layout_flag_preserved() {
    let mut b = raw();
    b[16] = 1;
    assert_eq!(decode_x86(&b), Some((1, -7, 19)));
}
#[test]
fn native_bridge_layout_preserves_displaced_cmp() {
    let b = super::super::raw_mouse_native::bridge_bytes(0x10000, 0x20000, 0x30000);
    assert!(b.windows(7).any(|v| v == [0x83, 0xbe, 0x4c, 8, 0, 0, 0]));
    assert_eq!(&b[..2], &[0x9c, 0x60]);
    let n = b.len();
    let rel = i32::from_le_bytes(b[n - 4..].try_into().unwrap());
    assert_eq!((0x10000i64 + n as i64 + rel as i64) as u32, 0x30000);
}

#[test]
fn native_bridge_executes_callback_and_original_cmp() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn callback(p: usize) {
        SEEN.store(p, Ordering::SeqCst);
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, k: u32) -> *mut c_void;
        fn VirtualProtect(p: *mut c_void, n: usize, k: u32, o: *mut u32) -> i32;
        fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    }
    unsafe {
        let memory = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4);
        assert!(!memory.is_null());
        let at = memory as usize;
        let bridge = super::super::raw_mouse_native::bridge_bytes(
            (at + 64) as u32,
            callback as *const () as u32,
            (at + 256) as u32,
        );
        // cdecl(viewport): preserve ESI; CALL bridge; restore ESI; RET.
        let mut caller = vec![0x56, 0x8b, 0x74, 0x24, 0x08, 0xe8];
        caller.extend((64i32 - 10).to_le_bytes());
        caller.extend([0x5e, 0xc3]);
        std::ptr::copy_nonoverlapping(caller.as_ptr(), memory as *mut u8, caller.len());
        std::ptr::copy_nonoverlapping(bridge.as_ptr(), (at + 64) as *mut u8, bridge.len());
        // Read flags left by the replayed original CMP.
        std::ptr::copy_nonoverlapping([0x9c, 0x58, 0xc3].as_ptr(), (at + 256) as *mut u8, 3);
        let mut old = 0;
        assert_ne!(VirtualProtect(memory, 4096, 0x20, &mut old), 0);
        assert_ne!(FlushInstructionCache(GetCurrentProcess(), memory, 4096), 0);
        let f: unsafe extern "cdecl" fn(usize) -> u32 = std::mem::transmute(at);
        let mut viewport = [0u32; 0x850 / 4];
        let p = viewport.as_mut_ptr() as usize;
        assert_ne!(f(p) & 0x40, 0);
        assert_eq!(SEEN.load(Ordering::SeqCst), p);
        viewport[0x84c / 4] = 1;
        assert_eq!(f(p) & 0x40, 0);
        assert_ne!(VirtualFree(memory, 0, 0x8000), 0);
    }
}

#[test]
fn absolute_recovery_does_not_replay_native_probe_packets() {
    let mut s = active(); s.add(0, 45, 3); s.add(1, 30000, 60000);
    assert!(s.wants_raw(1,true)); assert!(!s.suppress_legacy(1,true));
    s.add(0, 17, -6); assert_eq!(s.frame(id(),true),None);
    assert!(!s.active); assert_eq!(s.frame(id(),true),None); assert!(!s.active);
    assert_eq!(s.frame(id(),true),None); assert!(s.active);
    s.add(0, 11, -7); assert_eq!(s.frame(id(),true),Some((11.0,-7.0,1)));
}
#[test]
fn zero_relative_does_not_rearm_absolute_device() {
    let mut s=active();s.add(1,100,100);
    for _ in 0..10 { s.add(0,0,0); assert_eq!(s.frame(id(),true),None); assert!(!s.active); }
}
#[test]
fn repeated_mixed_absolute_input_stays_native() {
    let mut s=active();
    for _ in 0..100 { s.add(1,100,200);s.add(0,5,6);s.frame(id(),true);assert!(!s.suppress_legacy(1,true));assert!(!s.failed); }
    s.add(0,3,4);s.frame(id(),true);assert!(!s.active);s.frame(id(),true);assert!(s.active);
    assert_eq!(s.frame(id(),true),Some((0.0,0.0,0)));
}
#[test]
fn absolute_after_relative_probe_cancels_rearm() {
    let mut s=active();s.add(1,50,50);s.add(0,5,5);s.frame(id(),true);
    s.add(1,55,55);for _ in 0..3 {s.frame(id(),true);assert!(!s.active);}
}
#[test]
fn focus_menu_and_identity_change_cancel_recovery_probe() {
    for kind in 0..3 {
        let mut s=active();s.add(1,100,100);s.add(0,5,6);s.frame(id(),true);
        let mut next=id();
        match kind {0=>s.suspend(),1=>{s.frame(id(),false);},_=>{next.pawn+=1;s.frame(next,true);}}
        for _ in 0..3 {s.frame(next,true);assert!(!s.active);}
        s.add(0,8,8);s.frame(next,true);assert!(s.active);
        assert_eq!(s.frame(next,true),Some((0.0,0.0,0)));
    }
}
#[test]
fn recovery_probe_scope_and_terminal_overflow_are_separate() {
    let mut s=active();s.add(1,200,300);
    assert!(!s.wants_raw(9,true));assert!(!s.wants_raw(1,false));
    s.add(0,i32::MAX,1);assert!(s.failed);assert!(!s.wants_raw(1,true));
    s.add(0,4,5);for _ in 0..4 {assert_eq!(s.frame(id(),true),None);}
}

#[test]
fn alternating_frame_absolute_input_never_rearms() {
 let mut s=active();
 for _ in 0..100 {
  s.add(1,100,100);s.add(0,4,5);s.frame(id(),true);assert!(!s.active);
  s.add(0,4,5);s.frame(id(),true);assert!(!s.active);
 }
 s.frame(id(),true);assert!(s.active);assert_eq!(s.frame(id(),true),Some((0.0,0.0,0)));
}
