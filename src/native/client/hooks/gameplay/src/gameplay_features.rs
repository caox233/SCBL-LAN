use std::cell::Cell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use retour::static_detour;
use tracing::error;
use tracing::info;

#[path = "feature_patches.rs"]
mod patches;
static INTEL_PATCHES: Mutex<patches::PatchGroup> = Mutex::new(patches::PatchGroup::new());
static OVERCHARGE_PATCHES: Mutex<patches::PatchGroup> = Mutex::new(patches::PatchGroup::new());

const SUPPORTED_WORLD_RAYCAST: usize = 0x0058_9860;

const INTEL_ABILITY_ACTIVATE: usize = 0x00ED_9FB0;
const INTEL_ABILITY_DEACTIVATE: usize = 0x00E6_DB00;
const ARMOR_ABILITY_ACTIVATE: usize = 0x00E6_CC40;
const ARMOR_ABILITY_DEACTIVATE: usize = 0x00E6_CD60;
const OVERCHARGE_UPDATE: usize = 0x00E9_9DB0;
const OVERCHARGE_DISRUPTION_CANCEL_BRANCH: usize = 0x00E9_9DDF;
const OVERCHARGE_DISRUPTION_CANCEL_ORIGINAL: [u8; 2] = [0x75, 0x67];
const OVERCHARGE_DISRUPTION_CANCEL_BYPASS: [u8; 2] = [0x90, 0x90];
const ARMOR_ABILITY_FLAGS_OFFSET: usize = 0x40;
const ARMOR_ABILITY_OWNER_OFFSET: usize = 0x6C;
const ARMOR_ABILITY_CHARGE_OFFSET: usize = 0x7C;
const OVERCHARGE_PHASE_TIMER_OFFSET: usize = 0xC0;
const OVERCHARGE_STATE_OFFSET: usize = 0xC4;
const ARMOR_ABILITY_ACTIVE: usize = 0x08;
const OVERCHARGE_CASTING: usize = 0x20;
const OVERCHARGE_VTABLE: usize = 0x02A3_F3A4;
const OVERCHARGE_OWNER_DISRUPTION_FLAGS_OFFSET: usize = 0x78;
const OVERCHARGE_OWNER_DISRUPTED: usize = 0x02;
const OVERCHARGE_TRACE_LIMIT: usize = 64;
const PLAYER_MARK_CLEAR_BRANCH: usize = 0x00B9_2F60;
const PLAYER_MARK_ELIGIBILITY_BRANCH: usize = 0x00AD_8607;
const PLAYER_MARK_ELIGIBILITY_ORIGINAL: [u8; 2] = [0x74, 0x04];
const PLAYER_MARK_ELIGIBILITY_BYPASS: [u8; 2] = [0x90; 2];
const PLAYER_MARK_APPLY_BRANCH: usize = 0x00B9_30B2;
const PLAYER_MARK_APPLY_ORIGINAL: [u8; 6] = [0x0F, 0x84, 0xDC, 0x00, 0x00, 0x00];
const PLAYER_MARK_APPLY_BYPASS: [u8; 6] = [0x90; 6];
const PLAYER_MARK_ADD: usize = 0x00B5_EA00;
const PLAYER_MARK_CLEAR: usize = 0x00B5_ED10;
const PLAYER_MARK_COUNT_OFFSET: usize = 0x1A94;
const PLAYER_MARK_CAPACITY_OFFSET: usize = 0x1A98;
const PLAYER_MARK_ENTRIES_OFFSET: usize = 0x1A90;
const PLAYER_MARK_FLAGS_OFFSET: usize = 0x1A08;
const PLAYER_MARK_ACTIVE: usize = 0x800;
const PLAYER_MARK_PRESENTATION_OFFSET: usize = 0x1A9C;
const PLAYER_MARK_PRESENTATION_CONTEXT_OFFSET: usize = 0x28;
const PLAYER_MARK_PRESENTATION_FLAGS_OFFSET: usize = 0x2C;
const PLAYER_MARK_NOTIFICATION_LATCH: usize = 0x20;
const NOTIFICATION_CONTEXT_FLAGS_OFFSET: usize = 0x5C;
const NOTIFICATION_CONTEXT_ACTIVE: usize = 0x80_0000;
const NOTIFICATION_CONTEXT_CANDIDATE_OFFSET: usize = 0x358;
const NOTIFICATION_CANDIDATE_FLAGS_OFFSET: usize = 0x60;
const NOTIFICATION_CANDIDATE_ELIGIBLE: usize = 0x4_0000;
const NOTIFICATION_CANDIDATE_ACTOR_OFFSET: usize = 0x520;
const NOTIFICATION_ACTOR_FLAGS_OFFSET: usize = 0x4E0;
const NOTIFICATION_ACTOR_BLOCKED: usize = 0x2000;
const NOTIFICATION_ACTOR_STATE_OFFSET: usize = 0x93C;
const NOTIFICATION_STATE_FLAGS_OFFSET: usize = 0x4C;
const NOTIFICATION_STATE_BLOCKED: usize = 0x80;
const PLAYER_NOTIFICATION_DISPATCH: usize = 0x00EE_A6D0;
const INTEL_DETECTED_NOTIFICATION: usize = 10;
const PLAYER_MARK_ENTRY_SIZE: usize = 0x10;
const PLAYER_MARK_ENTRY_KEY_OFFSET: usize = 0x04;

const MERC_SPRINT_ENTER: usize = 0x015A_1640;
const MERC_SPRINT_CAMERA_CORRECTION: usize = 0x00DA_FD90;
const SPRINT_PAWN_OFFSET: usize = 0x48;
const ACTOR_NET_ROLE_OFFSET: usize = 0x4D;
const MERC_NET_ROLE: u8 = 1;

static INTEL_ENABLED: AtomicBool = AtomicBool::new(false);
static INTEL_ACTIVE_ABILITIES: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static OVERCHARGE_HOOKS_ENABLED: AtomicBool = AtomicBool::new(false);

/// The existing E99DB0 group must own preparation before hiding its cue.
pub(crate) fn overcharge_audio_ready() -> bool {
    OVERCHARGE_HOOKS_ENABLED.load(Ordering::Acquire)
}

static OVERCHARGE_TRACE_COUNT: AtomicUsize = AtomicUsize::new(0);
static OVERCHARGE_PRESERVED_RECORDED: AtomicBool = AtomicBool::new(false);
static SPRINT_ENABLED: AtomicBool = AtomicBool::new(false);
thread_local! {
    // The camera exception belongs to the synchronous native sprint-enter call,
    // not every camera update on every thread while another player enters sprint.
    static SPRINT_ENTER_ACTIVE: Cell<bool> = const { Cell::new(false) };
}
fn with_sprint_enter<R>(active: bool, call: impl FnOnce() -> R) -> R {
    SPRINT_ENTER_ACTIVE.with(|slot| {
        struct Restore<'a>(&'a Cell<bool>, bool);
        impl Drop for Restore<'_> {
            fn drop(&mut self) { self.0.set(self.1); }
        }
        let _restore = Restore(slot, slot.replace(active));
        call()
    })
}

static_detour! {
    static IntelAbilityActivateHook: unsafe extern "thiscall" fn(*mut u8);
    static IntelAbilityDeactivateHook: unsafe extern "thiscall" fn(*mut u8);
    static ArmorAbilityActivateHook: unsafe extern "thiscall" fn(*mut u8);
    static ArmorAbilityDeactivateHook: unsafe extern "thiscall" fn(*mut u8);
    static OverchargeUpdateHook: unsafe extern "thiscall" fn(*mut u8, f32);
    static PlayerMarkAddHook: unsafe extern "thiscall" fn(*mut u8, usize, usize, usize, f32, u8);
    static PlayerMarkClearHook: unsafe extern "thiscall" fn(*mut u8);
    static PlayerNotificationDispatchHook: unsafe extern "cdecl" fn(*mut u8, usize);
    static MercSprintEnterHook: unsafe extern "thiscall" fn(*mut u8);
    static MercSprintCameraCorrectionHook: unsafe extern "cdecl" fn(usize, usize, usize, usize, usize) -> usize;
}

unsafe fn read_usize(base: *const u8, offset: usize) -> usize {
    base.add(offset).cast::<usize>().read_unaligned()
}

unsafe fn read_u8(base: *const u8, offset: usize) -> u8 {
    base.add(offset).read()
}

unsafe fn read_i32(base: *const u8, offset: usize) -> i32 {
    base.add(offset).cast::<i32>().read_unaligned()
}

unsafe fn read_f32(base: *const u8, offset: usize) -> f32 {
    base.add(offset).cast::<f32>().read_unaligned()
}

unsafe fn set_mark_activation_bypass(enabled: bool) -> bool {
    let plan = [
        patches::Patch { address: PLAYER_MARK_CLEAR_BRANCH, original: &[0x75], modified: &[0xEB] },
        patches::Patch { address: PLAYER_MARK_ELIGIBILITY_BRANCH, original: &PLAYER_MARK_ELIGIBILITY_ORIGINAL, modified: &PLAYER_MARK_ELIGIBILITY_BYPASS },
        patches::Patch { address: PLAYER_MARK_APPLY_BRANCH, original: &PLAYER_MARK_APPLY_ORIGINAL, modified: &PLAYER_MARK_APPLY_BYPASS },
    ];
    let restored = INTEL_PATCHES.lock().unwrap_or_else(|p| p.into_inner()).set(&plan, enabled);
    if !restored { error!("SCBL_GAMEPLAY_FEATURE intel-patch transition=false enabled={enabled} undo=retained"); }
    restored
}

fn begin_intel_ability(this: *mut u8) -> bool {
    if this.is_null() {
        return false;
    }
    let mut active = INTEL_ACTIVE_ABILITIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let ability = this as usize;
    if active.contains(&ability) {
        return true;
    }
    if intel_patch_transition(active.len(), active.len() + 1) == Some(true) && !unsafe { set_mark_activation_bypass(true) } {
        return false;
    }
    active.push(ability);
    info!("SCBL_GAMEPLAY_FEATURE intel-mark-window active=true ability={ability:08X} active_count={}", active.len());
    true
}

fn end_intel_ability(this: *mut u8) {
    if this.is_null() {
        return;
    }
    let ability = this as usize;
    let mut active = INTEL_ACTIVE_ABILITIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(index) = active.iter().position(|candidate| *candidate == ability) else {
        return;
    };
    let before = active.len();
    active.swap_remove(index);
    if intel_patch_transition(before, active.len()) == Some(false) {
        unsafe { set_mark_activation_bypass(false) };
    }
    info!(
        "SCBL_GAMEPLAY_FEATURE intel-mark-window active={} ability={ability:08X} active_count={}",
        !active.is_empty(),
        active.len()
    );
}

/// Return the patch state only when the active Intel Suit population crosses
/// the inactive/active boundary. Overlapping local/remote ability objects must
/// not restore the retail branches until the final one deactivates.
fn intel_patch_transition(before: usize, after: usize) -> Option<bool> {
    match (before == 0, after == 0) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn clear_intel_abilities() -> bool {
    let mut active = INTEL_ACTIVE_ABILITIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let had_active = !active.is_empty();
    active.clear();
    had_active
}

fn intel_ability_activate(this: *mut u8) {
    begin_intel_ability(this);
    unsafe { IntelAbilityActivateHook.call(this) };
}

fn intel_ability_deactivate(this: *mut u8) {
    unsafe { crate::rfd_exposure::destroyed(this as usize); IntelAbilityDeactivateHook.call(this) };
    end_intel_ability(this);
}

pub(super) fn intel_immunity_active() -> bool {
    INTEL_ENABLED.load(Ordering::Acquire) && intel_mark_window_active()
}

fn intel_mark_window_active() -> bool {
    !INTEL_ACTIVE_ABILITIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty()
}

fn overcharge_trace_slot(index: usize) -> Option<usize> {
    (index < OVERCHARGE_TRACE_LIMIT).then_some(index)
}

#[derive(Clone, Copy)]
enum OverchargeTraceEvent {
    Activated,
    Deactivate,
}

impl OverchargeTraceEvent {
    fn label(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::Deactivate => "deactivate",
        }
    }
}

fn should_record_overcharge_cancel(vtable: usize, flags_before: usize, flags_after: usize) -> bool {
    vtable == OVERCHARGE_VTABLE && flags_before & OVERCHARGE_CASTING != 0 && flags_after & OVERCHARGE_CASTING == 0
}

fn should_preserve_overcharge_cast(vtable: usize, flags: usize, disruption_flags: usize) -> bool {
    vtable == OVERCHARGE_VTABLE && flags & OVERCHARGE_CASTING != 0 && disruption_flags & OVERCHARGE_OWNER_DISRUPTED != 0
}

unsafe fn overcharge_owner_disruption_flags(this: *const u8) -> usize {
    if this.is_null() {
        return 0;
    }
    let owner = read_usize(this, ARMOR_ABILITY_OWNER_OFFSET) as *const u8;
    (!owner.is_null()).then(|| read_usize(owner, OVERCHARGE_OWNER_DISRUPTION_FLAGS_OFFSET)).unwrap_or(0)
}

fn set_overcharge_disruption_cancel_bypass(enabled: bool) -> bool {
    let plan = [patches::Patch {
        address: OVERCHARGE_DISRUPTION_CANCEL_BRANCH,
        original: &OVERCHARGE_DISRUPTION_CANCEL_ORIGINAL,
        modified: &OVERCHARGE_DISRUPTION_CANCEL_BYPASS,
    }];
    let restored = unsafe { OVERCHARGE_PATCHES.lock().unwrap_or_else(|p| p.into_inner()).set(&plan, enabled) };
    if !restored { error!("SCBL_GAMEPLAY_FEATURE overcharge-patch transition=false enabled={enabled} undo=retained"); }
    restored
}

fn trace_overcharge_ability_event(event: OverchargeTraceEvent, this: *mut u8) {
    if OVERCHARGE_HOOKS_ENABLED.load(Ordering::Acquire) && !this.is_null() {
        let (vtable, flags, state, charge, phase_timer, disruption_flags) = unsafe {
            (
                read_usize(this, 0),
                read_usize(this, ARMOR_ABILITY_FLAGS_OFFSET),
                read_u8(this, OVERCHARGE_STATE_OFFSET),
                read_f32(this, ARMOR_ABILITY_CHARGE_OFFSET),
                read_f32(this, OVERCHARGE_PHASE_TIMER_OFFSET),
                overcharge_owner_disruption_flags(this),
            )
        };
        if vtable != OVERCHARGE_VTABLE {
            return;
        }
        let index = OVERCHARGE_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
        let Some(sequence) = overcharge_trace_slot(index) else {
            return;
        };
        info!(
            "SCBL_OVERCHARGE_TRACE sequence={} event={} class=UMPArmorAbilitySaboteur flags={:08X} state={} charge={:.3} phase_timer={:.3} disruption_flags={:08X}",
            sequence,
            event.label(),
            flags,
            state,
            charge,
            phase_timer,
            disruption_flags
        );
    }
}

fn armor_ability_activate(this: *mut u8) {
    let active_before = !this.is_null() && unsafe { read_usize(this, ARMOR_ABILITY_FLAGS_OFFSET) } & ARMOR_ABILITY_ACTIVE != 0;
    unsafe { ArmorAbilityActivateHook.call(this) };
    let active_after = !this.is_null() && unsafe { read_usize(this, ARMOR_ABILITY_FLAGS_OFFSET) } & ARMOR_ABILITY_ACTIVE != 0;
    if !active_before && active_after {
        unsafe { crate::rfd_exposure::activated(this as usize); }
        OVERCHARGE_PRESERVED_RECORDED.store(false, Ordering::Release);
        trace_overcharge_ability_event(OverchargeTraceEvent::Activated, this);
    }
}

fn armor_ability_deactivate(this: *mut u8) {
    let active = !this.is_null() && unsafe { read_usize(this, ARMOR_ABILITY_FLAGS_OFFSET) } & ARMOR_ABILITY_ACTIVE != 0;
    if active {
        trace_overcharge_ability_event(OverchargeTraceEvent::Deactivate, this);
    }
    if active { unsafe { crate::rfd_exposure::deactivating(this as usize); } }
    unsafe { ArmorAbilityDeactivateHook.call(this) };
    if !this.is_null() && unsafe { read_usize(this, 0) } == OVERCHARGE_VTABLE {
        OVERCHARGE_PRESERVED_RECORDED.store(false, Ordering::Release);
    }
}

fn overcharge_update(this: *mut u8, delta_seconds: f32) {
    let (vtable, flags_before, state_before, charge_before, timer_before, disruption_flags_before) = if this.is_null() {
        (0, 0, 0, 0.0, 0.0, 0)
    } else {
        unsafe {
            (
                read_usize(this, 0),
                read_usize(this, ARMOR_ABILITY_FLAGS_OFFSET),
                read_u8(this, OVERCHARGE_STATE_OFFSET),
                read_f32(this, ARMOR_ABILITY_CHARGE_OFFSET),
                read_f32(this, OVERCHARGE_PHASE_TIMER_OFFSET),
                overcharge_owner_disruption_flags(this),
            )
        }
    };
    unsafe { crate::overload_audio::prepare(this as usize, || OverchargeUpdateHook.call(this, delta_seconds)) };
    if this.is_null() {
        return;
    }
    let (flags_after, state_after, charge_after, timer_after) = unsafe {
        (
            read_usize(this, ARMOR_ABILITY_FLAGS_OFFSET),
            read_u8(this, OVERCHARGE_STATE_OFFSET),
            read_f32(this, ARMOR_ABILITY_CHARGE_OFFSET),
            read_f32(this, OVERCHARGE_PHASE_TIMER_OFFSET),
        )
    };
    if OVERCHARGE_HOOKS_ENABLED.load(Ordering::Acquire)
        && should_preserve_overcharge_cast(vtable, flags_before, disruption_flags_before)
        && !OVERCHARGE_PRESERVED_RECORDED.swap(true, Ordering::AcqRel)
    {
        let index = OVERCHARGE_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
        if let Some(sequence) = overcharge_trace_slot(index) {
            info!(
                "SCBL_OVERCHARGE_TRACE sequence={} event=casting-disruption-preserved class=UMPArmorAbilitySaboteur flags_after={:08X} state_before={} state_after={} charge_before={:.3} charge_after={:.3}",
                sequence, flags_after, state_before, state_after, charge_before, charge_after
            );
        }
    }
    if OVERCHARGE_HOOKS_ENABLED.load(Ordering::Acquire) && should_record_overcharge_cancel(vtable, flags_before, flags_after) {
        let index = OVERCHARGE_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
        if let Some(sequence) = overcharge_trace_slot(index) {
            info!(
                "SCBL_OVERCHARGE_TRACE sequence={} event=casting-cancelled class=UMPArmorAbilitySaboteur flags_before={:08X} flags_after={:08X} state_before={} state_after={} charge_before={:.3} charge_after={:.3} phase_timer_before={:.3} phase_timer_after={:.3} disruption_flags_before={:08X}",
                sequence, flags_before, flags_after, state_before, state_after, charge_before, charge_after, timer_before, timer_after, disruption_flags_before
            );
        }
    }
}

unsafe fn mark_record_timer_bits(this: *const u8, first_key: usize, second_key: usize) -> Option<u32> {
    use crate::equipment_native::word;
    let address = this as usize;
    let count = word(address, PLAYER_MARK_COUNT_OFFSET)? as i32;
    let capacity = word(address, PLAYER_MARK_CAPACITY_OFFSET)? as i32;
    let entries = word(address, PLAYER_MARK_ENTRIES_OFFSET)? as usize;
    if count <= 0 || count > 4096 || capacity < count || entries == 0 {
        return None;
    }
    for index in 0..count as usize {
        let row = entries.checked_add(index.checked_mul(PLAYER_MARK_ENTRY_SIZE)?)?;
        let key = row.checked_add(PLAYER_MARK_ENTRY_KEY_OFFSET)?;
        if word(key, 0)? as usize == first_key && word(key, 4)? as usize == second_key {
            return word(row, 0);
        }
    }
    None
}

unsafe fn mark_record_exists(this: *const u8, first_key: usize, second_key: usize) -> bool {
    mark_record_timer_bits(this, first_key, second_key).is_some()
}

fn player_mark_add(this: *mut u8, first_key: usize, second_key: usize, source: usize, duration: f32, refresh: u8) {
    // Observation only: native code owns duplicate, refresh and duration semantics.
    let intel_window_active = INTEL_ENABLED.load(Ordering::Acquire) && intel_mark_window_active();
    let diagnose = intel_window_active && !this.is_null();
    let (role, count_before, marker_before, duplicate_before) = if diagnose {
        unsafe {
            (
                read_u8(this, 0x48),
                read_i32(this, PLAYER_MARK_COUNT_OFFSET),
                read_usize(this, PLAYER_MARK_FLAGS_OFFSET) & PLAYER_MARK_ACTIVE != 0,
                mark_record_exists(this, first_key, second_key),
            )
        }
    } else {
        (0, 0, false, false)
    };

    let (
        presentation_present,
        notification_latched_before,
        notification_context_present,
        notification_context_active,
        notification_candidate_present,
        notification_candidate_eligible,
    ) = if diagnose {
        unsafe {
            let presentation = read_usize(this, PLAYER_MARK_PRESENTATION_OFFSET) as *mut u8;
            let presentation_present = !presentation.is_null();
            let notification_latched = presentation_present && read_usize(presentation, PLAYER_MARK_PRESENTATION_FLAGS_OFFSET) & PLAYER_MARK_NOTIFICATION_LATCH != 0;
            let context = if presentation_present {
                read_usize(presentation, PLAYER_MARK_PRESENTATION_CONTEXT_OFFSET) as *mut u8
            } else {
                std::ptr::null_mut()
            };
            let context_present = !context.is_null();
            let context_active = context_present && read_usize(context, NOTIFICATION_CONTEXT_FLAGS_OFFSET) & NOTIFICATION_CONTEXT_ACTIVE != 0;
            let candidate = if context_present {
                read_usize(context, NOTIFICATION_CONTEXT_CANDIDATE_OFFSET) as *mut u8
            } else {
                std::ptr::null_mut()
            };
            let candidate_present = !candidate.is_null();
            let candidate_eligible = candidate_present && read_usize(candidate, NOTIFICATION_CANDIDATE_FLAGS_OFFSET) & NOTIFICATION_CANDIDATE_ELIGIBLE != 0;
            (
                presentation_present,
                notification_latched,
                context_present,
                context_active,
                candidate_present,
                candidate_eligible,
            )
        }
    } else {
        (false, false, false, false, false, false)
    };

    let lifetime_ready = crate::intel_mark_lifetime::ready();
    let owned_timer_before = if lifetime_ready {
        unsafe { mark_record_timer_bits(this, first_key, second_key) }
    } else {
        None
    };
    unsafe { PlayerMarkAddHook.call(this, first_key, second_key, source, duration, refresh) };
    // A no-refresh duplicate that leaves its native timer unchanged owns no new
    // effect. Preserve the existing Intel provenance; explicit refresh, a new
    // row, or an actual timer change still relinquishes/reassigns ownership.
    if lifetime_ready && (refresh != 0 || owned_timer_before.is_none()
        || unsafe { mark_record_timer_bits(this, first_key, second_key) } != owned_timer_before)
    {
        unsafe { crate::intel_mark_lifetime::mark_added(this as usize, first_key as u32, second_key as u32, source); }
    }

    if diagnose {
        let count_after = unsafe { read_i32(this, PLAYER_MARK_COUNT_OFFSET) };
        let marker_after = unsafe { read_usize(this, PLAYER_MARK_FLAGS_OFFSET) } & PLAYER_MARK_ACTIVE != 0;
        let native_presentation_candidate = !duplicate_before && !marker_before;
        info!(
            "SCBL_GAMEPLAY_FEATURE intel-mark-add role={role} duplicate_before={duplicate_before} native_presentation_candidate={native_presentation_candidate} presentation_present={presentation_present} notification_latched_before={notification_latched_before} notification_context_present={notification_context_present} notification_context_active={notification_context_active} notification_candidate_present={notification_candidate_present} notification_candidate_eligible={notification_candidate_eligible} count_before={count_before} count_after={count_after} marker_before={marker_before} marker_after={marker_after} source_present={} duration_ms={:.0} refresh={}",
            source != 0,
            duration.max(0.0) * 1000.0,
            refresh != 0
        );
    }
}

fn player_notification_dispatch(candidate: *mut u8, event: usize) {
    let diagnose = INTEL_ENABLED.load(Ordering::Acquire) && intel_mark_window_active() && event == INTEL_DETECTED_NOTIFICATION;
    let (candidate_present, actor_present, actor_blocked, notification_state_present, notification_state_blocked) = if diagnose {
        unsafe {
            let candidate_present = !candidate.is_null();
            let actor = if candidate_present {
                read_usize(candidate, NOTIFICATION_CANDIDATE_ACTOR_OFFSET) as *mut u8
            } else {
                std::ptr::null_mut()
            };
            let actor_present = !actor.is_null();
            let actor_blocked = actor_present && read_usize(actor, NOTIFICATION_ACTOR_FLAGS_OFFSET) & NOTIFICATION_ACTOR_BLOCKED != 0;
            let state = if actor_present {
                read_usize(actor, NOTIFICATION_ACTOR_STATE_OFFSET) as *mut u8
            } else {
                std::ptr::null_mut()
            };
            let state_present = !state.is_null();
            let state_blocked = state_present && read_usize(state, NOTIFICATION_STATE_FLAGS_OFFSET) & NOTIFICATION_STATE_BLOCKED != 0;
            (candidate_present, actor_present, actor_blocked, state_present, state_blocked)
        }
    } else {
        (false, false, false, false, false)
    };

    unsafe { PlayerNotificationDispatchHook.call(candidate, event) };

    if diagnose {
        info!(
            "SCBL_GAMEPLAY_FEATURE intel-notification-dispatch event={event} candidate_present={candidate_present} actor_present={actor_present} actor_blocked={actor_blocked} notification_state_present={notification_state_present} notification_state_blocked={notification_state_blocked}"
        );
    }
}

fn player_mark_clear(this: *mut u8) {
    let diagnose = INTEL_ENABLED.load(Ordering::Acquire) && intel_mark_window_active() && !this.is_null();
    let (role, count_before, marker_before) = if diagnose {
        unsafe {
            (
                read_u8(this, 0x48),
                read_i32(this, PLAYER_MARK_COUNT_OFFSET),
                read_usize(this, PLAYER_MARK_FLAGS_OFFSET) & PLAYER_MARK_ACTIVE != 0,
            )
        }
    } else {
        (0, 0, false)
    };

    unsafe { PlayerMarkClearHook.call(this) };

    if diagnose {
        let count_after = unsafe { read_i32(this, PLAYER_MARK_COUNT_OFFSET) };
        let marker_after = unsafe { read_usize(this, PLAYER_MARK_FLAGS_OFFSET) } & PLAYER_MARK_ACTIVE != 0;
        info!("SCBL_GAMEPLAY_FEATURE intel-mark-clear role={role} count_before={count_before} count_after={count_after} marker_before={marker_before} marker_after={marker_after}");
    }
}

fn merc_sprint_enter(this: *mut u8) {
    let pawn = if this.is_null() { 0 } else { unsafe { read_usize(this, SPRINT_PAWN_OFFSET) } };
    let is_merc = SPRINT_ENABLED.load(Ordering::Acquire) && pawn != 0 && unsafe { read_u8(pawn as *const u8, ACTOR_NET_ROLE_OFFSET) } == MERC_NET_ROLE;
    with_sprint_enter(is_merc, || unsafe { MercSprintEnterHook.call(this) });
}

fn merc_sprint_camera_correction(arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> usize {
    if SPRINT_ENABLED.load(Ordering::Acquire) && SPRINT_ENTER_ACTIVE.with(Cell::get) {
        0
    } else {
        unsafe { MercSprintCameraCorrectionHook.call(arg1, arg2, arg3, arg4, arg5) }
    }
}

macro_rules! enable_hook {
    ($hook:ident, $address:expr, $callback:ident) => {{
        let target = std::mem::transmute($address);
        match $hook.initialize(target, $callback).and_then(|hook| hook.enable()) {
            Ok(_) => {
                info!("SCBL_GAMEPLAY_FEATURE hook={} enabled=true address={:08X}", stringify!($hook), $address);
                true
            }
            Err(err) => {
                error!("SCBL_GAMEPLAY_FEATURE hook={} enabled=false address={:08X} error={err:?}", stringify!($hook), $address);
                false
            }
        }
    }};
}

// Publish readiness only for a complete group. A partial detour set must not
// leave activation enabled without its matching deactivation/cleanup hook.
fn finish_hook_group(enabled: &[bool], mut disable: impl FnMut(usize)) -> bool {
    if !enabled.is_empty() && enabled.iter().all(|ok| *ok) { return true; }
    for (index, ok) in enabled.iter().enumerate().rev() {
        if *ok { disable(index); }
    }
    false
}

pub unsafe fn init(world_raycast: usize) {
    if world_raycast != SUPPORTED_WORLD_RAYCAST {
        error!("SCBL_GAMEPLAY_FEATURE unsupported executable world_raycast={world_raycast:08X}");
        return;
    }
    if INTEL_ENABLED.load(Ordering::Acquire) || OVERCHARGE_HOOKS_ENABLED.load(Ordering::Acquire)
        || SPRINT_ENABLED.load(Ordering::Acquire)
    {
        error!("SCBL_GAMEPLAY_FEATURE enabled=false reason=already-active");
        return;
    }
    // Retry both independent undo journals, even after an earlier hook setup
    // failed before publishing readiness. Unknown bytes must not be adopted.
    let intel_restored = set_mark_activation_bypass(false);
    let overcharge_restored = set_overcharge_disruption_cancel_bypass(false);
    if !intel_restored || !overcharge_restored {
        error!("SCBL_GAMEPLAY_FEATURE enabled=false reason=pending-patch-restoration");
        return;
    }
    let overcharge_activate_enabled = enable_hook!(ArmorAbilityActivateHook, ARMOR_ABILITY_ACTIVATE, armor_ability_activate);
    let overcharge_deactivate_enabled = enable_hook!(ArmorAbilityDeactivateHook, ARMOR_ABILITY_DEACTIVATE, armor_ability_deactivate);
    let overcharge_update_enabled = enable_hook!(OverchargeUpdateHook, OVERCHARGE_UPDATE, overcharge_update);
    if overcharge_activate_enabled && overcharge_deactivate_enabled && overcharge_update_enabled && set_overcharge_disruption_cancel_bypass(true) {
        OVERCHARGE_TRACE_COUNT.store(0, Ordering::Release);
        OVERCHARGE_PRESERVED_RECORDED.store(false, Ordering::Release);
        OVERCHARGE_HOOKS_ENABLED.store(true, Ordering::Release);
    } else {
        let _ = set_overcharge_disruption_cancel_bypass(false);
        let _ = OverchargeUpdateHook.disable();
        let _ = ArmorAbilityDeactivateHook.disable();
        let _ = ArmorAbilityActivateHook.disable();
        error!("SCBL_GAMEPLAY_FEATURE overcharge_immunity enabled=false reason=required_hook_failed");
    }
    let intel = [
        enable_hook!(IntelAbilityActivateHook, INTEL_ABILITY_ACTIVATE, intel_ability_activate),
        enable_hook!(IntelAbilityDeactivateHook, INTEL_ABILITY_DEACTIVATE, intel_ability_deactivate),
        enable_hook!(PlayerMarkAddHook, PLAYER_MARK_ADD, player_mark_add),
        enable_hook!(PlayerMarkClearHook, PLAYER_MARK_CLEAR, player_mark_clear),
        enable_hook!(PlayerNotificationDispatchHook, PLAYER_NOTIFICATION_DISPATCH, player_notification_dispatch),
        crate::intel_disruptor::init(),
    ];
    let intel_ready = finish_hook_group(&intel, |index| {
        let _ = match index {
            0 => IntelAbilityActivateHook.disable(),
            1 => IntelAbilityDeactivateHook.disable(),
            2 => PlayerMarkAddHook.disable(),
            3 => PlayerMarkClearHook.disable(),
            4 => PlayerNotificationDispatchHook.disable(),
            _ => { crate::intel_disruptor::deinit(); Ok(()) },
        };
    });
    INTEL_ENABLED.store(intel_ready, Ordering::Release);
    if !intel_ready { error!("SCBL_GAMEPLAY_FEATURE intel enabled=false reason=required_hook_failed"); }

    let sprint = [
        enable_hook!(MercSprintEnterHook, MERC_SPRINT_ENTER, merc_sprint_enter),
        enable_hook!(MercSprintCameraCorrectionHook, MERC_SPRINT_CAMERA_CORRECTION, merc_sprint_camera_correction),
    ];
    let sprint_ready = finish_hook_group(&sprint, |index| {
        let _ = if index == 0 { MercSprintEnterHook.disable() } else { MercSprintCameraCorrectionHook.disable() };
    });
    SPRINT_ENABLED.store(sprint_ready, Ordering::Release);
    if !sprint_ready { error!("SCBL_GAMEPLAY_FEATURE sprint enabled=false reason=required_hook_failed"); }
}

pub unsafe fn deinit() {
    // Code ownership is independent of the hook readiness flags and active
    // ability list; both can already be empty when a previous restore failed.
    let _ = set_overcharge_disruption_cancel_bypass(false);
    if OVERCHARGE_HOOKS_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = OverchargeUpdateHook.disable();
        let _ = ArmorAbilityDeactivateHook.disable();
        let _ = ArmorAbilityActivateHook.disable();
        OVERCHARGE_TRACE_COUNT.store(0, Ordering::Release);
        OVERCHARGE_PRESERVED_RECORDED.store(false, Ordering::Release);
    }
    if INTEL_ENABLED.swap(false, Ordering::AcqRel) {
        crate::intel_disruptor::deinit();
        let _ = PlayerNotificationDispatchHook.disable();
        let _ = PlayerMarkClearHook.disable();
        let _ = PlayerMarkAddHook.disable();
        let _ = IntelAbilityDeactivateHook.disable();
        let _ = IntelAbilityActivateHook.disable();
    }
    clear_intel_abilities();
    let _ = set_mark_activation_bypass(false);
    if SPRINT_ENABLED.swap(false, Ordering::AcqRel) {
        let _ = MercSprintCameraCorrectionHook.disable();
        let _ = MercSprintEnterHook.disable();
        SPRINT_ENTER_ACTIVE.with(|slot| slot.set(false));
    }
}

#[cfg(test)]
mod tests {
    use super::intel_patch_transition;
    use super::overcharge_trace_slot;
    use super::should_preserve_overcharge_cast;
    use super::should_record_overcharge_cancel;
    use super::OVERCHARGE_CASTING;
    use super::OVERCHARGE_OWNER_DISRUPTED;
    use super::OVERCHARGE_TRACE_LIMIT;
    use super::OVERCHARGE_VTABLE;

    #[test]
    fn intel_bypass_is_scoped_to_the_complete_active_window() {
        assert_eq!(intel_patch_transition(0, 1), Some(true));
        assert_eq!(intel_patch_transition(1, 2), None);
        assert_eq!(intel_patch_transition(2, 1), None);
        assert_eq!(intel_patch_transition(1, 0), Some(false));
        assert_eq!(intel_patch_transition(0, 0), None);
    }

    #[test]
    fn overcharge_diagnostic_budget_is_strictly_bounded() {
        assert_eq!(overcharge_trace_slot(0), Some(0));
        assert_eq!(overcharge_trace_slot(OVERCHARGE_TRACE_LIMIT - 1), Some(OVERCHARGE_TRACE_LIMIT - 1));
        assert_eq!(overcharge_trace_slot(OVERCHARGE_TRACE_LIMIT), None);
    }

    #[test]
    fn overcharge_records_only_casting_flag_cancellation_on_the_saboteur_ability() {
        assert!(should_record_overcharge_cancel(OVERCHARGE_VTABLE, OVERCHARGE_CASTING, 0));
        assert!(!should_record_overcharge_cancel(0x02A3_F084, OVERCHARGE_CASTING, 0));
        assert!(!should_record_overcharge_cancel(OVERCHARGE_VTABLE, 0, 0));
        assert!(!should_record_overcharge_cancel(OVERCHARGE_VTABLE, OVERCHARGE_CASTING, OVERCHARGE_CASTING));
    }

    #[test]
    fn overcharge_preserves_only_an_already_started_disrupted_saboteur_cast() {
        assert!(should_preserve_overcharge_cast(OVERCHARGE_VTABLE, OVERCHARGE_CASTING, OVERCHARGE_OWNER_DISRUPTED));
        assert!(!should_preserve_overcharge_cast(0x02A3_F084, OVERCHARGE_CASTING, OVERCHARGE_OWNER_DISRUPTED));
        assert!(!should_preserve_overcharge_cast(OVERCHARGE_VTABLE, 0, OVERCHARGE_OWNER_DISRUPTED));
        assert!(!should_preserve_overcharge_cast(OVERCHARGE_VTABLE, OVERCHARGE_CASTING, 0));
    }
}

#[cfg(test)]
mod hook_group_tests {
    use super::finish_hook_group;
    #[test]
    fn each_partial_intel_and_sprint_group_rolls_back_in_reverse_order() {
        for count in [2,6] { for failed in 0..count {
            let mut enabled=vec![true;count];enabled[failed]=false;
            let mut disabled=Vec::new();
            assert!(!finish_hook_group(&enabled,|index|disabled.push(index)));
            assert_eq!(disabled,(0..count).rev().filter(|i|*i!=failed).collect::<Vec<_>>());
        }}
    }
    #[test]
    fn complete_groups_remain_enabled_and_empty_groups_never_publish_ready() {
        assert!(finish_hook_group(&[true,true], |_|panic!("success must not disable")));
        assert!(finish_hook_group(&[true;5], |_|panic!("success must not disable")));
        assert!(!finish_hook_group(&[], |_|panic!("empty group")));
        assert!(!finish_hook_group(&[false,false], |_|panic!("failed hooks not enabled")));
    }
}

#[cfg(test)]
mod sprint_scope_tests {
    use super::{with_sprint_enter, SPRINT_ENTER_ACTIVE};
    use std::cell::Cell;
    #[test]
    fn nested_calls_restore_the_callers_scope_instead_of_clearing_it() {
        assert!(!SPRINT_ENTER_ACTIVE.with(Cell::get));
        with_sprint_enter(true, || {
            assert!(SPRINT_ENTER_ACTIVE.with(Cell::get));
            with_sprint_enter(false, || assert!(!SPRINT_ENTER_ACTIVE.with(Cell::get)));
            assert!(SPRINT_ENTER_ACTIVE.with(Cell::get));
            with_sprint_enter(true, || assert!(SPRINT_ENTER_ACTIVE.with(Cell::get)));
            assert!(SPRINT_ENTER_ACTIVE.with(Cell::get));
        });
        assert!(!SPRINT_ENTER_ACTIVE.with(Cell::get));
    }
    #[test]
    fn another_native_thread_cannot_inherit_the_camera_exception() {
        with_sprint_enter(true, || {
            std::thread::spawn(|| {
                assert!(!SPRINT_ENTER_ACTIVE.with(Cell::get));
                with_sprint_enter(true, || assert!(SPRINT_ENTER_ACTIVE.with(Cell::get)));
                assert!(!SPRINT_ENTER_ACTIVE.with(Cell::get));
            }).join().unwrap();
            assert!(SPRINT_ENTER_ACTIVE.with(Cell::get));
        });
    }
    #[test]
    fn scope_restores_on_unwind() {
        let result = std::panic::catch_unwind(|| with_sprint_enter(true, || panic!("test unwind")));
        assert!(result.is_err());
        assert!(!SPRINT_ENTER_ACTIVE.with(Cell::get));
    }
}

#[cfg(test)]
#[path = "drone_mark_tests.rs"]
mod drone_mark_tests;
