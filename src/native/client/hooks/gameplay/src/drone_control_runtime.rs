//! R58 retail DX11 runtime wiring. A single owner installs the complete drone
//! transaction or nothing. No keyboard polling, new spawn, synthetic charge or
//! global state-machine patch. All unknown actors and ordinary casts stay native.
use super::*;
#[path = "drone_control_audio.rs"]
mod audio;
#[path = "drone_shutdown.rs"]
mod shutdown;
#[path = "drone_exit_presentation.rs"]
mod exit_presentation;

pub(crate) unsafe fn hud_wait(ability: usize) -> Option<u8> {
    if !READY.load(Ordering::Acquire) {
        return None;
    }
    flow().hud_wait(ability)
}
use crate::equipment_native::{readable, signature};
use retour::static_detour;
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        OnceLock,
    },
};
static BODY_ELECTRIC_INSTALLED: AtomicBool = AtomicBool::new(false);
static OWNER_HIT_INSTALLED: AtomicBool = AtomicBool::new(false);
static BODY_ELECTRIC_PROTECTION: AtomicU32 = AtomicU32::new(0);
static OWNER_HIT_PROTECTION: AtomicU32 = AtomicU32::new(0);
static READY: AtomicBool = AtomicBool::new(false);
static BANK: AtomicUsize = AtomicUsize::new(0);
static LOGS: AtomicU32 = AtomicU32::new(0);
static FLOW: OnceLock<ActionFlow> = OnceLock::new();
fn flow() -> &'static ActionFlow {
    FLOW.get_or_init(ActionFlow::default)
}
const ACTION: usize = 0;
const EXIT_COPY: usize = 0x200;
const BASE_CHECK: usize = 0x400;
const DRONE_CHECK: usize = 0x500;
const PRESS_COPY: usize = 0x600;
const NETWORK: usize = 0x800;
const PREPARE: usize = 0xa00;
const QUEUE: usize = 0xc00;
const CONTROL_START: usize = 0xe00;
const PLACEMENT: usize = 0x1000;
const EMP_BODY_COPY: usize = 0x1200;
const OWNER_HIT_CONTEXT: usize = 0x1400;
const BODY_ELECTRIC_CONTEXT: usize = 0x1420;
const BANK_SIZE: usize = 0x2000;
static_detour! {
 static Action:unsafe extern "thiscall" fn(usize,*const u32,usize)->u32;
 static Exit:unsafe extern "thiscall" fn(usize);
 static TransOut:unsafe extern "thiscall" fn(usize,u32);
 static Entry:unsafe extern "thiscall" fn(usize);
 static ControlStart:unsafe extern "thiscall" fn(usize);
 static PressAction:unsafe extern "thiscall" fn(usize);
 static NetworkActivate:unsafe extern "thiscall" fn(usize);
 static Activate:unsafe extern "thiscall" fn(usize);
 static Deactivate:unsafe extern "thiscall" fn(usize);
 static Cancel:unsafe extern "thiscall" fn(usize);
 static BodyEmp:unsafe extern "thiscall" fn(usize,usize,f32,u32,u32,u32);
 static Tick:unsafe extern "thiscall" fn(usize,f32);
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protection: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protection: u32, old: *mut u32) -> i32;
    fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(process: *mut c_void, p: *const c_void, n: usize) -> i32;
}
unsafe fn bytes(at: usize, n: usize) -> Option<&'static [u8]> {
    readable(at, n).then(|| std::slice::from_raw_parts(at as *const u8, n))
}
unsafe fn ability_for_state(state: usize) -> usize {
    word(state, 0x48)
        .and_then(|p| word(p as usize, 0xcf4))
        .and_then(|a| word(a as usize, 0x5c))
        .unwrap_or(0) as usize
}
unsafe fn blocked(owner: usize) -> bool {
    let Some(name) = word(0x339f854, 0).filter(|n| *n != 0) else {
        return true;
    };
    let predicate: unsafe extern "thiscall" fn(usize, u32) -> u32 =
        std::mem::transmute(0xd5e8e0usize);
    predicate(owner, name) != 0
}
fn log(event: &str) {
    if LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
        tracing::info!(
            "SCBL_MOD_DRONE_CONTROL revision=58 event={} input=native_semantic",
            event
        );
    }
}
fn action(state: usize, name: *const u32, payload: usize) -> u32 {
    unsafe {
        if !READY.load(Ordering::Acquire) {
            return Action.call(state, name, payload);
        }
        let call: unsafe extern "thiscall" fn(usize, *const u32, usize) -> u32 =
            std::mem::transmute(BANK.load(Ordering::Acquire) + ACTION);
        call(state, name, payload)
    }
}
unsafe extern "thiscall" fn action_bridge(state: usize, name: *const u32, payload: usize) -> u32 {
    let native: unsafe extern "thiscall" fn(usize, *const u32, usize) -> u32 =
        std::mem::transmute(0x14cf100usize);
    let Some(value) = word(name as usize, 0) else {
        return native(state, name, payload);
    };
    let result = flow().action(
        state,
        ability_for_state(state),
        value,
        word(0x339f658, 0).unwrap_or(0),
        word(0x339fb54, 0).unwrap_or(0),
        Layout::RETAIL,
        |owner| blocked(owner),
        |value| native(state, &value, payload),
    );
    // Keep native HUD transition at actual MPExit; an early Standard event
    // suppresses the native shutdown visual without shortening TransOut.
    result
}
fn transout(state: usize, native_mode: u32) {
    unsafe {
        // Freeze the last controlled view BEFORE native TransOut switches cameras.
        if READY.load(Ordering::Acquire) && word(state, 0) == Some(Layout::RETAIL.state) {
            if let Some(body) = word(state, 0x138).filter(|v| *v != 0) {
                crate::drone_tags::freeze_parked_view(body as usize);
            }
        }
        let mode = shutdown::transition_mode(native_mode, || {
            if READY.load(Ordering::Acquire) {
                shutdown::transout_mode(
                    state,
                    flow().transout_mode(state, ability_for_state(state), native_mode),
                )
            } else {
                native_mode
            }
        });
        if mode != native_mode {
            log("native_immediate_transout");
        }
        exit_presentation::transition(
            mode,
            || READY.load(Ordering::Acquire)
                && flow().transout_mode(state, ability_for_state(state), native_mode) == 1,
            || { let _ = exit_presentation::begin(state); },
            |original_mode| TransOut.call(state, original_mode),
        );
    }
}
fn exit(state: usize) {
    unsafe {
        if !READY.load(Ordering::Acquire) {
            Exit.call(state);
            return;
        }
        let call: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(BANK.load(Ordering::Acquire) + EXIT_COPY);
        flow().exit(
            state,
            ability_for_state(state),
            |p| blocked(p),
            || call(state),
        );
        let ability = ability_for_state(state);
        if flow().keep_parked_on_owner_hit(ability) {
            if let Some(body) = word(ability, 0xa0).filter(|v| *v != 0) {
                audio::park(body as usize);
            }
        }
        restore_parked_weapon(ability_for_state(state));
        log("exit");
    }
}
unsafe extern "thiscall" fn fall_bridge(body: usize) {
    forced_fall_site(body, || {
        if let Some(target) = word(body, 0).and_then(|v| word(v as usize, 0x754)) {
            let f: unsafe extern "thiscall" fn(usize) = std::mem::transmute(target as usize);
            f(body);
        }
    });
}
fn entry(state: usize) {
    unsafe {
        if READY.load(Ordering::Acquire) {
            // Native Entry can queue an animation; do not consume its ticket yet.
            Entry.call(state);
            log("entry");
        } else {
            Entry.call(state);
        }
    }
}
fn control_start(state: usize) {
    unsafe {
        if !READY.load(Ordering::Acquire) || word(state, 0) != Some(Layout::RETAIL.state) {
            ControlStart.call(state);
            return;
        }
        let call: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(BANK.load(Ordering::Acquire) + CONTROL_START);
        if let Some(body) = word(ability_for_state(state), 0xa0).filter(|v| *v != 0) {
            crate::drone_tags::resume_drone_view(body as usize);
        }
        flow().entry(state, ability_for_state(state), || call(state));
        if let Some(body) = word(ability_for_state(state), 0xa0).filter(|v| *v != 0) {
            audio::resume(body as usize);
        }
        log("control_start");
    }
}
unsafe extern "thiscall" fn placement_bridge(state: usize) {
    let call: unsafe extern "thiscall" fn(usize) =
        std::mem::transmute(BANK.load(Ordering::Acquire) + PLACEMENT);
    call(state);
}
unsafe extern "thiscall" fn placement_transform_bridge(
    body: usize,
    pos: *const u32,
    rot: *const u32,
) {
    reentry_placement_site(body, pos, rot, |p, r| {
        let native: unsafe extern "thiscall" fn(usize, *const u32, *const u32) =
            std::mem::transmute(0xe68a20usize);
        native(body, p, r);
    });
}
fn press(ability: usize) {
    unsafe {
        if !READY.load(Ordering::Acquire) {
            PressAction.call(ability);
            return;
        }
        let call: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(BANK.load(Ordering::Acquire) + PRESS_COPY);
        call(ability);
    }
}
struct QueueCapture {
    owner: usize,
    result: Cell<Option<(usize, u32)>>,
}
thread_local! {static QUEUE_SCOPE:Cell<*const QueueCapture>=const{Cell::new(std::ptr::null())};}
unsafe extern "thiscall" fn dispatch_bridge(
    owner: usize,
    state: usize,
    params: usize,
    priority: u32,
) {
    let layer = (word(state, 0) == Some(Layout::RETAIL.state)
        && word(state, 0x48) == Some(owner as u32))
    .then(|| word(state, 0x34))
    .flatten(); // Before rejection can release state.
    let native: unsafe extern "thiscall" fn(usize, usize, usize, u32) =
        std::mem::transmute(0xd5e070usize);
    native(owner, state, params, priority);
    QUEUE_SCOPE.with(|s| {
        let p = s.get();
        if !p.is_null() && (*p).owner == owner {
            if let Some(layer) = layer.filter(|layer| queued_state(owner, state, *layer)) {
                (*p).result.set(Some((state, layer)));
            }
        }
    });
}
unsafe fn prepare(ability: usize, owner: usize, body: usize) -> Option<(usize, u32)> {
    let Some(inventory) = word(owner, 0x1410).filter(|v| *v != 0) else {
        log("reentry_inventory_missing");
        return None;
    };
    let inventory = inventory as usize;
    let lookup: unsafe extern "thiscall" fn(usize, u32) -> usize =
        std::mem::transmute(0xd5fb60usize);
    if lookup(inventory, 0x28) != body {
        log("reentry_inventory_body_mismatch");
        return None;
    }
    // The native gadget inventory may legitimately select 0x28 after other
    // gadgets are exhausted. Same-body lookup plus the parked ticket authorize
    // resumption; prepare_window preserves the previous slot in this case.
    let capture = QueueCapture {
        owner,
        result: Cell::new(None),
    };
    struct Restore(*const QueueCapture);
    impl Drop for Restore {
        fn drop(&mut self) {
            QUEUE_SCOPE.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(QUEUE_SCOPE.with(|s| s.replace(&capture)));
    let call: unsafe extern "thiscall" fn(usize) =
        std::mem::transmute(BANK.load(Ordering::Acquire) + PREPARE);
    call(ability);
    let result = capture.result.get();
    if result.is_none() {
        log("reentry_native_queue_rejected");
        // Match the saved native previous slot only when the same live graph remains.
        if word(ability, 0x6c) == Some(owner as u32) && word(ability, 0xa0) == Some(body as u32) {
            if let Some(slot) = word(ability, 0xb4).map(|v| v & 255).filter(|v| *v != 0x28) {
                let select: unsafe extern "thiscall" fn(usize, u32) =
                    std::mem::transmute(0xd762d0usize);
                select(owner, slot);
            }
        }
    }
    result
}
unsafe extern "thiscall" fn active_press(ability: usize) {
    refresh_and_restore(ability);
    restore_parked_weapon(ability);
    let result = flow().press_confirmed(
        ability,
        |_| native_entry_allowed(BANK.load(Ordering::Acquire) + DRONE_CHECK, ability),
        |owner, body| prepare(ability, owner, body),
    );
    match result {
        Press::Native => PressAction.call(ability),
        Press::Wait(_) => log("reentry_wait"),
        Press::Queued => log("reentry_queued"),
        Press::Restricted => log("reentry_restricted"),
    }
}
fn network(node: usize) {
    unsafe {
        if !READY.load(Ordering::Acquire) {
            NetworkActivate.call(node);
            return;
        }
        let f: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(BANK.load(Ordering::Acquire) + NETWORK);
        f(node);
    }
}
unsafe extern "thiscall" fn transform_bridge(body: usize, pos: *const u32, rot: *const u32) {
    reentry_position_site(body, pos, rot, |p, r| {
        if let Some(target) = word(body, 0).and_then(|v| word(v as usize, 0x43c)) {
            let f: unsafe extern "thiscall" fn(usize, *const u32, *const u32) =
                std::mem::transmute(target as usize);
            f(body, p, r);
        }
    });
}

unsafe fn restore_parked_weapon(ability: usize) {
    if let Some((owner, slot)) = flow().parked_weapon_restore(ability) {
        // E58F00 normally restores this slot only when the body is destroyed.
        // Retention must restore it on voluntary exit too; otherwise PREPARE
        // correctly rejects drone-slot 0x28 forever (and the next cast saves 0x28).
        let select: unsafe extern "thiscall" fn(usize, u32) = std::mem::transmute(0xd762d0usize);
        select(owner, slot);
        let actual = word(owner, 0x1410)
            .and_then(|p| word(p as usize, 0x30))
            .map(|v| v & 255);
        log(if actual == Some(slot) {
            "parked_gadget_restored"
        } else {
            "parked_gadget_depleted_keep_drone"
        });
    }
}
unsafe fn terminate_parked(ability: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(name) = word(0x339fa30, 0).filter(|v| *v != 0) else {
        return;
    };
    let in_state: unsafe extern "thiscall" fn(usize, u32) -> u32 =
        std::mem::transmute(0xd5e8e0usize);
    // Inside MP control native Disable owns the fall. Outside it, that message
    // has no receiver; use the same native fall and replication endpoint.
    if terminate_parked_with(
        ability,
        Layout::RETAIL,
        |owner| in_state(owner, name) != 0,
        |body| crate::drone_fall::disable_parked(body),
    ) {
        log("parked_terminal_fall");
    }
}
unsafe fn refresh_and_restore(ability: usize) {
    if !flow().refresh(ability) {
        return;
    }
    // A higher-priority native transition replaced our accepted pending state.
    // The same live parked graph was checked by refresh; undo only its selected
    // drone slot, never force a gun change if another native action already did.
    let Some(owner) = word(ability, 0x6c) else {
        return;
    };
    let Some(inventory) = word(owner as usize, 0x1410).filter(|p| *p != 0) else {
        return;
    };
    if word(inventory as usize, 0x30).is_some_and(|v| v & 255 == 0x28) {
        if let Some(slot) = word(ability, 0xb4).map(|v| v & 255).filter(|s| *s != 0x28) {
            let select: unsafe extern "thiscall" fn(usize, u32) =
                std::mem::transmute(0xd762d0usize);
            select(owner as usize, slot);
        }
    }
}

unsafe fn in_drone_control(owner: usize) -> bool {
    let Some(name) = word(0x339fa30, 0).filter(|v| *v != 0) else {
        return true;
    };
    let predicate: unsafe extern "thiscall" fn(usize, u32) -> u32 =
        std::mem::transmute(0xd5e8e0usize);
    predicate(owner, name) != 0
}
unsafe extern "thiscall" fn owner_hit_cancel(a: usize, damage_info: usize) {
    if READY.load(Ordering::Acquire) && retain_parked_for_damage(flow(), a, damage_info) {
        log("parked_owner_hit_retained");
        return;
    }
    // Preserve the actual virtual target for every other ability and death.
    let native: unsafe extern "thiscall" fn(usize) =
        std::mem::transmute(*(*(a as *const usize) as *const usize).add(0xb8 / 4));
    native(a);
}
unsafe extern "thiscall" fn body_electric_event(body: usize, owner: usize, name: usize, a: usize, b: usize) -> u32 {
    let native: unsafe extern "thiscall" fn(usize, usize, usize, usize) -> u32 =
        std::mem::transmute(0xd873f0usize);
    if !READY.load(Ordering::Acquire) {
        return native(owner, name, a, b);
    }
    let (result, completed) = parked_electric_dispatch(
        body, owner, Layout::RETAIL, |p| in_drone_control(p),
        || native(owner, name, a, b),
        |p| crate::drone_fall::disable_parked(p),
    );
    if completed { log("parked_body_electric_native_fall"); }
    result
}
unsafe extern "C" fn emp_feedback_owner_bridge(body: usize) -> usize {
    let native: unsafe extern "C" fn(usize) -> usize = std::mem::transmute(0xe93df0usize);
    if !READY.load(Ordering::Acquire) {
        return native(word(body, 0x9fc).unwrap_or(0) as usize);
    }
    emp_feedback_owner_with(
        body,
        Layout::RETAIL,
        |owner| in_drone_control(owner),
        |owner| native(owner),
    )
}
fn body_emp(body: usize, source: usize, duration: f32, a: u32, b: u32, c: u32) {
    unsafe {
        if !READY.load(Ordering::Acquire) {
            BodyEmp.call(body, source, duration, a, b, c);
            return;
        }
        let native: unsafe extern "thiscall" fn(usize, usize, f32, u32, u32, u32) =
            std::mem::transmute(BANK.load(Ordering::Acquire) + EMP_BODY_COPY);
        let (_, completed) = parked_emp_dispatch(
            body,
            Layout::RETAIL,
            |owner| in_drone_control(owner),
            || native(body, source, duration, a, b, c),
            |body| crate::drone_fall::disable_parked(body),
        );
        if completed {
            log("parked_body_emp_native_fall");
        }
    }
}
unsafe fn owner_hit_write(expected: &[u8; 6], replacement: &[u8; 6]) -> bool {
    const AT: usize = 0xdc1e21;
    owned_patch_write(AT, expected, replacement, &OWNER_HIT_ORIGINAL,
        &OWNER_HIT_INSTALLED, &OWNER_HIT_PROTECTION)
}

// Installation and teardown have one lifecycle owner. Keep the original page
// protection and cleanup obligation until bytes, cache and protection all agree.
// A foreign patch is never overwritten, even when cleanup remains pending.
unsafe fn owned_patch_write<const N: usize>(
    at: usize, expected: &[u8; N], replacement: &[u8; N], original: &[u8; N],
    installed: &AtomicBool, protection: &AtomicU32,
) -> bool {
    let restoring = replacement == original;
    let pending = installed.load(Ordering::Acquire);
    let current = bytes(at, N);
    if current != Some(expected.as_slice())
        && !(restoring && pending && current == Some(original.as_slice()))
    {
        return false;
    }
    if !restoring && pending { return false; }
    let mut old = 0;
    if VirtualProtect(at as _, N, 0x40, &mut old) == 0 { return false; }
    if !pending { protection.store(old, Ordering::Release); }
    std::ptr::copy_nonoverlapping(replacement.as_ptr(), at as *mut u8, N);
    let flushed = FlushInstructionCache(GetCurrentProcess(), at as _, N) != 0;
    let mut ignored = 0;
    let restored = VirtualProtect(at as _, N, protection.load(Ordering::Acquire), &mut ignored) != 0;
    let completed = flushed && restored;
    // Original bytes alone are not a completed rollback. A later deinit retries
    // unfinished cache/protection work, including when bytes are already original.
    installed.store(!restoring || !completed, Ordering::Release);
    if restoring && completed { protection.store(0, Ordering::Release); }
    completed
}
unsafe fn install_owner_hit() -> bool {
    let Some(patch) = bytes(0xdc1e03, 54)
        .and_then(|v| owner_hit_patch(v, BANK.load(Ordering::Acquire) + OWNER_HIT_CONTEXT))
    else {
        return false;
    };
    owner_hit_write(&OWNER_HIT_ORIGINAL, &patch)
}
unsafe fn remove_owner_hit() {
    if !OWNER_HIT_INSTALLED.load(Ordering::Acquire) { return; }
    let mut patch = [0xba, 0, 0, 0, 0, 0x90];
    patch[1..5].copy_from_slice(&((BANK.load(Ordering::Acquire) + OWNER_HIT_CONTEXT) as u32).to_le_bytes());
    if !owner_hit_write(&patch, &OWNER_HIT_ORIGINAL) {
        tracing::warn!("SCBL_MOD_DRONE_CONTROL owner_hit_restore=false");
    }
}

unsafe fn body_electric_write(expected: &[u8; 5], replacement: &[u8; 5]) -> bool {
    let at = BODY_ELECTRIC_CALL;
    owned_patch_write(at, expected, replacement, &BODY_ELECTRIC_ORIGINAL,
        &BODY_ELECTRIC_INSTALLED, &BODY_ELECTRIC_PROTECTION)
}
unsafe fn install_body_electric() -> bool {
    let Some(patch) = bytes(0xb2a5f0, 455).and_then(|v|
        body_electric_patch(v, BANK.load(Ordering::Acquire) + BODY_ELECTRIC_CONTEXT)) else { return false; };
    body_electric_write(&BODY_ELECTRIC_ORIGINAL, &patch)
}
unsafe fn remove_body_electric() {
    if !BODY_ELECTRIC_INSTALLED.load(Ordering::Acquire) { return; }
    let mut patch = [0xe8, 0, 0, 0, 0];
    patch[1..].copy_from_slice(&((BANK.load(Ordering::Acquire) + BODY_ELECTRIC_CONTEXT) as u32)
        .wrapping_sub((BODY_ELECTRIC_CALL + 5) as u32).to_le_bytes());
    if !body_electric_write(&patch, &BODY_ELECTRIC_ORIGINAL) {
        tracing::warn!("SCBL_MOD_DRONE_CONTROL body_electric_restore=false");
    }
}

fn activate(a: usize) {
    unsafe {
        flow().cancel(a);
        observe(a, "activate_before", true);
        Activate.call(a);
        observe(a, "activate_after", true);
    }
}
fn deactivate(a: usize) {
    unsafe {
        flow().cancel(a);
        observe(a, "deactivate_before", true);
        terminate_parked(a);
        Deactivate.call(a);
        observe(a, "deactivate_after", true);
    }
}
fn cancel(a: usize) {
    unsafe {
        flow().cancel(a);
        observe(a, "cancel_before", true);
        terminate_parked(a);
        Cancel.call(a);
        observe(a, "cancel_after", true);
    }
}
fn tick(a: usize, dt: f32) {
    unsafe {
        if READY.load(Ordering::Acquire) {
            refresh_and_restore(a);
        }
        Tick.call(a, dt);
        if READY.load(Ordering::Acquire) {
            refresh_and_restore(a);
            audio::tick(a);
        }
        observe(a, "tick", false);
    }
}
unsafe fn observe(a: usize, event: &str, force: bool) {
    #[cfg(feature = "native-observation")]
    crate::drone_observation::observe_shared(a, event, force);
    #[cfg(not(feature = "native-observation"))]
    let _ = (a, event, force);
}
unsafe fn bank() -> Option<usize> {
    let b = VirtualAlloc(std::ptr::null_mut(), BANK_SIZE, 0x3000, 4) as usize;
    if b == 0 {
        return None;
    }
    let result = (|| {
        let (base, drone) = reentry_check_windows(
            bytes(0xe7e7a0, 195)?,
            bytes(0xe7eb00, 93)?,
            b + BASE_CHECK,
            b + DRONE_CHECK,
        )?;
        let chunks = [
            (OWNER_HIT_CONTEXT, owner_hit_context_thunk(owner_hit_cancel as *const () as usize)?),
            (BODY_ELECTRIC_CONTEXT, body_electric_context_thunk(body_electric_event as *const () as usize)?),
            (
                EMP_BODY_COPY,
                emp_feedback_window(
                    bytes(0xaea010, 385)?,
                    b + EMP_BODY_COPY,
                    emp_feedback_owner_bridge as *const () as usize,
                )?,
            ),
            (
                ACTION,
                action_window(
                    bytes(0x14d3490, 136)?,
                    b + ACTION,
                    action_bridge as *const () as usize,
                )?,
            ),
            (
                EXIT_COPY,
                exit_window(
                    bytes(0x14b2390, 347)?,
                    b + EXIT_COPY,
                    fall_bridge as *const () as usize,
                )?,
            ),
            (BASE_CHECK, base),
            (DRONE_CHECK, drone),
            (
                PRESS_COPY,
                ability_press_window(
                    bytes(0xe7e870, 235)?,
                    b + PRESS_COPY,
                    active_press as *const () as usize,
                )?,
            ),
            (
                NETWORK,
                replication_activate_window(
                    bytes(0xfe1060, 92)?,
                    b + NETWORK,
                    transform_bridge as *const () as usize,
                )?,
            ),
            (
                PREPARE,
                prepare_window(bytes(0xe7eb60, 366)?, b + PREPARE, b + QUEUE)?,
            ),
            (
                QUEUE,
                queue_window(
                    bytes(0xd75ca0, 96)?,
                    b + QUEUE,
                    dispatch_bridge as *const () as usize,
                )?,
            ),
            (
                CONTROL_START,
                control_start_window(
                    bytes(0x14749e0, 188)?,
                    b + CONTROL_START,
                    placement_bridge as *const () as usize,
                )?,
            ),
            (
                PLACEMENT,
                mp_placement_window(
                    bytes(0x1471810, 470)?,
                    b + PLACEMENT,
                    placement_transform_bridge as *const () as usize,
                )?,
            ),
        ];
        for (offset, code) in chunks {
            std::ptr::copy_nonoverlapping(code.as_ptr(), (b + offset) as *mut u8, code.len());
        }
        let mut old = 0;
        if VirtualProtect(b as _, BANK_SIZE, 0x20, &mut old) == 0
            || FlushInstructionCache(GetCurrentProcess(), b as _, BANK_SIZE) == 0
        {
            return None;
        }
        Some(b)
    })();
    if result.is_none() {
        VirtualFree(b as _, 0, 0x8000);
    }
    result
}
pub(crate) unsafe fn init(world: usize) -> bool {
    if READY.load(Ordering::Acquire) {
        return true;
    }
    if world != 0x589860
        || !crate::drone_fall::ready()
        || !crate::drone_tags::parked_view_ready()
        || word(Layout::RETAIL.state as usize, 0x98) != Some(0x14b2390)
        || word(Layout::RETAIL.state as usize, 0xa4) != Some(0x14640b0)
        || word(Layout::RETAIL.state as usize, 0x244) != Some(0x14c1ab0)
        || word(Layout::RETAIL.state as usize, 0x280) != Some(0x1471810)
        || word(Layout::RETAIL.ability as usize, 0xb8) != Some(0xe6d7a0)
        || bytes(0xdc1e03, 54)
            .and_then(|v| owner_hit_patch(v, owner_hit_cancel as *const () as usize))
            .is_none()
        || body_electric_patch(bytes(0xb2a5f0, 455).unwrap_or(&[]), body_electric_event as *const () as usize).is_none()
        || ![
            (
                0xaea010,
                385,
                "dd99cd6345de8ed1874e8fd609c2e5f52590dec1b347ac23ccf97b2a9bd21d8c",
            ),
            (
                0x14c1ab0,
                311,
                "7585aff5c7b4ec3fc79cab2f82db31b2715073d1151c853622107bca947d3bb0",
            ),
            (
                0x14640b0,
                56,
                "727b0c72b741a64d1c43874f51f81c47a72e76c118652a0e0414005c6c3e1597",
            ),
            (
                0x14f72e0,
                94,
                "d3a763d27f4a97b1d0244a0362a9ba35c3532f894dc52c373b425e16e6036e80",
            ),
            (
                0x14640f0,
                10,
                "93c1eaef3a936557e7570734d80c6de63d9c3a121f336f868236657a2423f9b3",
            ),
            (
                0x14b2250,
                306,
                "3d6319e8473af5c302230f235c61e7c08259236f7f3c60d83eecff7841fb9688",
            ),
            (
                0xe58f00,
                98,
                "a9e544bc5692a1ccc4cbae39e9b384ce026e6af8bb930404e6368b270359e98c",
            ),
            (
                0xe6d7a0,
                71,
                "12d269c89b565ddf32cbce1b0e2ac844140c84952387e4f7092e6bf1243786b9",
            ),
            (
                0xe99c60,
                332,
                "a62e231c91c793de55231053051ed7c11725e04f4aeaa0e7f83fd168b8cc0ced",
            ),
            (
                0xd5e070,
                161,
                "555c4324d0bd1de4e84a995634aecd0beeef301b7e932816163c4a15f73a07bc",
            ),
            (
                0xef31b0,
                330,
                "e2f6426df9c6d36c6259aa5d8a2e794a55890e6753cd7719c43b046242044def",
            ),
        ]
        .iter()
        .all(|&(p, n, s)| signature(p, n, s))
    {
        tracing::warn!("SCBL_MOD_DRONE_CONTROL enabled=false reason=signature");
        return false;
    }
    let Some(b) = bank() else {
        tracing::warn!("SCBL_MOD_DRONE_CONTROL enabled=false reason=clone");
        return false;
    };
    type F = unsafe extern "thiscall" fn(usize);
    type A = unsafe extern "thiscall" fn(usize, *const u32, usize) -> u32;
    type T = unsafe extern "thiscall" fn(usize, f32);
    if Action
        .initialize(std::mem::transmute::<usize, A>(0x14d3490), action)
        .is_err()
        || Exit
            .initialize(std::mem::transmute::<usize, F>(0x14b2390), exit)
            .is_err()
        || TransOut
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, u32)>(0x14c1ab0),
                transout,
            )
            .is_err()
        || Entry
            .initialize(std::mem::transmute::<usize, F>(0x14b2250), entry)
            .is_err()
        || ControlStart
            .initialize(std::mem::transmute::<usize, F>(0x14749e0), control_start)
            .is_err()
        || PressAction
            .initialize(std::mem::transmute::<usize, F>(0xe7e870), press)
            .is_err()
        || NetworkActivate
            .initialize(std::mem::transmute::<usize, F>(0xfe1060), network)
            .is_err()
        || Activate
            .initialize(std::mem::transmute::<usize, F>(0xe7eb60), activate)
            .is_err()
        || Deactivate
            .initialize(std::mem::transmute::<usize, F>(0xe58f00), deactivate)
            .is_err()
        || Cancel
            .initialize(std::mem::transmute::<usize, F>(0xe6d7a0), cancel)
            .is_err()
        || BodyEmp
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(usize, usize, f32, u32, u32, u32),
                >(0xaea010),
                body_emp,
            )
            .is_err()
        || Tick
            .initialize(std::mem::transmute::<usize, T>(0xe99c60), tick)
            .is_err()
    {
        VirtualFree(b as _, 0, 0x8000);
        return false;
    }
    BANK.store(b, Ordering::Release);
    if Action.enable().is_err()
        || Exit.enable().is_err()
        || TransOut.enable().is_err()
        || Entry.enable().is_err()
        || ControlStart.enable().is_err()
        || PressAction.enable().is_err()
        || NetworkActivate.enable().is_err()
        || Activate.enable().is_err()
        || Deactivate.enable().is_err()
        || Cancel.enable().is_err()
        || Tick.enable().is_err()
        || BodyEmp.enable().is_err()
        || !install_owner_hit()
        || !install_body_electric()
    {
        deinit();
        tracing::warn!("SCBL_MOD_DRONE_CONTROL enabled=false reason=hook");
        return false;
    }
    if !shutdown::init() {
        deinit();
        return false;
    }
    flow().clear();
    LOGS.store(0, Ordering::Relaxed);
    #[cfg(feature = "native-observation")]
    crate::drone_observation::start_shared();
    READY.store(true, Ordering::Release);
    audio::init();
    // Optional presentation only: unsupported HUD bytes keep native behavior.
    let early_close = exit_presentation::init();
    tracing::info!("SCBL_MOD_DRONE_EXIT revision=64 accepted_local_native_close={} native_timer_seconds=0.2 transout_argument_unchanged=true", early_close);
    tracing::info!("SCBL_MOD_DRONE_CONTROL enabled=true native_input=true reentry_seconds=3 same_body=true lifecycle_owner=single exit_presentation=native parked_owner_hit_guard=true electric_owner_native_cancel=true parked_body_electric_bridge=true parked_body_emp_bridge=true parked_emp_owner_feedback=false forced_exit_override={}", shutdown::OVERRIDE_NATIVE_PRESENTATION);
    true
}
pub(crate) unsafe fn deinit() {
    exit_presentation::deinit();
    audio::deinit();
    READY.store(false, Ordering::Release);
    remove_body_electric();
    remove_owner_hit();
    let _ = BodyEmp.disable();
    shutdown::deinit();
    let _ = Tick.disable();
    let _ = Cancel.disable();
    let _ = Deactivate.disable();
    let _ = Activate.disable();
    let _ = NetworkActivate.disable();
    let _ = PressAction.disable();
    let _ = ControlStart.disable();
    let _ = Entry.disable();
    let _ = TransOut.disable();
    let _ = Exit.disable();
    let _ = Action.disable();
    flow().clear();
    // Keep RX thunks mapped until process shutdown: a callback already dispatched
    // before disable may still be returning through one. No dangling code pointer.
}

// Test-only forwarding across the private runtime/presentation boundary.
#[cfg(test)]
pub(super) unsafe fn test_finish_destroyed(
    hud: usize, state: usize, layout: Layout, hud_vt: u32, finish: impl FnOnce(usize),
) -> bool {
    exit_presentation::test_finish_destroyed(hud, state, layout, hud_vt, finish)
}

#[cfg(test)]
pub(crate) unsafe fn test_advance_disabled(
    hud: usize, state: usize, layout: Layout, hud_vt: u32,
    present: impl FnOnce(usize, u32), update: impl FnOnce(usize),
) -> bool {
    exit_presentation::test_advance_disabled(hud, state, layout, hud_vt, present, update)
}

#[cfg(test)]
pub(crate) unsafe fn test_bind_disabled(hud: usize, hud_vt: u32) -> bool {
    exit_presentation::test_bind_disabled(hud, hud_vt)
}
