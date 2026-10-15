//! DX11 friendly Disruptor entry: keep Intel mark, HUD and score consistent.
//! BAD1A0 is shared by Spy and Merc. Only a marked Merc in the existing Intel
//! immunity window bypasses BAD1B3..BAD219. BAD21A's friendly protection and
//! native authority/network tail still run. Never intercept global mark clear.
use crate::equipment_native::{patch, readable, signature, word};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
const SITE: usize = 0x00bad1b3;
const TAIL: usize = 0x00bad21a;
const ORIGINAL: [u8; 10] = [0xf7, 0x86, 8, 0x1a, 0, 0, 0, 8, 0, 0];
static THUNK: AtomicUsize = AtomicUsize::new(0);
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, kind: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}
unsafe fn protected(pawn: usize, intel_active: bool) -> bool {
    if !intel_active
        || word(pawn, 0) != Some(0x02ad2abc)
        || !word(pawn, 0x1a08).is_some_and(|f| f & 0x800 != 0)
    {
        return false;
    }
    let (Some(p), Some(n), Some(cap)) =
        (word(pawn, 0x1a90), word(pawn, 0x1a94), word(pawn, 0x1a98))
    else {
        return false;
    };
    n > 0 && n <= 256 && cap >= n && cap <= 4096 && readable(p as usize, n as usize * 16)
}
unsafe extern "system" fn keep_mark(pawn: usize) -> u32 {
    protected(pawn, crate::gameplay_features::intel_immunity_active()) as u32
}
fn jump(site: usize, to: usize) -> [u8; 10] {
    let mut bytes = [0x90; 10];
    bytes[0] = 0xe9;
    bytes[1..5].copy_from_slice(&(to as u32).wrapping_sub(site as u32 + 5).to_le_bytes());
    bytes
}
fn bridge(base: u32, callback: u32, native: u32, tail: u32) -> Vec<u8> {
    // Save flags/GPR/FX and pass the native receiver (ESI), not the caster.
    let mut b = vec![
        0x9c, 0x60, 0x89, 0xe0, 0x81, 0xec, 0x20, 2, 0, 0, 0x83, 0xe4, 0xf0, 0x89, 0x84, 0x24, 0,
        2, 0, 0, 0x0f, 0xae, 4, 0x24, 0xfc, 0x56, 0xb8,
    ];
    b.extend(callback.to_le_bytes());
    b.extend([0xff, 0xd0, 0x85, 0xc0, 0x0f, 0x85]);
    let branch = b.len();
    b.extend([0; 4]);
    let restore = [
        0x0f, 0xae, 0x0c, 0x24, 0x8b, 0xa4, 0x24, 0, 2, 0, 0, 0x61, 0x9d,
    ];
    b.extend(restore);
    b.extend(ORIGINAL);
    b.push(0xe9);
    b.extend(native.wrapping_sub(base + b.len() as u32 + 4).to_le_bytes());
    let displacement = (b.len() as u32).wrapping_sub(branch as u32 + 4);
    b[branch..branch + 4].copy_from_slice(&displacement.to_le_bytes());
    b.extend(restore);
    b.push(0xe9);
    b.extend(tail.wrapping_sub(base + b.len() as u32 + 4).to_le_bytes());
    b
}
pub(super) unsafe fn init() -> bool {
    if !signature(
        0xbad1a0,
        194,
        "3253d990f7190a4e4c72acc7c62352196bc743cd80fef83992bca35f510bc835",
    ) {
        tracing::error!("SCBL_MOD_INTEL_TEAM enabled=false reason=native-signature");
        return false;
    }
    let p = VirtualAlloc(std::ptr::null_mut(), 256, 0x3000, 4);
    if p.is_null() {
        return false;
    }
    let b = bridge(
        p as u32,
        keep_mark as *const () as u32,
        (SITE + 10) as u32,
        TAIL as u32,
    );
    std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len());
    let mut old = 0;
    if VirtualProtect(p, 256, 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), p, b.len()) == 0
    {
        let _ = VirtualFree(p, 0, 0x8000); // no branch can reach this buffer yet
        return false;
    }
    // patch() can fail after publishing bytes if its compensating restore fails.
    // Record ownership first so deinit can restore the site; keep published code
    // mapped for any in-flight branch, including on failed installation.
    THUNK.store(p as usize, Ordering::Release);
    if !patch(SITE, &ORIGINAL, &jump(SITE, p as usize)) {
        deinit();
        return false;
    }
    tracing::info!(
        "SCBL_MOD_INTEL_TEAM enabled=true mark_hud_score=preserved friendly_disruption=native"
    );
    true
}
pub(super) unsafe fn deinit() {
    let p = THUNK.load(Ordering::Acquire);
    if p != 0 {
        let _ = patch(SITE, &jump(SITE, p), &ORIGINAL);
    }
    // Published code retained until exit, as with other native host bridges.
}
#[cfg(test)]
pub(super) unsafe fn fixture_patch(relocation: usize, active: bool) {
    unsafe extern "system" fn yes(pawn: usize) -> u32 {
        protected(pawn, true) as u32
    }
    unsafe extern "system" fn no(pawn: usize) -> u32 {
        protected(pawn, false) as u32
    }
    let p = VirtualAlloc(std::ptr::null_mut(), 256, 0x3000, 0x40);
    assert!(!p.is_null());
    let callback = if active {
        yes as *const () as u32
    } else {
        no as *const () as u32
    };
    let site = SITE.wrapping_add(relocation);
    let b = bridge(
        p as u32,
        callback,
        (site + 10) as u32,
        TAIL.wrapping_add(relocation) as u32,
    );
    std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len());
    assert!(patch(site, &ORIGINAL, &jump(site, p as usize)));
}
