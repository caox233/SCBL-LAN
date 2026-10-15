//! Preserve only an observed Intel cast's RF emission across Disruptor.
//! No skill reactivation, scan, cooldown/mark write, or replication-format change.
//! Remote casts start at the native replicated activation, not at HUD discovery.
use crate::equipment_native::{armor, float, signature, word};
use crate::rfd_rules::Exposure;
use retour::static_detour;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};

const INTEL: u32 = 0x02a3f2dc;
const SPY: u32 = 0x02ad3234;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static RECORDS: Mutex<Vec<Record>> = Mutex::new(Vec::new());
static REMOTE_IDLE: Mutex<Vec<Key>> = Mutex::new(Vec::new());
pub(crate) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}
static_detour! {
    static AbilityTick: unsafe extern "thiscall" fn(usize, f32);
    static Disrupt: unsafe extern "thiscall" fn(usize, usize);
    static Rf: unsafe extern "thiscall" fn(usize, u32, u8);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    ability: usize,
    pawn: usize,
    object: u32,
    low: u32,
    high: u32,
    world: u32,
}
#[derive(Clone, Copy)]
struct Record {
    key: Key,
    clock: Exposure,
}
unsafe fn key(ability: usize) -> Option<Key> {
    if word(ability, 0)? != INTEL {
        return None;
    }
    let pawn = word(ability, 0x6c)? as usize;
    if word(pawn, 0)? != SPY || word(armor(pawn)?, 0x5c)? as usize != ability {
        return None;
    }
    let peer = word(pawn, 0x2c0)? as usize;
    let (low, high) = (word(peer, 0x70)?, word(peer, 0x74)?);
    if low | high == 0 {
        return None;
    }
    Some(Key {
        ability,
        pawn,
        object: word(pawn, 0x20)?,
        low,
        high,
        world: word(pawn, 0x144)?,
    })
}
unsafe fn alive(k: Key) -> bool {
    key(k.ability) == Some(k) && word(k.pawn, 0x60).is_some_and(|v| v & 0x800 == 0)
}
pub(crate) unsafe fn activated(ability: usize) {
    if crate::intel_rfd_targets::ready() {
        reconcile(ability);
        return;
    }
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(k) = key(ability).filter(|k| alive(*k)) else {
        return;
    };
    if word(ability, 0x40).is_none_or(|v| v & 1 == 0 || v & 8 == 0 || v & 32 != 0) {
        return;
    }
    // E6CC40 initializes +8C from +3C on authority; remote E6D100 observes
    // the same native activation, but intentionally does not own that timer.
    let remote = word(k.pawn, 0x4c).is_some_and(|v| (v >> 8) & 255 == 2);
    if remote {
        let eligible = REMOTE_IDLE.lock().ok().is_some_and(|mut idle| {
            let known = idle.contains(&k);
            idle.retain(|old| *old != k);
            known
        });
        // A late observer has no trustworthy start time. Preserve retail,
        // rather than incorrectly extending an already-running cast by +3C.
        if !eligible {
            return;
        }
        let old_hold = RECORDS
            .lock()
            .ok()
            .is_some_and(|rs| rs.iter().any(|r| r.key == k && r.clock.held));
        if old_hold {
            Rf.call(k.pawn, 2, 0);
        }
    }
    let Some(seconds) = float(ability, if remote { 0x3c } else { 0x8c }) else {
        return;
    };
    let Some(clock) = Exposure::new(seconds) else {
        return;
    };
    let Ok(mut records) = RECORDS.lock() else {
        return;
    };
    records.retain(|r| {
        key(r.key.ability) == Some(r.key) && r.clock.remaining > 0.0 && r.key.ability != ability
    });
    if records.len() < 128 {
        records.push(Record { key: k, clock });
        if LOGS.fetch_add(1, Ordering::Relaxed) < 12 {
            tracing::info!("SCBL_MOD_RFD observed_intel_cast=true remote={} native_seconds={} native_rate={:?}",remote,seconds,float(ability,0x84));
        }
    }
}
/// Called before native Intel object destruction, while its identity is valid.
pub(crate) unsafe fn destroyed(ability: usize) {
    if crate::intel_rfd_targets::ready() {
        if let Some(k) = key(ability) {
            Rf.call(k.pawn, 2, 0);
        }
        return;
    }
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let retired = RECORDS.lock().ok().and_then(|mut rs| {
        rs.iter()
            .position(|r| r.key.ability == ability)
            .map(|i| rs.remove(i))
    });
    if let Some(r) = retired {
        if r.clock.held && key(ability) == Some(r.key) {
            Rf.call(r.key.pawn, 2, 0);
        }
    }
    if let Ok(mut idle) = REMOTE_IDLE.lock() {
        idle.retain(|k| k.ability != ability);
    }
}
pub(crate) unsafe fn deactivating(ability: usize) {
    // Deactivation/Disruptor does not end valid target marks in this MOD.
    if crate::intel_rfd_targets::ready() {
        return;
    }
    if !READY.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut records) = RECORDS.lock() {
        // Disrupt() records provenance BEFORE the original deactivation. Any
        // unrelated/manual stop retires an ordinary clock, never starts a hold.
        records.retain(|r| r.key.ability != ability || r.clock.held && alive(r.key));
    }
}
pub(crate) unsafe fn held(pawn: usize) -> bool {
    if crate::intel_rfd_targets::ready() {
        return crate::intel_rfd_targets::active(pawn) == Some(true);
    }
    if !READY.load(Ordering::Acquire) {
        return false;
    }
    RECORDS.lock().ok().is_some_and(|rs| {
        rs.iter()
            .any(|r| r.key.pawn == pawn && r.clock.held && r.clock.remaining > 0.0 && alive(r.key))
    })
}
fn rf(pawn: usize, reason: u32, enabled: u8) {
    if reason == 2 {
        if let Some(active) = unsafe { crate::intel_rfd_targets::active(pawn) } {
            unsafe {
                Rf.call(pawn, reason, u8::from(active));
            }
            return;
        }
    }
    let enabled = if enabled == 0 && reason == 2 && unsafe { held(pawn) } {
        1
    } else {
        enabled
    };
    unsafe { Rf.call(pawn, reason, enabled) }
}
fn retain_on_disruption(pawn: usize) -> bool {
    let mut preserve = false;
    if READY.load(Ordering::Acquire) {
        if let Ok(mut records) = RECORDS.lock() {
            for r in records.iter_mut().filter(|r| r.key.pawn == pawn) {
                if unsafe { alive(r.key) } && r.clock.remaining > 0.0 {
                    // Capture exact authoritative remaining time before the
                    // original Disruptor zeroes it; repeated overlap cannot extend.
                    unsafe {
                        if word(pawn, 0x4c).is_some_and(|v| (v >> 8) & 255 != 2) {
                            if let Some(t) =
                                float(r.key.ability, 0x8c).filter(|v| v.is_finite() && *v > 0.0)
                            {
                                r.clock.remaining = r.clock.remaining.min(t);
                            }
                        }
                    }
                    r.clock.disrupt();
                    preserve = r.clock.held;
                }
            }
        }
    }
    preserve
}
fn disrupt(pawn: usize, origin: usize) {
    if crate::intel_rfd_targets::ready() {
        unsafe {
            Disrupt.call(pawn, origin);
            if let Some(active) = crate::intel_rfd_targets::active(pawn) {
                Rf.call(pawn, 2, u8::from(active));
            }
        }
        return;
    }
    let preserve = retain_on_disruption(pawn);
    unsafe {
        Disrupt.call(pawn, origin);
        if preserve && held(pawn) {
            // Native per-reason writer updates the native RF registry. Reason 1
            // (remote aggregate) and reason 3 (local goggles) are never overwritten.
            Rf.call(pawn, 2, 1);
            if LOGS.fetch_add(1, Ordering::Relaxed) < 12 {
                tracing::info!(
                    "SCBL_MOD_RFD intel_disruptor=retain-original-window marks_or_recast=false"
                );
            }
        }
    }
}
fn advance(ability: usize, dt: f32) -> Option<usize> {
    if READY.load(Ordering::Acquire) {
        unsafe {
            if let Some(k) = key(ability).filter(|k| alive(*k)) {
                let remote = word(k.pawn, 0x4c).is_some_and(|v| (v >> 8) & 255 == 2);
                let idle = word(k.pawn, 0x2c0)
                    .and_then(|peer| word(peer as usize, 0x4e6))
                    .is_some_and(|v| v & 255 == 0);
                if remote && idle {
                    if let Ok(mut seen) = REMOTE_IDLE.lock() {
                        seen.retain(|old| key(old.ability) == Some(*old));
                        if seen.len() < 128 && !seen.contains(&k) {
                            seen.push(k);
                        }
                    }
                }
            }
        }
    }
    let mut clear = None;
    if READY.load(Ordering::Acquire) {
        if let Ok(mut records) = RECORDS.lock() {
            for r in records.iter_mut().filter(|r| r.key.ability == ability) {
                let alive = unsafe { alive(r.key) };
                let rate = unsafe { float(ability, 0x84) }.unwrap_or(f32::NAN);
                let was_held = r.clock.held;
                r.clock.tick(dt, rate, alive);
                if was_held && !r.clock.held && unsafe { key(ability) } == Some(r.key) {
                    clear = Some(r.key.pawn);
                }
            }
            records.retain(|r| {
                r.key.ability != ability || r.clock.remaining > 0.0 && unsafe { alive(r.key) }
            });
        }
    }
    clear
}
fn tick(ability: usize, dt: f32) {
    if crate::intel_rfd_targets::ready() {
        unsafe {
            AbilityTick.call(ability, dt);
            reconcile(ability);
        }
        return;
    }
    let clear = advance(ability, dt);
    unsafe {
        if let Some(pawn) = clear {
            if word(pawn, 0x4c).is_some_and(|v| (v >> 8) & 255 == 2)
                || word(ability, 0x40).is_none_or(|v| v & 8 == 0)
                || word(pawn, 0x60).is_some_and(|v| v & 0x800 != 0)
            {
                Rf.call(pawn, 2, 0);
            }
        }
        AbilityTick.call(ability, dt);
    }
}
unsafe fn reconcile(ability: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(k) = key(ability) else {
        return;
    };
    crate::intel_rfd_targets::publish(k.pawn);
    if let Some(active) = crate::intel_rfd_targets::active(k.pawn) {
        // Use the native per-reason writer/registry. Never overwrite aggregate
        // reason 1 or tactical goggles reason 3, nor any mark / skill state.
        if word(k.pawn, 0x944).is_some_and(|f| (f & 2 != 0) != active) {
            Rf.call(k.pawn, 2, u8::from(active));
        }
    }
}
pub(crate) unsafe fn targets_changed(pawn: usize) {
    if !crate::intel_rfd_targets::ready() {
        return;
    }
    if let Some(ability) = armor(pawn).and_then(|a| word(a, 0x5c)) {
        reconcile(ability as usize);
    }
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860
        || !signature(
            0xe6d100,
            0x320,
            "bbc4a2f73ebfb0b434b37f15ebcb4a8c2e78e74e77eccb923bdfecdc04a6da4e",
        )
        || !signature(
            0xb5eec0,
            0x120,
            "47802eff333342608deb750647465b677c78e93d8f302e338c7a2fcea218facf",
        )
    {
        return;
    }
    if !signature(0x91fc00, 0x7c, RF_SHA) {
        return;
    }
    let ok = Rf
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, u32, u8)>(0x91fc00),
            rf,
        )
        .is_ok()
        && Disrupt
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, usize)>(0xb5eec0),
                disrupt,
            )
            .is_ok()
        && AbilityTick
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0xe6d100),
                tick,
            )
            .is_ok()
        && Rf.enable().is_ok()
        && Disrupt.enable().is_ok()
        && AbilityTick.enable().is_ok();
    if !ok {
        let _ = AbilityTick.disable();
        let _ = Disrupt.disable();
        let _ = Rf.disable();
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_RFD intel_exposure=observed-native-cast disruption-only=true replication_format=unchanged");
}
const RF_SHA: &str = "00fa8357e46b51e6cbab9b16093e187a73737b2c7515a09153603d91df12ee92";
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let records = RECORDS
        .lock()
        .map(|mut rs| std::mem::take(&mut *rs))
        .unwrap_or_default();
    for r in records {
        if r.clock.held
            && key(r.key.ability) == Some(r.key)
            && (word(r.key.pawn, 0x4c).is_some_and(|v| (v >> 8) & 255 == 2)
                || word(r.key.ability, 0x40).is_some_and(|v| v & 8 == 0))
        {
            Rf.call(r.key.pawn, 2, 0);
        }
    }
    let _ = AbilityTick.disable();
    let _ = Disrupt.disable();
    let _ = Rf.disable();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_layout_cast_disrupt_expiry_death_and_remote_provenance() {
        let mut pawn = vec![0u32; 0x1500 / 4];
        let mut ability = vec![0u32; 0xa0 / 4];
        let mut a = vec![0u32; 0x100 / 4];
        let mut peer = vec![0u32; 0x600 / 4];
        let p = pawn.as_mut_ptr() as usize;
        let s = ability.as_mut_ptr() as usize;
        pawn[0] = SPY;
        pawn[0x20 / 4] = 9;
        pawn[0x4c / 4] = 1 << 8;
        pawn[0x144 / 4] = 31;
        pawn[0x2c0 / 4] = peer.as_mut_ptr() as u32;
        pawn[0xcf4 / 4] = a.as_mut_ptr() as u32;
        a[0] = 0x02ad42ac;
        a[0x5c / 4] = s as u32;
        peer[0x70 / 4] = 123;
        peer[0x74 / 4] = 456;
        ability[0] = INTEL;
        ability[0x6c / 4] = p as u32;
        ability[0x40 / 4] = 9;
        ability[0x3c / 4] = 10.0f32.to_bits();
        ability[0x8c / 4] = 10.0f32.to_bits();
        ability[0x84 / 4] = 1.0f32.to_bits();
        RECORDS.lock().unwrap().clear();
        REMOTE_IDLE.lock().unwrap().clear();
        READY.store(true, Ordering::Release);
        unsafe {
            activated(s);
            assert!(!held(p));
            assert_eq!(advance(s, 3.0), None);
            ability[0x8c / 4] = 7.0f32.to_bits();
            assert!(retain_on_disruption(p));
            deactivating(s);
            ability[0x40 / 4] = 1;
            ability[0x8c / 4] = 0;
            assert!(held(p));
            assert_eq!(advance(s, 2.0), None);
            assert!(retain_on_disruption(p));
            assert_eq!(advance(s, 5.0), Some(p));
            assert!(!held(p));
            // Death removes only the RF hold; it does not rescan marks or revive.
            ability[0x40 / 4] = 9;
            ability[0x8c / 4] = 10.0f32.to_bits();
            activated(s);
            assert!(retain_on_disruption(p));
            pawn[0x60 / 4] = 0x800;
            assert_eq!(advance(s, 0.0), Some(p));
            assert!(!held(p));
            pawn[0x60 / 4] = 0;
            // Manual/EMP stop without Disruptor provenance must NOT retain.
            activated(s);
            deactivating(s);
            assert!(!retain_on_disruption(p));
            // Late-active remote observation has no trustworthy duration.
            pawn[0x4c / 4] = 2 << 8;
            peer[0x4e4 / 4] = 1 << 16;
            activated(s);
            assert!(!retain_on_disruption(p));
            // Native replicated idle->active observation permits the native
            // configured clock; no writes to peer state are made by the adapter.
            peer[0x4e4 / 4] = 0;
            advance(s, 0.0);
            peer[0x4e4 / 4] = 1 << 16;
            activated(s);
            advance(s, 3.0);
            assert!(retain_on_disruption(p));
            assert!(held(p));
            ability[0x40 / 4] = 1;
            assert_eq!(advance(s, 7.0), Some(p));
            assert!(!held(p));
            assert_eq!(
                peer[0x4e4 / 4],
                1 << 16,
                "replicated native active bit is read-only"
            );
            // Same allocation reused for a different network identity is not held.
            peer[0x4e4 / 4] = 0;
            advance(s, 0.0);
            ability[0x40 / 4] = 9;
            activated(s);
            assert!(retain_on_disruption(p));
            peer[0x70 / 4] = 999;
            assert!(!held(p));
            assert_eq!(advance(s, 0.0), None);
        }
        RECORDS.lock().unwrap().clear();
        REMOTE_IDLE.lock().unwrap().clear();
        READY.store(false, Ordering::Release);
        println!("R51_EXPOSURE observed cast10s disrupt at3s -> retain7s; repeat no refresh; death/manual stop/identity reuse/late remote fail closed; replicated active unchanged");
    }
}
