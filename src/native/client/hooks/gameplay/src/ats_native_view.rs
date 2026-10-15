//! Additional fixed-position suppressed-shot UI, not a replacement ATS HUD.
//! Source callbacks are native shot boundaries; only the HUD thread owns UI.
//! No actor position spoof, no global ATS enable, no game marking/name flags.
use crate::ats_marks::Mark;
use crate::ats_native_source::{self, Frame};
use crate::equipment_native::{signature, word};
use retour::static_detour;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
#[path = "ats_native_ui.rs"]
mod ui;
static READY: AtomicBool = AtomicBool::new(false);
static THREAD: AtomicU32 = AtomicU32::new(0);
static LOGS: AtomicU32 = AtomicU32::new(0);
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
}
static_detour! {
 static ShotHook:unsafe extern "thiscall" fn(usize);
 static TickHook:unsafe extern "thiscall" fn(usize,f32);
 static DestroyHook:unsafe extern "thiscall" fn(usize);
}
trait Surface {
    type Handle;
    fn create(&mut self, hud: usize) -> Option<Self::Handle>;
    fn restart(&mut self, h: &mut Self::Handle);
    fn update(&mut self, hud: usize, h: &mut Self::Handle, f: &Frame, m: &Mark);
    fn fade(&mut self, h: &mut Self::Handle);
    fn destroy(&mut self, h: Self::Handle);
    fn notify(&mut self);
}
struct Item<H> {
    handle: H,
    mark: Mark,
    fade_end: Option<u64>,
}
struct View<H> {
    hud: usize,
    key: (usize, usize, usize, u32),
    last: u64,
    items: BTreeMap<u64, Item<H>>,
}
impl<H> Default for View<H> {
    fn default() -> Self {
        Self {
            hud: 0,
            key: (0, 0, 0, 0),
            last: 0,
            items: BTreeMap::new(),
        }
    }
}
impl<H> View<H> {
    fn clear<S: Surface<Handle = H>>(&mut self, s: &mut S) {
        for (_, i) in std::mem::take(&mut self.items) {
            s.destroy(i.handle);
        }
        self.hud = 0;
        self.key = (0, 0, 0, 0);
        self.last = 0;
    }
    fn frame<S: Surface<Handle = H>>(
        &mut self,
        s: &mut S,
        hud: usize,
        f: Option<&Frame>,
        marks: &[Mark],
    ) {
        let Some(f) = f else {
            self.clear(s);
            return;
        };
        let key = (f.world, f.info, f.local, f.team);
        if self.hud != hud || self.key != key || f.now < self.last {
            self.clear(s);
        }
        self.hud = hud;
        self.key = key;
        self.last = f.now;
        // The caller validates scene/team/role and returns live copied marks only.
        // Do not create hidden/expired events when entering a menu/dead spectator.
        for m in marks.iter().take(32) {
            if m.source == 0
                || f.now < m.shot_time_ms
                || f.now >= m.expires_ms
                || !m.position.iter().all(|v| v.is_finite())
            {
                continue;
            }
            if let Some(i) = self.items.get_mut(&m.source) {
                if i.mark.serial != m.serial {
                    if i.fade_end.take().is_some() {
                        s.restart(&mut i.handle);
                    }
                    i.mark = *m;
                }
            } else if f.visible {
                if let Some(mut h) = s.create(hud) {
                    s.restart(&mut h);
                    s.notify();
                    self.items.insert(
                        m.source,
                        Item {
                            handle: h,
                            mark: *m,
                            fade_end: None,
                        },
                    );
                }
            }
        }
        let mut dead = Vec::new();
        for (&id, i) in &mut self.items {
            let retained = marks
                .iter()
                .any(|m| m.source == id && m.serial == i.mark.serial && f.now < m.expires_ms);
            if !retained && i.fade_end.is_none() {
                s.fade(&mut i.handle);
                i.fade_end = Some(f.now.saturating_add(500)); // native fade_low/IconEnd duration
            }
            if i.fade_end.is_some_and(|end| f.now >= end) {
                dead.push(id);
            } else {
                s.update(hud, &mut i.handle, f, &i.mark);
            }
        }
        for id in dead {
            if let Some(i) = self.items.remove(&id) {
                s.destroy(i.handle);
            }
        }
    }
}
thread_local! {static VIEW:RefCell<View<ui::Handles>>=RefCell::new(View::default());}
thread_local! {static LAST_REPORT:Cell<Option<(usize,bool,usize,usize,usize)>>=const {Cell::new(None)};}
// The retail ATS tick sets/tests +2F8 via 1A3FBD0/1A6FA6D. The generic
// wrapper's +231 is initialized to zero and is NOT this detector's show gate.
// Children remain parented to the native HUD: global menu/HUD hiding stays native.
unsafe fn hud_visible(hud: usize, f: &Frame) -> bool {
    word(hud, 0) == Some(0x2c3e85c)
        && word(hud, 0x25c).and_then(|p| word(p as usize, 0x310)) == Some(f.local as u32)
        && crate::equipment_native::readable(hud + 0x2f8, 1)
        && *((hud + 0x2f8) as *const u8) == 1
}
fn shot(pawn: usize) {
    unsafe {
        if READY.load(Ordering::Acquire) {
            ats_native_source::observe_source(pawn);
        }
        ShotHook.call(pawn); // preserve every native event exactly once
    }
}
fn tick(hud: usize, dt: f32) {
    unsafe {
        // Default retains the native ATS tick once. The narrow feedback feature
        // observes the same original call; diagnostic sampling is independent.
        #[cfg(feature = "goggle-feedback")]
        crate::goggle_ats_source::update(hud, || TickHook.call(hud, dt));
        #[cfg(not(feature = "goggle-feedback"))]
        TickHook.call(hud, dt);
        if !READY.load(Ordering::Acquire) || word(hud, 0) != Some(0x2c3e85c) {
            return;
        }
        let current = GetCurrentThreadId();
        let owner = THREAD.load(Ordering::Acquire);
        if owner == 0 {
            THREAD.store(current, Ordering::Release);
        } else if owner != current {
            return;
        }
        let mut f = ats_native_source::frame();
        if let Some(f) = &mut f {
            f.visible = hud_visible(hud, f);
        }
        let marks = ats_native_source::snapshot_frame(f.as_ref());
        VIEW.with(|v| {
            if let Ok(mut v) = v.try_borrow_mut() {
                let mut native = ui::Native::retail();
                v.frame(&mut native, hud, f.as_ref(), &marks);
                let active = f.as_ref().is_some_and(|f| f.visible);
                let positioned = v.items.values().filter(|i| i.handle.positioned).count();
                let report = (hud, active, marks.len(), v.items.len(), positioned);
                let changed = LAST_REPORT.with(|last| last.replace(Some(report)) != Some(report));
                if !marks.is_empty() && changed && LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
                    // A queued mark is not proof of creation, projection, or rendered pixels.
                    tracing::info!("SCBL_MOD_ATS_LAST_SHOT revision=C56 marks={} native_hud_active={} owned_icons={} positioned_icons={} positions=frozen live_acceptance=pending", marks.len(), active, v.items.len(), positioned);
                }
            }
        });
    }
}
fn destroyed(hud: usize) {
    unsafe {
        #[cfg(feature = "goggle-feedback")]
        crate::goggle_ats_source::clear();
        VIEW.with(|v| {
            if let Ok(mut v) = v.try_borrow_mut() {
                if v.hud == hud {
                    v.clear(&mut ui::Native::retail());
                }
            }
        });
        LAST_REPORT.with(|last| last.set(None));
        DestroyHook.call(hud); // all native handles and game arrays remain native-owned
    }
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860
        || !SIGNATURES.iter().all(|&(a, n, h)| signature(a, n, h))
        || !ats_native_source::init()
    {
        tracing::warn!("SCBL_MOD_ATS_LAST_SHOT enabled=false reason=signature");
        return;
    }
    // Install inner read-only observers only after the unmodified Tick body was
    // verified above. Failure leaves the existing ATS feature native/unchanged.
    #[cfg(feature = "goggle-feedback")]
    let _ = crate::goggle_ats_source::init(world);
    if ShotHook
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0xd76240),
            shot,
        )
        .is_err()
        || TickHook
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0x1a6f990),
                tick,
            )
            .is_err()
        || DestroyHook
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0x1a667d0),
                destroyed,
            )
            .is_err()
        || DestroyHook.enable().is_err()
        || TickHook.enable().is_err()
        || ShotHook.enable().is_err()
    {
        deinit();
        tracing::warn!("SCBL_MOD_ATS_LAST_SHOT enabled=false reason=hook");
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_ATS_LAST_SHOT enabled=true source=native-shot listener=native-roster fixed_position=true asset=BATS_Icon live_acceptance=pending");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    ats_native_source::deinit();
    if THREAD.load(Ordering::Acquire) == GetCurrentThreadId() {
        VIEW.with(|v| {
            if let Ok(mut v) = v.try_borrow_mut() {
                v.clear(&mut ui::Native::retail());
            }
        });
    }
    // Cross-thread process shutdown must not call GFx on the loader/shutdown
    // thread. Ordinary HUD destruction clears handles above on their owner thread.
    let _ = ShotHook.disable();
    let _ = TickHook.disable();
    #[cfg(feature = "goggle-feedback")]
    crate::goggle_ats_source::deinit();
    let _ = DestroyHook.disable();
}
include!("ats_native_view_signatures.rs");
#[cfg(test)]
#[path = "ats_native_view_tests.rs"]
mod tests;
