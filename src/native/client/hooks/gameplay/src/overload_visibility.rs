//! Exact overload-release root visibility, using the game's queued visibility command.
//! Offline candidate: caller must install only after overload_particles/thermal_smoke are ready.
use crate::equipment_native::{readable, signature, word};
use retour::GenericDetour;
use std::{cell::RefCell, sync::{Mutex, OnceLock, atomic::{AtomicBool, AtomicU32, Ordering}}};

const PARTICLE: u32 = 0x029e8754;
const LIMIT: usize = 128;
const VISIBILITY_SHA: &str = "c19afc6285ae60e7a16831821e78cd760de7bad9a348293f4f9e1648fe893e07";
const RESET_SHA: &str = "a9e8385f49d155c4289b4fa35a5018875244a6e6a0144ec36d31ccf0b14cf495";
const QUEUE_SHA: &str = "925ca64005a543e2f8ea2577ef7314491eb097fb7c54978fb95c20714b3942cc";
type Visibility = unsafe extern "cdecl" fn(usize, usize, u32);
type Reset = unsafe extern "thiscall" fn(usize);
type Queue = unsafe extern "cdecl" fn(usize);
static VISIBILITY: OnceLock<GenericDetour<Visibility>> = OnceLock::new();
static RESET: OnceLock<GenericDetour<Reset>> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);
static QUEUE_LOGS: AtomicU32 = AtomicU32::new(0);
static OVERRIDE_LOGS: AtomicU32 = AtomicU32::new(0);
static RESET_LOGS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Token { actor: usize, key: u32, world: u32, root: usize }
static QUEUED: Mutex<[Option<Token>; LIMIT]> = Mutex::new([None; LIMIT]);
#[derive(Clone, Copy)]
struct Pending { depth: u32, actors: [usize; 4], len: usize, overflow: bool }
impl Pending {
    const fn new() -> Self { Self { depth: 0, actors: [0; 4], len: 0, overflow: false } }
    fn note(&mut self, actor: usize) {
        if self.depth == 0 || self.overflow || self.actors[..self.len].contains(&actor) { return; }
        if self.len == self.actors.len() { self.overflow = true; self.len = 0; return; }
        self.actors[self.len] = actor; self.len += 1;
    }
}
thread_local! { static PENDING: RefCell<Pending> = const { RefCell::new(Pending::new()) }; }

unsafe fn token(actor: usize) -> Option<Token> {
    if !crate::overload_particles::contains(actor) || !readable(actor, 0x310)
        || word(actor, 0)? != PARTICLE { return None; }
    let root = word(actor, 0x30c)? as usize;
    if root == 0 || !readable(root, 0x114) || word(root, 0xf8)? == 0
        || word(root, 0x34)? & 1 != 0 { return None; }
    let world = word(actor, 0x144)?;
    (world != 0).then_some(Token { actor, key: word(actor, 0x20)?, world, root })
}
fn reserve(queue: &mut [Option<Token>; LIMIT], id: Token) -> bool {
    if queue.iter().flatten().any(|old| *old == id) { return false; }
    for slot in queue.iter_mut() {
        if slot.is_some_and(|old| old.actor == id.actor) { *slot = None; }
    }
    if let Some(slot) = queue.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(id); true
    } else { false } // Full registry fails native, never evicts a live effect.
}
unsafe fn queue_once(actor: usize) {
    if !READY.load(Ordering::Acquire) { return; }
    crate::overload_emission_lights::refresh(actor);
    let Some(id) = token(actor) else { return; };
    let Ok(mut queued) = QUEUED.lock() else { return; };
    if !reserve(&mut queued, id) { return; }
    drop(queued); // Native queue may invoke callbacks; never hold our lock.
    (std::mem::transmute::<usize, Queue>(0x9d9f00))(actor);
    if QUEUE_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
        tracing::info!("SCBL_OVERLOAD_VISIBILITY native_queue_called=true scope=exact-registered-root native_callback=unverified");
    }
}
// Called after native CreateNode and after thermal_smoke ROOTS lock is dropped.
// During Release, defer until original Release has attached its returned actor.
pub(super) unsafe fn root_ready(actor: usize) {
    if !READY.load(Ordering::Acquire) || !crate::overload_particles::contains(actor) { return; }
    let in_release = PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.depth == 0 { false } else { pending.note(actor); true }
    });
    if !in_release { queue_once(actor); }
}
// Called only after successful exact overload registration, before native attach.
pub(super) fn captured(actor: usize) {
    if !READY.load(Ordering::Acquire) { return; }
    retire(actor); // A new generation may reuse the same address/root.
    PENDING.with(|pending| pending.borrow_mut().note(actor));
}
pub(super) fn retire(actor: usize) {
    if let Ok(mut queued) = QUEUED.lock() {
        for slot in queued.iter_mut() {
            if slot.is_some_and(|old| old.actor == actor) { *slot = None; }
        }
    }
}
pub(super) unsafe fn with_release<R>(native: impl FnOnce() -> R) -> R {
    PENDING.with(|pending| pending.borrow_mut().depth += 1);
    let result = native();
    let actors = PENDING.with(|pending| {
        let mut p = pending.borrow_mut();
        p.depth -= 1;
        if p.depth != 0 { return None; }
        let actors = if p.overflow { None } else { Some((p.actors, p.len)) };
        p.len = 0; p.overflow = false;
        actors
    });
    if let Some((actors, len)) = actors {
        for actor in actors.into_iter().take(len) { queue_once(actor); }
    }
    result
}
fn visibility_arg(original: u32, exact_hidden: bool, known_root_bit: Option<u32>) -> u32 {
    if exact_hidden && known_root_bit.is_some_and(|bits| bits & 1 == 0) { 1 } else { original }
}
unsafe extern "cdecl" fn visibility(world: usize, node: usize, hidden: u32) {
    let adjusted = if READY.load(Ordering::Acquire) {
        if crate::overload_emission_lights::hidden_node(node) { 1 }
        else { visibility_arg(hidden, crate::overload_particles::hidden_node(node), word(node, 0x34)) }
    } else { hidden };
    if adjusted != hidden && OVERRIDE_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
        tracing::info!("SCBL_OVERLOAD_VISIBILITY visual_override=true scope=exact-registered-root-or-child-light");
    }
    VISIBILITY.get().expect("published before enable").call(world, node, adjusted);
}
fn before_reset(
    registered: bool,
    still_matches: impl FnOnce() -> bool,
    retire_registered: impl FnOnce(),
    retire_mismatch: impl FnOnce(),
) {
    if !registered { return; } // Ordinary/smoke ROOTS belong to another owner.
    retire_registered();
    if !still_matches() { retire_mismatch(); }
}
unsafe extern "thiscall" fn reset(actor: usize) {
    if READY.load(Ordering::Acquire) {
        // +350 is the native rebuild template input, not a generation number.
        // A mismatch retires only the overload classification, not shared ROOTS.
        // A same-template reinitialization may be this release's late activation.
        before_reset(
            crate::overload_particles::registered_address(actor),
            || unsafe { crate::overload_particles::contains(actor) },
            || { retire(actor); crate::overload_emission_lights::forget(actor); },
            || crate::overload_particles::retire_registration(actor),
        );
    }
    RESET.get().expect("published before enable").call(actor);
    if READY.load(Ordering::Acquire) && crate::overload_particles::contains(actor) {
        // Native reset has returned: refresh the root mapping and append the
        // visibility command after its native rebuild/attach commands.
        crate::thermal_smoke::track_root(actor);
        root_ready(actor);
        if RESET_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
            tracing::info!("SCBL_OVERLOAD_VISIBILITY reset_retained=true scope=matching-template-identity");
        }
    }
}
pub(super) unsafe fn init() -> bool {
    if READY.load(Ordering::Acquire) { return true; }
    if !signature(0xa37180, 291, VISIBILITY_SHA)
        || !signature(0x9c4760, 567, RESET_SHA)
        || !signature(0x9d9f00, 142, QUEUE_SHA)
        || !crate::overload_emission_lights::init() { return false; }
    let Some(visibility) = VISIBILITY.get().or_else(|| {
        let hook = GenericDetour::new(std::mem::transmute::<usize, Visibility>(0xa37180), visibility as Visibility).ok()?;
        let _ = VISIBILITY.set(hook); VISIBILITY.get()
    }) else { return false; };
    let Some(reset) = RESET.get().or_else(|| {
        let hook = GenericDetour::new(std::mem::transmute::<usize, Reset>(0x9c4760), reset as Reset).ok()?;
        let _ = RESET.set(hook); RESET.get()
    }) else { return false; };
    if reset.enable().is_err() { return false; }
    if visibility.enable().is_err() { let _ = reset.disable(); return false; }
    READY.store(true, Ordering::Release); // Both hooks or neither active for suppression.
    tracing::info!("SCBL_OVERLOAD_VISIBILITY enabled=true queue=native root_gate=exact reset=template-conditional runtime_acceptance=pending");
    true
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    tracing::info!("SCBL_OVERLOAD_VISIBILITY enabled=false restore=not-queued-on-shutdown-thread");
    // Do not queue restoration from an arbitrary shutdown thread. Native
    // 9D9F00's thread contract and actor lifetime at this point are unproven.
    // This is safe for process teardown, not a mid-game toggle contract.
    if let Ok(mut queued) = QUEUED.lock() { *queued = [None; LIMIT]; }
    if let Some(h) = VISIBILITY.get() { let _ = h.disable(); }
    if let Some(h) = RESET.get() { let _ = h.disable(); }
}
// Owned native-copy fixture only. Never resolves or patches the game image.
#[cfg(test)]
pub(super) unsafe fn fixture_install_reuse(entry: usize) -> bool {
    if RESET.get().is_some() { return false; }
    let Ok(hook) = GenericDetour::new(std::mem::transmute::<usize, Reset>(entry), reset as Reset)
        else { return false; };
    if RESET.set(hook).is_err() { return false; }
    if RESET.get().unwrap().enable().is_err() { return false; }
    READY.store(true, Ordering::Release);
    true
}
#[cfg(test)]
pub(super) unsafe fn fixture_disable_reuse() {
    READY.store(false, Ordering::Release);
    if let Some(h) = RESET.get() { let _ = h.disable(); }
    if let Ok(mut queued) = QUEUED.lock() { *queued = [None; LIMIT]; }
}
#[cfg(test)]
pub(super) unsafe fn fixture_install_visibility(entry: usize) -> bool {
    if VISIBILITY.get().is_some() { return false; }
    let Ok(hook) = GenericDetour::new(
        std::mem::transmute::<usize, Visibility>(entry), visibility as Visibility)
        else { return false; };
    if VISIBILITY.set(hook).is_err() { return false; }
    if VISIBILITY.get().unwrap().enable().is_err() { return false; }
    READY.store(true, Ordering::Release);
    true
}
#[cfg(test)]
pub(super) unsafe fn fixture_disable_visibility() {
    READY.store(false, Ordering::Release);
    if let Some(h) = VISIBILITY.get() { let _ = h.disable(); }
}
#[cfg(test)]
#[path = "overload_visibility_tests.rs"]
mod tests;


