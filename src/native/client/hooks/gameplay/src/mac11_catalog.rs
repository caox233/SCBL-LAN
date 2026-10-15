//! DX11 MOD: move the existing Spy MAC11 to Pistol; omit only Optic.
//! CF2470 owns registration, CEBA70 owns attachment allocation. No copies,
//! custom IDs, model changes, unlock changes or on-disk game/save mutations.
use crate::equipment_native::{readable, word, writable};
use crate::mac11_data::{legacy_id, RETAIL_ID, RETAIL_NAME};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "mac11_combat.rs"]
mod combat;

#[path = "mac11_preview.rs"]
mod preview;

const STANDARD: u32 = 0x02ae2570;
static READY: AtomicBool = AtomicBool::new(false);
type Factory = unsafe extern "thiscall" fn(usize, usize) -> u8;
type AttachmentFactory = unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
type Find = unsafe extern "thiscall" fn(usize, u32) -> usize;
static_detour! {
    static Register: unsafe extern "thiscall" fn(usize, usize) -> u8;
    static Attachment: unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
    static Lookup: unsafe extern "thiscall" fn(usize, u32) -> usize;
}
unsafe fn cbytes<'a>(p: usize, max: usize) -> Option<&'a [u8]> {
    if p == 0 {
        return None;
    }
    for n in 0..max {
        if !readable(p.checked_add(n)?, 1) {
            return None;
        }
        if *((p + n) as *const u8) == 0 {
            return Some(std::slice::from_raw_parts(p as *const u8, n));
        }
    }
    None
}
// Both combat archetype and preview bone checks require an unsuffixed FName.
// Keep the native bit18 flag, but do not treat numbered names as the same asset.
unsafe fn native_name_is_at(id: u32, expected: &[u8], root: usize) -> bool {
    if id >> 19 != 0 {
        return false;
    }
    let Some(table) = word(root, 0) else {
        return false;
    };
    let Some(entry) = word(table as usize, ((id & 0x3ffff) as usize) * 4) else {
        return false;
    };
    let Some(bytes) = (entry as usize).checked_add(8) else {
        return false;
    };
    cbytes(bytes, 64) == Some(expected)
}
unsafe fn native_name_is(id: u32, expected: &[u8]) -> bool {
    native_name_is_at(id, expected, 0x0333cd60)
}
unsafe fn name(p: usize) -> Option<&'static [u8]> {
    let data = word(p, 0x18)? as usize;
    let n = word(p, 0x1c)? as usize;
    let cap = word(p, 0x20)? as usize;
    if !(1..=128).contains(&n) || cap < n || cap > 1024 {
        return None;
    }
    cbytes(data, n)
}
unsafe fn retail_weapon(p: usize) -> bool {
    word(p, 0x10) == Some(RETAIL_ID)
        && word(p, 0x3c) == Some(1)
        && matches!(word(p, 0x40), Some(0 | 1))
        && name(p) == Some(RETAIL_NAME)
}
unsafe fn vector(p: usize, o: usize) -> Option<Vec<usize>> {
    let data = word(p, o)? as usize;
    let n = word(p, o + 4)? as usize;
    let cap = word(p, o + 8)? as usize;
    if n > cap || cap > 8192 || n > 4096 {
        return None;
    }
    if n == 0 {
        return Some(Vec::new());
    }
    if !readable(data, n * 4) {
        return None;
    }
    Some(
        (0..n)
            .map(|i| ((data + i * 4) as *const u32).read_unaligned() as usize)
            .collect(),
    )
}
unsafe fn move_existing(catalog: usize) -> bool {
    if word(catalog, 0) != Some(STANDARD) {
        return false;
    }
    let Some(items) = vector(catalog, 0x34) else {
        return false;
    };
    let candidates: Vec<_> = items.into_iter().filter(|&p| retail_weapon(p)).collect();
    if candidates.len() != 1 {
        return false;
    }
    let p = candidates[0];
    if !writable(p + 0x40, 4) {
        return false;
    }
    // CF2470 +273B stores SlotType here. Pistol=0, TwoHander=1.
    // Same object/NetID/archetype/children/stats/purchase progress throughout.
    ((p + 0x40) as *mut u32).write_unaligned(0);
    true
}
unsafe fn xml_attr(xml: usize, key: &[u8]) -> Option<&'static [u8]> {
    let vt = word(xml, 0)? as usize;
    let address = word(vt, 0x60)? as usize;
    if !readable(address, 1) {
        return None;
    }
    let get: unsafe extern "thiscall" fn(usize, *const u8) -> *const u8 =
        std::mem::transmute(address);
    cbytes(get(xml, key.as_ptr()) as usize, 128)
}
fn omit_attachment(exact_parent: bool, category: Option<&[u8]>) -> bool {
    exact_parent && category == Some(b"Optic")
}
unsafe fn attachment_with(
    catalog: usize,
    xml: usize,
    parent: usize,
    enabled: bool,
    original: impl FnOnce() -> u8,
) -> u8 {
    if enabled
        && word(catalog, 0) == Some(STANDARD)
        && omit_attachment(retail_weapon(parent), xml_attr_if_retail(parent, xml))
    {
        // Successful no-op before allocation: CF2470 advances its original
        // cursor normally. Never allocate then remove/destroy a native item.
        return 1;
    }
    original()
}
unsafe fn xml_attr_if_retail(parent: usize, xml: usize) -> Option<&'static [u8]> {
    if retail_weapon(parent) {
        xml_attr(xml, b"Category\0")
    } else {
        None
    }
}
fn attachment(catalog: usize, xml: usize, parent: usize) -> u8 {
    unsafe {
        attachment_with(catalog, xml, parent, READY.load(Ordering::Acquire), || {
            Attachment.call(catalog, xml, parent)
        })
    }
}
fn register(catalog: usize, xml: usize) -> u8 {
    unsafe {
        let active = READY.load(Ordering::Acquire);
        let exact = active
            && word(catalog, 0) == Some(STANDARD)
            && xml_attr(xml, b"NetName\0") == Some(RETAIL_NAME);
        let result = Register.call(catalog, xml);
        if result != 0 && exact {
            tracing::info!("SCBL_MOD_MAC11 revision=39 existing=true moved={} slot=Pistol omit=Optic ammo=preserved",move_existing(catalog));
        }
        result
    }
}
unsafe fn lookup_with(
    catalog: usize,
    id: u32,
    enabled: bool,
    mut original: impl FnMut(u32) -> usize,
) -> usize {
    let found = original(id);
    if found != 0 || !enabled || word(catalog, 0) != Some(STANDARD) {
        return found;
    }
    let Some(replacement) = legacy_id(id) else {
        return 0;
    };
    let parent = original(RETAIL_ID);
    if !retail_weapon(parent) {
        return 0;
    }
    let result = original(replacement);
    if replacement == RETAIL_ID {
        return parent;
    }
    if word(result, 0x10) == Some(replacement)
        && vector(parent, 0x88).is_some_and(|items| items.contains(&result))
    {
        result
    } else {
        0
    }
}
fn lookup(catalog: usize, id: u32) -> usize {
    unsafe {
        lookup_with(catalog, id, READY.load(Ordering::Acquire), |key| {
            Lookup.call(catalog, key)
        })
    }
}
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
                tracing::error!("SCBL_MOD_MAC11 enabled=false reason=native-signature");
                return;
            }
        }
        let ok = Register
            .initialize(std::mem::transmute::<usize, Factory>(0x00cf2470), register)
            .is_ok()
            && Attachment
                .initialize(
                    std::mem::transmute::<usize, AttachmentFactory>(0x00ceba70),
                    attachment,
                )
                .is_ok()
            && Lookup
                .initialize(std::mem::transmute::<usize, Find>(0x00cad660), lookup)
                .is_ok()
            && Lookup.enable().is_ok()
            && Attachment.enable().is_ok()
            && Register.enable().is_ok()
            && combat::init();
        if !ok {
            deinit();
            tracing::error!("SCBL_MOD_MAC11 enabled=false reason=detour");
            return;
        }
        READY.store(true, Ordering::Release);
        // Cosmetic adapter fails closed independently: a preview mismatch
        // must not disable the already validated catalog/combat feature.
        preview::init();
        tracing::info!("SCBL_MOD_MAC11 revision=39 adapter=true existing_only=true slot=Pistol omit=Optic catalog_pending=true");
    }
}
pub(crate) unsafe fn deinit() {
    preview::deinit();
    combat::deinit();
    READY.store(false, Ordering::Release);
    if Register.is_enabled() {
        let _ = Register.disable();
    }
    if Attachment.is_enabled() {
        let _ = Attachment.disable();
    }
    if Lookup.is_enabled() {
        let _ = Lookup.disable();
    }
    // Engine owns every record. Do not reclassify/free a live equipped object.
}
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        0xcf2470,
        1011,
        "ee68caf77bd1d0d7ef798fdf2dce7b54d5d77464a4cc5ceba6203ee1e0969c90",
    ),
    (
        0xceba70,
        742,
        "0ab863401e68860bc7849983103daf4fc1f043b81fbcb3bdf397e16c62be767b",
    ),
    (
        0xcad660,
        109,
        "cb21890006816d33ce30217c6db74ce8606875f6a647710b34e2a25a14a4991e",
    ),
];
#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: u32, n: &'static [u8], kind: u32) -> Vec<u32> {
        let mut x = vec![0x55aa55aa; 0xb4 / 4];
        x[4] = id;
        x[0x18 / 4] = n.as_ptr() as u32;
        x[0x1c / 4] = n.len() as u32;
        x[0x20 / 4] = n.len() as u32;
        x[0x3c / 4] = kind;
        x[0x40 / 4] = 1;
        x[0x88 / 4] = 0;
        x[0x8c / 4] = 0;
        x[0x90 / 4] = 0;
        x
    }
    fn catalog(items: &[u32]) -> [u32; 16] {
        let mut c = [0u32; 16];
        c[0] = STANDARD;
        c[0x34 / 4] = items.as_ptr() as u32;
        c[0x38 / 4] = items.len() as u32;
        c[0x3c / 4] = items.len() as u32;
        c
    }
    #[test]
    fn move_changes_exactly_slot_not_identity_stats_or_other_items() {
        let mut target = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        let sp = item(2406210396, b"SMG_MAC11\0", 0);
        let other = item(603875156, b"ADV_Spy_SMG_MP5\0", 1);
        let before = target.clone();
        let sp_before = sp.clone();
        let other_before = other.clone();
        let pointers = [
            sp.as_ptr() as u32,
            target.as_mut_ptr() as u32,
            other.as_ptr() as u32,
        ];
        let c = catalog(&pointers);
        let before_c = c;
        unsafe {
            assert!(move_existing(c.as_ptr() as usize));
            assert!(move_existing(c.as_ptr() as usize));
        }
        let mut expected = before;
        expected[0x40 / 4] = 0;
        assert_eq!(target, expected);
        assert_eq!(sp, sp_before);
        assert_eq!(other, other_before);
        assert_eq!(c, before_c);
    }
    #[test]
    fn ambiguous_or_missing_retail_entry_is_not_mutated() {
        let target = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        let before = target.clone();
        for pointers in [vec![], vec![target.as_ptr() as u32, target.as_ptr() as u32]] {
            let c = catalog(&pointers);
            unsafe {
                assert!(!move_existing(c.as_ptr() as usize));
            }
        }
        assert_eq!(target, before);
    }
    #[test]
    fn exact_id_name_type_and_slot_required() {
        let mut t = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        unsafe {
            assert!(retail_weapon(t.as_ptr() as usize));
            t[4] = 1424978178;
            assert!(!retail_weapon(t.as_ptr() as usize));
            t[4] = RETAIL_ID;
            t[0x3c / 4] = 0;
            assert!(!retail_weapon(t.as_ptr() as usize));
            t[0x3c / 4] = 1;
            t[0x40 / 4] = 11;
            assert!(!retail_weapon(t.as_ptr() as usize));
            t[0x40 / 4] = 1;
            t[0x18 / 4] = b"OTHER\0".as_ptr() as u32;
            assert!(!retail_weapon(t.as_ptr() as usize));
            assert!(!retail_weapon(0));
        }
    }
    #[test]
    fn only_optics_removed_ammo_and_every_other_category_preserved() {
        for category in [
            b"Optic".as_slice(),
            b"Ammo",
            b"Internal",
            b"Muzzle",
            b"Magazine",
            b"Foregrip",
            b"",
            b"Unknown",
        ] {
            assert_eq!(omit_attachment(true, Some(category)), category == b"Optic");
            assert!(!omit_attachment(false, Some(category)));
        }
        assert!(!omit_attachment(true, None));
    }
    #[repr(C)]
    struct Xml {
        table: *const usize,
        category: *const u8,
    }
    unsafe extern "thiscall" fn get(xml: usize, key: *const u8) -> *const u8 {
        if cbytes(key as usize, 32) == Some(b"Category") {
            (*(xml as *const Xml)).category
        } else {
            b"\0".as_ptr()
        }
    }
    #[test]
    fn filtered_attachment_never_calls_factory_or_mutates_vectors() {
        let target = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        let items = [target.as_ptr() as u32];
        let c = catalog(&items);
        let mut table = [0usize; 25];
        table[0x60 / 4] = get as usize;
        let xml = Xml {
            table: table.as_ptr(),
            category: b"Optic\0".as_ptr(),
        };
        let mut calls = 0;
        unsafe {
            assert_eq!(
                attachment_with(
                    c.as_ptr() as usize,
                    &xml as *const _ as usize,
                    target.as_ptr() as usize,
                    true,
                    || {
                        calls += 1;
                        0
                    }
                ),
                1
            );
            assert_eq!(calls, 0);
            assert_eq!(
                attachment_with(
                    c.as_ptr() as usize,
                    &xml as *const _ as usize,
                    target.as_ptr() as usize,
                    false,
                    || {
                        calls += 1;
                        0x5a
                    }
                ),
                0x5a
            );
        }
        assert_eq!(calls, 1);
        assert_eq!(target[0x8c / 4], 0);
    }
    #[test]
    fn ammo_factory_failure_is_preserved_not_swallowed() {
        let target = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        let items = [target.as_ptr() as u32];
        let c = catalog(&items);
        let mut table = [0usize; 25];
        table[0x60 / 4] = get as usize;
        let xml = Xml {
            table: table.as_ptr(),
            category: b"Ammo\0".as_ptr(),
        };
        let mut calls = 0;
        unsafe {
            assert_eq!(
                attachment_with(
                    c.as_ptr() as usize,
                    &xml as *const _ as usize,
                    target.as_ptr() as usize,
                    true,
                    || {
                        calls += 1;
                        0
                    }
                ),
                0
            );
        }
        assert_eq!(calls, 1);
    }
    #[test]
    fn legacy_saved_parent_resolves_same_existing_object_without_duplicates() {
        let parent = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        let p = parent.as_ptr() as usize;
        let items = [p as u32];
        let c = catalog(&items);
        for (id, enabled, expected) in [
            (1424978178, true, p),
            (1424978178, false, 0),
            (123, true, 0),
            (RETAIL_ID, true, p),
        ] {
            unsafe {
                assert_eq!(
                    lookup_with(
                        c.as_ptr() as usize,
                        id,
                        enabled,
                        |key| if key == RETAIL_ID { p } else { 0 }
                    ),
                    expected
                );
            }
        }
        assert_eq!(c[0x38 / 4], 1);
        assert_eq!(legacy_id(2341129349), None); // No new extended magazine.
        assert_eq!(legacy_id(2515535089), None); // No SP-only hollow-point.
    }
    #[test]
    fn original_lookup_always_wins_and_alias_needs_owned_child() {
        let child = item(2407719902, b"ADV_Spy_SMG_MAC11.SMG_Ammo_AP\0", 1);
        let cp = child.as_ptr() as usize;
        let children = [cp as u32];
        let mut parent = item(RETAIL_ID, b"ADV_Spy_SMG_MAC11\0", 1);
        parent[0x88 / 4] = children.as_ptr() as u32;
        parent[0x8c / 4] = 1;
        parent[0x90 / 4] = 1;
        let p = parent.as_ptr() as usize;
        let items = [p as u32];
        let c = catalog(&items);
        unsafe {
            assert_eq!(
                lookup_with(c.as_ptr() as usize, 705715625, true, |key| match key {
                    RETAIL_ID => p,
                    2407719902 => cp,
                    _ => 0,
                }),
                cp
            );
            assert_eq!(
                lookup_with(c.as_ptr() as usize, 705715625, true, |_| 99),
                99
            );
            parent[0x8c / 4] = 0;
            assert_eq!(
                lookup_with(c.as_ptr() as usize, 705715625, true, |key| match key {
                    RETAIL_ID => p,
                    2407719902 => cp,
                    _ => 0,
                }),
                0
            );
        }
    }
}

#[cfg(test)]
mod native_abi_tests {
    use super::*;
    #[test]
    #[ignore = "explicit MSVC x86 factory/attachment/lookup ABI fixture"]
    fn native_three_detours_preserve_thiscall_arguments_results_and_stack() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(path: *const u16) -> *mut c_void;
            fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
            fn FreeLibrary(module: *mut c_void) -> i32;
        }
        let path = std::env::var("SCBL_MAC11_ABI_FIXTURE").unwrap();
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let module = LoadLibraryW(path.as_ptr());
            assert!(!module.is_null());
            let get = GetProcAddress(module, b"_get_entry@4\0".as_ptr());
            let check = GetProcAddress(module, b"_check_entries@12\0".as_ptr());
            assert!(!get.is_null() && !check.is_null());
            let get: unsafe extern "system" fn(u32) -> usize = std::mem::transmute(get);
            let check: unsafe extern "system" fn(usize, usize, usize) -> u32 =
                std::mem::transmute(check);
            let (w, a, l) = (get(0), get(1), get(2));
            assert_eq!(check(w, a, l), 3000);
            Register
                .initialize(std::mem::transmute::<usize, Factory>(w), register)
                .unwrap();
            Attachment
                .initialize(
                    std::mem::transmute::<usize, AttachmentFactory>(a),
                    attachment,
                )
                .unwrap();
            Lookup
                .initialize(std::mem::transmute::<usize, Find>(l), lookup)
                .unwrap();
            Register.enable().unwrap();
            Attachment.enable().unwrap();
            Lookup.enable().unwrap();
            assert_eq!(check(w, a, l), 3000);
            deinit();
            assert_eq!(check(w, a, l), 3000);
            // Detour objects retain their trampoline; leave fixture resident
            // until the test process exits rather than dangling its code.
            let _ = FreeLibrary as usize;
        }
        println!("MSVC x86 native original/detoured/restored: 9000 calls; weapon ret4,attachment ret8,lookup ret4; results and stack preserved");
    }
}

#[cfg(test)]
#[path = "mac11_identity_tests.rs"]
mod identity_tests;
