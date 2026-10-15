//! DX11 HudMPBATSIcons per-source range adapter, not a sound/event producer.
//! 1A49850: thiscall, 44-byte record BY VALUE + three output pointers, ret 38h.
//! 1A6FDBA passes that record; 1A6FEDE compares the first output to Euclidean
//! distance. Only that output is changed. Native event/EMP/fade/audio paths stay.
//! UArmorSet+50/54 is populated by EDA570 with parts whose +28 is their name.
//! Retail dynamicwin/loc-int identify MPArmorBootsGhost as SILENCED BOOTS.
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use retour::static_detour;
use sha2::{Digest, Sha256};
use tracing::{error, info};

const RESOLVER: usize = 0x01A4_9850;
const RESOLVER_LEN: usize = 0x192;
const RESOLVER_SHA: &str = "bd8e003a2e84ac6c368539c309c38febde338e4a45d97205b9bfc81f48187e06";
const HUD_VTABLE: u32 = 0x02C3_E85C;
const SPY_PAWN_VTABLE: u32 = 0x02AD_3234;
const CROUCH_PREDICATE: usize = 0x00BA_3E20;
const CROUCH_PREDICATE_SHA: &str = "f5d7de856dce12f0dcfe493d34e9920fef082cf8a7692c7643b72f25c1261416";
const ARMOR_VTABLE: u32 = 0x02AD_42AC;
const BOOTS: &[u8] = b"MPArmorBootsGhost";
// Keep each native event/equipment combination observable without per-frame spam.
static LOG_BUDGET: [AtomicU32; 21] = [const { AtomicU32::new(0) }; 21];
static CROUCH_READY: AtomicBool = AtomicBool::new(false);

// UMovementState dispatches 4/5 at 15ADFxx/15AE0xx; UMPMercSprint uses 5.
// AGI_MP_BATSVision's weapon callback B105A0 emits 0/1: keep native distance.
// Reload type2 is emitted/cleared by UReloadFireWeaponState (15B7464/15CE240).
// All spy reload events are suppressed. Other known non-weapon actions (3..6)
// are capped at 20 m before applying silent boots; unknown events stay native.
// Do not classify by range: distinct event types can have identical ranges.
// The native resolver writes -1 from 2994EB8 for disabled detection.
// Its HUD icon and highlight gates compare range < Euclidean distance;
// unlike zero, -1 also suppresses a source at exactly the observer position.
const ATS_NO_DETECTION: f32 = -1.0;
fn is_spy_role(flags: u32) -> bool {
    flags & ((1 << 18) | (1 << 19)) == 1 << 18
}
fn is_movement_event(kind: u32) -> bool {
    matches!(kind, 3..=6)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AtsEntry {
    words: [u32; 11],
}
type Resolver = unsafe extern "thiscall" fn(*mut u8, AtsEntry, *mut f32, *mut f32, *mut u32);
static_detour! {
    static RangeHook: unsafe extern "thiscall" fn(*mut u8, AtsEntry, *mut f32, *mut f32, *mut u32);
}

#[repr(C)]
#[derive(Default)]
struct MemoryInfo {
    base: usize,
    allocation: usize,
    allocation_protect: u32,
    size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualQuery(address: *const c_void, info: *mut MemoryInfo, length: usize) -> usize;
}
// Runs on the existing HUD/game thread; no saved pawn or equipment pointers.
unsafe fn readable(address: usize, size: usize) -> bool {
    if address < 0x10000 || size == 0 {
        return false;
    }
    let Some(end) = address.checked_add(size) else {
        return false;
    };
    let mut m = MemoryInfo::default();
    VirtualQuery(
        address as *const c_void,
        &mut m,
        std::mem::size_of::<MemoryInfo>(),
    ) != 0
        && m.state == 0x1000
        && m.protect & 0x101 == 0
        && m.protect & 0xee != 0
        && m.base.checked_add(m.size).is_some_and(|limit| end <= limit)
}
unsafe fn word(address: usize, offset: usize) -> Option<u32> {
    let address = address.checked_add(offset)?;
    readable(address, 4).then(|| (address as *const u32).read_unaligned())
}
fn exact_boot_name(bytes: &[u8]) -> bool {
    bytes == BOOTS || bytes.strip_suffix(&[0]) == Some(BOOTS)
}
unsafe fn is_silent_part(part: usize) -> bool {
    let Some(pointer) = word(part, 0x28) else {
        return false;
    };
    let Some(length) = word(part, 0x2c) else {
        return false;
    };
    let Some(capacity) = word(part, 0x30) else {
        return false;
    };
    // Accept only the exact asset identifier, never substring/camo/name hints.
    if !(BOOTS.len()..=BOOTS.len() + 1).contains(&(length as usize))
        || capacity < length
        || capacity > 128
    {
        return false;
    }
    let pointer = pointer as usize;
    if readable(pointer, length as usize) {
        let bytes = std::slice::from_raw_parts(pointer as *const u8, length as usize);
        if exact_boot_name(bytes) {
            return true;
        }
    }
    if !readable(pointer, length as usize * 2) {
        return false;
    }
    let bytes = std::slice::from_raw_parts(pointer as *const u8, length as usize * 2);
    bytes
        .chunks_exact(2)
        .enumerate()
        .all(|(i, p)| p[1] == 0 && p[0] == BOOTS.get(i).copied().unwrap_or(0))
}
unsafe fn silent_spy(pawn: usize) -> Option<bool> {
    let flags = word(pawn, 0x374)?;
    if !is_spy_role(flags) {
        return Some(false);
    }
    let armor = word(pawn, 0xcf4)? as usize;
    if word(armor, 0)? != ARMOR_VTABLE {
        return None;
    }
    let entries = word(armor, 0x50)? as usize;
    let count = word(armor, 0x54)? as usize;
    let capacity = word(armor, 0x58)? as usize;
    if count == 0
        || count > 12
        || capacity < count
        || capacity > 24
        || !readable(entries, count * 4)
    {
        return None;
    }
    for i in 0..count {
        let part = word(entries, i * 4)? as usize;
        if is_silent_part(part) {
            return Some(true);
        }
    }
    Some(false)
}
unsafe fn crouch_contract() -> bool {
    if word(SPY_PAWN_VTABLE as usize, 0x53c) != Some(CROUCH_PREDICATE as u32)
        || !readable(CROUCH_PREDICATE, 13)
    {
        return false;
    }
    let bytes = std::slice::from_raw_parts(CROUCH_PREDICATE as *const u8, 13);
    format!("{:x}", Sha256::digest(bytes)) == CROUCH_PREDICATE_SHA
}
unsafe fn apply_range(hud: *mut u8, entry: AtsEntry, native: f32) -> f32 {
    if !native.is_finite()
        || native < 0.0
        || word(hud as usize, 0) != Some(HUD_VTABLE)
        || entry.words[6] > 6
    {
        return native;
    }
    // Native producer assigns +14 only for a pawn; +10 also accepts non-pawns.
    if entry.words[4] == 0 || entry.words[4] != entry.words[5] {
        return native;
    }
    let shoes = silent_spy(entry.words[5] as usize);
    let movement = is_movement_event(entry.words[6]);
    // Locked DX11 PlayerIsInCrouch job calls spy pawn vtable+53C, whose
    // 00BA3E20 predicate returns (pawn+B24 == 1). Only UMovementState's
    // proven walking events 4/5 receive the silent-boots crouch exception.
    let silent_crouch_walk = CROUCH_READY.load(Ordering::Acquire)
        && matches!(entry.words[6], 4 | 5)
        && shoes == Some(true)
        && word(entry.words[5] as usize, 0) == Some(SPY_PAWN_VTABLE)
        && word(entry.words[5] as usize, 0xb24) == Some(1);
    let reload_suppressed =
        entry.words[6] == 2 && word(entry.words[5] as usize, 0x374).is_some_and(is_spy_role);
    let adjusted = if reload_suppressed {
        ATS_NO_DETECTION
    } else if movement && word(entry.words[5] as usize, 0x374).is_some_and(is_spy_role) {
        let limited = native.min(2000.0);
        if silent_crouch_walk {
            ATS_NO_DETECTION
        } else if shoes == Some(true) {
            limited * 0.5
        } else {
            limited
        }
    } else {
        native
    };
    let bucket = match shoes {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    let bucket = bucket * 7 + entry.words[6] as usize;
    if LOG_BUDGET[bucket].fetch_add(1, Ordering::Relaxed) < 4 {
        info!("SCBL_MOD_ATS_RANGE revision=C55 kind={} movement={movement} reload_suppressed={reload_suppressed} silent_shoes={shoes:?} native={native} effective={adjusted} source=per-pawn-native-range", entry.words[6]);
    }
    adjusted
}
fn resolve(hud: *mut u8, entry: AtsEntry, range: *mut f32, scale: *mut f32, kind: *mut u32) {
    unsafe {
        RangeHook.call(hud, entry, range, scale, kind); // exactly once, all original outputs
        if readable(range as usize, 4) {
            let native = range.read_unaligned();
            let adjusted = apply_range(hud, entry, native);
            if adjusted != native {
                range.write_unaligned(adjusted);
            }
        }
    }
}
pub(crate) unsafe fn init(world_raycast: usize) {
    CROUCH_READY.store(false, Ordering::Release);
    if world_raycast != 0x0058_9860 || !readable(RESOLVER, RESOLVER_LEN) {
        return;
    }
    let bytes = std::slice::from_raw_parts(RESOLVER as *const u8, RESOLVER_LEN);
    if format!("{:x}", Sha256::digest(bytes)) != RESOLVER_SHA {
        error!("SCBL_MOD_ATS_RANGE enabled=false reason=native-signature");
        return;
    }
    let target: Resolver = std::mem::transmute(RESOLVER);
    if let Err(e) = RangeHook
        .initialize(target, resolve)
        .and_then(|h| h.enable())
    {
        error!("SCBL_MOD_ATS_RANGE enabled=false reason={e:?}");
        return;
    }
    let crouch_ready = crouch_contract();
    CROUCH_READY.store(crouch_ready, Ordering::Release);
    info!("SCBL_MOD_ATS_CROUCH native_contract={crouch_ready} scope=silent-spy-walking-4-5");
    info!("SCBL_MOD_ATS_RANGE enabled=true revision=C55 boots=MPArmorBootsGhost range=non-gunshot-cap20m-boots-half spy_reload=disabled-independent-of-boots weapon0/1=native unknown=native hot_rules=false");
}
pub(crate) unsafe fn deinit() {
    CROUCH_READY.store(false, Ordering::Release);
    if RangeHook.is_enabled() {
        let _ = RangeHook.disable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    struct Gear {
        pawn: Vec<u8>,
        armor: Vec<u8>,
        part: Vec<u8>,
        entries: Box<[u32; 1]>,
        name: Vec<u8>,
    }
    impl Gear {
        fn new(wide: bool) -> Self {
            let mut s = Self {
                pawn: vec![0; 0xcf8],
                armor: vec![0; 0x60],
                part: vec![0; 0x34],
                entries: Box::new([0]),
                name: Vec::new(),
            };
            s.name = if wide {
                BOOTS
                    .iter()
                    .chain([0].iter())
                    .flat_map(|b| [*b, 0])
                    .collect()
            } else {
                BOOTS.iter().chain([0].iter()).copied().collect()
            };
            put(&mut s.pawn, 0x374, 1 << 18);
            put(&mut s.pawn, 0xcf4, s.armor.as_ptr() as u32);
            put(&mut s.armor, 0, ARMOR_VTABLE);
            put(&mut s.armor, 0x50, s.entries.as_ptr() as u32);
            put(&mut s.armor, 0x54, 1);
            put(&mut s.armor, 0x58, 1);
            s.entries[0] = s.part.as_ptr() as u32;
            put(&mut s.part, 0x28, s.name.as_ptr() as u32);
            put(&mut s.part, 0x2c, BOOTS.len() as u32 + 1);
            put(&mut s.part, 0x30, BOOTS.len() as u32 + 1);
            s
        }
        fn entry(&self) -> AtsEntry {
            let mut e = AtsEntry::default();
            e.words[4] = self.pawn.as_ptr() as u32;
            e.words[5] = e.words[4];
            e.words[6] = 5; // verified movement event, not weapon default 0
            e
        }
    }
    #[test]
    fn exact_asset_not_single_player_whisper_or_camo() {
        assert!(exact_boot_name(BOOTS));
        assert!(!exact_boot_name(b"WhisperBoots"));
        assert!(!exact_boot_name(b"Black-MPArmorBootsGhost"));
        assert!(!exact_boot_name(b"MPArmorBootsIntelScout"));
    }
    #[test]
    fn current_pawn_equipment_changes_without_cached_identity() {
        unsafe {
            for wide in [false, true] {
                let mut s = Gear::new(wide);
                assert_eq!(silent_spy(s.pawn.as_ptr() as usize), Some(true));
                s.name[0] = b'X';
                assert_eq!(silent_spy(s.pawn.as_ptr() as usize), Some(false));
                put(&mut s.pawn, 0x374, 1 << 19);
                assert_eq!(silent_spy(s.pawn.as_ptr() as usize), Some(false));
                put(&mut s.pawn, 0x374, 1 << 18);
                put(&mut s.armor, 0x54, 100);
                assert_eq!(silent_spy(s.pawn.as_ptr() as usize), None);
            }
        }
    }
    #[test]
    fn per_target_distance_not_global_or_squared_or_event_synthesis() {
        unsafe {
            let mut hud = [HUD_VTABLE, 0];
            let a = Gear::new(false);
            let mut b = Gear::new(true);
            b.name[0] = b'X';
            for _ in 0..10 {
                assert_eq!(
                    apply_range(hud.as_mut_ptr().cast(), a.entry(), 2000.0),
                    1000.0
                );
                assert_eq!(
                    apply_range(hud.as_mut_ptr().cast(), b.entry(), 2000.0),
                    2000.0
                );
            }
            let mut e = a.entry();
            e.words[5] = 0;
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 2000.0), 2000.0);
            hud[0] = 0;
            assert_eq!(
                apply_range(hud.as_mut_ptr().cast(), a.entry(), 2000.0),
                2000.0
            );
        }
    }
    #[test]
    fn movement_only_even_when_gunshot_and_movement_have_identical_native_ranges() {
        unsafe {
            let mut hud = [HUD_VTABLE, 0];
            let gear = Gear::new(false);
            for native in [1.0, 750.0, 1500.0, 2000.0] {
                for event in [0, 1, 2, 3, 4, 5, 6, 7, u32::MAX] {
                    let mut entry = gear.entry();
                    entry.words[6] = event;
                    let expected = if event == 2 {
                        ATS_NO_DETECTION
                    } else if matches!(event, 3..=6) {
                        native * 0.5
                    } else {
                        native
                    };
                    assert_eq!(
                        apply_range(hud.as_mut_ptr().cast(), entry, native),
                        expected
                    );
                }
            }
            println!(
                "ATS_SCOPE actions3/4/5/6=1500->750; weapon0/1=1500->1500; reload2=-1; unknown=native"
            );
        }
    }
    #[test]
    fn event_and_equipment_are_recomputed_without_cross_event_sticky_state() {
        unsafe {
            let mut hud = [HUD_VTABLE, 0];
            let mut gear = Gear::new(true);
            for _ in 0..20 {
                let mut entry = gear.entry();
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 750.0);
                entry.words[6] = 0;
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 1500.0);
                entry.words[6] = 5;
                gear.name[0] = b'X';
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 1500.0);
                gear.name[0] = BOOTS[0];
            }
        }
    }
    #[test]
    fn all_spy_reload_is_disabled_with_any_boots_or_unavailable_armor() {
        unsafe {
            let mut hud = [HUD_VTABLE, 0];
            for wide in [false, true] {
                for equipment in 0..3 {
                    let mut gear = Gear::new(wide);
                    if equipment == 1 {
                        gear.name[0] = b'X';
                    }
                    if equipment == 2 {
                        put(&mut gear.pawn, 0xcf4, 0);
                    }
                    let mut entry = gear.entry();
                    entry.words[6] = 2;
                    for native in [0.0, 750.0, 1500.0, 2000.0] {
                        let actual = apply_range(hud.as_mut_ptr().cast(), entry, native);
                        assert_eq!(actual, ATS_NO_DETECTION);
                        for distance in [0.0, 0.001, 750.0, 3000.0] {
                            // Same finite comparison as both retail distance gates.
                            assert!(actual < distance);
                        }
                    }
                    entry.words[6] = 0;
                    assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 1500.0);
                    entry.words[6] = 2;
                    assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), -1.0);
                }
            }
            println!("ATS_RELOAD all-spies silent/ordinary/unknown-armor=1500->-1; both native distance gates skip even distance0; gunshot=1500");
        }
    }
    #[test]
    fn reload_does_not_suppress_merc_unknown_role_or_invalid_source() {
        unsafe {
            let mut hud = [HUD_VTABLE, 0];
            let mut gear = Gear::new(false);
            let mut entry = gear.entry();
            entry.words[6] = 2;
            for flags in [0, 1 << 19, (1 << 18) | (1 << 19)] {
                put(&mut gear.pawn, 0x374, flags);
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 1500.0);
            }
            put(&mut gear.pawn, 0x374, 1 << 18);
            entry.words[4] = 0;
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 1500.0);
            entry.words[4] = entry.words[5];
            entry.words[5] = 0;
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), entry, 1500.0), 1500.0);
        }
    }
    #[test]
    fn abi_layout_and_invalid_memory_fail_closed_to_native() {
        unsafe {
            assert_eq!(std::mem::size_of::<AtsEntry>(), 44);
            assert_eq!(std::mem::size_of::<MemoryInfo>(), 28);
            assert_eq!(silent_spy(0), None);
            assert!(!readable(usize::MAX, 4));
        }
    }
    #[test]
    #[ignore = "requires x86 C++ ATS ABI fixture; run explicitly"]
    fn cpp_by_value_thiscall_detour_keeps_stack_and_other_outputs() {
        unsafe {
            use windows::core::PCSTR;
            use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
            let path =
                std::ffi::CString::new(std::env::var("SCBL_ATS_ABI_FIXTURE").unwrap()).unwrap();
            let module = LoadLibraryA(PCSTR(path.as_ptr().cast())).unwrap();
            let getter: unsafe extern "system" fn() -> usize = std::mem::transmute(
                GetProcAddress(module, PCSTR(b"fixture_entry\0".as_ptr())).unwrap(),
            );
            let invoke: unsafe extern "system" fn(
                usize,
                *mut u8,
                AtsEntry,
                *mut f32,
                *mut f32,
                *mut u32,
            ) = std::mem::transmute(
                GetProcAddress(module, PCSTR(b"fixture_invoke\0".as_ptr())).unwrap(),
            );
            let address = getter();
            let target: Resolver = std::mem::transmute(address);
            let mut hud = [HUD_VTABLE, 0];
            let mut gear = Gear::new(false);
            let (mut range, mut scale, mut kind) = (0.0, 0.0, 0);
            invoke(
                address,
                hud.as_mut_ptr().cast(),
                gear.entry(),
                &mut range,
                &mut scale,
                &mut kind,
            );
            assert_eq!((range, scale, kind), (2000.0, 7.25, 77));
            RangeHook
                .initialize(target, resolve)
                .unwrap()
                .enable()
                .unwrap();
            for _ in 0..1000 {
                invoke(
                    address,
                    hud.as_mut_ptr().cast(),
                    gear.entry(),
                    &mut range,
                    &mut scale,
                    &mut kind,
                );
                assert_eq!((range, scale, kind), (1000.0, 7.25, 77));
            }
            for event in 0..=6 {
                let mut entry = gear.entry();
                entry.words[6] = event;
                invoke(
                    address,
                    hud.as_mut_ptr().cast(),
                    entry,
                    &mut range,
                    &mut scale,
                    &mut kind,
                );
                let expected = if event == 2 {
                    ATS_NO_DETECTION
                } else if matches!(event, 3..=6) {
                    1000.0
                } else {
                    2000.0
                };
                assert_eq!((range, scale, kind), (expected, 7.25, 77));
            }
            gear.name[0] = b'X';
            invoke(
                address,
                hud.as_mut_ptr().cast(),
                gear.entry(),
                &mut range,
                &mut scale,
                &mut kind,
            );
            assert_eq!((range, scale, kind), (2000.0, 7.25, 77));
            let mut reload = gear.entry();
            reload.words[6] = 2;
            invoke(
                address,
                hud.as_mut_ptr().cast(),
                reload,
                &mut range,
                &mut scale,
                &mut kind,
            );
            assert_eq!((range, scale, kind), (-1.0, 7.25, 77));
            assert_eq!(hud[1], 1010);
            RangeHook.disable().unwrap();
            invoke(
                address,
                hud.as_mut_ptr().cast(),
                gear.entry(),
                &mut range,
                &mut scale,
                &mut kind,
            );
            assert_eq!((range, scale, kind), (2000.0, 7.25, 77));
            println!("ATS_ABI_PROOF cpp=thiscall entry-by-value=44 stack-pop=56 original-once=1011 movement=2000->1000 weapon0/1=2000 reload2=-1 ordinary-boots-reload=-1 other3/6=1000 scale=7.25 kind=77 rollback=2000");
        }
    }
    #[test]
    fn c55_caps_all_known_actions_not_shots_and_keeps_native_disable() {
        unsafe {
            let mut hud = [HUD_VTABLE, 0];
            let mut g = Gear::new(false);
            for shoes in [true, false] {
                g.name[0] = if shoes { BOOTS[0] } else { b'X' };
                for k in 0..=6 {
                    let mut e = g.entry();
                    e.words[6] = k;
                    for range in [0.0f32, 1500.0, 2000.0, 4000.0] {
                        let expected = match k {
                            0 | 1 => range,
                            2 => -1.0,
                            _ => range.min(2000.0) * if shoes { 0.5 } else { 1.0 },
                        };
                        assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, range), expected);
                    }
                }
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), g.entry(), -1.0), -1.0);
            }
            put(&mut g.pawn, 0x374, 1 << 19);
            assert_eq!(
                apply_range(hud.as_mut_ptr().cast(), g.entry(), 4000.0),
                4000.0
            );
        }
    }
    #[test]
    fn silent_boots_crouch_only_disables_verified_walking_events() {
        unsafe {
            struct ResetCrouch;
            impl Drop for ResetCrouch {
                fn drop(&mut self) { CROUCH_READY.store(false, Ordering::Release); }
            }
            let _reset = ResetCrouch;
            CROUCH_READY.store(true, Ordering::Release);
            let mut hud = [HUD_VTABLE, 0];
            let mut g = Gear::new(false);
            put(&mut g.pawn, 0, SPY_PAWN_VTABLE);
            put(&mut g.pawn, 0xb24, 1);
            CROUCH_READY.store(false, Ordering::Release);
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), g.entry(), 1500.0), 750.0);
            CROUCH_READY.store(true, Ordering::Release);
            for kind in [4, 5] {
                let mut e = g.entry();
                e.words[6] = kind;
                for (stance, expected) in [(1, -1.0), (0, 750.0), (3, 750.0), (u32::MAX, 750.0), (1, -1.0)] {
                    put(&mut g.pawn, 0xb24, stance);
                    assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 1500.0), expected);
                }
            }
            put(&mut g.pawn, 0xb24, 1);
            for kind in [3, 6] {
                let mut e = g.entry();
                e.words[6] = kind;
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 4000.0), 1000.0);
            }
            for kind in [0, 1] {
                let mut e = g.entry();
                e.words[6] = kind;
                assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 1500.0), 1500.0);
            }
            let mut e = g.entry();
            e.words[6] = 2;
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 1500.0), -1.0);
            e.words[6] = 5;
            g.name[0] = b'X';
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 4000.0), 2000.0);
            g.name[0] = BOOTS[0];
            put(&mut g.pawn, 0x374, 1 << 19);
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 4000.0), 4000.0);
            put(&mut g.pawn, 0x374, 1 << 18);
            put(&mut g.pawn, 0, 0);
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 1500.0), 750.0);
            e.words[5] = 0;
            assert_eq!(apply_range(hud.as_mut_ptr().cast(), e, 1500.0), 1500.0);
        }
    }
}
