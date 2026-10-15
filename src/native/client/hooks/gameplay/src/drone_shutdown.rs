//! Dormant unified MP exit presentation; C17 uses native timing/movie selection.
//! One native MP exit policy for voluntary, empty battery, EMP and destruction.
//! Use the real HudMPDrone DroneEnd movie label, not a simulated overlay or a
//! pre-emptive global HUD mode. Native movie completion/cleanup remain owners.
use crate::equipment_native::{signature, word};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, Ordering};
static READY: AtomicBool = AtomicBool::new(false);
const HUD: u32 = 0x02c3ccac;
pub(super) const OVERRIDE_NATIVE_PRESENTATION: bool = false;

// Keep both former overrides behind one gate. Disabling only HudMode would
// still let ActionFlow force TransOut(1) and skip the original animation wait.
pub(super) fn transition_mode(native: u32, overridden: impl FnOnce() -> u32) -> u32 {
    if OVERRIDE_NATIVE_PRESENTATION {
        overridden()
    } else {
        native
    }
}
type Mode = unsafe extern "thiscall" fn(usize, u32);
static_detour! {static HudMode:unsafe extern "thiscall" fn(usize,u32);}
unsafe fn exit_mode(state: usize, native: u32, state_vt: u32, owner_vt: u32) -> u32 {
    if native != 0
        || word(state, 0) != Some(state_vt)
        || word(state, 0x30).is_none_or(|x| x & 1 != 0)
    {
        return native;
    }
    let owner = word(state, 0x48).unwrap_or(0) as usize;
    let controller = word(state, 0x64).unwrap_or(0) as usize;
    if word(owner, 0) == Some(owner_vt)
        && controller != 0
        && word(owner, 0x938) == Some(controller as u32)
        && word(controller, 0x4c).is_some_and(|x| matches!((x >> 8) & 255, 1 | 3))
    {
        1
    } else {
        native
    }
}
pub(super) unsafe fn transout_mode(state: usize, native: u32) -> u32 {
    if !OVERRIDE_NATIVE_PRESENTATION || !READY.load(Ordering::Acquire) {
        return native;
    }
    exit_mode(state, native, 0x02bebe74, 0x02ad2abc)
}
unsafe fn closing(hud: usize, mode: u32, vt: u32) -> bool {
    word(hud, 0) == Some(vt)
        && matches!(mode, 0x0100000a | 0x01000020)
        && word(hud, 0x2e4) == Some(0)
}
fn mode(hud: usize, value: u32) {
    unsafe {
        if OVERRIDE_NATIVE_PRESENTATION && READY.load(Ordering::Acquire) && closing(hud, value, HUD) {
            let end: Mode = std::mem::transmute(0x1a44af0usize);
            end(hud, 1);
            tracing::debug!(
                "SCBL_MOD_DRONE_SHUTDOWN native_label=DroneEnd mode={}",
                value
            );
        }
        HudMode.call(hud, value);
    }
}
pub(super) unsafe fn init() -> bool {
    if !OVERRIDE_NATIVE_PRESENTATION {
        tracing::info!("SCBL_MOD_DRONE_SHUTDOWN policy=native override_hook=false native_timing=true");
        return true;
    }
    if READY.load(Ordering::Acquire) {
        return true;
    }
    if !signature(
        0x1a4bd00,
        65,
        "019423126606ceb7a82cf40b2d897e1abc839c8bf33e5cf1d4da1f172ac3c301",
    ) || !signature(
        0x1a44af0,
        105,
        "3ce7e613563aecda3b50c26f7e6a73878557ba956b9793227a2cd7eecc74dd0d",
    ) || word(HUD as usize, 0x178) != Some(0x1a4bd00)
    {
        return false;
    }
    if HudMode
        .initialize(std::mem::transmute::<usize, Mode>(0x1a4bd00), mode)
        .is_err()
        || HudMode.enable().is_err()
    {
        let _ = HudMode.disable();
        return false;
    }
    READY.store(true, Ordering::Release);
    true
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = HudMode.disable();
}
#[cfg(test)]
#[path = "drone_shutdown_tests.rs"]
mod tests;
