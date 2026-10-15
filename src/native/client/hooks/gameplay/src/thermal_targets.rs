//! Thermal target membership must remain coherent through classification and draw.
//! Only native MP mode8 render proxies are changed, for one synchronous view draw.
//! Actor/default/material data, depth queues, other vision modes and FIX8 stay native.
use crate::equipment_native::{readable, signature, word};
use std::{cell::Cell, sync::atomic::{AtomicBool, AtomicU32, Ordering}, time::Instant};

const CAMERA: u32 = 0x02a76b1c;
const DRONE: u32 = 0x02a88f54;
const CATEGORY: usize = 0x213;
const MAX_OBJECTS: usize = 65536;
const MAX_TARGETS: usize = 256;
static READY: AtomicBool = AtomicBool::new(false);

// Bounded outer-mode8 observation only; no new native hook or reuse eligibility.
static PERF_VIEWS: AtomicU32 = AtomicU32::new(0);
static PERF_LOGS: AtomicU32 = AtomicU32::new(0);
fn perf_sample_due(view: u32) -> bool { view != 0 && (view <= 3 || view % 300 == 0) }
struct Observation {
    sample: u32, collect_start: Instant, collect_us: u128,
    objects: usize, targets: usize, collection_complete: bool,
    range_hits: u64, range_queries: u64, range_rejects: u64,
}
impl Observation {
    fn begin() -> Option<Self> {
        let sample = PERF_VIEWS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if !perf_sample_due(sample) { return None; }
        PERF_LOGS.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |n| (n < 32).then_some(n + 1)).ok()?;
        Some(Self { sample, collect_start: Instant::now(), collect_us: 0,
            objects: 0, targets: 0, collection_complete: false,
            range_hits: 0, range_queries: 0, range_rejects: 0 })
    }
}

#[derive(Clone, Copy)]
struct Swap { object: usize, owner: usize, table: u32, serial: u32, before: u8, during: u8 }
impl Swap {
    unsafe fn valid(&self, ranges: &mut ReadRanges<'_>) -> bool {
        ranges.word(self.object, 0x20) == Some(self.owner as u32)
            && ranges.word(self.owner, 0) == Some(self.table)
            && ranges.word(self.owner, 0x20) == Some(self.serial)
            && ranges.word(self.owner, 0x30c) == Some(self.object as u32)
            && ranges.writable(self.object + CATEGORY, 1)
    }
    unsafe fn change(&self, from: u8, to: u8, ranges: &mut ReadRanges<'_>) {
        // Do not overwrite a replacement owner/proxy or a native state change.
        if self.valid(ranges) && (self.object as *const u8).add(CATEGORY).read() == from {
            (self.object as *mut u8).add(CATEGORY).write(to);
        }
    }
}
struct Swaps(Vec<Swap>);
impl Swaps {
    unsafe fn apply(&self) {
        if self.0.is_empty() { return; }
        let mut ranges = ReadRanges::default();
        for p in &self.0 { p.change(p.before, p.during, &mut ranges); }
    }
    unsafe fn restore(&self) {
        if self.0.is_empty() { return; }
        // Never reuse collect/apply proofs after native code or a nested view.
        let mut ranges = ReadRanges::default();
        for p in self.0.iter().rev() { p.change(p.during, p.before, &mut ranges); }
    }
}
// Stack-local sets only. Nested ordinary/EMF views suspend the thermal parent.
// No frame-to-frame owner cache, world scan, shared actor writes or GPU hooks.
thread_local! { static ACTIVE: Cell<*const Swaps> = const { Cell::new(std::ptr::null()) }; }

#[derive(Clone, Copy)]
struct Input { data: usize, count: usize, stride: usize }
// Each validation cache belongs to one synchronous collect, apply, or restore.
// No native callback occurs inside any of these operations. Apply/restore only
// write category bytes, not region protection. Cache extents and permissions,
// never identity values; every field is reread in its original short-circuit order.
// Thousands of proxies commonly share heap regions; querying each twice would
// add milliseconds to thermal draws. Mixed/guard pages are never coalesced.
#[derive(Clone, Copy, Default)]
struct Span { start: usize, end: usize, writable: bool }
// Keep the existing four-region hot ring, then search a bounded positive-only
// index. Once full, retain those extents and let the hot ring cover later regions.
// This avoids per-hit linear scans and unbounded insert/eviction work. The index
// is operation-local, and never merges adjacent regions or caches read rejections.
const READ_REGION_LIMIT: usize = 128;
// Once the retained index is full, remember a small moving tail without
// evicting its original entries. Only validated readable regions are retained.
const READ_SPILL_LIMIT: usize = 8;
// After the inline index fills, cold scene-list revisits use a fixed four-way
// page hash. Allocate only on overflow, never grow the sorted index or scan all
// retained regions. Hashing selects candidates only: exact VQ extents and write
// permissions remain the proof, including for requests crossing a page edge.
const COLD_PAGE_BUCKETS: usize = 256;
const COLD_PAGE_WAYS: usize = 4;
struct ReadRanges<'a> {
    spans: [Span; 4], next: usize, observation: Option<&'a mut Observation>,
    regions: [Span; READ_REGION_LIMIT], region_count: usize,
    spill: [Span; READ_SPILL_LIMIT], spill_next: usize,
    cold: Vec<[Span; COLD_PAGE_WAYS]>, cold_next: usize, cold_disabled: bool,
}
impl Default for ReadRanges<'_> {
    fn default() -> Self {
        Self { spans: [Span::default(); 4], next: 0, observation: None,
            regions: [Span::default(); READ_REGION_LIMIT], region_count: 0,
            spill: [Span::default(); READ_SPILL_LIMIT], spill_next: 0,
            cold: Vec::new(), cold_next: 0, cold_disabled: false }
    }
}
impl ReadRanges<'_> {
    fn cold_bucket(p: usize) -> usize {
        // Fold high page bits so 64 KiB-aligned allocations do not all collide.
        ((p >> 12) ^ (p >> 20) ^ (p >> 28)) & (COLD_PAGE_BUCKETS - 1)
    }
    #[cold]
    #[inline(never)]
    fn remember_cold(&mut self, p: usize, span: Span) {
        if self.cold_disabled { return; }
        if self.cold.is_empty() {
            // Allocation failure affects query reuse only, never acceptance.
            if self.cold.try_reserve_exact(COLD_PAGE_BUCKETS).is_err() {
                self.cold_disabled = true;
                return;
            }
            self.cold.resize(COLD_PAGE_BUCKETS, [Span::default(); COLD_PAGE_WAYS]);
        }
        let bucket = &mut self.cold[Self::cold_bucket(p)];
        let slot = bucket.iter().position(|s| s.end == 0).unwrap_or_else(|| {
            let slot = self.cold_next;
            self.cold_next = (self.cold_next + 1) % COLD_PAGE_WAYS;
            slot
        });
        bucket[slot] = span;
    }
    fn remember_hot(&mut self, span: Span) {
        self.spans[self.next] = span;
        self.next = (self.next + 1) % self.spans.len();
    }
    unsafe fn contains(&mut self, p: usize, n: usize) -> bool { self.access(p, n, false) }
    unsafe fn writable(&mut self, p: usize, n: usize) -> bool { self.access(p, n, true) }
    unsafe fn word(&mut self, p: usize, offset: usize) -> Option<u32> {
        let address = p.checked_add(offset)?;
        self.contains(address, 4).then(|| (address as *const u32).read_unaligned())
    }
    unsafe fn access(&mut self, p: usize, n: usize, write: bool) -> bool {
        use windows::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
        if p < 0x10000 || n == 0 { return false; }
        let Some(end) = p.checked_add(n) else { return false; };
        if let Some(span) = self.spans.iter().find(|s| s.start <= p && end <= s.end) {
            if let Some(o) = self.observation.as_mut() { o.range_hits += 1; }
            return !write || span.writable;
        }
        let upper = self.regions[..self.region_count].partition_point(|s| s.start <= p);
        if upper != 0 {
            let span = self.regions[upper - 1];
            if end <= span.end {
                // A hot-ring miss is filled exactly as after a successful VQ.
                self.remember_hot(span);
                if let Some(o) = self.observation.as_mut() { o.range_hits += 1; }
                return !write || span.writable;
            }
        }
        if self.region_count == READ_REGION_LIMIT {
            if let Some(span) = self.spill.iter().copied().find(|s| s.start <= p && end <= s.end) {
                // Same hot-ring update as a successful VQ, but without querying.
                self.remember_hot(span);
                if let Some(o) = self.observation.as_mut() { o.range_hits += 1; }
                return !write || span.writable;
            }
            if !self.cold.is_empty() {
                if let Some(span) = self.cold[Self::cold_bucket(p)].iter().copied()
                    .find(|s| s.start <= p && end <= s.end)
                {
                    self.remember_hot(span);
                    if let Some(o) = self.observation.as_mut() { o.range_hits += 1; }
                    return !write || span.writable;
                }
            }
        }
        let mut m = MEMORY_BASIC_INFORMATION::default();
        // A miss counts only an actual VirtualQuery, not invalid-address prechecks.
        if let Some(o) = self.observation.as_mut() { o.range_queries += 1; }
        if VirtualQuery(Some(p as _), &mut m, std::mem::size_of_val(&m)) == 0
            || m.State != MEM_COMMIT || m.Protect.0 & 0x101 != 0 || m.Protect.0 & 0xee == 0
        {
            if let Some(o) = self.observation.as_mut() { o.range_rejects += 1; }
            return false;
        }
        let Some(limit) = (m.BaseAddress as usize).checked_add(m.RegionSize) else { return false; };
        if end > limit {
            if let Some(o) = self.observation.as_mut() { o.range_rejects += 1; }
            return false;
        }
        let span = Span { start: m.BaseAddress as usize, end: limit, writable: m.Protect.0 & 0xcc != 0 };
        self.remember_hot(span);
        if self.region_count < READ_REGION_LIMIT {
            // VQ extents are disjoint during this synchronous operation.
            // A successful uncached region belongs at the same predecessor gap.
            self.regions.copy_within(upper..self.region_count, upper + 1);
            self.regions[upper] = span;
            self.region_count += 1;
        } else {
            self.spill[self.spill_next] = span;
            self.spill_next = (self.spill_next + 1) % self.spill.len();
            self.remember_cold(p, span);
        }
        !write || span.writable
    }
}
unsafe fn input(header: usize, stride: usize) -> Option<Input> {
    if !readable(header, 12) { return None; }
    let raw = header as *const u32;
    let data = raw.read_unaligned() as usize;
    let count = raw.add(1).read_unaligned() as usize;
    let cap = raw.add(2).read_unaligned() as usize;
    if count > cap || cap > MAX_OBJECTS || (count > 0 && !readable(data, count.checked_mul(stride)?)) {
        return None;
    }
    Some(Input { data, count, stride })
}
unsafe fn collect(pass: usize, sample_allowed: bool, observation: &mut Option<Observation>) -> Option<Swaps> {
    let render = word(pass, 0x14)? as usize;
    let view = word(render, 0x20d0)? as usize;
    if word(view, 0x340)? != 8 { return None; }
    // Timing starts after the existing mode gate; do not repeat unsafe header reads.
    // Nested views stay inside the parent native interval, not double-counted logs.
    if sample_allowed { *observation = Observation::begin(); }
    let shader = word(word(pass, 8)? as usize, 8 * 4)? as usize;
    if word(shader, 0)? != 0x02c094a4 { return None; }
    let scene = word(render, 0x1c)? as usize;
    let index = word(render, 0x2208)?;
    let (p, s) = match index {
        u32::MAX => (0x70, 0xe8),
        0..=3 => (0xf4 + index as usize * 12, 0x124 + index as usize * 12),
        _ => return None,
    };
    let lists = [input(scene.checked_add(p)?, 4)?, input(scene.checked_add(s)?, 8)?];
    if let Some(o) = observation.as_mut() { o.objects = lists.iter().map(|p| p.count).sum(); }
    let mut swaps = Vec::<Swap>::new();
    // Collection-local address index only; keep Swap insertion/apply order native.
    // Only fully validated selected targets enter it; no cross-view identity cache.
    let mut selected = [0usize; MAX_TARGETS];
    let mut ranges = ReadRanges { observation: observation.as_mut(), ..ReadRanges::default() };
    for list in lists {
        for i in 0..list.count {
            let object = ((list.data + i * list.stride) as *const u32).read_unaligned() as usize;
            let slot = match selected[..swaps.len()].binary_search_by(|p| p.cmp(&object)) {
                Ok(_) => continue,
                Err(slot) => slot,
            };
            if !ranges.contains(object, 0x214) { continue; }
            let owner = ((object + 0x20) as *const u32).read_unaligned() as usize;
            if !ranges.contains(owner, 0x310) { continue; }
            let table = (owner as *const u32).read_unaligned();
            let during = match table { CAMERA => 1, DRONE => 0, _ => continue };
            if ((owner + 0x30c) as *const u32).read_unaligned() as usize != object { continue; }
            let before = (object as *const u8).add(CATEGORY).read();
            if before == during || !ranges.writable(object + CATEGORY, 1) { continue; }
            // Validate the entire bounded set before any write. Allocation failure
            // or excessive targets retains native behavior, never a partial set.
            if swaps.len() == MAX_TARGETS || swaps.try_reserve(1).is_err() { return None; }
            selected.copy_within(slot..swaps.len(), slot + 1);
            selected[slot] = object;
            swaps.push(Swap { object, owner, table,
                serial: ((owner + 0x20) as *const u32).read_unaligned(), before, during });
            if let Some(o) = ranges.observation.as_mut() { o.targets = swaps.len(); }
        }
    }
    if let Some(o) = ranges.observation.as_mut() { o.collection_complete = true; }
    Some(Swaps(swaps))
}

pub(super) unsafe fn with_targets<R>(pass: usize, enabled: bool, native: impl FnOnce() -> R) -> R {
    let parent = ACTIVE.with(Cell::get);
    if !parent.is_null() { (*parent).restore(); }
    struct Scope {
        own: Swaps, parent: *const Swaps, observation: Option<Observation>,
        apply_us: u128, native_start: Option<Instant>, native_returned: bool,
    }
    impl Drop for Scope {
        fn drop(&mut self) { unsafe {
            // Restore on unwind exactly as before. Never clock nonsampled views.
            let native_us = self.native_start.map(|start| start.elapsed().as_micros());
            let restore_start = self.observation.as_ref().map(|_| Instant::now());
            self.own.restore();
            ACTIVE.with(|slot| slot.set(self.parent));
            if !self.parent.is_null() { (*self.parent).apply(); }
            if let Some(o) = self.observation.take() {
                let restore_us = restore_start.expect("sample start").elapsed().as_micros();
                tracing::info!("SCBL_THERMAL_VIEW_COST sample={} mode=8 collect_us={} apply_us={} native_us={} restore_us={} input_entries={} targets_collected={} targets_selected_for_apply={} range_cache_hits={} range_cache_misses={} range_query_rejects={} collection_complete={} native_returned={} collection_clock_starts_after_mode_gate=true range_counts_collection_only=true native_includes_nested=true cpu_wall_not_frame_time=true runtime_acceptance=false",
                    o.sample, o.collect_us, self.apply_us, native_us.unwrap_or(0), restore_us,
                    o.objects, o.targets, self.own.0.len(), o.range_hits, o.range_queries,
                    o.range_rejects, o.collection_complete, self.native_returned);
            }
        } }
    }
    let mut observation = None;
    let own = if enabled { collect(pass, parent.is_null(), &mut observation) } else { None }
        .unwrap_or_else(|| Swaps(Vec::new()));
    if let Some(o) = observation.as_mut() { o.collect_us = o.collect_start.elapsed().as_micros(); }
    let mut frame = Scope {
        own, parent, observation, apply_us: 0, native_start: None, native_returned: false,
    };
    ACTIVE.with(|slot| slot.set(&frame.own));
    let apply_start = frame.observation.as_ref().map(|_| Instant::now());
    frame.own.apply();
    if let Some(start) = apply_start { frame.apply_us = start.elapsed().as_micros(); }
    frame.native_start = frame.observation.as_ref().map(|_| Instant::now());
    let result = native();
    frame.native_returned = true;
    result
}

// The shared native view owner calls this middleware; no second entry detour.
pub(crate) unsafe fn with_ready_targets(pass: usize, native: impl FnOnce()) {
    with_targets(pass, READY.load(Ordering::Acquire), native);
}
pub(super) unsafe fn init(world: usize) {
    if world != 0x589860 || READY.load(Ordering::Acquire) { return; }
    if !signature(0x175b4e0, 5139, "6a00377514208193ee8e399d004556c06754324e1fbf2302a870dbd84736f281")
        || !signature(0x1755d50, 0x992, "48e02c24f1ab417991485eb1f76342b7dcad61b80cfd46ba01f201ccefe439e9")
        || !signature(0x16e1fd0, 0x28, "c361137a4ee273376ba4e83137386fa2f2a1aaa9d3e1540af1fb49d8965182ae")
        || !signature(0x160d080, 168, "c3c4ae7a52c490b16aa3bc710ee9d90163780214eb7c1144f23eeb4c93ce43f2")
        || !signature(0xa6c988, 41, "8ab77d82fe80c9781da08f4d350fb05d797876daa2c132c0be4f5fcfbf689b5d")
        || !signature(0x9d8f42, 9, "a882bd42fdb44edfe6ba80afd00130f10fdc52a2e17afdbac99d7af4f1e8f244")
    { return; }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_THERMAL_TARGETS prepared=true camera=true drone=false scope=single-native-view owner=shared runtime_acceptance=pending");
}
pub(super) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
}
#[cfg(test)]
#[path = "thermal_targets_tests.rs"]
mod tests;

#[cfg(test)]
mod perf_policy_tests {
    #[test]
    fn only_first_three_and_each_three_hundred_are_due() {
        for n in 0..=10000 {
            assert_eq!(super::perf_sample_due(n), n != 0 && (n <= 3 || n % 300 == 0));
        }
        assert!(!super::perf_sample_due(u32::MAX));
    }
}

#[cfg(test)]
#[path = "thermal_read_ranges_tests.rs"]
mod read_ranges_tests;
