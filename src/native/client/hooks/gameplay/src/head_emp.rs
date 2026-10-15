//! DX11 Detection Enhancer: keep the native first EMP phase and every native
//! apply/expiry path. No global FX clear, no packets, no pawn/ability timer writes.
//! ED5150 cdecl(target,source,status,visual,strength_a,strength_b) has local and
//! received-event callers. E9E120 / EACDE0 end visual; B92DF0 / D10570 end EMP.
//! The host only adapts native values; the hot child decides the phase duration.
use crate::equipment_native::{
    armor, equipped, float, part_name, readable, signature, word, writable,
};
use crate::feature_contract::*;
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
const MERC: u32 = 0x02ad2abc;
const ARMOR: u32 = 0x02ad42ac;
const CONTROLLER: u32 = 0x02ac53ec;
const PART: u32 = 0x02a3d804;
const NAME: &[u8] = b"MPArmorHelmetTech";
// E9E120 copies this retail controller's +870 to phase timer +874.
// Read-only retail DX11 observation: 1.0. Non-default local assets fail closed.
const FIRST: f32 = 1.0;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static LIFECYCLE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
struct NativeObject {
    address: usize,
    class: u32,
    object: u32,
    world: usize,
    world_object: Option<u32>,
}
unsafe fn native_object(address: usize) -> Option<NativeObject> {
    let world = word(address, 0x144)? as usize;
    Some(NativeObject {
        address, class: word(address, 0)?, object: word(address, 0x20)?, world,
        world_object: if world == 0 { None } else { Some(word(world, 0x20)?) },
    })
}
fn active_epoch() -> Option<u32> {
    READY.load(Ordering::Acquire).then(|| LIFECYCLE.load(Ordering::Acquire))
}
unsafe fn armor_owner(a: usize) -> Option<(u32, NativeObject)> {
    if word(a, 0)? != ARMOR { return None; }
    Some((word(a, 0x20)?, native_object(word(a, 0x2c)? as usize)?))
}
type ApplyFn = unsafe extern "C" fn(usize, usize, f32, f32, f32, f32);
type AggregateFn = unsafe extern "thiscall" fn(usize);
static_detour! {
    static Apply: unsafe extern "C" fn(usize,usize,f32,f32,f32,f32);
    static Aggregate: unsafe extern "thiscall" fn(usize);
}
unsafe fn eligible(p: usize) -> Option<f32> {
    if word(p,0)?!=MERC || word(p,0x374)? & ((1<<18)|(1<<19)) != 1<<19
        || word(p,0x60)? & (1<<11)!=0 // same native ED5150 dead check
        || word(p,0x1a08)? & 2!=0 // preserve an already active overload
        || !equipped(armor(p)?,NAME)
    {
        return None;
    }
    let threshold = float(p, 0x1a10)?;
    let controller = word(p, 0x938)? as usize;
    if controller != 0
        && (word(controller, 0)? != CONTROLLER
            || word(controller, 0x520)? as usize != p
            || float(controller, 0x870)? != FIRST)
    {
        return None;
    }
    Some(threshold)
}
fn duration(status: f32, visual: f32, first: f32, threshold: f32) -> Option<f32> {
    let mut p = FeaturePacket::new(EVENT_HEAD_EMP);
    p.words[..4].copy_from_slice(&[
        status.to_bits(),
        visual.to_bits(),
        first.to_bits(),
        threshold.to_bits(),
    ]);
    crate::live_rules::feature(p)
        .map(|v| f32::from_bits(v.words[0]))
        .filter(|v| *v > 0.0)
}
fn apply_with(
    target: usize, source: usize, status: f32, visual: f32, a: f32, b: f32,
    original: impl FnOnce(usize, usize, f32, f32, f32, f32),
    feedback: impl FnOnce(usize),
) {
    let epoch = active_epoch();
    let phase = if epoch.is_some() {
        unsafe { eligible(target) }.and_then(|threshold| duration(status, visual, FIRST, threshold))
    } else {
        None
    };
    // The original can remove/reuse an actor or stop this adapter. Identity
    // evidence only gates the post-call feedback, not native EMP delivery.
    let owners = phase.and_then(|_| unsafe { Some((native_object(target)?, native_object(source)?, native_object(counter_controller(source)?)?)) });
    let (status, visual) = phase.map_or((status, visual), |p| (p, p));
    original(target, source, status, visual, a, b);
    if let (Some(phase), Some((target_owner, source_owner, controller_owner))) = (phase, owners) {
        if active_epoch() != epoch
            || unsafe { native_object(target) } != Some(target_owner)
            || unsafe { native_object(source) } != Some(source_owner)
            || unsafe { counter_controller(source).and_then(|p| native_object(p)) } != Some(controller_owner)
        { return; }
        // Confirm native apply actually ran, rather than announce a rejected hit.
        if unsafe { word(target, 0x1a08) }.is_some_and(|f| f & 1 != 0)
            && unsafe { float(target, 0x1a0c) } == Some(phase)
        {
            feedback(source);
            if LOGS.fetch_add(1, Ordering::Relaxed) < 20 {
                tracing::info!(
                    "SCBL_MOD_HEAD_EMP revision=32 applied=true first_phase={} native_expiry=true",
                    phase
                );
            }
        }
    }
}
fn apply(target: usize, source: usize, status: f32, visual: f32, a: f32, b: f32) {
    apply_with(target, source, status, visual, a, b,
        |t, s, d, v, a, b| unsafe { Apply.call(t, s, d, v, a, b) },
        |s| unsafe { countered(s) });
}
unsafe fn counter_controller(source: usize) -> Option<usize> {
    let flags = word(source, 0x60)?;
    let controller = if flags & 0x40000 != 0 {
        source
    } else {
        if word(source, 0x5c)? & 0x800000 == 0 { return None; }
        if word(word(source, 0)? as usize, 0x33c) != Some(0x00b8b2e0) { return None; }
        word(source, 0x358)? as usize
    };
    (word(controller, 0x60)? & 0x40000 != 0).then_some(controller)
}
unsafe fn countered(source: usize) {
    // Same local-instigator guard and native ResistStart/Denied event as E24D70.
    let Some(controller) = counter_controller(source) else { return; };
    let local: unsafe extern "thiscall" fn(usize) -> i32 = std::mem::transmute(0x008c4ed0usize);
    if local(controller) == 0 {
        return;
    }
    let send: unsafe extern "thiscall" fn(usize, usize, usize) =
        std::mem::transmute(0x0052e7f0usize);
    send(0x0339c048, 0, 0);
}
unsafe fn remove_original_bonus(a: usize) -> bool {
    if word(a, 0) != Some(ARMOR) {
        return false;
    }
    let owner = word(a, 0x2c).unwrap_or(0) as usize;
    if word(owner, 0) != Some(MERC) {
        return false;
    }
    let (Some(p), Some(n), Some(cap)) = (word(a, 0x50), word(a, 0x54), word(a, 0x58)) else {
        return false;
    };
    if n == 0 || n > 12 || cap < n || cap > 24 || !readable(p as usize, n as usize * 4) {
        return false;
    }
    let mut factors = [1.0f32; 3];
    let mut found = false;
    for i in 0..n as usize {
        let part = word(p as usize, i * 4).unwrap_or(0) as usize;
        if word(part, 0) != Some(PART) || !part_name(part, NAME) {
            continue;
        }
        if found {
            return false;
        } // malformed duplicate slot is not a new gear contract
        found = true;
        for (j, v) in factors.iter_mut().enumerate() {
            let Some(f) = float(part, 0xd8 + j * 4) else {
                return false;
            };
            if !f.is_finite() || !(0.1..=4.0).contains(&f) {
                return false;
            }
            *v = f;
        }
    }
    if !found || !writable(a + 0xf4, 12) {
        return false;
    }
    let mut adjusted = [0.0f32; 3];
    for i in 0..3 {
        let Some(old) = float(a, 0xf4 + i * 4) else {
            return false;
        };
        adjusted[i] = old / factors[i];
        if !old.is_finite() || old <= 0.0 || !adjusted[i].is_finite() {
            return false;
        }
    }
    // EFF670 multiplies these three without clamping. Remove only this part,
    // retaining any other equipped part's multiplier; never edit shared assets.
    for (i, v) in adjusted.into_iter().enumerate() {
        ((a + 0xf4 + i * 4) as *mut f32).write_unaligned(v);
    }
    true
}
fn aggregate_with(a: usize, original: impl FnOnce(usize)) {
    let epoch = active_epoch();
    let owner = epoch.and_then(|_| unsafe { armor_owner(a) });
    original(a);
    if owner.is_some() && active_epoch() == epoch && unsafe { armor_owner(a) } == owner {
        unsafe { remove_original_bonus(a); }
    }
}
fn aggregate(a: usize) {
    aggregate_with(a, |a| unsafe { Aggregate.call(a) });
}
pub(crate) fn init(world: usize) {
    if world != 0x00589860 {
        return;
    }
    unsafe {
        for (address, len, hash) in [
            (
                0xed5150,
                582,
                "1d6120f6f1872f688e89d367c4efe6912d6f39d72dc220bb17a644b6d93bf6e2",
            ),
            (
                0xeff670,
                1883,
                "3ecac68ea51be6dcaafa24231846a52178c0d76f5f9b733dfbeddac2e73855c1",
            ),
            (
                0xd10550,
                31,
                "7e7e658dfba4ea13aa0f94c47038f70155ec01aa87f4451915f2c3e7176233d2",
            ),
            (
                0xe24d70,
                64,
                "f1a9d1d3e552e5ec2555d173536f00f37f72cfd96a94236f17928b1c4ff6caed",
            ),
            (
                0x52e7f0,
                32,
                "8493983b8328faf4044a3a0aeca02382dea4adbe99b43c7f80d7938eb8936128",
            ),
            (
                0x8c4ed0,
                32,
                "77ddb6b4bd23630e7593693ce0207b130f005c5273246c70357aeb3685c7e54a",
            ),
            (
                0xe9e120,
                439,
                "b8177dbff7642a85b46a6f31488cb761716b7155259009261a1ff1b63c8c8200",
            ),
            (
                0xeacde0,
                171,
                "93541ad8eca18dd5fd5eb67abdf2f11b066d6b75e26edb37621ec0273af0c5d1",
            ),
        ] {
            if !signature(address, len, hash) {
                tracing::error!("SCBL_MOD_HEAD_EMP enabled=false reason=native-signature");
                return;
            }
        }
        if Apply
            .initialize(std::mem::transmute::<usize, ApplyFn>(0xed5150), apply)
            .is_err()
            || Aggregate
                .initialize(
                    std::mem::transmute::<usize, AggregateFn>(0xeff670),
                    aggregate,
                )
                .is_err()
            || Apply.enable().is_err()
            || Aggregate.enable().is_err()
        {
            let _ = Apply.disable();
            let _ = Aggregate.disable();
            return;
        }
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_HEAD_EMP revision=32 adapters=true original_sensor_bonus=removed phase=native-first counter=native-denied hot_child=true");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    LIFECYCLE.fetch_add(1, Ordering::AcqRel);
    let _ = Apply.disable();
    let _ = Aggregate.disable();
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u8>) {
        let name = NAME.iter().copied().chain(Some(0)).collect::<Vec<_>>();
        let mut part = vec![0u32; 0x110 / 4];
        part[0] = PART;
        part[0x28 / 4] = name.as_ptr() as u32;
        part[0x2c / 4] = name.len() as u32;
        part[0x30 / 4] = name.len() as u32;
        for (i, f) in [0.8f32, 1.5, 1.5].into_iter().enumerate() {
            part[(0xd8 + i * 4) / 4] = f.to_bits();
        }
        let list = vec![part.as_ptr() as u32];
        let mut pawn = vec![0u32; 0x1b00 / 4];
        pawn[0] = MERC;
        pawn[0x374 / 4] = 1 << 19;
        pawn[0x1a10 / 4] = 3.0f32.to_bits();
        let mut a = vec![0u32; 0x110 / 4];
        a[0] = ARMOR;
        a[0x2c / 4] = pawn.as_ptr() as u32;
        a[0x50 / 4] = list.as_ptr() as u32;
        a[0x54 / 4] = 1;
        a[0x58 / 4] = 1;
        pawn[0xcf4 / 4] = a.as_ptr() as u32;
        for (i, f) in [0.8f32, 1.5, 1.5].into_iter().enumerate() {
            a[(0xf4 + i * 4) / 4] = f.to_bits();
        }
        (pawn, a, part, list, name)
    }
    #[test]
    fn original_sensor_factors_are_removed_without_editing_shared_gear() {
        let (_p, mut a, part, _list, _name) = fixture();
        let before = part.clone();
        assert!(unsafe { remove_original_bonus(a.as_mut_ptr() as usize) });
        assert_eq!(&a[0xf4 / 4..0x100 / 4], &[1.0f32.to_bits(); 3]);
        assert_eq!(part, before);
    }
    #[test]
    fn other_sensor_bonuses_are_preserved() {
        let (_p, mut a, _part, _list, _name) = fixture();
        a[0xf8 / 4] = 3.0f32.to_bits();
        assert!(unsafe { remove_original_bonus(a.as_mut_ptr() as usize) });
        assert_eq!(f32::from_bits(a[0xf8 / 4]), 2.0);
    }
    #[test]
    fn spy_dead_unknown_and_existing_overload_are_not_changed() {
        let (mut p, _a, _part, _list, _name) = fixture();
        let ptr = p.as_ptr() as usize;
        assert_eq!(unsafe { eligible(ptr) }, Some(3.0));
        p[0x60 / 4] = 1 << 11;
        assert_eq!(unsafe { eligible(ptr) }, None);
        p[0x60 / 4] = 0;
        p[0x1a08 / 4] = 3;
        assert_eq!(unsafe { eligible(ptr) }, None);
        p[0x1a08 / 4] = 0;
        p[0x374 / 4] = 1 << 18;
        assert_eq!(unsafe { eligible(ptr) }, None);
    }
    #[test]
    fn wrong_gear_and_invalid_factors_do_not_partially_write() {
        let (_p, mut a, mut part, _list, mut name) = fixture();
        let before = a.clone();
        part[0xdc / 4] = f32::NAN.to_bits();
        assert!(!unsafe { remove_original_bonus(a.as_mut_ptr() as usize) });
        assert_eq!(a, before);
        part[0xdc / 4] = 1.5f32.to_bits();
        name[0] = b'X';
        assert!(!unsafe { remove_original_bonus(a.as_mut_ptr() as usize) });
        assert_eq!(a, before);
    }
}

#[cfg(test)]
pub(super) unsafe fn fixture_abi(
    g: crate::combat_abi_tests::Get,
    n: crate::combat_abi_tests::Read,
    v: crate::combat_abi_tests::Read,
) {
    let a: ApplyFn = std::mem::transmute(g(9));
    let b: AggregateFn = std::mem::transmute(g(10));
    Apply.initialize(a, apply).unwrap();
    Aggregate.initialize(b, aggregate).unwrap();
    Apply.enable().unwrap();
    Aggregate.enable().unwrap();
    for _ in 0..1000 {
        a(100, 200, 6.0, 5.0, 0.5, 0.25);
        b(300);
        assert_eq!(
            v(9),
            300u32
                .wrapping_add(6f32.to_bits())
                .wrapping_add(5f32.to_bits())
                .wrapping_add(0.5f32.to_bits())
                .wrapping_add(0.25f32.to_bits())
        );
        assert_eq!(v(10), 300);
    }
    assert_eq!(n(9), 1000);
    assert_eq!(n(10), 1000);
    Apply.disable().unwrap();
    Aggregate.disable().unwrap();
    a(100, 200, 6.0, 5.0, 0.5, 0.25);
    b(300);
    assert_eq!(n(9), 1001);
    assert_eq!(n(10), 1001);
}

#[cfg(test)]
include!("head_emp_lifecycle_tests.rs");
