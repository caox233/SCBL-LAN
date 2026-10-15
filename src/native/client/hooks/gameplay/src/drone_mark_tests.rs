//! Exact retail add/tick regression. No in-game process is written by this test.
use super::*;
use sha2::{Digest, Sha256};
use std::{ffi::c_void, path::PathBuf};
#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, kind: u32, protection: u32) -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, address: *const c_void, n: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}
type Add = unsafe extern "thiscall" fn(*mut u8, usize, usize, usize, f32, u8);
type Tick = unsafe extern "thiscall" fn(*mut u8, f32);
fn raw(name: &str, digest: &str) -> Vec<u8> {
    let p = PathBuf::from(std::env::var("SCBL_R58_C14_FIXTURE").unwrap());
    let bytes = std::fs::read(p.join(format!("{name}.bin"))).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), digest);
    bytes
}
unsafe fn memory() -> usize {
    let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40) as usize;
    assert_ne!(p, 0);
    p
}
unsafe extern "thiscall" fn clear_ui(p: *mut u8) {
    let flags = p.add(PLAYER_MARK_FLAGS_OFFSET).cast::<u32>();
    flags.write_unaligned(flags.read_unaligned() & !0x800);
}
#[test]
#[ignore = "exact retail add/tick ABI fixture"]
fn native_duplicate_refresh_and_expiry_are_not_blocked_by_any_intel_window() {
    unsafe {
        let at = memory();
        let mut add = raw(
            "mark_add",
            "cbad8c1d663db85eb7a28c9a151d3868b318fb14c9166fd57dae102dde6d6dfb",
        );
        // Native no-refresh compares to zero. Only relocate its read-only constant.
        let zero = at + 0x800;
        (zero as *mut u32).write(0);
        let offsets: Vec<_> = (0..add.len() - 3)
            .filter(|&i| add[i..i + 4] == 0x02993128u32.to_le_bytes())
            .collect();
        assert_eq!(offsets.len(), 1);
        add[offsets[0]..offsets[0] + 4].copy_from_slice(&(zero as u32).to_le_bytes());
        std::ptr::copy_nonoverlapping(add.as_ptr(), at as *mut u8, add.len());
        let native: Add = std::mem::transmute(at);
        // Actual product hook, not a policy reimplementation, calls these retail bytes.
        PlayerMarkAddHook
            .initialize(native, player_mark_add)
            .unwrap();
        let ta = memory();
        let mut tick = raw(
            "mark_tick",
            "821c82c73043b1924e89a121b79aa0b4c99f20f84fcb53758536707af8b7e1ec",
        );
        assert_eq!(tick[0xdb], 0xe8);
        tick[0xdc..0xe0].copy_from_slice(
            &(clear_ui as *const () as u32)
                .wrapping_sub((ta + 0xe0) as u32)
                .to_le_bytes(),
        );
        std::ptr::copy_nonoverlapping(tick.as_ptr(), ta as *mut u8, tick.len());
        FlushInstructionCache(GetCurrentProcess(), at as _, 4096);
        FlushInstructionCache(GetCurrentProcess(), ta as _, 4096);
        let tick: Tick = std::mem::transmute(ta);
        for intel in [false, true] {
            INTEL_ENABLED.store(intel, Ordering::Release);
            *INTEL_ACTIVE_ABILITIES.lock().unwrap() = if intel { vec![1] } else { vec![] };
            for vtable in [0x02ad3234, 0x02ad2abc] {
                let mut pawn = vec![0u32; 0x2000 / 4];
                let mut entries = vec![0u32; 8];
                pawn[0] = vtable;
                pawn[PLAYER_MARK_ENTRIES_OFFSET / 4] = entries.as_ptr() as u32;
                pawn[PLAYER_MARK_COUNT_OFFSET / 4] = 1;
                pawn[PLAYER_MARK_CAPACITY_OFFSET / 4] = 2;
                pawn[PLAYER_MARK_FLAGS_OFFSET / 4] = 0x800;
                entries[..4].copy_from_slice(&[(-1f32).to_bits(), 7, 9, 0]);
                let ptr = pawn.as_mut_ptr().cast::<u8>();
                player_mark_add(ptr, 7, 9, 0, 1., 1);
                assert_eq!(f32::from_bits(entries[0]), 1.);
                tick(ptr, 1.1);
                assert_eq!(pawn[PLAYER_MARK_COUNT_OFFSET / 4], 0);
                assert_eq!(pawn[PLAYER_MARK_FLAGS_OFFSET / 4] & 0x800, 0);
                println!("C14_MARK MODIFIED intel={intel} target={vtable:x} indefinite_to_finite=1 expired=true");
                // Restore same input, check native no-refresh and overwrite semantics.
                pawn[PLAYER_MARK_COUNT_OFFSET / 4] = 1;
                pawn[PLAYER_MARK_FLAGS_OFFSET / 4] = 0x800;
                entries[..4].copy_from_slice(&[5f32.to_bits(), 7, 9, 0]);
                player_mark_add(ptr, 7, 9, 0, 2., 0);
                assert_eq!(f32::from_bits(entries[0]), 5.);
                player_mark_add(ptr, 7, 9, 0, 8., 0);
                assert_eq!(f32::from_bits(entries[0]), 8.);
                player_mark_add(ptr, 7, 9, 0, 2., 1);
                assert_eq!(f32::from_bits(entries[0]), 2.);
                // Another source survives this source's expiry, no global HUD clear.
                entries[4..].copy_from_slice(&[20f32.to_bits(), 11, 12, 0]);
                pawn[PLAYER_MARK_COUNT_OFFSET / 4] = 2;
                tick(ptr, 2.1);
                assert_eq!(pawn[PLAYER_MARK_COUNT_OFFSET / 4], 1);
                assert_eq!(&entries[1..3], &[11, 12]);
                assert_eq!(pawn[PLAYER_MARK_FLAGS_OFFSET / 4] & 0x800, 0x800);
            }
        }
        INTEL_ACTIVE_ABILITIES.lock().unwrap().clear();
        INTEL_ENABLED.store(false, Ordering::Release);
        // Keep allocated native code alive for the static detour until test process exits.
    }
}
