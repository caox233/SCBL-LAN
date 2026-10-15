//! MOD-only unlock projection over native, already registered multiplayer items.
//! CB6CC0 reads Cost/TokenCost/Purchased/Unlocked; CC9D60 sets Type 0/1/2.
//! Never invent assets, change NetID/archetypes, debit currency or touch campaign.
use crate::equipment_native::{readable, word, writable};
use retour::static_detour;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
type ParseFn = unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
type SerializeFn = unsafe extern "thiscall" fn(usize, usize);
type CamoFn = unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
type StatusFn = unsafe extern "thiscall" fn(usize, usize) -> i32;
static STATUS_LOGS: AtomicU32 = AtomicU32::new(0);
static_detour! {
    static Parse: unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
    static Serialize: unsafe extern "thiscall" fn(usize, usize);
    static Camo: unsafe extern "thiscall" fn(usize, usize, usize) -> u8;
    static ArmorSerialize: unsafe extern "thiscall" fn(usize, usize);
    static Status: unsafe extern "thiscall" fn(usize, usize) -> i32;
}
// The native menu queries ownership AFTER catalog/profile initialization.
// Camos may have Type=0 even when owned by MP armor; never infer their scope
// from a name or grant all Type=0 records (that would unlock campaign items).
unsafe fn armor_catalog(registry: usize) -> Option<usize> {
    let data = word(registry, 4)? as usize;
    let len = word(registry, 8)? as usize;
    let cap = word(registry, 12)? as usize;
    if len > cap || cap > 64 || len == 0 || !readable(data, len.checked_mul(12)?) {
        return None;
    }
    for i in 0..len {
        let entry = data + i * 12;
        // Native TMap slots with chain <= -2 are tombstones, not live entries.
        if (word(entry, 0)? as i32) < -1 || word(entry, 4)? != 4 {
            continue;
        }
        let catalog = word(entry, 8)? as usize;
        if word(catalog, 0) == Some(0x02ae1f08) {
            return Some(catalog);
        }
    }
    None
}
unsafe fn unlock_queried_camo(catalog: usize, item: usize) -> bool {
    if word(catalog, 0) != Some(0x02ae1f08) || word(item, 0x40) != Some(13) {
        return false;
    }
    let Some(roots) = vector(catalog, 0x34) else {
        return false;
    };
    for parent in roots {
        let Some(kind @ (1 | 2)) = word(parent, 0x3c) else {
            continue;
        };
        if !word(item, 0x3c).is_some_and(|k| k == 0 || k == kind) {
            continue;
        }
        if vector(parent, 0x9c).is_some_and(|children| children.contains(&item)) {
            return grant_availability(item);
        }
    }
    false
}
unsafe fn status_with(
    ui: usize,
    item: usize,
    catalog: usize,
    enabled: bool,
    original: impl FnOnce(usize, usize) -> i32,
) -> i32 {
    let changed = enabled && unlock_queried_camo(catalog, item);
    // 1900B00 checks +1/+0, then F17630 checks the real loadout selection.
    // Call it once, unchanged: never return a fabricated "equipped" result.
    let result = original(ui, item);
    if changed && STATUS_LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
        tracing::info!("SCBL_MOD_UNLOCK revision=48 phase=camo-status availability_granted=true native_status={result} selections_unchanged=true");
    }
    result
}
fn status(ui: usize, item: usize) -> i32 {
    unsafe {
        let enabled = READY.load(Ordering::Acquire);
        let catalog = if enabled && word(item, 0x40) == Some(13) {
            word(0x0339e5a4, 0)
                .and_then(|root| word(root as usize, 0x14))
                .and_then(|registry| armor_catalog(registry as usize))
                .unwrap_or(0)
        } else {
            0
        };
        status_with(ui, item, catalog, enabled, |u, i| Status.call(u, i))
    }
}
unsafe fn unlock_item(item: usize) -> bool {
    if !matches!(word(item, 0x3c), Some(1 | 2)) {
        return false;
    }
    grant_availability(item)
}
unsafe fn grant_availability(item: usize) -> bool {
    if !word(item, 0x10).is_some_and(|id| id != 0) || !writable(item, 4) {
        return false;
    }
    let flags = std::slice::from_raw_parts_mut(item as *mut u8, 4);
    if flags.iter().any(|v| *v > 1) {
        return false;
    }
    // +0 Purchased; +1 Unlocked. +2 is the XML default equipment seed:
    // C9A4AC/C9A559 -> F086C0 auto-equips every child whose +2 is nonzero.
    // CB6CC0 computes it ONCE from XML Purchased/Unlocked, not current ownership.
    // Preserve +2/+3, prices, identity, category and all saved selections.
    let changed = flags[0] != 1 || flags[1] != 1;
    flags[..2].copy_from_slice(&[1, 1]);
    changed
}
unsafe fn vector(catalog: usize, offset: usize) -> Option<Vec<usize>> {
    let data = word(catalog, offset)? as usize;
    let len = word(catalog, offset + 4)? as usize;
    let cap = word(catalog, offset + 8)? as usize;
    if len > cap || cap > 8192 {
        return None;
    }
    if len == 0 {
        return Some(Vec::new());
    }
    if !readable(data, len.checked_mul(4)?) {
        return None;
    }
    Some(
        (0..len)
            .map(|i| ((data + i * 4) as *const u32).read_unaligned() as usize)
            .collect(),
    )
}
unsafe fn refresh_catalog(catalog: usize) -> usize {
    // CCADB0 serializes root vector +8; attachment factory CEBA70 inserts
    // attachments into +14. Only a weapon ROOT has a child vector at +88;
    // an attachment has its category there instead. Do not follow it as a vector.
    let Some(roots) = vector(catalog, 8) else {
        return 0;
    };
    let Some(children) = vector(catalog, 0x14) else {
        return 0;
    };
    roots
        .into_iter()
        .chain(children)
        .filter(|item| unlock_item(*item))
        .count()
}
unsafe fn unlock_armor_camos(parent: usize) -> usize {
    let Some(kind @ (1 | 2)) = word(parent, 0x3c) else {
        return 0;
    };
    // Retail CEB530 registers camo children at armor +9C, slot=13. A camo
    // may omit Type (0); ONLY the actual MP owning armor grants that scope.
    let Some(children) = vector(parent, 0x9c) else {
        return 0;
    };
    children
        .into_iter()
        .filter(|child| {
            word(*child, 0x40) == Some(13)
                && word(*child, 0x3c).is_some_and(|k| k == 0 || k == kind)
                && grant_availability(*child)
        })
        .count()
}
unsafe fn refresh_armor(catalog: usize) -> usize {
    // CAD2F0 serializes armor roots +34 and each root's camo vector +9C.
    let Some(roots) = vector(catalog, 0x34) else {
        return 0;
    };
    roots
        .into_iter()
        .map(|parent| usize::from(unlock_item(parent)) + unlock_armor_camos(parent))
        .sum()
}
unsafe fn camo_with(
    catalog: usize,
    xml: usize,
    parent: usize,
    enabled: bool,
    original: impl FnOnce(usize, usize, usize) -> u8,
) -> u8 {
    let result = original(catalog, xml, parent);
    if enabled && result != 0 {
        unlock_armor_camos(parent);
    }
    result
}
fn camo(catalog: usize, xml: usize, parent: usize) -> u8 {
    unsafe {
        camo_with(
            catalog,
            xml,
            parent,
            READY.load(Ordering::Acquire),
            |c, x, p| Camo.call(c, x, p),
        )
    }
}
unsafe fn armor_serialize_with(
    catalog: usize,
    archive: usize,
    enabled: bool,
    original: impl FnOnce(usize, usize),
) {
    original(catalog, archive);
    if enabled && word(archive, 4).is_some_and(|f| f & 0x1100 == 0x100) {
        let changed = refresh_armor(catalog);
        if LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
            tracing::info!("SCBL_MOD_UNLOCK revision=47 phase=armor-after-load items={changed} selections_unchanged=true");
        }
    }
}
fn armor_serialize(catalog: usize, archive: usize) {
    unsafe {
        armor_serialize_with(catalog, archive, READY.load(Ordering::Acquire), |c, a| {
            ArmorSerialize.call(c, a)
        });
    }
}
unsafe fn parse_with(
    catalog: usize,
    xml: usize,
    item: usize,
    enabled: bool,
    original: impl FnOnce(usize, usize, usize) -> u8,
) -> u8 {
    let result = original(catalog, xml, item);
    if enabled && result != 0 {
        unlock_item(item);
    }
    result
}
fn parse(catalog: usize, xml: usize, item: usize) -> u8 {
    unsafe {
        parse_with(
            catalog,
            xml,
            item,
            READY.load(Ordering::Acquire),
            |c, x, i| Parse.call(c, x, i),
        )
    }
}
unsafe fn serialize_with(
    catalog: usize,
    archive: usize,
    enabled: bool,
    original: impl FnOnce(usize, usize),
) {
    original(catalog, archive);
    // Only refresh after a native load. Never replace the serializer or its
    // sections/children, and do not mutate the catalog during a write archive.
    if enabled && word(archive, 4).is_some_and(|flags| flags & 0x1100 == 0x100) {
        let changed = refresh_catalog(catalog);
        if LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
            tracing::info!("SCBL_MOD_UNLOCK revision=47 phase=after-load items={changed} native_defaults_preserved=true");
        }
    }
}
fn serialize(catalog: usize, archive: usize) {
    unsafe {
        serialize_with(catalog, archive, READY.load(Ordering::Acquire), |c, a| {
            Serialize.call(c, a)
        });
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
                tracing::error!("SCBL_MOD_UNLOCK enabled=false reason=native-signature");
                return;
            }
        }
        if Parse
            .initialize(std::mem::transmute::<usize, ParseFn>(0xcb6cc0), parse)
            .is_err()
            || Serialize
                .initialize(
                    std::mem::transmute::<usize, SerializeFn>(0xccadb0),
                    serialize,
                )
                .is_err()
            || Camo
                .initialize(std::mem::transmute::<usize, CamoFn>(0xceb530), camo)
                .is_err()
            || ArmorSerialize
                .initialize(
                    std::mem::transmute::<usize, SerializeFn>(0xcad2f0),
                    armor_serialize,
                )
                .is_err()
            || Status
                .initialize(std::mem::transmute::<usize, StatusFn>(0x1900b00), status)
                .is_err()
            || Parse.enable().is_err()
            || Serialize.enable().is_err()
            || Camo.enable().is_err()
            || ArmorSerialize.enable().is_err()
            || Status.enable().is_err()
        {
            deinit();
            tracing::error!("SCBL_MOD_UNLOCK enabled=false reason=detour");
            return;
        }
        READY.store(true, Ordering::Release);
        tracing::info!("SCBL_MOD_UNLOCK revision=48 enabled=true scope=spy-merc-native-catalog armor_camos=true native_defaults_preserved=true camo_status_boundary=true");
    }
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if Status.is_enabled() {
        let _ = Status.disable();
    }
    if ArmorSerialize.is_enabled() {
        let _ = ArmorSerialize.disable();
    }
    if Camo.is_enabled() {
        let _ = Camo.disable();
    }
    if Serialize.is_enabled() {
        let _ = Serialize.disable();
    }
    if Parse.is_enabled() {
        let _ = Parse.disable();
    }
    // No native allocation or retained object pointers. MOD host is process-lifetime.
}
const SIGNATURES: &[(usize, usize, &str)] = &[
    (
        0x1900b00,
        117,
        "28a15262726bd0a404d0254ff716479e22e7fc3bf332e066d7ef3689b5e1808a",
    ),
    (
        0xceb530,
        413,
        "53a2e62e7cb4110e83cb70f1b93813d5b0e62153a2340db71c54a3999ebdabe2",
    ),
    (
        0xcad2f0,
        382,
        "5b3807177e65ba46dab2116d584e3d6a3922eb1f5eae67b9d7ba7ebdd9f5390e",
    ),
    (
        0xc9a460,
        307,
        "f93bdcd4ecd1be6d75d0cbb359e6d578721f166a4d696b46dca0e3dd493d2440",
    ),
    (
        0xcb6cc0,
        525,
        "0cda29175cedc31eb1d5285812d51f141ad955553ef3da9ef38a1b1e8125e3f4",
    ),
    (
        0xccadb0,
        696,
        "1d8b9a99533fcf5103a43816c4b316911ce2d6964e0e82f1a0918d157b2b82e6",
    ),
    (
        0xcc9d60,
        320,
        "a9cfc6ec4ebd9bca22589fd5065bbd36bfdf72cc6439153a6d24388dcf465dae",
    ),
];
#[cfg(test)]
mod tests {
    use super::*;
    fn item(kind: u32) -> Vec<u32> {
        let mut p = vec![0xaabbccdd; 0xa0 / 4];
        p[0] = 0x01000100;
        p[0x3c / 4] = kind;
        p[0x10 / 4] = 123;
        p
    }

    #[test]
    #[ignore = "explicit MSVC x86 catalog parse ret8 / serialize ret4 ABI fixture"]
    fn native_catalog_parser_and_serializer_abi() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        fn fixture_parse(c: usize, x: usize, i: usize) -> u8 {
            unsafe { parse_with(c, x, i, true, |c, x, i| Parse.call(c, x, i)) }
        }
        fn fixture_serialize(c: usize, a: usize) {
            unsafe { serialize_with(c, a, true, |c, a| Serialize.call(c, a)) }
        }
        let path = std::env::var("SCBL_UNLOCK_ABI_FIXTURE").unwrap();
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(path.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_entry@4\0".as_ptr());
            let parse_call = GetProcAddress(dll, b"_invoke_parse@16\0".as_ptr());
            let save_call = GetProcAddress(dll, b"_invoke_serialize@12\0".as_ptr());
            assert!(!get.is_null() && !parse_call.is_null() && !save_call.is_null());
            let get: unsafe extern "system" fn(u32) -> usize = std::mem::transmute(get);
            let pc: unsafe extern "system" fn(usize, usize, usize, usize) -> u32 =
                std::mem::transmute(parse_call);
            let sc: unsafe extern "system" fn(usize, usize, usize) -> u32 =
                std::mem::transmute(save_call);
            let mut p = item(1);
            let mut ptrs = [p.as_mut_ptr() as u32];
            let mut catalog = [0u32; 16];
            catalog[2] = ptrs.as_mut_ptr() as u32;
            catalog[3] = 1;
            catalog[4] = 1;
            let mut archive = [0u32, 0x100];
            let c = catalog.as_mut_ptr() as usize;
            let a = archive.as_mut_ptr() as usize;
            let item = p.as_mut_ptr() as usize;
            for stage in 0..3 {
                if stage == 1 {
                    Parse
                        .initialize(std::mem::transmute::<usize, ParseFn>(get(0)), fixture_parse)
                        .unwrap();
                    Serialize
                        .initialize(
                            std::mem::transmute::<usize, SerializeFn>(get(1)),
                            fixture_serialize,
                        )
                        .unwrap();
                    Parse.enable().unwrap();
                    Serialize.enable().unwrap();
                }
                if stage == 2 {
                    Serialize.disable().unwrap();
                    Parse.disable().unwrap();
                }
                for _ in 0..1000 {
                    assert_eq!(pc(get(0), c, 0x12345678, item), 0x15a);
                    assert_eq!(catalog[9], 0x12345678);
                    assert_eq!(catalog[10], item as u32);
                    assert_eq!(p[0], if stage == 1 { 0x00000101 } else { 0 });
                    assert_eq!(sc(get(1), c, a), 1);
                    assert_eq!(catalog[12], a as u32);
                    assert_eq!(p[0], if stage == 1 { 0x00000101 } else { 0 });
                }
            }
            assert_eq!(catalog[8], 3000);
            assert_eq!(catalog[11], 3000);
        }
        println!("Native MSVC x86: parse ret8 / serialize ret4; baseline locked, modified unlocked, rollback locked; 6000 original-once calls with unchanged ECX, args, return, ESP. ABI fixture, not real assets.");
    }

    #[test]
    fn native_mp_flags_only_and_idempotent() {
        for kind in [1, 2] {
            let mut p = item(kind);
            let mut expected = p.clone();
            expected[0] = 0x01000101;
            assert!(unsafe { unlock_item(p.as_mut_ptr() as usize) });
            assert_eq!(p, expected);
            assert!(!unsafe { unlock_item(p.as_mut_ptr() as usize) });
        }
    }
    #[test]
    fn campaign_unknown_corrupt_and_missing_items_unchanged() {
        for kind in [0, 3, 4, 7, u32::MAX] {
            let mut p = item(kind);
            let before = p.clone();
            assert!(!unsafe { unlock_item(p.as_mut_ptr() as usize) });
            assert_eq!(p, before);
        }
        let mut p = item(1);
        p[0] = 0x02000100;
        let before = p.clone();
        assert!(!unsafe { unlock_item(p.as_mut_ptr() as usize) });
        assert_eq!(p, before);
        assert!(!unsafe { unlock_item(0) });
        p[0] = 0;
        p[0x10 / 4] = 0;
        assert!(!unsafe { unlock_item(p.as_mut_ptr() as usize) });
    }
    #[test]
    fn parser_preserves_failure_arguments_result_and_once() {
        for (enabled, result, unlocked) in [(true, 0, false), (false, 1, false), (true, 0x5a, true)]
        {
            let mut p = item(1);
            let ptr = p.as_mut_ptr() as usize;
            let calls = std::cell::Cell::new(0);
            let output = unsafe {
                parse_with(4, 5, ptr, enabled, |c, x, i| {
                    assert_eq!((c, x, i), (4, 5, ptr));
                    calls.set(calls.get() + 1);
                    result
                })
            };
            assert_eq!(output, result);
            assert_eq!(calls.get(), 1);
            assert_eq!(p[0], if unlocked { 0x01000101 } else { 0x01000100 });
        }
    }
    #[test]
    fn load_refreshes_root_and_attachment_not_campaign_or_save() {
        let mut root = item(1);
        let mut attachment = item(2);
        let mut campaign = item(0);
        let mut roots = [root.as_mut_ptr() as u32, campaign.as_mut_ptr() as u32];
        let mut children = [attachment.as_mut_ptr() as u32];
        let mut catalog = [0u32; 0x24 / 4];
        catalog[2] = roots.as_mut_ptr() as u32;
        catalog[3] = 2;
        catalog[4] = 2;
        catalog[5] = children.as_mut_ptr() as u32;
        catalog[6] = 1;
        catalog[7] = 1;
        for (flags, enabled, should) in [
            (0x1000, true, false),
            (0x1100, true, false),
            (0x100, false, false),
            (0x100, true, true),
        ] {
            root[0] = 0;
            attachment[0] = 0;
            campaign[0] = 0;
            let mut archive = [0u32, flags];
            let calls = std::cell::Cell::new(0);
            unsafe {
                serialize_with(
                    catalog.as_mut_ptr() as usize,
                    archive.as_mut_ptr() as usize,
                    enabled,
                    |_, _| calls.set(calls.get() + 1),
                )
            };
            assert_eq!(calls.get(), 1);
            assert_eq!(root[0], if should { 0x00000101 } else { 0 });
            assert_eq!(attachment[0], root[0]);
            assert_eq!(campaign[0], 0);
        }
        root[0] = 0;
        catalog[6] = 9000;
        assert_eq!(unsafe { refresh_catalog(catalog.as_mut_ptr() as usize) }, 0);
        assert_eq!(root[0], 0);
    }
}

#[cfg(test)]
mod r47_probe {
    #[test]
    fn identical_input_native_default_equipment() {
        let mut record = [0u32; 0xA0 / 4];
        record[0x3c / 4] = 1;
        record[0x10 / 4] = 123;
        unsafe {
            super::unlock_item(record.as_mut_ptr() as usize);
        }
        let f = record[0].to_le_bytes();
        // Retail C9A4AC -> F086C0 adds this item iff catalog byte +2 is nonzero.
        println!("R47 same input MP locked attachment: purchased={} unlocked={} default_auto_equip={} xml_default_unlocked={}", f[0], f[1], f[2], f[3]);
    }
}

#[cfg(test)]
mod r47_tests {
    use super::*;
    fn record(kind: u32, slot: u32, flags: [u8; 4]) -> Vec<u32> {
        let mut r = vec![0; 0xb0 / 4];
        r[0] = u32::from_le_bytes(flags);
        r[0x10 / 4] = 77;
        r[0x3c / 4] = kind;
        r[0x40 / 4] = slot;
        r
    }
    fn own(parent: &mut [u32], children: &mut [u32]) {
        parent[0x9c / 4] = children.as_mut_ptr() as u32;
        parent[0xa0 / 4] = children.len() as u32;
        parent[0xa4 / 4] = children.len() as u32;
    }
    #[test]
    fn unlock_never_changes_native_default_equipment_seed() {
        for kind in [1, 2] {
            for seed in [0, 1] {
                for xml_unlocked in [0, 1] {
                    let mut r = record(kind, 11, [0, 0, seed, xml_unlocked]);
                    let before = r.clone();
                    unsafe {
                        unlock_item(r.as_mut_ptr() as usize);
                    }
                    assert_eq!(r[0].to_le_bytes(), [1, 1, seed, xml_unlocked]);
                    assert_eq!(r[1..], before[1..]);
                }
            }
        }
    }
    #[test]
    fn multiplayer_camo_scope_comes_from_owning_armor_only() {
        for parent_kind in [0, 1, 2, 3, u32::MAX] {
            for child_kind in [0, 1, 2, 3] {
                for slot in [11, 12, 13, 14] {
                    let mut parent = record(parent_kind, 12, [1, 1, 1, 1]);
                    let mut camo = record(child_kind, slot, [0, 1, 0, 1]);
                    let before = camo.clone();
                    let mut children = [camo.as_mut_ptr() as u32];
                    own(&mut parent, &mut children);
                    let parent_before = parent.clone();
                    let changed = unsafe { unlock_armor_camos(parent.as_mut_ptr() as usize) };
                    let allow = matches!(parent_kind, 1 | 2)
                        && (child_kind == 0 || child_kind == parent_kind)
                        && slot == 13;
                    assert_eq!(changed, usize::from(allow));
                    assert_eq!(camo[0], if allow { 0x01000101 } else { before[0] });
                    assert_eq!(camo[1..], before[1..]);
                    assert_eq!(parent, parent_before);
                }
            }
        }
    }
    #[test]
    fn camo_factory_failure_and_off_preserve_original_once_and_arguments() {
        for (enabled, result) in [(true, 0), (false, 0x5a), (true, 0x5a)] {
            let mut parent = record(1, 12, [1, 1, 1, 1]);
            let mut camo = record(0, 13, [0, 1, 0, 1]);
            let mut children = [camo.as_mut_ptr() as u32];
            own(&mut parent, &mut children);
            let p = parent.as_mut_ptr() as usize;
            let calls = std::cell::Cell::new(0);
            let got = unsafe {
                camo_with(123, 456, p, enabled, |c, x, q| {
                    assert_eq!((c, x, q), (123, 456, p));
                    calls.set(calls.get() + 1);
                    result
                })
            };
            assert_eq!(got, result);
            assert_eq!(calls.get(), 1);
            assert_eq!(
                camo[0],
                if enabled && result != 0 {
                    0x01000101
                } else {
                    0x01000100
                }
            );
        }
    }
    #[test]
    fn armor_load_reapplies_availability_without_selecting_or_writing_campaign() {
        let mut mp = record(1, 12, [0, 0, 0, 0]);
        let mut sp = record(0, 12, [0, 0, 0, 0]);
        let mut mp_camo = record(0, 13, [0, 0, 0, 0]);
        let mut sp_camo = record(0, 13, [0, 0, 0, 0]);
        let mut mp_ptrs = [mp_camo.as_mut_ptr() as u32];
        own(&mut mp, &mut mp_ptrs);
        let mut sp_ptrs = [sp_camo.as_mut_ptr() as u32];
        own(&mut sp, &mut sp_ptrs);
        let mut roots = [mp.as_mut_ptr() as u32, sp.as_mut_ptr() as u32];
        let mut catalog = [0u32; 0x40 / 4];
        catalog[0x34 / 4] = roots.as_mut_ptr() as u32;
        catalog[0x38 / 4] = 2;
        catalog[0x3c / 4] = 2;
        for (enabled, archive_flags, allow) in [
            (true, 0x1000, false),
            (true, 0x1100, false),
            (false, 0x100, false),
            (true, 0x100, true),
        ] {
            let mut archive = [0, archive_flags];
            let cat = catalog.as_mut_ptr() as usize;
            let a = archive.as_mut_ptr() as usize;
            let calls = std::cell::Cell::new(0);
            unsafe {
                armor_serialize_with(cat, a, enabled, |c, x| {
                    assert_eq!((c, x), (cat, a));
                    calls.set(calls.get() + 1);
                    mp[0] = 0;
                    mp_camo[0] = 0; // the native load replaces saved ownership
                });
            }
            assert_eq!(calls.get(), 1);
            assert_eq!(mp[0], if allow { 0x101 } else { 0 });
            assert_eq!(mp_camo[0], mp[0]);
            assert_eq!((sp[0], sp_camo[0]), (0, 0));
            assert_eq!(roots, [mp.as_mut_ptr() as u32, sp.as_mut_ptr() as u32]);
        }
    }
    #[test]
    fn corrupt_camo_arrays_and_flags_fail_closed() {
        let mut parent = record(1, 12, [1, 1, 1, 1]);
        parent[0xa0 / 4] = 1;
        assert_eq!(
            unsafe { unlock_armor_camos(parent.as_mut_ptr() as usize) },
            0
        );
        let mut child = record(0, 13, [2, 0, 0, 0]);
        let before = child.clone();
        let mut children = [child.as_mut_ptr() as u32];
        own(&mut parent, &mut children);
        assert_eq!(
            unsafe { unlock_armor_camos(parent.as_mut_ptr() as usize) },
            0
        );
        assert_eq!(child, before);
        child[0] = 0;
        child[0x10 / 4] = 0;
        assert_eq!(
            unsafe { unlock_armor_camos(parent.as_mut_ptr() as usize) },
            0
        );
        assert_eq!(child[0], 0);
    }

    #[test]
    #[ignore = "explicit MSVC x86 camo ret8 and armor serializer ret4 ABI fixture"]
    fn camo_and_armor_native_abi() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        fn cp(c: usize, x: usize, p: usize) -> u8 {
            unsafe { camo_with(c, x, p, true, |c, x, p| Camo.call(c, x, p)) }
        }
        fn ar(c: usize, a: usize) {
            unsafe { armor_serialize_with(c, a, true, |c, a| ArmorSerialize.call(c, a)) }
        }
        let path = std::env::var("SCBL_CAMO_ABI_FIXTURE").unwrap();
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(path.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_entry@4\0".as_ptr());
            let fc = GetProcAddress(dll, b"_invoke_camo@16\0".as_ptr());
            let fs = GetProcAddress(dll, b"_invoke_serialize@12\0".as_ptr());
            assert!(!get.is_null() && !fc.is_null() && !fs.is_null());
            let get: unsafe extern "system" fn(u32) -> usize = std::mem::transmute(get);
            let fc: unsafe extern "system" fn(usize, usize, usize, usize) -> u32 =
                std::mem::transmute(fc);
            let fs: unsafe extern "system" fn(usize, usize, usize) -> u32 = std::mem::transmute(fs);
            let mut parent = record(1, 12, [0; 4]);
            let mut child = record(0, 13, [0; 4]);
            let mut children = [child.as_mut_ptr() as u32];
            own(&mut parent, &mut children);
            let p = parent.as_mut_ptr() as usize;
            let mut roots = [p as u32];
            let mut catalog = [0u32; 24];
            catalog[13] = roots.as_mut_ptr() as u32;
            catalog[14] = 1;
            catalog[15] = 1;
            let c = catalog.as_mut_ptr() as usize;
            let mut archive = [0u32, 0x100];
            let a = archive.as_mut_ptr() as usize;
            for stage in 0..3 {
                if stage == 1 {
                    Camo.initialize(std::mem::transmute::<usize, CamoFn>(get(0)), cp)
                        .unwrap();
                    ArmorSerialize
                        .initialize(std::mem::transmute::<usize, SerializeFn>(get(1)), ar)
                        .unwrap();
                    Camo.enable().unwrap();
                    ArmorSerialize.enable().unwrap();
                }
                if stage == 2 {
                    Camo.disable().unwrap();
                    ArmorSerialize.disable().unwrap();
                }
                for _ in 0..1000 {
                    assert_eq!(fc(get(0), c, 0x12345678, p), 0x15a);
                    assert_eq!((catalog[17], catalog[18]), (0x12345678, p as u32));
                    assert_eq!(child[0], if stage == 1 { 0x101 } else { 0 });
                    assert_eq!(fs(get(1), c, a), 1);
                    assert_eq!(catalog[20], a as u32);
                    assert_eq!(child[0], if stage == 1 { 0x101 } else { 0 });
                    assert_eq!(parent[0], child[0]);
                }
            }
            assert_eq!((catalog[16], catalog[19]), (3000, 3000));
        }
        println!("Native camo/armor same input BASELINE unavailable; MODIFIED available, default_auto_equip=0; ROLLBACK unavailable. 6000 original-once calls; ECX/args/result/ESP preserved.");
    }
}

#[cfg(test)]
mod r48_tests {
    use super::*;
    fn record(kind: u32, slot: u32) -> Vec<u32> {
        let mut r = vec![0u32; 0xb0 / 4];
        r[0] = 0x01000100;
        r[4] = 77;
        r[0x3c / 4] = kind;
        r[0x40 / 4] = slot;
        r
    }
    fn link(record: &mut [u32], offset: usize, pointers: &mut [u32]) {
        record[offset / 4] = pointers.as_mut_ptr() as u32;
        record[offset / 4 + 1] = pointers.len() as u32;
        record[offset / 4 + 2] = pointers.len() as u32;
    }
    fn native_status(item: usize, equipped: bool) -> i32 {
        unsafe {
            if item == 0 {
                return -1;
            }
            let f = std::slice::from_raw_parts(item as *const u8, 2);
            if f[1] == 0 {
                0
            } else if f[0] == 0 {
                1
            } else if equipped {
                3
            } else {
                2
            }
        }
    }
    #[test]
    fn menu_after_profile_reset_grants_mp_camo_not_selection() {
        for kind in [1, 2] {
            for equipped in [false, true] {
                let mut parent = record(kind, 5);
                let mut camo = record(0, 13);
                let before = camo.clone();
                let item = camo.as_mut_ptr() as usize;
                let mut children = [item as u32];
                link(&mut parent, 0x9c, &mut children);
                let mut roots = [parent.as_mut_ptr() as u32];
                let mut catalog = [0u32; 0x40 / 4];
                catalog[0] = 0x02ae1f08;
                link(&mut catalog, 0x34, &mut roots);
                let cat = catalog.as_mut_ptr() as usize;
                for _ in 0..3 {
                    camo[0] = before[0]; // Later profile/default restoration.
                    assert_eq!(native_status(item, equipped), 1);
                    let calls = std::cell::Cell::new(0);
                    let output = unsafe {
                        status_with(123, item, cat, true, |u, i| {
                            assert_eq!((u, i), (123, item));
                            calls.set(calls.get() + 1);
                            native_status(i, equipped)
                        })
                    };
                    assert_eq!(output, if equipped { 3 } else { 2 });
                    assert_eq!(calls.get(), 1);
                    assert_eq!(camo[0], 0x01000101);
                    assert_eq!(camo[1..], before[1..]);
                    assert_eq!(parent[0], 0x01000100);
                }
            }
        }
    }
    #[test]
    fn menu_scope_excludes_campaign_weapons_unowned_and_unknown_camos() {
        for pk in [0, 1, 2, 3] {
            for ck in [0, 1, 2, 3] {
                for slot in [11, 12, 13, 14] {
                    for member in [false, true] {
                        for enabled in [false, true] {
                            let mut parent = record(pk, 5);
                            let mut camo = record(ck, slot);
                            let before = camo.clone();
                            let item = camo.as_mut_ptr() as usize;
                            let mut children = [if member { item as u32 } else { 0 }];
                            link(&mut parent, 0x9c, &mut children);
                            let mut roots = [parent.as_mut_ptr() as u32];
                            let mut catalog = [0u32; 16];
                            catalog[0] = 0x02ae1f08;
                            link(&mut catalog, 0x34, &mut roots);
                            let allow = enabled
                                && member
                                && slot == 13
                                && matches!(pk, 1 | 2)
                                && (ck == 0 || ck == pk);
                            let calls = std::cell::Cell::new(0);
                            let got = unsafe {
                                status_with(0, item, catalog.as_ptr() as usize, enabled, |_, i| {
                                    calls.set(calls.get() + 1);
                                    native_status(i, false)
                                })
                            };
                            assert_eq!(got, if allow { 2 } else { 1 });
                            assert_eq!(calls.get(), 1);
                            assert_eq!(camo[0], if allow { 0x01000101 } else { before[0] });
                            assert_eq!(camo[1..], before[1..]);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn registry_and_menu_invalid_inputs_fail_closed() {
        let mut catalog = [0u32; 16];
        catalog[0] = 0x02ae1f08;
        let mut entries = [u32::MAX, 4, catalog.as_mut_ptr() as u32];
        let mut registry = [0u32; 4];
        link(&mut registry, 4, &mut entries[..0]);
        // Registry entries are 12 bytes, not 4-byte pointer vectors.
        registry[1] = entries.as_mut_ptr() as u32;
        registry[2] = 1;
        registry[3] = 1;
        assert_eq!(
            unsafe { armor_catalog(registry.as_ptr() as usize) },
            Some(catalog.as_ptr() as usize)
        );
        for (chain, key, vt, len, cap) in [
            (u32::MAX - 1, 4, 0x02ae1f08, 1, 1),
            (u32::MAX, 1, 0x02ae1f08, 1, 1),
            (u32::MAX, 4, 0, 1, 1),
            (u32::MAX, 4, 0x02ae1f08, 2, 1),
            (u32::MAX, 4, 0x02ae1f08, 1, 65),
            (u32::MAX, 4, 0x02ae1f08, 0, 0),
        ] {
            entries[0] = chain;
            entries[1] = key;
            catalog[0] = vt;
            registry[2] = len;
            registry[3] = cap;
            assert_eq!(unsafe { armor_catalog(registry.as_ptr() as usize) }, None);
        }
        assert_eq!(unsafe { armor_catalog(0) }, None);
        assert!(!unsafe { unlock_queried_camo(0, 0) });
        assert_eq!(
            unsafe {
                status_with(42, 0, 0, true, |u, i| {
                    assert_eq!(u, 42);
                    native_status(i, false)
                })
            },
            -1
        );
    }
    #[test]
    #[ignore = "explicit MSVC x86 native availability query ret4 fixture"]
    fn native_camo_menu_baseline_modified_rollback() {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(p: *const u16) -> *mut c_void;
            fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
        }
        fn query(ui: usize, item: usize) -> i32 {
            unsafe {
                status_with(ui, item, word(ui, 12).unwrap() as usize, true, |u, i| {
                    Status.call(u, i)
                })
            }
        }
        let path = std::env::var("SCBL_CAMO_STATUS_ABI_FIXTURE").unwrap();
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let dll = LoadLibraryW(wide.as_ptr());
            assert!(!dll.is_null());
            let get = GetProcAddress(dll, b"_get_entry@0\0".as_ptr());
            let call = GetProcAddress(dll, b"_invoke_status@12\0".as_ptr());
            assert!(!get.is_null() && !call.is_null());
            let get: unsafe extern "system" fn() -> usize = std::mem::transmute(get);
            let call: unsafe extern "system" fn(usize, usize, usize) -> u32 =
                std::mem::transmute(call);
            let entry = get();
            let mut parent = record(1, 5);
            let mut camo = record(0, 13);
            let item = camo.as_mut_ptr() as usize;
            let mut children = [item as u32];
            link(&mut parent, 0x9c, &mut children);
            let mut roots = [parent.as_mut_ptr() as u32];
            let mut catalog = [0u32; 16];
            catalog[0] = 0x02ae1f08;
            link(&mut catalog, 0x34, &mut roots);
            let mut ui = [0, 0, 0, catalog.as_ptr() as u32];
            for stage in 0..3 {
                if stage == 1 {
                    Status
                        .initialize(std::mem::transmute::<usize, StatusFn>(entry), query)
                        .unwrap();
                    Status.enable().unwrap();
                }
                if stage == 2 {
                    Status.disable().unwrap();
                }
                for _ in 0..1000 {
                    for selected in [false, true] {
                        camo[0] = 0x01000100;
                        ui[2] = if selected { item as u32 } else { 0 };
                        let expected = if stage == 1 {
                            if selected {
                                3
                            } else {
                                2
                            }
                        } else {
                            1
                        };
                        assert_eq!(
                            call(entry, ui.as_mut_ptr() as usize, item),
                            0x10000 | expected
                        );
                        assert_eq!(ui[1], item as u32);
                        assert_eq!(camo[0], if stage == 1 { 0x01000101 } else { 0x01000100 });
                    }
                }
                println!("R48 {}: native_status={} purchased={} unlocked=1 default_auto_equip=0 xml_default=1; exact native selected/unselected result preserved; calls={}",
                    ["BASELINE","MODIFIED","ROLLBACK"][stage],
                    if stage==1 {"2(unselected)/3(selected)"}else{"1(buy)"},
                    if stage==1 {1}else{0},ui[0]);
            }
            assert_eq!(ui[0], 6000);
        }
    }
}
