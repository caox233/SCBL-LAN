//! Exact Saboteur release particle identity; the shared particle owner handles
//! drawing/deletion. Local native viewer role decides visibility, not emitter
//! locality, controlled pawn lifetime, or the observed camera target.
use crate::equipment_native::{readable, word};
use std::{ffi::c_void, sync::{Mutex, OnceLock, atomic::{AtomicBool, Ordering}}};

const SABOTEUR: u32 = 0x02a3f3a4;
const PARTICLE: u32 = 0x029e8754;
const LIMIT: usize = 128;
static ANY: AtomicBool = AtomicBool::new(false);
static EFFECTS: Mutex<[Option<Identity>; LIMIT]> = Mutex::new([None; LIMIT]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity { address: usize, key: u32, world: u32, template: u32 }

unsafe fn identity(p: usize) -> Option<Identity> {
    if !readable(p, 0x354) || word(p, 0)? != PARTICLE { return None; }
    // Native emitter rebuild reads +350 as its template input; +354 is a cached definition, not an epoch.
    let template = word(p, 0x350)?;
    let world = word(p, 0x144)?;
    (world != 0 && template != 0).then_some(Identity { address: p, key: word(p, 0x20)?, world, template })
}

// Reset only retires our classification. Shared root mappings may already
// belong to a new smoke effect and are owned by creation/deletion hooks.
pub(super) fn retire_registration(particle: usize) {
    crate::overload_visibility::retire(particle);
    crate::overload_emission_lights::forget(particle);
    crate::overload_audio::forget(particle);
    if !ANY.load(Ordering::Acquire) { return; }
    let Ok(mut effects) = EFFECTS.lock() else { return; };
    for entry in effects.iter_mut() {
        if entry.is_some_and(|id| id.address == particle) { *entry = None; }
    }
    ANY.store(effects.iter().any(Option::is_some), Ordering::Release);
}

pub(super) fn forget(particle: usize) {
    crate::thermal_smoke::forget_root(particle);
    retire_registration(particle);
}

pub(super) fn registered_address(particle: usize) -> bool {
    ANY.load(Ordering::Acquire) && EFFECTS.lock().ok().is_some_and(|effects|
        effects.iter().flatten().any(|id| id.address == particle))
}

pub(super) unsafe fn record_spawn(ability: usize, particle: usize) {
    probe::note(0);
    // This site returns a newly created actor. Reused addresses cannot inherit
    // classification even if the next ability/world no longer qualifies.
    forget(particle);
    if word(ability, 0) != Some(SABOTEUR) { probe::note(1); return; }
    let Some(owner) = word(ability, 0x6c).filter(|v| *v != 0) else { probe::note(2); return; };
    let Some(id) = identity(particle) else { probe::note(3); return; };
    if word(owner as usize, 0x144) != Some(id.world) { probe::note(4); return; }
    crate::overload_audio::record(ability,particle);
    let Ok(mut effects) = EFFECTS.lock() else { return; };
    if let Some(slot) = effects.iter_mut().find(|e| e.is_none()) {
        *slot = Some(id);
        ANY.store(true, Ordering::Release);
        probe::note(5);
        drop(effects);
        crate::thermal_smoke::track_root(particle);
        crate::overload_visibility::captured(particle);
    } else { probe::note(6); }
}

pub(super) unsafe fn contains(particle: usize) -> bool {
    if !ANY.load(Ordering::Acquire) { return false; }
    let expected = {
        let Ok(effects) = EFFECTS.lock() else { return false; };
        effects.iter().flatten().find(|id| id.address == particle).copied()
    };
    expected.is_some() && identity(particle) == expected
}

pub(super) fn clear() {
    crate::overload_audio::clear();
    crate::overload_emission_lights::clear();
    if let Ok(mut effects) = EFFECTS.lock() {
        let old = *effects;
        *effects = [None; LIMIT]; ANY.store(false, Ordering::Release);
        drop(effects);
        for id in old.iter().flatten() { crate::thermal_smoke::forget_root(id.address); }
    }
}

/// Relocate F059D2's test/jz/xorps while preserving all machine state across the
/// cdecl callback(ability=ESI, particle=EAX), including aligned x87/SSE state.
pub(super) fn bridge_bytes(base: u32, callback: u32, resume: u32, no_particle: u32) -> Vec<u8> {
    let mut b = vec![
        0x9c, 0x60, 0x89, 0xe0, // pushfd; pushad; eax = saved GPR frame
        0x81, 0xec, 0x20, 0x02, 0, 0, 0x83, 0xe4, 0xf0,
        0x89, 0x84, 0x24, 0, 0x02, 0, 0, // save original stack pointer
        0x0f, 0xae, 0x04, 0x24, 0xfc, // fxsave [esp]; cld
        0xff, 0x70, 0x1c, 0x56, 0xb8, // saved EAX; ESI; callback
    ];
    b.extend(callback.to_le_bytes());
    b.extend([
        0xff, 0xd0, 0x83, 0xc4, 8, // call eax; cdecl cleanup
        0x0f, 0xae, 0x0c, 0x24, // fxrstor [esp]
        0x8b, 0xa4, 0x24, 0, 0x02, 0, 0, 0x61, 0x9d,
        0x85, 0xc0, 0x0f, 0x84, // displaced test eax,eax; relocated jz
    ]);
    b.extend(no_particle.wrapping_sub(base.wrapping_add(b.len() as u32 + 4)).to_le_bytes());
    b.extend([0x0f, 0x57, 0xc0, 0xe9]); // displaced xorps xmm0,xmm0
    b.extend(resume.wrapping_sub(base.wrapping_add(b.len() as u32 + 4)).to_le_bytes());
    b
}

// Resolve local viewer only after a bounded registry hit, not for every particle.
pub(super) unsafe fn hidden(record: usize) -> bool {
    let Some(node)=word(record,0).map(|p|p as usize) else {return false};
    hidden_node(node)
}

pub(super) unsafe fn hidden_node(node: usize) -> bool {
    if !READY.load(Ordering::Acquire) || !ANY.load(Ordering::Acquire) { return false; }
    let Some(owner)=crate::thermal_smoke::particle_owner_node(node) else {return false};
    if !contains(owner) {return false;}
    probe::note(7);
    if word(owner,0x30c)!=Some(node as u32) {probe::draw(8,"root-mismatch");return false;}
    let Some(world)=word(owner,0x144) else {return false};
    let hide=hide_visuals(world as usize);
    probe::draw(if hide {9}else{10},if hide {"not-spy-goggles-hidden"}else{"spy-goggles-native"});
    hide
}

pub(super) unsafe fn hide_visuals(world:usize)->bool {
    crate::overload_visual_policy::hidden(world)
}
// Called only on a HUD visibility transition; no whole-world traversal.
pub(super) unsafe fn refresh_visuals() {
    if !READY.load(Ordering::Acquire) || !ANY.load(Ordering::Acquire) {return;}
    let snapshot={let Ok(effects)=EFFECTS.lock()else{return;};*effects};
    for id in snapshot.iter().flatten() {
        if contains(id.address) {
            crate::overload_visibility::retire(id.address);
            crate::overload_visibility::root_ready(id.address);
        }
    }
}

retour::static_detour! {static Release: unsafe extern "thiscall" fn(usize);}
fn released(ability:usize) {
    let observed=probe::begin();
    unsafe {crate::overload_visibility::with_release(||crate::overload_audio::release(ability,||Release.call(ability)));}
    if let Some(sequence)=observed { probe::report(sequence); }
}
static READY: AtomicBool = AtomicBool::new(false);
static THUNK: OnceLock<usize> = OnceLock::new();
const SITE: usize=0xf059d2;
const ORIGINAL:[u8;7]=[0x85,0xc0,0x74,0x40,0x0f,0x57,0xc0];
#[link(name="kernel32")]
extern "system" {
    fn VirtualAlloc(p:*mut c_void,n:usize,t:u32,protect:u32)->*mut c_void;
    fn VirtualProtect(p:*mut c_void,n:usize,protect:u32,old:*mut u32)->i32;
    fn VirtualFree(p:*mut c_void,n:usize,t:u32)->i32;
    fn FlushInstructionCache(process:*mut c_void,p:*const c_void,n:usize)->i32;
}
unsafe extern "cdecl" fn captured(ability:usize,particle:usize) {
    if READY.load(Ordering::Acquire) {
        record_spawn(ability,particle);
        // Deinitialization can cross the simulation-thread callback.
        if !READY.load(Ordering::Acquire) { forget(particle); }
    }
}
fn jump(from:usize,to:usize)->[u8;7] {
    let mut b=[0xe9,0,0,0,0,0x90,0x90];
    b[1..5].copy_from_slice(&(to as u32).wrapping_sub(from as u32+5).to_le_bytes());b
}
pub(super) unsafe fn init(world:usize) {
    if world!=0x589860 || READY.load(Ordering::Acquire) || THUNK.get().is_some() {return;}
    #[cfg(any(test, feature="goggle-feedback"))]
    let policy_ready=crate::goggle_warning_native::ready();
    #[cfg(not(any(test, feature="goggle-feedback")))]
    let policy_ready=false;
    if !policy_ready {
        tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=visual-policy-tick-not-ready");return;
    }
    if !crate::thermal_smoke::ready() || !crate::overload_audio::gain_ready()
        || !crate::gameplay_features::overcharge_audio_ready() {
        tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=shared-particle-or-audio-owner-not-ready");return;
    }
    if !crate::equipment_native::signature(0xf05950,0x183,"3a3e9a19ee9a87bc9ec7e472cf06b20f614cb5329a01a7dbed254f379eb6b424") {
        tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=native-release-signature");return;
    }
    let memory=VirtualAlloc(std::ptr::null_mut(),128,0x3000,4);
    if memory.is_null() {tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=bridge-allocation");return;}
    let code=bridge_bytes(memory as u32,captured as *const () as u32,(SITE+7) as u32,0xf05a16);
    std::ptr::copy_nonoverlapping(code.as_ptr(),memory as *mut u8,code.len());
    let mut old=0;
    if VirtualProtect(memory,128,0x20,&mut old)==0 || FlushInstructionCache(-1isize as _,memory,code.len())==0 {
        VirtualFree(memory,0,0x8000);return; // not published, no in-flight branch
    }
    if THUNK.set(memory as usize).is_err() {VirtualFree(memory,0,0x8000);return;}
    // Retain RX storage even after failed publication or detach.
    if !crate::equipment_native::patch(SITE,&ORIGINAL,&jump(SITE,memory as usize)) {
        tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=bridge-publish");return;
    }
    if Release.initialize(std::mem::transmute::<usize,unsafe extern "thiscall" fn(usize)>(0xf05950),released)
        .and_then(|hook|hook.enable()).is_err() {
        let _=Release.disable();
        let _=crate::equipment_native::patch(SITE,&jump(SITE,memory as usize),&ORIGINAL);
        tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=release-scope-hook");return;
    }
    if !crate::overload_visibility::init() {
        let _=Release.disable();
        let _=crate::equipment_native::patch(SITE,&jump(SITE,memory as usize),&ORIGINAL);
        tracing::warn!("SCBL_OVERLOAD_PARTICLE enabled=false reason=root-visibility-hook");return;
    }
    crate::overload_audio::enabled(true);
    READY.store(true,Ordering::Release);
    tracing::info!("SCBL_OVERLOAD_PARTICLE enabled=true viewer=living-spy-goggles-only ordinary-and-cameras=hidden emission-lights=exact runtime_acceptance=pending");
}
pub(super) unsafe fn deinit() {
    crate::overload_visibility::deinit();
    crate::overload_emission_lights::deinit();
    crate::overload_visual_policy::clear();
    crate::overload_audio::enabled(false);
    READY.store(false,Ordering::Release);
    let _=Release.disable();
    if let Some(&address)=THUNK.get() {let _=crate::equipment_native::patch(SITE,&jump(SITE,address),&ORIGINAL);}
    clear();
}
#[cfg(test)]
pub(super) fn fixture_enable(enabled:bool) {READY.store(enabled,Ordering::Release);}
#[cfg(test)]
#[path="overload_particles_tests.rs"]
mod tests;

#[cfg(test)]
pub(super) unsafe fn fixture_capture(ability:usize,particle:usize){captured(ability,particle);}

// Temporary bounded diagnostic. It never supplies classification or changes a
// return value. No addresses, names, account data, or extra native calls are logged.
mod probe {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    const LIMIT:u32=8;
    static RELEASES:AtomicU32=AtomicU32::new(0);
    static FIRST_DRAW:AtomicBool=AtomicBool::new(false);
    static COUNTS:[AtomicU32;12]=[const {AtomicU32::new(0)};12];
    fn active()->bool {let n=RELEASES.load(Ordering::Relaxed);n>0 && n<=LIMIT}
    pub(super) fn begin()->Option<u32> {
        // Saturation also prevents a very long session from reopening the budget.
        let n=RELEASES.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|n|
            (n<=LIMIT).then(||n+1)).unwrap_or(LIMIT+1)+1;
        if n>LIMIT {return None;}
        FIRST_DRAW.store(false,Ordering::Relaxed);Some(n)
    }
    pub(super) fn note(index:usize) {
        if active() {COUNTS[index].fetch_add(1,Ordering::Relaxed);}
    }
    pub(super) fn draw(index:usize,outcome:&str) {
        if !active() {return;}
        note(index);
        if !FIRST_DRAW.swap(true,Ordering::Relaxed) {
            tracing::info!("SCBL_OVERLOAD_PARTICLE_PROBE draw_first=true release_window={} outcome={}",RELEASES.load(Ordering::Relaxed),outcome);
        }
    }
    pub(super) fn report(sequence:u32) {
        let values:Vec<u32>=COUNTS.iter().map(|c|c.load(Ordering::Relaxed)).collect();
        tracing::info!("SCBL_OVERLOAD_PARTICLE_PROBE release={} cumulative_capture_abilityreject_ownermissing_identityreject_worldreject_registered_full_drawhit_rootreject_merchidden_spynative_unknown={:?} draw_totals_may_be_from_prior_releases=true native_calls_unchanged=true",sequence,values);
    }
}
