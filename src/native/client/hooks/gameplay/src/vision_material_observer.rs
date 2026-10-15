//! Opt-in, F8-started observation of the existing material return. No rendering writes.
use std::cell::Cell;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
const HEADER_LIMIT: u32 = 4096;
const PRELOCK_FAILURES: u8 = 16;
const READ_LIMIT: u32 = 65_536;
const SAMPLE_LOG_LIMIT: u32 = 128;
const ROUND_LIMIT: u8 = 3;
const WINDOW_MS: u64 = 30_000;
const SAMPLE_MS: u64 = 100;
const LOG_MS: u64 = 500;
const SLOTS: usize = 8;
#[inline]
pub(super) fn build_enabled() -> bool {
    option_env!("SCBL_THERMAL_MATERIAL_OBSERVER_BUILD") == Some("1")
}
static ENABLED: AtomicBool = AtomicBool::new(false);
static STOPPED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State::new());
thread_local! { static VIEW: Cell<Option<(usize, u64, u8)>> = const { Cell::new(None) }; static VERIFIED: Cell<Option<VerifiedToken>> = const { Cell::new(None) }; }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VerifiedToken {
    pub round: u8, pub elapsed_ms: u64, pub render: usize, pub root: usize,
    pub object: usize, pub surface: usize, pub selected: usize,
    pub actor: u32, pub serial: u32, pub world: u32, pub level: u32,
    pub category: u8, pub strength: u8, pub stage: u32, pub key: u32,
}
pub(super) fn clear_verified() { VERIFIED.with(|v| v.set(None)); }
pub(super) fn take_verified() -> Option<VerifiedToken> { VERIFIED.with(|v| v.replace(None)) }
pub(super) fn capturing() -> bool { enabled() && VIEW.with(Cell::get).is_some() }
pub(super) fn capturing_round() -> Option<u8> { if enabled() { VIEW.with(Cell::get).map(|(_, _, round)| round) } else { None } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity { actor: u32, serial: u32, world: u32, level: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record {
    id: Identity, proxy: u32, actor_flags: u32, flags_lo: u32, flags_hi: u32,
    heat: u32, category: u8, strength: u8, stage: u32,
    root: u32, pass: u32, table: u32, shader: u32, view: u32,
    surface: usize, selected: usize, head: u32, key: u32, material_flags: u32,
}
#[derive(Clone, Copy)]
struct Sample { record: Record, round: u8, elapsed_ms: u64 }
struct State {
    waiting: bool, wait_release: bool, pressed: bool, running: bool, started: Option<u64>, round: u8,
    owner: Option<Identity>, owner_proxy: usize, headers: u32, attempts: [u32; 8],
    prelock_failures: u8, owner_deep_left: u8,
    last_window: Option<u64>, seen: usize, offset: usize, regular_left: usize,
    owner_seen: usize, owner_offset: usize, owner_first: bool, owner_rotating: bool,
    last_emitted: [Option<Record>; 2], log_epoch: Option<u64>, logged_stage: [bool; 2],
    deferred_changes: u32,
    last: [Option<Record>; 2], records: [Option<Record>; 8], history_next: usize,
    record_times: [u64; 8], final_emitted: bool,
    pending: [Option<Sample>; 8], observed_changes: u32, samples_queued: u32, truncated: u32,
    rejected: u32, reads: u32,
}
impl State {
    const fn new() -> Self { Self { waiting: false, wait_release: false, pressed: false, running: false,
        started: None, round: 0, owner: None, owner_proxy: 0, headers: 0, attempts: [0; 8],
        prelock_failures: 0, owner_deep_left: 0,
        last_window: None, seen: 0, offset: 0, regular_left: 0,
        owner_seen: 0, owner_offset: 0, owner_first: false, owner_rotating: false,
        last_emitted: [None; 2], log_epoch: None, logged_stage: [false; 2], deferred_changes: 0,
        last: [None; 2], records: [None; 8], history_next: 0,
        record_times: [0; 8], final_emitted: false, pending: [None; 8],
        observed_changes: 0, samples_queued: 0, truncated: 0, rejected: 0, reads: 0 } }
}
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn ReadProcessMemory(process: *mut core::ffi::c_void, base: *const core::ffi::c_void,
        buffer: *mut core::ffi::c_void, size: usize, read: *mut usize) -> i32;
    #[cfg(not(test))]
    fn GetTickCount64() -> u64;
}
#[cfg(not(test))]
#[link(name = "user32")]
extern "system" { fn GetAsyncKeyState(key: i32) -> i16; }
#[cfg(test)]
static TEST_KEY: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_READS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(test)]
static TEST_KEYS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn key_down() -> bool {
    #[cfg(test)] { TEST_KEYS.fetch_add(1, Ordering::Relaxed); TEST_KEY.load(Ordering::Relaxed) }
    #[cfg(not(test))] { unsafe { (GetAsyncKeyState(0x77) as u16 & 0x8000) != 0 } } // F8 only
}
#[cfg(test)]
static TEST_CLOCKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(test)]
static TEST_NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000_000);
fn clock() -> u64 {
    #[cfg(test)] { TEST_CLOCKS.fetch_add(1, Ordering::Relaxed); TEST_NOW.load(Ordering::Relaxed) }
    #[cfg(not(test))] { unsafe { GetTickCount64() } }
}
struct Reader { reads: u32, limit: u32 }
impl Reader {
    fn word(&mut self, base: usize, offset: usize) -> Option<u32> {
        if !ENABLED.load(Ordering::Acquire) || self.reads >= self.limit { return None; } // hard read cap/deinit
        let p = base.checked_add(offset)?;
        if base < 0x10000 || p.checked_add(4).is_none() { return None; }
        self.reads += 1;
        #[cfg(test)] TEST_READS.fetch_add(1, Ordering::Relaxed);
        let (mut value, mut read) = (0u32, 0usize);
        let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), p as *const _,
            &mut value as *mut _ as *mut _, 4, &mut read) };
        (ok != 0 && read == 4).then_some(value)
    }
}
fn lane(stage: u32, selected: usize) -> Option<usize> {
    let s = match stage { 2 => 0, 4 => 1, _ => return None };
    Some(s * 2 + usize::from(selected != 0))
}
// Category is observed data, never the Merc identity predicate. Ignore transient
// surface/head/pass bookkeeping as a reason to emit another semantic sample.
fn same_signal(a: Record, b: Record) -> bool {
    a.id == b.id && a.proxy == b.proxy && a.actor_flags == b.actor_flags
        && a.flags_lo == b.flags_lo && a.flags_hi == b.flags_hi && a.heat == b.heat
        && a.stage == b.stage && a.selected == b.selected && a.key == b.key
        && a.material_flags == b.material_flags
}
fn snapshot(r: &mut Reader, pass: usize, root: usize, render: usize,
    object: usize, surface: usize, selected: usize) -> Option<Record> {
    let flags_lo = r.word(object, 0x34)?;
    let flags_hi = r.word(object, 0x38)?;
    if flags_lo & 0x400 == 0 && flags_hi & (0x10000000 | 0xc0000) == 0 { return None; }
    let heat = r.word(object, 0x210)?;
    let category = (heat >> 24) as u8;
    let stage = r.word(render, 0x233c)?;
    lane(stage, selected)?;
    if r.word(render, 0x236c)? as usize != pass || r.word(pass, 0x14)? as usize != render
        || r.word(pass, 0)? as usize != root || r.word(root, 0)? != 0x02c06404 { return None; }
    let view = r.word(render, 0x20d0)?;
    if r.word(view as usize, 0x340)? != 8 { return None; }
    let table = r.word(pass, 8)?;
    let shader = r.word(table as usize, 8 * 4)?;
    if r.word(shader as usize, 0)? != 0x02c094a4 || r.word(shader as usize, 4)? as usize != root { return None; }
    let actor = r.word(object, 0x20)?;
    if r.word(actor as usize, 0)? != 0x02ad2abc || r.word(actor as usize, 0x30c)? as usize != object { return None; }
    let id = Identity { actor, serial: r.word(actor as usize, 0x20)?,
        world: r.word(actor as usize, 0x144)?, level: r.word(actor as usize, 0x148)? };
    if id.world == 0 || id.level == 0 { return None; }
    let actor_flags = r.word(actor as usize, 0x34)?;
    let head = r.word(root, 0x160)?;
    let (key, material_flags) = if selected == 0 { (0, 0) } else {
        if r.word(selected, 0)? != 0x02c06404 { return None; }
        (r.word(selected, 0x14)?, r.word(selected, 0x18c)?)
    };
    Some(Record { id, proxy: object as u32, actor_flags, flags_lo, flags_hi,
        heat, category, strength: (heat >> 8) as u8, stage, root: root as u32,
        pass: pass as u32, table, shader, view, surface, selected, head, key, material_flags })
}
#[inline]
pub(super) fn enabled() -> bool { build_enabled() && ENABLED.load(Ordering::Acquire) }
pub(super) fn init() {
    if !build_enabled() || STOPPED.load(Ordering::Acquire)
        || std::env::var("SCBL_THERMAL_MATERIAL_OBSERVER").as_deref() != Ok("1") { return; }
    let Ok(mut s) = STATE.try_lock() else { return; };
    if ENABLED.load(Ordering::Acquire) || s.round != 0 { return; }
    *s = State::new(); s.waiting = true;
    ENABLED.store(true, Ordering::Release);
    tracing::info!("SCBL_THERMAL_MATERIAL_OBSERVER state=waiting action=F8 next_round=1 round_limit=3 timeout_starts=F8 writes=0");
}
fn finish(s: &mut State, reason: &str) {
    super::final_ps_probe::finish_round(s.round);
    s.running = false; s.waiting = s.round < ROUND_LIMIT && !STOPPED.load(Ordering::Acquire);
    s.wait_release = true; s.pressed = true;
    if !s.waiting { ENABLED.store(false, Ordering::Release); }
    tracing::info!("SCBL_THERMAL_MATERIAL_OBSERVER state=ended round={} reason={} headers={} attempts={:?} records={} rejected={} reads={} observed_changes={} sample_logs={} truncated={} deferred_changes={} cadence_ms=500 final_limit=8 manual_rearm={} action=release_F8_then_press no_auto_rearm=true",
        s.round, reason, s.headers, s.attempts, s.records.iter().flatten().count(), s.rejected,
        s.reads, s.observed_changes, s.samples_queued, s.truncated, s.deferred_changes, s.waiting);
    // A cap never hides the last state. Emit at most eight copied recent signals,
    // in chronological ring order, once per round even if deinit follows timeout.
    if !s.final_emitted {
        s.final_emitted = true;
        for offset in 0..s.records.len() {
            let index = (s.history_next + offset) % s.records.len();
            if let Some(record) = s.records[index] {
                emit_sample(Sample { record, round: s.round, elapsed_ms: s.record_times[index] }, "final_sample");
            }
        }
    }
}
fn drain(s: &mut State) -> [Option<Sample>; 8] { std::mem::replace(&mut s.pending, [None; 8]) }
fn emit_sample(sample: Sample, state: &str) {
        let r = sample.record;
        tracing::info!("SCBL_THERMAL_MATERIAL_OBSERVER state={} round={} elapsed_ms={} actor={:#x} serial={} world={:#x} level={:#x} proxy={:#x} actor34={} actor35={} flags_lo={:#x} flags_hi={:#x} heat={:#x} category={} strength={} stage={} root={:#x} pass={:#x} table={:#x} shader={:#x} view={:#x} surface={:#x} selected={:#x} head={:#x} selected_is_head={} native_rejected={} key={:#x} through_walls={} range_low={} range_high={} stable=true writes=0",
            state, sample.round, sample.elapsed_ms, r.id.actor, r.id.serial, r.id.world, r.id.level, r.proxy, r.actor_flags & 255, (r.actor_flags >> 8) & 255,
            r.flags_lo, r.flags_hi, r.heat, r.category, r.strength, r.stage, r.root, r.pass, r.table, r.shader, r.view,
            r.surface, r.selected, r.head, r.selected == r.head as usize, r.selected == 0,
            r.key, r.material_flags & 255, (r.material_flags >> 8) & 255, (r.material_flags >> 16) & 255);
}
fn emit(records: [Option<Sample>; 8]) {
    for sample in records.into_iter().flatten() { emit_sample(sample, "sample"); }
}
pub(super) fn deinit() {
    if !build_enabled() { return; }
    STOPPED.store(true, Ordering::Release);
    let was_enabled = ENABLED.swap(false, Ordering::AcqRel);
    if let Ok(mut s) = STATE.try_lock() {
        let records = drain(&mut s);
        if was_enabled { finish(&mut s, "deinit"); }
        s.owner = None; s.started = None; s.waiting = false; s.running = false;
        drop(s); emit(records);
    }
}
struct ViewScope(Option<(usize, u64, u8)>);
impl Drop for ViewScope { fn drop(&mut self) { VIEW.with(|v| v.set(self.0)); } }
pub(super) fn with_view(pass: usize, native: impl FnOnce()) {
    if !enabled() { native(); return; }
    let mut current = None;
    if let Ok(mut s) = STATE.try_lock() {
        if s.waiting {
            let down = key_down();
            if s.wait_release {
                if !down {
                    s.wait_release = false;
                    tracing::info!("SCBL_THERMAL_MATERIAL_OBSERVER state=waiting action=press_F8 next_round={} round_limit=3", s.round + 1);
                }
            } else if down && !s.pressed && s.round < ROUND_LIMIT {
                let round = s.round + 1;
                *s = State::new(); s.round = round; s.running = true; s.started = Some(clock());
                tracing::info!("SCBL_THERMAL_MATERIAL_OBSERVER state=ready round={} waiting_for=first_verified_merc deadline=F8_plus_30000ms manual_rearm_only=true", round);
            }
            s.pressed = down;
        }
        if s.running {
            let now = clock();
            if s.started.is_some_and(|start| now.wrapping_sub(start) >= WINDOW_MS) { finish(&mut s, "timeout"); }
            else { current = Some((pass, now, s.round)); }
        }
    }
    let _scope = ViewScope(VIEW.with(|v| v.replace(current)));
    native();
    if let Ok(mut s) = STATE.try_lock() { let records = drain(&mut s); drop(s); emit(records); }
}
pub(super) fn after_native(pass: usize, root: usize, render: usize, object: usize, surface: usize, selected: usize) {
    clear_verified(); // Never reuse a prior material callback, including early returns.
    if !enabled() || pass == 0 { return; }
    let Some((active_pass, stamp, _round)) = VIEW.with(Cell::get) else { return; };
    if active_pass != pass { return; }
    let Ok(mut s) = STATE.try_lock() else { return; };
    if !s.running { return; }
    // Start at most one 8-slot window every 100 ms. Offset rotates through the
    // PREVIOUS window's observed callback count without reading skipped objects.
    if s.last_window.is_none_or(|last| stamp.checked_sub(last).is_some_and(|age| age >= SAMPLE_MS)) {
        s.offset = if s.seen == 0 { 0 } else { s.offset.saturating_add(SLOTS) % s.seen };
        s.seen = 0; s.last_window = Some(stamp);
        s.owner_offset = if s.owner_seen <= 1 { 1 } else { 1 + s.owner_offset % (s.owner_seen - 1) };
        s.owner_seen = 0;
        s.owner_first = s.owner.is_some(); s.owner_rotating = s.owner.is_some();
        s.owner_deep_left = 2;
        s.regular_left = SLOTS - 2 * usize::from(s.owner.is_some());
    }
    let index = s.seen; s.seen = s.seen.saturating_add(1);
    // Two bounded owner slots: first callback observes a later category change;
    // the second rotates through ONLY this proxy's callback ordinal, so earlier
    // completed-stage draws and dense foreign objects cannot permanently pin it.
    // Pointer priority is not authority: all stored samples still validate identity.
    let owner_priority = if s.owner.is_some() && object == s.owner_proxy {
        let owner_index = s.owner_seen; s.owner_seen = s.owner_seen.saturating_add(1);
        if s.owner_first { s.owner_first = false; true }
        else if s.owner_rotating && owner_index == s.owner_offset { s.owner_rotating = false; true }
        else { false }
    } else { false };
    if !owner_priority {
        if s.owner.is_some() && object == s.owner_proxy { return; } // do not spend regular slots on repeated owner draws
        if index >= s.offset && s.regular_left != 0 { s.regular_left -= 1; }
        else { return; }
    }
    // If a view stalls or is left, expired callbacks cannot perform another RPM.
    if s.started.is_some_and(|start| clock().wrapping_sub(start) >= WINDOW_MS) { finish(&mut s, "timeout"); return; }
    if s.headers >= HEADER_LIMIT || s.reads >= READ_LIMIT { finish(&mut s, "hard_read_or_header_budget"); return; }
    s.headers += 1;
    let mut r = Reader { reads: 0, limit: 60.min(READ_LIMIT - s.reads) };
    let classified = r.word(object, 0x210).zip(r.word(render, 0x233c));
    let Some((heat, stage)) = classified else { s.reads += r.reads; s.rejected += 1; return; };
    let Some(b) = lane(stage, selected) else { s.reads += r.reads; s.rejected += 1; return; };
    let merc = (|| {
        let lo = r.word(object, 0x34)?; let hi = r.word(object, 0x38)?;
        if lo & 0x400 == 0 && hi & (0x10000000 | 0xc0000) == 0 { return None; }
        let actor = r.word(object, 0x20)?;
        (r.word(actor as usize, 0)? == 0x02ad2abc
            && r.word(actor as usize, 0x30c)? as usize == object).then_some(actor)
    })();
    let Some(actor) = merc else { s.reads += r.reads; s.rejected += 1; return; };
    let was_locked = s.owner.is_some();
    if let Some(owner) = s.owner {
        if actor != owner.actor { s.reads += r.reads; s.rejected += 1; return; }
        let current = (|| Some(Identity { actor, serial: r.word(actor as usize, 0x20)?,
            world: r.word(actor as usize, 0x144)?, level: r.word(actor as usize, 0x148)? }))();
        let Some(current) = current else { s.reads += r.reads; s.rejected += 1; return; };
        if current != owner { s.reads += r.reads; finish(&mut s, "target_identity_changed"); return; }
        if s.owner_deep_left == 0 { s.reads += r.reads; return; }
        s.owner_deep_left -= 1;
    } else if s.prelock_failures >= PRELOCK_FAILURES {
        s.reads += r.reads; finish(&mut s, "prelock_validation_budget"); return;
    }
    s.attempts[b] += 1; // per-lane telemetry, NOT a lifetime sampling quota
    let first = snapshot(&mut r, pass, root, render, object, surface, selected);
    let second = first.and_then(|_| snapshot(&mut r, pass, root, render, object, surface, selected));
    s.reads += r.reads;
    let record = first.filter(|one| Some(*one) == second && one.heat == heat && one.stage == stage);
    let Some(record) = record else {
        s.rejected += 1; if !was_locked { s.prelock_failures += 1; }
        if s.reads >= READ_LIMIT { finish(&mut s, "hard_read_budget"); }
        return;
    };
    if let Some(owner) = s.owner {
        if owner.actor == record.id.actor && owner != record.id { finish(&mut s, "target_identity_changed"); return; }
        if owner != record.id { s.rejected += 1; return; }
    }
    if !ENABLED.load(Ordering::Acquire) { return; }
    if s.owner.is_none() {
        s.owner = Some(record.id);
        // Move, do not add, up to two remaining header slots to owner priority.
        let reserved = s.regular_left.min(2); s.regular_left -= reserved;
        s.owner_first = reserved >= 1; s.owner_rotating = reserved >= 2;
        s.owner_seen = 0; s.owner_offset = 1; s.owner_deep_left = 2;
        tracing::info!("SCBL_THERMAL_MATERIAL_OBSERVER state=running round={} deadline=F8_plus_30000ms target_locked=true", s.round);
    }
    s.owner_proxy = object;
    if record.stage == 4 {
        VERIFIED.with(|v| v.set(Some(VerifiedToken {
            round: s.round, elapsed_ms: stamp.wrapping_sub(s.started.unwrap_or(stamp)),
            render, root, object, surface, selected,
            actor: record.id.actor, serial: record.id.serial,
            world: record.id.world, level: record.id.level,
            category: record.category, strength: record.strength, stage: record.stage, key: record.key,
        })));
    }
    let last_stage = usize::from(record.stage == 4); // null/non-null belongs to one temporal history
    let elapsed = stamp.wrapping_sub(s.started.unwrap_or(stamp));
    let changed = !s.last[last_stage].is_some_and(|previous| same_signal(previous, record));
    if changed {
        s.last[last_stage] = Some(record); s.observed_changes += 1;
        let next = s.history_next; s.records[next] = Some(record); s.record_times[next] = elapsed;
        s.history_next = (next + 1) % s.records.len();
    }
    // Observe every granted 100-ms slot, but reserve one output per stage per
    // 500-ms epoch across the ENTIRE 30 seconds (at most 120 ordinary samples).
    // A throttled signal is not marked emitted; a stable later observation may
    // publish it. Sub-500-ms transients are not guaranteed in ordinary output.
    let epoch = elapsed / LOG_MS;
    if s.log_epoch.is_none_or(|previous| epoch > previous) { s.log_epoch = Some(epoch); s.logged_stage = [false; 2]; }
    if s.log_epoch != Some(epoch) { if changed { s.deferred_changes += 1; } return; } // never reopen an older epoch
    if s.last_emitted[last_stage].is_some_and(|previous| same_signal(previous, record)) { return; }
    if s.logged_stage[last_stage] {
        if changed { s.deferred_changes += 1; }
        return;
    }
    if s.samples_queued < SAMPLE_LOG_LIMIT {
        if let Some(slot) = s.pending.iter().position(Option::is_none) {
            s.pending[slot] = Some(Sample { record, round: s.round, elapsed_ms: elapsed });
            s.samples_queued += 1; s.logged_stage[last_stage] = true;
            // Only admission to the drained output queue advances emitted state.
            // Observation-only history above never suppresses a deferred output.
            s.last_emitted[last_stage] = Some(record);
        } else if changed { s.truncated += 1; }
    } else if changed { s.truncated += 1; }
}

