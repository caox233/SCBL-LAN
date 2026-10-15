//! DX11 paired melee adapter. No world delta, reach, damage or input changes.
use crate::{
    equipment_native::{armor, equipped, signature, word, writable},
    feature_contract::*,
};
use retour::static_detour;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};
const DRIVER: u32 = 0x02beeea4;
const NERVE: u32 = 0x02ad45e4;
const GENERIC_NERVE: u32 = 0x02ad4314;
// Retail DFA execution at 0x1579EDE uses the same paired factory but the
// generic UEPLNerve. 0x14D3F90 is only a PREVIEW query and must not be hooked.
fn paired_kill_nerve(nerve_vt: u32, state_vt: u32) -> bool {
    nerve_vt == NERVE
        || (nerve_vt == GENERIC_NERVE
            && matches!(
                state_vt,
                0x02bf073c | 0x02bf0984 | 0x02bf0bcc | 0x02bf1794 | 0x02bebc3c
            ))
}
const SPY: u32 = 1 << 18;
const MERC: u32 = 1 << 19;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static PAIRS: Mutex<[Option<Pair>; 24]> = Mutex::new([None; 24]);
#[derive(Clone, Copy, Debug)]
struct Speed {
    pawn: usize,
    object: u32,
    graph: usize,
    before: u32,
    applied: u32,
}
#[derive(Clone, Copy, Debug)]
struct Pair {
    driver: usize,
    nerve: usize,
    owner: Speed,
    target: Speed,
}
static_detour! {
    static Create: unsafe extern "thiscall" fn(usize,u32,usize,usize,usize,usize,i32)->usize;
    static Finish: unsafe extern "thiscall" fn(usize);
}
unsafe fn graph(pawn: usize) -> Option<usize> {
    // Do not guess proxy/special skeleton paths.
    if word(pawn, 0x944)? & 0x10000000 != 0 {
        return None;
    }
    let mesh = word(pawn, 0x1f0)? as usize;
    let g = word(mesh, 0xc4)? as usize;
    (writable(g.checked_add(0x7f8)?, 4)).then_some(g)
}
fn adjusted(before: f32, factor: f32) -> Option<f32> {
    let native = if before == -1.0 { 1.0 } else { before };
    (native.is_finite()
        && (0.1..=4.0).contains(&native)
        && factor.is_finite()
        && (1.0..=1.5).contains(&factor))
    .then_some(native * factor)
}
unsafe fn snapshot(pawn: usize, factor: f32) -> Option<Speed> {
    let g = graph(pawn)?;
    let before = word(g, 0x7f8)?;
    Some(Speed {
        pawn,
        object: word(pawn, 0x20)?,
        graph: g,
        before,
        applied: adjusted(f32::from_bits(before), factor)?.to_bits(),
    })
}
unsafe fn restore(s: Speed) {
    // Compare ownership and value: never overwrite a new native animation.
    if word(s.pawn, 0x20) == Some(s.object)
        && graph(s.pawn) == Some(s.graph)
        && word(s.graph, 0x7f8) == Some(s.applied)
    {
        ((s.graph + 0x7f8) as *mut u32).write_unaligned(s.before);
    }
}
unsafe fn restore_pair(pair: Pair) {
    restore(pair.owner);
    restore(pair.target);
}
unsafe fn attach(
    driver: usize,
    controller: usize,
    state: usize,
    target: usize,
    factor: f32,
) -> bool {
    if factor <= 1.0 || word(driver, 0) != Some(DRIVER) || word(driver, 0x30) != Some(state as u32)
    {
        return false;
    }
    let owner = word(driver, 0x28).unwrap_or(0) as usize;
    let nerve = word(driver, 0x2c).unwrap_or(0) as usize;
    if word(controller, 0) != Some(0x02bf80ac)
        || word(controller, 0x28) != Some(owner as u32)
        || word(state, 0x48) != Some(owner as u32)
        || !paired_kill_nerve(word(nerve, 0).unwrap_or(0), word(state, 0).unwrap_or(0))
        || word(nerve, 0x28) != Some(owner as u32)
        || word(owner, 0x374).map(|x| x & (SPY | MERC)) != Some(SPY)
        || word(target, 0x374).map(|x| x & (SPY | MERC)) != Some(MERC)
    {
        return false;
    }
    let Some(a) = armor(owner) else { return false };
    if !equipped(a, b"MPArmorGlovesGhost") {
        return false;
    }
    // Factory adds the target participant before the paired animation starts.
    let (Some(parts), Some(n), Some(cap)) =
        (word(nerve, 0x40), word(nerve, 0x44), word(nerve, 0x48))
    else {
        return false;
    };
    if n == 0
        || n > 2
        || cap < n
        || cap > 8
        || !(0..n as usize).any(|i| {
            word(parts as usize, i * 4).and_then(|p| word(p as usize, 0x28)) == Some(target as u32)
        })
    {
        return false;
    }
    let Ok(mut pairs) = PAIRS.lock() else {
        return false;
    };
    // Factory may replace an older paired driver; remove owned overrides first.
    for slot in pairs.iter_mut() {
        if slot
            .is_some_and(|p| p.driver == driver || p.owner.pawn == owner || p.target.pawn == target)
        {
            restore_pair(slot.take().unwrap());
        }
    }
    let Some(slot) = pairs.iter_mut().find(|p| p.is_none()) else {
        return false;
    };
    let (Some(a), Some(b)) = (snapshot(owner, factor), snapshot(target, factor)) else {
        return false;
    };
    if a.graph == b.graph {
        return false;
    }
    ((a.graph + 0x7f8) as *mut u32).write_unaligned(a.applied);
    ((b.graph + 0x7f8) as *mut u32).write_unaligned(b.applied);
    *slot = Some(Pair {
        driver,
        nerve,
        owner: a,
        target: b,
    });
    true
}
unsafe fn detach(driver: usize) {
    if let Ok(mut pairs) = PAIRS.lock() {
        for slot in pairs.iter_mut() {
            if slot.is_some_and(|p| p.driver == driver) {
                let pair = slot.take().unwrap();
                if word(driver, 0) == Some(DRIVER) && word(driver, 0x2c) == Some(pair.nerve as u32)
                {
                    restore_pair(pair);
                }
            }
        }
    }
}
fn create(
    controller: usize,
    selector: u32,
    state: usize,
    target: usize,
    bind: usize,
    class: usize,
    mode: i32,
) -> usize {
    let result = unsafe { Create.call(controller, selector, state, target, bind, class, mode) };
    if READY.load(Ordering::Acquire) {
        let mut p = FeaturePacket::new(EVENT_MELEE_SPEED);
        p.words[0] = 1;
        let factor = crate::live_rules::feature(p).map_or(1.0, |x| f32::from_bits(x.words[0]));
        if unsafe { attach(result, controller, state, target, factor) }
            && LOGS.fetch_add(1, Ordering::Relaxed) < 12
        {
            tracing::info!(
                "SCBL_MOD_MELEE_SPEED paired=true multiplier={} native_reach=unchanged revision=30",
                factor
            );
        }
    }
    result
}
fn finish(driver: usize) {
    unsafe {
        detach(driver);
        Finish.call(driver);
    }
}
pub(crate) fn init(world: usize) {
    if world != 0x00589860 {
        return;
    }
    unsafe {
        if !signature(
            0x14be9b0,
            202,
            "2406e680e2a774b0c1d08fc0fdfc4cdaf8bdfaf398784c9b7435e4eb5448171e",
        ) || !signature(
            0x1468410,
            32,
            "aaa7024f68ee782dedc29abe8cca32184801e9ad97b644f93c625df282d02402",
        ) || !signature(
            0x1440bc0,
            104,
            "38d246ce58488ac2949292d5a2a9f1b16850349b7730114283e2e1b5f0fc1cae",
        ) {
            tracing::error!("SCBL_MOD_MELEE_SPEED enabled=false reason=native-signature");
            return;
        }
        if Create
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(
                        usize,
                        u32,
                        usize,
                        usize,
                        usize,
                        usize,
                        i32,
                    ) -> usize,
                >(0x14be9b0),
                create,
            )
            .is_err()
            || Finish
                .initialize(
                    std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0x1468410),
                    finish,
                )
                .is_err()
            || Finish.enable().is_err()
            || Create.enable().is_err()
        {
            let _ = Create.disable();
            let _ = Finish.disable();
            return;
        }
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_MELEE_SPEED adapters=true revision=30 paired_graph_multiplier=true hot_child=true");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Create.disable();
    if let Ok(mut pairs) = PAIRS.lock() {
        for p in pairs.iter_mut() {
            if let Some(p) = p.take() {
                restore_pair(p)
            }
        }
    }
    let _ = Finish.disable();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_animation_is_only_accepted_for_confirmed_kill_states() {
        for state in [0x02bf073c, 0x02bf0984, 0x02bf0bcc, 0x02bf1794, 0x02bebc3c] {
            assert!(paired_kill_nerve(GENERIC_NERVE, state));
        }
        assert!(!paired_kill_nerve(GENERIC_NERVE, 0));
        assert!(!paired_kill_nerve(GENERIC_NERVE, 0x02bc91cc)); // climb, not kill
        assert!(!paired_kill_nerve(0, 0x02bf073c));
    }
    #[test]
    fn speed_uses_native_sentinel_and_preserves_other_native_rates() {
        assert_eq!(adjusted(-1.0, 1.5), Some(1.5));
        assert_eq!(adjusted(1.0, 1.5), Some(1.5));
        assert_eq!(adjusted(0.8, 1.5), Some(1.2));
        for n in [f32::NAN, 0.0, -2.0, 10.0] {
            assert_eq!(adjusted(n, 1.5), None);
        }
    }
    #[test]
    fn invalid_pointers_do_not_change_memory() {
        unsafe {
            assert!(!attach(0, 0, 0, 0, 1.5));
            detach(0);
        }
    }
    #[test]
    fn paired_glove_adapter_installs_and_restores_both_participants() {
        let mut owner = vec![0u32; 0x2000 / 4];
        let mut target = vec![0u32; 0x2000 / 4];
        let mut meshes = [vec![0u32; 0x100 / 4], vec![0u32; 0x100 / 4]];
        let mut graphs = [vec![0u32; 0x900 / 4], vec![0u32; 0x900 / 4]];
        for (i, p) in [&mut owner, &mut target].iter_mut().enumerate() {
            p[0x20 / 4] = 100 + i as u32;
            p[0x1f0 / 4] = meshes[i].as_mut_ptr() as u32;
            meshes[i][0xc4 / 4] = graphs[i].as_mut_ptr() as u32;
            graphs[i][0x7f8 / 4] = (-1.0f32).to_bits();
        }
        owner[0x374 / 4] = SPY;
        target[0x374 / 4] = MERC;
        let op = owner.as_mut_ptr() as u32;
        let tp = target.as_mut_ptr() as u32;
        let mut armor = vec![0u32; 0x100 / 4];
        armor[0] = 0x02ad42ac;
        let name = b"MPArmorGlovesGhost";
        let mut part = vec![0u32; 0x40 / 4];
        part[0x28 / 4] = name.as_ptr() as u32;
        part[0x2c / 4] = name.len() as u32;
        part[0x30 / 4] = name.len() as u32;
        let parts = [part.as_mut_ptr() as u32];
        armor[0x50 / 4] = parts.as_ptr() as u32;
        armor[0x54 / 4] = 1;
        armor[0x58 / 4] = 1;
        owner[0xcf4 / 4] = armor.as_mut_ptr() as u32;
        let mut state = vec![0u32; 0x100 / 4];
        state[0x48 / 4] = op;
        let mut participant = vec![0u32; 0x80 / 4];
        participant[0x28 / 4] = tp;
        let participants = [participant.as_ptr() as u32];
        let mut nerve = vec![0u32; 0x100 / 4];
        nerve[0] = NERVE;
        nerve[0x28 / 4] = op;
        nerve[0x40 / 4] = participants.as_ptr() as u32;
        nerve[0x44 / 4] = 1;
        nerve[0x48 / 4] = 1;
        let mut driver = vec![0u32; 0x100 / 4];
        driver[0] = DRIVER;
        driver[0x28 / 4] = op;
        driver[0x2c / 4] = nerve.as_mut_ptr() as u32;
        driver[0x30 / 4] = state.as_mut_ptr() as u32;
        let mut controller = vec![0u32; 0x100 / 4];
        controller[0] = 0x02bf80ac;
        controller[0x28 / 4] = op;
        let (d, ctrl, st) = (
            driver.as_mut_ptr() as usize,
            controller.as_mut_ptr() as usize,
            state.as_mut_ptr() as usize,
        );
        unsafe {
            assert!(attach(d, ctrl, st, tp as usize, 1.5));
            assert_eq!(graphs[0][0x7f8 / 4], 1.5f32.to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], 1.5f32.to_bits());
            // Repeated setup must not compound 1.5 into 2.25.
            assert!(attach(d, ctrl, st, tp as usize, 1.5));
            assert_eq!(graphs[0][0x7f8 / 4], 1.5f32.to_bits());
            detach(d);
            assert_eq!(graphs[0][0x7f8 / 4], (-1.0f32).to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], (-1.0f32).to_bits());
            let current_factor = crate::combat_rules::melee_speed(true);
            assert_eq!(current_factor, 1.20);
            assert!(attach(d, ctrl, st, tp as usize, current_factor));
            assert_eq!(graphs[0][0x7f8 / 4], current_factor.to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], current_factor.to_bits());
            assert!(attach(d, ctrl, st, tp as usize, current_factor));
            assert_eq!(graphs[0][0x7f8 / 4], current_factor.to_bits());
            detach(d);
            assert_eq!(graphs[0][0x7f8 / 4], (-1.0f32).to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], (-1.0f32).to_bits());
            // DFA shares the paired factory/finish but NOT UHandToHandNerve.
            nerve[0] = GENERIC_NERVE;
            state[0] = 0x02bf073c;
            assert!(attach(d, ctrl, st, tp as usize, 1.3));
            assert_eq!(graphs[0][0x7f8 / 4], 1.3f32.to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], 1.3f32.to_bits());
            detach(d);
            assert_eq!(graphs[0][0x7f8 / 4], (-1.0f32).to_bits());
            assert!(attach(d, ctrl, st, tp as usize, current_factor));
            assert_eq!(graphs[0][0x7f8 / 4], current_factor.to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], current_factor.to_bits());
            detach(d);
            assert_eq!(graphs[0][0x7f8 / 4], (-1.0f32).to_bits());
            assert_eq!(graphs[1][0x7f8 / 4], (-1.0f32).to_bits());
            state[0] = 0x02bc91cc;
            assert!(!attach(d, ctrl, st, tp as usize, 1.3));
            state[0] = 0x02bf073c;
            armor[0x54 / 4] = 0;
            assert!(!attach(d, ctrl, st, tp as usize, 1.5));
            assert_eq!(graphs[0][0x7f8 / 4], (-1.0f32).to_bits());
        }
    }
    #[test]
    fn restoration_only_changes_the_owned_speed() {
        let mut pawn = vec![0u32; 0x2000 / 4];
        let mut mesh = vec![0u32; 0x100 / 4];
        let mut g = vec![0u32; 0x900 / 4];
        pawn[0x20 / 4] = 77;
        pawn[0x1f0 / 4] = mesh.as_mut_ptr() as u32;
        mesh[0xc4 / 4] = g.as_mut_ptr() as u32;
        g[0x7f8 / 4] = 1.5f32.to_bits();
        let s = Speed {
            pawn: pawn.as_mut_ptr() as usize,
            object: 77,
            graph: g.as_mut_ptr() as usize,
            before: (-1.0f32).to_bits(),
            applied: 1.5f32.to_bits(),
        };
        unsafe { restore(s) };
        assert_eq!(g[0x7f8 / 4], (-1.0f32).to_bits());
        g[0x7f8 / 4] = 2.0f32.to_bits();
        unsafe { restore(s) };
        assert_eq!(g[0x7f8 / 4], 2.0f32.to_bits());
        g[0x7f8 / 4] = s.applied;
        pawn[0x20 / 4] = 78;
        unsafe { restore(s) };
        assert_eq!(g[0x7f8 / 4], s.applied);
    }
}

#[cfg(test)]
pub(super) unsafe fn fixture_abi(
    g: crate::combat_abi_tests::Get,
    n: crate::combat_abi_tests::Read,
    v: crate::combat_abi_tests::Read,
) {
    assert!(!READY.load(Ordering::Acquire));
    let a: unsafe extern "thiscall" fn(usize, u32, usize, usize, usize, usize, i32) -> usize =
        std::mem::transmute(g(0));
    let b: unsafe extern "thiscall" fn(usize) = std::mem::transmute(g(1));
    Create.initialize(a, create).unwrap();
    Finish.initialize(b, finish).unwrap();
    Create.enable().unwrap();
    Finish.enable().unwrap();
    for _ in 0..1000 {
        assert_eq!(a(100, 2, 3, 4, 5, 6, 7), 127);
        b(200);
        assert_eq!(v(1), 200);
    }
    assert_eq!(n(0), 1000);
    assert_eq!(n(1), 1000);
    Create.disable().unwrap();
    Finish.disable().unwrap();
    assert_eq!(a(100, 2, 3, 4, 5, 6, 7), 127);
}
