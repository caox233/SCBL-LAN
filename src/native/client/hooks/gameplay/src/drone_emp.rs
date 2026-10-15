//! EMP body policy AFTER native arbitration (local and received EMPEvent).
//! The operator's B5E240 -> E6CE20 ability cancellation remains completely native.
//! No HP writes, bullet hooks, owner-stop interception, timers or new packets.
use crate::{
    equipment_native::{float, readable, signature, word},
    feature_contract::*,
};
use retour::static_detour;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
// R56: retain the experimental half-HP policy, but install no EMP hooks.
const HEALTH_BASED_EMP_ENABLED: bool = false;
const DRONE: u32 = 0x02a88f54;
const GRENADE: u32 = 0x02aa60c4;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
type Damage = unsafe extern "thiscall" fn(
    usize,
    i32,
    *const u32,
    *const [f32; 3],
    *const [f32; 3],
    u32,
    usize,
) -> i32;
static_detour! {
    static Body: unsafe extern "thiscall" fn(usize,usize,f32,u32,u32,u32);
    static Explosion: unsafe extern "thiscall" fn(usize)->u8;
    static Deliver: unsafe extern "C" fn(usize,usize,f32);
}
#[derive(Clone, Copy)]
struct Seen {
    active: bool,
    bodies: [usize; 64],
    used: usize,
}
impl Seen {
    const fn empty() -> Self {
        Self {
            active: false,
            bodies: [0; 64],
            used: 0,
        }
    }
    fn duplicate(&mut self, body: usize) -> bool {
        if !self.active || body == 0 {
            return false;
        }
        if self.bodies[..self.used].contains(&body) {
            return true;
        }
        if self.used < self.bodies.len() {
            self.bodies[self.used] = body;
            self.used += 1;
        }
        false
    }
}
thread_local! { static SEEN:RefCell<Seen>=const {RefCell::new(Seen::empty())}; }
struct Scope(Seen);
impl Drop for Scope {
    fn drop(&mut self) {
        SEEN.with(|s| *s.borrow_mut() = self.0);
    }
}
fn scope() -> Scope {
    let mut next = Seen::empty();
    next.active = true;
    Scope(SEEN.with(|s| std::mem::replace(&mut *s.borrow_mut(), next)))
}
fn explosion(grenade: usize) -> u8 {
    if !READY.load(Ordering::Acquire) || unsafe { word(grenade, 0) } != Some(GRENADE) {
        return unsafe { Explosion.call(grenade) };
    }
    let _scope = scope();
    unsafe { Explosion.call(grenade) }
}
fn deliver(target: usize, source: usize, duration: f32) {
    // Original EMP has a permanent FX guard; nonlethal EMP must instead dedup
    // the SAME explosion's recursive/direct body visit before any network send.
    let duplicate = READY.load(Ordering::Acquire)
        && unsafe { word(target, 0) } == Some(DRONE)
        && SEEN.with(|s| s.borrow_mut().duplicate(target));
    if !duplicate {
        unsafe { Deliver.call(target, source, duration) }
    }
}
unsafe fn health(body: usize) -> Option<(u32, u32)> {
    if word(body, 0)? != DRONE
        || word(body, 0x4b0)? & 0x80000000 != 0
        || word(body, 0x910)? & 0xff == 0
        || word(body, 0xb68)? != 0
    {
        return None;
    }
    let owner = word(body, 0x9fc)? as usize;
    if word(owner, 0x374).map(|f| f & ((1 << 18) | (1 << 19))) != Some(1 << 19) {
        return None;
    }
    let world = word(body, 0x144)? as usize;
    if !readable(world, 0x6d4) {
        return None;
    }
    let hp = word(body, 0x5dc)?;
    let max = word(body, 0x5e0)?;
    (hp > 0 && max >= 2 && hp <= max && max <= i32::MAX as u32).then_some((hp, max))
}
unsafe fn apply_nonlethal(body: usize, source: usize, hp: u32, damage: u32, apply: Damage) -> bool {
    // The parent native damage function is called only if death is impossible.
    // Rounded integer boundary is delegated to the original EMP destruction.
    if damage == 0 || damage >= hp {
        return false;
    }
    let mut position = [0.0f32; 3];
    for (i, p) in position.iter_mut().enumerate() {
        let Some(v) = float(body, 0x9c + i * 4).filter(|v| v.is_finite()) else {
            return false;
        };
        *p = v;
    }
    let direction = [0.0; 3];
    let electric = 6u32;
    apply(
        body,
        damage as i32,
        &electric,
        &position,
        &direction,
        0,
        source,
    );
    // Preserve native invulnerability/failure behavior, do not manufacture HP.
    word(body, 0x5dc) == Some(hp - damage)
}
fn body(this: usize, source: usize, duration: f32, a: u32, b: u32, c: u32) {
    if READY.load(Ordering::Acquire) {
        if let Some((hp, max)) = unsafe { health(this) } {
            let mut p = FeaturePacket::new(EVENT_DRONE_EMP);
            p.words[..2].copy_from_slice(&[hp, max]);
            let damage = crate::live_rules::feature(p).map_or(0, |p| p.words[0]);
            if unsafe {
                apply_nonlethal(
                    this,
                    source,
                    hp,
                    damage,
                    std::mem::transmute::<usize, Damage>(0xf1b310),
                )
            } {
                if LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
                    tracing::info!("SCBL_MOD_DRONE_EMP revision=29 hp_before={} hp_after={} maximum={} native_damage=true original_destroy=false",hp,hp-damage,max);
                }
                return;
            }
        }
    }
    unsafe { Body.call(this, source, duration, a, b, c) }
}
pub(crate) fn init(world: usize) {
    if !HEALTH_BASED_EMP_ENABLED {
        tracing::info!("SCBL_MOD_DRONE_EMP policy=native-one-emp health_policy_enabled=false");
        return;
    }
    if world != 0x00589860 {
        return;
    }
    unsafe {
        if !signature(
            0xaea010,
            385,
            "dd99cd6345de8ed1874e8fd609c2e5f52590dec1b347ac23ccf97b2a9bd21d8c",
        ) || !signature(
            0xaec7e0,
            32,
            "7e16536e2edd5de76e7ff4fcef1472ea498c8fe560c74f51b50a3c89192613d7",
        ) || !signature(
            0xeae710,
            32,
            "60cf22d792d91f806c059a11e558564af383cac18e01e0bc749950ede86d8ea5",
        ) || !signature(
            0xf1b310,
            234,
            "4bcabcf8e61da1c3dba9865d347c2cbf494a48ca4221e31e49b327f7371e942a",
        ) {
            tracing::error!("SCBL_MOD_DRONE_EMP enabled=false reason=native-signature");
            return;
        }
        if Body
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(usize, usize, f32, u32, u32, u32),
                >(0xaea010),
                body,
            )
            .is_err()
            || Explosion
                .initialize(
                    std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize) -> u8>(
                        0xaec7e0,
                    ),
                    explosion,
                )
                .is_err()
            || Deliver
                .initialize(
                    std::mem::transmute::<usize, unsafe extern "C" fn(usize, usize, f32)>(0xeae710),
                    deliver,
                )
                .is_err()
            || Deliver.enable().is_err()
            || Explosion.enable().is_err()
            || Body.enable().is_err()
        {
            let _ = Body.disable();
            let _ = Explosion.disable();
            let _ = Deliver.disable();
            return;
        }
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_DRONE_EMP adapters=true revision=29 operator_emp=native bullets=native network_arbitration=native hot_child=true");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Body.disable();
    let _ = Explosion.disable();
    let _ = Deliver.disable();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explosion_dedup_not_time_or_source_based() {
        let mut a = Seen::empty();
        assert!(!a.duplicate(123));
        assert!(!a.duplicate(123));
        a.active = true;
        assert!(!a.duplicate(123));
        assert!(a.duplicate(123));
        assert!(!a.duplicate(456));
        let mut b = Seen::empty();
        b.active = true;
        assert!(!b.duplicate(123));
    }
    #[test]
    fn nested_explosions_restore_independent_scope() {
        let _outer = scope();
        SEEN.with(|s| assert!(!s.borrow_mut().duplicate(1)));
        {
            let _inner = scope();
            SEEN.with(|s| assert!(!s.borrow_mut().duplicate(1)));
        }
        SEEN.with(|s| assert!(s.borrow_mut().duplicate(1)));
    }
    unsafe extern "thiscall" fn fake_damage(
        p: usize,
        n: i32,
        t: *const u32,
        pos: *const [f32; 3],
        dir: *const [f32; 3],
        context: u32,
        source: usize,
    ) -> i32 {
        assert_eq!(*t, 6);
        assert_eq!(*pos, [1.0, 2.0, 3.0]);
        assert_eq!(*dir, [0.0; 3]);
        assert_eq!(context, 0);
        assert_eq!(source, 77);
        let hp = (p + 0x5dc) as *mut u32;
        hp.write(hp.read() - n as u32);
        1
    }
    unsafe extern "thiscall" fn refuse(
        _: usize,
        _: i32,
        _: *const u32,
        _: *const [f32; 3],
        _: *const [f32; 3],
        _: u32,
        _: usize,
    ) -> i32 {
        0
    }
    #[test]
    fn native_damage_once_and_failed_damage_falls_back() {
        let mut b = vec![0u32; 0xc00 / 4];
        b[0x5dc / 4] = 100;
        for (i, v) in [1.0f32, 2.0, 3.0].iter().enumerate() {
            b[0x9c / 4 + i] = v.to_bits();
        }
        let p = b.as_mut_ptr() as usize;
        unsafe {
            assert!(apply_nonlethal(p, 77, 100, 50, fake_damage));
            assert_eq!(b[0x5dc / 4], 50);
            assert!(!apply_nonlethal(p, 77, 50, 50, fake_damage));
            assert_eq!(b[0x5dc / 4], 50);
            assert!(!apply_nonlethal(p, 77, 50, 0, fake_damage));
            assert!(!apply_nonlethal(p, 77, 50, 25, refuse));
            assert_eq!(b[0x5dc / 4], 50);
        }
    }
    #[test]
    fn unknown_or_operator_objects_never_use_body_policy() {
        unsafe {
            assert!(health(0).is_none());
        }
        let mut p = vec![0u32; 0x2000 / 4];
        p[0] = 0x02ad2abc;
        unsafe {
            assert!(health(p.as_ptr() as usize).is_none());
        }
    }
}

#[cfg(test)]
pub(super) unsafe fn fixture_abi(
    g: crate::combat_abi_tests::Get,
    n: crate::combat_abi_tests::Read,
    v: crate::combat_abi_tests::Read,
) {
    assert!(!READY.load(Ordering::Acquire));
    let a: unsafe extern "thiscall" fn(usize, usize, f32, u32, u32, u32) =
        std::mem::transmute(g(2));
    let b: unsafe extern "thiscall" fn(usize) -> u8 = std::mem::transmute(g(3));
    let c: unsafe extern "C" fn(usize, usize, f32) = std::mem::transmute(g(4));
    Body.initialize(a, body).unwrap();
    Explosion.initialize(b, explosion).unwrap();
    Deliver.initialize(c, deliver).unwrap();
    Body.enable().unwrap();
    Explosion.enable().unwrap();
    Deliver.enable().unwrap();
    for _ in 0..1000 {
        a(100, 200, 1.5, 3, 4, 5);
        assert_eq!(v(2), 312 + 1.5f32.to_bits());
        assert_eq!(b(400), 0xab);
        assert_eq!(v(3), 400);
        c(100, 200, 1.5);
        assert_eq!(v(4), 300 + 1.5f32.to_bits());
    }
    for i in 2..5 {
        assert_eq!(n(i), 1000);
    }
    Body.disable().unwrap();
    Explosion.disable().unwrap();
    Deliver.disable().unwrap();
    a(100, 200, 1.5, 3, 4, 5);
    assert_eq!(n(2), 1001);
    assert_eq!(b(400), 0xab);
    c(100, 200, 1.5);
    let mut object = vec![0u32; 0xc00 / 4];
    for (i, f) in [1.0f32, 2.0, 3.0].iter().enumerate() {
        object[0x9c / 4 + i] = f.to_bits();
    }
    for _ in 0..1000 {
        object[0x5dc / 4] = 100;
        assert!(apply_nonlethal(
            object.as_mut_ptr() as usize,
            77,
            100,
            50,
            std::mem::transmute(g(5))
        ));
        assert_eq!(object[0x5dc / 4], 50);
    }
    assert_eq!(n(5), 1000);
}

#[cfg(test)]
pub(super) fn fixture_health_policy() -> bool { HEALTH_BASED_EMP_ENABLED }

#[cfg(test)]
#[test]
fn native_emp_policy_does_not_install_half_health_hooks() {
    init(0x589860);
    assert!(!READY.load(Ordering::Acquire));
    assert!(!Body.is_enabled() && !Explosion.is_enabled() && !Deliver.is_enabled());
}
