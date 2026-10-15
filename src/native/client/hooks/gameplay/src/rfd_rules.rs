//! Presentation-only policy. A real, unprotected RF source always wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Native,
    Interference,
}

pub fn display(pants: bool, protected_source: bool, ordinary_source: bool) -> Display {
    if pants && protected_source && !ordinary_source {
        Display::Interference
    } else {
        Display::Native
    }
}
// D7B160 reads replicated goggles; B04570 reason 3 is the local goggles
// RF flag, not an independent source that bypasses the infiltration helmet.
pub fn ordinary_goggles(active: bool, local_rf_flags: u32, helmet_blocks: bool) -> bool {
    (active || local_rf_flags & 4 != 0) && !helmet_blocks
}
pub fn protected(kind: u32, flags: u32, retained_intel: bool) -> bool {
    match kind {
        1 => flags & 8 != 0 && flags & 32 == 0,
        2 => retained_intel || flags & 8 != 0 && flags & 32 == 0,
        3 => flags & (8 | 32) != 0,
        _ => false,
    }
}
// Presentation only: native range/FOV and distance locals remain untouched.
pub const INTERFERENCE_ALPHA: f32 = 70.0;
/// Independent presentation RNG. Neither distance, source count nor the wave
/// RNG affects the on/off schedule. Times are wall-clock milliseconds.
pub struct Blink {
    seed: u32,
    last: Option<u64>,
    next: u64,
    on: bool,
}
#[cfg(test)]
mod blink_tests {
    use super::*;
    #[test]
    fn random_blink_is_hard_off_bounded_and_not_fixed_double_pulse() {
        let mut b = Blink::new(17);
        let mut phases = Vec::new();
        let mut last = true;
        let mut start = 0;
        for t in 0..20000 {
            let on = b.sample(t, true);
            if on != last {
                let duration = t - start;
                assert!((if last { 200..=600 } else { 60..=180 }).contains(&duration));
                phases.push((last, duration));
                start = t;
                last = on;
            }
        }
        assert!(phases.len() > 40);
        for on in [true, false] {
            let values: std::collections::BTreeSet<_> =
                phases.iter().filter(|p| p.0 == on).map(|p| p.1).collect();
            assert!(values.len() > 10);
        }
        println!("C49_BLINK hard_hidden=true visible_ms=200..600 hidden_ms=60..180 random_phases={} wave_reset=false",phases.len());
    }
    #[test]
    fn random_schedule_is_frame_rate_independent_and_resets_without_signal() {
        let mut reference = Blink::new(123);
        let mut stepped = Blink::new(123);
        for t in 0..5000 {
            let a = reference.sample(t, true);
            if t % 73 == 0 {
                assert_eq!(a, stepped.sample(t, true), "{t}");
            }
        }
        assert!(stepped.sample(6000, false));
        assert!(stepped.sample(6001, true));
        assert!(stepped.sample(10, true));
        assert!(stepped.sample(100000, true));
    }
    #[test]
    fn sweep_keeps_advancing_through_hidden_phases_and_count_cannot_change_blink() {
        let mut reference = Sweep::new(3);
        let mut actual = Sweep::new(3);
        let mut blink = Blink::new(5);
        let mut hidden = 0;
        for now in 0..5000 {
            let signal = actual.sample(now, 2);
            assert_eq!(signal, reference.sample(now, 2));
            if !blink.sample(now, true) {
                hidden += 1;
            }
        }
        assert!(hidden > 500);
    }
}
impl Blink {
    pub const fn new(seed: u32) -> Self {
        Self {
            seed: if seed == 0 { 0x6a09e667 } else { seed },
            last: None,
            next: 0,
            on: true,
        }
    }
    pub fn reset(&mut self) {
        self.last = None;
        self.next = 0;
        self.on = true;
    }
    fn duration(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        if self.on {
            200 + (self.seed % 401) as u64
        } else {
            60 + (self.seed % 121) as u64
        }
    }
    pub fn sample(&mut self, now: u64, eligible: bool) -> bool {
        if !eligible {
            self.reset();
            return true;
        }
        if self
            .last
            .is_some_and(|p| now < p || now.saturating_sub(p) > 5000)
        {
            self.reset();
        }
        if self.last.is_none() {
            self.on = true;
            self.next = now.saturating_add(self.duration());
        } else {
            // Advance all elapsed phases, not one phase per rendered frame.
            while now >= self.next {
                self.on = !self.on;
                self.next = self.next.saturating_add(self.duration());
                if self.next == u64::MAX {
                    break;
                }
            }
        }
        self.last = Some(now);
        self.on
    }
}
pub fn unique_count(identities: impl IntoIterator<Item = (usize, u32)>) -> usize {
    let mut unique = Vec::new();
    for id in identities {
        if id.0 != 0 && !unique.contains(&id) {
            unique.push(id);
        }
    }
    unique.len()
}
pub fn interval(count: usize) -> Option<(u64, u32)> {
    match count {
        0 => None,
        1 => Some((180, 61)),
        2 => Some((140, 51)),
        _ => Some((100, 51)),
    }
}
pub fn period(count: usize) -> u64 {
    match count {
        0 => 0,
        1 => 1400,
        2 => 960,
        _ => 700,
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Signal {
    pub direction: f32,
    pub scale: f32,
    // Zero is reserved for native; 1/2/3 select Small/Medium/Large.
    pub band: u32,
}
#[derive(Clone, Copy)]
pub struct Sweep {
    seed: u32,
    next: u64,
    start: u64,
    from: f32,
    to: f32,
    band: u32,
    count: usize,
    last: Option<u64>,
    phase: f64,
}
impl Sweep {
    pub const fn new(seed: u32) -> Self {
        Self {
            seed: if seed == 0 { 0x91e10da5 } else { seed },
            next: 0,
            start: 0,
            from: 120.0,
            to: 120.0,
            band: 2,
            count: 0,
            last: None,
            phase: 0.0,
        }
    }
    fn random(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }
    fn below(&mut self, bound: u32) -> u32 {
        // Xorshift has 2^32-1 nonzero states; rejection avoids modulo bias.
        let limit = u32::MAX - u32::MAX % bound;
        loop {
            let x = self.random() - 1;
            if x < limit {
                return x % bound;
            }
        }
    }
    fn scale(&self, now: u64) -> f32 {
        let t = (now.saturating_sub(self.start) as f32 / 60.0).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * (t * t * (3.0 - 2.0 * t))
    }
    pub fn reset(&mut self) {
        self.count = 0;
        self.last = None;
        self.next = 0;
        self.phase = 0.0;
    }
    pub fn sample(&mut self, now: u64, count: usize) -> Signal {
        let count = count.min(3);
        let Some((minimum, span)) = interval(count) else {
            self.reset();
            return Signal {
                direction: -1.0,
                scale: 120.0,
                band: 2,
            };
        };
        let first = self.last.is_none();
        if let Some(last) = self.last {
            // Integrate the preceding interval with its previous speed. Switching
            // tiers preserves phase, and frame rate cannot reverse the sweep.
            self.phase =
                (self.phase + now.saturating_sub(last) as f64 / period(self.count) as f64) % 1.0;
        } else {
            self.seed ^= now as u32;
            if self.seed == 0 {
                self.seed = 0x91e10da5;
            }
        }
        self.last = Some(now);
        if count != self.count {
            self.next = now;
        }
        self.count = count;
        if first || now >= self.next {
            self.from = self.scale(now);
            self.band = 1 + self.below(3);
            self.to = 90.0 + self.below(6001) as f32 / 100.0;
            if first {
                self.from = self.to;
            }
            self.start = now;
            self.next = now.saturating_add(minimum + self.below(span) as u64);
        }
        Signal {
            direction: (self.phase * 2.0 - 1.0) as f32,
            scale: self.scale(now),
            band: self.band,
        }
    }
}

/// A clock for an observed cast, never a new activation and never mark-count based.
#[derive(Clone, Copy, Debug)]
pub struct Exposure {
    pub remaining: f32,
    pub held: bool,
}
impl Exposure {
    pub fn new(seconds: f32) -> Option<Self> {
        (seconds.is_finite() && seconds > 0.0 && seconds <= 600.0).then_some(Self {
            remaining: seconds,
            held: false,
        })
    }
    pub fn tick(&mut self, dt: f32, rate: f32, alive: bool) {
        if !alive || !dt.is_finite() || dt < 0.0 || !rate.is_finite() || rate <= 0.0 {
            self.remaining = 0.0;
            self.held = false;
            return;
        }
        self.remaining = (self.remaining - dt * rate).max(0.0);
        if self.remaining == 0.0 {
            self.held = false;
        }
    }
    pub fn disrupt(&mut self) {
        if self.remaining > 0.0 {
            self.held = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_and_remote_goggles_both_respect_infiltration_helmet() {
        for local in [0, 4] {
            for active in [false, true] {
                assert!(!ordinary_goggles(active, local, true));
                assert_eq!(ordinary_goggles(active, local, false), active || local == 4);
            }
        }
    }
    #[test]
    fn normal_goggles_override_pants_even_with_cloak() {
        assert_eq!(display(true, true, true), Display::Native);
        assert_eq!(display(true, true, false), Display::Interference);
        assert_eq!(display(true, false, false), Display::Native);
        assert_eq!(display(false, true, false), Display::Native);
    }
    #[test]
    fn only_three_clothing_windows_are_obfuscated() {
        assert!(protected(1, 9, false));
        assert!(protected(2, 1, true));
        assert!(protected(3, 33, false));
        assert!(protected(3, 9, false));
        assert!(!protected(0, 9, true));
        assert!(!protected(1, 1, false));
        assert!(!protected(2, 1, false));
        assert!(!protected(3, 65, false));
    }
    #[test]
    fn merged_count_deduplicates_pawn_identity_and_has_no_distance_input() {
        assert_eq!(unique_count([(1, 7), (1, 7), (2, 8), (0, 9)]), 2);
        assert_eq!(unique_count([(1, 7); 128]), 1);
        assert_eq!(unique_count([]), 0);
        assert_eq!(INTERFERENCE_ALPHA, 70.0);
        assert_eq!(
            [period(1), period(2), period(3), period(128)],
            [1400, 960, 700, 700]
        );
        assert_eq!(
            [interval(1), interval(2), interval(3), interval(128)],
            [
                Some((180, 61)),
                Some((140, 51)),
                Some((100, 51)),
                Some((100, 51))
            ]
        );
    }
    #[test]
    fn sweep_tiers_random_intervals_and_count_cap() {
        for count in [1, 2, 3, 4, 128] {
            let mut a = Sweep::new(17);
            let (minimum, span) = interval(count).unwrap();
            let mut changes = 0;
            let mut next = 0;
            let mut prev = -1.0;
            let mut wraps = 0;
            for now in 1000..11000 {
                let x = a.sample(now, count);
                assert!((-1.0..=1.0).contains(&x.direction));
                assert!((90.0..=150.0).contains(&x.scale));
                assert!((1..=3).contains(&x.band));
                if x.direction < prev {
                    assert!(prev > 0.99 && x.direction < -0.99);
                    wraps += 1;
                }
                prev = x.direction;
                if a.next != next {
                    changes += 1;
                    assert!((minimum..minimum + span as u64).contains(&(a.next - now)));
                    next = a.next;
                }
            }
            assert_eq!(wraps, 9999 / period(count));
            println!("R55_SWEEP count={count} period_ms={} random_ms={minimum}..{} wraps_10s={wraps} redraws={changes}", period(count), minimum+span as u64-1);
        }
        let mut a = Sweep::new(17);
        let mut b = Sweep::new(17);
        for now in 1000..11000 {
            assert_eq!(a.sample(now, 3), b.sample(now, 128));
        }
    }
    #[test]
    fn sweep_phase_survives_tier_change_and_resets_only_without_sources() {
        let mut a = Sweep::new(17);
        assert_eq!(a.sample(1000, 0).direction, -1.0);
        assert_eq!(a.sample(1000, 1).direction, -1.0);
        let quarter = 1000 + period(1) / 4;
        let before = a.sample(quarter, 1);
        assert_eq!(before.direction, -0.5);
        assert_eq!(a.sample(quarter, 3).direction, before.direction);
        assert!((a.sample(quarter + period(3) / 4, 3).direction).abs() < 0.00001);
        a.sample(quarter + period(3) / 4 + 1, 0);
        assert_eq!(a.sample(9999, 2).direction, -1.0);
    }
    #[test]
    fn sweep_motion_is_frame_rate_independent() {
        let mut a = Sweep::new(0);
        let mut b = Sweep::new(0);
        a.sample(1000, 1);
        b.sample(1000, 1);
        for now in 1001..=1750 {
            a.sample(now, 1);
        }
        let x = a.sample(1750, 1);
        let y = b.sample(1750, 1);
        assert!((x.direction - y.direction).abs() < 0.00001);
        assert!(x.direction > -1.0);
    }
    #[test]
    fn random_band_and_scale_are_independent_of_count_and_not_distance_driven() {
        let mut a = Sweep::new(42);
        let mut buckets = [0usize; 3];
        let mut sums = [0.0f64; 3];
        let mut small = [0; 3];
        let mut large = [0; 3];
        for _ in 0..90000 {
            let band = a.below(3) as usize;
            let scale = 90.0 + a.below(6001) as f64 / 100.0;
            buckets[band] += 1;
            sums[band] += scale;
            if scale < 100.0 {
                small[band] += 1;
            }
            if scale > 140.0 {
                large[band] += 1;
            }
        }
        for i in 0..3 {
            assert!((29000..31000).contains(&buckets[i]));
            assert!((119.0..121.0).contains(&(sums[i] / buckets[i] as f64)));
            assert!(small[i] > 4000 && large[i] > 4000);
        }
        println!("R54_RANDOM independent bands={buckets:?}; every band covers 90..150 scale, no distance/direction input");
    }
    #[test]
    fn random_scale_transition_is_bounded_and_continuous() {
        let mut a = Sweep::new(17);
        a.sample(1000, 1);
        let next = a.next;
        let previous = a.sample(next - 1, 1).scale;
        assert_eq!(a.sample(next, 1).scale, previous);
        let goal = a.to;
        for now in next..=next + 60 {
            assert!((90.0..=150.0).contains(&a.sample(now, 1).scale));
        }
        assert_eq!(a.sample(next + 60, 1).scale, goal);
    }
    #[test]
    fn disruption_keeps_only_remaining_cast_without_refresh_or_mark_requirement() {
        let mut e = Exposure::new(10.0).unwrap();
        e.tick(3.0, 1.0, true);
        e.disrupt();
        assert_eq!(e.remaining, 7.0);
        assert!(e.held);
        e.tick(2.0, 1.0, true);
        e.disrupt();
        assert_eq!(e.remaining, 5.0);
        e.tick(5.0, 1.0, true);
        assert!(!e.held);
        assert_eq!(e.remaining, 0.0);
    }
    #[test]
    fn exposure_death_and_invalid_time_fail_closed() {
        let mut e = Exposure::new(10.0).unwrap();
        e.disrupt();
        e.tick(0.0, 1.0, false);
        assert!(!e.held);
        assert!(Exposure::new(f32::NAN).is_none());
        assert!(Exposure::new(0.0).is_none());
        let mut e = Exposure::new(10.0).unwrap();
        e.disrupt();
        e.tick(f32::NAN, 1.0, true);
        assert!(!e.held);
    }
}
