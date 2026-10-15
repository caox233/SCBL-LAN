//! C70: native MP sonar restored after the C69 pulse trial.
//! Per-view blended output only: thermal30m, EMF40m, sonar40m.
//! Sonar uses native20m/s and invisible persistence5s; native glow/fade stay intact.
//! No custom cadence, per-target lifetime, material override or tail-cap hooks.
use crate::equipment_native::{float, readable, signature, word, writable};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static READY: AtomicBool = AtomicBool::new(false);
static LOGGED: AtomicU32 = AtomicU32::new(0);
static_detour! { static Blend: unsafe extern "cdecl" fn(usize, f32, usize); }
const VIEW_DISTANCE: usize = 0x16c;
const SONAR_WAVE_SPEED: usize = 0x17c;
const SONAR_SPEED_CM_S: f32 = 2000.0;
const SONAR_PERSISTENCE: usize = 0x1b0;
const SONAR_PERSISTENCE_S: f32 = 5.0;
fn distance_cm(mode: u32) -> Option<f32> {
    match mode {
        8 => Some(3000.0),
        9 => Some(4000.0),
        7 => Some(4000.0),
        _ => None,
    }
}
unsafe fn apply(view: usize) -> Option<(u32, f32, f32)> {
    let state = word(view, 0x538)? as usize;
    let mode = word(state, 0x1c)?;
    let distance = distance_cm(mode)?;
    let output = word(state, 4)? as usize;
    // Native +0 is the source effect list, +4 is its private blended result.
    // Do not patch a malformed alias of any immutable/source profile.
    let sources = word(state, 0)? as usize;
    let field = output.checked_add(VIEW_DISTANCE)?;
    if output == 0 || sources == output || !readable(sources, 47 * 0x30) || !writable(field, 4) {
        return None;
    }
    for index in 0..47 {
        if ((sources + index * 0x30) as *const u32).read_unaligned() as usize == output {
            return None;
        }
    }
    let old = float(output, VIEW_DISTANCE)?;
    if !old.is_finite() || old <= 0.0 {
        return None;
    }
    // Validate every requested sonar field before any write. Shared source
    // profiles, glow duration, fade shape, palette and all other modes stay native.
    let sonar_fields = if mode == 7 {
        let speed = float(output, SONAR_WAVE_SPEED)?;
        let persistence = float(output, SONAR_PERSISTENCE)?;
        let speed_field = output.checked_add(SONAR_WAVE_SPEED)?;
        let persistence_field = output.checked_add(SONAR_PERSISTENCE)?;
        if !writable(speed_field, 4)
            || !writable(persistence_field, 4)
            || !speed.is_finite()
            || speed <= 0.0
            || !persistence.is_finite()
            || persistence < 0.0
        {
            return None;
        }
        Some((speed_field, persistence_field))
    } else {
        None
    };
    (field as *mut f32).write_unaligned(distance);
    if let Some((speed, persistence)) = sonar_fields {
        (speed as *mut f32).write_unaligned(SONAR_SPEED_CM_S);
        (persistence as *mut f32).write_unaligned(SONAR_PERSISTENCE_S);
    }
    Some((mode, old, distance))
}
fn blend(world: usize, dt: f32, view: usize) {
    unsafe {
        Blend.call(world, dt, view);
        if READY.load(Ordering::Acquire) {
            if let Some((mode, old, distance)) = apply(view) {
                let bit = 1 << (mode - 7);
                if LOGGED.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
                    tracing::info!("SCBL_MOD_SPY_GOGGLE_RANGE applied=true mode={} native_cm={} maximum_cm={} per_view_only=true", mode, old, distance);
                    if mode == 7 {
                        tracing::info!("SCBL_MOD_SONAR_NATIVE revision=C70 applied=true speed_cm_s=2000 persistence_s=5 glow_unchanged=true native_fade=true source_profiles_unchanged=true pulse_override=false");
                    }
                }
            }
        }
    }
}
const CONTRACTS: &[(usize, usize, &str)] = &[
    // blend cdecl(world,dt,view); per-view state at view+538
    (
        0xa31120,
        0x60,
        "ffa2da40a274305e9486aa8b65d388f8bdf29d7dd1780b83e0a70d1a10e3ba90",
    ),
    // per-view output state+4 copied from source; native reset every update
    (
        0xa31186,
        0x3e,
        "cc70af70fb45f3594022628396d996c048749091fa0dfce1272f378125ba282b",
    ),
    // copy profile bytes 30..268 to per-view output
    (
        0x166b340,
        0x2c,
        "d3ee4ae4fec8c0d23b06aa6eba7571e39083e00e3515fa16d8b87de419992dbe",
    ),
    // Viewmode view distance property at profile+16C
    (
        0x166d3ce,
        0x62,
        "a927b8445eac1bed9f3c8e308039b6bc3663077b8ad6c990de4cc68fea0842c4",
    ),
    // native sonar maximum uses blended +16C minus 100-unit edge margin
    (
        0xa2ea60,
        0x85,
        "3de8e720cde95ca2b80a54d92ea5f34deb1cc5b47adbbb2ec5569f5747e71980",
    ),
    // native EMF/sonar view distance consumption
    (
        0x174e110,
        0x1a0,
        "5887d51904957d66b7b0d01d71dde7c4fdfa8a04fe32f1b9b8215ff6af38abe1",
    ),
    // thermal view distance consumption
    (
        0x175b5f8,
        0x18,
        "5ef9a60b579a54f6648716e14db92131954119edfc8d77de279f8d832b57fc99",
    ),
    // MP thermal enables mode8
    (
        0xaa6518,
        0x14,
        "56cc8729c9099260c55544d309a6ac0ba9a85bd5e4392e9e6203b06181b60c67",
    ),
    // MP EMF enables mode9
    (
        0xaecec0,
        0x2d,
        "e6c4d9c77ca353083559ade48c942341f69e4a8c0e3e23654994e05db5a9c964",
    ),
    // MP sonar enables mode7
    (
        0xaed3df,
        0x14,
        "903d3de6b7ac40553e684d32d675eff9b31cda1d170d30a54583a506e074bc7b",
    ),
];
const SONAR_CONTRACTS: &[(usize, usize, &str)] = &[
    (
        0x166da37,
        0x64,
        "ba1534701c6584d8543144611c6938095271a620367ec092efad91f6aaab1da4",
    ), // Sonar wave speed registered at profile+17C
    (
        0xa2e7f0,
        0x270,
        "7ee88a2a43d7acdb67e3692c4d5f60bce0fa30ec01d818ccc9e74f207fd36d5c",
    ), // native wave update cdecl(dt,view); speed, range and persistence
    (
        0xa2e869,
        0x8,
        "c3d1ff2f3d55fa93b9c6c31b2e110ad01f0410960e19809c701acc641e62f4b3",
    ), // mode-scoped native tail limit load
    (
        0x174e3d1,
        0x8,
        "f0d51f7e4a77627d93a5cd53f754dfd40137d67e38554be1757ae746840ddc42",
    ), // mode-scoped native tail limit load
    (
        0x174e488,
        0x8,
        "9f684145cf44b90490f411af4eff57ebdf1bfb0a02001016c40f7d71c2bf3473",
    ), // mode-scoped native tail limit load
];

// Native10000cm tail cap already accommodates 20m/s * 5s.
// C68 cap thunks and C69 pulse hooks are deliberately not installed.
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860
        || !CONTRACTS
            .iter()
            .chain(SONAR_CONTRACTS.iter())
            .all(|&(p, n, h)| signature(p, n, h))
    {
        tracing::warn!("SCBL_MOD_SPY_GOGGLE_RANGE initialized=false reason=native-contract");
        return;
    }
    if Blend
        .initialize(
            std::mem::transmute::<usize, unsafe extern "cdecl" fn(usize, f32, usize)>(0xa31120),
            blend,
        )
        .and_then(|h| h.enable())
        .is_ok()
    {
        READY.store(true, Ordering::Release);
        tracing::info!("SCBL_MOD_SPY_GOGGLE_RANGE revision=C70 initialized=true thermal_m=30 emf_m=40 sonar_m=40 sonar_speed_mps=20 sonar_persistence_s=5 native_tail_limit_cm=10000 pulse_override=false native_fade=true other_modes=unchanged");
    } else {
        let _ = Blend.disable();
        tracing::warn!("SCBL_MOD_SPY_GOGGLE_RANGE initialized=false reason=hook");
    }
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Blend.disable();
    LOGGED.store(0, Ordering::Relaxed);
    // No cached raw pointers: next native blend restores its entire result.
}
#[cfg(test)]
#[path = "spy_goggle_range_tests.rs"]
mod tests;
