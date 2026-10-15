//! Rank labels and badge resource bounds for the extended MP progression.
//! Only seven proven rank-image formatting CALL sites, never global printf.
use crate::equipment_native::{readable, writable};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

const RANK_FORMAT: usize = 0x02c2_84cc;
const FORMATTER: usize = 0x0044_bc50;
const LABEL: usize = 0x00ed_3680;
const WIDE_CONSTRUCT: usize = 0x0044_ef80;
static READY: AtomicBool = AtomicBool::new(false);
type CallPatch = (usize, [u8; 5], [u8; 5]);
static PATCHES: Mutex<Vec<CallPatch>> = Mutex::new(Vec::new());
type LabelFn = unsafe extern "thiscall" fn(usize, usize, i32) -> usize;
type WideFn = unsafe extern "thiscall" fn(usize, *const u16) -> usize;
type FormatFn = unsafe extern "C" fn(usize, i32, usize) -> usize;
static_detour! { static Label: unsafe extern "thiscall" fn(usize, usize, i32) -> usize; }
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(h: *mut c_void, p: *const c_void, n: usize) -> i32;
}
fn badge_index(format: usize, index: i32, enabled: bool) -> i32 {
    if enabled && format == RANK_FORMAT && (56..=998).contains(&index) {
        55
    } else {
        index
    }
}
fn decimal_label(level: i32, enabled: bool) -> Option<Vec<u16>> {
    (enabled && (57..=999).contains(&level))
        .then(|| level.to_string().encode_utf16().chain(Some(0)).collect())
}
unsafe fn label_with(
    object: usize,
    out: usize,
    level: i32,
    enabled: bool,
    original: impl FnOnce(usize, usize, i32) -> usize,
    construct: impl FnOnce(usize, *const u16) -> usize,
) -> usize {
    if out % 4 == 0 && writable(out, 12) {
        if let Some(text) = decimal_label(level, enabled) {
            // Original ED3680 constructs the output using 44EF80; keep that
            // allocator, ownership and return-value contract, not a Rust string.
            return construct(out, text.as_ptr());
        }
    }
    original(object, out, level)
}
fn label(object: usize, out: usize, level: i32) -> usize {
    unsafe {
        let make: WideFn = std::mem::transmute(WIDE_CONSTRUCT);
        label_with(
            object,
            out,
            level,
            READY.load(Ordering::Acquire),
            |o, p, l| Label.call(o, p, l),
            |p, s| make(p, s),
        )
    }
}
unsafe extern "C" fn format_rank(format: usize, index: i32, suffix: usize) -> usize {
    let original: FormatFn = std::mem::transmute(FORMATTER);
    original(
        format,
        badge_index(format, index, READY.load(Ordering::Acquire)),
        suffix,
    )
}
fn call_bytes(address: usize, target: usize) -> [u8; 5] {
    let mut bytes = [0xe8, 0, 0, 0, 0];
    bytes[1..].copy_from_slice(
        &(target as u32)
            .wrapping_sub((address as u32).wrapping_add(5))
            .to_le_bytes(),
    );
    bytes
}
unsafe fn write_owned(address: usize, expected: &[u8; 5], replacement: &[u8; 5]) -> bool {
    if !readable(address, 5) || std::slice::from_raw_parts(address as *const u8, 5) != expected {
        return false;
    }
    let Ok(protect) = region::protect_with_handle(
        address as *mut u8,
        5,
        region::Protection::READ_WRITE_EXECUTE,
    ) else {
        return false;
    };
    std::ptr::copy_nonoverlapping(replacement.as_ptr(), address as *mut u8, 5);
    let matches = std::slice::from_raw_parts(address as *const u8, 5) == replacement;
    drop(protect);
    // The caller journals before writing. Do not blindly overwrite bytes on a
    // failed post-check; the journal restores only a still-owned replacement.
    matches && FlushInstructionCache(GetCurrentProcess(), address as *const c_void, 5) != 0
}
const SITES: &[usize] = &[
    0x1942a03, 0x194a5c4, 0x1969e83, 0x1969fa5, 0x196a011, 0x196b060, 0x1986cef,
];
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        LABEL,
        198,
        "50f2f25671c3ea418c0ef0b2dd4b9e5579f071b5c4a8891b92be3e2d98813677",
    ),
    (
        WIDE_CONSTRUCT,
        202,
        "c9c357c9a0e1147b07928f358b5969a02d5266e4b3772ace4ba97cc79b653a69",
    ),
    (
        FORMATTER,
        128,
        "05cf454992da0230689ac6e823bba55e968c3928e92b44fb9ef65d7faefd839b",
    ),
];
unsafe fn restore_calls_with(
    patches: &mut Vec<CallPatch>,
    mut write: impl FnMut(usize, &[u8; 5], &[u8; 5]) -> bool,
    mut flush: impl FnMut(usize) -> bool,
) -> bool {
    for index in (0..patches.len()).rev() {
        let (address, old, new) = patches[index];
        if !readable(address, 5) {
            continue;
        }
        let current = std::slice::from_raw_parts(address as *const u8, 5);
        let restored = if current == old {
            flush(address)
        } else if current == new {
            write(address, &new, &old)
                && std::slice::from_raw_parts(address as *const u8, 5) == old
        } else {
            false
        };
        if restored {
            patches.remove(index);
        }
    }
    patches.is_empty()
}
unsafe fn restore_patches() -> bool {
    let Ok(mut patches) = PATCHES.lock() else {
        tracing::error!("SCBL_MOD_RANK_PRESENTATION restore=false reason=patch-state-poisoned");
        return false;
    };
    let restored = restore_calls_with(
        &mut patches,
        |a, expected, replacement| write_owned(a, expected, replacement),
        |a| FlushInstructionCache(GetCurrentProcess(), a as *const c_void, 5) != 0,
    );
    if !restored {
        tracing::error!("SCBL_MOD_RANK_PRESENTATION restore=false pending={} foreign_bytes_preserved=true", patches.len());
    }
    restored
}
pub(crate) fn init(world: usize) -> bool {
    if world != 0x00589860 {
        return false;
    }
    if READY.load(Ordering::Acquire) {
        return true;
    }
    unsafe {
        // Do not initialize over an active hook or discard outstanding undo.
        if Label.is_enabled() || !restore_patches() {
            return false;
        }
        for &(a, n, h) in SIGNATURES {
            if !readable(a, n)
                || format!(
                    "{:x}",
                    Sha256::digest(std::slice::from_raw_parts(a as *const u8, n))
                ) != h
            {
                tracing::error!("SCBL_MOD_RANK_PRESENTATION enabled=false reason=native-signature");
                return false;
            }
        }
        for &a in SITES {
            if !readable(a - 5, 10)
                || std::slice::from_raw_parts((a - 5) as *const u8, 5)
                    != [0x68, 0xcc, 0x84, 0xc2, 0x02]
                || std::slice::from_raw_parts(a as *const u8, 5) != call_bytes(a, FORMATTER)
            {
                tracing::error!(
                    "SCBL_MOD_RANK_PRESENTATION enabled=false reason=callsite-signature"
                );
                return false;
            }
        }
        if Label
            .initialize(std::mem::transmute::<usize, LabelFn>(LABEL), label)
            .is_err()
            || Label.enable().is_err()
        {
            deinit();
            return false;
        }
        let Ok(mut patches) = PATCHES.lock() else {
            let _ = Label.disable();
            return false;
        };
        for &a in SITES {
            let old = call_bytes(a, FORMATTER);
            let new = call_bytes(a, format_rank as usize);
            // Enabling the label hook is outside this transaction. Recheck
            // ownership before recording this site, and record before writing.
            if !readable(a, 5) || std::slice::from_raw_parts(a as *const u8, 5) != old {
                drop(patches);
                deinit();
                return false;
            }
            patches.push((a, old, new));
            if !write_owned(a, &old, &new) {
                drop(patches);
                deinit();
                return false;
            }
        }
        READY.store(true, Ordering::Release);
    }
    tracing::info!("SCBL_MOD_RANK_PRESENTATION revision=43 label=actual-number badge=retail-56-for-extended only_rank_calls=true");
    true
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    restore_patches();
    if Label.is_enabled() {
        let _ = Label.disable();
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "MSVC x86 isolated rank format call-site patch and cdecl varargs ABI fixture"]
    fn native_rank_callsite_and_rollback_preserve_arguments_and_stack() {
        use std::sync::atomic::AtomicUsize;
        static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn wrapper(fmt: usize, index: i32, suffix: usize) -> usize {
            let original: FormatFn = std::mem::transmute(ORIGINAL.load(Ordering::Relaxed));
            original(fmt, badge_index(fmt, index, true), suffix)
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
            fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, prot: u32) -> *mut c_void;
            fn VirtualFree(p: *mut c_void, n: usize, t: u32) -> i32;
        }
        let path = std::env::var("SCBL_RANK_ABI_FIXTURE").unwrap();
        let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(w.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_original@0\0".as_ptr());
            let invoke = GetProcAddress(dll, b"_invoke@20\0".as_ptr());
            let count = GetProcAddress(dll, b"_call_count@0\0".as_ptr());
            assert!(!get.is_null() && !invoke.is_null() && !count.is_null());
            let get: unsafe extern "system" fn() -> usize = std::mem::transmute(get);
            let invoke: unsafe extern "system" fn(usize, usize, i32, usize, *mut usize) -> u32 =
                std::mem::transmute(invoke);
            let count: unsafe extern "system" fn() -> u32 = std::mem::transmute(count);
            let original = get();
            ORIGINAL.store(original, Ordering::Relaxed);
            let mem = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40) as usize;
            assert!(mem != 0);
            let mut code = vec![
                0x55, 0x8b, 0xec, 0xff, 0x75, 0x10, 0xff, 0x75, 0x0c, 0xff, 0x75, 0x08, 0xe8, 0, 0,
                0, 0, 0x83, 0xc4, 0x0c, 0x5d, 0xc3,
            ];
            let old = call_bytes(mem + 12, original);
            let new = call_bytes(mem + 12, wrapper as usize);
            code[12..17].copy_from_slice(&old);
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem as *mut u8, code.len());
            FlushInstructionCache(GetCurrentProcess(), mem as *const c_void, code.len());
            let before = count();
            for stage in 0..3 {
                if stage == 1 {
                    assert!(write_owned(mem + 12, &old, &new));
                }
                if stage == 2 {
                    assert!(write_owned(mem + 12, &new, &old));
                }
                for n in 0..1000 {
                    let index = if n % 2 == 0 { 998 } else { 55 };
                    let mut out = 0usize;
                    assert_eq!(invoke(mem, RANK_FORMAT, index, 0x12345678, &mut out), 1);
                    assert_eq!(out, if stage == 1 { 55 } else { index as usize });
                    assert_eq!(
                        invoke(original, RANK_FORMAT, index, 0x12345678, &mut out),
                        1
                    );
                    assert_eq!(out, index as usize); // global formatter untouched
                }
            }
            assert_eq!(count() - before, 6000);
            assert_eq!(
                std::slice::from_raw_parts(mem as *const u8, code.len()),
                code
            );
            let foreign = [0x90u8; 5];
            assert!(!write_owned(mem + 12, &foreign, &new));
            assert_eq!(std::slice::from_raw_parts((mem + 12) as *const u8, 5), old);
            assert_eq!(VirtualFree(mem as *mut c_void, 0, 0x8000), 1);
        }
        println!("Native cdecl varargs: baseline index998, modified rank-only index55, rollback998; global formatter unchanged; 6000 exact-once calls; format/suffix/return/ESP preserved; foreign bytes not overwritten. Real UI untested.");
    }

    #[test]
    fn badges_clamp_only_existing_extended_tier_without_changing_numeric_level() {
        for i in [-2, -1, 0, 1, 54, 55, 56, 57, 997, 998, 999, i32::MAX] {
            assert_eq!(
                badge_index(RANK_FORMAT, i, true),
                if (56..=998).contains(&i) { 55 } else { i }
            );
            assert_eq!(badge_index(RANK_FORMAT, i, false), i);
            assert_eq!(badge_index(RANK_FORMAT + 1, i, true), i);
        }
        assert_eq!(decimal_label(999, true), Some(vec![57, 57, 57, 0]));
    }
    #[test]
    fn native_levels_and_unknowns_keep_original_localization() {
        for n in [i32::MIN, -1, 0, 1, 55, 56, 1000, i32::MAX] {
            assert_eq!(decimal_label(n, true), None);
        }
        for n in [57, 100, 998, 999] {
            assert_eq!(decimal_label(n, false), None);
            assert_eq!(
                String::from_utf16(&decimal_label(n, true).unwrap()[..n.to_string().len()])
                    .unwrap(),
                n.to_string()
            );
        }
    }
    #[test]
    fn label_uses_native_constructor_only_for_extended_valid_output() {
        let mut out = [0u32; 3];
        let ptr = out.as_mut_ptr() as usize;
        unsafe {
            assert_eq!(
                label_with(
                    10,
                    ptr,
                    999,
                    true,
                    |_, _, _| panic!("no missing localization"),
                    |p, s| {
                        assert_eq!(p, ptr);
                        assert_eq!(std::slice::from_raw_parts(s, 4), [57, 57, 57, 0]);
                        p
                    }
                ),
                ptr
            );
            for (p, level, enabled) in [
                (0, 999, true),
                (ptr, 56, true),
                (ptr, 999, false),
                (ptr, 1000, true),
            ] {
                assert_eq!(
                    label_with(
                        10,
                        p,
                        level,
                        enabled,
                        |o, out, l| {
                            assert_eq!((o, out, l), (10, p, level));
                            0x1234
                        },
                        |_, _| panic!("must use original")
                    ),
                    0x1234
                );
            }
        }
    }
    #[test]
    fn relative_calls_wrap_exactly_as_x86_without_global_formatter_patch() {
        for a in [0x1942a03usize, 0x7ffff000, 0xfffffff0] {
            for t in [FORMATTER, 0x90000000, 0x1000] {
                let b = call_bytes(a, t);
                let offset = u32::from_le_bytes(b[1..].try_into().unwrap());
                assert_eq!((a as u32).wrapping_add(5).wrapping_add(offset), t as u32);
                assert_ne!(a, FORMATTER);
            }
        }
        assert_eq!(SITES.len(), 7);
    }
}

#[cfg(test)]
#[path = "rank_transaction_tests.rs"]
mod transaction_tests;
