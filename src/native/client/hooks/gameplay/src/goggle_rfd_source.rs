//! Read the native RFD result immediately after Update, before pants display
//! hides icons/merges/randomizes waves. Owns no detour and writes no game state.
use crate::ats_marks::Role;
use crate::ats_native_source::{self, Frame};
use crate::equipment_native::{readable, signature, word};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

const HUD: u32 = 0x2c3eb6c;
static READY: AtomicBool = AtomicBool::new(false);
static CURRENT: Mutex<Option<Snapshot>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub world: usize,
    pub local: usize,
    pub native_now_ms: u64,
    pub targets: Vec<u64>,
    pub enabled: bool,
    pub interference: bool,
}
unsafe fn byte(p: usize, offset: usize) -> Option<u8> {
    let p = p.checked_add(offset)?;
    readable(p, 1).then(|| (p as *const u8).read())
}
unsafe fn shown(widget: usize) -> bool {
    word(widget, 0x1a8).is_some_and(|d| byte(d as usize, 0x40) == Some(1))
}
#[cfg(test)]
unsafe fn capture(hud: usize, f: &Frame, people: &[(u32, u64, Role)]) -> Option<Snapshot> {
    capture_with(hud, f, people, |_| false)
}
unsafe fn capture_with(
    hud: usize,
    f: &Frame,
    people: &[(u32, u64, Role)],
    mut fake_only: impl FnMut(usize) -> bool,
) -> Option<Snapshot> {
    if word(hud, 0)? != HUD || people.len() > 32 {
        return None;
    }
    let context = word(hud, 0x25c)? as usize;
    if word(context, 0x310)? as usize != f.local {
        return None;
    }
    let mut out = Snapshot {
        world: f.world,
        local: f.local,
        native_now_ms: f.now,
        targets: Vec::new(),
        enabled: false,
        interference: false,
    };
    // 1A738FD: native goggles/availability state. Old visible widgets do not
    // count after this gate closes, because the native loop was skipped.
    match byte(hud, 0x2ec)? {
        0 => return Some(out),
        1 => out.enabled = true,
        _ => return None,
    }
    let (p, n, cap) = (
        word(hud, 0x2ac)? as usize,
        word(hud, 0x2b0)? as usize,
        word(hud, 0x2b4)? as usize,
    );
    if n > 128 || cap < n || cap > 4096 || (n != 0 && !readable(p, n * 0x28)) {
        return None;
    }
    for i in 0..n {
        let e = p + i * 0x28;
        let pawn = word(e, 0x20)? as usize;
        // RFD also sees equipment actors. Only a live opposing Spy identity
        // from the validated roster can be a recipient of an exposure warning.
        if word(pawn, 0) != Some(0x2ad3234) || word(pawn, 0x144) != Some(f.world as u32) {
            continue;
        }
        let Some(object) = word(pawn, 0x20).filter(|v| *v != 0) else {
            continue;
        };
        let mut matches = people
            .iter()
            .filter(|&&(o, k, r)| o == object && k != 0 && r == Role::Spy);
        let Some(&(_, key, _)) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            return None;
        }
        // Native icon and wave have different range/projection gates. Neither
        // is recomputed here. Alpha/scale/random direction are not eligibility.
        let icon = word(e, 0)? as usize;
        let wave = word(e, 4)? as usize;
        if shown(icon) || shown(wave) {
            // Use the same precise-source priority as the pants renderer.
            // A fake skill wave warns its local RFD user, not the remote Spy.
            if fake_only(pawn) {
                out.interference = true;
            } else if !out.targets.contains(&key) {
                out.targets.push(key);
            }
        }
    }
    Some(out)
}
fn publish(next: Option<Snapshot>) {
    if let Ok(mut current) = CURRENT.lock() {
        *current = next;
    }
}
pub(crate) fn clear() {
    publish(None);
    crate::rfd_interference::publish(None);
}
/// Called only by the existing RFD Update owner, once original Update returns,
/// and BEFORE the presentation adapter changes any native widget visibility.
pub(crate) unsafe fn after_native_update(hud: usize) {
    if !READY.load(Ordering::Acquire) {
        clear();
        return;
    }
    let next = (|| {
        let f = ats_native_source::frame()?;
        let people = ats_native_source::detector_players(&f)?;
        capture_with(hud, &f, &people, |p| {
            crate::rfd_presentation::classify(p) == crate::rfd_rules::Display::Interference
        })
    })();
    if let Some(s) = &next {
        crate::goggle_feedback_native::observe(
            crate::goggle_exposure::Source::Rfd,
            s.world,
            s.local,
            s.native_now_ms,
            &s.targets,
            false,
        );
    }
    crate::rfd_interference::publish(next.as_ref().and_then(|s| {
        Some(crate::rfd_interference::Receipt {
            life: (s.world, s.local, word(s.local, 0x20)? as u64),
            now: s.native_now_ms,
            enabled: s.enabled,
            fake: s.interference,
        })
    }));
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
/// Verify before equipment_native installs its existing visibility bridge.
pub(crate) unsafe fn init(world: usize) {
    clear();
    let ok = world == 0x589860
        && signature(
            0x1a73860,
            0x1200,
            "3c7af93c59a49fc895f0efde1135bbd58fca3e9e5ab68d972058221af366d045",
        )
        && signature(
            0x1a52310,
            0x47,
            "82b05f2280f757c010bcd55a2f4492f2f4e902f298ae5b71ee69d1d8f4e22316",
        );
    READY.store(ok, Ordering::Release);
    if ok {
        tracing::info!("SCBL_MOD_GOGGLE_OBSERVER source=native_RFD_result observer_ready=true peer_delivery=false warning_enabled=false");
    }
}
pub(crate) fn deinit() {
    READY.store(false, Ordering::Release);
    clear();
}

#[cfg(test)]
#[path = "goggle_rfd_source_tests.rs"]
mod tests;
