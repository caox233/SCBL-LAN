use super::*;
use std::{
    ffi::c_void,
    sync::atomic::{AtomicU32, Ordering},
};
static ENDS: AtomicU32 = AtomicU32::new(0);
static STARTS: AtomicU32 = AtomicU32::new(0);
static BASE: AtomicU32 = AtomicU32::new(0);
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, k: u32) -> *mut c_void;
    fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
}
unsafe extern "thiscall" fn play(_: usize, label: *const u8) {
    let s = std::ffi::CStr::from_ptr(label as _).to_bytes();
    match s {
        b"DroneEnd" => {
            ENDS.fetch_add(1, Ordering::SeqCst);
        }
        b"DroneStart" => {
            STARTS.fetch_add(1, Ordering::SeqCst);
        }
        _ => panic!("foreign label"),
    }
}
unsafe extern "thiscall" fn base(_: usize, _: u32) {
    BASE.fetch_add(1, Ordering::SeqCst);
}
#[test]
fn all_mp_exit_causes_have_one_policy_without_battery_or_body_lifetime_dependency() {
    unsafe {
        let mut state = vec![0u32; 0x160 / 4];
        let mut owner = vec![0u32; 0x1400 / 4];
        let mut controller = vec![0u32; 0x100 / 4];
        let p = state.as_mut_ptr() as usize;
        state[0] = 1;
        owner[0] = 2;
        state[0x48 / 4] = owner.as_ptr() as u32;
        state[0x64 / 4] = controller.as_ptr() as u32;
        owner[0x938 / 4] = controller.as_ptr() as u32;
        controller[0x4c / 4] = 1 << 8;
        for cause in ["voluntary", "empty", "EMP", "bullet", "overload"] {
            assert_eq!(exit_mode(p, 0, 1, 2), 1);
            println!("C13_EXIT cause={cause} native_mode=1 full_cleanup=true");
        }
        for old in [1, 2, 99] {
            assert_eq!(exit_mode(p, old, 1, 2), old);
        }
        state[0x30 / 4] = 1;
        assert_eq!(exit_mode(p, 0, 1, 2), 0);
        state[0x30 / 4] = 0;
        controller[0x4c / 4] = 2 << 8;
        assert_eq!(exit_mode(p, 0, 1, 2), 0);
        controller[0x4c / 4] = 1 << 8;
        state[0] = 9;
        assert_eq!(exit_mode(p, 0, 1, 2), 0);
        state[0] = 1;
        owner[0x938 / 4] = 0;
        assert_eq!(exit_mode(p, 0, 1, 2), 0);
    }
}

#[test]
fn c17_default_presentation_keeps_native_mode_without_calling_dormant_overrides() {
    assert!(!OVERRIDE_NATIVE_PRESENTATION);
    for cause in ["voluntary", "empty", "EMP", "bullet", "overload"] {
        for native in [0, 1, 2, 99] {
            assert_eq!(transition_mode(native, || panic!("dormant override invoked")), native);
            // Disabled mode must not dereference even a missing state pointer.
            assert_eq!(unsafe { transout_mode(0, native) }, native);
            println!("C17_EXIT cause={cause} input={native} forwarded={native} dormant_override_called=false");
        }
    }
    // Default init installs no HudMode hook and needs no live game addresses.
    assert!(unsafe { init() });
    assert!(!READY.load(Ordering::Acquire));
}
#[test]
#[ignore = "retail native DroneEnd movie selector"]
fn native_drone_end_is_exactly_once_and_base_visibility_cleanup_still_runs() {
    unsafe {
        let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C13_FIXTURE").unwrap());
        let b = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40) as usize;
        assert_ne!(b, 0);
        let mut set = std::fs::read(root.join("hud_drone_end.bin")).unwrap();
        assert_eq!(set.len(), 105);
        for (off, label) in [
            (0x2c, b"DroneStart\0".as_slice()),
            (0x43, b"DroneEnd\0".as_slice()),
        ] {
            std::ptr::copy_nonoverlapping(
                label.as_ptr(),
                (b + 0x500 + off) as *mut u8,
                label.len(),
            );
            set[off..off + 4].copy_from_slice(&((b + 0x500 + off) as u32).to_le_bytes());
        }
        std::ptr::copy_nonoverlapping(set.as_ptr(), b as *mut u8, set.len());
        let mut code = std::fs::read(root.join("hud_drone_mode.bin")).unwrap();
        for (off, to) in [(0x15, b), (0x2a, b), (0x36, base as *const () as usize)] {
            code[off + 1..off + 5].copy_from_slice(
                &(to as u32)
                    .wrapping_sub((b + 0x100 + off + 5) as u32)
                    .to_le_bytes(),
            );
        }
        std::ptr::copy_nonoverlapping(code.as_ptr(), (b + 0x100) as *mut u8, code.len());
        let mut vt = vec![0u32; 0x200 / 4];
        vt[0x12c / 4] = play as *const () as u32;
        let mut hud = vec![0u32; 0x300 / 4];
        hud[0] = vt.as_ptr() as u32;
        let p = hud.as_mut_ptr() as usize;
        let setter: Mode = std::mem::transmute(b);
        let native: Mode = std::mem::transmute(b + 0x100);
        for phase in ["BASELINE", "MODIFIED", "ROLLBACK", "C17_CURRENT_NATIVE"] {
            for mode in [0x0100000a, 0x01000020] {
                ENDS.store(0, Ordering::SeqCst);
                BASE.store(0, Ordering::SeqCst);
                hud[0x2e4 / 4] = 0;
                for _ in 0..2 {
                    if phase == "MODIFIED" && closing(p, mode, vt.as_ptr() as u32) {
                        setter(p, 1);
                    }
                    native(p, mode);
                }
                let ends = ENDS.load(Ordering::SeqCst);
                assert_eq!(ends, u32::from(phase == "MODIFIED" || mode == 0x01000020));
                assert_eq!(BASE.load(Ordering::SeqCst), 2);
                if phase == "C17_CURRENT_NATIVE" {
                    assert!(!OVERRIDE_NATIVE_PRESENTATION);
                    println!("C17_HUD mode={mode:x} DroneEnd={ends} native_base_calls=2 override_installed=false");
                }
                println!("C13_UI {phase} mode={mode:x} DroneEnd={ends} native_base_calls=2");
            }
        }
        VirtualFree(b as _, 0, 0x8000);
    }
}
