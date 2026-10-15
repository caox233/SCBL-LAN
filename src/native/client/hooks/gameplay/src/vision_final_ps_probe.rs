//! C13 diagnostic only: bind one verified material return to this native render call's
//! exact PS-lookup CALL and draw CALL. No shader, device or render-state writes.
use super::material_observer::{self, VerifiedToken};
use crate::equipment_native::{readable, signature, word};
use retour::GenericDetour;
use std::{
    cell::Cell,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};

const OUTER_AT: usize = 0x016e_44d0;
const PS_SITE: usize = 0x016e_5644;
const PS_TARGET: usize = 0x0173_aa50;
const DRAW_SITE: usize = 0x016e_5744;
const DRAW_TARGET: usize = 0x0160_6290;
// The original native outer has two stack arguments and RET 8.
type OuterFn = unsafe extern "stdcall" fn(usize, usize) -> usize;
// The original lookup has four stack arguments and bare RET (cdecl).
type PsFn = unsafe extern "C" fn(usize, usize, u32, u32) -> usize;
// The original draw uses ECX=device, six stack arguments and RET 18h.
type DrawFn = unsafe extern "thiscall" fn(usize, usize, usize, usize, usize, usize, usize) -> usize;
static OUTER: OnceLock<GenericDetour<OuterFn>> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);
static PATCHES: Mutex<[Option<Patch>; 2]> = Mutex::new([None, None]);
#[derive(Clone, Copy)]
struct Patch {
    site: usize,
    old: [u8; 5],
    new: [u8; 5],
    protection: u32,
}
#[derive(Clone, Copy)]
struct Frame {
    render: usize,
    record: usize,
    token: Option<VerifiedToken>,
    ps: bool,
    draw: bool,
    device_matches: bool,
    family: u32,
    masked_lo: Option<u32>,
    masked_hi: Option<u32>,
    shader: Option<u32>,
    lookup: usize,
    device: usize,
}
impl Frame {
    const fn new(render: usize, record: usize) -> Self {
        Self {
            render,
            record,
            token: None,
            ps: false,
            draw: false,
            device_matches: false,
            family: 0,
            masked_lo: None,
            masked_hi: None,
            shader: None,
            lookup: 0,
            device: 0,
        }
    }
}
thread_local! { static FRAME: Cell<Option<Frame>> = const { Cell::new(None) }; }
#[derive(Clone, Copy, PartialEq, Eq)]
struct Summary {
    round: u8,
    actor: u32,
    serial: u32,
    selected: usize,
    surface: usize,
    key: u32,
    category: u8,
    strength: u8,
    outcome: u8,
    family: u32,
    lo: Option<u32>,
    hi: Option<u32>,
    shader: Option<u32>,
    lookup: usize,
    device_matches: bool,
}
struct Logs {
    round: u8,
    count: u8,
    epoch: Option<u64>,
    last: Option<Summary>,
    latest: Option<(Summary, Frame)>,
}
static LOGS: Mutex<Logs> = Mutex::new(Logs {
    round: 0,
    count: 0,
    epoch: None,
    last: None,
    latest: None,
});
// Additional record-link reads are paid only after an observer-verified token.
// Reserve before RPM; per-second reset avoids exhausting the 30-s late symptom.
struct ReadBudget {
    round: u8,
    epoch: u64,
    used: u8,
}
static READ_BUDGET: Mutex<ReadBudget> = Mutex::new(ReadBudget {
    round: 0,
    epoch: u64::MAX,
    used: 0,
});
fn admit_record_read(b: &mut ReadBudget, round: u8, elapsed_ms: u64) -> bool {
    let epoch = elapsed_ms / 1000;
    if round < b.round {
        return false;
    }
    if b.round != round || b.epoch != epoch {
        b.round = round;
        b.epoch = epoch;
        b.used = 0;
    }
    if b.used >= 8 {
        return false;
    }
    b.used += 1;
    true
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualProtect(p: *mut c_void, n: usize, protection: u32, old: *mut u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(h: *mut c_void, p: *const c_void, n: usize) -> i32;
}
#[inline]
fn build_enabled() -> bool {
    material_observer::build_enabled()
}
fn rel_call(site: usize, target: usize) -> [u8; 5] {
    let mut b = [0xe8, 0, 0, 0, 0];
    b[1..].copy_from_slice(&(target as u32).wrapping_sub(site as u32 + 5).to_le_bytes());
    b
}
fn complete_restore(p: &mut Option<Patch>, matched: bool, protected: bool, flushed: bool) -> bool {
    if matched && protected && flushed {
        *p = None;
        true
    } else {
        false
    }
}
unsafe fn restore(p: &mut Option<Patch>) -> bool {
    let Some(j) = *p else { return true };
    if !readable(j.site, 5) {
        return false;
    }
    let current = std::slice::from_raw_parts(j.site as *const u8, 5);
    if current == j.old {
        // A previous attempt may have copied old bytes but failed protection/ICache.
        // Do not discard the journal until both operations actually succeed.
        let mut ignored = 0;
        let protected = VirtualProtect(j.site as _, 5, j.protection, &mut ignored) != 0;
        let flushed = FlushInstructionCache(GetCurrentProcess(), j.site as _, 5) != 0;
        return complete_restore(p, true, protected, flushed);
    }
    if current != j.new {
        return false;
    } // foreign change: do not overwrite it
    let mut prior = 0;
    if VirtualProtect(j.site as _, 5, 0x40, &mut prior) == 0 {
        return false;
    }
    std::ptr::copy_nonoverlapping(j.old.as_ptr(), j.site as *mut u8, 5);
    let matched = std::slice::from_raw_parts(j.site as *const u8, 5) == j.old;
    let mut ignored = 0;
    let protected = VirtualProtect(j.site as _, 5, j.protection, &mut ignored) != 0;
    let flushed = FlushInstructionCache(GetCurrentProcess(), j.site as _, 5) != 0;
    complete_restore(p, matched, protected, flushed)
}
unsafe fn install_call(site: usize, target: usize, bridge: usize, p: &mut Option<Patch>) -> bool {
    if p.is_some() || !readable(site, 5) {
        return false;
    }
    let old = rel_call(site, target);
    if std::slice::from_raw_parts(site as *const u8, 5) != old {
        return false;
    }
    let new = rel_call(site, bridge);
    let mut protection = 0;
    if VirtualProtect(site as _, 5, 0x40, &mut protection) == 0 {
        return false;
    }
    std::ptr::copy_nonoverlapping(new.as_ptr(), site as *mut u8, 5);
    let matched = std::slice::from_raw_parts(site as *const u8, 5) == new;
    let mut ignored = 0;
    let protected = VirtualProtect(site as _, 5, protection, &mut ignored) != 0;
    let flushed = FlushInstructionCache(GetCurrentProcess(), site as _, 5) != 0;
    *p = Some(Patch {
        site,
        old,
        new,
        protection,
    });
    if matched && protected && flushed {
        true
    } else {
        let _ = restore(p);
        false
    }
}
fn call_once<F: FnOnce() -> usize, A: FnOnce() -> bool, G: FnOnce(usize)>(
    enabled: bool,
    still_active: A,
    original: F,
    after: G,
) -> usize {
    let result = original();
    if enabled && still_active() {
        after(result);
    }
    result
}
fn bind(
    f: &mut Frame,
    t: VerifiedToken,
    object: Option<usize>,
    surface: Option<usize>,
    root: Option<usize>,
) -> bool {
    if f.token.is_some()
        || f.render != t.render
        || object != Some(t.object)
        || surface != Some(t.surface)
        || root != Some(t.root)
    {
        return false;
    }
    f.token = Some(t);
    true
}
fn mark_ps(
    f: &mut Frame,
    device: usize,
    expected_device: Option<usize>,
    family: usize,
    lo: u32,
    hi: u32,
    masks: Option<(u32, u32)>,
    shader: Option<u32>,
    lookup: usize,
) {
    if f.token.is_none() || f.ps {
        return;
    }
    f.ps = true;
    f.device = device;
    f.family = family as u32;
    f.lookup = lookup;
    f.device_matches = expected_device == Some(device);
    if f.device_matches {
        f.shader = shader;
        if let Some((ml, mh)) = masks {
            f.masked_lo = Some(lo & ml);
            f.masked_hi = Some(hi & mh);
        }
    }
}
fn mark_draw(f: &mut Frame, device: usize) {
    if f.token.is_some() && f.ps && f.device_matches && f.device == device {
        f.draw = true;
    }
}
fn outcome(f: Frame) -> Option<u8> {
    let t = f.token?;
    Some(if t.selected == 0 {
        0
    } else if f.draw {
        3
    } else if f.ps {
        2
    } else {
        1
    })
}
fn summary(frame: Frame) -> Option<Summary> {
    let t = frame.token?;
    Some(Summary {
        round: t.round,
        actor: t.actor,
        serial: t.serial,
        selected: t.selected,
        surface: t.surface,
        key: t.key,
        category: t.category,
        strength: t.strength,
        outcome: outcome(frame)?,
        family: frame.family,
        lo: frame.masked_lo,
        hi: frame.masked_hi,
        shader: frame.shader,
        lookup: frame.lookup,
        device_matches: frame.device_matches,
    })
}
fn emit(frame: Frame, summary: Summary) {
    let Some(t) = frame.token else {
        return;
    };
    let state = match summary.outcome {
        0 => "native_null",
        1 => "selected_no_final_ps",
        2 => "ps_lookup_no_draw",
        _ => "draw_call_returned",
    };
    tracing::info!("SCBL_THERMAL_FINAL_PS state={} round={} elapsed_ms={} stage={} actor={:#x} serial={} selected={:#x} surface={:#x} key={:#x} category={} strength={} family={:#x} masked_lo={:?} masked_hi={:?} device_match={} device_6c={:?} lookup_eax={:#x} actual_draw_call={} early_skip={} writes=0 gpu_pixel_unverified=true",
        state, t.round, t.elapsed_ms, t.stage, t.actor, t.serial, t.selected, t.surface, t.key,
        t.category, t.strength, frame.family, frame.masked_lo, frame.masked_hi,
        frame.device_matches, frame.shader, frame.lookup, frame.draw, !frame.draw);
}
fn select_log(logs: &mut Logs, frame: Frame) -> Option<(Summary, Frame)> {
    let t = frame.token?;
    let summary = summary(frame)?;
    if t.round < logs.round {
        return None;
    } // nested old round returned after a newer one
    if logs.round != t.round {
        logs.round = t.round;
        logs.count = 0;
        logs.epoch = None;
        logs.last = None;
        logs.latest = None;
    }
    if logs
        .latest
        .is_some_and(|(_, previous)| previous.token.is_some_and(|p| p.elapsed_ms > t.elapsed_ms))
    {
        return None;
    }
    logs.latest = Some((summary, frame));
    // Monotone cadence: a nested older outer may finish after a newer outer.
    let epoch = t.elapsed_ms / 1000;
    if logs.epoch.is_some_and(|last| epoch <= last) {
        return None;
    }
    logs.epoch = Some(epoch);
    if logs.last == Some(summary) || logs.count >= 31 {
        return None;
    }
    logs.last = Some(summary);
    logs.count += 1;
    Some((summary, frame))
}
fn take_final(logs: &mut Logs, round: u8) -> Option<(Summary, Frame)> {
    if logs.round != round {
        return None;
    }
    let (summary, frame) = logs.latest?;
    if logs.last == Some(summary) || logs.count >= 32 {
        return None;
    }
    logs.last = Some(summary);
    logs.count += 1;
    Some((summary, frame))
}
fn note(frame: Frame) {
    let Ok(mut logs) = LOGS.try_lock() else {
        return;
    };
    let selected = select_log(&mut logs, frame);
    drop(logs);
    if let Some((summary, frame)) = selected {
        emit(frame, summary);
    }
}
pub(super) fn finish_round(round: u8) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Ok(mut logs) = LOGS.try_lock() else {
        return;
    };
    let selected = take_final(&mut logs, round);
    drop(logs);
    if let Some((summary, frame)) = selected {
        emit(frame, summary);
    }
}
struct FrameGuard {
    previous: Option<Frame>,
}
impl FrameGuard {
    fn enter(current: Option<Frame>) -> Self {
        Self {
            previous: FRAME.with(|cell| cell.replace(current)),
        }
    }
}
impl Drop for FrameGuard {
    fn drop(&mut self) {
        FRAME.with(|cell| cell.set(self.previous));
    }
}
unsafe extern "stdcall" fn outer(render: usize, record: usize) -> usize {
    let hook = OUTER.get().expect("published before enable");
    if !READY.load(Ordering::Acquire) {
        return hook.call(render, record);
    }
    if !material_observer::capturing() {
        // A nested render after a view-scope switch must not inherit its parent's token.
        if FRAME.with(Cell::get).is_none() {
            return hook.call(render, record);
        }
        let _shadow = FrameGuard::enter(None);
        return hook.call(render, record);
    }
    let guard = FrameGuard::enter(Some(Frame::new(render, record)));
    let result = hook.call(render, record);
    let completed = FRAME.with(Cell::get);
    drop(guard); // RAII restores on both normal and unwinding exits.
    if READY.load(Ordering::Acquire) && material_observer::capturing() {
        if let Some(frame) = completed {
            note(frame);
        }
    }
    result
}
// Called immediately after the existing material observer; consumes exactly one
// double-validated token and requires this outer render's actual record arguments.
pub(super) fn material_verified(token: Option<VerifiedToken>) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(t) = token else {
        return;
    };
    FRAME.with(|cell| {
        let Some(mut f) = cell.get() else {
            return;
        };
        if f.token.is_some() || f.render != t.render {
            return;
        }
        let Ok(mut budget) = READ_BUDGET.try_lock() else {
            return;
        };
        if !admit_record_read(&mut budget, t.round, t.elapsed_ms) {
            return;
        }
        drop(budget);
        let object = unsafe { word(f.record, 0) }.map(|v| v as usize);
        let surface = unsafe { word(f.record, 0x1c) }.map(|v| v as usize);
        let root = unsafe { word(f.record, 0x20) }.map(|v| v as usize);
        if bind(&mut f, t, object, surface, root) {
            cell.set(Some(f));
        }
    });
}
unsafe extern "C" fn ps(device: usize, family: usize, lo: u32, hi: u32) -> usize {
    let original: PsFn = std::mem::transmute(PS_TARGET);
    call_once(
        READY.load(Ordering::Acquire),
        || READY.load(Ordering::Acquire) && material_observer::capturing(),
        || unsafe { original(device, family, lo, hi) },
        |result| {
            FRAME.with(|cell| {
                let Some(mut f) = cell.get() else {
                    return;
                };
                if f.token.is_none() || f.ps {
                    return;
                }
                // Exact CALL 16E5644: only a token from this outer render is eligible.
                let expected = unsafe { word(f.render, 0xc) }.map(|v| v as usize);
                let matches = expected == Some(device);
                let masks = if matches {
                    let base = unsafe { word(0x0344_a658, 0) };
                    let off = family.checked_mul(8);
                    match (base, off) {
                        (Some(base), Some(off)) => unsafe { word(base as usize, off) }
                            .zip(unsafe { word(base as usize, off + 4) }),
                        _ => None,
                    }
                } else {
                    None
                };
                let shader = if matches {
                    unsafe { word(device, 0x6c) }
                } else {
                    None
                };
                mark_ps(
                    &mut f, device, expected, family, lo, hi, masks, shader, result,
                );
                cell.set(Some(f));
            });
        },
    )
}
unsafe extern "thiscall" fn draw(
    device: usize,
    a: usize,
    b: usize,
    c: usize,
    d: usize,
    e: usize,
    f: usize,
) -> usize {
    let original: DrawFn = std::mem::transmute(DRAW_TARGET);
    call_once(
        READY.load(Ordering::Acquire),
        || READY.load(Ordering::Acquire) && material_observer::capturing(),
        || unsafe { original(device, a, b, c, d, e, f) },
        |_| {
            FRAME.with(|cell| {
                let Some(mut frame) = cell.get() else {
                    return;
                };
                mark_draw(&mut frame, device);
                cell.set(Some(frame));
            });
        },
    )
}
pub(super) unsafe fn init() {
    if READY.load(Ordering::Acquire)
        || !build_enabled()
        || !material_observer::enabled()
        || std::env::var("SCBL_THERMAL_FINAL_PS_PROBE").as_deref() != Ok("1")
    {
        return;
    }
    let Ok(mut patches) = PATCHES.lock() else {
        return;
    };
    if patches.iter().any(Option::is_some) {
        return;
    }
    // All contracts are from the same pinned retail EXE. Verify before any patch.
    if !signature(
        OUTER_AT,
        0x13a0,
        "a0318b12a11f28700466ab95ae258d2fd1db0122e46fc7966feba498f9928748",
    ) || !signature(
        PS_TARGET,
        0x51,
        "e9ad88e58fe6178c185719588fbf9ea752a1ba9cfbcdea95e07cc86eefcda349",
    ) || !signature(
        DRAW_TARGET,
        0x64,
        "24a4eaba97d033ec72158e5c57ef6774b96e8113bb1b2204a5052e5e7deff21e",
    ) {
        return;
    }
    let hook = match OUTER.get() {
        Some(h) => h,
        None => {
            let Ok(h) = GenericDetour::new(
                std::mem::transmute::<usize, OuterFn>(OUTER_AT),
                outer as OuterFn,
            ) else {
                return;
            };
            if OUTER.set(h).is_err() {
                return;
            }
            OUTER.get().expect("published")
        }
    };
    if hook.enable().is_err() {
        return;
    }
    if !install_call(
        PS_SITE,
        PS_TARGET,
        ps as *const () as usize,
        &mut patches[0],
    ) || !install_call(
        DRAW_SITE,
        DRAW_TARGET,
        draw as *const () as usize,
        &mut patches[1],
    ) {
        let draw_ok = restore(&mut patches[1]);
        let ps_ok = restore(&mut patches[0]);
        if draw_ok && ps_ok {
            let _ = hook.disable();
        }
        tracing::error!("SCBL_THERMAL_FINAL_PS enabled=false reason=install_failed restore_ps={} restore_draw={}", ps_ok, draw_ok);
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_THERMAL_FINAL_PS enabled=true rounds=3 trigger=F8 window_ms=30000 max_changes_per_round=32 exact_call_sites=true writes=0");
}
pub(super) unsafe fn deinit() {
    // An explicit shutdown may precede the observer's own finish callback.
    if READY.load(Ordering::Acquire) {
        if let Ok(logs) = LOGS.try_lock() {
            let round = logs.round;
            drop(logs);
            finish_round(round);
        }
    }
    READY.store(false, Ordering::Release);
    if let Ok(mut patches) = PATCHES.lock() {
        let draw_ok = restore(&mut patches[1]);
        let ps_ok = restore(&mut patches[0]);
        if draw_ok && ps_ok {
            if let Some(hook) = OUTER.get() {
                let _ = hook.disable();
            }
        } else {
            tracing::error!(
                "SCBL_THERMAL_FINAL_PS restore=false ps={} draw={} outer_retained=true",
                ps_ok,
                draw_ok
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn token(render: usize, selected: usize) -> VerifiedToken {
        VerifiedToken {
            round: 1,
            elapsed_ms: 10,
            render,
            root: 0x30000,
            object: 0x40000,
            surface: 0x50000,
            selected,
            actor: 0x60000,
            serial: 7,
            world: 0x70000,
            level: 0x80000,
            category: 251,
            strength: 255,
            stage: 4,
            key: 0xabc,
        }
    }
    #[test]
    fn disabled_bridge_calls_original_once_without_after_read() {
        let (mut calls, mut reads) = (0, 0);
        let eax = call_once(
            false,
            || panic!("disabled must not even check active"),
            || {
                calls += 1;
                0x1234
            },
            |_| {
                reads += 1;
            },
        );
        assert_eq!((eax, calls, reads), (0x1234, 1, 0));
        let eax = call_once(
            true,
            || true,
            || {
                calls += 1;
                0x5678
            },
            |_| {
                reads += 1;
            },
        );
        assert_eq!((eax, calls, reads), (0x5678, 2, 1));
    }
    #[test]
    fn deinit_during_native_call_preserves_eax_without_after_read() {
        let active = std::cell::Cell::new(true);
        let (mut calls, mut reads) = (0, 0);
        let eax = call_once(
            true,
            || active.get(),
            || {
                calls += 1;
                active.set(false);
                0x8765
            },
            |_| {
                reads += 1;
            },
        );
        assert_eq!((eax, calls, reads), (0x8765, 1, 0));
    }
    #[test]
    fn failed_protection_or_flush_keeps_restore_journal_even_when_bytes_old() {
        let j = Patch {
            site: 0x10000,
            old: [0xe8, 1, 2, 3, 4],
            new: [0xe8, 5, 6, 7, 8],
            protection: 0x20,
        };
        let mut pending = Some(j);
        assert!(!complete_restore(&mut pending, true, false, true));
        assert!(pending.is_some());
        assert!(!complete_restore(&mut pending, true, true, false));
        assert!(pending.is_some());
        assert!(complete_restore(&mut pending, true, true, true));
        assert!(pending.is_none());
    }
    #[test]
    fn noncapturing_nested_outer_shadows_parent_token_and_restores_it() {
        let mut parent = Frame::new(0x10000, 0x20000);
        parent.token = Some(token(0x10000, 0x90000));
        let prior = FRAME.with(|v| v.replace(Some(parent)));
        {
            let _shadow = FrameGuard::enter(None);
            assert!(FRAME.with(Cell::get).is_none());
        }
        assert_eq!(FRAME.with(Cell::get).unwrap().token, parent.token);
        FRAME.with(|v| v.set(prior));
    }
    #[test]
    fn record_read_budget_resets_each_second_and_round_before_rpm() {
        let mut b = ReadBudget {
            round: 0,
            epoch: u64::MAX,
            used: 0,
        };
        for _ in 0..8 {
            assert!(admit_record_read(&mut b, 1, 100));
        }
        assert!(!admit_record_read(&mut b, 1, 100));
        assert!(admit_record_read(&mut b, 1, 1100));
        assert!(admit_record_read(&mut b, 2, 100));
    }
    #[test]
    fn exact_record_binding_rejects_wrong_material() {
        let t = token(0x10000, 0x90000);
        let mut f = Frame::new(0x10000, 0x20000);
        assert!(!bind(
            &mut f,
            t,
            Some(t.object),
            Some(t.surface),
            Some(0xdead)
        ));
        assert!(!bind(
            &mut f,
            t,
            Some(0xbeef),
            Some(t.surface),
            Some(t.root)
        ));
        assert!(f.token.is_none());
        assert!(bind(
            &mut f,
            t,
            Some(t.object),
            Some(t.surface),
            Some(t.root)
        ));
        assert!(!bind(
            &mut f,
            t,
            Some(t.object),
            Some(t.surface),
            Some(t.root)
        ));
    }
    #[test]
    fn nested_outer_frame_restores_without_cross_draw_token() {
        let outer = Frame::new(0x10000, 0x20000);
        let previous = FRAME.with(|v| v.replace(Some(outer)));
        assert!(previous.is_none());
        let nested = FRAME.with(|v| v.replace(Some(Frame::new(0x11000, 0x22000))));
        let mut inner = FRAME.with(Cell::get).unwrap();
        let t = token(0x11000, 0x90000);
        assert!(bind(
            &mut inner,
            t,
            Some(t.object),
            Some(t.surface),
            Some(t.root)
        ));
        FRAME.with(|v| v.set(Some(inner)));
        FRAME.with(|v| {
            let got = v.replace(nested).unwrap();
            assert_eq!(got.token, Some(t));
        });
        assert!(FRAME.with(Cell::get).unwrap().token.is_none());
        FRAME.with(|v| v.set(previous));
    }
    #[test]
    fn null_selected_and_early_skip_are_not_draw() {
        let mut f = Frame::new(0x10000, 0x20000);
        f.token = Some(token(0x10000, 0));
        assert_eq!(outcome(f), Some(0));
        f.token = Some(token(0x10000, 0x90000));
        assert_eq!(outcome(f), Some(1));
        mark_ps(
            &mut f,
            0x30000,
            Some(0x30000),
            0x1b9,
            0x1234,
            0xffff,
            Some((0x0f0f, 0x00ff)),
            Some(0x4abc),
            0x4abc,
        );
        assert_eq!(outcome(f), Some(2));
        assert_eq!(
            (f.masked_lo, f.masked_hi, f.shader),
            (Some(0x0204), Some(0x00ff), Some(0x4abc))
        );
        mark_draw(&mut f, 0xdead);
        assert_eq!(outcome(f), Some(2));
        mark_draw(&mut f, 0x30000);
        assert_eq!(outcome(f), Some(3));
    }
    #[test]
    fn missing_masks_or_wrong_device_do_not_forge_draw() {
        let mut f = Frame::new(0x10000, 0x20000);
        f.token = Some(token(0x10000, 0x90000));
        mark_ps(
            &mut f,
            0x30000,
            Some(0x30000),
            0x1ba,
            4,
            8,
            None,
            Some(0x4abc),
            0,
        );
        mark_draw(&mut f, 0x30000);
        assert_eq!(outcome(f), Some(3)); // mask read failure does not erase real draw
        assert_eq!((f.masked_lo, f.masked_hi), (None, None));
        let mut wrong = Frame::new(0x10000, 0x20000);
        wrong.token = Some(token(0x10000, 0x90000));
        mark_ps(
            &mut wrong,
            0x30000,
            Some(0xbeef),
            0x1ba,
            4,
            8,
            Some((0xff, 0xff)),
            Some(0x4abc),
            0x4abc,
        );
        mark_draw(&mut wrong, 0x30000);
        assert_eq!(outcome(wrong), Some(2));
        assert_eq!(wrong.shader, None);
    }
    #[test]
    fn nested_completion_never_rewinds_epoch_and_final_29s_state_flushes() {
        let mut logs = Logs {
            round: 0,
            count: 0,
            epoch: None,
            last: None,
            latest: None,
        };
        let mut f = Frame::new(0x10000, 0x20000);
        for (ms, should_emit) in [(1000, true), (999, false), (2000, false), (1999, false)] {
            let mut t = token(0x10000, 0x90000);
            t.elapsed_ms = ms;
            f.token = Some(t);
            assert_eq!(select_log(&mut logs, f).is_some(), should_emit);
        }
        assert_eq!(logs.epoch, Some(2));
        assert_eq!(logs.latest.unwrap().1.token.unwrap().elapsed_ms, 2000);
        let mut t = token(0x10000, 0x90000);
        t.elapsed_ms = 29000;
        f.token = Some(t);
        assert!(select_log(&mut logs, f).is_none()); // unchanged signature is deduplicated
        assert_eq!(logs.epoch, Some(29));
        assert_eq!(logs.latest.unwrap().1.token.unwrap().elapsed_ms, 29000);
        t.elapsed_ms = 29950;
        t.key = 0x999;
        f.token = Some(t);
        assert!(select_log(&mut logs, f).is_none()); // same epoch but latest retained
        let final_state = take_final(&mut logs, 1).unwrap();
        assert_eq!(final_state.1.token.unwrap().elapsed_ms, 29950);
        assert_eq!(final_state.0.key, 0x999);
        assert!(take_final(&mut logs, 1).is_none());
        assert!(logs.count <= 32);
    }
    #[test]
    fn callsite_displacements_target_exact_native_or_bridge_address() {
        for (site, target) in [(PS_SITE, PS_TARGET), (DRAW_SITE, DRAW_TARGET)] {
            let b = rel_call(site, target);
            assert_eq!(b[0], 0xe8);
            let d = i32::from_le_bytes(b[1..].try_into().unwrap()) as u32;
            assert_eq!((site as u32).wrapping_add(5).wrapping_add(d), target as u32);
        }
    }
}

#[cfg(test)]
mod material_surface_transition_tests {
    use super::*;
fn frame(ms: u64, surface: usize) -> Frame {
    let mut f=Frame::new(0x1000,0x2000);
    f.token=Some(VerifiedToken { round:2, elapsed_ms:ms, render:0x1000, root:0x3000,
        object:0x4000, surface, selected:0x9d0d12e0, actor:0xa786a000, serial:591547,
        world:1, level:2, category:251, strength:255, stage:4, key:0xa7bb48a1 });
    f.ps=true; f.draw=true; f.device_matches=true; f.family=0x1ba;
    f.masked_lo=Some(0); f.masked_hi=Some(0); f.shader=Some(0x6e2287b0);
    f.lookup=0x6e2287b0; f
}
fn logs() -> Logs { Logs { round:0,count:0,epoch:None,last:None,latest:None } }
#[test] fn surface_transition_same_shader_emits() {
    let mut l=logs(); assert!(select_log(&mut l,frame(4812,0xa540c660)).is_some());
    let emitted=select_log(&mut l,frame(21734,0xa87bf4b0)).is_some();
    println!("OBS surface_transition_same_shader_emits emitted={emitted}"); assert!(emitted);
}
#[test] fn surface_transition_in_last_second_flushes() {
    let mut l=logs(); assert!(select_log(&mut l,frame(29000,0xa540c660)).is_some());
    assert!(select_log(&mut l,frame(29734,0xa87bf4b0)).is_none());
    let emitted=take_final(&mut l,2).is_some();
    println!("OBS surface_transition_in_last_second_flushes emitted={emitted}"); assert!(emitted);
}
#[test] fn unchanged_draws_remain_deduplicated() {
    let mut l=logs(); assert!(select_log(&mut l,frame(4812,0xa540c660)).is_some());
    for ms in [5000,6000,21734,29000] { assert!(select_log(&mut l,frame(ms,0xa540c660)).is_none()); }
    assert!(take_final(&mut l,2).is_none()); assert_eq!(l.count,1);
}
#[test] fn stale_completion_cannot_overwrite_newer_surface() {
    let mut l=logs(); select_log(&mut l,frame(21734,0xa87bf4b0));
    assert!(select_log(&mut l,frame(4812,0xa540c660)).is_none());
    assert_eq!(l.latest.unwrap().1.token.unwrap().surface,0xa87bf4b0);
}


}
