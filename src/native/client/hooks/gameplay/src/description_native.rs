//! C31 native localization adapter: one original call, same native return buffer.
//! Does not own text pointers, patch assets or change selection/save state.
#[path = "drone_exit_label.rs"]
mod drone_exit_label;
use crate::equipment_native::{readable, signature, word, writable};
use crate::mod_descriptions::{self, FILE, SECTION};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const LOCALIZE: usize = 0x004b_39c0;
const RING: usize = 0x032c_1818;
const SLOTS: usize = 128;
const SLOT_BYTES: usize = 0x800;
const LANGUAGE: usize = 0x0337_ce38;
static READY: AtomicBool = AtomicBool::new(false);
static REWRITE_LOGS: AtomicU32 = AtomicU32::new(0);
type LocalizeFn = unsafe extern "C" fn(usize, usize, usize, usize, i32, usize) -> usize;
static_detour! {
    static Localize: unsafe extern "C" fn(usize, usize, usize, usize, i32, usize) -> usize;
}
#[derive(Clone, Copy)]
struct Args {
    section: usize,
    key: usize,
    file: usize,
    language: usize,
    optional: i32,
    resolver: usize,
}
unsafe fn ascii<const N: usize>(p: usize, wide: bool, out: &mut [u8; N]) -> Option<&str> {
    let step = if wide { 2 } else { 1 };
    for (i, slot) in out.iter_mut().enumerate() {
        let a = p.checked_add(i * step)?;
        if !readable(a, step) {
            return None;
        }
        let v = if wide {
            (a as *const u16).read_unaligned()
        } else {
            (a as *const u8).read() as u16
        };
        if v == 0 {
            return std::str::from_utf8(&out[..i]).ok();
        }
        if v > 127 {
            return None;
        }
        *slot = v as u8;
    }
    None
}
fn capacity(result: usize, base: usize) -> Option<usize> {
    let offset = result.checked_sub(base)?;
    if offset >= SLOTS * SLOT_BYTES || !matches!(offset % SLOT_BYTES, 0 | 2) {
        return None;
    }
    Some((SLOT_BYTES - offset % SLOT_BYTES) / 2)
}
unsafe fn rewrite(result: usize, base: usize, text: &str) -> bool {
    let Some(cap) = capacity(result, base) else {
        return false;
    };
    let len = text.encode_utf16().count();
    if len == 0 || len >= cap || text.contains('\0') || !writable(result, (len + 1) * 2) {
        return false;
    }
    // Same native ring slot/lifetime; no DLL-owned pointer escapes to the game.
    for (i, unit) in text.encode_utf16().chain([0]).enumerate() {
        (result as *mut u16).add(i).write_unaligned(unit);
    }
    true
}
unsafe fn localize_with(
    args: Args,
    enabled: bool,
    default_language: usize,
    normal_locale: bool,
    ring: usize,
    original: impl FnOnce(Args) -> usize,
) -> usize {
    // Preserve original lookup/fallback/locking/ring advance for every request.
    let result = original(args);
    if !enabled || !normal_locale || args.resolver != 0 || capacity(result, ring).is_none() {
        return result;
    }
    let (mut section, mut key, mut file, mut lang) = ([0u8; 64], [0u8; 96], [0u8; 64], [0u8; 8]);
    let Some(s) = ascii(args.section, true, &mut section) else {
        return result;
    };
    if s != SECTION {
        return result;
    }
    let Some(f) = ascii(args.file, false, &mut file) else {
        return result;
    };
    if f != FILE {
        return result;
    }
    let Some(k) = ascii(args.key, true, &mut key) else {
        return result;
    };
    let lp = if args.language == 0 {
        default_language
    } else {
        args.language
    };
    let Some(l) = ascii(lp, false, &mut lang) else {
        return result;
    };
    if let Some(text) = mod_descriptions::lookup(true, s, k, f, l) {
        // Missing native data stays missing (including optional _PC fallback).
        if readable(result, 2) && (result as *const u16).read_unaligned() != 0 {
            if rewrite(result, ring, text)
                && READY.load(Ordering::Acquire)
                && REWRITE_LOGS.fetch_add(1, Ordering::Relaxed) < 8
            {
                tracing::info!("SCBL_MOD_DESCRIPTIONS revision=C61 applied=true key={} locale={} same_native_ring=true", k, l);
            }
        }
    }
    result
}
fn normal_locale_flags(loaded: Option<u32>, ids: Option<u32>, annotated: Option<u32>) -> bool {
    loaded.is_some_and(|v| v != 0) && ids == Some(0) && annotated == Some(0)
}
fn localize(
    section: usize,
    key: usize,
    file: usize,
    language: usize,
    optional: i32,
    resolver: usize,
) -> usize {
    let args = Args {
        section,
        key,
        file,
        language,
        optional,
        resolver,
    };
    unsafe {
        // C61: 330C284 is set in ordinary retail flow (startup clears it for
        // debugflow); it also gates native missing-text fallback. It is NOT an
        // "IDs/annotated text" mode. Preserve the original function's handling,
        // but do not reject valid localized equipment descriptions because of it.
        let normal = normal_locale_flags(
            word(0x0330_c1c0, 0),
            word(0x0330_c1f0, 0),
            word(0x0330_c1f4, 0),
        );
        localize_with(
            args,
            READY.load(Ordering::Acquire),
            LANGUAGE,
            normal,
            RING,
            |a| Localize.call(a.section, a.key, a.file, a.language, a.optional, a.resolver),
        )
    }
}
pub(super) unsafe fn init(world_raycast: usize) -> bool {
    drone_exit_label::init(world_raycast);
    if READY.load(Ordering::Acquire) {
        return true;
    }
    if world_raycast != 0x0058_9860
        || !signature(
            LOCALIZE,
            1126,
            "3def92b51f841ede4b5d2069724f1f26ddecabf014b0675674d9bb383fa87e06",
        )
        || !signature(
            0x004c_9ae0,
            6,
            "f88b9383c65c750c0fd54d88faf3158f74c5fa8f7767485b7dc69e8007db5160",
        )
        || !signature(
            0x00cd_d340,
            358,
            "5098a4d9c99ae74fbab78b0332ce1b236ea9cfcb0f9a409a781848c5f1f3ade8",
        )
    {
        tracing::warn!("SCBL_MOD_DESCRIPTIONS enabled=false reason=unsupported-native-contract");
        return false;
    }
    if Localize
        .initialize(std::mem::transmute::<usize, LocalizeFn>(LOCALIZE), localize)
        .is_err()
        || Localize.enable().is_err()
    {
        let _ = Localize.disable();
        return false;
    }
    READY.store(true, Ordering::Release);
    tracing::info!(
        "SCBL_MOD_DESCRIPTIONS revision=C61 enabled=true scope=exact-mp-equipment locale=int,cht"
    );
    true
}
pub(super) unsafe fn deinit() {
    drone_exit_label::deinit();
    READY.store(false, Ordering::Release);
    let _ = Localize.disable();
}
#[cfg(test)]
#[path = "description_tests.rs"]
mod tests;
