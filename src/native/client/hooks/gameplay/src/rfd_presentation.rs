//! DX11 RFD presentation: retain native detection, range and notification.
//! Pants-only emitters share one random-size left-to-right native marker; normal sources win.
use crate::equipment_native::{armor, equipped, float, readable, signature, word};
use crate::rfd_rules::{self, Blink, Display, Signal, Sweep};
use retour::static_detour;
use std::{
    cell::RefCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

const SITE: usize = 0x1a743b9;
const ORIGINAL: [u8; 8] = [0xf3, 0x0f, 0x11, 0x85, 0x60, 0xff, 0xff, 0xff];
const BAND_SITE: usize = 0x1a744e4;
const BAND_ORIGINAL: [u8; 8] = [0xf3, 0x0f, 0x10, 0x45, 0xbc, 0x0f, 0x5a, 0xc0];
const BAND_SMALL: usize = 0x1a745eb;
const BAND_LARGE: usize = 0x1a744fa;
const BAND_MIDDLE: usize = 0x1a7457f;
static READY: AtomicBool = AtomicBool::new(false);
static THUNK: AtomicUsize = AtomicUsize::new(0);
static_detour! {static Update:unsafe extern "thiscall" fn(usize,f32);}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
    fn GetTickCount64() -> u64;
}
type Visible = unsafe extern "thiscall" fn(usize, u8);
type Alpha = unsafe extern "thiscall" fn(usize, f32);
type Scale = unsafe extern "thiscall" fn(usize, f32);
type Goggles = unsafe extern "thiscall" fn(usize) -> u32;
#[derive(Clone, Copy, PartialEq, Eq)]
struct Entry {
    actor: usize,
    object: u32,
    pawn: usize,
    pawn_object: u32,
    icon: usize,
    marker: usize,
    icon_data: usize,
    marker_data: usize,
}
#[derive(Clone, Copy)]
struct Saved {
    entry: Entry,
    icon_visible: u8,
    marker_visible: u8,
    alpha: f32,
    modified_alpha: f32,
}
struct Frame {
    hud: usize,
    signal: Signal,
    pants: Vec<Entry>,
    signals: Vec<Entry>,
}
struct History {
    hud: usize,
    object: u32,
    sweep: Sweep,
    blink: Blink,
    count: usize,
    saved: Vec<Saved>,
}
thread_local! {
    static UPDATE_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INTERRUPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NATIVE_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FRAME:RefCell<Option<Frame>>=const {RefCell::new(None)};
    static HISTORY:RefCell<History>=const {RefCell::new(History{hud:0,object:0,sweep:Sweep::new(0x7654321),blink:Blink::new(0x6a09e667),count:0,saved:Vec::new()})};
}
// One custom presentation transaction per thread. Nested native updates still
// execute exactly once, but cannot append to or commit the interrupted frame.
struct UpdateScope;
impl UpdateScope {
    fn enter() -> Option<Self> {
        if UPDATE_ACTIVE.with(|a| a.replace(true)) {
            INTERRUPTED.with(|i| i.set(true));
            None
        } else {
            INTERRUPTED.with(|i| i.set(false));
            Some(Self)
        }
    }
}
impl Drop for UpdateScope {
    fn drop(&mut self) { UPDATE_ACTIVE.with(|a| a.set(false)); }
}
struct NativeOnly(bool);
impl Drop for NativeOnly {
    fn drop(&mut self) { NATIVE_ONLY.with(|n| n.set(self.0)); }
}
unsafe fn native_update_only(hud: usize, dt: f32) {
    let _scope = NativeOnly(NATIVE_ONLY.with(|n| n.replace(true)));
    Update.call(hud, dt);
}
fn custom_allowed() -> bool {
    READY.load(Ordering::Acquire)
        && !NATIVE_ONLY.with(std::cell::Cell::get)
        && !INTERRUPTED.with(std::cell::Cell::get)
}
unsafe fn current_owner(hud: usize, object: u32) -> bool {
    custom_allowed() && word(hud, 0x20) == Some(object) && entries(hud).is_some()
}
unsafe fn current_entry(hud: usize, object: u32, expected: Entry) -> bool {
    current_owner(hud, object) && entries(hud).is_some_and(|(p, n)| {
        (0..n).any(|i| entry(p + i * 0x28) == Some(expected))
    })
}
fn discard_history() {
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        h.hud = 0;
        h.object = 0;
        h.saved.clear();
        h.count = 0;
        h.sweep.reset();
        h.blink.reset();
    });
}
unsafe fn entries(hud: usize) -> Option<(usize, usize)> {
    if word(hud, 0)? != 0x02c3eb6c {
        return None;
    }
    let (p, n, cap) = (
        word(hud, 0x2ac)? as usize,
        word(hud, 0x2b0)? as usize,
        word(hud, 0x2b4)? as usize,
    );
    (n <= 128 && cap >= n && cap <= 4096 && (n == 0 || readable(p, n * 0x28))).then_some((p, n))
}
unsafe fn entry(p: usize) -> Option<Entry> {
    let actor = word(p, 0x1c)? as usize;
    let pawn = word(p, 0x20)? as usize;
    let icon = word(p, 0)? as usize;
    let marker = word(p, 4)? as usize;
    let icon_data = word(icon, 0x1a8)? as usize;
    let marker_data = word(marker, 0x1a8)? as usize;
    if !readable(icon_data, 0x50) || !readable(marker_data, 0x50) {
        return None;
    }
    Some(Entry {
        actor,
        object: word(actor, 0x20)?,
        pawn,
        pawn_object: word(pawn, 0x20).unwrap_or(0),
        icon,
        marker,
        icon_data,
        marker_data,
    })
}
unsafe fn visible(object: usize, value: u8) {
    std::mem::transmute::<usize, Visible>(0x18a1c80)(object, value);
}
unsafe fn alpha(object: usize, value: f32) {
    std::mem::transmute::<usize, Alpha>(0x18a2290)(object, value);
}
unsafe fn apply_pants_display(frame: usize, marker: usize, signal: Signal, setter: Scale) {
    (frame.wrapping_sub(0xa0) as *mut f32).write_unaligned(signal.direction);
    setter(marker, signal.scale);
}
unsafe fn native_visible(data: usize) -> u8 {
    word(data, 0x40).map_or(0, |x| (x & 255) as u8)
}
pub(crate) unsafe fn classify(pawn: usize) -> Display {
    if word(pawn, 0) != Some(0x02ad3234) {
        return Display::Native;
    }
    let Some(a) = armor(pawn) else {
        return Display::Native;
    };
    if !equipped(a, b"MPArmorPantsGhost") {
        return Display::Native;
    }
    let Some(ability) = word(a, 0x5c).map(|x| x as usize) else {
        return Display::Native;
    };
    if word(ability, 0x6c) != Some(pawn as u32) {
        return Display::Native;
    }
    let kind = match word(ability, 0) {
        Some(0x2a3eef4) => 1,
        Some(0x2a3f2dc) => 2,
        Some(0x2a3f3a4) => 3,
        _ => 0,
    };
    let Some(flags) = word(ability, 0x40) else {
        return Display::Native;
    };
    let Some(armor_flags) = word(a, 0x7c) else {
        return Display::Native;
    };
    let Some(rf_flags) = word(pawn, 0x944) else {
        return Display::Native;
    };
    let goggles = std::mem::transmute::<usize, Goggles>(0xd75d30)(pawn) != 0;
    // The retail helmet gate is armor+7C bit 3 (1A73BB9). Do not invent an
    // equipment-name alias, nor mistake replicated aggregate RF for goggles.
    let ordinary = rfd_rules::ordinary_goggles(goggles, rf_flags, armor_flags & 8 != 0);
    rfd_rules::display(
        true,
        if kind == 2 && crate::intel_rfd_targets::ready() {
            crate::intel_rfd_targets::active(pawn) == Some(true)
        } else {
            rfd_rules::protected(kind, flags, crate::rfd_exposure::held(pawn))
        },
        ordinary,
    )
}
// C55 limits only pants-generated interference. Real goggles exposure keeps
// the native range and takes priority in classify(). Coordinates are read now,
// never from the randomized marker or retained HUD entries.
unsafe fn interference_in_range(hud: usize, pawn: usize) -> bool {
    let Some(local) = word(hud, 0x25c)
        .and_then(|p| word(p as usize, 0x310))
        .map(|p| p as usize)
    else {
        return false;
    };
    if word(local, 0) != Some(0x02ad2abc)
        || word(local, 0x144).filter(|p| *p != 0) != word(pawn, 0x144)
    {
        return false;
    }
    let mut squared = 0.0f64;
    for o in [0x9c, 0xa0, 0xa4] {
        let (Some(a), Some(b)) = (float(local, o), float(pawn, o)) else {
            return false;
        };
        if !a.is_finite() || !b.is_finite() {
            return false;
        }
        squared += (a as f64 - b as f64).powi(2);
    }
    squared <= 1000.0f64.powi(2)
}
pub(crate) unsafe fn visibility(frame: usize) {
    if !custom_allowed() {
        return;
    }
    let hud = word(frame.wrapping_sub(0x1b0), 0).unwrap_or(0) as usize;
    let index = word(frame.wrapping_sub(0x24), 0).unwrap_or(u32::MAX) as usize;
    let Some((p, n)) = entries(hud) else { return };
    if index >= n {
        return;
    }
    let p = p + index * 0x28;
    let Some(pawn) = word(p, 0x20).map(|x| x as usize) else {
        return;
    };
    let native_visible = (frame.wrapping_sub(0x29) as *const u8).read() != 0;
    if native_visible && !crate::intel_rfd_targets::filter_visibility(pawn, native_visible) {
        (frame.wrapping_sub(0x29) as *mut u8).write(0);
        return;
    }
    // Native anti-RFD helmets permit clothing exposure but reject goggles-only
    // exposure. Retained Intel is still clothing exposure after native active
    // becomes false. Never override entry+18, death, or the global all-RF veto.
    if crate::rfd_exposure::held(pawn) && word(p, 0x18).is_some_and(|v| v & 255 == 0) {
        let all_veto = word(0x339e5a4, 0).and_then(|g| word(g as usize, 0x80));
        if all_veto == Some(0) {
            (frame.wrapping_sub(0x29) as *mut u8).write(1);
        }
    }
    if (frame.wrapping_sub(0x29) as *const u8).read() == 0
        || classify(pawn) != Display::Interference
    {
        return;
    }
    // This boundary precedes BOTH native icon and wave drawing. The original
    // false branch hides both widgets and cannot publish an interference receipt.
    if !interference_in_range(hud, pawn) {
        (frame.wrapping_sub(0x29) as *mut u8).write(0);
        return;
    }
    let Some(e) = entry(p) else { return };
    FRAME.with(|cell| {
        if let Ok(mut ctx) = cell.try_borrow_mut() {
            if let Some(f) = ctx.as_mut().filter(|f| f.hud == hud) {
                f.pants.push(e);
            }
        }
    });
}
unsafe extern "system" fn direction(frame: usize) {
    if !custom_allowed() || !readable(frame.wrapping_sub(0x208), 0x208) {
        return;
    }
    let hud = word(frame.wrapping_sub(0x1b0), 0).unwrap_or(0) as usize;
    let index = word(frame.wrapping_sub(0x24), 0).unwrap_or(u32::MAX) as usize;
    let Some((p, n)) = entries(hud) else { return };
    if index >= n {
        return;
    }
    let Some(e) = entry(p + index * 0x28) else {
        return;
    };
    let Some(_) = float(frame.wrapping_sub(0xa4), 0).filter(|x| x.is_finite() && *x > 0.0) else {
        return;
    };
    let signal = FRAME.with(|cell| {
        cell.try_borrow().ok().and_then(|ctx| {
            ctx.as_ref().filter(|f| f.hud == hud && f.pants.contains(&e)).map(|f| f.signal)
        })
    });
    let Some(signal) = signal else { return };
    let Some(object) = word(hud, 0x20) else { return };
    apply_pants_display(frame, e.marker, signal, std::mem::transmute::<usize, Scale>(0x18a1ce0));
    if !current_entry(hud, object, e) { return; }
    FRAME.with(|cell| {
        if let Ok(mut ctx) = cell.try_borrow_mut() {
            if let Some(f) = ctx.as_mut().filter(|f| f.hud == hud && f.pants.contains(&e)) {
                if !f.signals.contains(&e) { f.signals.push(e); }
            }
        }
    });
}
unsafe extern "system" fn band_override(frame: usize) -> u32 {
    if !custom_allowed() || !readable(frame.wrapping_sub(0x208), 0x208) {
        return 0;
    }
    let hud = word(frame.wrapping_sub(0x1b0), 0).unwrap_or(0) as usize;
    let index = word(frame.wrapping_sub(0x24), 0).unwrap_or(u32::MAX) as usize;
    let Some((p, n)) = entries(hud) else { return 0 };
    if index >= n {
        return 0;
    }
    let Some(e) = entry(p + index * 0x28) else {
        return 0;
    };
    FRAME.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|ctx| {
                ctx.as_ref()
                    .filter(|f| f.hud == hud && f.signals.contains(&e))
                    .map(|f| f.signal.band)
            })
            .unwrap_or(0)
    })
}
fn effective_signals(signals: &[Entry], current: &[Entry]) -> Vec<Entry> {
    // Native callbacks are the eligibility gate. No nearby-player enumeration.
    signals
        .iter()
        .copied()
        .filter(|e| current.contains(e))
        .collect()
}
fn update(hud: usize, dt: f32) {
    let Some(_scope) = UpdateScope::enter() else {
        unsafe { native_update_only(hud, dt) };
        return;
    };
    if !READY.load(Ordering::Acquire) {
        #[cfg(feature = "goggle-feedback")]
        crate::goggle_rfd_source::clear();
        unsafe { native_update_only(hud, dt) };
        return;
    }
    let current = unsafe {
        entries(hud).map(|(p, n)| {
            (0..n)
                .filter_map(|i| entry(p + i * 0x28))
                .collect::<Vec<_>>()
        })
    };
    let Some(current) = current else {
        #[cfg(feature = "goggle-feedback")]
        crate::goggle_rfd_source::clear();
        unsafe { native_update_only(hud, dt) };
        return;
    };
    let object = unsafe { word(hud, 0x20) }.unwrap_or(0);
    let (signal, saved) = HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        if h.hud != hud || h.object != object {
            h.hud = hud;
            h.object = object;
            h.saved.clear();
            h.sweep = Sweep::new(0x7654321);
            h.blink = Blink::new(unsafe { GetTickCount64() } as u32 ^ object);
            h.count = 0;
        }
        // Final native eligibility is known after Update, so cadence uses the
        // preceding frame's count (at most one HUD update of latency). A new
        // detection starts in the one-source tier, never runs native Update twice.
        let count = h.count.max(1);
        (h.sweep.sample(unsafe { GetTickCount64() }, count), std::mem::take(&mut h.saved))
    });
    // Native callbacks can reenter or invalidate the HUD. No RefCell borrow
    // spans a callback; every following write revalidates the current entry.
    for s in saved {
        if current.contains(&s.entry) && unsafe { current_entry(hud, object, s.entry) } {
            unsafe {
                if native_visible(s.entry.icon_data) == 0 {
                    visible(s.entry.icon, s.icon_visible);
                    if !current_entry(hud, object, s.entry) { break; }
                }
                if native_visible(s.entry.marker_data) == 0 {
                    visible(s.entry.marker, s.marker_visible);
                    if !current_entry(hud, object, s.entry) { break; }
                }
                if float(s.entry.marker_data, 0x38) == Some(s.modified_alpha) {
                    alpha(s.entry.marker, s.alpha);
                    if !current_entry(hud, object, s.entry) { break; }
                }
            }
        }
    }
    if !unsafe { current_owner(hud, object) } {
        discard_history();
        unsafe { native_update_only(hud, dt) };
        return;
    }
    let previous = FRAME.with(|f| {
        f.replace(Some(Frame {
            hud,
            signal,
            pants: Vec::new(),
            signals: Vec::new(),
        }))
    });
    unsafe { Update.call(hud, dt) };
    // Capture the two native accepted-result widgets before pants presentation
    // hides precise icons or merges waves. Never publish randomized display.
    #[cfg(feature = "goggle-feedback")]
    unsafe {
        crate::goggle_rfd_source::after_native_update(hud)
    };
    let frame = FRAME.with(|f| f.replace(previous));
    let Some(frame) = frame else { return };
    if !unsafe { current_owner(hud, object) } {
        discard_history();
        return;
    }
    let Some(current) = (unsafe {
        entries(hud).map(|(p, n)| {
            (0..n)
                .filter_map(|i| entry(p + i * 0x28))
                .collect::<Vec<_>>()
        })
    }) else {
        HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            h.count = 0;
            h.saved.clear();
            h.sweep.reset();
            h.blink.reset();
        });
        return;
    };
    let eligible = effective_signals(&frame.signals, &current);
    let count = rfd_rules::unique_count(eligible.iter().map(|e| (e.pawn, e.pawn_object)));
    // Capture/notification already ran above. Hidden phases never remove a
    // genuine detector receipt and never restart the independent sweep.
    let blink_on = HISTORY.with(|h| {
        h.borrow_mut()
            .blink
            .sample(unsafe { GetTickCount64() }, count != 0)
    });
    // Stable identity, not closest/strongest target. All representatives now have
    // the same size, band and opacity; distance cannot select their appearance.
    let selected = eligible
        .iter()
        .copied()
        .min_by_key(|e| (e.pawn, e.pawn_object, e.marker));
    let mut saved = Vec::new();
    for e in frame.pants.iter().copied() {
        unsafe {
            if !current.contains(&e) || !current_entry(hud, object, e) {
                continue;
            }
            let Some(a) =
                float(e.marker_data, 0x38).filter(|x| x.is_finite() && (0.0..=100.0).contains(x))
            else {
                continue;
            };
            let mut s = Saved {
                entry: e,
                icon_visible: native_visible(e.icon_data),
                marker_visible: native_visible(e.marker_data),
                alpha: a,
                modified_alpha: a,
            };
            visible(e.icon, 0);
            if !current_entry(hud, object, e) { discard_history(); return; }
            if Some(e) == selected && s.marker_visible != 0 {
                // Fixed 70 percent opacity. Drop behind a coincident ordinary signal
                // rather than obscuring a correctly detected target.
                let overlaps = current.iter().any(|other| {
                    *other != e
                        && !frame.pants.contains(other)
                        && native_visible(other.marker_data) != 0
                        && float(other.marker_data, 0x10)
                            .zip(float(e.marker_data, 0x10))
                            .is_some_and(|(x, y)| (x - y).abs() < 40.0)
                });
                s.modified_alpha = rfd_rules::INTERFERENCE_ALPHA;
                alpha(e.marker, s.modified_alpha);
                if !current_entry(hud, object, e) { discard_history(); return; }
                if overlaps || !blink_on {
                    visible(e.marker, 0);
                }
            } else {
                visible(e.marker, 0);
            }
            if !current_entry(hud, object, e) { discard_history(); return; }
            saved.push(s);
        }
    }
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        h.saved = saved;
        h.count = count;
        if count == 0 {
            h.sweep.reset();
        }
    });
}
fn bridge(base: u32, callback: u32) -> Vec<u8> {
    // Replay native store first; callback sets display direction and independent random scale.
    let mut b = ORIGINAL.to_vec();
    b.extend([
        0x9c, 0x60, 0x89, 0xe0, 0x81, 0xec, 0x20, 0x02, 0, 0, 0x83, 0xe4, 0xf0, 0x89, 0x84, 0x24,
        0, 0x02, 0, 0, 0x0f, 0xae, 0x04, 0x24, 0xfc, 0x55, 0xb8,
    ]);
    b.extend(callback.to_le_bytes());
    b.extend([
        0xff, 0xd0, 0x0f, 0xae, 0x0c, 0x24, 0x8b, 0xa4, 0x24, 0, 0x02, 0, 0, 0x61, 0x9d,
    ]);
    let end = base.wrapping_add(b.len() as u32 + 5);
    b.push(0xe9);
    b.extend((SITE as u32 + 8).wrapping_sub(end).to_le_bytes());
    b
}
fn band_bridge(base: u32, callback: u32, native_return: u32, bands: [u32; 3]) -> Vec<u8> {
    // Route only the animation selector, never overwrite real distance -44.
    // Callback 0/invalid stays native; 1/2/3 means Small/Medium/Large.
    let mut b = vec![
        0x9c, 0x60, 0x89, 0xe0, 0x81, 0xec, 0x20, 0x02, 0, 0, 0x83, 0xe4, 0xf0, 0x89, 0x84, 0x24,
        0, 0x02, 0, 0, 0x0f, 0xae, 0x04, 0x24, 0xfc, 0x55, 0xb8,
    ];
    b.extend(callback.to_le_bytes());
    b.extend([0xff, 0xd0]);
    let mut branches = Vec::new();
    for id in 1..=3u8 {
        b.extend([0x83, 0xf8, id, 0x0f, 0x84]);
        branches.push(b.len());
        b.extend([0; 4]);
    }
    let restore = [
        0x0f, 0xae, 0x0c, 0x24, 0x8b, 0xa4, 0x24, 0, 0x02, 0, 0, 0x61, 0x9d,
    ];
    b.extend(restore);
    b.extend(BAND_ORIGINAL);
    b.push(0xe9);
    b.extend(
        native_return
            .wrapping_sub(base + b.len() as u32 + 4)
            .to_le_bytes(),
    );
    for (index, target) in bands.into_iter().enumerate() {
        let branch = branches[index];
        let displacement = (b.len() as u32).wrapping_sub(branch as u32 + 4);
        b[branch..branch + 4].copy_from_slice(&displacement.to_le_bytes());
        b.extend(restore);
        b.push(0xe9);
        b.extend(target.wrapping_sub(base + b.len() as u32 + 4).to_le_bytes());
    }
    b
}
fn jump_from(site: usize, to: usize) -> [u8; 8] {
    let mut b = [0xe9, 0, 0, 0, 0, 0x90, 0x90, 0x90];
    b[1..5].copy_from_slice(&(to as u32).wrapping_sub(site as u32 + 5).to_le_bytes());
    b
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860 || !crate::equipment_native::ready() {
        return;
    }
    if !signature(
        BAND_SITE,
        8,
        "87caee51c88f129edd6d5a611925b7261733d815bedc1b9da9ad9a1b66fc9f11",
    ) || !signature(
        0x18a1ce0,
        0x6a,
        "3be09e44ddad421fa125c2688dcc1191bea7b277b98b452f32bc1da4fc1aa4c0",
    ) || !signature(
        SITE,
        8,
        "0aeebbe9f41964380685c2cce5c15b27d69ba4dfd0b74b12701ce1bfef8422ad",
    ) || !signature(
        0x18a1c80,
        0x5c,
        "0af8694bc1f619cc24e1aa03c398e6d78cc3d9923e4a21f078c02803791ea40f",
    ) || !signature(
        0x18a2290,
        0x6c,
        "ddd423ec3b4594925d54736f5c6089c1d294d6c337fc871893eebc92a3d19433",
    ) || !signature(
        0xd75d30,
        0x100,
        "8788cf182e57a274b7040b41fad01dbc7de7116e86652f1dca2a0ca7ed93b4c5",
    ) {
        return;
    }
    let memory = VirtualAlloc(std::ptr::null_mut(), 512, 0x3000, 4);
    if memory.is_null() {
        return;
    }
    let code = bridge(memory as u32, direction as *const () as u32);
    let band = band_bridge(
        memory as u32 + 128,
        band_override as *const () as u32,
        BAND_SITE as u32 + 8,
        [BAND_SMALL as u32, BAND_MIDDLE as u32, BAND_LARGE as u32],
    );
    if code.len() > 128 || band.len() > 384 {
        let _ = VirtualFree(memory, 0, 0x8000); // still unpublished
        return;
    }
    std::ptr::copy_nonoverlapping(code.as_ptr(), memory as _, code.len());
    std::ptr::copy_nonoverlapping(band.as_ptr(), (memory as usize + 128) as _, band.len());
    let mut old = 0;
    if VirtualProtect(memory, 512, 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), memory, 512) == 0
    {
        let _ = VirtualFree(memory, 0, 0x8000); // no native branch references it
        return;
    }
    if Update
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0x1a73860),
            update,
        )
        .is_err()
    {
        let _ = VirtualFree(memory, 0, 0x8000); // Update never references this bank
        return;
    }
    // A failed patch can still have published bytes if its own rollback fails.
    // Keep the address before the first write so deinit can retry exact restores.
    THUNK.store(memory as usize, Ordering::Release);
    if !crate::equipment_native::patch(SITE, &ORIGINAL, &jump_from(SITE, memory as usize)) {
        return;
    }
    if !crate::equipment_native::patch(
        BAND_SITE,
        &BAND_ORIGINAL,
        &jump_from(BAND_SITE, memory as usize + 128),
    ) {
        let _ = crate::equipment_native::patch(SITE, &jump_from(SITE, memory as usize), &ORIGINAL);
        return;
    }
    if Update.enable().is_err() {
        let _ = crate::equipment_native::patch(
            BAND_SITE,
            &jump_from(BAND_SITE, memory as usize + 128),
            &BAND_ORIGINAL,
        );
        let _ = crate::equipment_native::patch(SITE, &jump_from(SITE, memory as usize), &ORIGINAL);
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_RFD presentation=single-merged-pants-signal normal-source-priority=true alpha=70 scale=random90-150 band=randomSmallMediumLarge count_tiers=1/2/3+ sweep=left-to-right period_ms=1400/960/700 random_ms=180-240/140-190/100-150");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Update.disable();
    let thunk = THUNK.load(Ordering::Acquire);
    if thunk != 0 {
        let _ = crate::equipment_native::patch(
            BAND_SITE,
            &jump_from(BAND_SITE, thunk + 128),
            &BAND_ORIGINAL,
        );
        let _ = crate::equipment_native::patch(SITE, &jump_from(SITE, thunk), &ORIGINAL);
    }
    // Published thunks live until process exit, consistent with the parent host.
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_scope_rejects_nested_commit_and_recovers_on_next_update() {
        let outer = UpdateScope::enter().expect("idle owner");
        assert!(!INTERRUPTED.with(std::cell::Cell::get));
        assert!(UpdateScope::enter().is_none());
        assert!(INTERRUPTED.with(std::cell::Cell::get));
        assert!(UPDATE_ACTIVE.with(std::cell::Cell::get));
        drop(outer);
        assert!(!UPDATE_ACTIVE.with(std::cell::Cell::get));
        let next = UpdateScope::enter().expect("released owner");
        assert!(!INTERRUPTED.with(std::cell::Cell::get));
        drop(next);
    }

    #[test]
    fn nested_native_only_scope_restores_previous_suppression() {
        assert!(!NATIVE_ONLY.with(std::cell::Cell::get));
        let outer = NativeOnly(NATIVE_ONLY.with(|n| n.replace(true)));
        let inner = NativeOnly(NATIVE_ONLY.with(|n| n.replace(true)));
        drop(inner);
        assert!(NATIVE_ONLY.with(std::cell::Cell::get));
        drop(outer);
        assert!(!NATIVE_ONLY.with(std::cell::Cell::get));
    }

    #[test]
    fn scope_cleanup_releases_owner_and_suppression_on_unwind() {
        let r = std::panic::catch_unwind(|| {
            let _owner = UpdateScope::enter().expect("idle owner");
            let _native = NativeOnly(NATIVE_ONLY.with(|n| n.replace(true)));
            panic!("controlled Rust fixture unwind; not an engine exception");
        });
        assert!(r.is_err());
        assert!(!UPDATE_ACTIVE.with(std::cell::Cell::get));
        assert!(!NATIVE_ONLY.with(std::cell::Cell::get));
    }

    #[test]
    fn direction_bridge_replays_store_and_preserves_cpu_state() {
        let b = bridge(0x1000, 0x2000);
        assert_eq!(&b[..8], &ORIGINAL);
        for pattern in [[0x0f, 0xae, 0x04, 0x24], [0x0f, 0xae, 0x0c, 0x24]] {
            assert!(b.windows(4).any(|x| x == pattern));
        }
        let rel = u32::from_le_bytes(b[b.len() - 4..].try_into().unwrap());
        assert_eq!(
            0x1000u32.wrapping_add(b.len() as u32).wrapping_add(rel),
            SITE as u32 + 8
        );
    }

    #[test]
    fn count_only_native_eligible_current_pawns_not_entry_or_radius_count() {
        let first = Entry {
            actor: 1,
            object: 1,
            pawn: 11,
            pawn_object: 2,
            icon: 20,
            marker: 30,
            icon_data: 40,
            marker_data: 50,
        };
        let duplicate = Entry {
            marker: 31,
            ..first
        };
        let second = Entry { pawn: 12, ..first };
        let invalidated = Entry {
            pawn_object: 9,
            ..first
        };
        let normal_or_out_of_range = Entry { pawn: 13, ..first };
        let signals = [first, duplicate, second, invalidated];
        let current = [first, duplicate, second, normal_or_out_of_range];
        let eligible = effective_signals(&signals, &current);
        assert_eq!(
            rfd_rules::unique_count(eligible.iter().map(|e| (e.pawn, e.pawn_object))),
            2
        );
        assert!(!eligible.contains(&normal_or_out_of_range));
        assert!(!eligible.contains(&invalidated));
        assert!(effective_signals(&signals, &[]).is_empty());
    }
    #[test]
    fn animation_bridge_routes_only_selector_and_replays_native_distance_load() {
        let code = band_bridge(0x1000, 0x2000, 0x3000, [0x4000, 0x5000, 0x6000]);
        assert!(code.len() < 384);
        assert_eq!(code.windows(8).filter(|b| *b == BAND_ORIGINAL).count(), 1);
        assert_eq!(
            code.windows(4)
                .filter(|b| *b == [0x0f, 0xae, 0x0c, 0x24])
                .count(),
            4
        );
        // No write to the real distance local: both icon and notification still use it.
        assert!(!code.windows(3).any(|b| b == [0x89, 0x45, 0xbc]));
    }
    #[test]
    #[ignore = "x86 generated bridge and supported-DX11 extracted scale setter"]
    fn execute_random_display_and_band_routes_without_changing_real_distance() {
        unsafe fn executable(bytes: &[u8]) -> usize {
            let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4);
            assert!(!p.is_null());
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as _, bytes.len());
            let mut old = 0;
            assert_ne!(VirtualProtect(p, 4096, 0x20, &mut old), 0);
            assert_ne!(
                FlushInstructionCache(GetCurrentProcess(), p, bytes.len()),
                0
            );
            p as usize
        }
        unsafe extern "system" fn no_override(_: usize) -> u32 {
            0
        }
        unsafe extern "system" fn middle(_: usize) -> u32 {
            2
        }
        unsafe extern "system" fn small(_: usize) -> u32 {
            1
        }
        unsafe extern "system" fn large(_: usize) -> u32 {
            3
        }
        unsafe extern "system" fn invalid(_: usize) -> u32 {
            4
        }
        // Native queue is intentionally absent in this ABI fixture, not a renderer.
        let root = std::path::PathBuf::from(std::env::var("SCBL_RFD_NATIVE_FIXTURE").unwrap());
        let global = 0u32;
        let mut bytes = std::fs::read(root.join("r53-native-scale.bin")).unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "3be09e44ddad421fa125c2688dcc1191bea7b277b98b452f32bc1da4fc1aa4c0"
        );
        let operand = 0x344d674u32.to_le_bytes();
        let mut relocations = 0;
        for i in 0..bytes.len() - 3 {
            if bytes[i..i + 4] == operand {
                bytes[i..i + 4].copy_from_slice(&(&global as *const u32 as u32).to_le_bytes());
                relocations += 1;
            }
        }
        assert_eq!(relocations, 1);
        let mut payload = [0u32; 32];
        let mut object = [0u32; 128];
        object[0x1a8 / 4] = payload.as_mut_ptr() as u32;
        let mut stack = [0u32; 160];
        let frame = unsafe { stack.as_mut_ptr().add(140) } as usize;
        unsafe {
            let setter = std::mem::transmute::<usize, Scale>(executable(&bytes));
            for native_scale in [90.0f32, 100.0, 120.0, 125.0, 150.0] {
                payload[0] = native_scale.to_bits();
                (frame.wrapping_sub(0x44) as *mut f32).write(123.5);
                (frame.wrapping_sub(0xa4) as *mut f32).write(native_scale / 100.0);
                let random_scale = 240.0 - native_scale;
                apply_pants_display(
                    frame,
                    object.as_mut_ptr() as usize,
                    Signal {
                        direction: 0.75,
                        scale: random_scale,
                        band: 2,
                    },
                    setter,
                );
                assert_eq!(f32::from_bits(payload[0]), random_scale);
                assert_eq!((frame.wrapping_sub(0xa0) as *const f32).read(), 0.75);
                assert_eq!((frame.wrapping_sub(0x44) as *const f32).read(), 123.5);
                assert_eq!(
                    (frame.wrapping_sub(0xa4) as *const f32).read(),
                    native_scale / 100.0
                );
                assert_eq!(payload[0x4c / 4] & 8, 8);
            }
            for (callback, route) in [
                (no_override as *const () as u32, 1u32),
                (small as *const () as u32, 2u32),
                (middle as *const () as u32, 3u32),
                (large as *const () as u32, 4u32),
                (invalid as *const () as u32, 1u32),
            ] {
                let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
                assert_ne!(p, 0);
                let mut wrapper = vec![
                    0x55, 0x89, 0xe5, 0x81, 0xec, 0x08, 0x02, 0, 0, 0xc7, 0x45, 0xbc,
                ];
                wrapper.extend(123.5f32.to_bits().to_le_bytes());
                wrapper.push(0xb8);
                wrapper.extend(0.2f32.to_bits().to_le_bytes());
                wrapper.extend([0x66, 0x0f, 0x6e, 0xc0, 0xba]);
                wrapper.extend(0x11223344u32.to_le_bytes());
                wrapper.extend([0xf9, 0xe9]);
                wrapper.extend(
                    ((p + 128) as u32)
                        .wrapping_sub((p + wrapper.len() + 4) as u32)
                        .to_le_bytes(),
                );
                let bridge = band_bridge(
                    (p + 128) as u32,
                    callback,
                    (p + 512) as u32,
                    [(p + 768) as u32, (p + 1024) as u32, (p + 1280) as u32],
                );
                std::ptr::copy_nonoverlapping(wrapper.as_ptr(), p as _, wrapper.len());
                std::ptr::copy_nonoverlapping(bridge.as_ptr(), (p + 128) as _, bridge.len());
                for (offset, id, xmm_low) in [
                    (512usize, 1u32, (123.5f64.to_bits() & 0xffffffff) as u32),
                    (768, 2, 0.2f32.to_bits()),
                    (1024, 3, 0.2f32.to_bits()),
                    (1280, 4, 0.2f32.to_bits()),
                ] {
                    let mut r = vec![0x73, 0];
                    let mut failures = vec![1]; // jnc fail
                    r.extend([0x81, 0xfa]);
                    r.extend(0x11223344u32.to_le_bytes());
                    r.extend([0x75, 0]);
                    failures.push(r.len() - 1);
                    r.extend([0x81, 0x7d, 0xbc]);
                    r.extend(123.5f32.to_bits().to_le_bytes());
                    r.extend([0x75, 0]);
                    failures.push(r.len() - 1);
                    r.extend([0x66, 0x0f, 0x7e, 0xc0, 0x3d]);
                    r.extend(xmm_low.to_le_bytes());
                    r.extend([0x75, 0]);
                    failures.push(r.len() - 1);
                    r.push(0xb8);
                    r.extend(id.to_le_bytes());
                    r.extend([0xc9, 0xc3]);
                    let fail = r.len();
                    r.extend([0x31, 0xc0, 0xc9, 0xc3]);
                    for branch in failures {
                        r[branch] = (fail - branch - 1) as u8;
                    }
                    std::ptr::copy_nonoverlapping(r.as_ptr(), (p + offset) as _, r.len());
                }
                let mut old = 0;
                assert_ne!(VirtualProtect(p as _, 4096, 0x20, &mut old), 0);
                assert_ne!(FlushInstructionCache(GetCurrentProcess(), p as _, 4096), 0);
                let run = std::mem::transmute::<usize, unsafe extern "C" fn() -> u32>(p);
                for _ in 0..1000 {
                    assert_eq!(run(), route);
                }
            }
        }
        println!("R54_NATIVE_UI scales90/100/120/125/150 replaced independently; real_distance123.5 unchanged; native/small/middle/large/invalid routes 1000 each flags,GPR,XMM,stack preserved; not a rendered gameplay test");
    }

    #[test]
    #[ignore = "supported-DX11 native three-band selector; renderer virtual call stubbed"]
    fn execute_retail_wave_selector_normal_and_pants_near_middle_far() {
        unsafe extern "system" fn native(_: usize) -> u32 {
            0
        }
        unsafe extern "system" fn small(_: usize) -> u32 {
            1
        }
        unsafe extern "system" fn middle(_: usize) -> u32 {
            2
        }
        unsafe extern "system" fn large(_: usize) -> u32 {
            3
        }
        let root = std::path::PathBuf::from(std::env::var("SCBL_RFD_NATIVE_FIXTURE").unwrap());
        let selector = std::fs::read(root.join("r53-native-band.bin")).unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(&selector)),
            "71104c21e93f64b4105d2aa129e45e7de270df484f7776c403c64cdc3d962db2"
        );
        let mut hud = [0u32; 256];
        let mut item = [0u32; 10];
        let mut movie = [0u32; 4];
        let mut vtable = [0u32; 80];
        hud[0x2ac / 4] = item.as_mut_ptr() as u32;
        item[8 / 4] = movie.as_mut_ptr() as u32;
        movie[0] = vtable.as_mut_ptr() as u32;
        unsafe {
            for (override_band, callback) in [
                (None, native as *const () as u32),
                (Some(2), small as *const () as u32),
                (Some(1), middle as *const () as u32),
                (Some(0), large as *const () as u32),
            ] {
                for (distance, native_band, native_label) in [
                    (50f32, 0, 0x2c40bd0u32),
                    (200., 1, 0x2c40bc4),
                    (800., 2, 0x2c40bb8),
                ] {
                    let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
                    assert_ne!(p, 0);
                    let mut wrapper = vec![0x55, 0x89, 0xe5, 0x81, 0xec, 0x08, 0x02, 0, 0];
                    for (offset, value) in [
                        (-0x1b0i32, hud.as_mut_ptr() as u32),
                        (-0x24, 0),
                        (-0x44, distance.to_bits()),
                        (-0x18, 100f32.to_bits()),
                        (-0x1c, 400f32.to_bits()),
                    ] {
                        wrapper.extend([0xc7, 0x85]);
                        wrapper.extend(offset.to_le_bytes());
                        wrapper.extend(value.to_le_bytes());
                    }
                    wrapper.push(0xe9);
                    wrapper.extend(
                        ((p + 128) as u32)
                            .wrapping_sub((p + wrapper.len() + 4) as u32)
                            .to_le_bytes(),
                    );
                    let code = band_bridge(
                        (p + 128) as u32,
                        callback,
                        (p + 512 + 8) as u32,
                        [BAND_SMALL, BAND_MIDDLE, BAND_LARGE]
                            .map(|address| (p + 512 + address - BAND_SITE) as u32),
                    );
                    // Native selector branches stay byte-identical and relative to its copy.
                    let mut resume = vec![0x81, 0x7d, 0xbc];
                    resume.extend(distance.to_bits().to_le_bytes());
                    resume.extend([0x75, 0x07, 0xa1]);
                    resume.extend((&item[0x14 / 4] as *const u32 as u32).to_le_bytes());
                    resume.extend([0xc9, 0xc3, 0xb8]);
                    resume.extend(u32::MAX.to_le_bytes());
                    resume.extend([0xc9, 0xc3]);
                    let movie_stub = [
                        0x8b, 0x44, 0x24, 4, 0x89, 0x41, 4, 0xff, 0x41, 8, 0xc2, 4, 0,
                    ];
                    vtable[0x12c / 4] = (p + 1024) as u32;
                    for (offset, data) in [
                        (0, wrapper.as_slice()),
                        (128, code.as_slice()),
                        (512, selector.as_slice()),
                        (512 + selector.len(), resume.as_slice()),
                        (1024, movie_stub.as_slice()),
                    ] {
                        std::ptr::copy_nonoverlapping(data.as_ptr(), (p + offset) as _, data.len());
                    }
                    let mut old = 0;
                    assert_ne!(VirtualProtect(p as _, 4096, 0x20, &mut old), 0);
                    assert_ne!(FlushInstructionCache(GetCurrentProcess(), p as _, 4096), 0);
                    let run = std::mem::transmute::<usize, unsafe extern "C" fn() -> u32>(p);
                    item[0x14 / 4] = u32::MAX;
                    movie[1] = 0;
                    movie[2] = 0;
                    let expected = override_band.unwrap_or(native_band);
                    for _ in 0..1000 {
                        assert_eq!(run(), expected);
                    }
                    assert_eq!(item[0x14 / 4], expected);
                    assert_eq!(
                        movie[1],
                        override_band.map_or(native_label, |v| [0x2c40bd0, 0x2c40bc4, 0x2c40bb8]
                            [v as usize])
                    );
                    assert_eq!(
                        movie[2], 1,
                        "native cached-band guard must not restart animation every frame"
                    );
                }
            }
        }
        println!("R54_RETAIL_BANDS normal near/middle/far=WaveLarge/WaveMedium/WaveSmall; pants each chosen Small/Medium/Large independent of near/middle/far; unchanged real distance and native animation caching (1 play per 1000 updates)");
    }
    #[test]
    #[ignore = "explicit supported-DX11 extracted native byte fixture"]
    fn execute_native_alpha_visibility_and_direction_bridge() {
        unsafe extern "system" fn change(frame: usize) {
            (frame.wrapping_sub(0xa0) as *mut f32).write(0.75);
        }
        unsafe fn executable(bytes: &[u8]) -> usize {
            let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4);
            assert!(!p.is_null());
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as _, bytes.len());
            let mut old = 0;
            assert_ne!(VirtualProtect(p, 4096, 0x20, &mut old), 0);
            assert_ne!(
                FlushInstructionCache(GetCurrentProcess(), p, bytes.len()),
                0
            );
            p as usize
        }
        let root = std::path::PathBuf::from(std::env::var("SCBL_RFD_NATIVE_FIXTURE").unwrap());
        let global = 0u32;
        let mut payload = [0u32; 32];
        payload[0] = 125f32.to_bits();
        payload[0x38 / 4] = 80f32.to_bits();
        payload[0x40 / 4] = 1;
        let mut object = [0u32; 128];
        object[0x1a8 / 4] = payload.as_mut_ptr() as u32;
        unsafe {
            for (name, expected, is_alpha) in [
                (
                    "r51-native-alpha.bin",
                    "ddd423ec3b4594925d54736f5c6089c1d294d6c337fc871893eebc92a3d19433",
                    true,
                ),
                (
                    "r51-native-visible.bin",
                    "0af8694bc1f619cc24e1aa03c398e6d78cc3d9923e4a21f078c02803791ea40f",
                    false,
                ),
            ] {
                let mut bytes = std::fs::read(root.join(name)).unwrap();
                use sha2::{Digest, Sha256};
                assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
                let operand = 0x344d674u32.to_le_bytes();
                let mut relocations = 0;
                for i in 0..bytes.len() - 3 {
                    if bytes[i..i + 4] == operand {
                        bytes[i..i + 4]
                            .copy_from_slice(&(&global as *const u32 as u32).to_le_bytes());
                        relocations += 1;
                    }
                }
                assert_eq!(relocations, 1);
                let p = executable(&bytes);
                if is_alpha {
                    std::mem::transmute::<usize, Alpha>(p)(object.as_mut_ptr() as usize, 40.0);
                } else {
                    std::mem::transmute::<usize, Visible>(p)(object.as_mut_ptr() as usize, 0);
                }
            }
            assert_eq!(
                f32::from_bits(payload[0]),
                125.0,
                "native signal scale untouched"
            );
            assert_eq!(f32::from_bits(payload[0x38 / 4]), 40.0);
            assert_eq!(payload[0x40 / 4] & 255, 0);
            assert_eq!(payload[0x4c / 4] & (0x400 | 0x40), 0x440);
            let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
            assert_ne!(p, 0);
            // Establish the exact native EBP frame, XMM0=0.2 and jump through
            // the real generated bridge. Resume checks XMM0 and reads -A0.
            let mut wrapper = vec![0x55, 0x89, 0xe5, 0x81, 0xec, 0x08, 0x02, 0, 0, 0xb8];
            wrapper.extend(0.2f32.to_bits().to_le_bytes());
            wrapper.extend([0x66, 0x0f, 0x6e, 0xc0, 0xe9]);
            wrapper.extend(
                ((p + 128) as u32)
                    .wrapping_sub((p + wrapper.len() + 4) as u32)
                    .to_le_bytes(),
            );
            let mut code = bridge((p + 128) as u32, change as *const () as u32);
            let n = code.len();
            code[n - 4..].copy_from_slice(
                &((p + 512) as u32)
                    .wrapping_sub((p + 128 + n) as u32)
                    .to_le_bytes(),
            );
            let mut resume = vec![0x66, 0x0f, 0x7e, 0xc2, 0x81, 0xfa];
            resume.extend(0.2f32.to_bits().to_le_bytes());
            resume.extend([
                0x75, 0x08, 0x8b, 0x85, 0x60, 0xff, 0xff, 0xff, 0xc9, 0xc3, 0x31, 0xc0, 0xc9, 0xc3,
            ]);
            std::ptr::copy_nonoverlapping(wrapper.as_ptr(), p as _, wrapper.len());
            std::ptr::copy_nonoverlapping(code.as_ptr(), (p + 128) as _, code.len());
            std::ptr::copy_nonoverlapping(resume.as_ptr(), (p + 512) as _, resume.len());
            let mut old = 0;
            assert_ne!(VirtualProtect(p as _, 4096, 0x20, &mut old), 0);
            assert_ne!(FlushInstructionCache(GetCurrentProcess(), p as _, 4096), 0);
            let run = std::mem::transmute::<usize, unsafe extern "C" fn() -> u32>(p);
            for _ in 0..1000 {
                assert_eq!(run(), 0.75f32.to_bits());
            }
        }
        println!("R51_NATIVE_UI alpha80->40 visible1->0 strength125->125; direction0.2->0.75 XMM/stack preserved 1000 calls; queue-global stubbed, not real renderer");
    }
}

#[cfg(test)]
mod c55_range_tests {
    use super::*;
    #[test]
    fn pants_range_actual_coordinates_and_scene_only() {
        unsafe {
            let mut hud = vec![0u32; 0x300 / 4];
            let mut context = vec![0u32; 0x314 / 4];
            let mut merc = vec![0u32; 0x380 / 4];
            let mut spy = vec![0u32; 0x380 / 4];
            hud[0x25c / 4] = context.as_ptr() as u32;
            context[0x310 / 4] = merc.as_ptr() as u32;
            merc[0] = 0x02ad2abc;
            merc[0x144 / 4] = 42;
            spy[0x144 / 4] = 42;
            for (x, y, yes) in [
                (1000.0f32, 0.0f32, true),
                (1000.01, 0.0, false),
                (600.0, 800.0, true),
                (600.0, 800.1, false),
                (f32::NAN, 0.0, false),
            ] {
                spy[0x9c / 4] = x.to_bits();
                spy[0xa0 / 4] = y.to_bits();
                assert_eq!(
                    interference_in_range(hud.as_ptr() as usize, spy.as_ptr() as usize),
                    yes
                );
            }
            spy[0x9c / 4] = 0;
            spy[0xa0 / 4] = 0;
            spy[0x144 / 4] = 43;
            assert!(!interference_in_range(
                hud.as_ptr() as usize,
                spy.as_ptr() as usize
            ));
            assert!(!interference_in_range(0, 0));
        }
    }
}
