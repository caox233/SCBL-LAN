//! C50: Intel RF follows accepted, still-present player marks, not cast duration.
//! The old disruption clock remains available for builds without the private
//! feedback bridge. A release enabling this adapter must have both ends updated.
use crate::equipment_native::{armor, signature, word};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static READY: AtomicBool = AtomicBool::new(false);
static FILTER_LOGS: AtomicU32 = AtomicU32::new(0);
static_detour! { static Clothing:unsafe extern "thiscall" fn(usize)->u8; }
pub(crate) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}
pub(crate) unsafe fn intel(pawn: usize) -> bool {
    word(pawn, 0) == Some(0x2ad3234)
        && armor(pawn).and_then(|a| word(a, 0x5c)).is_some_and(|a| {
            word(a as usize, 0) == Some(0x2a3f2dc) && word(a as usize, 0x6c) == Some(pawn as u32)
        })
}
pub(crate) unsafe fn active(pawn: usize) -> Option<bool> {
    if !ready() || !intel(pawn) {
        return None;
    }
    if word(pawn, 0x60).is_some_and(|f| f & 0x800 != 0) {
        return Some(false);
    }
    if let Some(v) = crate::intel_mark_lifetime::actual_targets(pawn) {
        return Some(v);
    }
    #[cfg(feature = "goggle-feedback")]
    {
        return crate::goggle_feedback_native::intel_remote(pawn);
    }
    #[cfg(not(feature = "goggle-feedback"))]
    None
}
pub(crate) unsafe fn publish(pawn: usize) {
    if !ready() || !intel(pawn) {
        return;
    }
    #[cfg(feature = "goggle-feedback")]
    if let Some(active) = crate::intel_mark_lifetime::actual_targets(pawn) {
        crate::goggle_feedback_native::intel_publish(pawn, active);
    }
}
fn visible_reason(native: bool, targets: Option<bool>, goggles: bool) -> bool {
    native && (targets != Some(false) || goggles)
}
/// The replicated reason-1 aggregate can outlive the reason-2 clothing bit.
/// Consume the verified source result at the existing per-entry visibility gate
/// as well. Do NOT write the aggregate bit or erase native goggles emission.
pub(crate) unsafe fn filter_visibility(pawn: usize, native: bool) -> bool {
    let targets = active(pawn);
    if targets != Some(false) {
        return native;
    }
    let (Some(a), Some(rf)) = (armor(pawn), word(pawn, 0x944)) else {
        return native;
    };
    let Some(flags) = word(a, 0x7c) else {
        return native;
    };
    let goggles =
        std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize) -> u32>(0xd75d30)(pawn)
            != 0;
    let ordinary = crate::rfd_rules::ordinary_goggles(goggles, rf, flags & 8 != 0);
    let visible = visible_reason(native, targets, ordinary);
    if native && !visible && FILTER_LOGS.fetch_add(1, Ordering::Relaxed) < 6 {
        tracing::info!("SCBL_MOD_INTEL_RF revision=C51 no_live_targets=true stale_entry_filtered=true ordinary_goggles=false");
    }
    visible
}
fn clothing(pawn: usize) -> u8 {
    unsafe {
        active(pawn)
            .map(u8::from)
            .unwrap_or_else(|| Clothing.call(pawn))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_aggregate_is_not_a_second_intel_source() {
        assert!(!visible_reason(true, Some(false), false));
        assert!(visible_reason(true, Some(true), false));
        assert!(visible_reason(true, Some(false), true));
        assert!(visible_reason(true, None, false));
        assert!(
            !visible_reason(false, Some(true), false),
            "native range/death veto stays false"
        );
    }
}
pub(crate) unsafe fn init(world: usize) {
    #[cfg(feature = "goggle-feedback")]
    if world == 0x589860
        && crate::intel_mark_lifetime::ready()
        && crate::rfd_exposure::ready()
        && crate::goggle_feedback_native::ready()
        && signature(
            0xd32a60,
            0x36,
            "5ffe9290ae3d1e75bcb86ea52d5af6ae08e97f0cb5ac27a43718ad44250fc85b",
        )
    {
        if Clothing
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize) -> u8>(0xd32a60),
                clothing,
            )
            .and_then(|h| h.enable())
            .is_ok()
        {
            READY.store(true, Ordering::Release);
            tracing::info!("SCBL_MOD_INTEL_RF revision=C51 actual_player_marks=true mark_tick_reconcile=true visibility_filter=true other_rf_preserved=true live_verified=false");
        }
    }
    let _ = world;
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Clothing.disable();
}
