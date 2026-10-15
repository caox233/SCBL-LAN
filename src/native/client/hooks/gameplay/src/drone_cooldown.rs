//! Use the native normalized recharge, thresholds and authority gates.
//! Override only the Drone ability's refill denominator during E6CFB0; no
//! synthetic timer, charge reset, EMP delay or shared data edits.
//! R58: retail multiplayer sampling confirms kind3 normalized battery +7c,
//! configured duration +3c=120, and native movement multiplier +84=1 or4.
//! Halve only the drain denominator during E6D070. Never scale the game tick.
use crate::equipment_native::{float, signature, word, writable};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, Ordering};
const DRONE_ABILITY: u32 = 0x02a3f14c;
pub(super) const SECONDS: f32 = 15.0;
// Production and ordinary observer builds retain the half-native endurance.
#[cfg(not(feature = "drone-capture-long-battery"))]
const BATTERY_DURATION_FACTOR: f32 = 0.5;
// Explicit private capture build: 10x MOD endurance, finite native battery.
#[cfg(feature = "drone-capture-long-battery")]
const BATTERY_DURATION_FACTOR: f32 = 5.0;
static READY: AtomicBool = AtomicBool::new(false);
static BATTERY_READY: AtomicBool = AtomicBool::new(false);
static_detour! { static Recharge: unsafe extern "thiscall" fn(usize,f32); }
static_detour! { static Drain: unsafe extern "thiscall" fn(usize,f32); }
unsafe fn with_duration<R>(ability: usize, call: impl FnOnce() -> R) -> R {
    let duration = if word(ability, 0) == Some(DRONE_ABILITY)
        && word(ability, 0x6c).and_then(|p| word(p as usize, 0)) == Some(0x02ad2abc)
        && writable(ability + 0x2c, 4)
    {
        float(ability, 0x2c).filter(|v| v.is_finite() && *v > 0.0)
    } else {
        None
    };
    struct Restore(*mut f32, f32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                self.0.write_unaligned(self.1);
            }
        }
    }
    let _restore = duration.map(|v| {
        let p = (ability + 0x2c) as *mut f32;
        p.write_unaligned(SECONDS);
        Restore(p, v)
    });
    call()
}
// The native HUD independently reads this denominator to display seconds. Keep
// its number consistent with recharge; never change charge or the drain rate.
pub(super) unsafe fn with_hud_duration<R>(ability: usize, call: impl FnOnce() -> R) -> R {
    with_duration(ability, call)
}
fn recharge(ability: usize, dt: f32) {
    unsafe {
        if READY.load(Ordering::Acquire) {
            with_duration(ability, || Recharge.call(ability, dt));
        } else {
            Recharge.call(ability, dt);
        }
    }
}
unsafe fn with_battery_duration<R>(ability: usize, dt: f32, call: impl FnOnce() -> R) -> R {
    let duration = if dt.is_finite()
        && dt > 0.0
        && word(ability, 0) == Some(DRONE_ABILITY)
        && word(ability, 0x28).is_some_and(|v| v & 255 == 3)
        && word(ability, 0x40).is_some_and(|v| v & 0x2c == 8)
        && word(ability, 0x6c).is_some_and(|p| {
            word(p as usize, 0) == Some(0x02ad2abc)
                && word(p as usize, 0x4c).is_some_and(|v| matches!((v >> 8) & 255, 1 | 3))
        })
        && writable(ability + 0x3c, 4)
    {
        float(ability, 0x3c).filter(|v| v.is_finite() && *v * BATTERY_DURATION_FACTOR > 0.0)
    } else {
        None
    };
    struct Restore(*mut f32, f32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                self.0.write_unaligned(self.1);
            }
        }
    }
    let _restore = duration.map(|value| {
        let p = (ability + 0x3c) as *mut f32;
        p.write_unaligned(value * BATTERY_DURATION_FACTOR);
        Restore(p, value)
    });
    call()
}
fn drain(ability: usize, dt: f32) {
    unsafe {
        if BATTERY_READY.load(Ordering::Acquire) {
            with_battery_duration(ability, dt, || Drain.call(ability, dt));
        } else {
            Drain.call(ability, dt);
        }
    }
}
pub(super) unsafe fn init(world: usize) {
    if world != 0x589860
        || !signature(
            0xe6cfb0,
            191,
            "9e85d42c0a0975c683133637e0ecc2ffd97d201207be0a6c1e28eaff8cdb25da",
        )
    {
        return;
    }
    if Recharge
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0xe6cfb0),
            recharge,
        )
        .is_err()
        || Recharge.enable().is_err()
    {
        let _ = Recharge.disable();
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_DRONE_COOLDOWN seconds={} native_recharge=true emp_pause_and_other_abilities=unchanged", SECONDS);
    if !signature(
        0xe6d070,
        129,
        "377df237a91acb181b81fc6cecba41274e545543346061b303010dc8509a5813",
    ) || Drain
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0xe6d070),
            drain,
        )
        .is_err()
        || Drain.enable().is_err()
    {
        let _ = Drain.disable();
        tracing::warn!("SCBL_MOD_DRONE_BATTERY enabled=false reason=signature-or-hook");
        return;
    }
    BATTERY_READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_DRONE_BATTERY enabled=true duration_factor={} diagnostic_long_battery={} native_movement_multiplier=unchanged native_timer_and_authority=unchanged", BATTERY_DURATION_FACTOR, cfg!(feature = "drone-capture-long-battery"));
}
pub(super) unsafe fn deinit() {
    BATTERY_READY.store(false, Ordering::Release);
    let _ = Drain.disable();
    READY.store(false, Ordering::Release);
    let _ = Recharge.disable();
}
#[cfg(test)]
pub(super) unsafe fn fixture_recharge(
    ability: usize,
    dt: f32,
    native: unsafe extern "thiscall" fn(usize, f32),
) {
    with_duration(ability, || native(ability, dt));
}
#[cfg(test)]
pub(super) unsafe fn fixture_drain(
    ability: usize,
    dt: f32,
    native: unsafe extern "thiscall" fn(usize, f32),
) {
    with_battery_duration(ability, dt, || native(ability, dt));
}
