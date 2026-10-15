//! DX11 multiplayer input rules, deliberately separate from obstruction/HUD.
//! Retail evidence: r12-native-controller-action-map/action-delivery,
//! reload-event/reload-update/reload-exit-complete and sprint-event dumps.
//! r15 reads the reason retained in the live reload state's native parameters.
//! Native transitions can be queued: a transient PawnEvent call scope is NOT
//! the reload lifetime. No pending key/event marker or timeout is retained.
//! r16 consumes manual-reload sprint at F25540, before its TWO consumers:
//! E9EDD0 controller flags and pawn+1404 vtable+7C state-machine input.
//! Returning from only E9EDD0 cannot prevent the second consumer.
//! r17 records physical sprint edges before consuming reload-blocked motion.
//! r19 also recognizes the native +194 completion branch, which calls back with NULL.
//! Generic NULL exits remain excluded; completion proof is call-scoped, not timed.
//! All entry points remain on the game's calling thread; no worker or key polling.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use retour::static_detour;
use tracing::{error, info};

use super::{read_pointer, read_u32_at};

const INPUT_DISPATCH: usize = 0x00F2_5540;
const INPUT_ACTION: usize = 0x00E9_EDD0;
const RELOAD_REASON_NAME: usize = 0x0339_FA84;
const EMPTY_ONLY_NAME: usize = 0x0339_FA98;
const AUTO_RELOAD_TIMER_NAME: usize = 0x0339_FAE4;
const DRAW_RELOAD_NAME: usize = 0x0339_FAE0;
const RELOAD_UPDATE: usize = 0x015B_74D0;
const RELOAD_EVENT: usize = 0x0156_F070;
const RELOAD_COMPLETE: usize = 0x0152_6C20;
const RELOAD_NATIVE_FINISH: usize = 0x0156_E6D0;
const RELOAD_ANIMATION_NAME: usize = 0x033C_2DCC;
const SET_FIRE: usize = 0x00E7_04A0;
const PAWN_EVENT: usize = 0x00D8_73F0;
const RELOAD_VTABLE: usize = 0x02BD_7FCC;
const MERC_SPRINT_VTABLE: usize = 0x02BD_2D24;
const FORCE_SHOOT_NAME: usize = 0x0339_F5E4;
const END_SPRINT_NAME: usize = 0x0339_FCAC;
const FIRE_BIT: u32 = 1 << 25;
const SPRINT_HELD_BIT: u32 = 1 << 30;
const SPRINT_REQUEST_BITS: u32 = 3 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Other,
    Fire,
    FireReleased,
    SprintPressed,
    SprintHeld,
    SprintReleased,
}

// The native controller maps abstract actions to delivered input IDs. These
// are NOT Windows virtual keys. Rebinding Shift does not change this policy.
fn classify(bindings: &[[u8; 4]], delivered: u32) -> Action {
    if delivered == 0 || delivered >= 0xbb {
        return Action::Other;
    }
    let has = |kind| bindings.iter().any(|entry| entry[0] == kind && u32::from(entry[1]) == delivered);
    if has(2) || has(0xad) {
        Action::Fire
    } else if has(3) || has(0xae) {
        Action::FireReleased
    } else if has(0x50) {
        Action::SprintPressed
    } else if has(0x51) {
        Action::SprintReleased
    } else if has(0x4f) {
        Action::SprintHeld
    } else {
        Action::Other
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReloadKind {
    None,
    Automatic,
    Manual,
}

// 143D910 copies transition.params[1] to state+58 and retains it throughout
// the native state. 15269C0 distinguishes EmptyOnly / AutoReloadTimer and the
// default (zero) player request; explicit ReloadWeaponEvent uses reason Reload.
// Unknown/missing reasons never acquire the new sprint restriction.
#[derive(Clone, Copy)]
struct ReloadNames {
    reload: u32,
    empty_only: u32,
    auto_timer: u32,
    draw: u32,
}

fn live_reload_kind(active: bool, reason: Option<u32>, names: ReloadNames) -> ReloadKind {
    if !active {
        return ReloadKind::None;
    }
    let ids = [names.reload, names.empty_only, names.auto_timer, names.draw];
    if ids.contains(&0) || ids.iter().enumerate().any(|(i, id)| ids[i + 1..].contains(id)) {
        return ReloadKind::Automatic; // preserve accepted fire guard, no extra sprint lock
    }
    match reason {
        Some(id) if id == names.empty_only || id == names.auto_timer || id == names.draw => ReloadKind::Automatic,
        Some(0) => ReloadKind::Manual,
        Some(id) if id == names.reload => ReloadKind::Manual,
        _ => ReloadKind::Automatic,
    }
}

thread_local! {
    static RELOAD_OBSERVATION: Cell<Option<(usize, usize, ReloadKind, Option<u32>)>> = const { Cell::new(None) };
}
static RELOAD_LOG_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SprintGate {
    #[default]
    Ready,
    AwaitRelease,
    AwaitPress,
}

#[derive(Clone, Copy, Default)]
struct Policy {
    pawn: usize,
    gate: SprintGate,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Decision {
    suppress: bool,
    stop_sprint: bool,
}

impl Policy {
    fn observe_sprint_edge(&mut self, pawn: usize, merc: bool, action: Action) {
        if self.pawn != pawn || !merc {
            *self = Self { pawn, gate: SprintGate::Ready };
        }
        if merc {
            match action {
                Action::SprintReleased if self.gate != SprintGate::Ready => self.gate = SprintGate::AwaitPress,
                Action::SprintPressed if self.gate == SprintGate::AwaitPress => self.gate = SprintGate::Ready,
                _ => {}
            }
        }
    }

    fn decide(&mut self, pawn: usize, merc: bool, reload: ReloadKind, sprint: bool, held: bool, action: Action) -> Decision {
        // A genuine release/repress counts even while movement is prohibited.
        // Held alone never clears the independent fire/repress rule.
        self.observe_sprint_edge(pawn, merc, action);
        if merc {
            if reject_manual_reload_sprint(merc, reload, action) {
                return Decision { suppress: true, stop_sprint: sprint };
            }
            if matches!(action, Action::SprintPressed | Action::SprintHeld) && self.gate != SprintGate::Ready {
                return Decision { suppress: true, stop_sprint: false };
            }
        }
        if action != Action::Fire {
            return Decision::default();
        }
        let stop_sprint = merc && sprint;
        if stop_sprint {
            self.gate = if held { SprintGate::AwaitRelease } else { SprintGate::AwaitPress };
        }
        Decision {
            suppress: reload != ReloadKind::None,
            stop_sprint,
        }
    }
}

thread_local! {
    static POLICY: Cell<Policy> = Cell::new(Policy { pawn: 0, gate: SprintGate::Ready });
    static POLICY_OWNER: Cell<Option<InputOwner>> = const { Cell::new(None) };
    static NATIVE_RELOAD_OWNER: Cell<Option<InputOwner>> = const { Cell::new(None) };
}
// Stop invalidates all thread-local owners without touching another thread's TLS.
static LIFECYCLE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InputOwner {
    controller: usize,
    controller_object: u32,
    pawn: usize,
    pawn_object: u32,
    input: usize,
    world: usize,
    world_object: u32,
    lifecycle: u32,
}

fn input_owner(controller: *const u8, pawn: *const u8, input: *const u8) -> Option<InputOwner> {
    // Native UObject identifier (+20) and actor World (+144), already used by
    // the other local adapters. These are identity checks, not a GC lifetime pin
    // or a claim that identical address/index/world reuse can always be detected.
    let word = |p, o| unsafe { crate::equipment_native::word(p, o) };
    let world = word(pawn as usize, 0x144)? as usize;
    if world < 0x10000 || word(controller as usize, 0x144)? as usize != world { return None; }
    Some(InputOwner {
        controller: controller as usize, controller_object: word(controller as usize, 0x20)?,
        pawn: pawn as usize, pawn_object: word(pawn as usize, 0x20)?, input: input as usize,
        world, world_object: word(world, 0x20)?, lifecycle: LIFECYCLE.load(Ordering::Acquire),
    })
}

fn bind_policy_owner(controller: *const u8, pawn: *const u8, input: *const u8) -> bool {
    let owner = input_owner(controller, pawn, input);
    POLICY_OWNER.with(|slot| {
        if owner.is_none() || slot.get() != owner {
            POLICY.with(|policy| policy.set(Policy::default()));
            slot.set(owner);
        }
    });
    owner.is_some()
}
// One noisy held-input path must not consume every other rule's evidence.
static SPRINT_EDGE_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
static LOG_COUNTS: [AtomicU32; 10] = [const { AtomicU32::new(0) }; 10];
static COMPLETION_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
static NATIVE_FINISH_LOG_COUNT: AtomicU32 = AtomicU32::new(0);

static_detour! {
    // F25540: ECX controller, one delivered-ID stack arg, EAX handled, ret 4.
    // Native handled exits F25647/F25682 return 1; ignored F25899 returns 0.
    static InputDispatchHook: unsafe extern "thiscall" fn(*mut c_void, u32) -> i32;
    // E9EDD0 uses ECX controller and one stack argument; every exit is ret 4.
    static InputActionHook: unsafe extern "thiscall" fn(*mut c_void, u32);
    // UReloadFireWeaponState vtable +94: ECX state, float dt, ret 4.
    static ReloadUpdateHook: unsafe extern "thiscall" fn(*mut c_void, f32);
    // Vtable +B4: ECX state, FName*, payload*, integer handled, ret 8.
    static ReloadEventHook: unsafe extern "thiscall" fn(*mut c_void, *const u32, *mut c_void) -> i32;
    // 1526C20: ECX state, notification pointer on stack, AL bool, ret 4.
    static ReloadCompleteHook: unsafe extern "thiscall" fn(*mut c_void, *const u8) -> u8;
    // 156E6D0 vtable+194: ECX state, native notification*, AL bool, ret 4.
    static ReloadNativeFinishHook: unsafe extern "thiscall" fn(*mut c_void, *const u8) -> u8;
}

fn rule_log_index(reason: &str) -> usize {
    match reason {
        "merc-fire-end-sprint" => 0,
        "merc-reload-end-sprint" => 1,
        "reload-fire-rejected" => 2,
        "merc-reload-sprint-rejected" => 3,
        "sprint-await-release-and-new-press" => 4,
        "merc-reload-pending-sprint-cleared" => 5,
        "reload-held-fire-cleared" => 6,
        "merc-reload-sprint-dispatch-consumed" => 7,
        "manual-reload-completed-sprint-rearmed" => 9,
        _ => 8,
    }
}

fn log_rule(reason: &'static str) {
    if super::diagnostics_enabled() && LOG_COUNTS[rule_log_index(reason)].fetch_add(1, Ordering::Relaxed) < 48 {
        info!("SCBL_COMBAT_INPUT revision=19 action={reason}");
    }
}

fn locally_observed(pawn: *const u8) -> bool {
    let now = unsafe { super::GetTickCount64() };
    super::LOCAL_PAWN_OBSERVATION.with(|slot| slot.get().is_some_and(|(id, at)| id == pawn as usize && now >= at && now - at <= 250))
}

fn controller_pawn(controller: *const u8) -> Option<(*const u8, *const u8, bool)> {
    let pawn = read_pointer(controller, 0x520)?;
    if !locally_observed(pawn) || read_pointer(pawn, 0x938)? != controller {
        return None;
    }
    let flags = read_u32_at(pawn, 0x374)?;
    let (spy, merc) = super::pawn_roles(Some(flags));
    if spy == merc {
        return None;
    }
    let input = read_pointer(pawn, 0x93c)?;
    Some((pawn, input, merc))
}

fn active_state(pawn: *const u8, expected_vtable: usize) -> Option<*const u8> {
    // D873F0 walks exactly these three native state layers. No guessing from
    // animation, mouse state or cached pointers from a previous pawn.
    for layer in 0..3 {
        let Some(manager) = read_pointer(pawn, 0x16e0 + layer * 4) else {
            continue;
        };
        let Some(state) = read_pointer(manager, 0x28) else {
            continue;
        };
        if read_u32_at(state, 0) == Some(expected_vtable as u32) && read_pointer(state, 0x48) == Some(pawn) {
            return Some(state);
        }
    }
    None
}

fn reloading(pawn: *const u8, input: *const u8) -> bool {
    // 15B7402 sets enum 1; 15CE10F resets it to 0. Require both the live
    // class and enum, not merely a requested reload or a stale animation.
    active_state(pawn, RELOAD_VTABLE).is_some() && unsafe { *input.add(0x1a0) == 1 }
}

fn current_reload_names() -> ReloadNames {
    unsafe {
        ReloadNames {
            reload: (RELOAD_REASON_NAME as *const u32).read_unaligned(),
            empty_only: (EMPTY_ONLY_NAME as *const u32).read_unaligned(),
            auto_timer: (AUTO_RELOAD_TIMER_NAME as *const u32).read_unaligned(),
            draw: (DRAW_RELOAD_NAME as *const u32).read_unaligned(),
        }
    }
}

fn reload_kind_with_names(pawn: *const u8, input: *const u8, names: ReloadNames) -> ReloadKind {
    let state = active_state(pawn, RELOAD_VTABLE);
    let active = state.is_some() && unsafe { *input.add(0x1a0) == 1 };
    let reason = state.and_then(|s| read_u32_at(s, 0x58));
    live_reload_kind(active, reason, names)
}

fn reload_kind(pawn: *const u8, input: *const u8) -> ReloadKind {
    let kind = reload_kind_with_names(pawn, input, current_reload_names());
    if super::diagnostics_enabled() {
        let state = active_state(pawn, RELOAD_VTABLE);
        let reason = state.and_then(|s| read_u32_at(s, 0x58));
        let observed = (pawn as usize, state.map_or(0, |s| s as usize), kind, reason);
        let changed = RELOAD_OBSERVATION.with(|slot| slot.replace(Some(observed)) != Some(observed));
        if changed && RELOAD_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 256 {
            // Numeric FName only, no pointers, payload or player identity.
            info!("SCBL_MANUAL_RELOAD revision=19 live={:?} reason={:?}", kind, reason);
        }
    }
    kind
}

fn mapped_action(controller: *const u8, delivered: u32) -> Action {
    let Some(map) = read_pointer(controller, 0x550) else {
        return Action::Other;
    };
    let Some(count) = read_u32_at(map, 0x2c).filter(|n| *n > 0 && *n <= 256) else {
        return Action::Other;
    };
    let Some(entries) = read_pointer(map, 0x28) else {
        return Action::Other;
    };
    classify(unsafe { std::slice::from_raw_parts(entries.cast::<[u8; 4]>(), count as usize) }, delivered)
}

unsafe fn clear_fire(controller: *const u8) {
    let set: unsafe extern "thiscall" fn(*const u8, u8) = std::mem::transmute(SET_FIRE);
    set(controller, 0);
}

unsafe fn stop_sprint(pawn: *const u8, input: *const u8, reason: &'static str) {
    // Clear only the two proven native sprint request/held bits, then send
    // the same EndSprint event consumed by UMPMercSprint::event. Do not
    // directly destroy states or overwrite speed/camera/animation fields.
    let flags = input.add(0x48).cast::<u32>().cast_mut();
    flags.write_unaligned(flags.read_unaligned() & !SPRINT_REQUEST_BITS);
    let event: unsafe extern "thiscall" fn(*const u8, *const u32, *mut c_void, u8) -> u32 = std::mem::transmute(PAWN_EVENT);
    event(pawn, END_SPRINT_NAME as *const u32, std::ptr::null_mut(), 0);
    log_rule(reason);
}

fn reject_manual_reload_sprint(merc: bool, reload: ReloadKind, action: Action) -> bool {
    merc && reload == ReloadKind::Manual && matches!(action, Action::SprintPressed | Action::SprintHeld)
}

// Keep the original complete fanout behind one gate. In r15 the first consumer
// returned early, but F25842 still delivered the same sprint to the state machine.
// No queued state is freed, no animation/event is fabricated, no raw key changed.
// Release and every non-sprint action retain their original dispatch/return value.
fn dispatch_input_with(controller: *const u8, delivered: u32, names: ReloadNames, original: impl FnOnce() -> i32) -> i32 {
    let reject = controller_pawn(controller).is_some_and(|(pawn, input, merc)| {
        if !bind_policy_owner(controller, pawn, input) { return false; }
        let action = mapped_action(controller, delivered);
        // Native consumers never see a reload-consumed press. Observe it here,
        // without writing native input flags, changing reload or replaying input.
        POLICY.with(|slot| {
            let mut policy = slot.get();
            let before = policy.gate;
            policy.observe_sprint_edge(pawn as usize, merc, action);
            slot.set(policy);
            if before != policy.gate && super::diagnostics_enabled() && SPRINT_EDGE_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 96 {
                info!("SCBL_SPRINT_REARM revision=19 action={action:?} from={before:?} to={:?}", policy.gate);
            }
        });
        reject_manual_reload_sprint(merc, reload_kind_with_names(pawn, input, names), action)
    });
    if reject {
        log_rule("merc-reload-sprint-dispatch-consumed");
        1
    } else {
        original()
    }
}

fn input_dispatch(controller: *mut c_void, delivered: u32) -> i32 {
    dispatch_input_with(controller.cast(), delivered, current_reload_names(), || unsafe { InputDispatchHook.call(controller, delivered) })
}

fn input_action(controller: *mut c_void, delivered: u32) {
    if let Some((pawn, input, merc)) = controller_pawn(controller.cast()) {
        if !bind_policy_owner(controller.cast(), pawn, input) {
            unsafe { InputActionHook.call(controller, delivered) };
            return;
        }
        let action = mapped_action(controller.cast(), delivered);
        let reload = reload_kind(pawn, input);
        let sprint = merc && active_state(pawn, MERC_SPRINT_VTABLE).is_some();
        let held = read_u32_at(input, 0x48).unwrap_or_default() & SPRINT_HELD_BIT != 0;
        let decision = POLICY.with(|slot| {
            let mut policy = slot.get();
            let result = policy.decide(pawn as usize, merc, reload, sprint, held, action);
            slot.set(policy);
            result
        });
        if decision.stop_sprint {
            unsafe { stop_sprint(pawn, input, if action == Action::Fire { "merc-fire-end-sprint" } else { "merc-reload-end-sprint" }) };
        }
        if decision.suppress {
            if action == Action::Fire {
                unsafe { clear_fire(controller.cast()) };
                log_rule("reload-fire-rejected");
            } else if merc && reload == ReloadKind::Manual {
                log_rule("merc-reload-sprint-rejected");
            } else {
                log_rule("sprint-await-release-and-new-press");
            }
            return;
        }
    }
    unsafe { InputActionHook.call(controller, delivered) };
}

fn local_reload_controller(state: *const u8) -> Option<*const u8> {
    let pawn = read_pointer(state, 0x48)?;
    let controller = read_pointer(pawn, 0x938)?;
    let (local, input, _) = controller_pawn(controller)?;
    (local == pawn && active_state(pawn, RELOAD_VTABLE) == Some(state) && reloading(pawn, input)).then_some(controller)
}

fn reload_sprint_control(merc: bool, manual_reload: bool, sprint: bool, flags: u32) -> (u32, bool) {
    if merc && manual_reload {
        (flags & !SPRINT_REQUEST_BITS, sprint)
    } else {
        (flags, false)
    }
}

fn reload_update(state: *mut c_void, dt: f32) {
    // Handles fire that was already held BEFORE reload began. Preserve the
    // complete native reload update, ammo transfer and exit timing.
    if let Some(controller) = local_reload_controller(state.cast()) {
        if let Some((pawn, input, merc)) = controller_pawn(controller) {
            let flags = read_u32_at(input, 0x48).unwrap_or_default();
            let sprint = merc && active_state(pawn, MERC_SPRINT_VTABLE).is_some();
            let (requests, stop) = reload_sprint_control(merc, reload_kind(pawn, input) == ReloadKind::Manual, sprint, flags);
            if stop {
                // Sprint began BEFORE reload: end only the confirmed merc
                // sprint state, leaving this reload's native update running.
                unsafe { stop_sprint(pawn, input, "merc-reload-end-sprint") };
            } else if requests != flags {
                // Also discard a pending pre-reload request. No persistent
                // reload latch: once reload ends, normal native input resumes.
                unsafe { input.add(0x48).cast::<u32>().cast_mut().write_unaligned(requests) };
                log_rule("merc-reload-pending-sprint-cleared");
            }
        }
        let firing = read_pointer(controller, 0x520)
            .and_then(|p| read_pointer(p, 0x93c))
            .and_then(|i| read_u32_at(i, 0x44))
            .unwrap_or_default()
            & FIRE_BIT
            != 0;
        if firing {
            unsafe { clear_fire(controller) };
            log_rule("reload-held-fire-cleared");
        }
    }
    unsafe { ReloadUpdateHook.call(state, dt) };
}

fn reject_reload_event(local_reload: bool, event: Option<u32>, force_shoot: u32) -> bool {
    local_reload && force_shoot != 0 && event == Some(force_shoot)
}

fn reload_event(state: *mut c_void, event: *const u32, payload: *mut c_void) -> i32 {
    let controller = local_reload_controller(state.cast());
    let name = (!event.is_null()).then(|| unsafe { event.read_unaligned() });
    let force_shoot = unsafe { (FORCE_SHOOT_NAME as *const u32).read_unaligned() };
    if reject_reload_event(controller.is_some(), name, force_shoot) {
        unsafe { clear_fire(controller.unwrap()) };
        log_rule("reload-force-shoot-consumed");
        return 1;
    }
    // InterruptReload, weapon swap, death and all other native events retain
    // their existing semantics; never freeze the reload state to suppress fire.
    unsafe { ReloadEventHook.call(state, event, payload) }
}

// 156E876 binds sReload callback index 0; 159F6E0 resolves it to 1526C20.
// 1497C2C emits kind 2 for a removed animation; 1497CA6 emits kind 1 for
// a zero primary-animation parameter. For this listener, only kind 0 re-arms.
// A generic state exit (swap/death/cancel) is deliberately NOT completion.
fn normal_reload_notice(kind: Option<u8>, name: Option<u32>, reload_name: u32) -> bool {
    kind == Some(0) && reload_name != 0 && name == Some(reload_name)
}

// The native vtable+194 handler calls 1526C20 with NULL, after confirming its
// +14 completion flag. Other exits also call it with NULL: never accept NULL
// globally. This proof exists only inside the original handler on this thread.
thread_local! { static NATIVE_RELOAD_FINISH: Cell<usize> = const { Cell::new(0) }; }

struct ReloadFinishScope {
    previous: usize,
    owner: Option<InputOwner>,
    lifecycle: u32,
}
impl Drop for ReloadFinishScope {
    fn drop(&mut self) {
        // A nested stop/new lifecycle must not resurrect the outer proof.
        if LIFECYCLE.load(Ordering::Acquire) == self.lifecycle {
            NATIVE_RELOAD_FINISH.with(|slot| slot.set(self.previous));
            NATIVE_RELOAD_OWNER.with(|slot| slot.set(self.owner));
        }
    }
}

fn native_reload_finish_with(state: *mut c_void, notification: *const u8, original: impl FnOnce(*mut c_void, *const u8) -> u8) -> u8 {
    // 1413840 dispatches native notification IDs 1/5 to reload vtable+194.
    // This native 0x18-byte notification is NOT the sReload listener structure.
    let kind = read_u32_at(notification, 0);
    let finished = read_u32_at(notification, 0x14);
    let proof = read_u32_at(state.cast(), 0) == Some(RELOAD_VTABLE as u32) && matches!(kind, Some(1 | 5)) && finished.is_some_and(|flag| flag != 0);
    let previous = NATIVE_RELOAD_FINISH.with(|slot| slot.replace(if proof { state as usize } else { 0 }));
    let owner = if proof {
        local_reload_controller(state.cast()).and_then(|controller| {
            let (pawn, input, _) = controller_pawn(controller)?;
            input_owner(controller, pawn, input)
        })
    } else { None };
    let old_owner = NATIVE_RELOAD_OWNER.with(|slot| slot.replace(owner));
    let _scope = ReloadFinishScope { previous, owner: old_owner, lifecycle: LIFECYCLE.load(Ordering::Acquire) };
    if super::diagnostics_enabled() && NATIVE_FINISH_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 128 {
        info!("SCBL_RELOAD_NATIVE_FINISH revision=19 kind={kind:?} finished={finished:?} proof={proof}");
    }
    original(state, notification)
}

fn reload_native_finish(state: *mut c_void, notification: *const u8) -> u8 {
    native_reload_finish_with(state, notification, |s, n| unsafe { ReloadNativeFinishHook.call(s, n) })
}

fn reload_complete_with(state: *mut c_void, notification: *const u8, names: ReloadNames, animation_name: impl FnOnce() -> u32, original: impl FnOnce(*mut c_void, *const u8) -> u8) -> u8 {
    // Snapshot while native owns the live state. Never dereference state after
    // the original callback, which requests its removal. No across-frame pointer.
    let eligible = local_reload_controller(state.cast()).and_then(|controller| {
        let (pawn, input, merc) = controller_pawn(controller)?;
        if !merc || reload_kind_with_names(pawn, input, names) != ReloadKind::Manual { return None; }
        let owner = input_owner(controller, pawn, input)?;
        let weapon = read_pointer(pawn, 0x4b8)?;
        let weapon_object = unsafe { crate::equipment_native::word(weapon as usize, 0x20) }?;
        Some((controller, pawn, owner, weapon, weapon_object))
    });
    let kind = (!notification.is_null()).then(|| unsafe { notification.read() });
    let name = read_u32_at(notification, 4);
    let proven_native_null = notification.is_null() && !state.is_null()
        && NATIVE_RELOAD_FINISH.with(|slot| slot.get() == state as usize)
        && eligible.is_some_and(|(_, _, owner, _, _)| NATIVE_RELOAD_OWNER.with(|slot| slot.get() == Some(owner)));
    let result = original(state, notification); // exactly once, untouched ABI
                                                // The original lazily interns sReload on its first call, so resolve AFTER it.
    let normal = normal_reload_notice(kind, name, animation_name()) || proven_native_null;
    let mut rearmed = false;
    if result != 0 && normal {
        if let Some((controller, pawn, owner, weapon, weapon_object)) = eligible {
            let same_owner = controller_pawn(controller).is_some_and(|(current, input, merc)| {
                merc && current == pawn && input_owner(controller, current, input) == Some(owner)
                    && read_pointer(current, 0x4b8) == Some(weapon)
                    && unsafe { crate::equipment_native::word(weapon as usize, 0x20) } == Some(weapon_object)
            });
            if same_owner {
                POLICY.with(|slot| {
                    let mut policy = slot.get();
                    if policy.pawn == pawn as usize && policy.gate != SprintGate::Ready {
                        policy.gate = SprintGate::Ready;
                        slot.set(policy);
                        rearmed = true;
                        log_rule("manual-reload-completed-sprint-rearmed");
                    }
                });
            }
        }
    }
    if super::diagnostics_enabled() && COMPLETION_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 128 {
        info!(
            "SCBL_RELOAD_COMPLETION revision=19 kind={kind:?} name={name:?} native_null={proven_native_null} eligible={} normal={normal} result={result} rearmed={rearmed}",
            eligible.is_some()
        );
    }
    result
}

fn reload_complete(state: *mut c_void, notification: *const u8) -> u8 {
    reload_complete_with(
        state,
        notification,
        current_reload_names(),
        || unsafe { (RELOAD_ANIMATION_NAME as *const u32).read_unaligned() },
        |s, n| unsafe { ReloadCompleteHook.call(s, n) },
    )
}

unsafe fn signatures_match() -> bool {
    let signatures: &[(usize, &[u8])] = &[
        // state initialization persists params+4 in state+58 (no hook here).
        (0x0143_D93D, &[0x8b, 0x45, 0x0c, 0x8b, 0x10, 0x89, 0x56, 0x54, 0x8b, 0x48, 0x04, 0x89, 0x4e, 0x58]),
        (0x0152_69EB, &[0x8b, 0x46, 0x58, 0x3b, 0x05, 0x98, 0xfa, 0x39, 0x03]),
        (INPUT_DISPATCH, &[0x55, 0x8b, 0xec, 0x51, 0x56, 0x8b, 0xf1, 0x83, 0xbe, 0x44, 0x01, 0, 0, 0]),
        // Both consumers share EBX delivered ID; guard before either call.
        (
            0x00F2_5832,
            &[0x53, 0x8b, 0xce, 0xe8, 0x96, 0x95, 0xf7, 0xff, 0x8b, 0x17, 0x8b, 0x42, 0x7c, 0x53, 0x8b, 0xcf, 0xff, 0xd0],
        ),
        (0x00F2_5647, &[0xb8, 0x01, 0, 0, 0, 0x5e, 0x8b, 0xe5, 0x5d, 0xc2, 0x04, 0]),
        (INPUT_ACTION, &[0x55, 0x8b, 0xec, 0x53, 0x56, 0x8b, 0xf1, 0x8b, 0x86, 0x20, 0x05, 0, 0]),
        (RELOAD_UPDATE, &[0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0x56, 0x8b, 0xf1, 0x8b, 0x46, 0x48]),
        (RELOAD_EVENT, &[0x55, 0x8b, 0xec, 0x83, 0xec, 0x08, 0x56, 0x57, 0x8b, 0x7d, 0x08]),
        (RELOAD_NATIVE_FINISH, &[0x55, 0x8b, 0xec, 0x53, 0x8b, 0x5d, 0x08, 0x83, 0x7b, 0x14, 0, 0x56, 0x8b, 0xf1, 0x74, 0x52]),
        (0x0156_E728, &[0x6a, 0, 0x8b, 0xce, 0xe8, 0xef, 0x84, 0xfb, 0xff]),
        (0x0156_E73A, &[0x5e, 0x5b, 0x5d, 0xc2, 0x04, 0]),
        (RELOAD_COMPLETE, &[0x55, 0x8b, 0xec, 0x6a, 0xff, 0x68, 0xce, 0xe4, 0x83, 0x02]),
        (0x0152_6CEC, &[0xb0, 0x01, 0x8b, 0x4d, 0xf4, 0x64, 0x89, 0x0d, 0, 0, 0, 0, 0x59, 0x5e, 0x8b, 0xe5, 0x5d, 0xc2, 0x04, 0]),
        (0x0159_F6ED, &[0xc7, 0x40, 0x04, 0x20, 0x6c, 0x52, 0x01]),
        (0x0149_7C28, &[0x8b, 0x4e, 0x38, 0x52, 0xc6, 0x45, 0xc0, 0x02, 0xff, 0xd0]),
        (0x0149_7CC9, &[0x8b, 0x46, 0x3c, 0x8d, 0x55, 0xe0, 0x89, 0x4d, 0xec, 0x8b, 0x4e, 0x38, 0x52, 0xff, 0xd0]),
        (SET_FIRE, &[0x55, 0x8b, 0xec, 0x8a, 0x55, 0x08, 0x88, 0x91, 0x5b, 0x05, 0, 0]),
        (PAWN_EVENT, &[0x55, 0x8b, 0xec, 0x6a, 0xff, 0x68, 0xf8, 0x80, 0x73, 0x02]),
    ];
    signatures.iter().all(|(address, bytes)| std::slice::from_raw_parts(*address as *const u8, bytes.len()) == *bytes)
        && read_u32_at(RELOAD_VTABLE as *const u8, 0x68) == Some(0x0143_d910)
        && read_u32_at(RELOAD_VTABLE as *const u8, 0x74) == Some(0x0152_69c0)
        && read_u32_at(RELOAD_VTABLE as *const u8, 0x94) == Some(RELOAD_UPDATE as u32)
        && read_u32_at(RELOAD_VTABLE as *const u8, 0xb4) == Some(RELOAD_EVENT as u32)
        && read_u32_at(RELOAD_VTABLE as *const u8, 0x194) == Some(RELOAD_NATIVE_FINISH as u32)
        && read_u32_at(MERC_SPRINT_VTABLE as *const u8, 0xb4) == Some(0x014f_1590)
}

pub(super) unsafe fn init() {
    // Called only after the parent's retail DX11 gate. This group is atomic:
    // a missing binding disables the NEW rules, leaving the accepted r11 alone.
    if !signatures_match() {
        error!("SCBL_COMBAT_INPUT enabled=false reason=signature-mismatch");
        return;
    }
    let result = InputActionHook
        .initialize(std::mem::transmute(INPUT_ACTION), input_action)
        .and_then(|_| ReloadUpdateHook.initialize(std::mem::transmute(RELOAD_UPDATE), reload_update))
        .and_then(|_| ReloadEventHook.initialize(std::mem::transmute(RELOAD_EVENT), reload_event))
        .and_then(|_| InputDispatchHook.initialize(std::mem::transmute(INPUT_DISPATCH), input_dispatch))
        .and_then(|_| ReloadCompleteHook.initialize(std::mem::transmute(RELOAD_COMPLETE), reload_complete))
        .and_then(|_| ReloadNativeFinishHook.initialize(std::mem::transmute(RELOAD_NATIVE_FINISH), reload_native_finish))
        .and_then(|_| ReloadCompleteHook.enable())
        .and_then(|_| ReloadNativeFinishHook.enable())
        .and_then(|_| ReloadEventHook.enable())
        .and_then(|_| ReloadUpdateHook.enable())
        .and_then(|_| InputActionHook.enable())
        .and_then(|_| InputDispatchHook.enable());
    if let Err(reason) = result {
        deinit();
        error!("SCBL_COMBAT_INPUT enabled=false reason={reason:?}");
    } else {
        info!("SCBL_COMBAT_INPUT enabled=true revision=19 reload=input-and-state merc_sprint=native-end-and-repress reload_sprint=outer-dispatch-before-state-fanout sprint_edges=before-reload-consume manual_completion=native-normal-and-scoped-null-rearm mapping=native dx=11");
    }
}

pub(super) unsafe fn deinit() {
    LIFECYCLE.fetch_add(1, Ordering::AcqRel);
    POLICY_OWNER.with(|slot| slot.set(None));
    NATIVE_RELOAD_OWNER.with(|slot| slot.set(None));
    RELOAD_OBSERVATION.with(|slot| slot.set(None));
    NATIVE_RELOAD_FINISH.with(|slot| slot.set(0));
    if ReloadNativeFinishHook.is_enabled() {
        let _ = ReloadNativeFinishHook.disable();
    }
    if ReloadCompleteHook.is_enabled() {
        let _ = ReloadCompleteHook.disable();
    }
    if InputDispatchHook.is_enabled() {
        let _ = InputDispatchHook.disable();
    }
    if InputActionHook.is_enabled() {
        let _ = InputActionHook.disable();
    }
    if ReloadUpdateHook.is_enabled() {
        let _ = ReloadUpdateHook.disable();
    }
    if ReloadEventHook.is_enabled() {
        let _ = ReloadEventHook.disable();
    }
    POLICY.with(|slot| slot.set(Policy::default()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_notice(kind: u8, name: u32) -> [u8; 8] {
        let mut bytes = [0; 8];
        bytes[0] = kind;
        bytes[4..8].copy_from_slice(&name.to_le_bytes());
        bytes
    }

    fn latch_fire(f: &DispatchFixture) {
        POLICY.with(|slot| {
            let mut p = Policy::default();
            p.decide(f.pawn.as_ptr() as usize, true, ReloadKind::None, true, true, Action::Fire);
            assert_eq!(p.gate, SprintGate::AwaitRelease);
            slot.set(p);
        });
    }

    #[test]
    fn normal_manual_completion_allows_continuously_held_sprint_after_fire() {
        let mut f = DispatchFixture::new();
        latch_fire(&f);
        for _ in 0..8 {
            assert_eq!(f.dispatch(29, &mut Vec::new()), 1);
        }
        let notice = completion_notice(0, 700);
        let original_calls = Cell::new(0);
        let name_initialized = Cell::new(false);
        let state = f.state.as_mut_ptr().cast();
        let result = reload_complete_with(
            state,
            notice.as_ptr(),
            names(),
            || {
                assert!(name_initialized.get());
                700
            },
            |s, n| {
                assert_eq!(s, state);
                assert_eq!(n, notice.as_ptr());
                original_calls.set(original_calls.get() + 1);
                POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
                name_initialized.set(true);
                f.input[0x1a0] = 0; // native completion
                set32(&mut f.manager, 0x28, 0);
                1
            },
        );
        assert_eq!(result, 1);
        assert_eq!(original_calls.get(), 1);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::Ready));
        assert_eq!(
            dispatch_input_with(f.controller.as_ptr(), 29, names(), || {
                POLICY.with(|s| {
                    let mut p = s.get();
                    assert!(!p.decide(f.pawn.as_ptr() as usize, true, ReloadKind::None, false, true, Action::SprintHeld).suppress);
                    s.set(p);
                });
                37
            }),
            37
        );
    }

    #[test]
    fn completed_manual_reload_still_waits_for_native_state_exit() {
        let f = DispatchFixture::new();
        latch_fire(&f);
        let notice = completion_notice(0, 700);
        let before = f.input.clone();
        reload_complete_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), names(), || 700, |_, _| 1);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::Ready));
        // Completion queued removal but reload is still live: cannot run early.
        assert_eq!(f.dispatch(29, &mut Vec::new()), 1);
        assert_eq!(f.input, before); // no injected key or native input write
    }

    #[test]
    fn cancelled_unknown_and_null_reload_notifications_preserve_fire_gate() {
        let f = DispatchFixture::new();
        latch_fire(&f);
        for (kind, name, expected) in [(1, 700, 700), (2, 700, 700), (255, 700, 700), (0, 701, 700), (0, 0, 0)] {
            let notice = completion_notice(kind, name);
            let calls = Cell::new(0);
            assert_eq!(
                reload_complete_with(
                    f.state.as_ptr().cast_mut().cast(),
                    notice.as_ptr(),
                    names(),
                    || expected,
                    |_, _| {
                        calls.set(calls.get() + 1);
                        1
                    }
                ),
                1
            );
            assert_eq!(calls.get(), 1);
            POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
        }
        reload_complete_with(f.state.as_ptr().cast_mut().cast(), std::ptr::null(), names(), || 700, |_, _| 1);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
    }

    #[test]
    fn automatic_and_unknown_reload_completion_do_not_override_fire_rule() {
        let mut f = DispatchFixture::new();
        latch_fire(&f);
        let notice = completion_notice(0, 700);
        for reason in [names().empty_only, names().auto_timer, names().draw, 99999] {
            set32(&mut f.state, 0x58, reason as usize);
            reload_complete_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), names(), || 700, |_, _| 1);
            POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
        }
    }

    #[test]
    fn inactive_remote_spy_and_wrong_owner_cannot_rearm_manual_completion() {
        for variant in 0..6 {
            let mut f = DispatchFixture::new();
            latch_fire(&f);
            let notice = completion_notice(0, 700);
            match variant {
                0 => f.input[0x1a0] = 0,
                1 => super::super::LOCAL_PAWN_OBSERVATION.with(|s| s.set(None)),
                2 => set32(&mut f.pawn, 0x374, 0x40000),
                3 => set32(&mut f.state, 0x48, 0),
                4 => set32(&mut f.state, 0, 0),
                _ => set32(&mut f.pawn, 0x4b8, 0),
            }
            reload_complete_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), names(), || 700, |_, _| 1);
            POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
        }
    }

    #[test]
    fn weapon_or_controller_change_during_completion_preserves_fire_gate() {
        for variant in 0..3 {
            let mut f = DispatchFixture::new();
            latch_fire(&f);
            let notice = completion_notice(0, 700);
            reload_complete_with(
                f.state.as_ptr().cast_mut().cast(),
                notice.as_ptr(),
                names(),
                || 700,
                |_, _| {
                    match variant {
                        0 => set32(&mut f.pawn, 0x4b8, f.state.as_ptr() as usize),
                        1 => set32(&mut f.controller, 0x520, 0),
                        _ => POLICY.with(|s| {
                            s.set(Policy {
                                pawn: 123,
                                gate: SprintGate::AwaitRelease,
                            })
                        }),
                    }
                    1
                },
            );
            POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
        }
    }

    #[test]
    fn failed_native_completion_preserves_gate_and_exact_return_value() {
        let f = DispatchFixture::new();
        latch_fire(&f);
        let notice = completion_notice(0, 700);
        assert_eq!(reload_complete_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), names(), || 700, |_, _| 0), 0);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
    }

    #[test]
    fn completion_only_rearms_policy_without_starting_sprint_or_firing() {
        let f = DispatchFixture::new();
        let notice = completion_notice(0, 700);
        for gate in [SprintGate::AwaitRelease, SprintGate::AwaitPress, SprintGate::Ready] {
            POLICY.with(|s| s.set(Policy { pawn: f.pawn.as_ptr() as usize, gate }));
            let before = f.input.clone();
            reload_complete_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), names(), || 700, |_, _| 1);
            POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::Ready));
            assert_eq!(f.input, before);
        }
    }

    fn native_finish_notice(kind: u32, finished: u32) -> [u8; 0x18] {
        let mut bytes = [0; 0x18];
        bytes[0..4].copy_from_slice(&kind.to_le_bytes());
        bytes[0x14..0x18].copy_from_slice(&finished.to_le_bytes());
        bytes
    }

    #[test]
    fn native_finished_branch_null_completion_rearms_continuously_held_sprint() {
        for kind in [1, 5] {
            let mut f = DispatchFixture::new();
            latch_fire(&f);
            let notice = native_finish_notice(kind, 1);
            let state = f.state.as_mut_ptr().cast();
            let calls = Cell::new(0);
            let before = f.input.clone();
            let result = native_reload_finish_with(state, notice.as_ptr(), |s, n| {
                assert_eq!((s, n), (state, notice.as_ptr()));
                calls.set(calls.get() + 1);
                // The outer callback itself must not rearm before native completion.
                POLICY.with(|slot| assert_eq!(slot.get().gate, SprintGate::AwaitRelease));
                assert_eq!(
                    reload_complete_with(
                        s,
                        std::ptr::null(),
                        names(),
                        || 0,
                        |s, n| {
                            assert_eq!(s, state);
                            assert!(n.is_null());
                            calls.set(calls.get() + 1);
                            1
                        }
                    ),
                    1
                );
                73 // preserve original AL, not the nested completion's AL
            });
            assert_eq!(result, 73);
            assert_eq!(calls.get(), 2);
            assert_eq!(f.input, before); // no fabricated press, fire or native state write
            POLICY.with(|slot| assert_eq!(slot.get().gate, SprintGate::Ready));
            NATIVE_RELOAD_FINISH.with(|slot| assert_eq!(slot.get(), 0));
            assert_eq!(f.dispatch(29, &mut Vec::new()), 1); // motion still blocked in reload
            f.input[0x1a0] = 0;
            POLICY.with(|slot| {
                let mut p = slot.get();
                assert!(!p.decide(f.pawn.as_ptr() as usize, true, ReloadKind::None, false, true, Action::SprintHeld).suppress);
            });
        }
    }

    #[test]
    fn incomplete_unknown_or_untyped_native_notices_cannot_authorize_null_completion() {
        for (kind, flag) in [(1, 0), (5, 0), (0, 1), (2, 1), (3, 1), (14, 1), (255, 1)] {
            let f = DispatchFixture::new();
            latch_fire(&f);
            let notice = native_finish_notice(kind, flag);
            let state = f.state.as_ptr().cast_mut().cast();
            assert_eq!(
                native_reload_finish_with(state, notice.as_ptr(), |s, _| {
                    NATIVE_RELOAD_FINISH.with(|v| assert_eq!(v.get(), 0));
                    reload_complete_with(s, std::ptr::null(), names(), || 700, |_, _| 1);
                    19
                }),
                19
            );
            POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
        }
        let f = DispatchFixture::new();
        latch_fire(&f);
        native_reload_finish_with(f.state.as_ptr().cast_mut().cast(), std::ptr::null(), |s, _| {
            reload_complete_with(s, std::ptr::null(), names(), || 700, |_, _| 1)
        });
        POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
    }

    #[test]
    fn completion_proof_cannot_escape_native_call_or_authorize_another_state() {
        let f = DispatchFixture::new();
        latch_fire(&f);
        let notice = native_finish_notice(1, 1);
        let mut other = f.state.clone();
        let state = f.state.as_ptr().cast_mut().cast();
        native_reload_finish_with(state, notice.as_ptr(), |_, _| {
            reload_complete_with(other.as_mut_ptr().cast(), std::ptr::null(), names(), || 700, |_, _| 1);
            POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
            1
        });
        NATIVE_RELOAD_FINISH.with(|v| assert_eq!(v.get(), 0));
        reload_complete_with(state, std::ptr::null(), names(), || 700, |_, _| 1);
        POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
    }

    #[test]
    fn nested_noncompletion_masks_outer_proof_and_restores_it() {
        let f = DispatchFixture::new();
        latch_fire(&f);
        let notice = native_finish_notice(5, 1);
        let cancel = native_finish_notice(3, 1);
        let state = f.state.as_ptr().cast_mut().cast();
        native_reload_finish_with(state, notice.as_ptr(), |s, _| {
            native_reload_finish_with(s, cancel.as_ptr(), |s, _| reload_complete_with(s, std::ptr::null(), names(), || 700, |_, _| 1));
            POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
            NATIVE_RELOAD_FINISH.with(|v| assert_eq!(v.get(), s as usize));
            reload_complete_with(s, std::ptr::null(), names(), || 0, |_, _| 1)
        });
        POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::Ready));
        NATIVE_RELOAD_FINISH.with(|v| assert_eq!(v.get(), 0));
    }

    #[test]
    fn native_null_proof_does_not_override_automatic_spy_nonlocal_or_invalid_live_state() {
        for variant in 0..8 {
            let mut f = DispatchFixture::new();
            latch_fire(&f);
            let notice = native_finish_notice(1, 1);
            match variant {
                0 => set32(&mut f.state, 0x58, names().auto_timer as usize),
                1 => set32(&mut f.state, 0x58, 9999),
                2 => set32(&mut f.pawn, 0x374, 0x40000),
                3 => super::super::LOCAL_PAWN_OBSERVATION.with(|v| v.set(None)),
                4 => f.input[0x1a0] = 0,
                5 => set32(&mut f.state, 0, 0),
                6 => set32(&mut f.state, 0x48, 0),
                _ => set32(&mut f.pawn, 0x4b8, 0),
            }
            native_reload_finish_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), |s, _| {
                reload_complete_with(s, std::ptr::null(), names(), || 700, |_, _| 1)
            });
            POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
        }
    }

    #[test]
    fn failed_or_owner_changed_null_completion_keeps_gate_even_with_native_proof() {
        for variant in 0..3 {
            let mut f = DispatchFixture::new();
            latch_fire(&f);
            let notice = native_finish_notice(1, 1);
            native_reload_finish_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), |s, _| {
                reload_complete_with(
                    s,
                    std::ptr::null(),
                    names(),
                    || 700,
                    |_, _| match variant {
                        0 => 0,
                        1 => {
                            set32(&mut f.pawn, 0x4b8, f.state.as_ptr() as usize);
                            1
                        }
                        _ => {
                            set32(&mut f.controller, 0x520, 0);
                            1
                        }
                    },
                )
            });
            POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
        }
    }

    #[test]
    fn unrelated_force_shoot_logging_cannot_consume_completion_budget() {
        assert_ne!(rule_log_index("manual-reload-completed-sprint-rearmed"), rule_log_index("reload-force-shoot-consumed"));
        assert_ne!(rule_log_index("manual-reload-completed-sprint-rearmed"), rule_log_index("sprint-await-release-and-new-press"));
    }

    #[test]
    fn native_completion_scope_never_turns_cancel_listener_into_success() {
        let f = DispatchFixture::new();
        latch_fire(&f);
        let notice = native_finish_notice(1, 1);
        for kind in [1, 2] {
            let cancel = completion_notice(kind, 700);
            native_reload_finish_with(f.state.as_ptr().cast_mut().cast(), notice.as_ptr(), |s, _| {
                reload_complete_with(s, cancel.as_ptr(), names(), || 700, |_, _| 1)
            });
            POLICY.with(|v| assert_eq!(v.get().gate, SprintGate::AwaitRelease));
        }
    }

    fn set32(bytes: &mut [u8], offset: usize, value: usize) {
        bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }

    // This fixture runs the same pointer/mapping/lifetime adapter as the detour,
    // not just the abstract Policy. The native closure models both consumers.
    struct DispatchFixture {
        controller: Vec<u8>,
        pawn: Vec<u8>,
        input: Vec<u8>,
        manager: Vec<u8>,
        state: Vec<u8>,
        _map: Vec<u8>,
        _world: Vec<u8>,
        _entries: Vec<[u8; 4]>,
    }

    impl DispatchFixture {
        fn new() -> Self {
            POLICY.with(|s| s.set(Policy::default()));
            let mut f = Self {
                controller: vec![0; 0x600],
                pawn: vec![0; 0x1700],
                input: vec![0; 0x200],
                manager: vec![0; 0x60],
                state: vec![0; 0xb0],
                _map: vec![0; 0x40],
                _world: vec![0; 0x200],
                _entries: vec![[0x50, 27, 0, 0], [0x4f, 29, 0, 0], [0x51, 28, 0, 0], [2, 42, 0, 0]],
            };
            set32(&mut f.controller, 0x520, f.pawn.as_ptr() as usize);
            set32(&mut f.controller, 0x550, f._map.as_ptr() as usize);
            set32(&mut f._map, 0x28, f._entries.as_ptr() as usize);
            set32(&mut f._map, 0x2c, f._entries.len());
            set32(&mut f.pawn, 0x938, f.controller.as_ptr() as usize);
            set32(&mut f.pawn, 0x93c, f.input.as_ptr() as usize);
            set32(&mut f.pawn, 0x374, 0x80000);
            set32(&mut f.pawn, 0x4b8, f.input.as_ptr() as usize);
            set32(&mut f.pawn, 0x16e4, f.manager.as_ptr() as usize);
            set32(&mut f.manager, 0x28, f.state.as_ptr() as usize);
            set32(&mut f.state, 0, RELOAD_VTABLE);
            set32(&mut f.state, 0x48, f.pawn.as_ptr() as usize);
            f.input[0x1a0] = 1;
            set32(&mut f.pawn, 0x20, 101);
            set32(&mut f.controller, 0x20, 102);
            set32(&mut f._world, 0x20, 103);
            set32(&mut f.input, 0x20, 104);
            set32(&mut f.pawn, 0x144, f._world.as_ptr() as usize);
            set32(&mut f.controller, 0x144, f._world.as_ptr() as usize);
            f.observe();
            assert!(bind_policy_owner(f.controller.as_ptr(), f.pawn.as_ptr(), f.input.as_ptr()));
            f
        }
        fn observe(&self) {
            super::super::LOCAL_PAWN_OBSERVATION.with(|s| s.set(Some((self.pawn.as_ptr() as usize, unsafe { super::super::GetTickCount64() }))));
        }
        fn dispatch(&self, action: u32, calls: &mut Vec<&'static str>) -> i32 {
            dispatch_input_with(self.controller.as_ptr(), action, names(), || {
                calls.push("controller");
                calls.push("state-machine");
                37
            })
        }
    }

    impl Drop for DispatchFixture {
        fn drop(&mut self) {
            POLICY.with(|s| s.set(Policy::default()));
            super::super::LOCAL_PAWN_OBSERVATION.with(|s| s.set(None));
        }
    }

    #[test]
    fn reload_consumed_fresh_press_rearms_fire_gate_before_completion() {
        let mut f = DispatchFixture::new();
        let id = f.pawn.as_ptr() as usize;
        POLICY.with(|slot| {
            let mut p = Policy::default();
            p.decide(id, true, ReloadKind::None, true, true, Action::Fire);
            assert_eq!(p.gate, SprintGate::AwaitRelease);
            slot.set(p);
        });
        // Use the real outer adapter followed by the real inner policy.
        let inner = |action| {
            dispatch_input_with(f.controller.as_ptr(), action, names(), || {
                POLICY.with(|slot| {
                    let mut p = slot.get();
                    let d = p.decide(id, true, ReloadKind::Manual, false, false, mapped_action(f.controller.as_ptr(), action));
                    slot.set(p);
                    assert!(!d.suppress); // release passes
                });
                37
            })
        };
        assert_eq!(inner(28), 37); // physical release during reload
        POLICY.with(|p| assert_eq!(p.get().gate, SprintGate::AwaitPress));
        assert_eq!(inner(27), 1); // fresh press: delay motion, NOT edge bookkeeping
        POLICY.with(|p| assert_eq!(p.get().gate, SprintGate::Ready));
        f.input[0x1a0] = 0;
        set32(&mut f.manager, 0x28, 0);
        assert_eq!(
            dispatch_input_with(f.controller.as_ptr(), 29, names(), || {
                POLICY.with(|slot| {
                    let mut p = slot.get();
                    let d = p.decide(id, true, ReloadKind::None, false, false, Action::SprintHeld);
                    assert!(!d.suppress);
                    slot.set(p);
                });
                37
            }),
            37
        );
        POLICY.with(|p| p.set(Policy::default()));
    }

    #[test]
    fn reload_exit_without_normal_completion_never_fabricates_a_repress() {
        let mut f = DispatchFixture::new();
        let id = f.pawn.as_ptr() as usize;
        POLICY.with(|s| {
            s.set(Policy {
                pawn: id,
                gate: SprintGate::AwaitRelease,
            })
        });
        for _ in 0..20 {
            assert_eq!(f.dispatch(29, &mut Vec::new()), 1);
            POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitRelease));
        }
        f.input[0x1a0] = 0;
        assert_eq!(
            dispatch_input_with(f.controller.as_ptr(), 29, names(), || {
                POLICY.with(|s| {
                    let mut p = s.get();
                    assert!(p.decide(id, true, ReloadKind::None, false, true, Action::SprintHeld).suppress);
                    s.set(p);
                });
                37
            }),
            37
        );
    }

    #[test]
    fn released_toggle_fire_gate_accepts_fresh_press_during_reload() {
        let mut f = DispatchFixture::new();
        let id = f.pawn.as_ptr() as usize;
        POLICY.with(|s| {
            s.set(Policy {
                pawn: id,
                gate: SprintGate::AwaitPress,
            })
        });
        assert_eq!(f.dispatch(27, &mut Vec::new()), 1);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::Ready));
        f.input[0x1a0] = 0;
        assert_eq!(
            dispatch_input_with(f.controller.as_ptr(), 29, names(), || {
                POLICY.with(|s| {
                    let mut p = s.get();
                    assert!(!p.decide(id, true, ReloadKind::None, false, true, Action::SprintHeld).suppress);
                    s.set(p);
                });
                37
            }),
            37
        );
    }

    #[test]
    fn release_after_reload_press_is_forwarded_without_synthetic_sprint() {
        let mut f = DispatchFixture::new();
        let id = f.pawn.as_ptr() as usize;
        POLICY.with(|s| {
            s.set(Policy {
                pawn: id,
                gate: SprintGate::AwaitPress,
            })
        });
        let mut calls = Vec::new();
        assert_eq!(f.dispatch(27, &mut calls), 1);
        assert!(calls.is_empty());
        assert_eq!(f.dispatch(28, &mut calls), 37);
        assert_eq!(calls, ["controller", "state-machine"]);
        calls.clear();
        f.input[0x1a0] = 0;
        assert_eq!(f.dispatch(0, &mut calls), 37);
        assert_eq!(calls, ["controller", "state-machine"]); // original idle input only
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::Ready));
    }

    #[test]
    fn nonlocal_press_cannot_rearm_local_fire_gate() {
        let f = DispatchFixture::new();
        let id = f.pawn.as_ptr() as usize;
        POLICY.with(|s| {
            s.set(Policy {
                pawn: id,
                gate: SprintGate::AwaitPress,
            })
        });
        super::super::LOCAL_PAWN_OBSERVATION.with(|s| s.set(None));
        assert_eq!(f.dispatch(27, &mut Vec::new()), 37);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::AwaitPress));
        f.observe();
        assert_eq!(f.dispatch(27, &mut Vec::new()), 1);
        POLICY.with(|s| assert_eq!(s.get().gate, SprintGate::Ready));
    }

    #[test]
    fn outer_dispatch_consumes_sprint_before_both_native_consumers() {
        let f = DispatchFixture::new();
        let mut calls = Vec::new();
        for action in [27, 29] {
            assert_eq!(f.dispatch(action, &mut calls), 1);
            assert!(calls.is_empty());
        }
        assert_eq!(f.input[0x1a0], 1);
        assert_eq!(read_pointer(f.manager.as_ptr(), 0x28), Some(f.state.as_ptr()));
        assert_eq!(read_u32_at(f.state.as_ptr(), 0x30), Some(0)); // no interrupted flag
    }

    #[test]
    fn outer_dispatch_forwards_release_and_other_actions_exactly_once() {
        let f = DispatchFixture::new();
        for action in [28, 42, 0, 0xbb] {
            let mut calls = Vec::new();
            assert_eq!(f.dispatch(action, &mut calls), 37);
            assert_eq!(calls, ["controller", "state-machine"]);
        }
    }

    #[test]
    fn outer_dispatch_holding_sprint_is_allowed_as_soon_as_native_reload_ends() {
        let mut f = DispatchFixture::new();
        let mut calls = Vec::new();
        assert_eq!(f.dispatch(27, &mut calls), 1);
        for _ in 0..10 {
            assert_eq!(f.dispatch(29, &mut calls), 1);
        }
        assert!(calls.is_empty());
        f.input[0x1a0] = 0; // native completion, not a guessed timer
        set32(&mut f.manager, 0x28, 0);
        assert_eq!(f.dispatch(29, &mut calls), 37); // no release or fresh press
        assert_eq!(calls, ["controller", "state-machine"]);
    }

    #[test]
    fn outer_dispatch_auto_unknown_and_nonlocal_reload_are_not_restricted() {
        let mut f = DispatchFixture::new();
        for reason in [names().empty_only, names().auto_timer, names().draw, 9999] {
            set32(&mut f.state, 0x58, reason as usize);
            let mut calls = Vec::new();
            assert_eq!(f.dispatch(27, &mut calls), 37);
            assert_eq!(calls.len(), 2);
        }
        set32(&mut f.state, 0x58, 0);
        set32(&mut f.pawn, 0x374, 0x40000); // spy
        assert_eq!(f.dispatch(27, &mut Vec::new()), 37);
        set32(&mut f.pawn, 0x374, 0x80000);
        super::super::LOCAL_PAWN_OBSERVATION.with(|s| s.set(None));
        assert_eq!(f.dispatch(27, &mut Vec::new()), 37);
        f.observe();
        set32(&mut f.state, 0x48, 1234); // wrong owner
        assert_eq!(f.dispatch(27, &mut Vec::new()), 37);
    }

    #[test]
    fn inner_return_alone_does_not_consume_outer_native_fanout() {
        let mut policy = Policy::default();
        let decision = policy.decide(1, true, ReloadKind::Manual, false, false, Action::SprintPressed);
        assert!(decision.suppress); // r15 can truthfully log rejected here
        let mut calls = Vec::new();
        if !decision.suppress {
            calls.push("controller");
        }
        calls.push("state-machine"); // F25842 still executes after inner return
        assert_eq!(calls, ["state-machine"]);
        let f = DispatchFixture::new();
        assert_eq!(f.dispatch(27, &mut calls), 1);
        assert_eq!(calls, ["state-machine"]); // no second delivery
    }

    #[test]
    fn live_reload_requires_exact_layer_owner_class_and_enum() {
        let mut pawn = vec![0u8; 0x1700];
        let mut manager = vec![0u8; 0x50];
        let mut state = vec![0u8; 0x80];
        let mut input = vec![0u8; 0x200];
        set32(&mut pawn, 0x16e4, manager.as_ptr() as usize);
        set32(&mut manager, 0x28, state.as_ptr() as usize);
        set32(&mut state, 0, RELOAD_VTABLE);
        set32(&mut state, 0x48, pawn.as_ptr() as usize);
        input[0x1a0] = 1;
        assert!(reloading(pawn.as_ptr(), input.as_ptr()));
        assert_eq!(reload_kind_with_names(pawn.as_ptr(), input.as_ptr(), names()), ReloadKind::Manual);
        input[0x1a0] = 0;
        assert!(!reloading(pawn.as_ptr(), input.as_ptr()));
        input[0x1a0] = 1;
        set32(&mut state, 0x48, 1234);
        assert!(!reloading(pawn.as_ptr(), input.as_ptr()));
        set32(&mut state, 0x48, pawn.as_ptr() as usize);
        set32(&mut state, 0, MERC_SPRINT_VTABLE);
        assert!(!reloading(pawn.as_ptr(), input.as_ptr()));
        assert_eq!(active_state(pawn.as_ptr(), MERC_SPRINT_VTABLE), Some(state.as_ptr()));
        set32(&mut manager, 0x28, 0);
        assert_eq!(active_state(pawn.as_ptr(), MERC_SPRINT_VTABLE), None);
    }

    #[test]
    fn native_mapping_adapter_obeys_bounds_and_rebindings() {
        let mut controller = vec![0u8; 0x600];
        let mut map = vec![0u8; 0x40];
        let entries = [[0x50u8, 27u8, 0u8, 0u8], [0x51, 28, 0, 0], [2, 42, 0, 0]];
        set32(&mut controller, 0x550, map.as_ptr() as usize);
        set32(&mut map, 0x28, entries.as_ptr() as usize);
        set32(&mut map, 0x2c, entries.len());
        assert_eq!(mapped_action(controller.as_ptr(), 27), Action::SprintPressed);
        assert_eq!(mapped_action(controller.as_ptr(), 28), Action::SprintReleased);
        assert_eq!(mapped_action(controller.as_ptr(), 42), Action::Fire);
        set32(&mut map, 0x2c, 257);
        assert_eq!(mapped_action(controller.as_ptr(), 42), Action::Other);
    }

    #[test]
    fn remote_missing_or_replaced_controller_is_passthrough() {
        let mut controller = vec![0u8; 0x600];
        let mut pawn = vec![0u8; 0x1700];
        let input = vec![0u8; 0x200];
        set32(&mut controller, 0x520, pawn.as_ptr() as usize);
        set32(&mut pawn, 0x938, controller.as_ptr() as usize);
        set32(&mut pawn, 0x93c, input.as_ptr() as usize);
        set32(&mut pawn, 0x374, 0x80000);
        super::super::LOCAL_PAWN_OBSERVATION.with(|slot| slot.set(None));
        assert!(controller_pawn(controller.as_ptr()).is_none());
        super::super::LOCAL_PAWN_OBSERVATION.with(|slot| slot.set(Some((pawn.as_ptr() as usize, unsafe { super::super::GetTickCount64() }))));
        assert_eq!(controller_pawn(controller.as_ptr()), Some((pawn.as_ptr(), input.as_ptr(), true)));
        set32(&mut pawn, 0x938, 1234);
        assert!(controller_pawn(controller.as_ptr()).is_none());
        set32(&mut pawn, 0x938, controller.as_ptr() as usize);
        set32(&mut pawn, 0x374, 0x20000);
        assert!(controller_pawn(controller.as_ptr()).is_none());
        super::super::LOCAL_PAWN_OBSERVATION.with(|slot| slot.set(None));
    }

    #[test]
    fn native_mapping_not_virtual_keys() {
        let bindings = [[2, 7, 0, 0], [3, 8, 0, 0], [0x50, 90, 0, 0], [0x51, 91, 0, 0], [0x4f, 92, 0, 0]];
        assert_eq!(classify(&bindings, 7), Action::Fire);
        assert_eq!(classify(&bindings, 8), Action::FireReleased);
        assert_eq!(classify(&bindings, 90), Action::SprintPressed);
        assert_eq!(classify(&bindings, 91), Action::SprintReleased);
        assert_eq!(classify(&bindings, 92), Action::SprintHeld);
        assert_eq!(classify(&bindings, 16), Action::Other);
    }
    #[test]
    fn alternate_fire_mapping_and_invalid_ids() {
        assert_eq!(classify(&[[0xad, 30, 0, 0]], 30), Action::Fire);
        assert_eq!(classify(&[[0xae, 31, 0, 0]], 31), Action::FireReleased);
        assert_eq!(classify(&[[2, 0, 0, 0]], 0), Action::Other);
        assert_eq!(classify(&[[2, 255, 0, 0]], 255), Action::Other);
    }
    #[test]
    fn reload_fire_blocked_for_both_factions() {
        for merc in [false, true] {
            assert!(Policy::default().decide(1, merc, ReloadKind::Manual, false, false, Action::Fire).suppress);
        }
    }
    #[test]
    fn reload_never_blocks_release_or_unrelated_inputs() {
        for action in [Action::Other, Action::FireReleased, Action::SprintReleased] {
            assert_eq!(Policy::default().decide(1, true, ReloadKind::Manual, false, false, action), Decision::default());
        }
    }
    #[test]
    fn completed_reload_allows_next_fire() {
        let mut p = Policy::default();
        assert!(p.decide(1, false, ReloadKind::Manual, false, false, Action::Fire).suppress);
        assert!(!p.decide(1, false, ReloadKind::None, false, false, Action::Fire).suppress);
    }
    #[test]
    fn merc_fire_ends_sprint_without_suppressing_shot() {
        assert_eq!(
            Policy::default().decide(1, true, ReloadKind::None, true, true, Action::Fire),
            Decision { suppress: false, stop_sprint: true }
        );
    }
    #[test]
    fn holding_sprint_cannot_restart_after_fire() {
        let mut p = Policy::default();
        p.decide(1, true, ReloadKind::None, true, true, Action::Fire);
        for _ in 0..100 {
            for a in [Action::SprintHeld, Action::SprintPressed] {
                assert!(p.decide(1, true, ReloadKind::None, false, true, a).suppress);
            }
        }
    }
    #[test]
    fn release_then_real_new_press_rearms_sprint() {
        let mut p = Policy::default();
        p.decide(1, true, ReloadKind::None, true, true, Action::Fire);
        assert!(!p.decide(1, true, ReloadKind::None, false, false, Action::SprintReleased).suppress);
        assert!(p.decide(1, true, ReloadKind::None, false, false, Action::SprintHeld).suppress);
        assert!(!p.decide(1, true, ReloadKind::None, false, true, Action::SprintPressed).suppress);
        assert_eq!(p.gate, SprintGate::Ready);
    }
    #[test]
    fn toggle_sprint_already_released_needs_only_next_press() {
        let mut p = Policy::default();
        p.decide(1, true, ReloadKind::None, true, false, Action::Fire);
        assert!(p.decide(1, true, ReloadKind::None, false, false, Action::SprintHeld).suppress);
        assert!(!p.decide(1, true, ReloadKind::None, false, true, Action::SprintPressed).suppress);
    }
    #[test]
    fn spy_sprint_and_normal_merc_fire_unchanged() {
        assert_eq!(Policy::default().decide(1, false, ReloadKind::None, true, true, Action::Fire), Decision::default());
        assert_eq!(Policy::default().decide(1, true, ReloadKind::None, false, true, Action::Fire), Decision::default());
    }
    #[test]
    fn pawn_replacement_does_not_inherit_sprint_latch() {
        let mut p = Policy::default();
        p.decide(1, true, ReloadKind::None, true, true, Action::Fire);
        assert!(!p.decide(2, true, ReloadKind::None, false, true, Action::SprintPressed).suppress);
    }
    #[test]
    fn reload_plus_sprint_stops_run_but_cannot_fire() {
        assert_eq!(
            Policy::default().decide(1, true, ReloadKind::Manual, true, true, Action::Fire),
            Decision { suppress: true, stop_sprint: true }
        );
    }
    #[test]
    fn aiming_without_fire_does_not_cancel_sprint() {
        assert_eq!(Policy::default().decide(1, true, ReloadKind::None, true, true, Action::Other), Decision::default());
    }
    #[test]
    fn reload_event_only_consumes_confirmed_local_force_shoot() {
        assert!(reject_reload_event(true, Some(7), 7));
        assert!(!reject_reload_event(false, Some(7), 7));
        assert!(!reject_reload_event(true, Some(8), 7));
        assert!(!reject_reload_event(true, None, 7));
        assert!(!reject_reload_event(true, Some(0), 0));
    }
    #[test]
    fn sprint_mask_preserves_all_unrelated_input_bits() {
        assert_eq!(u32::MAX & !SPRINT_REQUEST_BITS, 0x3fff_ffff);
        assert_eq!(FIRE_BIT, 0x0200_0000);
    }

    #[test]
    fn merc_manual_reload_blocks_sprint_start_and_hold() {
        for action in [Action::SprintPressed, Action::SprintHeld] {
            assert_eq!(
                Policy::default().decide(1, true, ReloadKind::Manual, false, true, action),
                Decision { suppress: true, stop_sprint: false }
            );
            assert_eq!(
                Policy::default().decide(1, true, ReloadKind::Manual, true, true, action),
                Decision { suppress: true, stop_sprint: true }
            );
        }
    }

    #[test]
    fn spy_reload_does_not_gain_merc_sprint_restriction() {
        for action in [Action::SprintPressed, Action::SprintHeld, Action::SprintReleased] {
            assert_eq!(Policy::default().decide(1, false, ReloadKind::Manual, true, true, action), Decision::default());
        }
    }

    #[test]
    fn reload_completion_restores_native_sprint_without_new_latch() {
        let mut policy = Policy::default();
        assert!(policy.decide(1, true, ReloadKind::Manual, false, true, Action::SprintPressed).suppress);
        assert_eq!(policy.gate, SprintGate::Ready);
        assert!(!policy.decide(1, true, ReloadKind::None, false, true, Action::SprintHeld).suppress);
    }

    #[test]
    fn sprint_press_during_reload_rearms_fire_gate_but_delays_motion() {
        let mut policy = Policy::default();
        policy.decide(1, true, ReloadKind::None, true, true, Action::Fire);
        policy.decide(1, true, ReloadKind::Manual, false, false, Action::SprintReleased);
        assert_eq!(policy.gate, SprintGate::AwaitPress);
        assert!(policy.decide(1, true, ReloadKind::Manual, false, true, Action::SprintPressed).suppress);
        assert_eq!(policy.gate, SprintGate::Ready);
        assert!(!policy.decide(1, true, ReloadKind::None, false, true, Action::SprintHeld).suppress);
    }

    #[test]
    fn merc_reload_ends_existing_sprint_and_clears_pending_requests() {
        assert_eq!(reload_sprint_control(true, true, true, u32::MAX), (0x3fff_ffff, true));
        assert_eq!(reload_sprint_control(true, true, false, u32::MAX), (0x3fff_ffff, false));
        assert_eq!(reload_sprint_control(true, true, false, 17), (17, false));
    }

    #[test]
    fn reload_sprint_cleanup_preserves_spy_and_non_reload_state() {
        for (merc, reload) in [(false, false), (false, true), (true, false)] {
            assert_eq!(reload_sprint_control(merc, reload, true, u32::MAX), (u32::MAX, false));
        }
    }

    #[test]
    fn automatic_reload_keeps_native_sprint_but_still_cannot_fire() {
        for action in [Action::SprintPressed, Action::SprintHeld, Action::SprintReleased] {
            assert_eq!(Policy::default().decide(1, true, ReloadKind::Automatic, true, true, action), Decision::default());
        }
        assert!(Policy::default().decide(1, true, ReloadKind::Automatic, false, false, Action::Fire).suppress);
        assert_eq!(reload_sprint_control(true, false, true, u32::MAX), (u32::MAX, false));
    }

    fn names() -> ReloadNames {
        ReloadNames {
            reload: 123,
            empty_only: 456,
            auto_timer: 789,
            draw: 321,
        }
    }

    #[test]
    fn live_default_and_explicit_requests_are_manual_without_key_hold() {
        for reason in [Some(0), Some(123)] {
            assert_eq!(live_reload_kind(true, reason, names()), ReloadKind::Manual);
            let mut policy = Policy::default();
            // The original event/key can have returned hundreds of frames ago.
            for _ in 0..600 {
                let kind = live_reload_kind(true, reason, names());
                assert!(policy.decide(1, true, kind, false, false, Action::SprintPressed).suppress);
                assert!(policy.decide(1, true, kind, false, true, Action::SprintHeld).suppress);
            }
        }
    }

    #[test]
    fn queued_native_transition_does_not_need_event_scope_or_pending_latch() {
        assert_eq!(live_reload_kind(false, Some(0), names()), ReloadKind::None);
        assert_eq!(live_reload_kind(false, Some(123), names()), ReloadKind::None);
        // Native queue commits later; only the actual live state now matters.
        assert_eq!(live_reload_kind(true, Some(123), names()), ReloadKind::Manual);
        assert_eq!(live_reload_kind(false, Some(123), names()), ReloadKind::None);
    }

    #[test]
    fn automatic_draw_and_unknown_reasons_never_acquire_manual_sprint_rule() {
        for reason in [Some(456), Some(789), Some(321), Some(999), None] {
            let kind = live_reload_kind(true, reason, names());
            assert_eq!(kind, ReloadKind::Automatic);
            assert!(!Policy::default().decide(1, true, kind, false, true, Action::SprintPressed).suppress);
            assert!(Policy::default().decide(1, true, kind, false, false, Action::Fire).suppress);
        }
    }

    #[test]
    fn uninitialized_or_aliased_reason_names_fail_open_for_new_sprint_rule_only() {
        for invalid in [ReloadNames { reload: 0, ..names() }, ReloadNames { empty_only: 123, ..names() }] {
            assert_eq!(live_reload_kind(true, Some(0), invalid), ReloadKind::Automatic);
            assert_eq!(live_reload_kind(false, Some(0), invalid), ReloadKind::None);
        }
    }

    #[test]
    fn live_adapter_follows_end_cancel_and_reused_native_state_without_stale_tag() {
        let mut pawn = vec![0u8; 0x1700];
        let mut manager = vec![0u8; 0x50];
        let mut state = vec![0u8; 0x80];
        let mut input = vec![0u8; 0x200];
        set32(&mut pawn, 0x16e0, manager.as_ptr() as usize);
        set32(&mut manager, 0x28, state.as_ptr() as usize);
        set32(&mut state, 0, RELOAD_VTABLE);
        set32(&mut state, 0x48, pawn.as_ptr() as usize);
        set32(&mut state, 0x58, 123);
        let read = |p: &[u8], i: &[u8]| reload_kind_with_names(p.as_ptr(), i.as_ptr(), names());
        assert_eq!(read(&pawn, &input), ReloadKind::None);
        input[0x1a0] = 1;
        assert_eq!(read(&pawn, &input), ReloadKind::Manual);
        input[0x1a0] = 0;
        assert_eq!(read(&pawn, &input), ReloadKind::None);
        set32(&mut state, 0x58, 789);
        input[0x1a0] = 1;
        assert_eq!(read(&pawn, &input), ReloadKind::Automatic);
        set32(&mut manager, 0x28, 0);
        assert_eq!(read(&pawn, &input), ReloadKind::None);
        set32(&mut manager, 0x28, state.as_ptr() as usize);
        set32(&mut state, 0x48, 1234); // another pawn cannot inherit state
        assert_eq!(read(&pawn, &input), ReloadKind::None);
    }

    #[test]
    fn repeated_sprint_rule_cannot_exhaust_manual_reload_diagnostic_budget() {
        assert_ne!(rule_log_index("sprint-await-release-and-new-press"), rule_log_index("merc-reload-sprint-rejected"));
        assert_ne!(rule_log_index("reload-held-fire-cleared"), rule_log_index("merc-reload-end-sprint"));
        let before = RELOAD_LOG_COUNT.load(Ordering::Relaxed);
        let counter = &LOG_COUNTS[rule_log_index("sprint-await-release-and-new-press")];
        counter.fetch_add(1000, Ordering::Relaxed);
        assert_eq!(RELOAD_LOG_COUNT.load(Ordering::Relaxed), before);
    }
}

#[cfg(test)]
include!("combat_input_lifecycle_tests.rs");
