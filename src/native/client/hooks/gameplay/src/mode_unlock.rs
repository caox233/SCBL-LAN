//! C27: remove only multiplayer Match minRank/maxRank gates.
//! Retail 802670 defaults 1..50; 84EE20 parses the same fields at +48/+4C.
//! Never spoof profile XP/rank, ownership, passport, party size or campaign flags.
use crate::equipment_native::{readable, signature, writable};
use sha2::{Digest, Sha256};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const CTOR: usize = 0x0080_2670;
const ATTRIBUTE: usize = 0x0084_ee20;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);

// player_capacity runs before this module and legitimately replaces the
// three-byte party-size comparison at 19A80E1. Validate the complete consumer
// in either exact known state; never mask arbitrary bytes or disable its guard.
fn selection_contract(bytes: &[u8]) -> bool {
    bytes.len() == 721 && matches!(format!("{:x}", Sha256::digest(bytes)).as_str(),
        "7388d1fad441c399063430c544ef872cf0275c8ddd28da1ae49d1fd02e765289"
        | "d59e584c6667b2528b21d0428fe44d66a83c6bdc31c112719c37e9212c187a45")
}
unsafe fn selection_supported() -> bool {
    readable(0x019a7f30, 721)
        && selection_contract(std::slice::from_raw_parts(0x019a7f30 as *const u8, 721))
}
type CtorFn = unsafe extern "thiscall" fn(usize, usize, u32, usize) -> usize;
// The registered Match attribute callback consumes three stack arguments (ret12).
// ECX is scratch, no implicit object; result is unused by native XML dispatch.
type AttributeFn = unsafe extern "stdcall" fn(usize, usize, usize);
static_detour! {
    static Ctor: unsafe extern "thiscall" fn(usize, usize, u32, usize) -> usize;
    static Attribute: unsafe extern "stdcall" fn(usize, usize, usize);
}
pub(super) unsafe fn project(record: usize) -> bool {
    let Some(min) = record.checked_add(0x48) else { return false; };
    if !writable(min, 8) { return false; }
    let min = min as *mut i32;
    let changed = min.read_unaligned() != 0 || min.add(1).read_unaligned() != i32::MAX;
    min.write_unaligned(0);
    min.add(1).write_unaligned(i32::MAX);
    changed
}
fn constructed_with(
    record: usize, registry: usize, id: u32, context: usize, enabled: bool,
    original: impl FnOnce(usize, usize, u32, usize) -> usize,
) -> usize {
    let result = original(record, registry, id, context);
    if enabled && result == record && result != 0 {
        unsafe { project(result); }
    }
    result
}
fn attribute_with(
    record: usize, key: usize, value: usize, enabled: bool,
    original: impl FnOnce(usize, usize, usize),
) {
    // Always let native parse exactly the original key/value first, once.
    original(record, key, value);
    if enabled && unsafe { project(record) } && LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
        tracing::info!("SCBL_MOD_MODE_UNLOCK min_rank=0 max_rank=2147483647 profile_unchanged=true passport_party_unchanged=true");
    }
}
fn constructed(record: usize, registry: usize, id: u32, context: usize) -> usize {
    constructed_with(record, registry, id, context, READY.load(Ordering::Acquire),
        |r, p, i, c| unsafe { Ctor.call(r, p, i, c) })
}
fn attribute(record: usize, key: usize, value: usize) {
    attribute_with(record, key, value, READY.load(Ordering::Acquire),
        |r, k, v| unsafe { Attribute.call(r, k, v) });
}
pub(super) unsafe fn init(world_raycast: usize) -> bool {
    if READY.load(Ordering::Acquire) { return true; }
    if world_raycast != 0x0058_9860
        || !signature(CTOR,212,"13f1fb82dc0cc1314ab7595bff0744d74f7e6760c2509908abca97279205c23c")
        || !signature(ATTRIBUTE,1119,"dd2a5a8ee8b70b1e76c0ab0e4cb52f1e943a5045310b143cbb858f5ffb03c9fc")
        || !signature(0x029bce08,28,"c66f816c38cc561b90259cf7d91d8a8242e0033183887a24bcaa62dedf714fc2")
        || !selection_supported()
    {
        tracing::warn!("SCBL_MOD_MODE_UNLOCK enabled=false reason=unsupported-native-contract");
        return false;
    }
    let ok = Ctor.initialize(std::mem::transmute::<usize,CtorFn>(CTOR),constructed).is_ok()
        && Attribute.initialize(std::mem::transmute::<usize,AttributeFn>(ATTRIBUTE),attribute).is_ok()
        && Ctor.enable().is_ok() && Attribute.enable().is_ok();
    if !ok {
        let _ = Attribute.disable();
        let _ = Ctor.disable();
        return false;
    }
    READY.store(true,Ordering::Release);
    tracing::info!("SCBL_MOD_MODE_UNLOCK enabled=true scope=native-mp-match-rank-only");
    true
}
pub(super) unsafe fn deinit() {
    READY.store(false,Ordering::Release);
    let _=Attribute.disable();
    let _=Ctor.disable();
}
#[cfg(test)]
#[path="mode_unlock_tests.rs"]
mod tests;
