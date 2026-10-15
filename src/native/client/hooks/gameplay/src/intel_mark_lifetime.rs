//! R50: retire only marks actually added by the authoritative Intel Suit scan.
//! DX11 EC2C20 supplies pawn+2C0's 64-bit key and C94FC0's controller to
//! B93060/B5EA00, then sends the same operation via 102DE00. Other gadgets and
//! weapon tags can use the SAME key: never infer provenance from equipment.
//! B84790 runs on the existing player mark tick. We remove one source through
//! B84A50 and send the native active=false event; never clear all marks/HUD bits.
use crate::equipment_native::{readable, signature, word};
use retour::static_detour;
use std::cell::Cell;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};

const SPY: u32 = 0x02ad3234;
const MERC: u32 = 0x02ad2abc;
const MAX_RECORDS: usize = 1024;
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static RECORDS: Mutex<Vec<Record>> = Mutex::new(Vec::new());
#[cfg(test)]
static FIXTURE_RELOCATION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn native_address(address: usize) -> usize {
    #[cfg(test)]
    {
        return address.wrapping_add(FIXTURE_RELOCATION.load(Ordering::Relaxed));
    }
    #[cfg(not(test))]
    {
        address
    }
}
thread_local! { static CAST: Cell<Option<Source>> = const { Cell::new(None) }; }
type Remove = unsafe extern "thiscall" fn(usize, u32, u32, usize);
type Send = unsafe extern "thiscall" fn(usize, usize, usize, u8, f32, u8, f32);
static_detour! {
    static Scan: unsafe extern "thiscall" fn(usize);
    static Tick: unsafe extern "thiscall" fn(usize, f32);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    address: usize,
    low: u32,
    high: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Source {
    pawn: Identity,
    controller: Identity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record {
    target: Identity,
    source: Source,
}

unsafe fn identity(address: usize) -> Option<Identity> {
    let peer = word(address, 0x2c0)? as usize;
    let (low, high) = (word(peer, 0x70)?, word(peer, 0x74)?);
    ((low | high) != 0).then_some(Identity { address, low, high })
}
unsafe fn current(id: Identity) -> bool {
    identity(id.address) == Some(id)
}
unsafe fn authority(pawn: usize) -> bool {
    // Exact 55FBC0 predicate; only the peer that produced the native scan sends.
    word(pawn, 0x5c).is_some_and(|f| f & 0x800000 != 0)
        && word(pawn, 0x4c).is_some_and(|f| (f >> 8) & 255 != 2)
}
unsafe fn dead(pawn: usize) -> Option<bool> {
    // AEPlayerSpy vtable+330 = B8B300, also used by the native EMP dead filter.
    (word(pawn, 0)? == SPY).then_some(word(pawn, 0x60)? & 0x800 != 0)
}
unsafe fn source(ability: usize) -> Option<Source> {
    let p = word(ability, 0x6c)? as usize;
    if dead(p) != Some(false) || !authority(p) {
        return None;
    }
    let controller = word(p, 0x358)? as usize;
    // Exact C94FC0 check. Do not mistake the presentation controller for this.
    if word(controller, 0x60)? & 0x40000 == 0 {
        return None;
    }
    Some(Source {
        pawn: identity(p)?,
        controller: identity(controller)?,
    })
}
unsafe fn mark_present(target: usize, s: Source) -> Option<bool> {
    if word(target, 0)? != MERC {
        return None;
    }
    let (p, n, cap) = (
        word(target, 0x1a90)? as usize,
        word(target, 0x1a94)? as usize,
        word(target, 0x1a98)? as usize,
    );
    if n > 256 || cap < n || cap > 4096 {
        return None;
    }
    if n == 0 {
        return Some(false);
    }
    if !readable(p, n * 16) {
        return None;
    }
    Some((0..n).any(|i| {
        word(p, i * 16 + 4) == Some(s.pawn.low)
            && word(p, i * 16 + 8) == Some(s.pawn.high)
            && word(p, i * 16 + 12) == Some(s.controller.address as u32)
    }))
}

struct CastScope(Option<Source>);
impl CastScope {
    fn enter(source: Option<Source>) -> Self {
        Self(CAST.with(|v| v.replace(source)))
    }
}
impl Drop for CastScope {
    fn drop(&mut self) {
        CAST.with(|v| v.set(self.0));
    }
}
fn scan(ability: usize) {
    let s = if READY.load(Ordering::Acquire) {
        unsafe { source(ability) }
    } else {
        None
    };
    let _scope = CastScope::enter(s);
    unsafe {
        Scan.call(ability);
        drop(_scope);
        if let Some(s) = s {
            crate::rfd_exposure::targets_changed(s.pawn.address);
        }
    }
}

// Called ONLY after an accepted native add, not after the existing ignored
// duplicate branch. A later unrelated add with this key relinquishes ownership
// even if it merely extends the timer; do not erase that different effect.
unsafe fn record_add(
    records: &mut Vec<Record>,
    cast: Option<Source>,
    target: usize,
    low: u32,
    high: u32,
    tagger: usize,
) {
    records.retain(|r| {
        !(r.target.address == target && r.source.pawn.low == low && r.source.pawn.high == high)
    });
    let Some(s) = cast else {
        return;
    };
    if (s.pawn.low, s.pawn.high, s.controller.address) != (low, high, tagger)
        || mark_present(target, s) != Some(true)
    {
        return;
    }
    let Some(target) = identity(target) else {
        return;
    };
    if records.len() >= MAX_RECORDS {
        // Bounded metadata only; dropping observation never deletes game state.
        records.remove(0);
    }
    records.push(Record { target, source: s });
}
pub(super) unsafe fn mark_added(target: usize, low: u32, high: u32, tagger: usize) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let cast = CAST.with(Cell::get);
    if let Ok(mut records) = RECORDS.lock() {
        record_add(&mut records, cast, target, low, high, tagger);
    }
}

// Query only observed, accepted Intel scans. Equipped armor and the generic
// marked bit are NOT provenance. Native non-positive mark timers are persistent
// entries (B84790); expiry is represented by removal of the exact native row.
unsafe fn has_targets(records: &[Record], pawn: usize) -> bool {
    records.iter().any(|r| {
        r.source.pawn.address == pawn
            && dead(pawn) == Some(false)
            && current(r.source.pawn)
            && current(r.source.controller)
            && current(r.target)
            && word(r.target.address, 0x60).is_some_and(|f| f & 0x800 == 0)
            && mark_present(r.target.address, r.source) == Some(true)
    })
}
pub(crate) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}
pub(crate) unsafe fn actual_targets(pawn: usize) -> Option<bool> {
    if !ready() || word(pawn, 0) != Some(SPY) || !authority(pawn) {
        return None;
    }
    RECORDS
        .lock()
        .ok()
        .map(|records| has_targets(&records, pawn))
}

unsafe fn take_dead(records: &mut Vec<Record>, target: usize) -> Vec<Record> {
    let mut out = Vec::new();
    records.retain(|r| {
        // Every pointer is readable AND its native 64-bit identity must match.
        // No timeout, equipment guess, world roster mutation or saved pawn reuse.
        if !current(r.target)
            || !current(r.source.pawn)
            || mark_present(r.target.address, r.source) == Some(false)
        {
            return false;
        }
        if r.target.address != target
            || dead(r.source.pawn.address) != Some(true)
            || !authority(r.source.pawn.address)
        {
            return true;
        }
        if mark_present(target, r.source) != Some(true) || !current(r.source.controller) {
            return true;
        }
        let peer = word(r.source.controller.address, 0x2c0).unwrap_or(0) as usize;
        // Native 102DE00 sends only while +78 is active. Wait rather than claim
        // a local-only clear is synchronized. Natural expiry remains untouched.
        if !word(peer, 0x78).is_some_and(|f| f & 255 != 0) {
            return true;
        }
        out.push(*r);
        false
    });
    out
}
unsafe fn cancel(record: Record, remove: Remove, send: Send) {
    let s = record.source;
    remove(
        record.target.address,
        s.pawn.low,
        s.pawn.high,
        s.controller.address,
    );
    let peer = word(s.controller.address, 0x2c0).unwrap_or(0) as usize;
    send(peer, s.pawn.address, record.target.address, 0, 0.0, 0, 0.0);
}
fn tick(target: usize, dt: f32) {
    // Keep source identities before native Tick removes the final mark row.
    // Clothing's active tick is not the lifetime clock for target marks.
    let sources: Vec<_> = if crate::intel_rfd_targets::ready() {
        RECORDS
            .lock()
            .ok()
            .map(|rs| {
                rs.iter()
                    .filter(|r| r.target.address == target)
                    .map(|r| r.source.pawn)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if READY.load(Ordering::Acquire) {
        let pending = RECORDS
            .lock()
            .ok()
            .map(|mut records| unsafe { take_dead(&mut records, target) })
            .unwrap_or_default();
        // Release the metadata lock before native calls and network callbacks.
        for r in pending {
            unsafe {
                cancel(
                    r,
                    std::mem::transmute::<usize, Remove>(native_address(0xb84a50)),
                    std::mem::transmute::<usize, Send>(native_address(0x102de00)),
                );
            }
            if LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
                tracing::info!("SCBL_MOD_INTEL_LIFETIME revision=50 caster_dead=true source_cancel_sent=true other_sources_preserved=true");
            }
        }
    }
    unsafe {
        Tick.call(target, dt);
        for source in sources {
            if current(source) {
                crate::rfd_exposure::targets_changed(source.address);
            }
        }
    }
}

pub(super) fn init(world: usize) {
    if world != 0x00589860 || !crate::drone_tags::source_cancel_ready() {
        return;
    }
    unsafe {
        if !signature(
            0xec2c20,
            1049,
            "65b3bfc3f2bacbc61430e714d92c9b960fb03681008b47885650b26d1a5a9211",
        ) || !signature(
            0xb84790,
            382,
            "821c82c73043b1924e89a121b79aa0b4c99f20f84fcb53758536707af8b7e1ec",
        ) || !signature(
            0xb84a50,
            138,
            "47c029dc24bf4aa6b80f6effe4799fef33fc04519a407a48b42a458d5f49af6a",
        ) || !signature(
            0x102de00,
            231,
            "3a6d004594ca99c3cbe9fd0b081e9611f38aa1c691d1b81df6c6aa0382f56b9d",
        ) || !signature(
            0xb8b300,
            10,
            "2c20847dd16be552bfa3d838263dd37a9376bf1cefad573855261e56cdd490b3",
        ) || !signature(
            0xc94fc0,
            22,
            "ae73613dd4600d74aef0b211701d1cc84231e8553d7b738db07d27670db48ee3",
        ) {
            tracing::error!("SCBL_MOD_INTEL_LIFETIME enabled=false reason=native-signature");
            return;
        }
        if Scan
            .initialize(
                std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(0xec2c20),
                scan,
            )
            .is_err()
            || Tick
                .initialize(
                    std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(0xb84790),
                    tick,
                )
                .is_err()
            || Tick.enable().is_err()
            || Scan.enable().is_err()
        {
            let _ = Scan.disable();
            let _ = Tick.disable();
            tracing::error!("SCBL_MOD_INTEL_LIFETIME enabled=false reason=required-hook");
            return;
        }
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_INTEL_LIFETIME revision=50 enabled=true exact_cast_provenance=true");
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Scan.disable();
    let _ = Tick.disable();
    if let Ok(mut records) = RECORDS.lock() {
        records.clear();
    }
}

#[cfg(test)]
mod tests;

// Disposable x86 transaction fixture only. Runs the real Tick detour against
// extracted retail mark tick/removal bytes; its RPC endpoint is a recording stub.
#[cfg(test)]
#[allow(dead_code)]
pub(super) unsafe fn fixture_install_tick(ability: usize, target: usize, relocation: usize) {
    FIXTURE_RELOCATION.store(relocation, Ordering::Relaxed);
    let s = source(ability).expect("verified fixture source");
    let mut records = RECORDS.lock().unwrap();
    records.clear();
    record_add(
        &mut records,
        Some(s),
        target,
        s.pawn.low,
        s.pawn.high,
        s.controller.address,
    );
    assert_eq!(records.len(), 1);
    drop(records);
    Tick.initialize(
        std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, f32)>(native_address(
            0xb84790,
        )),
        tick,
    )
    .unwrap();
    Tick.enable().unwrap();
    READY.store(true, Ordering::Release);
}
#[cfg(test)]
#[allow(dead_code)]
pub(super) unsafe fn fixture_remove_tick() {
    READY.store(false, Ordering::Release);
    Tick.disable().unwrap();
    FIXTURE_RELOCATION.store(0, Ordering::Relaxed);
    assert!(RECORDS.lock().unwrap().is_empty());
}
