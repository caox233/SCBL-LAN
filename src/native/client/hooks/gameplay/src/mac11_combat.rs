//! MAC11 native archetype classification before either inventory-add path.
//! CD6910 resolves the requested MP archetype; E550B0 creates the weapon from it.
//! DB58A0 and DB5600 both read DD51F0 before slot eviction. DB5600 alone is too late.
//! Preserve gun identity, model, family, attachments, native return and all flags.
use super::RETAIL_NAME;
use crate::equipment_native::{readable, word, writable};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const ARCHETYPE_VT: u32 = 0x02a176c4;
static READY: AtomicBool = AtomicBool::new(false);
static RESOLVE_LOGS: AtomicU32 = AtomicU32::new(0);
static ADD_LOGS: AtomicU32 = AtomicU32::new(0);
type ResolveFn = unsafe extern "thiscall" fn(usize, u32, u8, u8) -> usize;
type AddFn = unsafe extern "thiscall" fn(usize, usize, u32) -> u8;
static_detour! {
    static Resolve: unsafe extern "thiscall" fn(usize, u32, u8, u8) -> usize;
    static Add: unsafe extern "thiscall" fn(usize, usize, u32) -> u8;
}
unsafe fn exact_name(id: u32) -> bool {
    super::native_name_is(id, RETAIL_NAME)
}
unsafe fn classify_resolved(props: usize, exact_requested_name: bool) -> Option<u8> {
    if !exact_requested_name || word(props, 0) != Some(ARCHETYPE_VT) || !writable(props + 0x3b, 1) {
        return None;
    }
    let old = *((props + 0x3b) as *const u8);
    if !matches!(old, 1 | 2) {
        return None;
    }
    // Bind identity to the native lookup's requested FName, not a pawn role
    // that may not yet be initialized, or a transient UObject instance name.
    *((props + 0x3b) as *mut u8) = 2;
    Some(old)
}
unsafe fn resolve_with(
    config: usize,
    name: u32,
    a: u8,
    b: u8,
    enabled: bool,
    name_matches: impl FnOnce(u32) -> bool,
    original: impl FnOnce(usize, u32, u8, u8) -> usize,
) -> usize {
    let props = original(config, name, a, b);
    if enabled {
        let exact = name_matches(name);
        let changed = classify_resolved(props, exact);
        if exact && RESOLVE_LOGS.fetch_add(1, Ordering::Relaxed) < 24 {
            tracing::info!("SCBL_MOD_MAC11_COMBAT revision=39 stage=resolve requested=retail-mac11 native_returned={} native_type={} before={:?} secondary_classified={}",
                props!=0,word(props,0)==Some(ARCHETYPE_VT),changed,changed.is_some());
        }
    }
    props
}
fn resolve(config: usize, name: u32, a: u8, b: u8) -> usize {
    unsafe {
        resolve_with(
            config,
            name,
            a,
            b,
            READY.load(Ordering::Acquire),
            |id| exact_name(id),
            |c, n, a, b| Resolve.call(c, n, a, b),
        )
    }
}
fn add(pawn: usize, weapon: usize, equip: u32) -> u8 {
    unsafe {
        // Observation only: no second classification or slot mutation here.
        let result = Add.call(pawn, weapon, equip);
        if READY.load(Ordering::Acquire) && ADD_LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
            let props = word(weapon, 0x744).unwrap_or(0) as usize;
            let inventory = word(pawn, 0x1410).unwrap_or(0) as usize;
            let id = word(weapon, 0x740).unwrap_or(0);
            tracing::info!("SCBL_MOD_MAC11_COMBAT revision=39 stage=add result={} requested_name_visible={} props_type={} secondary_contains_weapon={} primary_present={}",
                result,id!=0&&exact_name(id),word(props,0)==Some(ARCHETYPE_VT),
                word(inventory,0x54)==Some(weapon as u32),word(inventory,0x50).is_some_and(|v|v!=0));
        }
        result
    }
}
pub(super) unsafe fn init() -> bool {
    for &(a, n, h) in SIGNATURES {
        if !readable(a, n)
            || format!(
                "{:x}",
                Sha256::digest(std::slice::from_raw_parts(a as *const u8, n))
            ) != h
        {
            tracing::error!("SCBL_MOD_MAC11_COMBAT enabled=false reason=native-signature");
            return false;
        }
    }
    if Resolve
        .initialize(std::mem::transmute::<usize, ResolveFn>(0xcd6910), resolve)
        .is_err()
        || Add
            .initialize(std::mem::transmute::<usize, AddFn>(0xdb5600), add)
            .is_err()
        || Add.enable().is_err()
        || Resolve.enable().is_err()
    {
        deinit();
        return false;
    }
    READY.store(true, Ordering::Release);
    true
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if Resolve.is_enabled() {
        let _ = Resolve.disable();
    }
    if Add.is_enabled() {
        let _ = Add.disable();
    }
    // Engine-owned cached archetypes must not be reclassified after insertion.
    // No native pointers retained, no mid-game unload.
}
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        0xcd6910,
        819,
        "c47230ef5e57c4679465021207a9c607160dda5d4061f3f37f48cf2254ebc63b",
    ),
    (
        0xdb5600,
        494,
        "82ffa176937704ff375d8a501d1a0008c977d0d7094276e80f5ff2f6f4cefa43",
    ),
    (
        0xd702b0,
        122,
        "8875d4bfe557b515906a443dad6d3c53e44cff5640245f740f5ce5419b840aba",
    ),
    (
        0xdd51f0,
        14,
        "18e899cb8a4c96462f6fc78b780097783207c21f56b32d05315dda396ccfb384",
    ),
];
#[cfg(test)]
mod tests {
    use super::*;
    fn props(kind: u8) -> Vec<u32> {
        let mut p = vec![0x55555555; 0x200 / 4];
        p[0] = ARCHETYPE_VT;
        unsafe {
            *((p.as_mut_ptr() as usize + 0x3b) as *mut u8) = kind;
        }
        p
    }
    #[test]
    fn requested_identity_classifies_before_any_pawn_or_slot_exists() {
        let mut p = props(1);
        let before = p.clone();
        let ptr = p.as_mut_ptr() as usize;
        let calls = std::cell::Cell::new(0);
        for _ in 0..3 {
            assert_eq!(
                unsafe {
                    resolve_with(
                        7,
                        123,
                        1,
                        0,
                        true,
                        |n| n == 123,
                        |c, n, a, b| {
                            assert_eq!((c, n, a, b), (7, 123, 1, 0));
                            calls.set(calls.get() + 1);
                            ptr
                        },
                    )
                },
                ptr
            );
        }
        assert_eq!(calls.get(), 3);
        let mut wanted = before;
        unsafe {
            *((wanted.as_mut_ptr() as usize + 0x3b) as *mut u8) = 2;
        }
        assert_eq!(p, wanted); // model, family, ID, damage and ammo untouched
    }
    #[test]
    fn unknown_identity_type_and_native_failure_are_not_fabricated() {
        for (enabled, exact, vt, kind) in [
            (false, true, ARCHETYPE_VT, 1),
            (true, false, ARCHETYPE_VT, 1),
            (true, true, 0, 1),
            (true, true, ARCHETYPE_VT, 3),
        ] {
            let mut p = props(kind);
            p[0] = vt;
            let ptr = p.as_mut_ptr() as usize;
            let before = p.clone();
            assert_eq!(
                unsafe { resolve_with(0, 2, 0, 1, enabled, |_| exact, |_, _, _, _| ptr) },
                ptr
            );
            assert_eq!(p, before);
        }
        assert_eq!(
            unsafe { resolve_with(0, 2, 0, 0, true, |_| true, |_, _, _, _| 0) },
            0
        );
    }

    #[test]
    #[ignore = "explicit MSVC x86 resolver ret12 ABI fixture"]
    fn native_resolver_preserves_thiscall_abi_and_classifies_before_consumers() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        fn fixture_resolve(c: usize, n: u32, a: u8, b: u8) -> usize {
            unsafe {
                resolve_with(
                    c,
                    n,
                    a,
                    b,
                    true,
                    |id| id == 123,
                    |c, n, a, b| Resolve.call(c, n, a, b),
                )
            }
        }
        let path = std::env::var("SCBL_MAC11_RESOLVE_ABI_FIXTURE").unwrap();
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(path.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_resolve@0\0".as_ptr());
            let call = GetProcAddress(dll, b"_invoke_resolve@24\0".as_ptr());
            assert!(!get.is_null() && !call.is_null());
            let get: unsafe extern "system" fn() -> usize = std::mem::transmute(get);
            let call: unsafe extern "system" fn(usize, usize, u32, u32, u32, *mut usize) -> u32 =
                std::mem::transmute(call);
            let entry = get();
            let mut p = props(1);
            // config: return pointer, observed name, a, b, call count
            let mut config = [p.as_mut_ptr() as usize, 0, 0, 0, 0];
            let mut returned = 0;
            for stage in 0..3 {
                if stage == 1 {
                    Resolve
                        .initialize(
                            std::mem::transmute::<usize, ResolveFn>(entry),
                            fixture_resolve,
                        )
                        .unwrap();
                    Resolve.enable().unwrap();
                }
                if stage == 2 {
                    Resolve.disable().unwrap();
                }
                for i in 0..1000 {
                    *((p.as_mut_ptr() as usize + 0x3b) as *mut u8) = 1;
                    let name = if i % 2 == 0 { 123 } else { 456 };
                    let a = (i & 255) as u32;
                    let b = ((i + 17) & 255) as u32;
                    assert_eq!(
                        call(
                            entry,
                            config.as_mut_ptr() as usize,
                            name,
                            a,
                            b,
                            &mut returned
                        ),
                        1
                    );
                    assert_eq!(returned, p.as_mut_ptr() as usize);
                    assert_eq!(&config[1..4], &[name as usize, a as usize, b as usize]);
                    assert_eq!(config[4], (stage * 1000 + i + 1) as usize);
                    assert_eq!(
                        *((returned + 0x3b) as *const u8),
                        if stage == 1 && name == 123 { 2 } else { 1 }
                    );
                }
            }
        }
        println!("MSVC x86 resolver ret12: BASELINE kind1; MODIFIED exact requested identity kind2 before consumers; ROLLBACK kind1; 3000 calls ECX/args/flags/pointer/ESP/original-once preserved. Native ABI model, not game.");
    }

    #[test]
    fn both_native_slot_consumers_see_secondary_before_evicting_primary() {
        let mut p = props(1);
        let ptr = p.as_mut_ptr() as usize;
        let mut inventory = [0, 11, 0];
        unsafe {
            resolve_with(0, 123, 0, 0, true, |_| true, |_, _, _, _| ptr);
        }
        // Both DB58A0 and DB5600 call DD51F0 first. Native reads +3b.
        let kind = unsafe { *((ptr + 0x3b) as *const u8) } as usize;
        assert_eq!(kind, 2);
        inventory[kind] = 22;
        assert_eq!(inventory, [0, 11, 22]); // original primary survives
    }
}
