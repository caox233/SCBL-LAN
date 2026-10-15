//! Per-view suppression of tracked smoke-grenade clouds, not smoke gameplay.

//! MP continuous cloud creation lives in AA8440, separately from EA4780.

//! EA4780 returns the particle spawned from grenade+610. No parent attachment is

//! required. Particle CreateNode passes null userdata: node+20 is NOT its owner.

//! Cache only already-classified actor+30c roots, with per-hit lifetime checks.

//! Registration and deletion cross threads; never use a simulation-thread TLS tag.

use crate::equipment_native::{readable, signature, word};

use retour::{Function, GenericDetour, HookableWith};

use std::sync::{Mutex, OnceLock, atomic::{AtomicBool, Ordering}};



const SMOKE: u32 = 0x02aac454;

const ADV_SMOKE: u32 = 0x02aa32cc;

const PARTICLE: u32 = 0x029e8754;

const CAPACITY: usize = 128;

static READY: AtomicBool = AtomicBool::new(false);

static HAS_SMOKE: AtomicBool = AtomicBool::new(false);

static CLOUDS: Mutex<Clouds> = Mutex::new(Clouds::new());



#[derive(Clone, Copy, Debug, PartialEq, Eq)]

struct Identity { address: usize, object: u32, world: u32 }

unsafe fn identity(p: usize) -> Option<Identity> {

    if !readable(p, 0x310) || (p as *const u32).read_unaligned() != PARTICLE { return None; }

    let object = ((p + 0x20) as *const u32).read_unaligned();

    let world = ((p + 0x144) as *const u32).read_unaligned();

    (world != 0).then_some(Identity { address: p, object, world })

}

struct Clouds { entries: [Option<Identity>; CAPACITY] }

impl Clouds {

    const fn new() -> Self { Self { entries: [None; CAPACITY] } }

    fn remove(&mut self, address: usize) {

        for entry in &mut self.entries { if entry.is_some_and(|id| id.address == address) { *entry = None; } }

    }

    fn insert(&mut self, id: Identity) -> bool {

        self.remove(id.address);

        if let Some(slot) = self.entries.iter_mut().find(|e| e.is_none()) { *slot = Some(id); true }

        else { false } // Unknown/over-capacity effects keep native drawing.

    }

    fn get(&self, address: usize) -> Option<Identity> {

        self.entries.iter().flatten().find(|id| id.address == address).copied()

    }

    fn any(&self) -> bool { self.entries.iter().any(Option::is_some) }

}

// Both classifiers have at most 128 actors. Draw lookup scans only this small

// owned table, not game actor memory; only its unique hit is dereferenced.

const ROOT_CAPACITY: usize = 2 * CAPACITY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

struct Root { id: Identity, node: usize }

struct Roots { entries: [Option<Root>; ROOT_CAPACITY] }

impl Roots {

    const fn new() -> Self { Self { entries: [None; ROOT_CAPACITY] } }

    fn remove(&mut self, actor: usize) {

        for e in &mut self.entries { if e.is_some_and(|r| r.id.address == actor) { *e = None; } }

    }

    fn insert(&mut self, root: Root) -> bool {

        self.remove(root.id.address);

        if let Some(slot) = self.entries.iter_mut().find(|e| e.is_none()) { *slot = Some(root); true }

        else { false } // Never evict a live actor to classify another particle.

    }

    fn by_node(&self, node: usize) -> Option<Root> {

        if node == 0 { return None; }

        let mut hits = self.entries.iter().flatten().filter(|r| r.node == node);

        let first = *hits.next()?;

        if hits.next().is_some() { None } else { Some(first) } // ambiguous = native

    }

}

static ROOTS: Mutex<Roots> = Mutex::new(Roots::new());

pub(super) fn forget_root(actor: usize) {

    if let Ok(mut roots) = ROOTS.lock() { roots.remove(actor); }

}

// Called only AFTER successful classification; a zero root is a pending mapping,

// not permission to suppress any draw. Serial/class/world still come from actor.

pub(super) unsafe fn track_root(actor: usize) {

    let Ok(mut roots) = ROOTS.lock() else { return; };

    let Some(id) = identity(actor) else { roots.remove(actor); return; };

    let Some(node) = word(actor, 0x30c) else { roots.remove(actor); return; };

    roots.insert(Root { id, node: node as usize });

}

pub(super) unsafe fn with_node_creation<R>(enabled: bool, actor: usize, native: impl FnOnce() -> R) -> R {

    let result = native(); // cdecl(actor)->void in production; called exactly once.

    if enabled {

        let Ok(mut roots) = ROOTS.lock() else { return result; };

        // Lookup after native also covers registration concurrent with creation.

        // Registration snapshots under this same lock, so it cannot overwrite a

        // refreshed node with an earlier zero snapshot. No native call under lock.

        if let Some(slot) = roots.entries.iter_mut().find(|e| e.is_some_and(|r| r.id.address == actor)) {

            let expected = slot.as_ref().unwrap().id;

            if identity(actor) == Some(expected) {

                if let Some(node) = word(actor, 0x30c) { slot.as_mut().unwrap().node = node as usize; }

                else { *slot = None; }

            } else { *slot = None; }

        }

    }

    if enabled { crate::overload_visibility::root_ready(actor); }

    result

}

pub(super) unsafe fn particle_owner(record: usize) -> Option<usize> {

    particle_owner_node(word(record, 0)? as usize)

}

pub(super) unsafe fn particle_owner_node(node: usize) -> Option<usize> {

    let root = { ROOTS.lock().ok()?.by_node(node)? };

    // Native destruction clears +30c at 9D966A/9D96BA. Never infer an owner from

    // node+20 or fall back to it: ordinary particles may have arbitrary userdata.

    (identity(root.id.address) == Some(root.id)

        && word(root.id.address, 0x30c) == Some(node as u32)).then_some(root.id.address)

}

unsafe fn smoke_source(source: usize, definition: usize) -> Option<u32> {

    if !matches!(word(source, 0)?, SMOKE | ADV_SMOKE)

        || definition == 0 || word(source, 0x610)? as usize != definition

    { return None; }

    word(source, 0x144).filter(|world| *world != 0)

}

unsafe fn register(world: Option<u32>, particle: usize) {

    let Some(id) = identity(particle).filter(|id| Some(id.world) == world) else { return; };

    let Ok(mut clouds) = CLOUDS.lock() else { return; };

    if !clouds.insert(id) { return; }

    HAS_SMOKE.store(true, Ordering::Release);

    drop(clouds);

    track_root(particle);

}

fn forget(particle: usize) {

    forget_root(particle);

    if !HAS_SMOKE.load(Ordering::Acquire) { return; }

    let Ok(mut clouds) = CLOUDS.lock() else { return; };

    clouds.remove(particle);

    HAS_SMOKE.store(clouds.any(), Ordering::Release);

}

unsafe fn hidden(render: usize, record: usize) -> bool {

    if !HAS_SMOKE.load(Ordering::Acquire) { return false; }

    // The record is a native synchronous borrow. It is not retained. Invalid

    // owners, unknown particles and reused object identities always draw normally.

    let Some(owner) = particle_owner(record) else { return false; };

    let expected = { let Ok(clouds) = CLOUDS.lock() else { return false; }; clouds.get(owner) };

    let Some(expected) = expected else { return false; };

    if identity(owner) != Some(expected) { return false; }

    let Some(view) = word(render, 0x20d0).map(|v| v as usize) else { return false; };

    let hidden = word(view, 0x340) == Some(8);

    if hidden && !SMOKE_SKIP_DIAGNOSTIC.swap(true, Ordering::Relaxed) {

        tracing::info!("SCBL_THERMAL_SMOKE actual_skip=true mode=8 temporary=true");

    }

    hidden

}


// Bounded source provenance only. The former observer collapsed every rejected
// source into one "other" bucket, losing the actual multiplayer smoke path.
// Store only vtable RVAs from the pinned executable's read-only section: never
// actor addresses, identities, names, network endpoints or arbitrary memory.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceTypes { entry: u8, source: u32, component: u32, network: u32, reciprocal: bool }
const SOURCE_TYPE_LIMIT: usize = 32;
static SOURCE_TYPES: Mutex<[Option<SourceTypes>; SOURCE_TYPE_LIMIT]> = Mutex::new([None; SOURCE_TYPE_LIMIT]);
static SOURCE_TYPES_FULL: AtomicBool = AtomicBool::new(false);
static SOURCE_TYPE_ATTEMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const SOURCE_TYPE_ATTEMPT_LIMIT: u32 = 1024;
fn source_type_attempt(counter: &std::sync::atomic::AtomicU32) -> Option<u32> {
    if counter.load(Ordering::Relaxed) >= SOURCE_TYPE_ATTEMPT_LIMIT { return None; }
    counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
        |n| (n < SOURCE_TYPE_ATTEMPT_LIMIT).then_some(n + 1)).ok()
}
fn native_type_rva(table: Option<u32>) -> u32 {
    table.filter(|p| (0x02973000..0x03104ad6).contains(p))
        .map_or(0, |p| p - 0x00400000)
}
fn claim_source_type(entries: &mut [Option<SourceTypes>; SOURCE_TYPE_LIMIT], key: SourceTypes) -> bool {
    if entries.contains(&Some(key)) { return false; }
    let Some(slot) = entries.iter_mut().find(|slot| slot.is_none()) else { return false; };
    *slot = Some(key); true
}
unsafe fn observe_source_type(entry: u8, source: usize) {
    if SOURCE_TYPES_FULL.load(Ordering::Relaxed) { return; }
    // Repeated identical sources also consume the total budget. Once exhausted,
    // there are no observer word/VirtualQuery calls or locks, only atomic loads.
    let Some(attempt) = source_type_attempt(&SOURCE_TYPE_ATTEMPTS) else { return; };
    if attempt + 1 == SOURCE_TYPE_ATTEMPT_LIMIT {
        tracing::info!("SCBL_THERMAL_SMOKE source_types_budget_exhausted=true attempt_limit=1024 observer_only=true temporary=true");
    }
    let table = word(source, 0);
    let source_type = native_type_rva(table);
    // Only the audited native grenade/sphere call sites pass entry 0/1. Refuse
    // arbitrary objects, and never infer suppression permission from this record.
    if source_type == 0 || entry > 1 || (entry == 1 && table != Some(SMOKE_SPHERE)) { return; }
    let component = word(source, if entry == 0 { 0x614 } else { 0x320 }).unwrap_or(0) as usize;
    let network = if entry == 0 { word(source, 0x2c0).unwrap_or(0) as usize } else { 0 };
    let network_type = if network != 0 { native_type_rva(word(network, 0)) } else { 0 };
    let key = SourceTypes { entry, source: source_type, component: native_type_rva(word(component, 0)),
        network: network_type, reciprocal: network_type != 0 && word(network, 0x88) == Some(source as u32) };
    let Ok(mut entries) = SOURCE_TYPES.lock() else { return; };
    let claimed = claim_source_type(&mut entries, key);
    if entries.iter().all(Option::is_some) { SOURCE_TYPES_FULL.store(true, Ordering::Relaxed); }
    drop(entries);
    if claimed {
        tracing::info!("SCBL_THERMAL_SMOKE source_types=true entry={} source_rva={:#x} component_rva={:#x} network_rva={:#x} reciprocal={} category_limit=32 observer_only=true temporary=true",
            key.entry, key.source, key.component, key.network, key.reciprocal);
    }
}

// Temporary source-path observer only: no permission to classify/suppress FX.

// Three unconditional first-entry records + nineteen first-category slots = 22

// records maximum per DLL lifetime. No pointers, IDs, strings from game objects.

static SOURCE_PROBE_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

const SOURCE_PROBE_LIMIT: u32 = 22;

fn source_probe_claim(bits: &std::sync::atomic::AtomicU32, category: u32) -> bool {

    if category >= SOURCE_PROBE_LIMIT { return false; }

    let bit = 1u32 << category;

    // Repeated categories avoid the write/RMW and the logging machinery.

    bits.load(Ordering::Relaxed) & bit == 0

        && bits.fetch_or(bit, Ordering::Relaxed) & bit == 0

}

fn source_probe_entry(category: u32, entry: &'static str) {

    if source_probe_claim(&SOURCE_PROBE_BITS, category) {

        tracing::info!("SCBL_THERMAL_SMOKE source_probe=true stage=entry entry={} reason=reached temporary=true", entry);

    }

}

#[derive(Clone, Copy)]

struct SourceProbe {

    entry: &'static str, reason: &'static str, source_kind: &'static str,

    component_kind: &'static str, origin_kind: &'static str,

    definition_match: bool, origin_resource_match: bool,

    before: (Option<u32>, Option<u32>, Option<u32>),

}

fn source_probe_kind(v: Option<u32>) -> &'static str {

    match v { Some(SMOKE)=>"smoke",Some(ADV_SMOKE)=>"advanced_smoke",

        Some(SMOKE_SPHERE)=>"sphere",Some(_)=>"other",None=>"unreadable" }

}

unsafe fn source_probe_snapshot(category: u32, entry: &'static str, reason: &'static str,

    source: usize, definition: usize) -> Option<SourceProbe>

{

    if !source_probe_claim(&SOURCE_PROBE_BITS, category) { return None; }

    let before = (word(source, 0), word(source, 0x20), word(source, 0x144));

    let mut probe = SourceProbe { entry, reason, source_kind: source_probe_kind(before.0),

        component_kind: "not_sphere", origin_kind: "not_sphere", definition_match: false,

        origin_resource_match: false, before };

    if before.0 == Some(SMOKE_SPHERE) {

        let component = word(source, 0x320).unwrap_or(0) as usize;

        probe.component_kind = match word(component, 0) {

            Some(SMOKE_COMPONENT)=>"smoke",Some(0x02a3e6dc)=>"explosion",

            Some(_)=>"other",None=>"unreadable_or_null" };

        let origin = word(source, 0x324).unwrap_or(0) as usize;

        let origin_type = word(origin, 0);

        probe.origin_kind = source_probe_kind(origin_type);

        probe.definition_match = definition != 0 && word(source, 0x33c) == Some(definition as u32);

        // Candidate association only; raw origin pointer is NOT lifetime proof.

        if matches!(origin_type, Some(SMOKE | ADV_SMOKE)) {

            probe.origin_resource_match = definition != 0 && word(origin, 0x610) == Some(definition as u32);

        }

    } else if matches!(before.0, Some(SMOKE | ADV_SMOKE)) {

        probe.definition_match = definition != 0 && word(source, 0x610) == Some(definition as u32);

    }

    Some(probe)

}

unsafe fn source_probe_factory(enabled: bool, source: usize, definition: usize,

    qualified: bool) -> Option<SourceProbe>

{

    // The original qualifier has already computed this flag; accepted hot-path

    // repeats need one relaxed bit load, no extra object reads or mutexes.

    let (category,reason) = if !enabled {(1,"disabled")}

    else if qualified {(7,"qualified")}

    else { match word(source,0) {

        None=>(2,"source_unreadable"),Some(SMOKE_SPHERE)=>(4,"sphere_source_not_grenade"),

        Some(SMOKE | ADV_SMOKE)=>{

            if definition==0 || word(source,0x610)!=Some(definition as u32) {(5,"definition_rejected")}

            else {(6,"world_rejected")}

        },Some(_)=>(3,"other_source"),

    }};

    source_probe_snapshot(category,"factory",reason,source,definition)

}

unsafe fn source_probe_volume(enabled: bool, source: usize, qualified: bool) -> Option<SourceProbe> {

    let (category,reason) = if !enabled {(9,"disabled")}

    else if qualified {(15,"qualified")}

    else if word(source,0)!=Some(SMOKE_SPHERE) {(10,"source_class_or_read_rejected")}

    else {

        let component=word(source,0x320).unwrap_or(0)as usize;

        match word(component,0) {

            None=>(11,"component_missing_or_unreadable"),

            Some(v) if v!=SMOKE_COMPONENT=>(12,"other_component"),

            Some(_)=>{

                if word(component,0x30).unwrap_or(0)==0 {(13,"resource_missing_or_unreadable")}

                else {(14,"world_or_identity_rejected")}

            }

        }

    };

    source_probe_snapshot(category,"volume",reason,source,0)

}

unsafe fn source_probe_continuous(enabled: bool, source: usize, qualified: bool) -> Option<SourceProbe> {

    let (category,reason)=if !enabled {(17,"disabled")}

    else if qualified {(21,"qualified")}

    else if word(source,0)!=Some(ADV_SMOKE) {(18,"source_class_or_read_rejected")}

    else if word(source,0x864).unwrap_or(0)==0 {(19,"resource_missing_or_unreadable")}

    else {(20,"world_or_identity_rejected")};

    source_probe_snapshot(category,"continuous",reason,source,0)

}

unsafe fn source_probe_finish(probe: Option<SourceProbe>, source: usize,

    returned_particle: Option<usize>, field: usize)

{

    let Some(probe)=probe else {return;};

    let stable=probe.before==(word(source,0),word(source,0x20),word(source,0x144));

    // EA4780 really returns a particle pointer. The other two returns do NOT:

    // report their actor field, only if the entry source identity stayed stable.

    let particle=returned_particle.unwrap_or_else(|| {

        if stable {word(source,field).unwrap_or(0)as usize} else {0}

    });

    let particle_exact=particle!=0 && word(particle,0)==Some(PARTICLE);

    let world_match=particle_exact && probe.before.2.is_some_and(|w|w!=0&&word(particle,0x144)==Some(w));

    tracing::info!("SCBL_THERMAL_SMOKE source_probe=true stage=result entry={} reason={} source_kind={} component_kind={} origin_kind={} definition_match={} origin_resource_match={} source_stable={} result_kind={} particle_present={} particle_exact={} world_match={} temporary=true",

        probe.entry,probe.reason,probe.source_kind,probe.component_kind,probe.origin_kind,

        probe.definition_match,probe.origin_resource_match,stable,

        if returned_particle.is_some(){"return"}else{"actor_field"},particle!=0,particle_exact,world_match);

}



pub(super) unsafe fn with_spawn(enabled: bool, source: usize, definition: usize, native: impl FnOnce() -> usize) -> usize {

    source_probe_entry(0, "factory");

    let world = if enabled { smoke_source(source, definition) } else { None };

    let probe = source_probe_factory(enabled, source, definition, world.is_some());

    let particle = native();

    // This factory allocates a new actor. Forget any old address even when the

    // new effect is not smoke (pool reuse must not inherit our old classification).

    if enabled { forget(particle); }

    crate::overload_particles::forget(particle);

    if world.is_some() { register(world, particle); }

    source_probe_finish(probe, source, Some(particle), 0);

    particle

}

// MP native tick AA8440 creates the continuous cloud directly via the world

// factory and stores it in grenade+860 (class resource +864). It does not call

// EA4780. Keep the explosion hook for its separate effect; never classify all FX.

#[derive(Clone, Copy, PartialEq, Eq)]

struct SmokeOwner { serial: u32, world: u32, cloud_class: u32 }

unsafe fn continuous_source(source: usize) -> Option<SmokeOwner> {

    if word(source, 0)? != ADV_SMOKE { return None; }

    let owner = SmokeOwner { serial: word(source, 0x20)?, world: word(source, 0x144)?,

        cloud_class: word(source, 0x864)? };

    (owner.world != 0 && owner.cloud_class != 0).then_some(owner)

}

pub(super) unsafe fn with_continuous<R>(enabled: bool, source: usize, native: impl FnOnce() -> R) -> R {

    source_probe_entry(16, "continuous");

    let owner = if enabled { continuous_source(source) } else { None };

    let probe = source_probe_continuous(enabled, source, owner.is_some());

    let result = native(); // Original spawn, timer, attachment and return unchanged.

    if let Some(owner) = owner {

        // Native tick can destroy/change the source. Reject reuse or transitions.

        if continuous_source(source) == Some(owner) {

            if let Some(particle) = word(source, 0x860) {

                register(Some(owner.world), particle as usize);

            }

        }

    }

    source_probe_finish(probe, source, None, 0x860);

    result

}

// E30F00 is the separate sphere-component producer. Classify ONLY an exact

// AESphereOfInfluence with an exact Smoke component and its non-null +30 FX.

// +320 is a single component pointer. +3BC is the returned particle actor.

const SMOKE_SPHERE: u32 = 0x02a29f0c;

const SMOKE_COMPONENT: u32 = 0x02a3e8bc;

#[derive(Clone, Copy, PartialEq, Eq)]

struct VolumeAllocation { serial: u32, component: usize, component_serial: u32, resource: u32 }

#[derive(Clone, Copy, PartialEq, Eq)]

struct VolumeOwner { allocation: VolumeAllocation, world: u32 }

unsafe fn volume_allocation(source: usize) -> Option<VolumeAllocation> {

    if word(source, 0)? != SMOKE_SPHERE { return None; }

    let component = word(source, 0x320)? as usize;

    let allocation = VolumeAllocation { serial: word(source, 0x20)?, component,

        component_serial: word(component, 0x20)?, resource: word(component, 0x30)? };

    (allocation.resource != 0).then_some(allocation)

}

unsafe fn volume_source(source: usize) -> Option<VolumeOwner> {

    let allocation = volume_allocation(source)?;

    if word(allocation.component, 0)? != SMOKE_COMPONENT { return None; }

    let world = word(source, 0x144)?;

    (world != 0).then_some(VolumeOwner { allocation, world })

}

// Temporary pre-visual-acceptance diagnostics. Bounded, no addresses/identifiers.

static VOLUME_DIAGNOSTICS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

static SMOKE_SKIP_DIAGNOSTIC: AtomicBool = AtomicBool::new(false);

pub(super) unsafe fn with_volume<R>(enabled: bool, source: usize, native: impl FnOnce() -> R) -> R {

    source_probe_entry(8, "volume");
    if enabled { observe_source_type(1, source); }

    let replica_token = replica::begin(enabled, source);

    let witness_world = source_sphere_volume_begin(enabled, source);

    let allocation = if enabled { volume_allocation(source) } else { None };

    let owner = if enabled { volume_source(source) } else { None };

    let probe = source_probe_volume(enabled, source, owner.is_some());

    let result = native(); // complete native creation/attach/state transition exactly once

    let mut particle = 0usize;

    if let Some(allocation) = allocation {

        // Do not classify a recycled/replaced source, component or FX resource.

        if volume_allocation(source) == Some(allocation) {

            particle = word(source, 0x3bc).unwrap_or(0) as usize;

            // Native's non-null +30 branch always allocates a fresh actor. Even

            // another component's allocation must not inherit a reused address.

            forget(particle);

            crate::overload_particles::forget(particle);

            source_sphere_volume_completed(source, allocation, witness_world, particle);

            replica::completed(source, allocation, replica_token, particle);

            if let Some(owner) = owner {

                if volume_source(source) == Some(owner) { register(Some(owner.world), particle); }

            }

        }

    }

    if owner.is_some() && VOLUME_DIAGNOSTICS.fetch_update(Ordering::Relaxed, Ordering::Relaxed,

        |n| (n < 8).then_some(n + 1)).is_ok()

    {

        let id = identity(particle);

        let registered = id.is_some() && CLOUDS.lock().ok().is_some_and(|c| c.get(particle) == id);

        let root_present = registered && word(particle, 0x30c).is_some_and(|n| n != 0);

        tracing::info!("SCBL_THERMAL_SMOKE volume_event=true registered={} root_present={} temporary=true", registered, root_present);

    }

    source_probe_finish(probe, source, None, 0x3bc);

    result

}



// Local source provenance: E3E210 returns the exact sphere it creates before

// the caller can delete the grenade. No retained source or raw +324 ancestry.

// Replica E3D220 has a separate exact-callsite scope below; flag400/fallback remains native.

// A scoped simulation-thread witness proves this call actually reached the

// resource-producing E30F00 path. It is never used as a render-thread tag.

#[derive(Clone, Copy)]

struct SourceSphereWitness { sphere: usize, allocation: VolumeAllocation, world: u32, particle: Identity }

#[derive(Clone, Copy)]

struct SourceSphereFrame { witness: Option<SourceSphereWitness> }

std::thread_local! {

    static SOURCE_SPHERE_SCOPE: std::cell::Cell<Option<SourceSphereFrame>> = const { std::cell::Cell::new(None) };

}

struct SourceSphereScope { previous: Option<SourceSphereFrame> }

impl SourceSphereScope {

    fn enter() -> Self {

        Self { previous: SOURCE_SPHERE_SCOPE.with(|slot| slot.replace(Some(SourceSphereFrame { witness: None }))) }

    }

    fn witness(&self) -> Option<SourceSphereWitness> {

        SOURCE_SPHERE_SCOPE.with(|slot| slot.get().and_then(|frame| frame.witness))

    }

}

impl Drop for SourceSphereScope {

    fn drop(&mut self) { SOURCE_SPHERE_SCOPE.with(|slot| slot.set(self.previous)); }

}

unsafe fn source_sphere_volume_begin(enabled: bool, sphere: usize) -> Option<u32> {

    let active = SOURCE_SPHERE_SCOPE.with(|slot| {

        if slot.get().is_none() { false }

        else { slot.set(Some(SourceSphereFrame { witness: None })); true }

    });

    if !active || !enabled { return None; }

    word(sphere, 0x144).filter(|world| *world != 0)

}

unsafe fn source_sphere_volume_completed(sphere: usize, allocation: VolumeAllocation,

    world: Option<u32>, particle: usize)

{

    let Some(world) = world else { return; };

    if word(sphere, 0x144) != Some(world) { return; }

    let Some(particle) = identity(particle).filter(|id| id.world == world) else { return; };

    SOURCE_SPHERE_SCOPE.with(|slot| {

        if slot.get().is_some() {

            slot.set(Some(SourceSphereFrame { witness: Some(SourceSphereWitness { sphere, allocation, world, particle }) }));

        }

    });

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

struct SourceSphereOwner {

    source_class: u32, serial: u32, world: u32, component: usize,

    component_class: u32, component_serial: u32, resource: u32,

}

unsafe fn source_sphere_owner(source: usize) -> Option<SourceSphereOwner> {

    let source_class = word(source, 0)?;

    if !matches!(source_class, SMOKE | ADV_SMOKE) { return None; }

    let component = word(source, 0x614)? as usize;

    // These are the native direct-component branch's actual preconditions.

    if component == 0 || word(component, 0x34)? & 0x400 != 0 { return None; }

    let owner = SourceSphereOwner { source_class, serial: word(source, 0x20)?,

        world: word(source, 0x144)?, component, component_class: word(component, 0)?,

        component_serial: word(component, 0x20)?, resource: word(component, 0x30)? };

    (owner.world != 0 && owner.resource != 0).then_some(owner)

}

// Temporary, at most 15 records per DLL lifetime: source kind x outcome.

// Other grenades cannot consume Smoke slots. No addresses, names or origin reads.

static SOURCE_SPHERE_EVENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn source_sphere_event_claim(bits: &std::sync::atomic::AtomicU32, kind: u32, outcome: u32) -> bool {

    if kind >= 3 || outcome >= 5 { return false; }

    let bit = 1u32 << (kind * 5 + outcome);

    bits.load(Ordering::Relaxed) & bit == 0 && bits.fetch_or(bit, Ordering::Relaxed) & bit == 0

}

unsafe fn source_sphere_event(kind: u32, qualified: bool, mut outcome: u32, particle: usize, world: Option<u32>) {

    let mut registered = false;

    if outcome == 3 {

        // After both result categories have occurred, no diagnostic identity or

        // registry lookup is needed. Normal registration behavior is unchanged.

        let result_bits = (1u32 << (kind * 5 + 3)) | (1u32 << (kind * 5 + 4));

        if SOURCE_SPHERE_EVENTS.load(Ordering::Relaxed) & result_bits == result_bits { return; }

        if let Some(id) = identity(particle).filter(|id| Some(id.world) == world) {

            registered = CLOUDS.lock().ok().is_some_and(|clouds| clouds.get(particle) == Some(id));

        }

        if registered { outcome = 4; }

    }

    if !source_sphere_event_claim(&SOURCE_SPHERE_EVENTS, kind, outcome) { return; }

    let source_kind = match kind {0 => "smoke", 1 => "advanced_smoke", _ => "other"};

    let outcome = match outcome {0 => "refused_qualification", 1 => "refused_transition",

        2 => "refused_sphere", 3 => "refused_particle_or_capacity", _ => "registered"};

    let root_present = registered && word(particle, 0x30c).is_some_and(|root| root != 0);

    tracing::info!("SCBL_THERMAL_SMOKE source_sphere_event=true source_kind={} qualified={} outcome={} registered={} root_present={} category_limit=15 temporary=true runtime_acceptance=pending",

        source_kind, qualified, outcome, registered, root_present);

}

pub(super) unsafe fn with_source_sphere(enabled: bool, source: usize,

    native: impl FnOnce() -> usize) -> usize

{

    // Even disabled or non-Smoke nested sources get a fresh frame and cannot

    // donate a particle to an enclosing Smoke source. Drop restores the caller.

    if enabled { observe_source_type(0, source); }
    let scope = SourceSphereScope::enter();

    let owner = if enabled { source_sphere_owner(source) } else { None };

    let kind = match owner.map(|owner| owner.source_class).or_else(|| word(source, 0)) {

        Some(SMOKE) => 0, Some(ADV_SMOKE) => 1, _ => 2,

    };

    let sphere = native(); // thiscall(source, position, flag, flag)->sphere, once

    let (mut outcome, mut particle) = (0, 0usize);

    if let Some(owner) = owner {

        outcome = 1;

        if source_sphere_owner(source) == Some(owner) {

            outcome = 2;

            if word(sphere, 0) == Some(SMOKE_SPHERE)

            && word(sphere, 0x144) == Some(owner.world)

            && word(sphere, 0x320) == Some(owner.component as u32)

            {

                if let Some(witness) = scope.witness().filter(|witness|

                    witness.sphere == sphere && witness.world == owner.world

                    && witness.allocation.component == owner.component

                    && witness.allocation.component_serial == owner.component_serial

                    && witness.allocation.resource == owner.resource

                    && volume_allocation(sphere) == Some(witness.allocation)

                    && word(sphere, 0x3bc) == Some(witness.particle.address as u32)

                    && identity(witness.particle.address) == Some(witness.particle))

                {

                    outcome = 3;

                    particle = witness.particle.address;

                    register(Some(owner.world), particle);

                }

            }

        }

    }

    source_sphere_event(kind, owner.is_some(), outcome, particle, owner.map(|owner| owner.world));

    sphere

}



pub(super) fn with_delete<R>(particle: usize, native: impl FnOnce() -> R) -> R {

    forget(particle);

    crate::overload_particles::forget(particle);

    native()

}

pub(super) unsafe fn with_particle<R>(enabled: bool, render: usize, record: usize, skipped: R, native: impl FnOnce() -> R) -> R {

    if enabled && (hidden(render, record) || crate::overload_particles::hidden(record)) { skipped } else { native() }

}



// Replica provenance: only E3D220's resolved-default and ordinary clone CALLs.
// No global class lookup hook, resource allowlist or +324 ancestry assumption.
mod replica {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    type Outer = unsafe extern "thiscall" fn(usize, usize, usize) -> usize;
    type Unary = unsafe extern "cdecl" fn(usize) -> usize;
    static OUTER: OnceLock<GenericDetour<Outer>> = OnceLock::new();
    static CAST: AtomicUsize = AtomicUsize::new(0);
    static CLONE: AtomicUsize = AtomicUsize::new(0);
    static LIVE: AtomicBool = AtomicBool::new(false);
    static STOPPED: AtomicBool = AtomicBool::new(false);
    static COMMIT: Mutex<()> = Mutex::new(());

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Component { address: usize, class: u32, serial: u32, uclass: u32, resource: u32 }
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct DefaultSource { address: usize, class: u32, serial: u32, component: Component }
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Sphere { address: usize, serial: u32, world: u32 }
    #[derive(Clone, Copy)]
    struct Frame { token: usize, sphere: Option<Sphere>, source: Option<DefaultSource>,
        clone: Option<Component>, witness: Option<Identity> }
    std::thread_local! {
        static FRAME: std::cell::Cell<Option<Frame>> = const { std::cell::Cell::new(None) };
        static SEQUENCE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    struct Scope { token: usize, previous: Option<Frame>, local: Option<SourceSphereFrame> }
    fn active() -> bool { LIVE.load(Ordering::Acquire) && READY.load(Ordering::Acquire) }
    unsafe fn component(p: usize) -> Option<Component> {
        let value = Component { address: p, class: word(p, 0)?, serial: word(p, 0x20)?,
            uclass: word(p, 0x24)?, resource: word(p, 0x30)? };
        (value.resource != 0).then_some(value)
    }
    unsafe fn source(p: usize) -> Option<DefaultSource> {
        let class = word(p, 0)?;
        if !matches!(class, SMOKE | ADV_SMOKE) { return None; }
        Some(DefaultSource { address: p, class, serial: word(p, 0x20)?,
            component: component(word(p, 0x614)? as usize)? })
    }
    unsafe fn sphere(p: usize) -> Option<Sphere> {
        if word(p, 0)? != SMOKE_SPHERE { return None; }
        let value = Sphere { address: p, serial: word(p, 0x20)?, world: word(p, 0x144)? };
        (value.world != 0).then_some(value)
    }
    fn frame() -> Option<Frame> {
        if SOURCE_SPHERE_SCOPE.with(|s| s.get().is_some()) { return None; }
        FRAME.with(|s| s.get())
    }
    fn save(value: Frame) {
        FRAME.with(|s| {
            if s.get().is_some_and(|f| f.token == value.token) { s.set(Some(value)); }
        });
    }
    impl Scope {
        unsafe fn enter(p: usize) -> Self {
            let token = SEQUENCE.with(|s| match s.get().checked_add(1) {
                Some(n) => { s.set(n); n }, None => 0,
            });
            let value = Frame { token, sphere: if token != 0 && active() { sphere(p) } else { None },
                source: None, clone: None, witness: None };
            let previous = FRAME.with(|s| s.replace(Some(value)));
            let local = SOURCE_SPHERE_SCOPE.with(|s| s.replace(None));
            Self { token, previous, local }
        }
        unsafe fn finish(&self) {
            // No lock spans a native callback. Shutdown takes the same commit gate
            // before thermal_smoke clears its registries, preventing a late insert.
            let Ok(_commit) = COMMIT.lock() else { return; };
            if !active() { return; }
            let Some(f) = frame().filter(|f| f.token == self.token) else { return; };
            let (Some(s), Some(d), Some(c), Some(id)) = (f.sphere, f.source, f.clone, f.witness) else { return; };
            if sphere(s.address) != Some(s) || source(d.address) != Some(d)
                || component(c.address) != Some(c) || word(s.address, 0x320) != Some(c.address as u32)
                || word(s.address, 0x3bc) != Some(id.address as u32) || identity(id.address) != Some(id)
                || id.world != s.world { return; }
            register(Some(s.world), id.address);
        }
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            FRAME.with(|s| s.set(self.previous));
            SOURCE_SPHERE_SCOPE.with(|s| s.set(self.local));
        }
    }
    unsafe extern "thiscall" fn outer(p: usize, net: usize, payload: usize) -> usize {
        let scope = Scope::enter(p); // even disabled/unknown nested calls are barriers
        let result = OUTER.get().expect("replica published before enable").call(p, net, payload);
        scope.finish();
        result // raw native EAX, not a claimed sphere return value
    }
    unsafe extern "cdecl" fn cast(p: usize) -> usize {
        let token = frame().map(|f| f.token);
        let native: Unary = std::mem::transmute(CAST.load(Ordering::Acquire));
        let result = native(p);
        if active() {
            if let Some(mut f) = frame().filter(|f| Some(f.token) == token && f.sphere.is_some()) {
                f.source = source(result); f.clone = None; f.witness = None; save(f);
            }
        }
        result
    }
    unsafe extern "cdecl" fn clone_component(p: usize) -> usize {
        let before = frame().filter(|f| active() && f.sphere.is_some());
        let qualified = before.and_then(|f| f.source).filter(|d|
            d.component.address == p && source(d.address) == Some(*d));
        let native: Unary = std::mem::transmute(CLONE.load(Ordering::Acquire));
        let result = native(p);
        if active() {
            if let Some(mut f) = frame().filter(|f| before.is_some_and(|b| b.token == f.token)) {
                f.clone = qualified.filter(|d| source(d.address) == Some(*d))
                    .and_then(|d| component(result).filter(|c|
                        c.uclass == d.component.uclass && c.class == d.component.class
                            && c.resource == d.component.resource));
                f.witness = None; save(f);
            }
        }
        result
    }
    pub(super) unsafe fn begin(enabled: bool, p: usize) -> Option<usize> {
        let mut f = frame()?;
        f.witness = None; save(f);
        if !enabled || !active() { return None; }
        let (s, d, c) = (f.sphere?, f.source?, f.clone?);
        (s.address == p && sphere(p) == Some(s) && source(d.address) == Some(d)
            && component(c.address) == Some(c) && word(p, 0x320) == Some(c.address as u32))
            .then_some(f.token)
    }
    pub(super) unsafe fn completed(p: usize, allocation: VolumeAllocation, token: Option<usize>, particle: usize) {
        if !active() { return; }
        let Some(mut f) = frame().filter(|f| Some(f.token) == token) else { return; };
        let (Some(s), Some(c)) = (f.sphere, f.clone) else { return; };
        if s.address != p || sphere(p) != Some(s) || allocation.serial != s.serial
            || allocation.component != c.address || allocation.component_serial != c.serial
            || allocation.resource != c.resource || component(c.address) != Some(c) { return; }
        f.witness = identity(particle).filter(|id| id.world == s.world); save(f);
    }

    #[derive(Clone, Copy)]
    struct Patch { address: usize, old: [u8; 5], new: [u8; 5], protection: u32 }
    #[repr(C)] #[derive(Default)]
    struct PageInfo { base: usize, allocation: usize, allocation_protect: u32, size: usize, state: u32, protect: u32, kind: u32 }
    struct State { calls: [Option<Patch>; 2], entry: Option<Patch> }
    static STATE: Mutex<State> = Mutex::new(State { calls: [None; 2], entry: None });
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualQuery(p: *const std::ffi::c_void, info: *mut PageInfo, size: usize) -> usize;
        fn VirtualProtect(p: *mut std::ffi::c_void, n: usize, protect: u32, old: *mut u32) -> i32;
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn FlushInstructionCache(h: *mut std::ffi::c_void, p: *const std::ffi::c_void, n: usize) -> i32;
    }
    fn branch(op: u8, address: usize, target: usize) -> [u8; 5] {
        let mut b = [op, 0, 0, 0, 0];
        b[1..].copy_from_slice(&(target as u32).wrapping_sub((address as u32).wrapping_add(5)).to_le_bytes()); b
    }
    unsafe fn bytes(address: usize) -> Option<[u8; 5]> {
        readable(address, 5).then(|| std::slice::from_raw_parts(address as *const u8, 5).try_into().unwrap())
    }
    unsafe fn protection(address: usize) -> Option<u32> {
        let mut info = PageInfo::default();
        if VirtualQuery(address as _, &mut info, std::mem::size_of::<PageInfo>()) == 0
            || !info.base.checked_add(info.size).is_some_and(|end| address.checked_add(5).is_some_and(|n| n <= end)) { return None; }
        Some(info.protect)
    }
    unsafe fn restore_protection(p: Patch) -> bool {
        let mut ignored = 0;
        VirtualProtect(p.address as _, 5, p.protection, &mut ignored) != 0
            && protection(p.address) == Some(p.protection)
    }
    unsafe fn flush(address: usize) -> bool {
        FlushInstructionCache(GetCurrentProcess(), address as _, 5) != 0
    }
    type Protect = unsafe extern "system" fn(*mut std::ffi::c_void, usize, u32, *mut u32) -> i32;
    unsafe fn write(address: usize, old: &[u8; 5], new: &[u8; 5]) -> bool {
        write_with(address, old, new, VirtualProtect, flush)
    }
    unsafe fn write_with(address: usize, old: &[u8; 5], new: &[u8; 5],
        protect: Protect, flush_code: unsafe fn(usize) -> bool) -> bool
    {
        if bytes(address) != Some(*old) { return false; }
        let mut protection = 0;
        if protect(address as _, 5, 0x40, &mut protection) == 0 { return false; }
        std::ptr::copy_nonoverlapping(new.as_ptr(), address as *mut u8, 5);
        let matched = bytes(address) == Some(*new);
        let mut ignored = 0;
        let protected = protect(address as _, 5, protection, &mut ignored) != 0;
        let flushed = flush_code(address); // flush written bytes even if protection restore failed
        matched && protected && flushed
    }
    unsafe fn restore(state: &mut State) -> bool {
        for i in (0..2).rev() {
            if let Some(p) = state.calls[i] {
                let ok = match bytes(p.address) {
                    Some(b) if b == p.old => flush(p.address),
                    Some(b) if b == p.new => write(p.address, &p.new, &p.old),
                    _ => false, // another owner: retain journal, never overwrite
                };
                if ok && bytes(p.address) == Some(p.old) && restore_protection(p) && flush(p.address) { state.calls[i] = None; }
            }
        }
        if let Some(p) = state.entry {
            let owned = bytes(p.address).is_some_and(|b| b == p.old || b == p.new);
            if owned {
                let h = OUTER.get().expect("entry journal has published detour");
                let disabled = h.disable().is_ok();
                let matched = bytes(p.address) == Some(p.old);
                let protected = matched && restore_protection(p);
                let flushed = flush(p.address);
                if disabled && matched && protected && flushed {
                    state.entry = None;
                }
            }
        }
        state.calls.iter().all(Option::is_none) && state.entry.is_none()
    }
    // The owned-byte fixture supplies relocated addresses/expected image hashes,
    // but runs this exact signature, publish, write, detour and restore transaction.
    struct Plan<'a> { entry: usize, cast_site: usize, clone_site: usize, cast_target: usize,
        clone_target: usize, signatures: &'a [(usize, usize, &'a str)] }
    unsafe fn install_at(plan: &Plan<'_>, volume_available: bool,
        writer: unsafe fn(usize, &[u8; 5], &[u8; 5]) -> bool) -> bool
    {
        let Ok(_commit) = COMMIT.lock() else { return false; };
        let Ok(mut state) = STATE.lock() else { return false; };
        if STOPPED.load(Ordering::Acquire) || !volume_available || OUTER.get().is_some() || state.entry.is_some()
            || state.calls.iter().any(Option::is_some) || plan.cast_site != plan.entry + 0x61
            || plan.clone_site != plan.entry + 0x1c6 || plan.signatures.is_empty()
            || !plan.signatures.iter().all(|&(p, n, h)| signature(p, n, h))
            || !readable(plan.entry, 6)
            || std::slice::from_raw_parts(plan.entry as *const u8, 6) != [0x55, 0x8b, 0xec, 0x83, 0xec, 0x14]
        { return false; }
        let pairs = [(plan.cast_site, plan.cast_target, cast as usize),
            (plan.clone_site, plan.clone_target, clone_component as usize)];
        let mut patches = pairs.map(|(p, target, wrapper)| Patch { address: p,
            old: branch(0xe8, p, target), new: branch(0xe8, p, wrapper), protection: 0 });
        for p in &mut patches {
            let Some(value) = protection(p.address) else { return false; }; p.protection = value;
        }
        let Some(entry_protection) = protection(plan.entry) else { return false; };
        if !patches.iter().all(|p| bytes(p.address) == Some(p.old)) { return false; }
        let Ok(hook) = GenericDetour::new(std::mem::transmute::<usize, Outer>(plan.entry), outer as Outer) else { return false; };
        if OUTER.set(hook).is_err() { return false; }
        CAST.store(plan.cast_target, Ordering::Release); CLONE.store(plan.clone_target, Ordering::Release);
        let mut ok = true;
        for (i, p) in patches.into_iter().enumerate() {
            state.calls[i] = Some(p); // record ownership BEFORE a possibly partial write
            if !writer(p.address, &p.old, &p.new) || bytes(p.address) != Some(p.new)
                || protection(p.address) != Some(p.protection) { ok = false; break; }
        }
        if ok {
            let old = [0x55, 0x8b, 0xec, 0x83, 0xec];
            let p = Patch { address: plan.entry, old, new: branch(0xe9, plan.entry, outer as usize), protection: entry_protection };
            state.entry = Some(p); // x86 retour uses the inline five-byte near JMP
            let enabled = bytes(p.address) == Some(p.old) && OUTER.get().unwrap().enable().is_ok();
            let flushed = flush(p.address);
            ok = enabled && bytes(p.address) == Some(p.new)
                && protection(p.address) == Some(p.protection) && flushed;
        }
        if !ok {
            LIVE.store(false, Ordering::Release);
            let restored = restore(&mut state);
            tracing::info!("SCBL_THERMAL_SMOKE replica_producer=false restored={} reason=publish-failed", restored);
            return false;
        }
        LIVE.store(true, Ordering::Release); true
    }
    pub(super) unsafe fn init(volume_available: bool) {
        let signatures = [
            (0xe3d220, 705, "56593a286b365c5d7a202a95776f0c6d62bad77377ab3d5d1d4864902247dde4"),
            (0xc8a830, 80, "c18681e2e549d21cf5b720a56c25547aee9b4a6c0fcefa37532274ec0250b6dc"),
            (0xe318a0, 313, "0d2b4b24c6d22636c1971412320e574fe962a7b2a6f700b275d695da0fba97ff"),
        ];
        let plan = Plan { entry: 0xe3d220, cast_site: 0xe3d281, clone_site: 0xe3d3e6,
            cast_target: 0xc8a830, clone_target: 0xe318a0, signatures: &signatures };
        let installed = install_at(&plan, volume_available, write);
        tracing::info!("SCBL_THERMAL_SMOKE replica_producer={} scope=resolved-smoke-default-normal-clone runtime_acceptance=pending", installed);
    }
    pub(super) unsafe fn deinit() -> bool {
        STOPPED.store(true, Ordering::Release);
        LIVE.store(false, Ordering::Release);
        // Install is one-shot per DLL lifetime. No later init can revive an old frame.
        let Ok(_commit) = COMMIT.lock() else { return false; };
        LIVE.store(false, Ordering::Release);
        let Ok(mut state) = STATE.lock() else { return false; };
        let restored = restore(&mut state);
        if !restored { tracing::info!("SCBL_THERMAL_SMOKE replica_restore=false reason=unresolved-owned-bytes"); }
        restored
    }
}

type CreateNode = unsafe extern "cdecl" fn(usize);

type Factory = unsafe extern "cdecl" fn(usize, usize, f32, usize) -> usize;

type Volume = unsafe extern "thiscall" fn(usize);

type Continuous = unsafe extern "thiscall" fn(usize, f32, u32) -> u32;

type Delete = unsafe extern "thiscall" fn(usize, u32) -> usize;

type Draw = unsafe extern "thiscall" fn(usize, usize, usize);

type DrawResult = unsafe extern "thiscall" fn(usize, usize, usize) -> u8;

static CREATE_NODE: OnceLock<GenericDetour<CreateNode>> = OnceLock::new();

static FACTORY: OnceLock<GenericDetour<Factory>> = OnceLock::new();

static VOLUME: OnceLock<GenericDetour<Volume>> = OnceLock::new();

type SourceSphere = unsafe extern "thiscall" fn(usize, usize, u32, u32) -> usize;

static SOURCE_SPHERE: OnceLock<GenericDetour<SourceSphere>> = OnceLock::new();

static CONTINUOUS: OnceLock<GenericDetour<Continuous>> = OnceLock::new();

static DELETE: OnceLock<GenericDetour<Delete>> = OnceLock::new();

static DRAW: OnceLock<GenericDetour<Draw>> = OnceLock::new();

static D0: OnceLock<GenericDetour<DrawResult>> = OnceLock::new();

static D2: OnceLock<GenericDetour<DrawResult>> = OnceLock::new();

static D6: OnceLock<GenericDetour<DrawResult>> = OnceLock::new();

static D9: OnceLock<GenericDetour<DrawResult>> = OnceLock::new();

static D1: OnceLock<GenericDetour<DrawResult>> = OnceLock::new();

static EXTRA_OVERRIDE: OnceLock<GenericDetour<Draw>> = OnceLock::new();

static EXTRA_OVERLAY: OnceLock<GenericDetour<Draw>> = OnceLock::new();

static EXTRA_OVERDRAW: OnceLock<GenericDetour<Draw>> = OnceLock::new();

static EXTRA_MIXED_OVERLAP: OnceLock<GenericDetour<Draw>> = OnceLock::new();



unsafe extern "cdecl" fn create_node(actor: usize) {

    with_node_creation(READY.load(Ordering::Acquire), actor, || {

        CREATE_NODE.get().expect("published before enable").call(actor)

    });

}

unsafe extern "cdecl" fn factory(source: usize, definition: usize, gain: f32, position: usize) -> usize {

    let particle = with_spawn(READY.load(Ordering::Acquire), source, definition, || {

        FACTORY.get().expect("published before enable").call(source, definition, gain, position)

    });

    if !READY.load(Ordering::Acquire) { forget(particle); }

    particle

}

unsafe extern "thiscall" fn volume(source: usize) {

    with_volume(READY.load(Ordering::Acquire), source, || {

        VOLUME.get().expect("published before enable").call(source)

    });

    if !READY.load(Ordering::Acquire) {

        if let Some(particle) = word(source, 0x3bc) { forget(particle as usize); }

    }

}

unsafe extern "thiscall" fn source_sphere(source: usize, position: usize, flag_a: u32, flag_b: u32) -> usize {

    let sphere = with_source_sphere(READY.load(Ordering::Acquire), source, || {

        SOURCE_SPHERE.get().expect("published before enable").call(source, position, flag_a, flag_b)

    });

    if !READY.load(Ordering::Acquire) && word(sphere, 0) == Some(SMOKE_SPHERE) {

        if let Some(particle) = word(sphere, 0x3bc) { forget(particle as usize); }

    }

    sphere

}

unsafe extern "thiscall" fn continuous(source: usize, dt: f32, kind: u32) -> u32 {

    let result = with_continuous(READY.load(Ordering::Acquire), source, || {

        CONTINUOUS.get().expect("published before enable").call(source, dt, kind)

    });

    if !READY.load(Ordering::Acquire) {

        if let Some(particle) = word(source, 0x860) { forget(particle as usize); }

    }

    result

}

unsafe extern "thiscall" fn delete(particle: usize, flags: u32) -> usize {

    // Remove before the native destructor can free/recycle storage. No native

    // callback runs with our mutex held; trampolines remain alive until process exit.

    with_delete(particle, || DELETE.get().expect("published before enable").call(particle, flags))

}

unsafe extern "thiscall" fn draw(mode: usize, render: usize, record: usize) {

    with_particle(READY.load(Ordering::Acquire), render, record, (), || {

        DRAW.get().expect("published before enable").call(mode, render, record)

    });

}

macro_rules! draw_result {

    ($name:ident, $slot:ident) => {

        unsafe extern "thiscall" fn $name(mode: usize, render: usize, record: usize) -> u8 {

            with_particle(READY.load(Ordering::Acquire), render, record, 1, || {

                $slot.get().expect("published before enable").call(mode, render, record)

            })

        }

    };

}

draw_result!(draw0, D0); draw_result!(draw2, D2); draw_result!(draw6, D6);

draw_result!(draw9, D9); draw_result!(draw1, D1);

// These four particle-only MaterialImpl vtable+10 methods bypass ParticleMaterial's

// six existing draw entrypoints. All use the same synchronous (render,record)

// borrow and return void/ret8. Classification and role decisions are unchanged.

macro_rules! extra_draw {

    ($name:ident, $slot:ident) => {

        unsafe extern "thiscall" fn $name(mode: usize, render: usize, record: usize) {

            with_particle(READY.load(Ordering::Acquire), render, record, (), || {

                $slot.get().expect("published before enable").call(mode, render, record)

            });

        }

    };

}

extra_draw!(extra_override, EXTRA_OVERRIDE);

extra_draw!(extra_overlay, EXTRA_OVERLAY);

extra_draw!(extra_overdraw, EXTRA_OVERDRAW);

extra_draw!(extra_mixed_overlap, EXTRA_MIXED_OVERLAP);

unsafe fn install<T: Function + HookableWith<T>>(slot: &OnceLock<GenericDetour<T>>, target: T, callback: T) -> bool {

    if slot.get().is_some() { return false; }

    let Ok(hook) = GenericDetour::new(target, callback) else { return false; };

    if slot.set(hook).is_err() { return false; }

    slot.get().expect("published before enable").enable().is_ok()

}

unsafe fn install_extra_draws() {

    let entries: &[(&str, usize, usize, &str, &OnceLock<GenericDetour<Draw>>, Draw)] = &[

        ("override", 0x170b920, 2546, "d1f19f76263dcf6c18f5d48a0324412189c4dfe629aef5bdac4a7e644f192061", &EXTRA_OVERRIDE, extra_override as Draw),

        ("overlay", 0x170e8f0, 269, "40d2d16d347328cf65335378846c72cc695321e210590bacaf9b5c79bc525506", &EXTRA_OVERLAY, extra_overlay as Draw),

        ("overdraw", 0x170f4e0, 362, "1804bcdf5e2376fe2cba8a3ba38ac1d8976e3ca20259a3cebd165e623c405dba", &EXTRA_OVERDRAW, extra_overdraw as Draw),

        ("mixed_overlap", 0x1712b30, 353, "a66dca4b1acedd7935c3afbf07123ac1685371ee490c185409258f21f993cfa7", &EXTRA_MIXED_OVERLAP, extra_mixed_overlap as Draw),

    ];

    for &(route, address, size, hash, slot, callback) in entries {

        let matches = signature(address, size, hash);

        let installed = matches && install(slot, std::mem::transmute::<usize, Draw>(address), callback);

        if !installed { if let Some(h) = slot.get() { let _ = h.disable(); } }

        tracing::info!("SCBL_THERMAL_SMOKE extra_particle_route={} enabled={} signature={} scope=existing-classified-root runtime_acceptance=pending", route, installed, matches);

    }

}

pub(super) fn ready() -> bool { READY.load(Ordering::Acquire) }

pub(super) unsafe fn init(world: usize) {

    if world != 0x589860 || READY.load(Ordering::Acquire) { return; }

    if !NATIVE.iter().all(|&(address, size, hash)| signature(address, size, hash)) {

        tracing::warn!("SCBL_THERMAL_SMOKE enabled=false reason=native-signature"); return;

    }

    if install(&DELETE, std::mem::transmute::<usize, Delete>(0x908880), delete as Delete)

        && install(&DRAW, std::mem::transmute::<usize, Draw>(0x1703e20), draw as Draw)

        && install(&D0, std::mem::transmute::<usize, DrawResult>(0x1706000), draw0 as DrawResult)

        && install(&D2, std::mem::transmute::<usize, DrawResult>(0x1707d30), draw2 as DrawResult)

        && install(&D6, std::mem::transmute::<usize, DrawResult>(0x1708a00), draw6 as DrawResult)

        && install(&D9, std::mem::transmute::<usize, DrawResult>(0x1709640), draw9 as DrawResult)

        && install(&D1, std::mem::transmute::<usize, DrawResult>(0x170a280), draw1 as DrawResult)

        && install(&CREATE_NODE, std::mem::transmute::<usize, CreateNode>(0x9da310), create_node as CreateNode)

        && install(&CONTINUOUS, std::mem::transmute::<usize, Continuous>(0xaa8440), continuous as Continuous)

        && install(&FACTORY, std::mem::transmute::<usize, Factory>(0xea4780), factory as Factory)

    {

        // Optional producer: a signature/hook mismatch disables ONLY this new

        // source. Existing grenade, root-node and draw gates remain available.

        let volume_producer = signature(0xe30f00, 1347,

            "d41925e4246664162fa78f4b68a3df6c6b19c8f94727e2ac3a7a4da36bc72d6f")

            && install(&VOLUME, std::mem::transmute::<usize, Volume>(0xe30f00), volume as Volume);

        if !volume_producer { if let Some(h) = VOLUME.get() { let _ = h.disable(); } }

        tracing::info!("SCBL_THERMAL_SMOKE volume_producer={} scope=exact-smoke-component runtime_acceptance=pending", volume_producer);

        // Independent fail-open producer; failure does not disable existing

        // grenade, sphere, root or draw routes (nor FIX8 / overload features).

        let source_producer = volume_producer && signature(0xe3e210, 1382,

            "dc833630f85821a705bb28765746d9fcd73b4082576fde939ac64edd8cb9d97f")

            && install(&SOURCE_SPHERE, std::mem::transmute::<usize, SourceSphere>(0xe3e210), source_sphere as SourceSphere);

        if !source_producer { if let Some(h) = SOURCE_SPHERE.get() { let _ = h.disable(); } }

        tracing::info!("SCBL_THERMAL_SMOKE source_sphere_producer={} scope=exact-smoke-actor-direct-component network_replica=separate-producer runtime_acceptance=pending", source_producer);

        replica::init(volume_producer);

        install_extra_draws();

        READY.store(true, Ordering::Release);

        tracing::info!("SCBL_THERMAL_SMOKE source_observer=true scope=three-existing-producers category_limit=22 suppression=unchanged temporary=true");

        tracing::info!("SCBL_THERMAL_SMOKE enabled=true scope=mode8-smoke-grenade-particle continuous_cloud=true root_node_cache=true native_smoke_state=unchanged runtime_acceptance=pending");

    } else { deinit(); }

}

pub(super) unsafe fn deinit() {

    crate::overload_particles::deinit();

    READY.store(false, Ordering::Release);

    let _ = replica::deinit();

    if let Some(h) = SOURCE_SPHERE.get() { let _ = h.disable(); }

    if let Some(h) = VOLUME.get() { let _ = h.disable(); }

    if let Some(h) = CONTINUOUS.get() { let _ = h.disable(); }

    if let Some(h) = FACTORY.get() { let _ = h.disable(); }

    if let Some(h) = CREATE_NODE.get() { let _ = h.disable(); }

    if let Some(h) = DRAW.get() { let _ = h.disable(); }

    for slot in [&EXTRA_OVERRIDE, &EXTRA_OVERLAY, &EXTRA_OVERDRAW, &EXTRA_MIXED_OVERLAP] {

        if let Some(h) = slot.get() { let _ = h.disable(); }

    }

    for slot in [&D0, &D2, &D6, &D9, &D1] { if let Some(h) = slot.get() { let _ = h.disable(); } }

    if let Some(h) = DELETE.get() { let _ = h.disable(); }

    if let Ok(mut clouds) = CLOUDS.lock() { *clouds = Clouds::new(); HAS_SMOKE.store(false, Ordering::Release); }

    if let Ok(mut roots) = ROOTS.lock() { *roots = Roots::new(); }

}

// Filled only from the pinned on-disk native image by candidate_test.py.

const NATIVE: &[(usize, usize, &str)] = &[

    (0x9da310, 706, "b70b34e1a3700fd1d3d30807034f5ea09a9d3fc38937fd3ff3b63e43168c769c"),

    (0xaa8440, 361, "00fc889070247646e3527af62acacca12da7693d2da128701d0b815df6b2266f"),

    (0xea4780, 560, "a37aabf6d1fcf7820c6a166d7f836ef9f3491bfbe8317205578f04250d1c2408"),

    (0x908880, 33, "b6dbaa9538e7c6575ae0a53c2e63975db94c989d56bdfaa2f4f23df9a90dc414"),

    (0x1703e20, 7460, "9db3c5d30bf700e29982b24ccd2731ccdf3966dd6792d3ca236794fccd582415"),

    (0x1706000, 7462, "5433aaf0cadfa0f10369bab3a56beb0407019eb56c3e9bcddb3a251910df56a4"),

    (0x1707d30, 3267, "91afd410cb6e5877eb4fa816dc50e6ec20d25697f2193ba883886ca2b9ef0332"),

    (0x1708a00, 3127, "910015a13e419c25fdf09b33ebcb8bdd37a91b2d70a01cbf5155b65fc09c3cc4"),

    (0x1709640, 3127, "696ab50c3d1b59b8ed674113c238b35826bcc29e2d60de1fbaffd0f0681f091b"),

    (0x170a280, 3127, "d4510748717237a5e68a31925fbc23f86e075f10424e26d6ed9c4f0b31653025"),

    (0xe246d2, 40, "bd2495dff4bdb0ab5b51dfc7d3cbdcbc7c6d56fcce54be3d5cb0e21d482fd01e"),

    (0x160d080, 168, "c3c4ae7a52c490b16aa3bc710ee9d90163780214eb7c1144f23eeb4c93ce43f2"),

];

#[cfg(test)]

#[path = "thermal_smoke_tests.rs"]

mod tests;



#[cfg(test)]

#[path = "particle_roots_tests.rs"]

mod particle_roots_tests;



#[cfg(test)]

#[path = "smoke_volume_tests.rs"]

mod smoke_volume_tests;



// Offline-only seam: exercise the actual production extra-overlay callback and

// GenericDetour on a relocated native copy, never on a running game.

#[cfg(test)]

pub(super) unsafe fn fixture_install_extra_overlay(target: usize) -> bool {

    let ok = install(&EXTRA_OVERLAY, std::mem::transmute::<usize, Draw>(target), extra_overlay as Draw);

    READY.store(ok, Ordering::Release);

    ok

}

#[cfg(test)]

pub(super) unsafe fn fixture_disable_extra_overlay() {

    READY.store(false, Ordering::Release);

    if let Some(h) = EXTRA_OVERLAY.get() { let _ = h.disable(); }

}

#[cfg(test)]

#[path = "source_observer_tests.rs"]

mod source_observer_tests;



#[cfg(test)]

#[path = "smoke_source_sphere_tests.rs"]

mod smoke_source_sphere_tests;

// Root's isolated pinned-body runner uses the real production detour callback.

#[cfg(test)]

pub(super) unsafe fn fixture_install_grenade_sphere(target: usize) -> bool {

    let ok = install(&SOURCE_SPHERE, std::mem::transmute::<usize, SourceSphere>(target), source_sphere as SourceSphere);

    READY.store(ok, Ordering::Release);

    ok

}

#[cfg(test)]

pub(super) unsafe fn fixture_disable_grenade_sphere() {

    READY.store(false, Ordering::Release);

    if let Some(h) = SOURCE_SPHERE.get() { let _ = h.disable(); }

}

#[cfg(test)]

pub(super) unsafe fn fixture_install_source_sphere(target: usize) -> bool { fixture_install_grenade_sphere(target) }

#[cfg(test)]

pub(super) unsafe fn fixture_disable_source_sphere() { fixture_disable_grenade_sphere(); }


#[cfg(test)]
#[path = "smoke_source_types_tests.rs"]
mod source_types_tests;
