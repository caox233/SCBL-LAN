//! Extend only the verified retail MP progression table; no player/save edits.
//! E906D0 and EFAB70 accumulate rows {rank,xp,tokens,flags,total_xp,total_tokens}.
//! Consumers E90700/50/80 and XP cap E90730 all use this table, not a byte rank.
use crate::equipment_native::{readable, word, writable};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

const RETAIL_LEVELS: usize = 56;
const MAX_LEVEL: usize = 999;
const PROGRESSION_VTABLE: u32 = 0x02a3_f604;
static READY: AtomicBool = AtomicBool::new(false);
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rank {
    level: i32,
    xp: i32,
    tokens: i32,
    flags: u32,
    total_xp: i32,
    total_tokens: i32,
}
#[repr(C)]
struct Array {
    data: *mut Rank,
    len: i32,
    cap: i32,
}
type InitFn = unsafe extern "thiscall" fn(usize);
type ResizeFn = unsafe extern "thiscall" fn(*mut Array, usize);
type FreeFn = unsafe extern "thiscall" fn(*mut Array);
static_detour! {
    static Init: unsafe extern "thiscall" fn(usize);
    static Reload: unsafe extern "thiscall" fn(usize);
}
// A config other than the retail 56-row curve is not silently replaced. In
// particular, reject sample/debug curves with duplicated ranks or bad totals.
fn extended(rows: &[Rank]) -> Result<Option<Vec<Rank>>, &'static str> {
    if rows.len() == MAX_LEVEL {
        let expected = extended(&rows[..RETAIL_LEVELS])?.ok_or("prefix")?;
        if rows == expected {
            return Ok(None);
        }
        // Reload may legitimately update row 56. Accept only our uniform tail
        // shape, then regenerate it from the new retail prefix.
        let tail = rows[RETAIL_LEVELS];
        if tail.xp <= 0
            || tail.tokens != 0
            || tail.flags & !1 != 0
            || rows[RETAIL_LEVELS..].iter().enumerate().any(|(i, row)| {
                row.level != (RETAIL_LEVELS + i + 1) as i32
                    || row.xp != tail.xp
                    || row.tokens != 0
                    || row.flags != tail.flags
            })
        {
            return Err("foreign-extension");
        }
        return Ok(Some(expected));
    }
    if rows.len() != RETAIL_LEVELS {
        return Err("row-count");
    }
    let mut xp = 0i32;
    let mut tokens = 0i32;
    for (i, row) in rows.iter().enumerate() {
        if row.level != (i + 1) as i32
            || row.xp < 0
            || row.tokens < 0
            || (i > 0 && row.xp == 0)
            || row.flags & !1 != 0
        {
            return Err("row-schema");
        }
        xp = xp.checked_add(row.xp).ok_or("xp-overflow")?;
        tokens = tokens.checked_add(row.tokens).ok_or("token-overflow")?;
        if row.total_xp != xp || row.total_tokens != tokens {
            return Err("native-totals");
        }
    }
    let last = rows[RETAIL_LEVELS - 1];
    // The user requested XP continuation, not a new currency/reward economy.
    if last.xp <= 0 || last.tokens != 0 {
        return Err("last-level-reward");
    }
    let final_xp = i64::from(xp) + (MAX_LEVEL - RETAIL_LEVELS) as i64 * i64::from(last.xp);
    // Native E90730 adds one to the cap.
    if final_xp >= i64::from(i32::MAX) {
        return Err("xp-overflow");
    }
    let mut result = Vec::with_capacity(MAX_LEVEL);
    result.extend_from_slice(rows);
    for level in RETAIL_LEVELS + 1..=MAX_LEVEL {
        xp += last.xp;
        result.push(Rank {
            level: level as i32,
            total_xp: xp,
            ..last
        });
    }
    Ok(Some(result))
}
unsafe fn project_with(
    object: usize,
    resize: impl FnOnce(*mut Array, usize),
    free: impl FnOnce(*mut Array),
) -> Result<bool, &'static str> {
    let array_address = object.checked_add(0x28).ok_or("object")?;
    if word(object, 0) != Some(PROGRESSION_VTABLE)
        || object % 4 != 0
        || !writable(array_address, std::mem::size_of::<Array>())
    {
        return Err("object");
    }
    let arr = array_address as *mut Array;
    let (data, len, cap) = ((*arr).data, (*arr).len, (*arr).cap);
    if len < 0
        || len > cap
        || cap > 4096
        || data as usize % 4 != 0
        || !matches!(len as usize, RETAIL_LEVELS | MAX_LEVEL)
        || !readable(data as usize, len as usize * std::mem::size_of::<Rank>())
    {
        return Err("array");
    }
    let Some(rows) = extended(std::slice::from_raw_parts(data, len as usize))? else {
        return Ok(false);
    };
    // Allocate through the SAME native allocator without reallocating or
    // exposing a partially initialized live array. The original data is only
    // released after an entirely validated replacement is ready.
    let mut replacement = Array {
        data: std::ptr::null_mut(),
        len: 0,
        cap: MAX_LEVEL as i32,
    };
    resize(&mut replacement, std::mem::size_of::<Rank>());
    if replacement.data.is_null() {
        return Err("allocation");
    }
    if !writable(
        replacement.data as usize,
        rows.len() * std::mem::size_of::<Rank>(),
    ) {
        free(&mut replacement);
        return Err("allocation-range");
    }
    std::ptr::copy_nonoverlapping(rows.as_ptr(), replacement.data, rows.len());
    replacement.len = MAX_LEVEL as i32;
    let mut old = std::ptr::replace(arr, replacement);
    old.len = 0;
    free(&mut old);
    Ok(true)
}
unsafe fn project(object: usize) {
    let resize: ResizeFn = std::mem::transmute(0x0042_dab0usize);
    let free: FreeFn = std::mem::transmute(0x0042_da10usize);
    match project_with(object, |a, s| resize(a, s), |a| free(a)) {
        Ok(true) => tracing::info!("SCBL_MOD_PROGRESSION revision=42 max_level=999 curve=repeat-retail-56 native_save_unchanged=true"),
        Ok(false) => {},
        Err(reason) => tracing::warn!("SCBL_MOD_PROGRESSION enabled=false reason={reason} native_curve_retained=true"),
    }
}
unsafe fn after_original(object: usize, enabled: bool, original: impl FnOnce(usize)) {
    original(object);
    if enabled {
        project(object);
    }
}
fn init_rows(object: usize) {
    unsafe {
        after_original(object, READY.load(Ordering::Acquire), |o| Init.call(o));
    }
}
fn reload_rows(object: usize) {
    unsafe {
        after_original(object, READY.load(Ordering::Acquire), |o| Reload.call(o));
    }
}
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        0x42dab0,
        251,
        "c9fad0206439eb35d9fe9e1fdb0a0c09f14765dd564d16d90dd03441ba028d70",
    ),
    (
        0x42da10,
        151,
        "b1c02502ac7fec07e7bc2c52c1cb872510d33ea0a9685cdf089bc592319d0af5",
    ),
    (
        0x00e906d0,
        48,
        "6fac52386460c463db5b3afba9a931b473a9619c9795cd83bb03c0c33279c69b",
    ),
    (
        0x00efab70,
        574,
        "f447ce09013a618b26c717038681f78e49f772a7bee1059e5f32a25e670d0002",
    ),
    (
        0x00e90700,
        172,
        "306e2db2f8778a68662d9bacbcecd1911f9cdf105a54c3907533b32b2f1bfb3c",
    ),
    (
        0x00c864e0,
        58,
        "0050d819179f710cfe2cc934b67ff12549ce2f62f1ac123e15d7d965c547b392",
    ),
];
pub(crate) fn init(world: usize) {
    if world != 0x00589860 {
        return;
    }
    unsafe {
        for &(a, n, h) in SIGNATURES {
            if !readable(a, n)
                || format!(
                    "{:x}",
                    Sha256::digest(std::slice::from_raw_parts(a as *const u8, n))
                ) != h
            {
                tracing::error!("SCBL_MOD_PROGRESSION enabled=false reason=native-signature");
                return;
            }
        }
        if Init
            .initialize(std::mem::transmute::<usize, InitFn>(0xe906d0), init_rows)
            .is_err()
            || Reload
                .initialize(std::mem::transmute::<usize, InitFn>(0xefab70), reload_rows)
                .is_err()
            || Init.enable().is_err()
            || Reload.enable().is_err()
        {
            deinit();
            tracing::error!("SCBL_MOD_PROGRESSION enabled=false reason=detour");
            return;
        }
        READY.store(true, Ordering::Release);
    }
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if Reload.is_enabled() {
        let _ = Reload.disable();
    }
    if Init.is_enabled() {
        let _ = Init.disable();
    }
    // The native UObject owns its native-allocated table and frees it normally.
    // No Rust pointer is retained and no loaded player XP is changed/clamped.
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "explicit x86 fixture executes hash-bound retail rank lookup code, not a game"]
    fn retail_machine_code_agrees_at_all_999_thresholds() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        let path = std::env::var("SCBL_PROGRESSION_ABI_FIXTURE").unwrap();
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(wide.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_entry@4\0".as_ptr());
            let call0 = GetProcAddress(dll, b"_invoke0@12\0".as_ptr());
            let call1 = GetProcAddress(dll, b"_invoke1@16\0".as_ptr());
            assert!(!get.is_null() && !call0.is_null() && !call1.is_null());
            let get: unsafe extern "system" fn(u32) -> usize = std::mem::transmute(get);
            let call0: unsafe extern "system" fn(usize, usize, *mut usize) -> u32 =
                std::mem::transmute(call0);
            let call1: unsafe extern "system" fn(usize, usize, i32, *mut usize) -> u32 =
                std::mem::transmute(call1);
            let retail = retail();
            for stage in 0..3 {
                let mut rows = if stage == 1 {
                    extended(&retail).unwrap().unwrap()
                } else {
                    retail.clone()
                };
                let mut object = [0u32; 16];
                object[10] = rows.as_mut_ptr() as u32;
                object[11] = rows.len() as u32;
                object[12] = rows.len() as u32;
                let obj = object.as_mut_ptr() as usize;
                let mut out = 0usize;
                assert_eq!(call0(get(0), obj, &mut out), 1); // native cumulative sum
                for (i, row) in rows.iter().enumerate() {
                    assert_eq!(call1(get(1), obj, row.total_xp, &mut out), 1);
                    assert_eq!(out, &rows[i] as *const Rank as usize);
                    assert_eq!(call1(get(3), obj, row.total_xp, &mut out), 1);
                    assert_eq!(
                        out,
                        if i + 1 < rows.len() {
                            &rows[i + 1] as *const Rank as usize
                        } else {
                            0
                        }
                    );
                    assert_eq!(call1(get(4), obj, row.level, &mut out), 1);
                    assert_eq!(out, row.total_xp as usize);
                    if i > 0 {
                        assert_eq!(call1(get(1), obj, row.total_xp - 1, &mut out), 1);
                        assert_eq!(out, &rows[i - 1] as *const Rank as usize);
                    }
                }
                assert_eq!(call0(get(2), obj, &mut out), 1);
                assert_eq!(out, rows.last().unwrap().total_xp as usize + 1);
                assert_eq!(call1(get(1), obj, i32::MAX, &mut out), 1);
                assert_eq!(
                    (out as *const Rank).read().level,
                    if stage == 1 { 999 } else { 56 }
                );
            }
        }
        println!("Retail x86 E906D0/E90700/E90730/E90750/E90780: BASELINE max56, MODIFIED max999/level56 XP, ROLLBACK max56; every threshold and next row agrees; ECX/ret0/ret4/ESP valid; player saves untouched.");
    }

    fn retail() -> Vec<Rank> {
        let mut xp = 0;
        (1..=56)
            .map(|level| {
                let delta = if level == 1 {
                    0
                } else if level == 56 {
                    20000
                } else {
                    1500
                };
                xp += delta;
                Rank {
                    level,
                    xp: delta,
                    tokens: 0,
                    flags: u32::from(level > 50),
                    total_xp: xp,
                    total_tokens: 0,
                }
            })
            .collect()
    }
    #[test]
    fn extension_preserves_all_56_rows_and_repeats_only_last_level_xp() {
        let source = retail();
        let result = extended(&source).unwrap().unwrap();
        assert_eq!(&result[..56], source.as_slice());
        assert_eq!(result.len(), 999);
        for (i, r) in result.iter().enumerate().skip(56) {
            assert_eq!(r.level, i as i32 + 1);
            assert_eq!(r.xp, source[55].xp);
            assert_eq!(
                r.total_xp - source[55].total_xp,
                (i as i32 - 55) * source[55].xp
            );
            assert_eq!(r.tokens, 0);
            assert_eq!(r.total_tokens, source[55].total_tokens);
            assert_eq!(r.flags, source[55].flags);
        }
        assert_eq!(extended(&result), Ok(None));
    }
    #[test]
    fn rejects_foreign_rows_malformed_totals_rewards_and_overflow() {
        let src = retail();
        for variant in 0..7 {
            let mut a = src.clone();
            match variant {
                0 => {
                    a.pop();
                }
                1 => a[0].level = 0,
                2 => a[15].level = 15,
                3 => a[55].xp = -1,
                4 => a[55].total_xp += 1,
                5 => {
                    a[55].tokens = 1;
                    a[55].total_tokens = 1;
                }
                _ => {
                    a[55].xp = i32::MAX - a[54].total_xp;
                    a[55].total_xp = i32::MAX;
                }
            }
            assert!(extended(&a).is_err(), "{variant}");
        }
        let mut full = extended(&src).unwrap().unwrap();
        full[57].xp += 1;
        assert_eq!(extended(&full), Err("foreign-extension"));
    }
    #[test]
    fn thresholds_are_exact_at_56_57_998_and_999_without_xp_rewrite() {
        let ranks = extended(&retail()).unwrap().unwrap();
        let level = |xp: i32| ranks.iter().rev().find(|r| r.total_xp <= xp).unwrap().level;
        for n in [56usize, 57, 998, 999] {
            let xp = ranks[n - 1].total_xp;
            assert_eq!(level(xp), n as i32);
            assert_eq!(level(xp - 1), n as i32 - 1);
            assert_eq!(level(xp + 1), n as i32);
        }
        assert_eq!(level(i32::MAX), 999);
    }
    #[test]
    fn native_array_commit_uses_native_ownership_and_no_player_fields() {
        let source = retail();
        let mut object = [0u32; 16];
        object[0] = PROGRESSION_VTABLE;
        let mut old = source.clone();
        let a = Array {
            data: old.as_mut_ptr(),
            len: 56,
            cap: 56,
        };
        unsafe {
            let pointer = object.as_mut_ptr() as usize;
            std::ptr::write((pointer + 0x28) as *mut Array, a);
            let mut allocated = vec![source[0]; 999];
            let newptr = allocated.as_mut_ptr();
            let oldptr = old.as_mut_ptr();
            let freed = std::cell::Cell::new(false);
            assert_eq!(
                project_with(
                    pointer,
                    |a, size| {
                        assert_eq!(size, 24);
                        assert_eq!((*a).cap, 999);
                        assert!((*a).data.is_null());
                        (*a).data = newptr;
                    },
                    |a| {
                        assert_eq!((*a).data, oldptr);
                        assert_eq!((*a).len, 0);
                        freed.set(true);
                    }
                ),
                Ok(true)
            );
            assert!(freed.get());
            assert_eq!(object[11], 999);
            assert_eq!(old, source);
            assert_eq!(allocated[998].level, 999);
            assert_eq!(
                project_with(
                    pointer,
                    |_, _| panic!("must not allocate"),
                    |_| panic!("must not free")
                ),
                Ok(false)
            );
        }
    }
    #[test]
    fn config_reload_regenerates_only_extension_after_native_recalculation() {
        let mut full = extended(&retail()).unwrap().unwrap();
        full[55].xp += 1000;
        let mut total = 0;
        for row in &mut full {
            total += row.xp;
            row.total_xp = total;
        }
        let expected_prefix = full[..56].to_vec();
        let updated = extended(&full).unwrap().unwrap();
        assert_eq!(updated[..56], expected_prefix);
        assert_eq!(updated[56].xp, updated[55].xp);
        assert_eq!(updated[998].xp, updated[55].xp);
    }
    #[test]
    fn allocation_failure_and_foreign_object_leave_original_bytes() {
        let mut source = retail();
        let mut object = [0u32; 16];
        object[0] = PROGRESSION_VTABLE;
        unsafe {
            let ptr = object.as_mut_ptr() as usize;
            std::ptr::write(
                (ptr + 0x28) as *mut Array,
                Array {
                    data: source.as_mut_ptr(),
                    len: 56,
                    cap: 56,
                },
            );
            let before = object;
            assert_eq!(
                project_with(ptr, |_, _| {}, |_| panic!("no allocation")),
                Err("allocation")
            );
            assert_eq!(object, before);
            (ptr as *mut u32).write(0);
            assert_eq!(
                project_with(
                    ptr,
                    |_, _| panic!("wrong object"),
                    |_| panic!("wrong object")
                ),
                Err("object")
            );
        }
    }
}
