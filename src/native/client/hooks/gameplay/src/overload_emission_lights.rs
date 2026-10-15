//! Exact overload particle child-light visibility. No new detour or actor-state write.
//! A37180's existing hook calls hidden_node; A37A40 is only the native queue producer.
use crate::equipment_native::{signature, word};
use std::sync::{Mutex, atomic::{AtomicBool, AtomicU32, Ordering}};

const LIGHT_VTABLE: u32 = 0x029e4244;
const MAX_LIGHTS_PER_PARENT: usize = 32;
const MAX_NATIVE_CAPACITY: u32 = 256;
const CACHE_LIMIT: usize = 256;
const UPDATE_SHA: &str = "495e91237f9a3fff9957cb870f0060dd99f2e6ab0a6966f1bbc9904f19888a44";
type Update = unsafe extern "cdecl" fn(usize);
static READY: AtomicBool = AtomicBool::new(false);
static ANY: AtomicBool = AtomicBool::new(false);
static QUEUE_LOGS: AtomicU32 = AtomicU32::new(0);
static EMPTY_LOGS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LightId { parent: usize, parent_key: u32, world: u32, light: usize, light_key: u32, node: usize }
#[derive(Clone, Copy)]
struct Scan { ids: [Option<LightId>; MAX_LIGHTS_PER_PARENT], len: usize }
impl Scan { const fn new() -> Self { Self { ids: [None; MAX_LIGHTS_PER_PARENT], len: 0 } } }
struct Cache { ids: [Option<LightId>; CACHE_LIMIT] }
impl Cache {
    const fn new() -> Self { Self { ids: [None; CACHE_LIMIT] } }
    fn remove_parent(&mut self, parent: usize) {
        for slot in &mut self.ids { if slot.is_some_and(|id| id.parent == parent) { *slot = None; } }
    }
    fn insert(&mut self, id: LightId) -> bool {
        // Ambiguous node ownership removes the old entry and rejects the new.
        if let Some(slot) = self.ids.iter_mut().find(|slot| slot.is_some_and(|old| old.node == id.node && old != id)) {
            *slot = None; return false;
        }
        if let Some(slot) = self.ids.iter_mut().find(|slot| slot.is_none()) { *slot = Some(id); true }
        else { false }
    }
    fn by_node(&self, node: usize) -> Option<LightId> {
        if node == 0 { return None; }
        let mut hits = self.ids.iter().flatten().filter(|id| id.node == node);
        let first = *hits.next()?;
        if hits.next().is_some() { None } else { Some(first) }
    }
}
static LIGHTS: Mutex<Cache> = Mutex::new(Cache::new());

// +420 pointer, +424 count, +428 capacity are established by 9B8B4D..9B8BAA.
fn array<R: Fn(usize, usize) -> Option<u32>>(parent: usize, read: &R) -> Option<(usize, usize)> {
    let data = read(parent, 0x420)? as usize;
    let count = read(parent, 0x424)? as usize;
    let capacity = read(parent, 0x428)?;
    if count > MAX_LIGHTS_PER_PARENT || capacity > MAX_NATIVE_CAPACITY
        || count as u32 > capacity || (count != 0 && data == 0) { return None; }
    Some((data, count))
}
fn valid_light<R: Fn(usize, usize) -> Option<u32>>(
    parent: usize, parent_key: u32, world: u32, light: usize, read: &R,
) -> Option<LightId> {
    if light == 0 || read(light, 0)? != LIGHT_VTABLE
        || read(light, 0x144)? != world || read(light, 0x168)? as usize != parent { return None; }
    let node = read(light, 0x30c)? as usize;
    if node == 0 || read(node, 0)? == 0 || read(node, 0x20)? as usize != light
        || read(parent, 0x30c)? as usize == node { return None; }
    Some(LightId { parent, parent_key, world, light, light_key: read(light, 0x20)?, node })
}
fn scan<R: Fn(usize, usize) -> Option<u32>, C: Fn(usize) -> bool>(
    parent: usize, read: &R, contains: &C,
) -> Option<Scan> {
    if !contains(parent) { return None; }
    let world = read(parent, 0x144)?;
    if world == 0 { return None; }
    let key = read(parent, 0x20)?;
    let (data, count) = array(parent, read)?;
    let mut out = Scan::new();
    for index in 0..count {
        let offset = index.checked_mul(4)?;
        let light = read(data, offset)? as usize;
        if let Some(id) = valid_light(parent, key, world, light, read) {
            if out.ids[..out.len].iter().flatten().any(|old| old.node == id.node) { return None; }
            out.ids[out.len] = Some(id); out.len += 1;
        }
    }
    Some(out)
}
fn current<R: Fn(usize, usize) -> Option<u32>, C: Fn(usize) -> bool>(
    id: LightId, read: &R, contains: &C,
) -> bool {
    if !contains(id.parent) || read(id.parent, 0x20) != Some(id.parent_key)
        || read(id.parent, 0x144) != Some(id.world) { return false; }
    let Some((data, count)) = array(id.parent, read) else { return false; };
    let mut member = false;
    for index in 0..count {
        let Some(offset) = index.checked_mul(4) else { return false; };
        if read(data, offset) == Some(id.light as u32) { member = true; break; }
    }
    member && valid_light(id.parent, id.parent_key, id.world, id.light, read) == Some(id)
}
fn refresh_core<R: Fn(usize, usize) -> Option<u32>, C: Fn(usize) -> bool>(
    cache: &Mutex<Cache>, any: &AtomicBool, parent: usize, read: &R, contains: &C,
    mut queue: impl FnMut(usize),
) -> usize {
    let found = scan(parent, read, contains);
    let mut selected = Scan::new();
    {
        let Ok(mut cache) = cache.lock() else { return 0; };
        cache.remove_parent(parent);
        let Some(found) = found else {
            any.store(cache.ids.iter().any(Option::is_some), Ordering::Release);
            return 0;
        };
        for id in found.ids.into_iter().take(found.len).flatten() {
            if cache.insert(id) { selected.ids[selected.len] = Some(id); selected.len += 1; }
        }
        any.store(cache.ids.iter().any(Option::is_some), Ordering::Release);
    }
    let mut queued = 0;
    for id in selected.ids.into_iter().take(selected.len).flatten() {
        // Recheck after dropping the lock. Native queue itself is never under our mutex.
        if current(id, read, contains) { queue(id.light); queued += 1; }
    }
    queued
}
fn hidden_core<R: Fn(usize, usize) -> Option<u32>, C: Fn(usize) -> bool, H: Fn(u32) -> bool>(
    cache: &Mutex<Cache>, node: usize, read: &R, contains: &C, hide_visuals: &H,
) -> bool {
    let id = { let Ok(cache) = cache.lock() else { return false; }; cache.by_node(node) };
    id.is_some_and(|id| current(id, read, contains) && hide_visuals(id.world))
}

pub(super) unsafe fn init() -> bool {
    let good = signature(0xa37a40, 224, UPDATE_SHA);
    READY.store(good, Ordering::Release);
    tracing::info!("SCBL_OVERLOAD_EMISSION_LIGHTS enabled={} mode=native-light-update runtime_acceptance=pending", good);
    good
}
pub(super) unsafe fn refresh(parent: usize) -> usize {
    if !READY.load(Ordering::Acquire) { return 0; }
    let n = refresh_core(&LIGHTS, &ANY, parent, &|p,o| unsafe { word(p,o) },
        &|p| unsafe { crate::overload_particles::contains(p) },
        |light| {
            unsafe { (std::mem::transmute::<usize, Update>(0xa37a40))(light); }
            if QUEUE_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
                let node = unsafe { word(light, 0x30c) }.unwrap_or(0) as usize;
                let mode315 = unsafe { word(light, 0x315) }.map(|x|x&0xff);
                let flags370 = unsafe { word(light, 0x370) }.map(|x|x&0xff);
                let hidden2c = unsafe { word(node, 0x2c) }.map(|x|x&1);
                let flags34 = unsafe { word(node, 0x34) }.map(|x|x&7);
                let index2ac = unsafe { word(node, 0x2ac) }.map(|x|x!=0);
                tracing::info!("SCBL_OVERLOAD_EMISSION_LIGHTS native_queue_called=true mode315={:?} flags370_low={:?} node2c_hidden={:?} node34_low3={:?} node2ac_nonzero={:?} callback=unverified",mode315,flags370,hidden2c,flags34,index2ac);
            }
        });
    if n == 0 && unsafe { crate::overload_particles::contains(parent) }
        && EMPTY_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
        let raw = unsafe { word(parent,0x424) };
        let count = raw.map(|v|v.min(MAX_NATIVE_CAPACITY));
        let over_bound = raw.is_some_and(|v|v > MAX_NATIVE_CAPACITY);
        tracing::info!("SCBL_OVERLOAD_EMISSION_LIGHTS recognized_children=0 native_count_capped={:?} over_bound={}",count,over_bound);
    }
    n
}
pub(super) unsafe fn hidden_node(node: usize) -> bool {
    READY.load(Ordering::Acquire) && ANY.load(Ordering::Acquire)
        && hidden_core(&LIGHTS, node, &|p,o| unsafe { word(p,o) },
        &|p| unsafe { crate::overload_particles::contains(p) },
        &|w| unsafe { crate::overload_particles::hide_visuals(w as usize) })
}
pub(super) fn forget(parent: usize) {
    if let Ok(mut cache) = LIGHTS.lock() {
        cache.remove_parent(parent);
        ANY.store(cache.ids.iter().any(Option::is_some), Ordering::Release);
    }
}
pub(super) fn clear() {
    if let Ok(mut cache) = LIGHTS.lock() { *cache = Cache::new(); ANY.store(false, Ordering::Release); }
}
pub(super) fn deinit() { READY.store(false, Ordering::Release); clear(); }
#[cfg(test)]
pub(super) fn fixture_enable(enabled: bool) { READY.store(enabled, Ordering::Release); }
#[cfg(test)]
pub(super) unsafe fn fixture_refresh(parent: usize, queue: impl FnMut(usize)) -> usize {
    if !READY.load(Ordering::Acquire) { return 0; }
    refresh_core(&LIGHTS, &ANY, parent, &|p,o| unsafe { word(p,o) },
        &|p| unsafe { crate::overload_particles::contains(p) },
        queue)
}
#[cfg(test)]
#[path = "overload_emission_lights_tests.rs"]
mod tests;
