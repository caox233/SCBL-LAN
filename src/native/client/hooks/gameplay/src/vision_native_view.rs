//! Sole physical owner of native vision view 0175B4E0.
//! Thermal target restoration and FIX8/diagnostic guards compose around one call.
//! No mode expansion, persistent native state, payload policy or rendering change.
use crate::equipment_native::signature;
use retour::GenericDetour;
use std::sync::{OnceLock, atomic::{AtomicBool, Ordering}};

const VIEW_VA: usize = 0x175b4e0;
const VIEW_BYTES: usize = 0x1413;
const VIEW_SHA256: &str = "6a00377514208193ee8e399d004556c06754324e1fbf2302a870dbd84736f281";
type ViewFn = unsafe extern "thiscall" fn(usize);
static VIEW: OnceLock<GenericDetour<ViewFn>> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);

unsafe extern "thiscall" fn draw(pass: usize) {
    crate::vision_transparency::with_native_view(pass, || {
        crate::thermal_targets::with_ready_targets(pass, || {
            VIEW.get().expect("published before enable").call(pass);
        });
    });
}
unsafe fn fallback(reason: &'static str) {
    // Restrict failure to the clients that require this view boundary.
    // Smoke/range and the existing classifier/material features stay independent.
    crate::thermal_targets::deinit();
    crate::vision_transparency::deinit_view_clients();
    tracing::warn!("SCBL_SHARED_VISION_VIEW enabled=false reason={} native_fallback=true thermal_smoke_and_range_untouched=true", reason);
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860 || READY.load(Ordering::Acquire) { return; }
    // Never accept a jump, relax the expected bytes, or borrow another hook's trampoline.
    if !signature(VIEW_VA, VIEW_BYTES, VIEW_SHA256) {
        fallback("native-signature"); return;
    }
    if VIEW.get().is_some() { fallback("already-published"); return; }
    let Ok(hook) = GenericDetour::new(std::mem::transmute::<usize, ViewFn>(VIEW_VA), draw as ViewFn) else {
        fallback("detour-create"); return;
    };
    if VIEW.set(hook).is_err() { fallback("detour-publish"); return; }
    if VIEW.get().expect("published before enable").enable().is_err() {
        if let Some(hook) = VIEW.get() { let _ = hook.disable(); }
        fallback("detour-enable"); return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_SHARED_VISION_VIEW enabled=true owner=single native=0x175b4e0 thermal_mode=8 reuse_mode=9 runtime_acceptance=pending");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if let Some(hook) = VIEW.get() { let _ = hook.disable(); }
}
