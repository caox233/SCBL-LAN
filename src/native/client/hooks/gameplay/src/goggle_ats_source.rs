//! Read native ATS accepted output, not a second range or sound simulation.
//! Popup visibility is read after Update. Footstep waves are aggregated by the
//! game, so observe their actual per-source positive writes before identity is
//! lost in that aggregate. No native output or source event is changed.
use crate::ats_marks::Role;
use crate::ats_native_source::{self, Frame};
use crate::equipment_native::{float, readable, signature, word};
use std::{
    cell::RefCell,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

const HUD: u32 = 0x2c3e85c;
const SITES: [usize; 2] = [0x1a706dc, 0x1a70838];
const ORIGINAL: [u8; 7] = [0xc7, 0x41, 0x14, 1, 0, 0, 0];
static READY: AtomicBool = AtomicBool::new(false);
static THUNK: AtomicUsize = AtomicUsize::new(0);
static CURRENT: Mutex<Option<Snapshot>> = Mutex::new(None);
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub world: usize,
    pub local: usize,
    pub native_now_ms: u64,
    /// Actual observations in this native Update, not a persistent mark lease.
    /// Retail processes alternating wave entries; a consumer must not interpret
    /// an absent wave on the other parity frame as a native rejection event.
    pub targets: Vec<u64>,
}
struct Pending {
    hud: usize,
    frame: Frame,
    people: Vec<(u32, u64, Role)>,
    waves: Vec<(u32, u64)>,
}
thread_local! { static PENDING:RefCell<Option<Pending>>=const {RefCell::new(None)}; }
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
}
unsafe fn byte(p: usize, o: usize) -> Option<u8> {
    let p = p.checked_add(o)?;
    readable(p, 1).then(|| (p as *const u8).read())
}
unsafe fn entries(hud: usize) -> Option<(usize, usize)> {
    if word(hud, 0)? != HUD {
        return None;
    }
    let (p, n, cap) = (
        word(hud, 0x2c8)? as usize,
        word(hud, 0x2cc)? as usize,
        word(hud, 0x2d0)? as usize,
    );
    (n <= 128 && n <= cap && cap <= 4096 && (n == 0 || readable(p, n * 0x2c))).then_some((p, n))
}
unsafe fn owner(hud: usize, f: &Frame) -> bool {
    word(hud, 0) == Some(HUD)
        && word(hud, 0x25c).and_then(|p| word(p as usize, 0x310)) == Some(f.local as u32)
}
unsafe fn identity(e: usize, f: &Frame, people: &[(u32, u64, Role)]) -> Option<(u32, u64)> {
    let pawn = word(e, 0x14)? as usize;
    if word(e, 0x10)? as usize != pawn
        || word(pawn, 0)? != 0x2ad3234
        || word(pawn, 0x144)? as usize != f.world
    {
        return None;
    }
    let object = word(pawn, 0x20).filter(|v| *v != 0)?;
    let mut found = people
        .iter()
        .filter(|&&(o, k, r)| o == object && k != 0 && r == Role::Spy);
    let &(_, key, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some((object, key))
}
unsafe fn record_wave(ebp: usize, wave: usize, pending: &mut Pending) {
    if !readable(ebp.wrapping_sub(0x200), 0x200)
        || word(ebp.wrapping_sub(0x1bc), 0) != Some(pending.hud as u32)
        || word(wave, 0x14) != Some(1)
        || !float(wave, 4).is_some_and(|v| v.is_finite() && v > 0.)
    {
        return;
    }
    let Some((p, n)) = entries(pending.hud) else {
        return;
    };
    let Some(i) = word(ebp.wrapping_sub(0x58), 0)
        .map(|i| i as usize)
        .filter(|i| *i < n)
    else {
        return;
    };
    let Some(id) = identity(p + i * 0x2c, &pending.frame, &pending.people) else {
        return;
    };
    if !pending.waves.contains(&id) {
        pending.waves.push(id);
    }
}
unsafe extern "system" fn on_wave(ebp: usize, wave: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    PENDING.with(|p| {
        if let Ok(mut p) = p.try_borrow_mut() {
            if let Some(p) = p.as_mut() {
                record_wave(ebp, wave, p);
            }
        }
    });
}
unsafe fn prepare(hud: usize) -> Option<Pending> {
    let frame = ats_native_source::frame()?;
    if !owner(hud, &frame) {
        return None;
    }
    entries(hud)?;
    let people = ats_native_source::detector_players(&frame)?;
    Some(Pending {
        hud,
        frame,
        people,
        waves: Vec::new(),
    })
}
unsafe fn capture(
    hud: usize,
    f: &Frame,
    people: &[(u32, u64, Role)],
    waves: &[(u32, u64)],
) -> Option<Snapshot> {
    if people.len() > 32 || waves.len() > 128 || !owner(hud, f) {
        return None;
    }
    let mut out = Snapshot {
        world: f.world,
        local: f.local,
        native_now_ms: f.now,
        targets: Vec::new(),
    };
    match byte(hud, 0x2f8)? {
        0 => return Some(out),
        1 => (),
        _ => return None,
    }
    let (p, n) = entries(hud)?;
    for i in 0..n {
        let e = p + i * 0x2c;
        let Some(id) = identity(e, f, people) else {
            continue;
        };
        let popup = word(e, 0)
            .and_then(|w| word(w as usize, 0x1a8))
            .is_some_and(|d| byte(d as usize, 0x40) == Some(1));
        if (popup || waves.contains(&id)) && !out.targets.contains(&id.1) {
            out.targets.push(id.1);
        }
    }
    Some(out)
}
fn publish(next: Option<Snapshot>) {
    if let Ok(mut s) = CURRENT.lock() {
        *s = next;
    }
}
pub(crate) fn clear() {
    publish(None);
}
/// The existing ATS owner supplies its original Tick exactly once. Scope the
/// internal wave observer to this call; nested calls restore the outer scope.
pub(crate) unsafe fn update(hud: usize, original: impl FnOnce()) {
    if !READY.load(Ordering::Acquire) {
        clear();
        original();
        return;
    }
    let before = prepare(hud);
    let previous = PENDING.with(|p| p.replace(before));
    original();
    let observed = PENDING.with(|p| p.replace(previous));
    let next = (|| {
        let observed = observed?;
        let f = ats_native_source::frame()?;
        if (
            observed.frame.world,
            observed.frame.info,
            observed.frame.local,
            observed.frame.team,
            observed.frame.now,
        ) != (f.world, f.info, f.local, f.team, f.now)
        {
            return None;
        }
        let people = ats_native_source::detector_players(&f)?;
        capture(hud, &f, &people, &observed.waves)
    })();
    if let Some(s) = &next {
        // No result on the alternate wave-update parity is not a native rejection.
        // A disabled detector is instead a complete empty withdrawal.
        let partial = byte(hud, 0x2f8) == Some(1);
        crate::goggle_feedback_native::observe(crate::goggle_exposure::Source::Ats,
            s.world, s.local, s.native_now_ms, &s.targets, partial);
    }
    publish(next);
}
#[allow(dead_code)]
pub(crate) fn snapshot(f: &Frame) -> Option<Snapshot> {
    CURRENT
        .lock()
        .ok()?
        .as_ref()
        .filter(|s| s.world == f.world && s.local == f.local && s.native_now_ms == f.now)
        .cloned()
}
fn bridge(base: u32, callback: u32, resume: u32) -> Vec<u8> {
    let mut b = ORIGINAL.to_vec(); // native wave updated=1 exactly once
    b.extend([
        0x9c, 0x60, 0x89, 0xe0, 0x81, 0xec, 0x20, 0x02, 0, 0, 0x83, 0xe4, 0xf0, 0x89, 0x84, 0x24,
        0, 0x02, 0, 0, 0x0f, 0xae, 0x04, 0x24, 0xfc, 0x51, 0x55, 0xb8,
    ]);
    b.extend(callback.to_le_bytes()); // stdcall(ebp, original ECX wave)
    b.extend([
        0xff, 0xd0, 0x0f, 0xae, 0x0c, 0x24, 0x8b, 0xa4, 0x24, 0, 0x02, 0, 0, 0x61, 0x9d, 0xe9,
    ]);
    b.extend(resume.wrapping_sub(base + b.len() as u32 + 4).to_le_bytes());
    b
}
fn jump(site: usize, to: usize) -> [u8; 7] {
    let mut b = [0xe9, 0, 0, 0, 0, 0x90, 0x90];
    b[1..5].copy_from_slice(&(to as u32).wrapping_sub(site as u32 + 5).to_le_bytes());
    b
}
/// Called after ATS signatures are checked but BEFORE its Tick detour installs.
pub(crate) unsafe fn init(world: usize) -> bool {
    if world != 0x589860
        || !signature(
            0x1a6f990,
            4165,
            "a25c45764a9645e8f7ed6dd2cbac0e4853b005318734f904268838fe5e7dd805",
        )
    {
        return false;
    }
    let p = VirtualAlloc(std::ptr::null_mut(), 512, 0x3000, 4) as usize;
    if p == 0 {
        return false;
    }
    for (i, site) in SITES.into_iter().enumerate() {
        let code = bridge(
            (p + i * 128) as u32,
            on_wave as *const () as u32,
            (site + 7) as u32,
        );
        if code.len() > 128 {
            let _ = VirtualFree(p as _, 0, 0x8000); // no branch can reach it yet
            return false;
        }
        std::ptr::copy_nonoverlapping(code.as_ptr(), (p + i * 128) as _, code.len());
    }
    let mut old = 0;
    if VirtualProtect(p as _, 512, 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), p as _, 512) == 0
    {
        let _ = VirtualFree(p as _, 0, 0x8000); // unpublished allocation only
        return false;
    }
    // patch() may fail after a write if protection/cache repair also fails.
    // Retain ownership BEFORE either publication attempt so deinit can retry
    // both sites. Never free storage an in-flight branch may still reference.
    THUNK.store(p, Ordering::Release);
    if !crate::equipment_native::patch(SITES[0], &ORIGINAL, &jump(SITES[0], p))
        || !crate::equipment_native::patch(SITES[1], &ORIGINAL, &jump(SITES[1], p + 128))
    {
        deinit();
        return false;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_GOGGLE_OBSERVER source=native_ATS_result observer_ready=true peer_delivery=false warning_enabled=false");
    true
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    clear();
    let p = THUNK.load(Ordering::Acquire);
    if p != 0 {
        for (i, site) in SITES.into_iter().enumerate() {
            let _ = crate::equipment_native::patch(site, &jump(site, p + i * 128), &ORIGINAL);
        }
    }
    // Published thunks remain mapped until process exit, like the existing host.
}
#[cfg(test)]
#[path = "goggle_ats_source_tests.rs"]
mod tests;
