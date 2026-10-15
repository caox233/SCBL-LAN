//! Optional, currently dormant conversion of the retail explosive endpoint.
//! The active policy keeps native destruction; only parked Disable delivery remains.
//! Convert the retail MP drone's explosive endpoint to its existing falling
//! state. AD3920 is ALSO used by ground contact after falling: that second call
//! must finish destruction, never start another fall. No generic actor hook,
//! damage scaling, health restoration or custom network packet is introduced.
#[path = "drone_damage.rs"]
mod damage;
use crate::equipment_native::{signature, word, writable};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const DRONE: u32 = 0x02a88f54;
// C17: retain the experimental no-explosion implementation, but install neither
// its endpoint detour nor its damage/death clones in the current gameplay DLL.
pub(super) const NONEXPLOSIVE_POLICY_ENABLED: bool = false;
type End = unsafe extern "thiscall" fn(usize, u8);
type Fall = unsafe extern "thiscall" fn(usize);
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static_detour! { static EndDrone: unsafe extern "thiscall" fn(usize,u8); }

#[derive(Debug, PartialEq, Eq)]
enum Route {
    Native,
    Fall,
    Finish,
}

unsafe fn route(body: usize, vt: u32) -> Route {
    if word(body, 0) != Some(vt)
        || !word(body, 0x4c).is_some_and(|v| matches!((v >> 8) & 255, 1..=3))
        || !writable(body + 0x7c4, 4)
        || !writable(body + 0xbc0, 4)
        || !writable(body + 0x4ac, 4)
    {
        return Route::Native;
    }
    match word(body, 0x910).map(|s| s & 255) {
        Some(1 | 2) => Route::Fall,
        Some(0 | 3) => Route::Finish,
        _ => Route::Native,
    }
}

unsafe fn apply(body: usize, replicate: u8, vt: u32, end: End, fall: Fall, rpc: Fall) -> Route {
    let selected = route(body, vt);
    if selected == Route::Native {
        end(body, replicate);
        return selected;
    }
    // AD3920 bc0.2 chooses explosive FX, E97010 7c4.10 chooses AOE.
    // This actor is ending its life; these bits must not be restored after the
    // call (which can destroy it), nor may a later ground contact explode it.
    *((body + 0xbc0) as *mut u32) &= !4;
    *((body + 0x7c4) as *mut u32) &= !0x400;
    if selected == Route::Finish {
        // State3 already does this natively. State0 can be an unactivated body
        // being removed: do not leave it parked or play explosion audio there.
        *((body + 0x4ac) as *mut u32) |= 0x80;
        end(body, replicate);
    } else {
        let broadcast = replicate != 0 && ((word(body, 0x4c).unwrap() >> 8) & 255) == 1;
        let node = word(body, 0x2c0).unwrap_or(0) as usize;
        // AF5F60 source cancellation, native falling FX, physics, velocity and
        // end event remain in B0F3B0. DroneTags prevents reacquisition in state3.
        fall(body);
        if broadcast && node != 0 {
            rpc(node);
        }
    }
    selected
}
unsafe extern "thiscall" fn original(body: usize, replicate: u8) {
    EndDrone.call(body, replicate);
}
fn end_drone(body: usize, replicate: u8) {
    unsafe {
        if !READY.load(Ordering::Acquire) {
            EndDrone.call(body, replicate);
            return;
        }
        let result = apply(
            body,
            replicate,
            DRONE,
            original,
            std::mem::transmute::<usize, Fall>(0xb0f3b0),
            std::mem::transmute::<usize, Fall>(0xfe0be0),
        );
        if result != Route::Native && LOGS.fetch_add(1, Ordering::Relaxed) < 24 {
            // Never read body after end: native terminal cleanup can free it.
            tracing::info!(
                "SCBL_MOD_DRONE_FALL revision=58 route={:?} native_cleanup=true",
                result
            );
        }
    }
}
pub(crate) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

unsafe fn disable_parked_with(body: usize, vt: u32, end: End, fall: Fall, rpc: Fall) -> bool {
    if word(body, 0x910).map(|v| v & 255) != Some(1) || route(body, vt) != Route::Fall {
        return false;
    }
    apply(body, 1, vt, end, fall, rpc) == Route::Fall
}
unsafe fn disable_parked_native_with(body: usize, vt: u32, fall: Fall, rpc: Fall) -> bool {
    if word(body, 0x910).map(|v| v & 255) != Some(1) || route(body, vt) != Route::Fall {
        return false;
    }
    // The original MP control-state receiver no longer exists while parked.
    // Deliver its native Disable endpoint, without editing explosion/damage bits
    // or detouring later collision, damage, scoring, or terminal cleanup.
    let broadcast = ((word(body, 0x4c).unwrap() >> 8) & 255) == 1;
    let node = word(body, 0x2c0).unwrap_or(0) as usize;
    fall(body);
    if broadcast && node != 0 {
        rpc(node);
    }
    true
}
/// Complete native Disable when its original receiver (MP control state) has
/// already exited. Never force the finish endpoint, reset HP, or detach a live
/// body here: native fall/ground contact and ability tick perform that cleanup.
pub(crate) unsafe fn disable_parked(body: usize) -> bool {
    if !READY.load(Ordering::Acquire) {
        return false;
    }
    if NONEXPLOSIVE_POLICY_ENABLED {
        disable_parked_with(
            body,
            DRONE,
            original,
            std::mem::transmute::<usize, Fall>(0xb0f3b0),
            std::mem::transmute::<usize, Fall>(0xfe0be0),
        )
    } else {
        disable_parked_native_with(
            body,
            DRONE,
            std::mem::transmute::<usize, Fall>(0xb0f3b0),
            std::mem::transmute::<usize, Fall>(0xfe0be0),
        )
    }
}
pub(crate) fn init(world: usize) {
    if world != 0x589860 || !crate::drone_tags::source_cancel_ready() {
        return;
    }
    unsafe {
        for (va, n, hash) in [
            (
                0xad3920,
                511,
                "d3e31004ef82b24836086414f453e8fc813202c4641a20f53c0cb981959b7162",
            ),
            (
                0xb0f3b0,
                579,
                "32ab1976fecca894518d20785ba67c1275c8449bfff2ff1109d48ccb0adb2fda",
            ),
            (
                0xe97010,
                386,
                "2e01388a531baf95578a522ab23a52f731c7da8bc7c4b1b0caeead1397f194e0",
            ),
            (
                0xfe0be0,
                208,
                "46a68337b33835038bfc7674598bac281c2b9573b924e2a3e86aaf0f5d9f19bb",
            ),
        ] {
            if !NONEXPLOSIVE_POLICY_ENABLED && !matches!(va, 0xb0f3b0 | 0xfe0be0) {
                continue;
            }
            if !signature(va, n, hash) {
                tracing::error!("SCBL_MOD_DRONE_FALL enabled=false reason=native-signature");
                return;
            }
        }
        if !NONEXPLOSIVE_POLICY_ENABLED {
            READY.store(true, Ordering::Release);
            tracing::info!("SCBL_MOD_DRONE_FALL policy=native nonexplosive_hooks=false parked_disable_bridge=true marking=unchanged");
            return;
        }
        if EndDrone
            .initialize(std::mem::transmute::<usize, End>(0xad3920), end_drone)
            .is_err()
            || EndDrone.enable().is_err()
        {
            let _ = EndDrone.disable();
            return;
        }
        if !damage::init() {
            let _ = EndDrone.disable();
            return;
        }
        READY.store(true, Ordering::Release);
        tracing::info!(
            "SCBL_MOD_DRONE_FALL enabled=true native_fall=true explosion_fx_sound_aoe=false"
        );
    }
}
pub(crate) fn deinit() {
    READY.store(false, Ordering::Release);
    unsafe {
        damage::deinit();
        let _ = EndDrone.disable();
    }
}

#[cfg(test)]
#[path = "drone_fall_tests.rs"]
mod tests;
