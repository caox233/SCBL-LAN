//! Start the native close signal and its native 0.2-second exit timer together.
//! Voluntary and current local disabled bodies share native close presentation.
//! Disable/falling and HP0 request native TransOut(0), then its existing close
//! signal and timer. Body fall/deletion remain native; no synthetic UI event.
use crate::equipment_native::{signature, word};
use super::super::{destroyed_exit_in_progress, with_destroyed_exit, Layout};
use retour::static_detour;
use std::sync::{atomic::{AtomicBool, Ordering}, Mutex};

const HUD: u32 = 0x02c3ccac;
const STATE: u32 = 0x02bebe74;
const PAWN: u32 = 0x02ad2abc;
const ADV_DRONE: u32 = 0x01000017;
static READY: AtomicBool = AtomicBool::new(false);
static CURRENT: Mutex<Option<Binding>> = Mutex::new(None);
type Mode = unsafe extern "thiscall" fn(usize, u32);
type Dtor = unsafe extern "thiscall" fn(usize);
type Update = unsafe extern "thiscall" fn(usize, f32);
static_detour! {
    static HudMode: unsafe extern "thiscall" fn(usize, u32);
    static HudDtor: unsafe extern "thiscall" fn(usize);
    static StateUpdate: unsafe extern "thiscall" fn(usize, f32);
}
#[link(name = "kernel32")]
extern "system" { fn GetCurrentThreadId() -> u32; }

#[derive(Clone, Copy)]
struct Binding { hud: usize, controller: usize, owner: usize, thread: u32 }
impl Binding {
    unsafe fn capture(hud: usize, vt: u32) -> Option<Self> {
        if word(hud, 0) != Some(vt) || word(hud, 0x2e4) != Some(0) { return None; }
        let controller = word(hud, 0x25c)? as usize;
        let owner = word(controller, 0x310)? as usize;
        if controller == 0 || owner == 0 { return None; }
        Some(Self { hud, controller, owner, thread: GetCurrentThreadId() })
    }
    unsafe fn matches(self, state: usize, vt: u32, state_vt: u32, pawn_vt: u32, closing: bool) -> bool {
        // The native mode event and TransOut must be on the same game thread.
        // A foreign render/thread callback cannot dereference a cached HUD.
        self.thread == GetCurrentThreadId()
            && word(state, 0) == Some(state_vt)
            && word(state, 0x48) == Some(self.owner as u32)
            && word(state, 0x64) == Some(self.controller as u32)
            && word(self.owner, 0) == Some(pawn_vt)
            && word(self.owner, 0x938) == Some(self.controller as u32)
            && word(self.controller, 0x310) == Some(self.owner as u32)
            && word(self.hud, 0) == Some(vt)
            && word(self.hud, 0x25c) == Some(self.controller as u32)
            && word(self.hud, 0x2e4).is_some_and(|phase| phase == 0 || (closing && phase == 1))
    }
    unsafe fn begin(self, state: usize, vt: u32, state_vt: u32, pawn_vt: u32, close: impl FnOnce(usize)) -> bool {
        if !self.matches(state, vt, state_vt, pawn_vt, false)
            || word(state, 0x148).is_none_or(|flags| flags & 1 == 0) { return false; }
        // Native TransOut has queued exit. Start BOTH DroneEnd and +14C=0.2
        // through its own animation callback; native tick finishes the exit.
        close(state);
        true
    }
}
// Called only from the native MP-state Update, with the current same-thread HUD
// binding. Do not require ability+A0: native ability cleanup may already clear it
// while this state still owns its +138 controlled body.
unsafe fn finish_destroyed(
    binding: Option<Binding>, state: usize, layout: Layout, hud_vt: u32,
    finish: impl FnOnce(usize),
) -> bool {
    if destroyed_exit_in_progress(state) { return true; }
    let Some(binding) = binding else { return false; };
    if !binding.matches(state, hud_vt, layout.state, layout.owner,
            word(state, 0x148).is_some_and(|flags| flags & 1 != 0))
        || word(state, 0x30).is_none_or(|v| v & 1 != 0)
        || word(binding.owner, 0x60).is_none_or(|v| v & 0x800 != 0)
        || word(binding.owner, 0x4c).is_none_or(|v| !matches!((v >> 8) & 255, 1 | 3))
    { return false; }
    let Some(body) = word(state, 0x138).filter(|p| *p != 0).map(|p| p as usize) else { return false; };
    if word(body, 0) != Some(layout.body)
        || word(body, 0x9fc) != Some(binding.owner as u32)
        || word(body, 0x4b0).is_none_or(|v| v & 0x80000000 != 0)
        || (word(body, 0x5dc).is_none_or(|hp| (hp as i32) > 0)
            && word(body, 0x910).map(|v| v & 255) != Some(3))
        || word(body, 0x910).is_none_or(|v| !matches!(v & 255, 2 | 3))
    { return false; }
    with_destroyed_exit(state, body, layout, || finish(state));
    true
}
unsafe fn advance_disabled(
    binding: Option<Binding>, state: usize, layout: Layout, hud_vt: u32,
    present: impl FnOnce(usize, u32), update: impl FnOnce(usize),
) -> bool {
    finish_destroyed(binding, state, layout, hud_vt, |s| {
        // Native +148 is the queued transition and +14C owns its timer. Never
        // re-run the close callback on later frames or reset the native timer.
        if word(s, 0x148).is_some_and(|flags| flags & 1 != 0) {
            update(s);
        } else {
            present(s, 0);
        }
    })
}

fn update(state: usize, dt: f32) {
    unsafe {
        if READY.load(Ordering::Acquire) {
            let binding = CURRENT.try_lock().ok().and_then(|slot| *slot);
            if advance_disabled(binding, state, Layout::RETAIL, HUD,
                |s, mode| {
                    transition(mode, || true, || { let _ = begin(s); },
                        |mode| super::TransOut.call(s, mode));
                },
                |s| StateUpdate.call(s, dt),
            ) {
                return;
            }
        }
        StateUpdate.call(state, dt);
    }
}
fn clear(hud: Option<usize>) {
    if let Ok(mut slot) = CURRENT.lock() {
        if hud.is_none() || slot.is_some_and(|b| Some(b.hud) == hud) { *slot = None; }
    }
}
fn mode(hud: usize, value: u32) {
    unsafe {
        // Original start/reset first, then bind only this active Drone HUD.
        HudMode.call(hud, value);
        if !READY.load(Ordering::Acquire) { return; }
        if value == ADV_DRONE {
            if let Ok(mut slot) = CURRENT.lock() { *slot = Binding::capture(hud, HUD); }
        } else { clear(Some(hud)); }
    }
}
fn dtor(hud: usize) {
    clear(Some(hud)); // invalidate before the original frees children / changes vt
    unsafe { HudDtor.call(hud); }
}
pub(super) unsafe fn begin(state: usize) -> bool {
    if !READY.load(Ordering::Acquire) { return false; }
    let binding = CURRENT.try_lock().ok().and_then(|slot| *slot);
    let Some(binding) = binding else { return false; };
    binding.begin(state, HUD, STATE, PAWN, |s| {
        let close: unsafe extern "thiscall" fn(usize, u32) -> u8 = std::mem::transmute(0x149d680usize);
        close(s, 0);
    })
}
/// Keep native TransOut and its argument unchanged; start the native close
/// callback only AFTER it has committed a valid local voluntary exit.
pub(super) fn transition<R>(
    native: u32, accepted: impl FnOnce() -> bool,
    close: impl FnOnce(), original: impl FnOnce(u32) -> R,
) -> R {
    let eligible = native == 0 && accepted();
    let result = original(native);
    if eligible { close(); }
    result
}
pub(super) unsafe fn init() -> bool {
    if READY.load(Ordering::Acquire) { return true; }
    if !signature(0x1a4bd00, 65, "019423126606ceb7a82cf40b2d897e1abc839c8bf33e5cf1d4da1f172ac3c301")
        || !signature(0x1a44af0, 105, "3ce7e613563aecda3b50c26f7e6a73878557ba956b9793227a2cd7eecc74dd0d")
        || !signature(0x1a4bc90, 112, "22bf8d379c3198902d26431d4b2433036e0095ea3c63d1ce8d3419d30d8beeb6")
        || !signature(0x1a52310, 71, "82b05f2280f757c010bcd55a2f4492f2f4e902f298ae5b71ee69d1d8f4e22316")
        || !signature(0x149d680, 35, "65a250c0e6057880da7129b6eb1e63d1754497bb018f0829fc9425a56e72f034")
        || !signature(0x14812c0, 143, "0bbdb919d0b4b277f39182547e9a0895ad20806efd4bdc601a5ed13b4a675cf7")
        || !signature(0x14ca680, 62, "64a4519d333d5493f32098c84c00492e7f8a8d4dca8c74b630674cfcb98576de")
        || !signature(0x1a4bd50, 25, "41f37601d0541e94d025251ebe5e59b1d7f6237d604bd8ed7c8001aa84084519")
        || word(STATE as usize, 0x94) != Some(0x14812c0)
        || word(STATE as usize, 0x170) != Some(0x14ca680)
        || word(HUD as usize, 0x178) != Some(0x1a4bd00)
        || word(HUD as usize, 0x12c) != Some(0x18f2400)
    { return false; }
    if HudMode.initialize(std::mem::transmute::<usize, Mode>(0x1a4bd00), mode).is_err()
        || HudDtor.initialize(std::mem::transmute::<usize, Dtor>(0x1a4bc90), dtor).is_err()
        || StateUpdate.initialize(std::mem::transmute::<usize, Update>(0x14812c0), update).is_err()
        || HudDtor.enable().is_err() || HudMode.enable().is_err() || StateUpdate.enable().is_err()
    { deinit(); return false; }
    clear(None);
    READY.store(true, Ordering::Release);
    true
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = StateUpdate.disable();
    clear(None);
    let _ = HudMode.disable();
    let _ = HudDtor.disable();
}
// Test-only native fixture adapters: production decision bodies remain shared.
#[cfg(test)]
pub(super) unsafe fn test_finish_destroyed(
    hud: usize, state: usize, layout: Layout, hud_vt: u32, finish: impl FnOnce(usize),
) -> bool {
    finish_destroyed(Binding::capture(hud, hud_vt), state, layout, hud_vt, finish)
}
#[cfg(test)]
pub(super) unsafe fn test_advance_disabled(
    hud: usize, state: usize, layout: Layout, hud_vt: u32,
    present: impl FnOnce(usize, u32), update: impl FnOnce(usize),
) -> bool {
    let binding = CURRENT.try_lock().ok().and_then(|slot| *slot);
    advance_disabled(binding, state, layout, hud_vt, present, update)
}
#[cfg(test)]
#[path = "drone_exit_presentation_tests.rs"]
mod tests;

#[cfg(test)]
pub(super) unsafe fn test_bind_disabled(hud: usize, hud_vt: u32) -> bool {
    let binding = Binding::capture(hud, hud_vt);
    *CURRENT.lock().unwrap() = binding;
    binding.is_some()
}
