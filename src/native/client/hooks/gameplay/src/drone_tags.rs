//! Native source-specific tag cancellation and end-of-life scan suppression.
//! B0F3B0 clears the tracked list, then enters crash state 3. B7EAB0 still
//! calls B51FC0 during that animation. Its target filter checks HP, not state3.
//! A disabled drone with positive HP can therefore acquire indefinite tags again.
//! Stop only that detector; preserve movement, damage, destruction and packets.
//! B93060 applies tag-immunity filters BEFORE testing active=false. A legitimate
//! end request can consequently leave an existing indefinite source behind.
//! Never clear HUD flags, other sources, source-zero requests or unknown layouts.
use crate::equipment_native::{readable, signature, word};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[path = "drone_view.rs"]
mod drone_view;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static SCAN_LOGS: AtomicU32 = AtomicU32::new(0);
type Clear = unsafe extern "thiscall" fn(usize);
type Remove = unsafe extern "thiscall" fn(usize, u32, u32, usize);
static_detour! {
    static Tag: unsafe extern "thiscall" fn(usize,u8,f32,u32,u32,usize,u8,f32);
    static ClearDrone: unsafe extern "thiscall" fn(usize);
    static Scan: unsafe extern "thiscall" fn(usize,f32);
}
// Exact verified retail DX11 fields; no process-wide pointer cache or timeout.
// Half-health living drones remain native. Unknown layouts/states also stay native.
unsafe fn retired(drone: usize) -> Option<bool> {
    if word(drone, 0)? != 0x02a88f54 {
        return None;
    }
    let state = word(drone, 0x910)? & 255;
    let hp = word(drone, 0x5dc)? as i32;
    Some(state == 3 || hp <= 0)
}
unsafe fn finish_retired_scan(drone: usize, clear: Clear) -> bool {
    if retired(drone) != Some(true) {
        return false;
    }
    // AF5F60 handles the bounded native tracking list, authority/owner and RPC.
    // Never emulate an active scan with an empty candidate list: B52219 would
    // turn a lost target into a new finite mark instead of cancelling it.
    clear(drone);
    true
}
unsafe extern "thiscall" fn clear_source(drone: usize) {
    clear_drone(drone);
}
fn scan(drone: usize, dt: f32) {
    unsafe {
        drone_view::observe(drone);
        if READY.load(Ordering::Acquire) && finish_retired_scan(drone, clear_source) {
            if SCAN_LOGS
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    (n < 8).then_some(n + 1)
                })
                .is_ok()
            {
                tracing::info!("SCBL_MOD_DRONE_TAG revision=31 retired_scan_suppressed=true state={:?} hp={:?} tracked_after={:?}",
                    word(drone,0x910).map(|v|v&255),word(drone,0x5dc).map(|v|v as i32),word(drone,0xbec));
            }
            return;
        }
        Scan.call(drone, dt);
    }
}
unsafe fn has_source(pawn: usize, low: u32, high: u32) -> Option<bool> {
    if low == 0 && high == 0 {
        return None;
    }
    if !matches!(word(pawn, 0)?, 0x02ad3234 | 0x02ad2abc) {
        return None;
    } // R50: Spy / Merc share B93060 / B84A50; only explicit source cancellation.
    let (p, n, cap) = (
        word(pawn, 0x1a90)? as usize,
        word(pawn, 0x1a94)?,
        word(pawn, 0x1a98)?,
    );
    if n > 64 || cap < n || cap > 256 {
        return None;
    }
    if n == 0 {
        return Some(false);
    }
    if !readable(p, n as usize * 16) {
        return None;
    }
    Some(
        (0..n as usize)
            .any(|i| word(p, i * 16 + 4) == Some(low) && word(p, i * 16 + 8) == Some(high)),
    )
}
unsafe fn finish_cancel(
    pawn: usize,
    active: u8,
    low: u32,
    high: u32,
    tagger: usize,
    remove: Remove,
) -> bool {
    if active != 0 || has_source(pawn, low, high) != Some(true) {
        return false;
    }
    // Native B84A50 removes this one 64-bit source and invokes native B5ED10
    // only when no sources remain. No artificial tag expiry / global reset.
    remove(pawn, low, high, tagger);
    has_source(pawn, low, high) == Some(false)
}
fn tag(
    pawn: usize,
    active: u8,
    duration: f32,
    low: u32,
    high: u32,
    tagger: usize,
    refresh: u8,
    cooldown: f32,
) {
    unsafe {
        Tag.call(pawn, active, duration, low, high, tagger, refresh, cooldown);
        if READY.load(Ordering::Acquire)
            && finish_cancel(
                pawn,
                active,
                low,
                high,
                tagger,
                std::mem::transmute::<usize, Remove>(0xb84a50),
            )
            && LOGS.fetch_add(1, Ordering::Relaxed) < 32
        {
            tracing::info!("SCBL_MOD_DRONE_TAG revision=31 source_cancel_repaired=true other_sources_preserved=true");
        }
    }
}
fn clear_drone(drone: usize) {
    unsafe {
        drone_view::clear_tracking(drone);
        let valid = READY.load(Ordering::Acquire) && word(drone, 0) == Some(0x02a88f54);
        let before = if valid { word(drone, 0xbec) } else { None };
        let owner_present = valid && word(drone, 0x8c).is_some_and(|x| x != 0);
        // AF5F60 does not destroy the actor. These are scalar diagnostics only.
        let role = if valid {
            word(drone, 0x4c).map(|x| (x >> 8) & 255)
        } else {
            None
        };
        ClearDrone.call(drone);
        if before.is_some_and(|x| x > 0) && LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
            tracing::info!("SCBL_MOD_DRONE_TAG revision=31 cleanup_before={:?} cleanup_after={:?} owner_present={} role={:?}",
                before,word(drone,0xbec),owner_present,role);
        }
    }
}
pub(super) fn source_cancel_ready() -> bool {
    READY.load(Ordering::Acquire)
}
pub(super) unsafe fn freeze_parked_view(drone: usize) { drone_view::freeze(drone); }
pub(super) fn resume_drone_view(drone: usize) { drone_view::forget(drone); }
pub(super) fn parked_view_ready() -> bool {
    drone_view::ready()
}
pub(crate) fn init(world: usize) {
    if world != 0x00589860 {
        return;
    }
    unsafe {
        if !signature(
            0xb51fc0,
            1154,
            "bc140743dce65e07162244d9f3424914eb25e92e0c7407a801e39931412239c6",
        ) || !signature(
            0xb0f3b0,
            579,
            "32ab1976fecca894518d20785ba67c1275c8449bfff2ff1109d48ccb0adb2fda",
        ) || !signature(
            0xb93060,
            311,
            "c092952c0fe98c168746acccdb60b931b41a4266a88c3d190f151f4d729db347",
        ) || !signature(
            0xb84a50,
            138,
            "47c029dc24bf4aa6b80f6effe4799fef33fc04519a407a48b42a458d5f49af6a",
        ) || !signature(
            0xaf5f60,
            247,
            "26af6839668d3a3d92ea6199b39087812937a2d6d2ee95017d477980beb3cb77",
        ) {
            tracing::error!("SCBL_MOD_DRONE_TAG enabled=false reason=native-signature");
            return;
        }
        if Tag
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(usize, u8, f32, u32, u32, usize, u8, f32),
                >(0xb93060),
                tag,
            )
            .is_err()
            || ClearDrone
                .initialize(
                    std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0xaf5f60),
                    clear_drone,
                )
                .is_err()
            || Scan
                .initialize(
                    std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0xb51fc0),
                    scan,
                )
                .is_err()
            || ClearDrone.enable().is_err()
            || Tag.enable().is_err()
            || Scan.enable().is_err()
        {
            let _ = Scan.disable();
            let _ = Tag.disable();
            let _ = ClearDrone.disable();
            return;
        }
    }
    READY.store(true, Ordering::Release);
    unsafe {
        drone_view::init();
    }
    tracing::info!(
        "SCBL_MOD_DRONE_TAG revision=31 source_specific_cancel=true active_scan=native retired_scan=disabled"
    );
}
pub(crate) unsafe fn deinit() {
    drone_view::deinit();
    READY.store(false, Ordering::Release);
    let _ = Scan.disable();
    let _ = Tag.disable();
    let _ = ClearDrone.disable();
}
#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "thiscall" fn clear_fixture(drone: usize) {
        let count = (drone + 0xbec) as *mut u32;
        count.write(0);
    }
    unsafe extern "thiscall" fn must_not_clear(_: usize) {
        panic!("a live or unknown drone must retain its native detection");
    }
    #[test]
    fn crash_state_cannot_reacquire_even_when_emp_left_positive_hp() {
        let mut d = vec![0u32; 0xc00 / 4];
        d[0] = 0x02a88f54;
        d[0x910 / 4] = 3;
        d[0x5dc / 4] = 300; // EMP operator cancellation may crash a nonzero-HP body.
        d[0xbec / 4] = 1;
        let p = d.as_mut_ptr() as usize;
        for _ in 0..120 {
            unsafe {
                assert!(finish_retired_scan(p, clear_fixture));
            }
            // Production wrapper does not call B51FC0 after a true result.
            assert_eq!(d[0xbec / 4], 0);
            assert_eq!(d[0x5dc / 4], 300);
            assert_eq!(d[0x910 / 4], 3);
        }
    }
    #[test]
    fn living_half_health_drone_and_new_generation_stay_native() {
        let mut d = vec![0u32; 0xc00 / 4];
        d[0] = 0x02a88f54;
        d[0x5dc / 4] = 300;
        d[0xbec / 4] = 1;
        let p = d.as_mut_ptr() as usize;
        for state in [0u32, 1, 2, 4, 255] {
            d[0x910 / 4] = state;
            unsafe {
                assert!(!finish_retired_scan(p, must_not_clear));
            }
            assert_eq!(d[0xbec / 4], 1);
        }
        d[0x910 / 4] = 3;
        unsafe {
            assert!(finish_retired_scan(p, clear_fixture));
        }
        d[0x910 / 4] = 1;
        d[0x5dc / 4] = 600;
        d[0xbec / 4] = 1;
        unsafe {
            assert!(!finish_retired_scan(p, must_not_clear));
        }
        assert_eq!(d[0xbec / 4], 1); // no tombstone poisoning a reused address
    }
    #[test]
    fn dead_drone_cleans_source_but_other_actor_classes_are_unchanged() {
        let mut d = vec![0u32; 0xc00 / 4];
        d[0] = 0x02a88f54;
        d[0x910 / 4] = 1;
        let p = d.as_mut_ptr() as usize;
        for hp in [0i32, -1] {
            d[0x5dc / 4] = hp as u32;
            d[0xbec / 4] = 2;
            unsafe {
                assert!(finish_retired_scan(p, clear_fixture));
            }
            assert_eq!(d[0xbec / 4], 0);
        }
        d[0] = 0x02ad3234;
        unsafe {
            assert_eq!(retired(p), None);
            assert!(!finish_retired_scan(p, must_not_clear));
            assert_eq!(retired(0), None);
        }
    }

    unsafe extern "thiscall" fn remove(pawn: usize, low: u32, high: u32, _tagger: usize) {
        let p = word(pawn, 0x1a90).unwrap() as usize;
        let n = word(pawn, 0x1a94).unwrap() as usize;
        for i in 0..n {
            if word(p, i * 16 + 4) == Some(low) && word(p, i * 16 + 8) == Some(high) {
                std::ptr::copy(
                    (p + (n - 1) * 16) as *const u32,
                    (p + i * 16) as *mut u32,
                    4,
                );
                ((pawn + 0x1a94) as *mut u32).write((n - 1) as u32);
                return;
            }
        }
        panic!("native remove must never be called without a matching source");
    }
    #[test]
    fn end_request_removes_only_requested_source_once() {
        let mut pawn = vec![0u32; 0x1b00 / 4];
        let mut marks = [
            (-1.0f32).to_bits(),
            21,
            22,
            30,
            (-1.0f32).to_bits(),
            41,
            42,
            50,
        ];
        pawn[0] = 0x02ad3234;
        pawn[0x1a90 / 4] = marks.as_mut_ptr() as u32;
        pawn[0x1a94 / 4] = 2;
        pawn[0x1a98 / 4] = 2;
        let p = pawn.as_mut_ptr() as usize;
        unsafe {
            assert!(!finish_cancel(p, 1, 21, 22, 30, remove)); // add unchanged
            assert!(!finish_cancel(p, 0, 0, 0, 30, remove)); // global clear unchanged
            assert!(!finish_cancel(p, 0, 99, 22, 30, remove)); // unrelated source
            assert!(finish_cancel(p, 0, 21, 22, 30, remove)); // immune early return repaired
            assert_eq!(pawn[0x1a94 / 4], 1);
            assert_eq!(marks[..4], [(-1.0f32).to_bits(), 41, 42, 50]);
            assert!(!finish_cancel(p, 0, 21, 22, 30, remove)); // duplicate harmless
            assert!(finish_cancel(p, 0, 41, 42, 50, remove));
            assert_eq!(pawn[0x1a94 / 4], 0);
        }
    }
    #[test]
    fn merc_received_end_removes_exact_source_without_touching_other_spies() {
        let mut pawn = vec![0u32; 0x1b00 / 4];
        let mut marks = [20.0f32.to_bits(), 11, 12, 13, 30.0f32.to_bits(), 21, 22, 23];
        pawn[0] = 0x02ad2abc;
        pawn[0x1a90 / 4] = marks.as_mut_ptr() as u32;
        pawn[0x1a94 / 4] = 2;
        pawn[0x1a98 / 4] = 2;
        let p = pawn.as_mut_ptr() as usize;
        unsafe {
            assert!(!finish_cancel(p, 1, 11, 12, 13, remove));
            assert!(finish_cancel(p, 0, 11, 12, 13, remove));
            assert_eq!(has_source(p, 11, 12), Some(false));
            assert_eq!(has_source(p, 21, 22), Some(true));
            assert!(!finish_cancel(p, 0, 11, 12, 13, remove));
        }
    }
    #[test]
    fn unknown_layout_and_corrupt_array_preserve_native() {
        unsafe {
            assert!(!finish_cancel(0, 0, 1, 2, 0, remove));
        }
        let mut pawn = vec![0u32; 0x1b00 / 4];
        let p = pawn.as_mut_ptr() as usize;
        pawn[0] = 0x02ad2abd; // unknown actor, not Spy or Merc
        unsafe {
            assert_eq!(has_source(p, 1, 2), None);
        }
        pawn[0] = 0x02ad3234;
        pawn[0x1a94 / 4] = 65;
        pawn[0x1a98 / 4] = 65;
        unsafe {
            assert_eq!(has_source(p, 1, 2), None);
        }
        pawn[0x1a94 / 4] = 1;
        pawn[0x1a98 / 4] = 0;
        unsafe {
            assert_eq!(has_source(p, 1, 2), None);
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
    let a: unsafe extern "thiscall" fn(usize, u8, f32, u32, u32, usize, u8, f32) =
        std::mem::transmute(g(6));
    let b: unsafe extern "thiscall" fn(usize) = std::mem::transmute(g(7));
    let d: unsafe extern "thiscall" fn(usize, f32) = std::mem::transmute(g(8));
    Scan.initialize(d, scan).unwrap();
    Tag.initialize(a, tag).unwrap();
    ClearDrone.initialize(b, clear_drone).unwrap();
    Scan.enable().unwrap();
    Tag.enable().unwrap();
    ClearDrone.enable().unwrap();
    for _ in 0..1000 {
        a(100, 1, 2.0, 3, 4, 5, 1, 6.0);
        assert_eq!(v(6), 122);
        b(200);
        assert_eq!(v(7), 200);
        d(300, 1.25);
        assert_eq!(v(8), 300 + 1.25f32.to_bits());
    }
    assert_eq!(n(6), 1000);
    assert_eq!(n(7), 1000);
    assert_eq!(n(8), 1000);
    let mut object = vec![0u32; 0xc00 / 4];
    object[0] = 0x02a88f54;
    object[0x910 / 4] = 1;
    object[0x5dc / 4] = 300;
    let p = object.as_mut_ptr() as usize;
    READY.store(true, Ordering::Release);
    d(p, 0.016);
    assert_eq!(n(8), 1001); // live original detector exactly once
    object[0x910 / 4] = 3;
    d(p, 0.016);
    assert_eq!(n(8), 1001); // actual detour suppresses crashed detector
    assert_eq!(n(7), 1001); // exact native clear trampoline
    object[0x910 / 4] = 1;
    d(p, 0.016);
    assert_eq!(n(8), 1002); // no retained pointer state
    READY.store(false, Ordering::Release);
    Scan.disable().unwrap();
    Tag.disable().unwrap();
    ClearDrone.disable().unwrap();
    a(100, 1, 2.0, 3, 4, 5, 1, 6.0);
    assert_eq!(n(6), 1001);
    d(300, 1.25);
    assert_eq!(n(8), 1003);
}
