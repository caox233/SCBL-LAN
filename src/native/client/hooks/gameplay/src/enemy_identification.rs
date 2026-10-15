//! DX11 enemy name/health policy. MOD-only host; no equipment/EMP/weapon writes.
//! Native evidence: HudMPPlayerNames target update 1A8A170 (thiscall, ret0c),
//! eligibility 1A50C20 (thiscall, ret0c); authoritative native tag flag +1a08.
//! The private RX eligibility copy preserves every native instruction except
//! its vision-mode shortcut and tag-blocker branch; all 20 calls are relocated.
//! A separate guarded two-byte camera patch removes the same equipment veto.
//! Gear flags and executable files remain untouched; failed code restoration stays owned.
use std::cell::Cell;
use std::ffi::c_void;
use std::io::Read;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

use retour::static_detour;
use sha2::{Digest, Sha256};
use tracing::{error, info};

const UPDATE: usize = 0x01A8_A170;
const ELIGIBILITY: usize = 0x01A5_0C20;
const ELIGIBILITY_LEN: usize = 0x5A3;
const BMS_EQUIPPED: usize = 0x0185_AFC0;
// Camera target classification checks the same ArmorSet+7c bit2 BEFORE the
// HUD eligibility call. Leaving this JNE active makes aiming_at false and
// defeats the HUD-only bypass. Remove only this equipment veto in MOD;
// native faction, raycast, cloak and reticle-color logic continue unchanged.
const TARGET_FILTER: usize = 0x00D8_F202;
const TARGET_FILTER_BRANCH: usize = TARGET_FILTER + 14;
const TARGET_FILTER_SIGNATURE: [u8; 16] = [
    0x8b, 0x81, 0xf4, 0x0c, 0, 0, 0x85, 0xc0, 0x74, 0x06, 0xf6, 0x40, 0x7c, 0x04, 0x75, 0x1e,
];
static TARGET_FILTER_PATCHED: AtomicBool = AtomicBool::new(false);

const ELIGIBILITY_SHA: &str = "2e6c12733cadff492fa6a22abc9bf83100a66117458e8265a4618b50ee78f071";
const RELATIVE_CALLS: &[(usize, u32)] = &[
    (79, 5448208),
    (127, 15059360),
    (179, 11162016),
    (205, 15115792),
    (223, 11191952),
    (235, 11191952),
    (242, 15115792),
    (299, 15116416),
    (328, 15366352),
    (373, 27527008),
    (411, 13734288),
    (456, 27527008),
    (487, 15256112),
    (522, 27527008),
    (1007, 15059360),
    (1024, 13694160),
    (1039, 13694384),
    (1055, 13694384),
    (1129, 27527008),
    (1425, 27527008),
];
type Eligibility = unsafe extern "thiscall" fn(*mut u8, *mut u8, *mut u8, f32) -> u8;

static_detour! {
    static TargetUpdate: unsafe extern "thiscall" fn(*mut u8, *mut u8, *mut u8, f32);
    static TagEligibility: unsafe extern "thiscall" fn(*mut u8, *mut u8, *mut u8, f32) -> u8;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetTickCount64() -> u64;
    fn GetCurrentProcess() -> *mut c_void;
    fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protection: u32) -> *mut c_void;
    fn VirtualProtect(address: *mut c_void, size: usize, protection: u32, old: *mut u32) -> i32;
    fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    fn FlushInstructionCache(process: *mut c_void, address: *const c_void, size: usize) -> i32;
    fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, path: *mut u16, capacity: u32) -> u32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Mod,
    Native,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Spy,
    Merc,
    Unknown,
}
#[derive(Clone, Copy, Default)]
struct Frame {
    hud: usize,
    entry: usize,
    target: usize,
    enforce: bool,
    permit: bool,
}
impl Frame {
    fn matches(self, hud: usize, entry: usize, target: usize) -> bool {
        self.enforce && self.hud == hud && self.entry == entry && self.target == target
    }
}
fn with_frame<T>(frame: Frame, work: impl FnOnce() -> T) -> T {
    struct Restore(Frame);
    impl Drop for Restore {
        fn drop(&mut self) {
            FRAME.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(FRAME.with(|s| s.replace(frame)));
    work()
}
thread_local! {
    static FRAME: Cell<Frame> = Cell::new(Frame::default());
    static LAST_DECISION: Cell<Option<(bool, bool, bool, bool)>> = const { Cell::new(None) };
}
static CLONE: AtomicUsize = AtomicUsize::new(0);
static LOG_BUDGET: AtomicU32 = AtomicU32::new(0);
static LIVE: Mutex<LiveConfig> = Mutex::new(LiveConfig {
    path: None,
    profile: Profile::Mod,
    last_poll: 0,
});

struct LiveConfig {
    path: Option<PathBuf>,
    profile: Profile,
    last_poll: u64,
}

fn role(flags: u32) -> Role {
    match (flags & (1 << 18) != 0, flags & (1 << 19) != 0) {
        (true, false) => Role::Spy,
        (false, true) => Role::Merc,
        _ => Role::Unknown,
    }
}
fn authorized(viewer: Role, target: Role, equipped: bool, intel: bool) -> bool {
    let role_id = |role| match role {
        Role::Unknown => 0,
        Role::Spy => 1,
        Role::Merc => 2,
    };
    crate::live_rules::nameplate(crate::rule_contract::NameplateInput {
        viewer_role: role_id(viewer),
        target_role: role_id(target),
        bms_equipped: u32::from(equipped),
        intel_marked: u32::from(intel),
    })
}
fn parse_profile(text: &str) -> Option<Profile> {
    match text.trim() {
        "schema=1\nprofile=mod" | "schema=1\r\nprofile=mod" => Some(Profile::Mod),
        "schema=1\nprofile=native" | "schema=1\r\nprofile=native" => Some(Profile::Native),
        _ => None,
    }
}
fn trace(message: impl FnOnce() -> String) {
    if LOG_BUDGET
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
            (x < 256).then_some(x + 1)
        })
        .is_ok()
    {
        info!("SCBL_MOD_NAMEPLATES {}", message());
    }
}

// Called only within native HUD / Intel callbacks, whose objects the original
// routine already dereferences synchronously. No raw pointers survive a frame.
unsafe fn dword(p: *const u8, offset: usize) -> u32 {
    if p.is_null() {
        0
    } else {
        p.add(offset).cast::<u32>().read_unaligned()
    }
}
unsafe fn pointer(p: *const u8, offset: usize) -> *mut u8 {
    dword(p, offset) as *mut u8
}
unsafe fn byte(p: *const u8, offset: usize) -> u8 {
    if p.is_null() {
        0
    } else {
        p.add(offset).read()
    }
}
unsafe fn local_controller() -> *mut u8 {
    let get: unsafe extern "cdecl" fn() -> *mut u8 = std::mem::transmute(0x0053_2210usize);
    let engine = get();
    let table = pointer(engine, 0);
    let address = dword(table, 0x138);
    if !(0x0040_1000..0x0297_2000).contains(&address) {
        return std::ptr::null_mut();
    }
    let controller: unsafe extern "thiscall" fn(*mut u8) -> *mut u8 =
        std::mem::transmute(address as usize);
    controller(engine)
}
unsafe fn aiming_at(controller: *mut u8, target: *mut u8) -> bool {
    if controller.is_null() || target.is_null() {
        return false;
    }
    let camera: unsafe extern "thiscall" fn(*mut u8) -> *mut u8 =
        std::mem::transmute(0x00E5_C9A0usize);
    let view = camera(controller);
    !view.is_null() && byte(pointer(view, 0x61c), 0xbe4) != 0 && pointer(view, 0x748) == target
}
unsafe fn same_team(a: *mut u8, b: *mut u8) -> bool {
    if a.is_null() || b.is_null() {
        return false;
    }
    let compare: unsafe extern "thiscall" fn(*mut u8, *mut u8) -> u32 =
        std::mem::transmute(0x008B_8370usize);
    compare(a, b) != 0
}
// B5EA00 / AD8620 set +1a08 bit 0x800; B5ED10 clears it.
// Read the native replicated presentation state on this HUD callback, never
// maintain an Intel-only timer/cache or emit any mark/damage/network action.
// This also covers replica pawns where the authority's source array is absent.
unsafe fn native_marked(target: *mut u8) -> bool {
    crate::equipment_native::word(target as usize, 0x1a08).is_some_and(|flags| flags & 0x800 != 0)
}
unsafe fn hide(entry: *mut u8) {
    if entry.is_null() {
        return;
    }
    entry.add(0x28).write(0);
    entry.add(0x2a).write(0);
    entry.add(0x2c).cast::<f32>().write_unaligned(0.0);
    let widget = pointer(entry, 0);
    let table = pointer(widget, 0);
    let address = dword(table, 0x108);
    if !widget.is_null() && (0x0040_1000..0x0297_2000).contains(&address) {
        let set: unsafe extern "thiscall" fn(*mut u8, u8) = std::mem::transmute(address as usize);
        set(widget, 0);
    }
}
fn current_profile() -> Profile {
    if !cfg!(any(test, feature = "dev-hot-reload")) {
        return Profile::Mod;
    }
    let now = unsafe { GetTickCount64() };
    let Ok(mut live) = LIVE.lock() else {
        return Profile::Mod;
    };
    if now.saturating_sub(live.last_poll) >= 1000 {
        live.last_poll = now;
        if let Some(path) = &live.path {
            match std::fs::File::open(path) {
                Ok(file) => {
                    let mut text = String::new();
                    if file.take(129).read_to_string(&mut text).is_ok() && text.len() <= 128 {
                        if let Some(profile) = parse_profile(&text) {
                            if live.profile != profile {
                                live.profile = profile;
                                info!("SCBL_MOD_NAMEPLATES profile={profile:?} source=live-file");
                            }
                        } // Partial/unknown edits retain the last valid policy.
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    live.profile = Profile::Mod;
                }
                Err(_) => {} // No implicit fallback to unrestricted native mode.
            }
        }
    }
    live.profile
}
fn update(hud: *mut u8, entry: *mut u8, target: *mut u8, dt: f32) {
    if current_profile() == Profile::Native || entry.is_null() || target.is_null() {
        unsafe { TargetUpdate.call(hud, entry, target, dt) };
        return;
    }
    let controller = unsafe { local_controller() };
    let viewer = unsafe { pointer(controller, 0x520) };
    let friendly = unsafe { same_team(viewer, target) };
    if friendly || viewer == target {
        unsafe { TargetUpdate.call(hud, entry, target, dt) };
        return;
    }
    let vrole = role(unsafe { dword(viewer, 0x374) });
    let trole = role(unsafe { dword(target, 0x374) });
    let equipped = if vrole == Role::Merc {
        let test: unsafe extern "thiscall" fn(*mut u8) -> u8 =
            unsafe { std::mem::transmute(BMS_EQUIPPED) };
        unsafe { test(std::ptr::null_mut()) != 0 }
    } else {
        false
    };
    let intel = unsafe { native_marked(target) };
    let aimed = unsafe { aiming_at(controller, target) };
    let permit = authorized(vrole, trole, equipped, intel) && aimed;
    let frame = Frame {
        hud: hud as usize,
        entry: entry as usize,
        target: target as usize,
        enforce: true,
        permit,
    };
    with_frame(frame, || unsafe {
        TargetUpdate.call(hud, entry, target, dt)
    });
    // Authority targets have a native visibility shortcut independent of the
    // eligibility return. Suppress the complete name+health container as well.
    if !permit {
        unsafe { hide(entry) };
    }
    let state = (equipped, intel, aimed, permit);
    if LAST_DECISION.with(|s| s.replace(Some(state)) != Some(state)) {
        trace(|| {
            format!("viewer={vrole:?} target={trole:?} bms_equipped={equipped} native_marked={intel} aimed={aimed} permit={permit}")
        });
    }
}
fn eligibility(hud: *mut u8, entry: *mut u8, target: *mut u8, dt: f32) -> u8 {
    let frame = FRAME.with(Cell::get);
    if !frame.matches(hud as usize, entry as usize, target as usize) {
        return unsafe { TagEligibility.call(hud, entry, target, dt) };
    }
    if !frame.permit {
        unsafe { hide(entry) };
        return 0;
    }
    let address = CLONE.load(Ordering::Acquire);
    if address == 0 {
        return 0;
    }
    let native: Eligibility = unsafe { std::mem::transmute(address) };
    let result = unsafe { native(hud, entry, target, dt) };
    // The original player updater uses +2a to authorize both health bars. Keep
    // its dead-target, screen-space and occlusion checks; never force visibility.
    if result != 0 {
        unsafe { entry.add(0x2a).write(1) };
    }
    result
}

fn relocated_eligibility(input: &[u8], destination: u32) -> Option<Vec<u8>> {
    if input.len() != ELIGIBILITY_LEN || format!("{:x}", Sha256::digest(input)) != ELIGIBILITY_SHA {
        return None;
    }
    let mut bytes = input.to_vec();
    for &(offset, target) in RELATIVE_CALLS {
        if bytes[offset] != 0xe8 {
            return None;
        }
        let relative = i32::from_le_bytes(bytes[offset + 1..offset + 5].try_into().ok()?);
        if (ELIGIBILITY as u32)
            .wrapping_add(offset as u32 + 5)
            .wrapping_add(relative as u32)
            != target
        {
            return None;
        }
        let delta = target.wrapping_sub(destination.wrapping_add(offset as u32 + 5));
        bytes[offset + 1..offset + 5].copy_from_slice(&delta.to_le_bytes());
    }
    // Skip only the active-vision alternate path, not normal aim/fade rules.
    if bytes[0x11c..0x122] != [0x0f, 0x84, 0xb9, 0, 0, 0] {
        return None;
    }
    bytes[0x11c..0x11e].copy_from_slice(&[0x90, 0xe9]);
    // Suppress only CF4+7c bit2's display veto; retain target+1a54 veto.
    if bytes[0x39f..0x3a1] != [0x75, 0x0c] {
        return None;
    }
    bytes[0x39f..0x3a1].copy_from_slice(&[0x90, 0x90]);
    Some(bytes)
}
unsafe fn allocate_eligibility() -> Option<usize> {
    let source = std::slice::from_raw_parts(ELIGIBILITY as *const u8, ELIGIBILITY_LEN);
    // Validate before allocating; unknown retail code is never executed.
    relocated_eligibility(source, ELIGIBILITY as u32)?;
    let memory = VirtualAlloc(std::ptr::null_mut(), ELIGIBILITY_LEN, 0x3000, 0x04);
    if memory.is_null() {
        return None;
    }
    let Some(code) = relocated_eligibility(source, memory as u32) else {
        VirtualFree(memory, 0, 0x8000);
        return None;
    };
    std::ptr::copy_nonoverlapping(code.as_ptr(), memory.cast(), code.len());
    let mut old = 0;
    if VirtualProtect(memory, code.len(), 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), memory, code.len()) == 0
    {
        VirtualFree(memory, 0, 0x8000);
        return None;
    }
    Some(memory as usize)
}
fn target_filter_transition(current: [u8; 2], enabled: bool) -> Option<[u8; 2]> {
    let original = [0x75, 0x1e];
    let modified = [0x90, 0x90];
    if current != original && current != modified {
        return None; // Never overwrite another patch or unknown retail bytes.
    }
    Some(if enabled { modified } else { original })
}
unsafe fn target_filter_bytes() -> Option<[u8; 2]> {
    let pointer = TARGET_FILTER_BRANCH as *mut u8;
    crate::equipment_native::readable(pointer as usize, 2)
        .then(|| pointer.cast::<u16>().read().to_le_bytes())
}
unsafe fn set_target_filter_bypass(enabled: bool) -> bool {
    let pointer = TARGET_FILTER_BRANCH as *mut u8;
    let Some(current) = target_filter_bytes() else {
        return false;
    };
    let Some(wanted) = target_filter_transition(current, enabled) else {
        return false;
    };
    if current != wanted {
        let Ok(guard) =
            region::protect_with_handle(pointer, 2, region::Protection::READ_WRITE_EXECUTE)
        else {
            return false;
        };
        if target_filter_bytes() != Some(current) {
            return false;
        }
        // Aligned two-byte branch in serialized startup/shutdown, not a live-edit
        // or hot-unload API. The owner journal alone handles failed restoration.
        pointer.cast::<u16>().write(u16::from_le_bytes(wanted));
        drop(guard);
    }
    // Original bytes may already be present after an earlier cache-sync failure.
    // They are not a completed rollback until the cache has also been synced.
    FlushInstructionCache(GetCurrentProcess(), pointer.cast(), 2) != 0
        && target_filter_bytes() == Some(wanted)
}
fn restore_target_filter_with(owned: &AtomicBool, restore: impl FnOnce() -> bool) -> bool {
    if !owned.load(Ordering::Acquire) {
        return true;
    }
    if !restore() {
        return false;
    }
    owned.store(false, Ordering::Release);
    true
}
unsafe fn install_target_filter() -> bool {
    // Recheck after detour setup; do not adopt an already-modified branch.
    if TARGET_FILTER_PATCHED.load(Ordering::Acquire)
        || target_filter_bytes() != Some([0x75, 0x1e])
    {
        return false;
    }
    TARGET_FILTER_PATCHED.store(true, Ordering::Release);
    set_target_filter_bypass(true)
}
fn live_path() -> Option<PathBuf> {
    let mut module = std::ptr::null_mut();
    let mut path = [0u16; 32768];
    unsafe {
        if GetModuleHandleExW(6, live_path as *const () as *const u16, &mut module) == 0 {
            return None;
        }
        let n = GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) as usize;
        if n == 0 || n >= path.len() {
            return None;
        }
        let mut path = PathBuf::from(std::ffi::OsString::from_wide(&path[..n]));
        path.set_file_name("scbl_nameplates.live");
        Some(path)
    }
}
pub(super) unsafe fn init() {
    if TARGET_FILTER_PATCHED.load(Ordering::Acquire) || CLONE.load(Ordering::Acquire) != 0 {
        error!("SCBL_MOD_NAMEPLATES enabled=false reason=active-or-pending-restoration");
        return;
    }
    let signatures: &[(usize, &[u8])] = &[
        (TARGET_FILTER, &TARGET_FILTER_SIGNATURE),
        (
            UPDATE,
            &[
                0x55, 0x8b, 0xec, 0x81, 0xec, 0xa4, 0, 0, 0, 0x89, 0x4d, 0x8c, 0x83, 0x7d, 0x0c, 0,
            ],
        ),
        (
            BMS_EQUIPPED,
            &[
                0x55, 0x8b, 0xec, 0x51, 0x53, 0x56, 0x57, 0xe8, 0x44, 0x72, 0xcd, 0xfe,
            ],
        ),
    ];
    if signatures
        .iter()
        .any(|(a, s)| std::slice::from_raw_parts(*a as *const u8, s.len()) != *s)
    {
        error!("SCBL_MOD_NAMEPLATES enabled=false reason=signature");
        return;
    }
    let Some(code) = allocate_eligibility() else {
        error!("SCBL_MOD_NAMEPLATES enabled=false reason=native-copy");
        return;
    };
    CLONE.store(code, Ordering::Release);
    if let Ok(mut live) = LIVE.lock() {
        live.path = live_path();
    }
    let result = TagEligibility
        .initialize(
            std::mem::transmute::<usize, Eligibility>(ELIGIBILITY),
            eligibility,
        )
        .and_then(|h| h.enable())
        .and_then(|_| {
            TargetUpdate.initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(*mut u8, *mut u8, *mut u8, f32),
                >(UPDATE),
                update,
            )
        })
        .and_then(|h| h.enable());
    if let Err(e) = result {
        deinit();
        error!("SCBL_MOD_NAMEPLATES enabled=false reason={e:?}");
        return;
    }
    if !install_target_filter() {
        deinit();
        error!("SCBL_MOD_NAMEPLATES enabled=false reason=target-filter-patch");
        return;
    }
    info!("SCBL_MOD_NAMEPLATES enabled=true revision=38 profile=Mod bms=equipped-only marks=native-current-flag target_filter=equipment-veto-removed reticle=native-independent live_config=scbl_nameplates.live");
}
pub(super) unsafe fn deinit() {
    if !restore_target_filter_with(&TARGET_FILTER_PATCHED, || set_target_filter_bypass(false)) {
        error!(
            "SCBL_MOD_NAMEPLATES target_filter_restore=false reason=unknown-bytes-protection-or-cache-sync pending=true"
        );
    }
    // Only plugin shutdown at process teardown, never a hot-unload mechanism.
    if TargetUpdate.is_enabled() {
        let _ = TargetUpdate.disable();
    }
    if TagEligibility.is_enabled() {
        let _ = TagEligibility.disable();
    }
    // Retire the immutable RX page for the remainder of this process. A native
    // callback may still be returning through it when detours are disabled.
    // The OS reclaims it at exit; this plugin is not a hot-unloadable module.
    CLONE.store(0, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_failure_keeps_ownership_until_successful_retry() {
        let owned = AtomicBool::new(true);
        let calls = Cell::new(0);
        assert!(!restore_target_filter_with(&owned, || { calls.set(1); false }));
        assert!(owned.load(Ordering::Acquire));
        assert!(restore_target_filter_with(&owned, || { calls.set(calls.get() + 1); true }));
        assert!(!owned.load(Ordering::Acquire));
        assert_eq!(calls.get(), 2);
    }
    #[test]
    fn unowned_restore_never_calls_writer() {
        let owned = AtomicBool::new(false);
        assert!(restore_target_filter_with(&owned, || panic!("unowned branch")));
    }
    #[test]
    fn repeated_unknown_restore_keeps_pending_record() {
        let owned = AtomicBool::new(true);
        for _ in 0..3 {
            assert!(!restore_target_filter_with(&owned, || false));
            assert!(owned.load(Ordering::Acquire));
        }
    }
    #[test]
    fn restore_unwind_does_not_drop_ownership() {
        let owned = AtomicBool::new(true);
        assert!(std::panic::catch_unwind(|| {
            restore_target_filter_with(&owned, || panic!("restore leaf failure"))
        }).is_err());
        assert!(owned.load(Ordering::Acquire));
    }
    #[test]
    fn native_target_filter_bypass_and_rollback_touch_only_equipment_branch() {
        let mut bytes = TARGET_FILTER_SIGNATURE;
        let original = bytes;
        let next = target_filter_transition([bytes[14], bytes[15]], true).unwrap();
        bytes[14..].copy_from_slice(&next);
        assert_eq!(&bytes[..14], &original[..14]);
        assert_eq!(&bytes[14..], &[0x90, 0x90]);
        let next = target_filter_transition([bytes[14], bytes[15]], false).unwrap();
        bytes[14..].copy_from_slice(&next);
        assert_eq!(bytes, original);
        assert_eq!(TARGET_FILTER_BRANCH, 0x00d8_f210);
        assert_eq!(TARGET_FILTER_BRANCH % 2, 0);
    }
    #[test]
    fn unknown_target_filter_bytes_fail_closed_and_idempotent_states_are_preserved() {
        assert_eq!(target_filter_transition([0x74, 0x1e], true), None);
        assert_eq!(target_filter_transition([0xe9, 0x00], false), None);
        assert_eq!(
            target_filter_transition([0x90, 0x90], true),
            Some([0x90, 0x90])
        );
        assert_eq!(
            target_filter_transition([0x75, 0x1e], false),
            Some([0x75, 0x1e])
        );
    }
    #[test]
    fn reticle_pants_veto_is_removed_independently_of_nameplate_permissions() {
        // This operation has no BMS/Intel parameter: reticle uses native enemy
        // targeting for everyone. Names/health remain governed by authorized.
        assert_eq!(
            target_filter_transition([0x75, 0x1e], true),
            Some([0x90, 0x90])
        );
        assert!(!authorized(Role::Merc, Role::Spy, false, false));
        assert!(authorized(Role::Merc, Role::Spy, true, false));
        assert!(!authorized(Role::Spy, Role::Merc, false, false));
        assert!(authorized(Role::Spy, Role::Merc, false, true));
    }
    fn native_fixture() -> Vec<u8> {
        let hex: String = include_str!("../tests/fixtures/dx11_nameplate_eligibility.hex")
            .split_whitespace()
            .collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn live_file_switch_and_invalid_edit_preserve_last_valid_policy() {
        let path = std::env::temp_dir().join(format!(
            "scbl-nameplates-test-{}-{}.live",
            std::process::id(),
            unsafe { GetTickCount64() }
        ));
        assert!(!path.exists());
        let saved = {
            let mut live = LIVE.lock().unwrap();
            std::mem::replace(
                &mut *live,
                LiveConfig {
                    path: Some(path.clone()),
                    profile: Profile::Mod,
                    last_poll: 0,
                },
            )
        };
        struct Restore {
            saved: Option<LiveConfig>,
            path: PathBuf,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                *LIVE.lock().unwrap() = self.saved.take().unwrap();
                let _ = std::fs::remove_file(&self.path);
            }
        }
        let _restore = Restore {
            saved: Some(saved),
            path: path.clone(),
        };
        let check = |text: &str, expected| {
            std::fs::write(&path, text).unwrap();
            LIVE.lock().unwrap().last_poll = 0;
            assert_eq!(current_profile(), expected);
        };
        check("schema=1\nprofile=native\n", Profile::Native);
        check("schema=1\nprofile=", Profile::Native);
        check(&"x".repeat(129), Profile::Native);
        check("schema=1\nprofile=mod\n", Profile::Mod);
        check("schema=1\nprofile=native\n", Profile::Native);
        std::fs::remove_file(path).unwrap();
        LIVE.lock().unwrap().last_poll = 0;
        assert_eq!(current_profile(), Profile::Mod);
    }
    #[test]
    fn native_mark_permission_tracks_set_clear_and_no_timer_cache() {
        let mut pawn = vec![0u32; 0x1a0c / 4];
        let ptr = pawn.as_mut_ptr().cast::<u8>();
        unsafe {
            assert!(!native_marked(ptr));
            for flags in [0x800, 0x801, 0xffff_ffff] {
                pawn[0x1a08 / 4] = flags;
                assert!(native_marked(ptr));
            }
            // Expiry / source cancellation / respawn clear the native flag.
            // No local mark source array is needed on a network replica.
            for flags in [0, 1, 0x200, 0xffff_f7ff] {
                pawn[0x1a08 / 4] = flags;
                assert!(!native_marked(ptr));
            }
            assert!(!native_marked(std::ptr::null_mut()));
        }
    }
    #[test]
    fn merc_requires_equipped_bms_or_native_mark() {
        assert!(authorized(Role::Merc, Role::Spy, true, false));
        assert!(!authorized(Role::Merc, Role::Spy, false, false));
        assert!(authorized(Role::Merc, Role::Spy, false, true));
    }
    #[test]
    fn spy_requires_effect_on_target_not_own_equipment() {
        assert!(authorized(Role::Spy, Role::Merc, false, true));
        assert!(!authorized(Role::Spy, Role::Merc, true, false));
    }
    #[test]
    fn unknown_and_same_role_never_gain_enemy_permission() {
        for a in [Role::Unknown, Role::Merc, Role::Spy] {
            for b in [Role::Unknown, Role::Merc, Role::Spy] {
                if a == b || a == Role::Unknown || b == Role::Unknown {
                    assert!(!authorized(a, b, true, true));
                }
            }
        }
    }
    #[test]
    fn bms_permission_does_not_depend_on_vision_or_disruption_or_target_pants() {
        for _active in [false, true] {
            for _emp in [false, true] {
                for _pants in [false, true] {
                    assert!(authorized(Role::Merc, Role::Spy, true, false));
                }
            }
        }
    }
    #[test]
    fn role_flags_reject_ambiguous_pawns() {
        assert_eq!(role(1 << 18), Role::Spy);
        assert_eq!(role(1 << 19), Role::Merc);
        assert_eq!(role(0), Role::Unknown);
        assert_eq!(role((1 << 18) | (1 << 19)), Role::Unknown);
    }
    #[test]
    fn live_config_accepts_only_versioned_complete_profiles() {
        assert_eq!(parse_profile("schema=1\nprofile=mod\n"), Some(Profile::Mod));
        assert_eq!(
            parse_profile("schema=1\r\nprofile=native\r\n"),
            Some(Profile::Native)
        );
        for x in [
            "",
            "native",
            "schema=2\nprofile=mod",
            "schema=1\nprofile=MOD",
            "schema=1\nprofile=mod\nother=1",
        ] {
            assert_eq!(parse_profile(x), None);
        }
    }
    #[test]
    fn unknown_native_bytes_are_not_cloned() {
        assert!(relocated_eligibility(&vec![0; ELIGIBILITY_LEN], 0x10000000).is_none());
        assert!(relocated_eligibility(&[], 0x10000000).is_none());
    }
    #[test]
    fn frame_is_scoped_to_one_exact_native_entry() {
        let f = Frame {
            hud: 1,
            entry: 2,
            target: 3,
            enforce: true,
            permit: true,
        };
        assert!(f.matches(1, 2, 3));
        assert!(!f.matches(1, 2, 4));
        assert!(!f.matches(1, 3, 3));
        assert!(!f.matches(4, 2, 3));
        assert!(!Frame {
            enforce: false,
            ..f
        }
        .matches(1, 2, 3));
    }
    #[test]
    fn nested_frames_restore_and_forward_native_value_once() {
        let calls = Cell::new(0);
        let f = Frame {
            hud: 1,
            entry: 2,
            target: 3,
            enforce: true,
            permit: true,
        };
        let value = with_frame(f, || {
            calls.set(calls.get() + 1);
            assert!(FRAME.with(Cell::get).matches(1, 2, 3));
            with_frame(Frame { target: 4, ..f }, || {
                assert!(FRAME.with(Cell::get).matches(1, 2, 4))
            });
            assert!(FRAME.with(Cell::get).matches(1, 2, 3));
            57
        });
        assert_eq!(value, 57);
        assert_eq!(calls.get(), 1);
        assert!(!FRAME.with(Cell::get).enforce);
    }
    #[test]
    fn native_copy_relocates_every_call_and_only_changes_two_policy_branches() {
        let original = native_fixture();
        for base in [0x1000_0000u32, 0xf000_0000] {
            let copy = relocated_eligibility(&original, base).unwrap();
            let mut changed = vec![false; original.len()];
            for &(i, target) in RELATIVE_CALLS {
                let rel = u32::from_le_bytes(copy[i + 1..i + 5].try_into().unwrap());
                assert_eq!(base.wrapping_add(i as u32 + 5).wrapping_add(rel), target);
                changed[i + 1..i + 5].fill(true);
            }
            changed[0x11c..0x11e].fill(true);
            changed[0x39f..0x3a1].fill(true);
            for i in 0..copy.len() {
                if !changed[i] {
                    assert_eq!(copy[i], original[i], "offset {i:x}");
                }
            }
            assert_eq!(&copy[0x11c..0x122], &[0x90, 0xe9, 0xb9, 0, 0, 0]);
            assert_eq!(&copy[0x39f..0x3a1], &[0x90, 0x90]);
            assert_eq!(&copy[0x3a1..0x3ad], &original[0x3a1..0x3ad]); // target+1a54 veto retained
            assert_eq!(&copy[copy.len() - 3..], &[0xc2, 0x0c, 0]); // same thiscall stack cleanup
        }
    }
    #[test]
    fn modified_native_fixture_is_rejected_without_mutation() {
        let mut input = native_fixture();
        input[0x100] ^= 1;
        let before = input.clone();
        assert!(relocated_eligibility(&input, 0x10000000).is_none());
        assert_eq!(input, before);
    }
}
