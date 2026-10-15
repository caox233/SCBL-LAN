//! The world-info +36C array contains all player controllers, not just this PC.
//! Retail 5863D0 -> 8B6970 -> 8C4ED0 selects the locally controlled entry.
//! Read-only, synchronous selection; multiple actual locals are ambiguous.
use crate::equipment_native::{readable, word};
unsafe fn ptr(p: usize, o: usize) -> Option<usize> {
    word(p, o).filter(|v| *v >= 0x10000).map(|v| v as usize)
}
pub(crate) unsafe fn controller(world: usize) -> Option<usize> {
    let wi = ptr(world, 0x6d0)?;
    let count = word(wi, 0x370)? as usize;
    let capacity = word(wi, 0x374)? as usize;
    if count == 0 || count > 32 || capacity < count || capacity > 128 { return None; }
    let array = ptr(wi, 0x36c)?;
    if !readable(array, count * 4) { return None; }
    let mut local = None;
    for i in 0..count {
        let pc = word(array, i * 4)? as usize;
        if pc == 0 { continue; }
        // Exact native 8C4ED0 predicate: local-control flag and not a proxy.
        if word(pc, 0x60)? & 0x40000 == 0 { continue; }
        let role = pc.checked_add(0x4d)?;
        if !readable(role, 1) { return None; }
        if (role as *const u8).read() == 2 { continue; }
        if ptr(pc, 0x144)? != world || local.is_some() { return None; }
        local = Some(pc);
    }
    local
}
#[cfg(test)]
#[path = "local_player_tests.rs"]
mod tests;
