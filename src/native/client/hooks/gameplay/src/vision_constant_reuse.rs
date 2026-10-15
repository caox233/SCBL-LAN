//! Private native candidate. Inspected-addon allowlist; runtime acceptance pending.
//! Raw hot-path reads rely on the same signed native object/lifetime contract as
//! fill/upload; arbitrary addon writes, source mutation, and SEH remain unproven.
#[path = "constant_reuse_policy.rs"]
mod policy;
#[path = "vision_constant_reuse_environment.rs"]
mod environment;
#[path = "vision_constant_shader.rs"]
mod shader;
use policy::{CacheKey, Coverage, PassCache, MAX_PAYLOAD};
use retour::GenericDetour;
use std::{cell::RefCell, mem::MaybeUninit, sync::OnceLock};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
const VIEW_VA: usize = 0x175b4e0;
const FILL_VA: usize = 0x1604cf0;
const QUEUE_A_VA: usize = 0x175aec0;
const QUEUE_B_VA: usize = 0x175afc0;
const MAX_QUEUE_ADMISSIONS: usize = 6;
const RESET_VA: usize = 0x1601e90;
const TEARDOWN_VA: usize = 0x1602070;
const BANK_VA: usize = 0x33c3f08;
const POOL_VA: usize = 0x33e7740;
const OWNER_SLOT_LIMIT: usize = 512; // signed constructor zeroes 0x800 at +0x21B4
const DESC_BYTES: usize = 0xcdc;
const LIST_BUDGET: usize = 256;
type FillFn = unsafe extern "thiscall" fn(usize, usize, usize, usize, usize, usize, usize);
type VoidFn = unsafe extern "C" fn();
// Both signed queue functions return void; opaque arguments are forwarded unchanged.
type QueueFn = unsafe extern "thiscall" fn(usize, usize, usize, usize);
static FILL: OnceLock<GenericDetour<FillFn>> = OnceLock::new();
static QUEUE_A: OnceLock<GenericDetour<QueueFn>> = OnceLock::new();
static QUEUE_B: OnceLock<GenericDetour<QueueFn>> = OnceLock::new();
static RESET: OnceLock<GenericDetour<VoidFn>> = OnceLock::new();
static TEARDOWN: OnceLock<GenericDetour<VoidFn>> = OnceLock::new();
const STOPPED: u32 = 0;
const PENDING: u32 = 1;
const CHECKING: u32 = 2;
const READY: u32 = 3;
static ACTIVATION: AtomicU32 = AtomicU32::new(STOPPED);
fn enabled() -> bool { ACTIVATION.load(Ordering::Acquire) == READY }
// Plugin startup precedes D3D/ReShade addon loading. Validate once when the
// first real EMF view arrives, not at startup and not on every draw/frame.
// CAS publication prevents teardown/deinit during validation reviving the cache.
unsafe fn activate_for_emf() {
    if ACTIVATION.compare_exchange(PENDING, CHECKING, Ordering::AcqRel, Ordering::Acquire).is_err() { return; }
    let verified = environment::verified();
    let next = if verified { READY } else { STOPPED };
    if ACTIVATION.compare_exchange(CHECKING, next, Ordering::AcqRel, Ordering::Acquire).is_err() { return; }
    if verified {
        let (created, proved) = shader::summary();
        tracing::info!("SCBL_EMF_SHADER_PROOF created={} proved={} read_scope=ps_b0_leading_gap_b4_b5 runtime_acceptance=false", created, proved);
        tracing::info!("SCBL_EMF_CONSTANT_REUSE_READY candidate=true default=false owner_slots=512 max_payload=4096 addon_contract=inspected_hash_allowlist runtime_acceptance=false");
    } else {
        shader::stop();
        tracing::warn!("SCBL_EMF_CONSTANT_REUSE_DISABLED reason=unverified_addon_environment stage=first_emf_view native_fallback=true");
    }
}
static GENERATION: AtomicU64 = AtomicU64::new(1);
static PASSES: AtomicU32 = AtomicU32::new(0);
static LOGS: AtomicU32 = AtomicU32::new(0);
fn built() -> bool {
    option_env!("SCBL_EMF_CONSTANT_REUSE_BUILD") == Some("1")
        && option_env!("SCBL_VISION_CAUSAL_BUILD") != Some("1")
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Pool { bank: usize, array: usize, count: usize, cap: usize, width: usize, cursor: usize }
#[derive(Clone, Copy, Default)]
struct Proof { resource: usize, array: usize, bank: usize, bucket: usize, index: usize }
// Diagnostic-only: selected at view entry, independent of cache activation.
// Counts are top-level native batches and pre-call list counts, NOT uploads or
// completed items. No descriptor/array/payload is read by this observer.
struct TailObservation {
    sample: u32, start: std::time::Instant, stop_at_us: Option<u128>,
    owner: usize, generation: u64, tail_forward_depth: u32,
    native_calls: u64, items_before_call: u64, items_unknown: u64,
    segment_complete: bool,
}
#[derive(Clone, Copy, Debug)]
struct QueueRecord {
    offset: usize, phase: usize, stats: [u64; 5], wall_us: Option<u128>,
    complete: bool, stopped: bool,
}
impl QueueRecord {
    const EMPTY: Self = Self { offset: 0, phase: 0, stats: [0; 5], wall_us: None,
        complete: false, stopped: false };
}
struct State {
    depth: u32, active: bool, busy: bool, pass: usize, render: usize, view: usize,
    owner: usize, generation: u64, binding: [usize; 3], stats: [u64; 5], last_hit_seen: u64, budget_stopped: bool,
    // View lifetime is independent of cache/budget activation. A fatal disable
    // can never be revived by a later queue in this same view.
    view_live: bool, view_faulted: bool,
    budget_stats: [u64; 3], budget_last_hit_seen: u64,
    fill_depth: u32, queue_depth: u32, queue_admissions: usize, queue_capped: bool,
    active_queue: Option<usize>, queue_hits: u64,
    queue_records: [QueueRecord; MAX_QUEUE_ADMISSIONS],
    observation: Option<TailObservation>,
    b0_cold_misses: u8, b0_useful: bool, // view-local cold admission budget, distinct from diagnostic counters
    b0_gap_stats: [u64; 3], // eligible, reused, uploaded; same bounded view log, no per-item clock
    cache: PassCache, pools: [Option<Pool>; 13], proofs: [Proof; 32],
}
impl State {
    const fn new() -> Self { Self { depth: 0, active: false, busy: false, pass: 0,
        render: 0, view: 0, owner: 0, generation: 0, binding: [0; 3], stats: [0; 5], last_hit_seen: 0, budget_stopped: false,
        view_live: false, view_faulted: false, budget_stats: [0; 3], budget_last_hit_seen: 0,
        fill_depth: 0, queue_depth: 0, queue_admissions: 0, queue_capped: false, active_queue: None, queue_hits: 0,
        queue_records: [QueueRecord::EMPTY; MAX_QUEUE_ADMISSIONS],
        observation: None, b0_cold_misses: 0, b0_useful: false, b0_gap_stats: [0; 3], cache: PassCache::new(), pools: [None; 13], proofs: [Proof { resource: 0,
        array: 0, bank: 0, bucket: 0, index: 0 }; 32] } }
    fn clear(&mut self) { self.cache.clear(); self.pools = [None; 13]; self.proofs = [Proof::default(); 32]; }
    fn disable(&mut self) { self.active = false; self.clear(); }
    fn fault(&mut self) {
        self.view_faulted = true; self.disable(); self.invalidate_observation();
    }
    fn invalidate_observation(&mut self) {
        if let Some(o) = self.observation.as_mut() { o.segment_complete = false; }
    }
    fn tail_sample(&mut self, owner: usize) -> Option<u32> {
        let o = self.observation.as_mut()?;
        if self.depth != 1 || o.generation != GENERATION.load(Ordering::Acquire) || !enabled() {
            o.segment_complete = false;
        }
        (o.segment_complete && self.budget_stopped && self.owner == owner
            && o.owner == owner && self.generation == o.generation
            && o.tail_forward_depth == 0).then_some(o.sample)
    }
    fn valid(&self) -> bool { self.active && self.view_live && !self.view_faulted && self.depth == 1 && enabled()
        && self.generation == GENERATION.load(Ordering::Acquire) }
    fn observe(&mut self, bucket: usize, p: Pool) {
        if self.pools[bucket].is_some_and(|q| q.bank != p.bank || q.array != p.array
            || q.count != p.count || q.cap != p.cap || q.width != p.width || p.cursor < q.cursor) { self.clear(); }
        self.pools[bucket] = Some(p);
    }
}
thread_local! { static TLS: RefCell<State> = const { RefCell::new(State::new()) }; }
unsafe fn u32_at(p: usize, o: usize) -> usize { ((p + o) as *const u32).read_unaligned() as usize }
unsafe fn u16_at(p: usize, o: usize) -> usize { ((p + o) as *const u16).read_unaligned() as usize }
fn span(p: usize, n: usize) -> bool { p >= 0x10000 && n != 0 && p.checked_add(n).is_some() }
unsafe fn checked_word(p: usize, o: usize) -> Option<usize> {
    let a = p.checked_add(o)?;
    super::readable(a, 4).then(|| (a as *const u32).read_unaligned() as usize)
}
// Diagnostic-only gate snapshots: no hook, clock, pointer output, or policy
// change. The existing observer owns the F8 opt-in/30-second/three-round scope.
// Each round permits 256 headers and three logs in each of four mode buckets.
// A capped round cannot starve a later F8 round; budgets never reset mid-round.
const GATE_HEADER_LIMIT: u32 = 256;
static GATE_HEADERS: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
static GATE_SAMPLES: [[AtomicU32; 4]; 3] =
    [const { [const { AtomicU32::new(0) }; 4] }; 3];
unsafe fn observe_gate(pass: usize, build: bool, outer: Option<bool>,
    activation_before: Option<u32>, scoped: bool, chosen: bool) {
    if option_env!("SCBL_THERMAL_MATERIAL_OBSERVER_BUILD") != Some("1") { return; }
    let Some(round @ 1..=3) = super::material_observer::capturing_round() else { return; };
    let i = usize::from(round - 1);
    let Ok(header) = GATE_HEADERS[i].fetch_update(Ordering::Relaxed, Ordering::Relaxed,
        |n| (n < GATE_HEADER_LIMIT).then_some(n + 1)) else { return; };
    // Use the same checked read primitive, but never use these observations to
    // select scope. The original scope already ran; a mutable native field can
    // disagree, so logs explicitly label this as a post-gate snapshot.
    let render = checked_word(pass, 0x14);
    let view = render.and_then(|r| checked_word(r, 0x20d0));
    let mode = view.and_then(|v| checked_word(v, 0x340));
    let bucket = match mode { Some(8) => 0, Some(9) => 1, Some(_) => 2, None => 3 };
    if let Ok(sample) = GATE_SAMPLES[i][bucket].fetch_update(Ordering::Relaxed,
        Ordering::Relaxed, |n| (n < 3).then_some(n + 1)) {
        let reverse = render.and_then(|r| checked_word(r, 0x236c));
        let owner = render.and_then(|r| checked_word(r, 0xc));
        let activation_after = ACTIVATION.load(Ordering::Acquire);
        let reason = if !build { "not_built" }
            else if outer == Some(false) { "nested" }
            else if !matches!(activation_before, Some(PENDING | READY)) { "activation_ineligible" }
            else if scoped { if chosen { "selected" } else { "activation_not_ready" } }
            else if render.is_none() { "pass_unreadable" }
            else if view.is_none() { "render_unreadable" }
            else if mode.is_none() { "view_unreadable" }
            else if mode != Some(9) { "mode_excluded" }
            else if reverse.is_none() { "reverse_unreadable" }
            else if reverse != Some(pass) { "reverse_mismatch" }
            else if owner.is_none() { "owner_unreadable" }
            else { "snapshot_differs_from_gate" };
        tracing::info!("SCBL_EMF_REUSE_GATE sample={} round={} header={} built={} outer={:?} activation_before={:?} activation_after={} scoped={} chosen={} reason={} pass_nonzero={} render_readable={} render_nonzero={} view_readable={} view_nonzero={} mode={:?} reverse_readable={} reverse_matches={} owner_readable={} owner_nonzero={} snapshot=after_gate raw_addresses=false writes=0 runtime_acceptance=false",
            sample + 1, round, header + 1, build, outer, activation_before,
            activation_after, scoped, chosen, reason, pass != 0, render.is_some(),
            render.is_some_and(|v| v != 0), view.is_some(), view.is_some_and(|v| v != 0),
            mode, reverse.is_some(), reverse == Some(pass), owner.is_some(),
            owner.is_some_and(|v| v != 0));
    }
    if header + 1 == GATE_HEADER_LIMIT {
        tracing::info!("SCBL_EMF_REUSE_GATE state=header_limit round={} headers={} mode8_samples={} mode9_samples={} other_samples={} unreadable_samples={} further_admissions=0 rearm=next_F8_round runtime_acceptance=false",
            round, GATE_HEADER_LIMIT, GATE_SAMPLES[i][0].load(Ordering::Relaxed),
            GATE_SAMPLES[i][1].load(Ordering::Relaxed), GATE_SAMPLES[i][2].load(Ordering::Relaxed),
            GATE_SAMPLES[i][3].load(Ordering::Relaxed));
    }
}


unsafe fn scope(pass: usize) -> Option<(usize, usize, usize)> {
    let render = checked_word(pass, 0x14)?;
    let view = checked_word(render, 0x20d0)?;
    if checked_word(view, 0x340)? != 9 || checked_word(render, 0x236c)? != pass { return None; }
    let owner = checked_word(render, 0xc)?;
    Some((render, view, owner))
}
// Reserve the existing first-three/every-300/max-32 log sample at entry.
// Unsampled views take no clock readings; sampled views add at most one stop clock.
struct ViewGuard(Option<u32>);
impl Drop for ViewGuard {
    fn drop(&mut self) {
        let (stats, last_hit_seen, budget_stopped, observation, queues, b0) = TLS.with(|t| {
            let mut s = t.borrow_mut();
            let observation = if self.0.is_some()
                && s.observation.as_ref().map(|o| o.sample) == self.0 {
                let mut o = s.observation.take().expect("matched sample");
                o.segment_complete &= s.depth == 1 && o.tail_forward_depth == 0
                    && o.generation == GENERATION.load(Ordering::Acquire) && enabled();
                Some(o)
            } else { None }; // A nested view cannot take the outer observation.
            let queues = (s.queue_admissions, s.queue_hits, s.queue_capped, s.view_faulted, s.queue_records);
            let a = (s.stats, s.last_hit_seen, s.budget_stopped, observation, queues, s.b0_gap_stats);
            if s.depth == 1 { s.view_live = false; }
            s.disable(); s.depth = s.depth.saturating_sub(1); a
        });
        if let Some(o) = observation {
            let view_us = o.start.elapsed().as_micros();
            let stop_recorded = o.stop_at_us.is_some(); // zero microseconds is valid
            let stop_at_us = o.stop_at_us.unwrap_or(0);
            let tail_wall_us = o.stop_at_us.map_or(0, |stop| view_us.saturating_sub(stop));
            // Wall segment includes ALL remaining view work, not fill-only CPU/GPU time.
            tracing::info!("SCBL_EMF_CONSTANT_REUSE candidate=true pass={} seen={} eligible={} hits={} misses={} fallback={} budget_stopped={} last_hit_seen={} view_us={} stop_recorded={} stop_at_us={} tail_wall_us={} tail_native_calls={} tail_items_before_call={} tail_items_unknown={} segment_complete={} queue_admissions={} queue_hits={} queue_capped={} view_faulted={} queue_records={:?} b0_gap_eligible={} b0_gap_hits={} b0_gap_uploads={} addon_contract=inspected_hash_allowlist runtime_acceptance=false", o.sample, stats[0], stats[1], stats[2], stats[3], stats[4], budget_stopped, last_hit_seen, view_us, stop_recorded, stop_at_us, tail_wall_us, o.native_calls, o.items_before_call, o.items_unknown, o.segment_complete, queues.0, queues.1, queues.2, queues.3, &queues.4[..queues.0], b0[0], b0[1], b0[2]);
        }
    }
}
// Middleware owned by the one shared native view detour. Mode9 guards unchanged.
pub(super) unsafe fn with_view(pass: usize, native: impl FnOnce()) {
    if !built() {
        observe_gate(pass, false, None, None, false, false);
        native(); return;
    }
    let outer = TLS.with(|t| { let mut s = t.borrow_mut(); let outer = s.depth == 0;
        if outer { s.observation = None; s.view_live = false; s.view_faulted = false;
            s.queue_depth = 0; s.active_queue = None;
        } else { s.fault(); }
        s.depth = s.depth.saturating_add(1); s.disable(); outer });
    let activation_before = ACTIVATION.load(Ordering::Acquire);
    let candidate_scope = if outer && matches!(activation_before, PENDING | READY) {
        scope(pass)
    } else { None };
    if candidate_scope.is_some() { activate_for_emf(); }
    let chosen = if enabled() { candidate_scope } else { None };
    observe_gate(pass, true, Some(outer), Some(activation_before), candidate_scope.is_some(), chosen.is_some());
    let sample = chosen.and_then(|_| {
        let n = PASSES.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        ((n <= 3 || n % 300 == 0) && LOGS.fetch_update(Ordering::Relaxed,
            Ordering::Relaxed, |v| (v < 32).then_some(v + 1)).is_ok()).then_some(n)
    });
    let start = sample.map(|_| std::time::Instant::now());
    TLS.with(|t| { let mut s = t.borrow_mut(); if let Some((render, v, owner)) = chosen {
        s.active = true; s.view_live = true; s.pass = pass; s.render = render; s.view = v; s.owner = owner;
        s.generation = GENERATION.load(Ordering::Acquire); s.binding = [0; 3]; s.stats = [0; 5]; s.b0_gap_stats = [0; 3]; s.b0_cold_misses = 0; s.b0_useful = false; s.last_hit_seen = 0; s.budget_stopped = false;
        s.budget_stats = [0; 3]; s.budget_last_hit_seen = 0;
        s.queue_admissions = 0; s.queue_capped = false; s.queue_hits = 0;
        s.queue_records = [QueueRecord::EMPTY; MAX_QUEUE_ADMISSIONS];
        s.observation = sample.zip(start).map(|(sample, start)| TailObservation {
            sample, start, stop_at_us: None, owner, generation: s.generation,
            tail_forward_depth: 0, native_calls: 0, items_before_call: 0,
            items_unknown: 0, segment_complete: true,
        });
    }});
    let _guard = ViewGuard(sample);
    native(); // no RefCell borrow crosses native; shared owner forwards exactly once
}
fn advance_generation() {
    if GENERATION.fetch_add(1, Ordering::AcqRel) == u64::MAX { ACTIVATION.store(STOPPED, Ordering::Release); }
    TLS.with(|t| t.borrow_mut().fault());
}
unsafe extern "C" fn reset() { advance_generation(); RESET.get().expect("published").call(); }
unsafe extern "C" fn teardown() {
    shader::stop();
    ACTIVATION.store(STOPPED, Ordering::Release); advance_generation();
    TEARDOWN.get().expect("published").call();
}
// Signed native queue identity, not its label text. Early phase 7/9 remains
// under the original view budget; only the later six queue roles may rearm.
fn queue_role(kind: u8, pass: usize, queue: usize, phase: usize) -> Option<(usize, bool)> {
    let offsets: &[usize] = if kind == 0 { &[0xc0, 0xb4, 0xcc, 0xf0] } else { &[0x9c, 0xa8] };
    let offset = offsets.iter().copied().find(|&o| pass.checked_add(o) == Some(queue))?;
    let late = if kind == 1 { phase == 5 }
        else if offset == 0xf0 { phase == 10 } else { phase == 8 };
    // Only signed early phase 7/9 continues the original view policy. Unknown
    // phase/header combinations disable the view instead of spanning a boundary.
    let early = kind == 0 && if offset == 0xf0 { phase == 9 } else { phase == 7 };
    (late || early).then_some((offset, late))
}
struct QueueGuard {
    tracked: bool, pass: usize, generation: u64, record: Option<usize>,
    before: [u64; 5], start: Option<std::time::Instant>,
}
impl Drop for QueueGuard {
    fn drop(&mut self) {
        if !self.tracked { return; }
        let wall_us = self.start.map(|t| t.elapsed().as_micros()); // at most one exit clock
        TLS.with(|t| { let mut s = t.borrow_mut();
            if s.pass != self.pass || s.generation != self.generation
                || s.generation != GENERATION.load(Ordering::Acquire) || !enabled() {
                s.fault();
            }
            if let Some(index) = self.record {
                let complete = s.view_live && !s.view_faulted && s.depth == 1
                    && s.queue_depth == 1 && s.fill_depth == 0 && !s.busy
                    && s.active_queue == Some(index);
                if !complete { s.fault(); }
                let stats = std::array::from_fn(|i| s.stats[i].saturating_sub(self.before[i]));
                let r = &mut s.queue_records[index];
                r.stats = stats; r.wall_us = wall_us; r.complete = complete;
                // Never restore a parent cache or leave queue-owned proofs live.
                s.active_queue = None; s.disable();
            }
            s.queue_depth = s.queue_depth.saturating_sub(1);
        });
    }
}
unsafe fn enter_queue(kind: u8, pass: usize, queue: usize) -> QueueGuard {
    let (tracked, generation, record, before, timed) = TLS.with(|t| {
        let mut s = t.borrow_mut();
        if !s.view_live { return (false, 0, None, [0; 5], false); }
        let generation = s.generation;
        let nested = s.queue_depth != 0;
        s.queue_depth = s.queue_depth.saturating_add(1);
        if nested || s.busy || s.fill_depth != 0 || s.depth != 1 || s.view_faulted || !enabled()
            || generation != GENERATION.load(Ordering::Acquire) || pass != s.pass {
            s.fault(); return (true, generation, None, s.stats, false);
        }
        // Do not refresh the view's initial generation or owner identity here.
        if u32_at(pass, 0x14) != s.render || u32_at(s.render, 0xc) != s.owner
            || u32_at(s.render, 0x20d0) != s.view || u32_at(s.view, 0x340) != 9
            || u32_at(s.render, 0x236c) != pass {
            s.fault(); return (true, generation, None, s.stats, false);
        }
        let phase = u32_at(s.render, 0x233c);
        let Some((offset, late)) = queue_role(kind, pass, queue, phase) else {
            s.fault(); return (true, generation, None, s.stats, false);
        };
        if !late { return (true, generation, None, s.stats, false); }
        // The signed functions return without drawing for count <= 0. Empty
        // invocations do not consume one of the six nonempty admissions.
        if !span(queue, 8) { s.fault(); return (true, generation, None, s.stats, false); }
        if (u32_at(queue, 4) as i32) <= 0 { return (true, generation, None, s.stats, false); }
        if s.queue_admissions >= MAX_QUEUE_ADMISSIONS {
            s.queue_capped = true; s.disable(); return (true, generation, None, s.stats, false);
        }
        let index = s.queue_admissions; s.queue_admissions += 1;
        s.clear(); s.budget_stats = [0; 3]; s.budget_last_hit_seen = 0;
        s.active_queue = Some(index); s.active = true;
        s.queue_records[index] = QueueRecord { offset, phase, ..QueueRecord::EMPTY };
        // Legacy tail is no longer a continuous stopped region once rearmed.
        // Retain its fields for compatibility but permanently mark it incomplete.
        s.invalidate_observation();
        (true, generation, Some(index), s.stats, s.observation.is_some())
    });
    QueueGuard { tracked, pass, generation, record, before,
        start: timed.then(std::time::Instant::now) } // at most one entry clock, sampled views only
}
unsafe extern "thiscall" fn queue_a(pass: usize, queue: usize, arg2: usize, arg3: usize) {
    let _guard = enter_queue(0, pass, queue);
    QUEUE_A.get().expect("published").call(pass, queue, arg2, arg3);
}
unsafe extern "thiscall" fn queue_b(pass: usize, queue: usize, arg2: usize, arg3: usize) {
    let _guard = enter_queue(1, pass, queue);
    QUEUE_B.get().expect("published").call(pass, queue, arg2, arg3);
}
#[repr(C)]
struct List { array: usize, count: usize }
unsafe fn original(owner: usize, device: usize, context: usize, out: usize,
    cap: usize, list: usize, flags: usize) {
    FILL.get().expect("published").call(owner, device, context, out, cap, list, flags);
}
unsafe fn one(owner: usize, device: usize, context: usize, out: usize,
    cap: usize, descriptor: usize, flags: usize) {
    let entry = descriptor; let list = List { array: &entry as *const usize as usize, count: 1 };
    original(owner, device, context, out, cap, &list as *const List as usize, flags);
}
unsafe fn pool(bucket: usize) -> Option<Pool> {
    if bucket >= 13 { return None; }
    let bank = u32_at(BANK_VA, 0); if bank > 1 { return None; }
    let a = POOL_VA + bank * 0x104 + bucket * 0x14;
    let p = Pool { bank, array: u32_at(a, 0), count: u32_at(a, 4), cap: u32_at(a, 8),
        width: u32_at(a, 12), cursor: u32_at(a, 16) };
    if p.width != 1usize << bucket || !(16..=MAX_PAYLOAD).contains(&p.width)
        || p.cursor > p.count || p.count > p.cap || p.cap > 65536 { return None; }
    if p.count != 0 && !span(p.array, p.count.checked_mul(4)?) { return None; }
    Some(p)
}
const RECORD_LIMIT: usize = 32;
struct Item {
    descriptor: usize, count: usize,
    len: usize, shader_epoch: Option<u64>, slot: usize, key: CacheKey, pool: Pool,
}
impl Item { fn proof_current(&self) -> bool { self.shader_epoch.is_none_or(|e| shader::epoch() == e) } }
// Ok(None): known native ineligible item. Err: schema/lifetime uncertainty.
unsafe fn item(owner: usize, device: usize, context: usize, desc: usize,
    list: usize, flags: usize, records: &mut [MaybeUninit<[u32; 4]>; RECORD_LIMIT]) -> Result<Option<Item>, ()> {
    if !span(desc, DESC_BYTES) { return Err(()); }
    let owner_slot = u32_at(desc, 0xcc8); let slot = u16_at(desc, 0xcd6);
    if owner_slot >= OWNER_SLOT_LIMIT || slot >= 14 { return Err(()); }
    let field = owner.checked_add(0x21b4).and_then(|p| p.checked_add(owner_slot * 4)).ok_or(())?;
    if !span(field, 4) { return Err(()); }
    if u32_at(field, 0) != 0 { return Ok(None); }
    let bucket = u32_at(desc, 0xccc); let p = pool(bucket).ok_or(())?;
    let count = u16_at(desc, 0xcd8);
    if count == 0 || count > RECORD_LIMIT { return Ok(None); }
    let record = u32_at(desc, 0xcc0);
    if !span(record, count * 16) { return Err(()); }
    let mut len = 0usize;
    let mut leading_gap = false;
    for (i, saved) in records[..count].iter_mut().enumerate() {
        // Snapshot metadata before reading payload or calling native code.
        std::ptr::copy_nonoverlapping((record + i * 16) as *const u8, saved.as_mut_ptr() as *mut u8, 16);
        // This complete 16-byte write initializes exactly this record, not the tail.
        let saved = saved.assume_init_ref();
        let offset = (saved[1] & 0xffff) as usize; let size = (saved[1] >> 16) as usize;
        // Only one precisely understood hole is admitted, and only with a
        // live PS-b0 component proof. No sorting/merging/other holes are allowed.
        if i == 1 && len == 4 && offset == 16 && slot == 0 {
            // A cold sparse-b0 stream costs more to inspect than to upload natively.
            // Stop only this NEW coverage after 16 misses and no reuse in this view.
            // Returning None forwards original records; no trust/cache decision is relaxed.
            if TLS.with(|t| { let s = t.borrow(); !s.b0_useful && s.b0_cold_misses >= 16 }) { return Ok(None); }
            leading_gap = true; len = 16;
        }
        if offset != len || size == 0 || len + size > p.width { return Ok(None); }
        if !span(saved[2] as usize, size) { return Err(()); }
        len += size;
    }
    let (coverage, shader_epoch) = if leading_gap {
        if len != u16_at(desc, 0xcd4) || len >= p.width { return Ok(None); }
        let Some(proof) = shader::leading_gap(owner, list, flags, slot, len) else { return Ok(None); };
        (Coverage::ShaderLeadingGap { bytes: len as u16, identity: proof.serial }, Some(proof.epoch))
    } else if len == p.width { (Coverage::Physical, None) }
    else {
        if len != u16_at(desc, 0xcd4) { return Ok(None); }
        let Some(proof) = shader::prefix(owner, list, flags, slot, len) else { return Ok(None); };
        (Coverage::ShaderPrefix { bytes: len as u16, identity: proof.serial }, Some(proof.epoch))
    };
    let generation = TLS.with(|t| t.borrow().generation);
    Ok(Some(Item { descriptor: desc, count, len, shader_epoch, slot,
        key: CacheKey { owner, device, context, descriptor: desc, bucket: bucket as u8, generation, coverage }, pool: p }))
}
// Track an in-flight fill even after budget stop, independently of log sampling.
// This prevents a native callback from rearming a queue inside its caller's fill.
// Active calls reuse their existing drop TLS visit; inactive non-EMF needs none.
struct FillGuard { active: bool, tracked: bool }
impl Drop for FillGuard { fn drop(&mut self) {
    if !self.active && !self.tracked { return; }
    TLS.with(|t| { let mut s = t.borrow_mut();
        if self.active { s.busy = false; }
        if self.tracked { s.fill_depth = s.fill_depth.saturating_sub(1); }
    });
} }
fn fallback(invalid: bool) { TLS.with(|t| { let mut s = t.borrow_mut(); s.stats[4] += 1; if invalid { s.fault(); } }); }
// A distinct forwarding guard suppresses callback reentry without changing busy,
// cache state, or native forwarding. It never keeps a RefCell borrow across native.
struct TailForwardGuard(u32);
impl Drop for TailForwardGuard {
    fn drop(&mut self) {
        TLS.with(|t| { let mut s = t.borrow_mut(); let depth = s.depth;
            if let Some(o) = s.observation.as_mut().filter(|o| o.sample == self.0) {
                o.tail_forward_depth = o.tail_forward_depth.saturating_sub(1);
                o.segment_complete &= depth == 1
                    && o.generation == GENERATION.load(Ordering::Acquire) && enabled();
            }
        });
    }
}
unsafe fn observe_tail(sample: u32, owner: usize, cap: usize, list: usize,
    stop_start: Option<std::time::Instant>) -> Option<TailForwardGuard> {
    // This one optional stop reading is after disable and before the native batch.
    let stop_at_us = stop_start.map(|start| start.elapsed().as_micros());
    let items = if cap == 14 && span(list, 8) {
        let count = u32_at(list, 4); // only list+4; do not read list+0 or its array
        (count <= LIST_BUDGET).then_some(count as u64)
    } else { None };
    TLS.with(|t| { let mut s = t.borrow_mut();
        if s.tail_sample(owner) != Some(sample) { return None; }
        let o = s.observation.as_mut().expect("matched sample");
        if o.stop_at_us.is_none() { o.stop_at_us = stop_at_us; }
        o.tail_forward_depth = 1;
        o.native_calls = o.native_calls.saturating_add(1);
        if let Some(n) = items { o.items_before_call = o.items_before_call.saturating_add(n); }
        else { o.items_unknown = o.items_unknown.saturating_add(1); }
        Some(TailForwardGuard(sample))
    })
}
unsafe extern "thiscall" fn fill(owner: usize, device: usize, context: usize,
    out: usize, cap: usize, list: usize, flags: usize) {
    let (active, tail_sample, stop_start, tracked) = TLS.with(|t| { let mut s = t.borrow_mut();
        let tracked = s.view_live;
        if tracked { s.fill_depth = s.fill_depth.saturating_add(1); }
        if !s.active { return (false, s.tail_sample(owner), None, tracked); } // no extra TLS/clock outside sampled tail
        if !s.valid() || s.busy || s.owner != owner { s.fault(); return (false, None, None, tracked); }
        // Excluded native bindings are not failed cache attempts. Measure ratio
        // over eligible attempts, but bound a no-hit tail by ALL inspected items.
        // Decide only before a whole list: preserve native batching/reload order.
        if policy::should_back_off(s.budget_stats[0], s.budget_stats[1], s.budget_stats[2], s.budget_last_hit_seen) {
            s.budget_stopped = true;
            if let Some(i) = s.active_queue { s.queue_records[i].stopped = true; }
            s.disable();
            let sample = s.tail_sample(owner);
            let start = sample.and_then(|_| s.observation.as_ref().map(|o| o.start));
            return (false, sample, start, tracked);
        }
        if u32_at(s.pass, 0x14) != s.render || u32_at(s.render, 0xc) != owner
            || u32_at(s.render, 0x20d0) != s.view || u32_at(s.view, 0x340) != 9
            || u32_at(s.render, 0x236c) != s.pass { s.fault(); return (false, None, None, tracked); }
        if s.binding != [owner, device, context] { s.clear(); s.binding = [owner, device, context]; }
        s.busy = true; (true, None, None, tracked) });
    let _guard = FillGuard { active, tracked };
    if !active {
        let _tail_guard = tail_sample.and_then(|sample| observe_tail(sample, owner, cap, list, stop_start));
        original(owner, device, context, out, cap, list, flags); return;
    }
    if cap != 14 || !span(out, 56) || !span(list, 8) || device == 0 || context == 0 {
        fallback(true); original(owner, device, context, out, cap, list, flags); return;
    }
    let count = u32_at(list, 4);
    let array = u32_at(list, 0);
    if count > LIST_BUDGET || (count != 0 && !span(array, count * 4)) {
        fallback(true); original(owner, device, context, out, cap, list, flags); return;
    }
    for i in 0..count {
        // Native captures count but reloads list+0 per iteration. Match that order.
        let current = u32_at(list, 0);
        if !span(current, count * 4) {
            fallback(true); // forward only the unprocessed suffix; never replay earlier items
            let rest = List { array: current.wrapping_add(i * 4), count: count - i };
            original(owner, device, context, out, cap, &rest as *const List as usize, flags); return;
        }
        let desc = u32_at(current, i * 4);
        TLS.with(|t| { let mut s = t.borrow_mut(); s.stats[0] += 1; s.budget_stats[0] += 1; });
        // Caller-owned scratch has no zero-fill and never travels inside Item.
        // A nested native callback gets independent stack storage.
        let mut records = [MaybeUninit::<[u32; 4]>::uninit(); RECORD_LIMIT];
        let candidate = if TLS.with(|t| t.borrow().valid()) { item(owner, device, context, desc, list, flags, &mut records) } else { Err(()) };
        let it = match candidate {
            Ok(Some(it)) => it,
            Ok(None) => { fallback(false); one(owner, device, context, out, cap, desc, flags); continue; }
            Err(()) => { fallback(true); one(owner, device, context, out, cap, desc, flags); continue; }
        };
        // Some(Item) proves all 0..count records were completely initialized.
        // Never form a reference to uninitialized 32-record storage or its tail.
        let records = std::slice::from_raw_parts_mut(records.as_mut_ptr() as *mut [u32; 4], it.count);
        let mut bytes = MaybeUninit::<[u8; MAX_PAYLOAD]>::uninit();
        if matches!(it.key.coverage, Coverage::ShaderLeadingGap { .. }) {
            // Initialize private comparison scratch only. Original native record
            // offsets/sizes remain untouched, so GPU [4,16) is never uploaded.
            std::ptr::write_bytes((bytes.as_mut_ptr() as *mut u8).add(4), 0, 12);
        }
        for record in records.iter() {
            let offset = (record[1] & 0xffff) as usize; let size = (record[1] >> 16) as usize;
            std::ptr::copy_nonoverlapping(record[2] as *const u8, (bytes.as_mut_ptr() as *mut u8).add(offset), size);
        }
        // Only the copied prefix is turned into a reference; never assume-init the tail.
        let payload = std::slice::from_raw_parts(bytes.as_ptr() as *const u8, it.len);
        let hit = TLS.with(|t| { let mut s = t.borrow_mut(); s.stats[1] += 1; s.budget_stats[1] += 1;
            if matches!(it.key.coverage, Coverage::ShaderLeadingGap { .. }) { s.b0_gap_stats[0] += 1; }
            s.observe(it.key.bucket as usize, it.pool);
            let (resource, index) = s.cache.lookup_with_index(it.key, payload)?;
            let proof = &s.proofs[index];
            if proof.resource != resource { return None; }
            (proof.bank == it.pool.bank && proof.array == it.pool.array
                && proof.bucket == it.key.bucket as usize && proof.index < it.pool.cursor
                && proof.index < it.pool.count && u32_at(it.pool.array, proof.index * 4) == resource).then_some(resource) });
        if let Some(resource) = hit {
            if it.proof_current() && TLS.with(|t| t.borrow().valid()) {
                ((out + it.slot * 4) as *mut u32).write_unaligned(resource as u32);
                TLS.with(|t| { let mut s = t.borrow_mut(); s.stats[2] += 1; s.last_hit_seen = s.stats[0];
                    s.budget_stats[2] += 1; s.budget_last_hit_seen = s.budget_stats[0];
                    if matches!(it.key.coverage, Coverage::ShaderLeadingGap { .. }) { s.b0_gap_stats[1] += 1; s.b0_useful = true; }
                    if s.active_queue.is_some() { s.queue_hits += 1; } }); continue;
            }
        }
        TLS.with(|t| { let mut s = t.borrow_mut(); s.stats[3] += 1;
            if matches!(it.key.coverage, Coverage::ShaderLeadingGap { .. }) { s.b0_gap_stats[2] += 1; s.b0_cold_misses = s.b0_cold_misses.saturating_add(1); }
        });
        // Stack-owned immutable upload snapshot; pointers installed only after moves.
        let mut descriptor = MaybeUninit::<[u32; DESC_BYTES / 4]>::uninit();
        std::ptr::copy_nonoverlapping(it.descriptor as *const u8, descriptor.as_mut_ptr() as *mut u8, DESC_BYTES);
        for record in records.iter_mut() {
            record[2] = payload.as_ptr().add((record[1] & 0xffff) as usize) as u32;
        }
        (descriptor.as_mut_ptr() as *mut u32).add(0xcc0 / 4).write(records.as_ptr() as u32);
        one(owner, device, context, out, cap, descriptor.as_ptr() as usize, flags);
        // No cache borrow survived native; callbacks may have nested/reset/destroyed.
        if !it.proof_current() || !TLS.with(|t| t.borrow().valid()) || GENERATION.load(Ordering::Acquire) != it.key.generation { continue; }
        let Some(after) = pool(it.key.bucket as usize) else { fallback(true); continue; };
        let resource = u32_at(out, it.slot * 4);
        if after.bank != it.pool.bank || after.cursor != it.pool.cursor + 1
            || after.count <= it.pool.cursor || resource == 0
            || u32_at(after.array, it.pool.cursor * 4) != resource { fallback(true); continue; }
        TLS.with(|t| { let mut s = t.borrow_mut(); if !s.valid() { return; }
            s.observe(it.key.bucket as usize, after);
            let Some(index) = s.cache.insert_with_index(it.key, payload, resource) else { return; }; // lifetime proof follows the exact payload slot
            s.proofs[index] = Proof { resource, array: after.array, bank: after.bank,
                bucket: it.key.bucket as usize, index: it.pool.cursor };
        });
    }
}
const CONTRACTS: &[(usize, usize, &str)] = &[
    (QUEUE_A_VA, 0xf8, "339f344f1bfdcd3ff38f9980af098cb5047c3ffc8728d6c59ddc35a84d7b8540"),
    (QUEUE_B_VA, 0xf8, "4f3b4416fa5eabb183814994a89efbcd0b572aceecb1ededd238781a03391326"),
    (VIEW_VA, 0x1413, "6a00377514208193ee8e399d004556c06754324e1fbf2302a870dbd84736f281"),
    (FILL_VA, 0xa0, "42eb8245d0124a7d07af1ed4aa3c752c44675bad3ad608e29b285255dde06d42"),
    (RESET_VA, 0x3d, "1c6da0767b6b15779737a635547804404111a7b40296450542edf3a53820495a"),
    (TEARDOWN_VA, 0x64, "efe63e802de4f3625a69f5b463c46b2f62fe435943699b2f1c7056e174cc93d3"),
    (0x1606e10, 0xb0, "368976c6ab4962be97f99569bf63a961271e5e2bb4b4c2bc3ae752e0488b078d"),
];
pub(super) unsafe fn init() {
    if !built() || !CONTRACTS.iter().all(|&(a, n, h)| super::signature(a, n, h)) { return; }
    if ACTIVATION.compare_exchange(STOPPED, PENDING, Ordering::AcqRel, Ordering::Acquire).is_err() { return; }
    let ok = shader::init()
        && super::install_direct(&RESET, std::mem::transmute::<usize, VoidFn>(RESET_VA), reset as VoidFn)
        && super::install_direct(&TEARDOWN, std::mem::transmute::<usize, VoidFn>(TEARDOWN_VA), teardown as VoidFn)
        && super::install_direct(&FILL, std::mem::transmute::<usize, FillFn>(FILL_VA), fill as FillFn)
        && super::install_direct(&QUEUE_A, std::mem::transmute::<usize, QueueFn>(QUEUE_A_VA), queue_a as QueueFn)
        && super::install_direct(&QUEUE_B, std::mem::transmute::<usize, QueueFn>(QUEUE_B_VA), queue_b as QueueFn);
    if !ok { deinit(); return; }
    tracing::info!("SCBL_EMF_CONSTANT_REUSE_PENDING reason=await_first_emf_view native_fallback=true");
}
pub(super) unsafe fn deinit() {
    shader::deinit();
    ACTIVATION.store(STOPPED, Ordering::Release); advance_generation();
    if let Some(h) = QUEUE_A.get() { let _ = h.disable(); }
    if let Some(h) = QUEUE_B.get() { let _ = h.disable(); }
    if let Some(h) = FILL.get() { let _ = h.disable(); }
    if let Some(h) = RESET.get() { let _ = h.disable(); }
    if let Some(h) = TEARDOWN.get() { let _ = h.disable(); }
}

