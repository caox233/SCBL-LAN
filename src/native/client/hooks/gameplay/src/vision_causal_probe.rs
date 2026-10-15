//! PRIVATE, bounded diagnostic. Full native view owns each observation.
//! Exact call counts; only pseudorandom 1/128 native calls are timed.
use retour::GenericDetour;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
type UploadFn = unsafe extern "C" fn(usize, usize, usize);
type AcquireFn = unsafe extern "C" fn(usize, usize) -> usize;
static UPLOAD: OnceLock<GenericDetour<UploadFn>> = OnceLock::new();
static ACQUIRE: OnceLock<GenericDetour<AcquireFn>> = OnceLock::new();
static ARMED: AtomicBool = AtomicBool::new(false);
static BEGIN: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicU32 = AtomicU32::new(0);
static HZ: OnceLock<u64> = OnceLock::new();
static CLOCK_BASELINE: OnceLock<(u64, u64)> = OnceLock::new();
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
const SECONDS: u64 = 30;
const SAMPLE_MASK: u32 = 127;
#[link(name = "kernel32")]
extern "system" {
    fn QueryPerformanceCounter(v: *mut i64) -> i32;
    fn QueryPerformanceFrequency(v: *mut i64) -> i32;
    fn GetCurrentThreadId() -> u32;
}
#[inline]
fn built() -> bool {
    option_env!("SCBL_VISION_CAUSAL_BUILD") == Some("1")
}
fn clock() -> Option<u64> {
    let mut t = 0i64;
    if unsafe { QueryPerformanceCounter(&mut t) } != 0 && t > 0 {
        Some(t as u64)
    } else {
        None
    }
}
fn us(t: u64, hz: u64) -> u64 {
    ((t as u128 * 1_000_000) / hz.max(1) as u128).min(u64::MAX as u128) as u64
}
fn next_random(x: &mut u32) -> u32 {
    // State persists across views, so periodic material/call order cannot lock
    // the sampler to the same draw every frame. Zero has a nonzero fallback.
    if *x == 0 {
        *x = 0x9e3779b9;
    }
    *x ^= *x << 13;
    *x ^= *x >> 17;
    *x ^= *x << 5;
    *x
}
#[derive(Clone, Copy, Debug, Default)]
struct Native {
    calls: u64,
    samples: u64,
    sample_ticks: u64,
    max_sample_ticks: u64,
}
impl Native {
    fn merge(&mut self, o: Self) {
        self.calls = self.calls.saturating_add(o.calls);
        self.samples = self.samples.saturating_add(o.samples);
        self.sample_ticks = self.sample_ticks.saturating_add(o.sample_ticks);
        self.max_sample_ticks = self.max_sample_ticks.max(o.max_sample_ticks);
    }
    fn sample(&mut self, t: u64) {
        self.samples += 1;
        self.sample_ticks = self.sample_ticks.saturating_add(t);
        self.max_sample_ticks = self.max_sample_ticks.max(t);
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Summary {
    views: u64,
    view_ticks: u64,
    max_view_ticks: u64,
    material_calls: u64,
    native_classify_ticks: u64,
    mod_classify_post_ticks: u64,
    nested_views: u64,
    incomplete_post_segments: u64,
    upload: Native,
    acquire: Native,
}
impl Summary {
    fn merge(&mut self, o: Self) {
        self.views += o.views;
        self.view_ticks = self.view_ticks.saturating_add(o.view_ticks);
        self.max_view_ticks = self.max_view_ticks.max(o.max_view_ticks);
        self.material_calls += o.material_calls;
        self.native_classify_ticks = self
            .native_classify_ticks
            .saturating_add(o.native_classify_ticks);
        self.mod_classify_post_ticks = self
            .mod_classify_post_ticks
            .saturating_add(o.mod_classify_post_ticks);
        self.nested_views += o.nested_views;
        self.incomplete_post_segments += o.incomplete_post_segments;
        self.upload.merge(o.upload);
        self.acquire.merge(o.acquire);
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct ThreadSummary {
    id: u32,
    data: Summary,
}
#[derive(Clone, Copy, Debug, Default)]
struct SlowView {
    thread_id: u32,
    offset_ticks: u64,
    data: Summary,
}
struct Scope {
    start: u64,
    thread_id: u32,
    data: Summary,
    post_start: Option<u64>,
}
struct Tls {
    scope: Option<Scope>,
    random: u32,
}
thread_local! {static TLS:RefCell<Tls>=const{RefCell::new(Tls{scope:None,random:0x243f6a88})};}
struct Session {
    begin: u64,
    hz: u64,
    last_end: u64,
    total: Summary,
    buckets_5s: [Summary; 6],
    threads: [ThreadSummary; 8],
    unlisted_thread_views: u64,
    slowest: [SlowView; 12],
}
impl Session {
    fn new(begin: u64, hz: u64) -> Self {
        Self {
            begin,
            hz,
            last_end: begin,
            total: Summary::default(),
            buckets_5s: [Summary::default(); 6],
            threads: [ThreadSummary::default(); 8],
            unlisted_thread_views: 0,
            slowest: [SlowView::default(); 12],
        }
    }
    fn merge(&mut self, s: Scope, end: u64) {
        self.last_end = self.last_end.max(end);
        let offset = s.start.saturating_sub(self.begin);
        // Bucket is the actual full-view entry time, never a previous classifier.
        let bucket = (offset / self.hz.saturating_mul(5).max(1)).min(5) as usize;
        self.total.merge(s.data);
        self.buckets_5s[bucket].merge(s.data);
        if let Some(t) = self
            .threads
            .iter_mut()
            .find(|t| t.id == s.thread_id || t.id == 0)
        {
            t.id = s.thread_id;
            t.data.merge(s.data);
        } else {
            self.unlisted_thread_views += 1;
        }
        let i = self
            .slowest
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.data.view_ticks)
            .unwrap()
            .0;
        if s.data.view_ticks > self.slowest[i].data.view_ticks {
            self.slowest[i] = SlowView {
                thread_id: s.thread_id,
                offset_ticks: offset,
                data: s.data,
            };
        }
    }
}
fn finish_if_idle() {
    if ARMED.load(Ordering::Acquire) || ACTIVE.load(Ordering::Acquire) != 0 {
        return;
    }
    let completed = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(s) = completed {
        // One summary outside every view scope and outside the aggregate lock.
        tracing::info!("SCBL_EMF_CAUSAL_END revision=2 scope=full_native_view_all_threads qpc_hz={} duration_us={} sample_denominator=128 samples_are_not_all_calls=true tick_fields_are_raw=true sample_ticks_include_qpc_baseline=true empty_qpc_pair_min_median_ticks={:?} nested_views_counted_in_outer_only=true total={:?} buckets_5s={:?} threads={:?} unlisted_thread_views={} slowest={:?} visuals_unchanged=true timing_instrumentation=true",
            s.hz,us(s.last_end.saturating_sub(s.begin),s.hz),CLOCK_BASELINE.get(),s.total,s.buckets_5s,s.threads,s.unlisted_thread_views,s.slowest);
    }
}
fn request_finish() {
    ARMED.store(false, Ordering::Release);
    finish_if_idle();
}
struct ViewGuard;
impl Drop for ViewGuard {
    fn drop(&mut self) {
        let end = clock();
        let scope = TLS.with(|t| t.borrow_mut().scope.take());
        if let Some(mut s) = scope {
            let end = end.unwrap_or(s.start);
            s.data.views = 1;
            s.data.view_ticks = end.saturating_sub(s.start);
            s.data.max_view_ticks = s.data.view_ticks;
            if s.post_start.take().is_some() {
                s.data.incomplete_post_segments += 1;
            }
            {
                let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(session) = session.as_mut() {
                    if session.begin == 0 {
                        session.begin = BEGIN.load(Ordering::Acquire);
                    }
                    session.merge(s, end);
                }
            }
            if end.saturating_sub(BEGIN.load(Ordering::Acquire))
                >= HZ.get().copied().unwrap_or(1).saturating_mul(SECONDS)
            {
                ARMED.store(false, Ordering::Release);
            }
        }
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
        finish_if_idle();
    }
}
unsafe fn enter(pass: usize) -> Option<ViewGuard> {
    if !ARMED.load(Ordering::Acquire) {
        return None;
    }
    // Recursion is deliberately measured as part of the owning outer view.
    if TLS.with(|t| {
        let mut t = t.borrow_mut();
        if let Some(s) = t.scope.as_mut() {
            s.data.nested_views += 1;
            true
        } else {
            false
        }
    }) {
        return None;
    }
    let render = super::word(pass, 0x14)? as usize;
    let view = super::word(render, 0x20d0)? as usize;
    if super::word(view, 0x340) != Some(9) {
        return None;
    }
    let now = clock()?;
    let begin = match BEGIN.compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => now,
        Err(t) => t,
    };
    let hz = *HZ.get().expect("clock before arming");
    if now.saturating_sub(begin) >= hz.saturating_mul(SECONDS) {
        request_finish();
        return None;
    }
    ACTIVE.fetch_add(1, Ordering::AcqRel);
    if !ARMED.load(Ordering::Acquire) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
        finish_if_idle();
        return None;
    }
    let thread_id = GetCurrentThreadId();
    TLS.with(|t| {
        let mut t = t.borrow_mut();
        t.random ^= thread_id.rotate_left(11) ^ now as u32;
        t.scope = Some(Scope {
            start: now,
            thread_id,
            data: Summary::default(),
            post_start: None,
        });
    });
    Some(ViewGuard)
}
// Diagnostic builds use the same physical view owner, never a competing detour.
pub(super) unsafe fn with_view(pass: usize, native: impl FnOnce()) {
    if !built() { native(); return; }
    let guard = enter(pass);
    native();
    drop(guard);
}
pub(super) fn classify_start() -> Option<u64> {
    if built() && TLS.with(|t| t.borrow().scope.is_some()) {
        clock()
    } else {
        None
    }
}
pub(super) unsafe fn classify_end(_pass: usize, start: Option<u64>) {
    if let Some(start) = start {
        if let Some(end) = clock() {
            TLS.with(|t| {
                if let Some(s) = t.borrow_mut().scope.as_mut() {
                    s.data.native_classify_ticks = s
                        .data
                        .native_classify_ticks
                        .saturating_add(end.saturating_sub(start));
                    s.post_start = Some(end);
                }
            });
        }
    }
}
pub(super) fn classify_done() {
    if !built() {
        return;
    }
    let pending = TLS.with(|t| {
        t.borrow()
            .scope
            .as_ref()
            .is_some_and(|s| s.post_start.is_some())
    });
    if pending {
        if let Some(end) = clock() {
            TLS.with(|t| {
                if let Some(s) = t.borrow_mut().scope.as_mut() {
                    if let Some(start) = s.post_start.take() {
                        s.data.mod_classify_post_ticks = s
                            .data
                            .mod_classify_post_ticks
                            .saturating_add(end.saturating_sub(start));
                    }
                }
            });
        }
    }
}
pub(super) fn material_call() {
    if !built() {
        return;
    }
    TLS.with(|t| {
        if let Some(s) = t.borrow_mut().scope.as_mut() {
            s.data.material_calls += 1;
        }
    });
}
fn native_start(kind: u8) -> Option<u64> {
    let selected = TLS.with(|t| {
        let mut t = t.borrow_mut();
        if t.scope.is_none() {
            return false;
        }
        let selected = next_random(&mut t.random) & SAMPLE_MASK == 0;
        let s = t.scope.as_mut().unwrap();
        let m = if kind == 0 {
            &mut s.data.upload
        } else {
            &mut s.data.acquire
        };
        m.calls += 1;
        selected
    });
    if selected {
        clock()
    } else {
        None
    }
}
fn native_end(kind: u8, start: Option<u64>) {
    if let Some(start) = start {
        if let Some(end) = clock() {
            TLS.with(|t| {
                if let Some(s) = t.borrow_mut().scope.as_mut() {
                    let m = if kind == 0 {
                        &mut s.data.upload
                    } else {
                        &mut s.data.acquire
                    };
                    m.sample(end.saturating_sub(start));
                }
            });
        }
    }
}
unsafe extern "C" fn upload(a: usize, b: usize, c: usize) {
    let start = native_start(0);
    UPLOAD.get().expect("published before enable").call(a, b, c);
    native_end(0, start);
}
unsafe extern "C" fn acquire(a: usize, b: usize) -> usize {
    let start = native_start(1);
    let r = ACQUIRE.get().expect("published before enable").call(a, b);
    native_end(1, start);
    r
}
pub(super) unsafe fn init() {
    if !built() || std::env::var("SCBL_VISION_CAUSAL_TRACE").as_deref() != Ok("1") {
        return;
    }
    let mut hz = 0i64;
    if QueryPerformanceFrequency(&mut hz) == 0 || hz <= 0 || HZ.set(hz as u64).is_err() {
        return;
    }
    let mut empty = [0u64; 64];
    for t in &mut empty {
        let a = clock();
        let b = clock();
        *t = match (a, b) {
            (Some(a), Some(b)) => b.saturating_sub(a),
            _ => 0,
        };
    }
    empty.sort_unstable();
    let _ = CLOCK_BASELINE.set((empty[0], empty[32]));
    if !super::signature(
        0x175b4e0,
        0x1413,
        "6a00377514208193ee8e399d004556c06754324e1fbf2302a870dbd84736f281",
    ) || !super::signature(
        0x1601df0,
        0x89,
        "91295e07ad4128579b3761cf5de6909fb20db83d06c9e196fa7bb9c934d728fe",
    ) || !super::signature(
        0x1602230,
        0x109,
        "769c0f6894246d1a8759a3c3eab48b4128cf4bfacb7872e29587e73ce532e47c",
    ) {
        return;
    }
    let a = super::install_direct(
        &UPLOAD,
        std::mem::transmute::<usize, UploadFn>(0x1601df0),
        upload as UploadFn,
    );
    let b = a
        && super::install_direct(
            &ACQUIRE,
            std::mem::transmute::<usize, AcquireFn>(0x1602230),
            acquire as AcquireFn,
        );
    if !b {
        deinit();
        return;
    }
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session::new(0, hz as u64));
    ARMED.store(true, Ordering::Release);
    tracing::info!("SCBL_EMF_CAUSAL_READY revision=2 window_seconds=30 full_view=0x175b4e0 scope=all_threads sample_denominator=128 no_scope_clock_reads=0 native_parameters_unchanged=true");
}
pub(super) unsafe fn deinit() {
    request_finish();
    if let Some(h) = UPLOAD.get() {
        let _ = h.disable();
    }
    if let Some(h) = ACQUIRE.get() {
        let _ = h.disable();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scope(t: u64, thread: u32) -> Scope {
        Scope {
            start: t,
            thread_id: thread,
            data: Summary {
                views: 1,
                view_ticks: 10,
                max_view_ticks: 10,
                ..Summary::default()
            },
            post_start: None,
        }
    }
    #[test]
    fn bucket_uses_actual_view_start() {
        let mut s = Session::new(100, 1000);
        s.merge(scope(100, 1), 110);
        s.merge(scope(15100, 2), 15110);
        assert_eq!(s.buckets_5s[0].views, 1);
        assert_eq!(s.buckets_5s[3].views, 1);
        assert_eq!(s.buckets_5s[1].views, 0);
    }
    #[test]
    fn migrated_threads_all_aggregate() {
        let mut s = Session::new(100, 1000);
        s.merge(scope(100, 7), 110);
        s.merge(scope(200, 9), 210);
        assert_eq!(s.total.views, 2);
        assert_eq!(s.threads[0].id, 7);
        assert_eq!(s.threads[1].id, 9);
    }
    #[test]
    fn sample_values_never_claim_full_timing() {
        let mut m = Native {
            calls: 128,
            ..Native::default()
        };
        m.sample(3);
        assert_eq!((m.calls, m.samples, m.sample_ticks), (128, 1, 3));
    }
    #[test]
    fn submicrosecond_ticks_accumulate() {
        let mut m = Native::default();
        for _ in 0..10 {
            m.sample(3);
        }
        assert_eq!(us(m.sample_ticks, 10_000_000), 3);
    }
    #[test]
    fn pseudorandom_budget() {
        let mut x = 1;
        let count = (0..1_000_000)
            .filter(|_| next_random(&mut x) & 127 == 0)
            .count();
        assert!(count > 6000 && count < 10000, "{count}");
        assert!(count * 2 < 1_000_000 * 2 / 64);
    }
    #[test]
    fn zero_seed_does_not_stick() {
        let mut x = 0;
        assert_ne!(next_random(&mut x), 0);
    }
    #[test]
    fn clock_conversion_saturates() {
        assert_eq!(us(u64::MAX, 1), u64::MAX);
    }
}
