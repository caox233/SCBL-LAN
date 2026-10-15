use super::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static CLEARS: AtomicU32 = AtomicU32::new(0);
static ENDS: AtomicU32 = AtomicU32::new(0);
static DAMAGE: AtomicU32 = AtomicU32::new(0);
static BOOMS: AtomicU32 = AtomicU32::new(0);
static RPC: AtomicU32 = AtomicU32::new(0);
static FALL_FX: AtomicU32 = AtomicU32::new(0);
static BOOM_FX: AtomicU32 = AtomicU32::new(0);
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, k: u32, prot: u32) -> *mut c_void;
    fn VirtualFree(p: *mut c_void, n: usize, k: u32) -> i32;
}
unsafe extern "thiscall" fn clear(_: usize) {
    CLEARS.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "thiscall" fn end(_: usize) {
    ENDS.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "thiscall" fn damage(_: usize, _: usize, _: u32, _: u32) {
    DAMAGE.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "thiscall" fn rpc(_: usize) {
    RPC.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "thiscall" fn audio(_: usize, id: u32, _: u32, _: f32, _: u32, _: u32) {
    if id == 99 {
        BOOMS.fetch_add(1, Ordering::Relaxed);
    }
}
unsafe extern "thiscall" fn sound(_: usize, _: u32, _: f32, _: u32) {
    BOOMS.fetch_add(1, Ordering::Relaxed);
}
unsafe extern "thiscall" fn spawn(
    _: usize,
    id: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
) -> usize {
    if id == 11 {
        FALL_FX.fetch_add(1, Ordering::Relaxed);
    } else if id == 12 {
        BOOM_FX.fetch_add(1, Ordering::Relaxed);
    }
    0
}
unsafe fn put(p: usize, b: &[u8]) {
    std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len());
}
unsafe fn jump(p: usize, d: usize) {
    let mut x = vec![0xe9];
    x.extend((d as u32).wrapping_sub(p as u32 + 5).to_le_bytes());
    put(p, &x);
}
fn reset() {
    for v in [&CLEARS, &ENDS, &DAMAGE, &BOOMS, &RPC, &FALL_FX, &BOOM_FX] {
        v.store(0, Ordering::Relaxed);
    }
}
fn counts() -> [u32; 7] {
    [&CLEARS, &ENDS, &DAMAGE, &BOOMS, &RPC, &FALL_FX, &BOOM_FX].map(|v| v.load(Ordering::Relaxed))
}

#[test]
#[ignore = "exact retail windows required"]
fn native_fall_and_terminal_cleanup_baseline_modified_rollback() {
    let _guard = LOCK.lock().unwrap();
    let dir = std::path::PathBuf::from(std::env::var("SCBL_R58_FALL_FIXTURE").unwrap());
    unsafe {
        let base = VirtualAlloc(std::ptr::null_mut(), 0x3000000, 0x3000, 0x40) as usize;
        assert_ne!(base, 0);
        let delta = base.wrapping_sub(0x400000);
        let a = |v: usize| v.wrapping_add(delta);
        for (name, va, n, relocs) in [
            ("explode", 0xad3920, 511, &[222usize][..]),
            ("fall", 0xb0f3b0, 579, &[161, 508][..]),
            ("terminal", 0xe97010, 386, &[7, 282][..]),
        ] {
            let mut code = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
            assert_eq!(code.len(), n);
            for &i in relocs {
                let v = u32::from_le_bytes(code[i..i + 4].try_into().unwrap());
                code[i..i + 4].copy_from_slice(&(a(v as usize) as u32).to_le_bytes());
            }
            put(a(va), &code);
        }
        *(a(0x2d75c00) as *mut f32) = 1.;
        *(a(0x2998550) as *mut f32) = 0.;
        for (va, target) in [
            (0xaf5f60, clear as *const () as usize),
            (0xe3e210, damage as *const () as usize),
            (0x6843d0, sound as *const () as usize),
        ] {
            jump(a(va), target);
        }
        for (va, code) in [
            (0xe8ca30, &[0x31, 0xc0, 0xc3][..]),
            (0xde29b0, &[0x31, 0xc0, 0xc3][..]),
            (0xe66590, &[0x31, 0xc0, 0xc2, 8, 0][..]),
            (0xaa6a70, &[0xc3][..]),
            (0xe69330, &[0xd9, 0xe8, 0xc3][..]),
            (0x8f1340, &[0xc2, 8, 0][..]),
        ] {
            put(a(va), code);
        }
        jump(a(0xfe0cb0), rpc as *const () as usize);
        let vt = a(DRONE as usize);
        let mut world = vec![0u32; 4];
        let mut wvt = vec![0u32; 0xa0 / 4];
        world[0] = wvt.as_mut_ptr() as u32;
        wvt[0x9c / 4] = spawn as *const () as u32;
        put(base + 0x100, &[0xc2, 8, 0]);
        *((vt + 0x2cc) as *mut u32) = (base + 0x100) as u32;
        *((vt + 0x654) as *mut u32) = (base + 0x100) as u32;
        *((vt + 0x368) as *mut u32) = audio as *const () as u32;
        *((vt + 0x5e8) as *mut u32) = end as *const () as u32;
        let native: End = std::mem::transmute(a(0xad3920));
        let fall: Fall = std::mem::transmute(a(0xb0f3b0));
        let physics = vec![0u32; 0x40 / 4];
        let mut body = vec![0u32; 0xc40 / 4];
        for role in [1u32, 2, 3] {
            for explosive_fx in [false, true] {
                for initial_state in [1u32, 2] {
                    for phase in ["BASELINE", "MODIFIED", "ROLLBACK", "C17_CURRENT_NATIVE"] {
                        body.fill(0);
                        body[0] = vt as u32;
                        body[0x4c / 4] = role << 8;
                        body[0x910 / 4] = initial_state;
                        body[0x7c4 / 4] = 0x2400;
                        body[0xbc0 / 4] = if explosive_fx { 4 } else { 0 };
                        body[0x148 / 4] = world.as_ptr() as u32;
                        body[0x610 / 4] = physics.as_ptr() as u32;
                        body[0xb5c / 4] = 11;
                        body[0xb64 / 4] = 12;
                        body[0xb80 / 4] = 99;
                        body[0xba4 / 4] = 99;
                        body[0x960 / 4] = 2f32.to_bits();
                        body[0x964 / 4] = 3f32.to_bits();
                        body[0x968 / 4] = 4f32.to_bits();
                        body[0x2c0 / 4] = 0x1234;
                        reset();
                        let p = body.as_mut_ptr() as usize;
                        if phase == "MODIFIED" {
                            if initial_state == 1 {
                                assert!(disable_parked_with(p, vt as u32, native, fall, rpc));
                                assert!(!disable_parked_with(p, vt as u32, native, fall, rpc));
                            } else {
                                assert_eq!(apply(p, 1, vt as u32, native, fall, rpc), Route::Fall);
                            }
                            assert_eq!(body[0x910 / 4] & 255, 3);
                            assert_eq!(body[0x9a8 / 4], 2f32.to_bits());
                            assert_eq!(body[0x9ac / 4], 3f32.to_bits());
                            assert_eq!(body[0x9b0 / 4], 4f32.to_bits());
                            assert_eq!(counts(), [1, 0, 0, 0, u32::from(role == 1), 1, 0]);
                            println!("R58_FALL {phase} role={role} explosive_fx={explosive_fx} first={:?} state=3 velocity=preserved",counts());
                            assert_eq!(body[0x7c4 / 4], 0x2000);
                            assert_eq!(body[0xbc0 / 4] & 4, 0);
                            // Collision / terminal notification may arrive later. It must destroy,
                            // not restart the falling animation or reacquire tag sources.
                            assert_eq!(apply(p, 0, vt as u32, native, fall, rpc), Route::Finish);
                            assert_eq!(counts(), [1, 1, 0, 0, u32::from(role == 1), 1, 0]);
                            println!(
                                "R58_FALL {phase} role={role} terminal={:?} repeated_fall=false",
                                counts()
                            );
                        } else {
                            if phase == "C17_CURRENT_NATIVE" {
                                assert!(!NONEXPLOSIVE_POLICY_ENABLED);
                            }
                            native(p, 1);
                            let result = counts();
                            assert_eq!(result[0], 0);
                            assert_eq!(result[1], 1);
                            assert_eq!(result[2], 1);
                            assert!(result[3] > 0);
                            assert_eq!(result[4], u32::from(role == 1));
                            assert_eq!(result[6], u32::from(explosive_fx));
                            if phase == "C17_CURRENT_NATIVE" {
                                println!("C17_END role={role} state={initial_state} explosive_fx={explosive_fx} counts={result:?} destruction=native damage_hook_enabled=false");
                            }
                            println!("R58_FALL {phase} role={role} explosive_fx={explosive_fx} result={result:?}");
                        }
                    }
                }
            }
        }
        // Foreign classes/roles/unknown states keep bytes and native routing.
        for (valid_vt, role, state) in [(false, 1, 2), (true, 0, 2), (true, 4, 2), (true, 1, 4)] {
            body[0] = if valid_vt { vt as u32 } else { 0xdeadbeef };
            body[0x4c / 4] = role << 8;
            body[0x910 / 4] = state;
            assert_eq!(route(body.as_ptr() as usize, vt as u32), Route::Native);
        }
        assert_eq!(route(0, vt as u32), Route::Native);
        assert_ne!(VirtualFree(base as _, 0, 0x8000), 0);
    }
}

#[test]
fn c17_parked_disable_delivers_native_fall_without_editing_effect_or_damage_flags() {
    use std::sync::atomic::AtomicUsize;
    static CALLS: AtomicU32 = AtomicU32::new(0);
    static PEER: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "thiscall" fn fall(p: usize) {
        // Simulate only the native endpoint's state transition; assert the bridge
        // never rewrites either explosive flag before dispatch.
        assert_eq!(*((p + 0xbc0) as *const u32), 4);
        assert_eq!(*((p + 0x7c4) as *const u32), 0x2400);
        CALLS.fetch_add(1, Ordering::SeqCst);
        *((p + 0x910) as *mut u32) = 3;
    }
    unsafe extern "thiscall" fn send(peer: usize) {
        PEER.store(peer, Ordering::SeqCst);
    }
    assert!(!NONEXPLOSIVE_POLICY_ENABLED);
    unsafe {
        for role in [1, 2, 3] {
            let mut body = vec![0u32; 0xc00 / 4];
            body[0] = DRONE;
            body[0x4c / 4] = role << 8;
            body[0x910 / 4] = 1;
            body[0xbc0 / 4] = 4;
            body[0x7c4 / 4] = 0x2400;
            body[0x2c0 / 4] = 0x1234;
            let p = body.as_mut_ptr() as usize;
            CALLS.store(0, Ordering::SeqCst);
            PEER.store(0, Ordering::SeqCst);
            assert!(disable_parked_native_with(p, DRONE, fall, send));
            assert!(!disable_parked_native_with(p, DRONE, fall, send));
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(PEER.load(Ordering::SeqCst), if role == 1 { 0x1234 } else { 0 });
            assert_eq!(body[0xbc0 / 4], 4);
            assert_eq!(body[0x7c4 / 4], 0x2400);
            println!("C17_PARKED role={role} native_fall=1 repeated_fall=0 fx_aoe_flags_unchanged=true");
        }
        assert!(!disable_parked_native_with(0, DRONE, fall, send));
    }
}
