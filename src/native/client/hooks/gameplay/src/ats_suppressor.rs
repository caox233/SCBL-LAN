//! ATS-only suppressor correction. No global weapon getter hook or state writes.
//! A positive installed attachment may override native false while the remote
//! firing logic is absent. Native true and all unknown facts remain unchanged.
use crate::equipment_native::{patch, signature, word};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
const SITE: usize = 0xb105c2;
const ORIGINAL: [u8; 5] = [0xe8, 0xc9, 0x88, 0x2f, 0];
static INSTALLED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);

pub(crate) unsafe fn repair_result(weapon: usize, native: u8, info: Option<usize>, name: Option<u32>) -> u8 {
    if native != 0 {
        return native;
    }
    match (info, name) {
        (Some(info), Some(name)) if crate::ats_native_source::suppression_for_ats(weapon, info, name) == Some(true) => 1,
        _ => native,
    }
}
fn call_bytes(site: usize, target: usize) -> [u8; 5] {
    let mut bytes = [0xe8, 0, 0, 0, 0];
    bytes[1..].copy_from_slice(&(target as u32).wrapping_sub(site as u32 + 5).to_le_bytes());
    bytes
}
unsafe extern "thiscall" fn classify(weapon: usize) -> u8 {
    let original: unsafe extern "thiscall" fn(usize) -> u8 = std::mem::transmute(0xe08e90usize);
    let native = original(weapon); // exactly once; other game callers never enter this adapter
    if native != 0 || !READY.load(Ordering::Acquire) {
        return native;
    }
    let result = repair_result(weapon, native, word(0x339e5a0, 0).map(|p| p as usize), word(0x33a039c, 0));
    if result != native && LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
        tracing::info!("SCBL_MOD_ATS_SUPPRESSOR corrected=true source=installed-attachment native=false logic_missing={} live_acceptance=pending", word(weapon, 0x748) == Some(0));
    }
    result
}
pub(crate) unsafe fn init(world: usize) {
    if INSTALLED.load(Ordering::Acquire) {
        return;
    }
    if world != 0x589860
        || !signature(0xb105a0, 110, "d49bd282485b2feffe6a76c7932f22d0d8c5e2721ce2c81d953c15a0fc9fc81a")
        || !signature(0xe08e90, 18, "3f27ba87598b5f3653f956d322e3be51bd81a514f2d04811bcdd2d3a97c008df")
        || !signature(0xdfffd0, 99, "a1da17de7f25336c79f2151f0ac3ca2f1734fbbd832caaa1e9cfa2e2d4ecaa8c")
        || !patch(SITE, &ORIGINAL, &call_bytes(SITE, classify as usize))
    {
        tracing::warn!("SCBL_MOD_ATS_SUPPRESSOR enabled=false reason=signature-or-callsite");
        return;
    }
    INSTALLED.store(true, Ordering::Release);
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_ATS_SUPPRESSOR enabled=true scope=ats-callsite-only installed-facts=true live_acceptance=pending");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if INSTALLED.load(Ordering::Acquire) && patch(SITE, &call_bytes(SITE, classify as usize), &ORIGINAL) {
        INSTALLED.store(false, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_true_or_unavailable_scope_never_changes_classification() {
        unsafe {
            for original in [0, 1, 2, 255] {
                assert_eq!(repair_result(0, original, None, None), original);
                assert_eq!(repair_result(0, original, Some(0), Some(0)), original);
            }
        }
    }
    #[test]
    fn callsite_rel32_reconstructs_original_and_wraps_on_i686() {
        assert_eq!(call_bytes(SITE, 0xe08e90), ORIGINAL);
        let p=call_bytes(0xf0000000, 0x10000000);
        assert_eq!(0xf0000005u32.wrapping_add(u32::from_le_bytes(p[1..].try_into().unwrap())),0x10000000);
    }
}
