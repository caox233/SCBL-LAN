//! R58 development-only, read-only sampling of the retail MP drone ability.
//! This is NOT the enter/exit implementation. Forward every native call exactly
//! once; never change charge, actor references, action names or replication.
use crate::equipment_native::{float, signature, word};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

const ABILITY: u32 = 0x02a3f14c;
const MERC: u32 = 0x02ad2abc;
const DRONE: u32 = 0x02a88f54;
static READY: AtomicBool = AtomicBool::new(false);
static TICK_LOGS: AtomicU32 = AtomicU32::new(0);
// Tick changes must not consume the budget reserved for exit/cancel evidence.
static ACTION_LOGS: AtomicU32 = AtomicU32::new(0);
// No cached object is ever dereferenced. One local snapshot bounds memory use.
static LAST: Mutex<Option<(usize, Snapshot)>> = Mutex::new(None);
static_detour! {
    static Activate: unsafe extern "thiscall" fn(usize);
    static Deactivate: unsafe extern "thiscall" fn(usize);
    static Cancel: unsafe extern "thiscall" fn(usize);
    static Tick: unsafe extern "thiscall" fn(usize, f32);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    kind: u8,
    flags: u32,
    owner_role: u8,
    body_present: bool,
    body_state: Option<u8>,
    hp: Option<i32>,
    max_hp: Option<i32>,
    destroyed: Option<bool>,
    // Quantized for change suppression only; these are NOT gameplay timers.
    charge_pct: Option<i32>,
    configured_seconds: Option<i32>,
    remaining_seconds: Option<i32>,
    consumption_pct: Option<i32>,
}
fn quantize(value: Option<f32>, scale: f32) -> Option<i32> {
    value
        .filter(|v| v.is_finite() && v.abs() < 100000.0)
        .map(|v| (v * scale).round() as i32)
}
unsafe fn snapshot(ability: usize) -> Option<Snapshot> {
    if word(ability, 0)? != ABILITY {
        return None;
    }
    let owner = word(ability, 0x6c)? as usize;
    if word(owner, 0)? != MERC {
        return None;
    }
    let body = word(ability, 0xa0)? as usize;
    let valid = word(body, 0) == Some(DRONE);
    Some(Snapshot {
        kind: (word(ability, 0x28)? & 255) as u8,
        flags: word(ability, 0x40)?,
        owner_role: ((word(owner, 0x4c)? >> 8) & 255) as u8,
        body_present: valid,
        body_state: valid
            .then(|| word(body, 0x910).map(|v| (v & 255) as u8))
            .flatten(),
        hp: valid.then(|| word(body, 0x5dc).map(|v| v as i32)).flatten(),
        max_hp: valid.then(|| word(body, 0x5e0).map(|v| v as i32)).flatten(),
        destroyed: valid
            .then(|| word(body, 0x4b0).map(|v| v & 0x80000000 != 0))
            .flatten(),
        charge_pct: quantize(float(ability, 0x7c), 100.0),
        configured_seconds: quantize(float(ability, 0x3c), 1.0),
        remaining_seconds: quantize(float(ability, 0x8c), 1.0),
        consumption_pct: quantize(float(ability, 0x84), 100.0),
    })
}
unsafe fn observe(ability: usize, event: &str, force: bool) {
    let (logs, limit) = if force {
        (&ACTION_LOGS, 64)
    } else {
        (&TICK_LOGS, 160)
    };
    if !READY.load(Ordering::Acquire) || logs.load(Ordering::Relaxed) >= limit {
        return;
    }
    let Some(value) = snapshot(ability) else {
        return;
    };
    let changed = if let Ok(mut last) = LAST.try_lock() {
        let changed = *last != Some((ability, value));
        *last = Some((ability, value));
        changed
    } else {
        false
    };
    if (force || changed) && logs.fetch_add(1, Ordering::Relaxed) < limit {
        tracing::info!(
            "SCBL_MOD_DRONE_OBSERVE revision=58 readonly=true event={} state={:?}",
            event,
            value
        );
    }
}
unsafe fn around(ability: usize, event: &str, call: impl FnOnce()) {
    observe(ability, event, true);
    call();
    observe(ability, "after", true);
}
fn activate(ability: usize) {
    unsafe {
        around(ability, "activate", || Activate.call(ability));
    }
}
fn deactivate(ability: usize) {
    unsafe {
        around(ability, "deactivate", || Deactivate.call(ability));
    }
}
fn cancel(ability: usize) {
    unsafe {
        around(ability, "cancel", || Cancel.call(ability));
    }
}
fn tick(ability: usize, dt: f32) {
    unsafe {
        Tick.call(ability, dt);
        observe(ability, "tick", false);
    }
}

pub(super) unsafe fn init(world: usize) {
    if world != 0x589860
        || ![
            (
                0xe7eb60,
                366,
                "9a26031908c6b43fd69e75aac7c6629090488066e5eb02e7a729188b84a87050",
            ),
            (
                0xe58f00,
                98,
                "a9e544bc5692a1ccc4cbae39e9b384ce026e6af8bb930404e6368b270359e98c",
            ),
            (
                0xe6d7a0,
                71,
                "12d269c89b565ddf32cbce1b0e2ac844140c84952387e4f7092e6bf1243786b9",
            ),
            (
                0xe99c60,
                332,
                "a62e231c91c793de55231053051ed7c11725e04f4aeaa0e7f83fd168b8cc0ced",
            ),
        ]
        .iter()
        .all(|&(p, n, sha)| signature(p, n, sha))
    {
        tracing::warn!("SCBL_MOD_DRONE_OBSERVE enabled=false reason=signature");
        return;
    }
    type Action = unsafe extern "thiscall" fn(usize);
    type Update = unsafe extern "thiscall" fn(usize, f32);
    if Activate
        .initialize(std::mem::transmute::<usize, Action>(0xe7eb60), activate)
        .is_err()
        || Deactivate
            .initialize(std::mem::transmute::<usize, Action>(0xe58f00), deactivate)
            .is_err()
        || Cancel
            .initialize(std::mem::transmute::<usize, Action>(0xe6d7a0), cancel)
            .is_err()
        || Tick
            .initialize(std::mem::transmute::<usize, Update>(0xe99c60), tick)
            .is_err()
        || Activate.enable().is_err()
        || Deactivate.enable().is_err()
        || Cancel.enable().is_err()
        || Tick.enable().is_err()
    {
        deinit();
        tracing::warn!("SCBL_MOD_DRONE_OBSERVE enabled=false reason=hook");
        return;
    }
    TICK_LOGS.store(0, Ordering::Relaxed);
    ACTION_LOGS.store(0, Ordering::Relaxed);
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_DRONE_OBSERVE enabled=true readonly=true tick_budget=160 action_budget=64 external_rules=false");
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Tick.disable();
    let _ = Cancel.disable();
    let _ = Deactivate.disable();
    let _ = Activate.disable();
    if let Ok(mut last) = LAST.lock() {
        *last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observer_snapshot_never_changes_native_bytes() {
        let mut ability = vec![0u32; 0xc0 / 4];
        let mut pawn = vec![0u32; 0x70 / 4];
        let mut body = vec![0u32; 0xc00 / 4];
        pawn[0] = MERC;
        pawn[0x4c / 4] = 1 << 8;
        body[0] = DRONE;
        body[0x910 / 4] = 3;
        body[0x5dc / 4] = 50;
        body[0x5e0 / 4] = 100;
        ability[0] = ABILITY;
        ability[0x6c / 4] = pawn.as_ptr() as u32;
        ability[0xa0 / 4] = body.as_ptr() as u32;
        ability[0x28 / 4] = 4;
        ability[0x3c / 4] = 60f32.to_bits();
        ability[0x7c / 4] = 0.5f32.to_bits();
        let before = (ability.clone(), pawn.clone(), body.clone());
        let value = unsafe { snapshot(ability.as_ptr() as usize) }.unwrap();
        assert_eq!(value.kind, 4);
        assert_eq!(value.owner_role, 1);
        assert_eq!(value.body_state, Some(3));
        assert_eq!(value.charge_pct, Some(50));
        assert_eq!((ability.clone(), pawn, body), before);
        ability[0] = 0;
        assert!(unsafe { snapshot(ability.as_ptr() as usize) }.is_none());
        assert!(unsafe { snapshot(0) }.is_none());
    }
    #[test]
    fn wrapper_calls_native_once_and_does_not_synthesize_gameplay() {
        let mut count = 0;
        unsafe {
            around(0, "fixture", || count += 1);
        }
        assert_eq!(count, 1);
        assert_eq!(quantize(Some(f32::NAN), 100.0), None);
        assert_eq!(quantize(Some(f32::INFINITY), 100.0), None);
    }
}

// Shared mode installs no hooks; the control adapter is the sole lifecycle owner.
#[cfg(feature="native-observation")]
pub(super) fn start_shared(){
    TICK_LOGS.store(0,Ordering::Relaxed);ACTION_LOGS.store(0,Ordering::Relaxed);
    READY.store(true,Ordering::Release);
}
#[cfg(feature="native-observation")]
pub(super) unsafe fn observe_shared(ability:usize,event:&str,force:bool){observe(ability,event,force);}
