// Approved cost/validation seam: the actual ReadRanges::contains implementation.
// All addresses belong to this process; no game memory, hooks or synthetic VQ.
use super::*;
use windows::Win32::System::Memory::*;

const PAGE: usize = 0x1000; // Existing i686 Windows native fixtures use 4 KiB pages.
struct Pages { base: *mut std::ffi::c_void }
impl Pages {
    unsafe fn reserve(pages: usize, commit: bool) -> Self {
        let flags = if commit { MEM_RESERVE | MEM_COMMIT } else { MEM_RESERVE };
        let base = VirtualAlloc(None, pages * PAGE, flags, PAGE_READWRITE);
        assert!(!base.is_null(), "VirtualAlloc failed");
        Self { base }
    }
    fn at(&self, page: usize) -> usize { self.base as usize + page * PAGE }
    unsafe fn protect(&self, page: usize, flags: PAGE_PROTECTION_FLAGS) {
        let mut old = PAGE_READWRITE;
        VirtualProtect(self.at(page) as _, PAGE, flags, &mut old).unwrap();
    }
    unsafe fn fragmented(regions: usize) -> Self {
        let p = Self::reserve(regions * 2, true);
        for i in 0..regions { p.protect(i * 2 + 1, PAGE_NOACCESS); }
        p
    }
}
impl Drop for Pages {
    fn drop(&mut self) { unsafe { assert!(VirtualFree(self.base, 0, MEM_RELEASE).is_ok()); } }
}
fn observation() -> Observation {
    Observation { sample: 0, collect_start: Instant::now(), collect_us: 0,
        objects: 0, targets: 0, collection_complete: false,
        range_hits: 0, range_queries: 0, range_rejects: 0 }
}

// The first red-capable slice: a measured OS-query budget, not a fake cache model.
// 32 separate readable OS regions, each used 64 times in a fixed permutation.
// Baseline's four slots issue 2048 queries; the candidate must issue exactly 32.
#[test]
fn read_ranges_fragmented_reuse_queries_each_region_once() {
    unsafe {
        let pages = Pages::fragmented(32);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        for _ in 0..64 {
            for i in 0..32 {
                assert!(ranges.contains(pages.at(((i * 13) % 32) * 2) + 16, 0x310));
            }
        }
        drop(ranges);
        println!("fragmented32 calls=2048 queries={} hits={} rejects={}",
            o.range_queries, o.range_hits, o.range_rejects);
        assert_eq!(o.range_queries, 32, "reused valid regions must not repeatedly invoke VQ");
        assert_eq!(o.range_hits, 2016);
        assert_eq!(o.range_rejects, 0);
    }
}

#[test]
fn read_ranges_rejects_guard_noaccess_uncommitted_and_overflow() {
    unsafe {
        let p = Pages::reserve(4, true);
        p.protect(1, PAGE_GUARD | PAGE_READWRITE);
        p.protect(2, PAGE_NOACCESS);
        let reserved = Pages::reserve(1, false);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        assert!(!ranges.contains(0, 1));
        assert!(!ranges.contains(0xffff, 1));
        assert!(!ranges.contains(p.at(0), 0));
        assert!(!ranges.contains(usize::MAX - 4, 16));
        assert!(!ranges.contains(p.at(0), usize::MAX));
        drop(ranges);
        assert_eq!(o.range_queries, 0, "prechecks must not call the OS");
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        assert!(ranges.contains(p.at(0), PAGE));
        assert!(ranges.contains(p.at(0) + PAGE - 1, 1));
        assert!(!ranges.contains(p.at(0) + PAGE - 1, 2));
        for _ in 0..2 {
            assert!(!ranges.contains(p.at(1), 1));
            assert!(!ranges.contains(p.at(2), 1));
            assert!(!ranges.contains(reserved.at(0), 1));
        }
        assert!(ranges.contains(p.at(3), PAGE));
        drop(ranges);
        assert_eq!(o.range_queries, 9);
        assert_eq!(o.range_rejects, 7, "rejected regions must not become positive entries");
        let mut m = MEMORY_BASIC_INFORMATION::default();
        assert_ne!(VirtualQuery(Some(p.at(1) as _), &mut m, std::mem::size_of_val(&m)), 0);
        assert_ne!(m.Protect.0 & PAGE_GUARD.0, 0, "validation must not consume a guard");
    }
}

#[test]
fn read_ranges_does_not_union_adjacent_readable_regions() {
    unsafe {
        let p = Pages::reserve(10, true);
        for i in [1, 3, 5, 7, 9] { p.protect(i, PAGE_READONLY); }
        let mut ranges = ReadRanges::default();
        // Fill more than four slots so both the hot and indexed paths are tested.
        for i in 0..10 { assert!(ranges.contains(p.at(i), PAGE)); }
        assert!(!ranges.contains(p.at(0) + PAGE - 16, 32));
        assert!(!ranges.contains(p.at(1) + PAGE - 16, 32));
        assert!(ranges.contains(p.at(0), PAGE));
        assert!(ranges.contains(p.at(1), PAGE));
    }
}

#[test]
fn read_ranges_full_index_retains_four_region_tail_locality_and_validation() {
    unsafe {
        let p = Pages::fragmented(260);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        for i in 0..256 { assert!(ranges.contains(p.at(i * 2), 0x214)); }
        for _ in 0..256 {
            for i in 256..260 { assert!(ranges.contains(p.at(i * 2) + 8, 0x310)); }
        }
        drop(ranges);
        assert_eq!(o.range_queries, 260, "full secondary index must not displace hot-tail support");
        assert_eq!(o.range_hits, 1020);
        let mut ranges = ReadRanges::default();
        for i in (0..260).rev() {
            assert!(ranges.contains(p.at(i * 2), PAGE));
            assert!(!ranges.contains(p.at(i * 2) + PAGE - 1, 2));
            assert!(!ranges.contains(p.at(i * 2 + 1), 1));
        }
    }
}

#[test]
fn read_ranges_new_collection_requeries_after_protection_change() {
    unsafe {
        let p = Pages::reserve(1, true);
        let mut old = ReadRanges::default();
        assert!(old.contains(p.at(0), 0x310));
        drop(old); // No protection changes while a collection cache is live.
        p.protect(0, PAGE_NOACCESS);
        let mut o = observation();
        let mut next = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        assert!(!next.contains(p.at(0), 0x310));
        drop(next);
        assert_eq!(o.range_queries, 1);
        assert_eq!(o.range_hits, 0);
        assert_eq!(o.range_rejects, 1);
    }
}

unsafe fn cost_trial(p: &Pages, regions: usize, cycles: usize, sampled: bool) -> (u128, u64, u64) {
    // Allocation/protection are outside timing. Construction of a fresh cache is
    // inside timing, including its larger zero-initialization in the candidate.
    let mut o = observation();
    let start = Instant::now();
    for _ in 0..64 {
        let mut ranges = if sampled {
            ReadRanges { observation: Some(&mut o), ..ReadRanges::default() }
        } else { ReadRanges::default() };
        for cycle in 0..cycles {
            for i in 0..regions {
                let region = (i * 13) % regions; // Permutation for power-of-two counts.
                let offset = (cycle & 3) * 16;
                assert!(std::hint::black_box(ranges.contains(
                    std::hint::black_box(p.at(region * 2) + offset), 0x310)));
            }
        }
    }
    (start.elapsed().as_nanos(), o.range_queries, o.range_hits)
}

#[test]
#[ignore = "root-run release cost observation; not a flaky wall-time assertion or FPS proof"]
fn read_ranges_cost_matrix_observe() {
    unsafe {
        for (name, regions, cycles) in [
            ("hot1", 1, 4096), ("hot4", 4, 1024), ("reuse32", 32, 128),
            ("reuse128", 128, 32), ("cold512", 512, 1), ("over_capacity512", 512, 8),
        ] {
            let p = Pages::fragmented(regions);
            let mut plain = Vec::new();
            let mut sampled = Vec::new();
            let mut queries = 0;
            let mut hits = 0;
            for _ in 0..7 {
                plain.push(cost_trial(&p, regions, cycles, false).0);
                let result = cost_trial(&p, regions, cycles, true);
                sampled.push(result.0); queries = result.1; hits = result.2;
            }
            plain.sort_unstable(); sampled.sort_unstable();
            println!("READ_RANGES_COST case={name} draws=64 calls_per_draw={} plain_median_ns={} sampled_median_ns={} queries={} hits={} cache_bytes={} actual_windows_memory=true frame_time=false",
                regions * cycles, plain[3], sampled[3], queries, hits,
                std::mem::size_of::<ReadRanges<'static>>());
        }
    }
}

// C6 follow-up: actual ReadRanges, real owned Windows regions; no game or GPU.
#[test]
fn read_ranges_full_index_reuses_eight_spill_regions() {
    unsafe {
        let p = Pages::fragmented(136);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        for i in 0..128 { assert!(ranges.contains(p.at(i * 2) + 16, 0x310)); }
        for _ in 0..64 {
            for i in 128..136 { assert!(ranges.contains(p.at(i * 2) + 16, 0x310)); }
        }
        drop(ranges);
        println!("SPILL_COUNTS calls=640 queries={} hits={} rejects={} regions=136 tail=8 cycles=64",
            o.range_queries, o.range_hits, o.range_rejects);
        assert_eq!(o.range_queries, 136, "a repeatedly reused eight-region tail must not query the OS again");
        assert_eq!(o.range_hits, 504);
        assert_eq!(o.range_rejects, 0);
    }
}

#[test]
fn read_ranges_spill_does_not_evict_original_index_or_hot_four() {
    unsafe {
        let p = Pages::fragmented(140);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        for i in 0..128 { assert!(ranges.contains(p.at(i * 2), 0x214)); }
        // More than eight spill candidates exercises bounded replacement.
        for i in 128..140 { assert!(ranges.contains(p.at(i * 2), 0x214)); }
        let before = ranges.observation.as_ref().unwrap().range_queries;
        for _ in 0..8 {
            for i in 136..140 { assert!(ranges.contains(p.at(i * 2) + 8, 0x310)); }
        }
        // Neither spill storage nor its replacement may displace the first 128.
        for i in (0..128).rev() { assert!(ranges.contains(p.at(i * 2), PAGE)); }
        assert_eq!(ranges.observation.as_ref().unwrap().range_queries, before);
        drop(ranges);
        assert_eq!(o.range_queries, 140);
        assert_eq!(o.range_hits, 160);
        assert_eq!(o.range_rejects, 0);
    }
}

#[test]
fn read_ranges_spill_saturation_preserves_rejection_boundaries() {
    unsafe {
        let p = Pages::fragmented(140);
        let guard = Pages::reserve(1, true);
        guard.protect(0, PAGE_GUARD | PAGE_READWRITE);
        let reserved = Pages::reserve(1, false);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        for i in 0..128 { assert!(ranges.contains(p.at(i * 2), PAGE)); }
        // Nine distinct tail regions exceeds the new spill capacity. Validation
        // must stay correct even when the performance policy cannot retain them.
        for _ in 0..3 {
            for i in 128..137 { assert!(ranges.contains(p.at(i * 2), PAGE)); }
        }
        let before = ranges.observation.as_ref().unwrap().range_queries;
        let rejected_before = ranges.observation.as_ref().unwrap().range_rejects;
        for _ in 0..2 {
            assert!(!ranges.contains(p.at(136 * 2) + PAGE - 1, 2));
            assert!(!ranges.contains(p.at(136 * 2 + 1), 1));
            assert!(!ranges.contains(guard.at(0), 1));
            assert!(!ranges.contains(reserved.at(0), 1));
        }
        assert_eq!(ranges.observation.as_ref().unwrap().range_queries - before, 8);
        assert_eq!(ranges.observation.as_ref().unwrap().range_rejects - rejected_before, 8);
        let after = ranges.observation.as_ref().unwrap().range_queries;
        assert!(!ranges.contains(usize::MAX - 3, 8));
        assert!(!ranges.contains(0xffff, 1));
        assert_eq!(ranges.observation.as_ref().unwrap().range_queries, after);
        drop(ranges);
        let mut m = MEMORY_BASIC_INFORMATION::default();
        assert_ne!(VirtualQuery(Some(guard.at(0) as _), &mut m, std::mem::size_of_val(&m)), 0);
        assert_ne!(m.Protect.0 & PAGE_GUARD.0, 0);
    }
}

unsafe fn spill_cost_trial(p: &Pages, sampled: bool) -> (u128, u64, u64) {
    let mut o = observation();
    let start = Instant::now();
    for _ in 0..64 {
        let mut ranges = if sampled {
            ReadRanges { observation: Some(&mut o), ..ReadRanges::default() }
        } else { ReadRanges::default() };
        for i in 0..128 {
            assert!(std::hint::black_box(ranges.contains(std::hint::black_box(p.at(i * 2) + 16), 0x310)));
        }
        for _ in 0..64 {
            for i in 128..136 {
                assert!(std::hint::black_box(ranges.contains(std::hint::black_box(p.at(i * 2) + 16), 0x310)));
            }
        }
    }
    (start.elapsed().as_nanos(), o.range_queries, o.range_hits)
}

#[test]
#[ignore = "root-run owned cost observation; no wall-time pass threshold and no FPS proof"]
fn read_ranges_spill_cost_observe() {
    unsafe {
        let p = Pages::fragmented(136);
        let (mut plain, mut sampled) = (Vec::new(), Vec::new());
        let (mut queries, mut hits) = (0, 0);
        for _ in 0..7 {
            plain.push(spill_cost_trial(&p, false).0);
            let result = spill_cost_trial(&p, true);
            sampled.push(result.0); queries = result.1; hits = result.2;
        }
        plain.sort_unstable(); sampled.sort_unstable();
        println!("SPILL_COST case=warm128_tail8 draws=64 calls_per_draw=640 plain_median_ns={} sampled_median_ns={} queries={} hits={} cache_bytes={} actual_windows_memory=true frame_time=false",
            plain[3], sampled[3], queries, hits, std::mem::size_of::<ReadRanges<'static>>());
    }
}


// C14 cold-flow regression: real ReadRanges + owned VirtualQuery regions.
// Cold readable non-targets are revisited by multiple scene lists. A bounded
// positive index must reduce their repeated OS queries, without actor caching.
#[test]
fn read_ranges_cold_512_reuse_has_bounded_query_cost() {
    unsafe {
        let p = Pages::fragmented(512);
        let mut o = observation();
        let mut ranges = ReadRanges { observation: Some(&mut o), ..ReadRanges::default() };
        for _ in 0..4 {
            for i in 0..512 {
                assert!(ranges.contains(p.at(((i * 13) % 512) * 2) + 16, 0x310));
            }
        }
        drop(ranges);
        println!("C14_COLD512 calls=2048 queries={} hits={} rejects={}",
            o.range_queries, o.range_hits, o.range_rejects);
        assert!(o.range_queries <= 640,
            "C14_COLD_REPEAT_RED queries={} ceiling=640", o.range_queries);
        assert_eq!(o.range_queries + o.range_hits, 2048);
        assert_eq!(o.range_rejects, 0);
    }
}

#[test]
fn read_ranges_cold_index_keeps_exact_extents_and_write_permissions() {
    unsafe {
        let p = Pages::fragmented(600);
        p.protect(500 * 2, PAGE_READONLY);
        let guard = Pages::reserve(1, true);
        guard.protect(0, PAGE_GUARD | PAGE_READWRITE);
        let mut ranges = ReadRanges::default();
        for i in 0..600 { assert!(ranges.contains(p.at(i * 2), PAGE)); }
        // Neither a bucket collision nor a hit may turn read-only into writable,
        // merge into the neighboring noaccess page, or consume a guard.
        for _ in 0..2 {
            assert!(ranges.contains(p.at(500 * 2) + 16, 0x310));
            assert!(!ranges.writable(p.at(500 * 2) + CATEGORY, 1));
            assert!(ranges.writable(p.at(499 * 2) + CATEGORY, 1));
            assert!(!ranges.contains(p.at(500 * 2) + PAGE - 1, 2));
            assert!(!ranges.contains(p.at(500 * 2 + 1), 1));
            assert!(!ranges.contains(guard.at(0), 1));
        }
        drop(ranges);
        let mut m = MEMORY_BASIC_INFORMATION::default();
        assert_ne!(VirtualQuery(Some(guard.at(0) as _), &mut m, std::mem::size_of_val(&m)), 0);
        assert_ne!(m.Protect.0 & PAGE_GUARD.0, 0);
        p.protect(500 * 2, PAGE_NOACCESS);
        let mut fresh = ReadRanges::default();
        assert!(!fresh.contains(p.at(500 * 2), 1), "next operation must rebuild permissions");
    }
}

#[test]
fn read_ranges_cold_saturation_never_uses_unrelated_region_proof() {
    unsafe {
        let p = Pages::fragmented(2048);
        let mut ranges = ReadRanges::default();
        for i in 0..2048 { assert!(ranges.contains(p.at(i * 2), PAGE)); }
        for i in (0..2048).rev() {
            assert!(ranges.writable(p.at(i * 2) + CATEGORY, 1));
            assert!(!ranges.contains(p.at(i * 2) + PAGE - 1, 2));
            assert!(!ranges.contains(p.at(i * 2 + 1), 1));
        }
    }
}

#[test]
#[ignore = "owned unique-region worst-case cost; not game/FPS proof or a wall-time gate"]
fn read_ranges_cold_unique_cost_matrix_observe() {
    unsafe {
        for regions in [6, 32, 128, 512, 2048] {
            let p = Pages::fragmented(regions);
            let (mut plain, mut sampled) = (Vec::new(), Vec::new());
            let (mut queries, mut hits) = (0, 0);
            for _ in 0..9 {
                plain.push(cost_trial(&p, regions, 1, false).0);
                let result = cost_trial(&p, regions, 1, true);
                sampled.push(result.0); queries = result.1; hits = result.2;
            }
            plain.sort_unstable(); sampled.sort_unstable();
            println!("C14_UNIQUE_COST regions={regions} calls_per_draw={regions} draws=64 plain_median_ns={} sampled_median_ns={} queries={} hits={} inline_cache_bytes={} actual_windows_memory=true frame_time=false",
                plain[4], sampled[4], queries, hits, std::mem::size_of::<ReadRanges<'static>>());
        }
    }
}
