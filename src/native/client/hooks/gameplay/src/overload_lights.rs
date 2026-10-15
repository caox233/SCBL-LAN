//! Ten seconds of blackout, followed by each lamp's native recovery.
//! EF1850 identifies Saboteur enqueue scope; EE3010 appends unmodified rows;
//! ECC050 authorizes only the row actually being consumed. C83C10 receives a
//! temporary phase=10 and total=10+native recovery, then configuration restores.
//! No ability +A8 changes, native queue mutations, frame timer, or world sweep.
use crate::equipment_native::{signature, word, writable};
use retour::static_detour;
use std::{cell::Cell, sync::{Mutex, atomic::{AtomicBool, Ordering}}};

const SABOTEUR: u32 = 0x02a3f3a4;
const SPY: u32 = 0x02ad3234;
const LIGHT_EMP: u32 = 0x00c83c10;
const SECONDS: f32 = 10.0;
const LIMIT: usize = 128;
static READY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Source { address: usize, object: u32, world: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Manager { address: usize, class: u32, object: u32, world: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row { target: usize, owner: usize, duration: u32, flag: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Event { manager: Manager, index: usize, row: Row, serial: u32, owner: Source }
#[derive(Clone, Copy)]
struct QueueState { manager: Manager, rows: usize, count: usize }
thread_local! {
    static SOURCE: Cell<Option<Source>> = const { Cell::new(None) };
    static ACTIVE: Cell<Option<Event>> = const { Cell::new(None) };
}
static PENDING: Mutex<[Option<Event>; LIMIT]> = Mutex::new([None; LIMIT]);

unsafe fn source(ability: usize) -> Option<Source> {
    if word(ability, 0)? != SABOTEUR { return None; }
    let p = word(ability, 0x6c)? as usize;
    if word(p, 0)? != SPY || word(p, 0x374)? & (3 << 18) != 1 << 18 { return None; }
    Some(Source { address: p, object: word(p, 0x20)?, world: word(p, 0x144)? })
}
fn with_cell<T: Copy, R>(slot: &Cell<Option<T>>, value: Option<T>, call: impl FnOnce() -> R) -> R {
    struct Restore<'a, T: Copy>(&'a Cell<Option<T>>, Option<T>);
    impl<T: Copy> Drop for Restore<'_, T> { fn drop(&mut self) { self.0.set(self.1); } }
    let _restore = Restore(slot, slot.replace(value));
    call()
}
fn with_source<R>(value: Option<Source>, call: impl FnOnce() -> R) -> R {
    SOURCE.with(|slot| with_cell(slot, value, call))
}
fn with_active<R>(value: Option<Event>, call: impl FnOnce() -> R) -> R {
    ACTIVE.with(|slot| with_cell(slot, value, call))
}
unsafe fn eligible(target: usize, owner: usize, native: f32) -> bool {
    let Some(scope) = SOURCE.with(Cell::get) else { return false; };
    native.is_finite() && native > 0.0 && owner == scope.address && scope.world != 0
        && word(owner, 0) == Some(SPY)
        && word(owner, 0x20) == Some(scope.object)
        && word(owner, 0x144) == Some(scope.world)
        && word(target, 0x144) == Some(scope.world)
        && word(target, 0).and_then(|v| word(v as usize, 0x6f0)) == Some(LIGHT_EMP)
}
unsafe fn snapshot(p: usize) -> Option<QueueState> {
    let manager = Manager { address: p, class: word(p, 0)?, object: word(p, 0x20)?, world: word(p, 0x144)? };
    let rows = word(p, 0x7d8)? as usize;
    let count = word(p, 0x7dc)? as usize;
    let capacity = word(p, 0x7e0)? as usize;
    // Malformed/unbounded arrays only lose authorization; native still runs.
    if manager.class == 0 || manager.world == 0 || count > capacity || capacity > 1 << 20
        || (count != 0 && rows == 0) { return None; }
    Some(QueueState { manager, rows, count })
}
unsafe fn row(q: QueueState, index: usize) -> Option<Row> {
    if index >= q.count { return None; }
    let p = q.rows.checked_add(index.checked_mul(16)?)?;
    Some(Row { target: word(p, 0)? as usize, owner: word(p, 4)? as usize,
        duration: word(p, 8)?, flag: word(p, 12)? & 1 })
}
fn clear_pending() { if let Ok(mut p) = PENDING.lock() { *p = [None; LIMIT]; } }
fn clear_manager(manager: usize) {
    if let Ok(mut p) = PENDING.lock() {
        for entry in p.iter_mut() {
            if entry.is_some_and(|e| e.manager.address == manager) { *entry = None; }
        }
    }
}
struct ClearOnUnwind { manager: usize, armed: bool }
impl Drop for ClearOnUnwind {
    fn drop(&mut self) { if self.armed { clear_manager(self.manager); } }
}
unsafe fn valid_event(e: Event) -> bool {
    e.manager.world == e.owner.world && word(e.row.target, 0x20) == Some(e.serial)
        && with_source(Some(e.owner), || eligible(e.row.target, e.row.owner, f32::from_bits(e.row.duration)))
}
unsafe fn with_queue<R>(manager: usize, target: usize, owner: usize, native: f32, flags: u32, call: impl FnOnce() -> R) -> R {
    let before = snapshot(manager);
    if before.is_none_or(|q| q.count == 0) { clear_manager(manager); }
    let candidate = before.and_then(|q| {
        if !eligible(target, owner, native) { return None; }
        Some(Event { manager: q.manager, index: q.count,
            row: Row { target, owner, duration: native.to_bits(), flag: flags & 1 },
            serial: word(target, 0x20)?, owner: SOURCE.with(Cell::get)? })
    });
    let mut guard = ClearOnUnwind { manager, armed: true };
    let result = call(); // Exactly once; all native queue arguments unchanged.
    let after = snapshot(manager);
    let append = before.zip(after).filter(|(a, b)| a.manager == b.manager && a.count.checked_add(1) == Some(b.count));
    if let Some((_, q)) = append {
        if let Ok(mut p) = PENDING.lock() {
            for entry in p.iter_mut() {
                if entry.is_some_and(|e| e.manager.address == manager
                    && (e.manager != q.manager || row(q, e.index) != Some(e.row))) { *entry = None; }
            }
            if let Some(e) = candidate.filter(|e| row(q, e.index) == Some(e.row) && valid_event(*e)) {
                // Do not evict another live authorization on overflow.
                if let Some(slot) = p.iter_mut().find(|e| e.is_none()) { *slot = Some(e); }
            }
        }
    } else { clear_manager(manager); }
    guard.armed = false;
    result
}
unsafe fn with_consume<R>(manager: usize, call: impl FnOnce() -> R) -> R {
    let before = snapshot(manager);
    // Same-manager reentry cannot inherit an outer authorization.
    if ACTIVE.with(|slot| slot.get().is_some_and(|e| e.manager.address == manager)) {
        ACTIVE.with(|slot| slot.set(None));
        clear_manager(manager);
    }
    let event = if let Some(q) = before.filter(|q| q.count != 0) {
        let head = row(q, 0);
        PENDING.lock().ok().and_then(|mut p| {
            let mut found = None;
            for slot in p.iter_mut() {
                if let Some(e) = *slot {
                    if e.manager.address != manager { continue; }
                    if e.manager != q.manager || e.index >= q.count { *slot = None; }
                    else if e.index == 0 {
                        *slot = None;
                        if head == Some(e.row) && valid_event(e) { found = Some(e); }
                    }
                }
            }
            found
        })
    } else { clear_manager(manager); None };
    let mut guard = ClearOnUnwind { manager, armed: true };
    let result = with_active(event, call); // ECC050 consumes exactly one row.
    if let Some((_, b)) = before.zip(snapshot(manager)).filter(|(a, b)| a.count > 0 && a.manager == b.manager) {
        if let Ok(mut p) = PENDING.lock() {
            for slot in p.iter_mut() {
                if let Some(mut e) = *slot {
                    if e.manager.address != manager { continue; }
                    // Native shifts even rows synchronously appended in call().
                    if e.manager != b.manager || e.index == 0 { *slot = None; continue; }
                    e.index -= 1;
                    *slot = (row(b, e.index) == Some(e.row)).then_some(e);
                }
            }
        }
    } else { clear_manager(manager); }
    guard.armed = false;
    result
}
unsafe fn take_active(target: usize, owner: usize, native: f32, flags: u32) -> Option<Event> {
    ACTIVE.with(|slot| {
        let e = slot.get()?;
        if e.row.target != target || e.row.duration != native.to_bits() || e.row.flag != flags
            || !valid_event(e) { return None; }
        // EAE7E0 maps original owner to null unless this native flag is set.
        let mapped = if word(e.owner.address, 0x5c)? & 0x200000 != 0 { e.owner.address } else { 0 };
        if owner != mapped { return None; }
        slot.set(None); // Single-use even if native reenters the handler.
        Some(e)
    })
}
unsafe fn with_emp<R>(target: usize, owner: usize, native: f32, flags: u32, call: impl FnOnce(f32) -> R) -> R {
    let Some(event) = take_active(target, owner, native, flags) else { return call(native); };
    let offset = if flags != 0 { 0x744 } else { 0x740 };
    let Some(bits) = word(target, offset) else { return call(native); };
    let phase = f32::from_bits(bits);
    if !phase.is_finite() || phase < 0.0 || !writable(target + offset, 4) { return call(native); }
    let total = SECONDS + (native - phase).max(0.0);
    if !total.is_finite() { return call(native); }
    struct Restore { event: Event, offset: usize, bits: u32 }
    impl Drop for Restore { fn drop(&mut self) { unsafe {
        let p = self.event.row.target;
        if word(p, 0x20) == Some(self.event.serial) && word(p, 0x144) == Some(self.event.owner.world)
            && word(p, self.offset) == Some(SECONDS.to_bits()) && writable(p + self.offset, 4)
            && word(p, 0).and_then(|v| word(v as usize, 0x6f0)) == Some(LIGHT_EMP) {
            ((p + self.offset) as *mut u32).write_unaligned(self.bits);
        }
    } } }
    ((target + offset) as *mut u32).write_unaligned(SECONDS.to_bits());
    let _restore = Restore { event, offset, bits };
    call(total)
}
static_detour! {
    static Light: unsafe extern "thiscall" fn(usize,usize,f32,u32,u32,u32);
    static Consumer: unsafe extern "thiscall" fn(usize,f32);
    static Devices: unsafe extern "thiscall" fn(usize);
    static Queue: unsafe extern "thiscall" fn(usize,usize,usize,f32,u32);
}
fn devices(ability: usize) {
    let scope = READY.load(Ordering::Acquire).then(|| unsafe { source(ability) }).flatten();
    with_source(scope, || unsafe { Devices.call(ability) });
}
fn queue(manager: usize, target: usize, owner: usize, native: f32, flags: u32) { unsafe {
    if READY.load(Ordering::Acquire) {
        with_queue(manager, target, owner, native, flags, || Queue.call(manager, target, owner, native, flags));
    } else { Queue.call(manager, target, owner, native, flags); }
} }
fn consumer(manager: usize, dt: f32) { unsafe {
    if READY.load(Ordering::Acquire) { with_consume(manager, || Consumer.call(manager, dt)); }
    else { with_active(None, || Consumer.call(manager, dt)); }
} }
fn light(target: usize, owner: usize, native: f32, a: u32, flags: u32, b: u32) { unsafe {
    if READY.load(Ordering::Acquire) {
        with_emp(target, owner, native, flags, |total| Light.call(target, owner, total, a, flags, b));
    } else { Light.call(target, owner, native, a, flags, b); }
} }
pub(super) unsafe fn init(world: usize) {
    if READY.load(Ordering::Acquire) || world != 0x589860 { return; }
    if !signature(0xef1850, 417, "61922581e52a72498b3a23e41348f0066fd3e19acfdf9dfe502ea4514a3cf0c8")
        || !signature(0xee3010, 79, "45974c4851932ba49d54672d6fe4153695184b3b89583ea0815809872c7e18ff")
        || !signature(0xecc050, 145, "cd2d093b16e6f838a90cbf66b5899cf4ef304c09a2711703aa9daea068ee089d")
        || !signature(LIGHT_EMP as usize, 360, "a49fa60ac7b935c2528af0ae3d24c064a7acecbd5c9248753003f35b754fff4f") {
        tracing::warn!("SCBL_OVERLOAD_LIGHT enabled=false reason=native-signature");
        return;
    }
    if Light.initialize(std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize,usize,f32,u32,u32,u32)>(LIGHT_EMP as usize), light)
        .and_then(|hook| hook.enable()).is_err()
        || Consumer.initialize(std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize,f32)>(0xecc050), consumer)
            .and_then(|hook| hook.enable()).is_err()
        || Queue.initialize(std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize,usize,usize,f32,u32)>(0xee3010), queue)
            .and_then(|hook| hook.enable()).is_err()
        || Devices.initialize(std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0xef1850), devices)
            .and_then(|hook| hook.enable()).is_err() {
        let _ = Devices.disable(); let _ = Queue.disable();
        let _ = Consumer.disable(); let _ = Light.disable();
        clear_pending();
        tracing::warn!("SCBL_OVERLOAD_LIGHT enabled=false reason=hook-group");
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_OVERLOAD_LIGHT enabled=true blackout_seconds={} recovery=native scope=saboteur-native-queue-position other_devices=unchanged", SECONDS);
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Devices.disable(); let _ = Queue.disable();
    let _ = Consumer.disable(); let _ = Light.disable();
    clear_pending();
}
#[cfg(test)]
pub(super) unsafe fn fixture_duration(ability: usize, target: usize, owner: usize, native: f32) -> f32 {
    with_source(source(ability), || if eligible(target, owner, native) { SECONDS } else { native })
}
// Owned mock native array, but production append/consume correlation and handler.
#[cfg(test)]
pub(super) unsafe fn fixture_event<R>(ability: usize, target: usize, owner: usize, native: f32, flags: u32, call: impl FnOnce(f32) -> R) -> R {
    let mut manager = vec![0u32; 0x7e4 / 4];
    let mut rows = vec![0u32; 4];
    manager[0] = 1; manager[0x20 / 4] = 1;
    manager[0x144 / 4] = word(target, 0x144).unwrap_or(0);
    manager[0x7d8 / 4] = rows.as_mut_ptr() as u32; manager[0x7e0 / 4] = 1;
    let m = manager.as_mut_ptr() as usize;
    with_source(source(ability), || with_queue(m, target, owner, native, flags, || {
        rows.copy_from_slice(&[target as u32, owner as u32, native.to_bits(), flags & 1]);
        manager[0x7dc / 4] = 1;
    }));
    with_consume(m, || {
        let mapped = if word(owner, 0x5c).is_some_and(|f| f & 0x200000 != 0) { owner } else { 0 };
        let result = with_emp(target, mapped, native, flags & 1, call);
        manager[0x7dc / 4] = 0;
        result
    })
}
#[cfg(test)]
#[path = "overload_lights_tests.rs"]
mod tests;
