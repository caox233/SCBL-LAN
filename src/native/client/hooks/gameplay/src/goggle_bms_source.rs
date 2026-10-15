//! Observe the native BMS result AFTER its complete Update. No distance test,
//! native query replay, synthetic tag, or visibility write is performed here.
//! This collector alone does not deliver a warning to another peer.
use crate::ats_marks::Role;
use crate::ats_native_source::{self, Frame};
use crate::equipment_native::{readable, signature, word};
use retour::static_detour;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

const HUD: u32 = 0x2c3e9e4;
const UPDATE: usize = 0x1a58930;
const UPDATE_LEN: usize = 0xee1;
const UPDATE_SHA: &str = "fbcfc2cd74153f82f0ac45947bfcffe9c1ae41b3059be1e900bde7664c85c285";
static READY: AtomicBool = AtomicBool::new(false);
static CURRENT: Mutex<Option<Snapshot>> = Mutex::new(None);
static_detour! { static Update: unsafe extern "thiscall" fn(usize, f32); }

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub world: usize,
    pub local: usize,
    pub native_now_ms: u64,
    /// Native peer identities only; never retain target pawn/widget pointers.
    pub targets: Vec<u64>,
}

unsafe fn byte(p: usize, offset: usize) -> Option<u8> {
    let p = p.checked_add(offset)?;
    readable(p, 1).then(|| (p as *const u8).read())
}
unsafe fn capture(hud: usize, frame: &Frame, people: &[(u32, u64, Role)]) -> Option<Snapshot> {
    if word(hud, 0)? != HUD || people.len() > 32 {
        return None;
    }
    // Exact 1A52310 HUD owner chain. Reject a stale/spectator/foreign HUD.
    let context = word(hud, 0x25c)? as usize;
    if word(context, 0x310)? as usize != frame.local {
        return None;
    }
    let mut result = Snapshot {
        world: frame.world,
        local: frame.local,
        native_now_ms: frame.now,
        targets: Vec::new(),
    };
    match byte(hud, 0x328)? {
        0 => return Some(result), // native goggles/disruption gate is off
        1 => (),
        _ => return None,
    }
    // Native entry layout: widget +0, object key +4, is-player flag +8.
    // +8 IS NOT a pawn pointer. Retail resolves +4 through live roster +20.
    let (p, n, cap) = (
        word(hud, 0x2b4)? as usize,
        word(hud, 0x2b8)? as usize,
        word(hud, 0x2bc)? as usize,
    );
    if n > 128 || cap < n || cap > 4096 || (n != 0 && !readable(p, n * 0x2c)) {
        return None;
    }
    for i in 0..n {
        let entry = p + i * 0x2c;
        if word(entry, 8)? == 0 {
            continue;
        }
        let object = word(entry, 4)?;
        let mut matches = people
            .iter()
            .filter(|&&(id, key, role)| id == object && key != 0 && role == Role::Spy);
        let Some(&(_, target, _)) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            return None;
        }
        let widget = word(entry, 0)? as usize;
        let Some(display) = word(widget, 0x1a8).map(|p| p as usize) else {
            continue;
        };
        // 18A1C80 stores the native accepted/rejected state at DisplayData+40.
        // BMS pulse alpha is cosmetic: do not confuse fade alpha with rejection.
        if byte(display, 0x40) == Some(1) && !result.targets.contains(&target) {
            result.targets.push(target);
        }
    }
    Some(result)
}

unsafe fn observe(hud: usize) -> Option<Snapshot> {
    let f = ats_native_source::frame()?;
    let people = ats_native_source::detector_players(&f)?;
    capture(hud, &f, &people)
}
fn tick(hud: usize, dt: f32) {
    unsafe {
        Update.call(hud, dt);
    } // original exactly once, result not overridden
    if READY.load(Ordering::Acquire) {
        let next = unsafe { observe(hud) };
        if let Some(s) = &next {
            crate::goggle_feedback_native::observe(crate::goggle_exposure::Source::Bms,
                s.world, s.local, s.native_now_ms, &s.targets, false);
        }
        if let Ok(mut state) = CURRENT.lock() {
            *state = next;
        }
    }
}
/// Consumer must match the current local scene and current native frame clock;
/// never treat a last cached detection as a new observation.
#[allow(dead_code)]
pub(crate) fn snapshot(frame: &Frame) -> Option<Snapshot> {
    let state = CURRENT.lock().ok()?;
    state
        .as_ref()
        .filter(|s| {
            s.world == frame.world && s.local == frame.local && s.native_now_ms == frame.now
        })
        .cloned()
}
pub(crate) unsafe fn init(world_raycast: usize) {
    if world_raycast != 0x589860
        || !signature(UPDATE, UPDATE_LEN, UPDATE_SHA)
        || !signature(
            0x1a3fd60,
            0x5b,
            "63b549e67f1857479863d6d792ed918d9208e22dc9884f5cd52a0c5c61081ed3",
        )
        || !signature(
            0x1a52310,
            0x47,
            "82b05f2280f757c010bcd55a2f4492f2f4e902f298ae5b71ee69d1d8f4e22316",
        )
        || !signature(
            0x44d890,
            0x17,
            "ad9e471e9702b455f2edc32635ee9f23df2568ec719aee4f70e01e149e8ec38b",
        )
    {
        return;
    }
    if Update
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(UPDATE),
            tick,
        )
        .and_then(|h| h.enable())
        .is_err()
    {
        deinit();
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_GOGGLE_OBSERVER source=native_BMS_result observer_ready=true peer_delivery=false warning_enabled=false");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Update.disable();
    if let Ok(mut state) = CURRENT.lock() {
        *state = None;
    }
}

#[cfg(test)]
#[path = "goggle_bms_source_tests.rs"]
mod tests;
