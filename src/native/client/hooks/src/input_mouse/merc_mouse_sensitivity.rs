//! C71: restore the omitted user mouse factor in the native merc sprint branch.
//! No raw-input replacement, fixed sprint multiplier, pawn/input-flag writes,
//! third-person change, gamepad change, FOV change or movement change.
use super::memory::{float, signature, word};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, Ordering};

const COMMON: usize = 0x00d6_8ca0;
const MAPPING: usize = 0x00d2_a060;
const MERC_MODE_VT: u32 = 0x02ae_9404;
type MapFn = unsafe extern "thiscall" fn(usize, f32, f32, f32) -> f32;
static READY: AtomicBool = AtomicBool::new(false);
static LOGGED: AtomicBool = AtomicBool::new(false);
static_detour! { static Common: unsafe extern "thiscall" fn(usize) -> f32; }

unsafe fn parameters(core: usize) -> Option<[f32; 3]> {
    let pawn = word(core, 0x24)? as usize;
    // Require both native first-person bit and merc role; explicitly exclude spy.
    let flags = word(pawn, 0x374)?;
    if flags & 0x000e_0000 != 0x000a_0000 {
        return None;
    }
    let state = float(pawn, 0x17a0)?;
    if !state.is_finite() || state > 0.0 {
        return None;
    }
    let input = word(pawn, 0x93c)? as usize;
    if word(input, 0x48)? & 0x8000_0000 == 0 {
        return None;
    }
    let pc = word(core, 0x20)? as usize;
    let camera = word(core, 0x1c)? as usize;
    let mode = word(core, 0x2c)? as usize;
    if word(mode, 0)? != MERC_MODE_VT
        || word(mode, 4)? as usize != core
        || word(pc, 0x60)? & 0x40000 == 0
        || word(pc, 0x4c)? & 0xff00 == 0x0200
        || word(pc, 0x310)? as usize != pawn
        || word(pc, 0x520)? as usize != pawn
        || word(pawn, 0x938)? as usize != pc
        || word(pc, 0x6a4)? as usize != camera
        || word(camera, 0x61c)? as usize != core
    {
        return None;
    }
    let device = word(pc, 0x4d8)? as usize;
    if word(device, 0x28)? & 4 == 0 {
        return None;
    }
    let values = [
        float(camera, 0x354)?,
        float(camera, 0x360)?,
        float(camera, 0x364)?,
    ];
    if !values.iter().all(|v| v.is_finite())
        || !(0.0..=1.0).contains(&values[0])
        || values[1] <= 0.0
        || values[1] > 1.0
        || values[2] < 1.0
    {
        return None;
    }
    Some(values)
}

// The original computes the pawn/weapon base factor even in the bypass case.
// Preserve that result and apply exactly the omitted native user mapping once.
pub(super) unsafe fn finish(core: usize, original: f32, mapper: MapFn) -> f32 {
    if !original.is_finite() || original <= 0.0 {
        return original;
    }
    let Some([value, minimum, maximum]) = parameters(core) else {
        return original;
    };
    let factor = mapper(core, value, minimum, maximum);
    let corrected = (original as f64 * factor as f64) as f32;
    if !factor.is_finite() || factor <= 0.0 || !corrected.is_finite() {
        return original;
    }
    corrected
}

fn common(core: usize) -> f32 {
    unsafe {
        let original = Common.call(core);
        if !READY.load(Ordering::Acquire) {
            return original;
        }
        let corrected = finish(core, original, std::mem::transmute::<usize, MapFn>(MAPPING));
        if corrected.to_bits() != original.to_bits() && !LOGGED.swap(true, Ordering::Relaxed) {
            tracing::info!("SCBL_CORE_MERC_MOUSE revision=C71 applied=true native_fps_slider=true fixed_multiplier=false original={} corrected={}", original, corrected);
        }
        corrected
    }
}

pub(crate) unsafe fn init(world: usize) {
    if READY.load(Ordering::Acquire) {
        return;
    }
    if world != 0x589860
        || !signature(
            COMMON,
            0x165,
            "88a15987dba2ae25ba3e252645df80df97c7e5552e758b227c039b64bb7148e9",
        )
        || !signature(
            MAPPING,
            0x7e,
            "85b031ca5ea23e3fc360c361afa24ed3be5b419f3a075760d659134703185c1d",
        )
    {
        tracing::warn!("SCBL_CORE_MERC_MOUSE initialized=false reason=native-contract");
        return;
    }
    if Common
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize) -> f32>(COMMON),
            common,
        )
        .and_then(|hook| hook.enable())
        .is_ok()
    {
        READY.store(true, Ordering::Release);
        tracing::info!("SCBL_CORE_MERC_MOUSE revision=C71 initialized=true local_merc_mouse_only=true native_fps_mapping=true");
    } else {
        let _ = Common.disable();
        tracing::warn!("SCBL_CORE_MERC_MOUSE initialized=false reason=hook");
    }
}

pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Common.disable();
    LOGGED.store(false, Ordering::Relaxed);
}

#[cfg(test)]
#[path = "merc_mouse_sensitivity_tests.rs"]
mod tests;
