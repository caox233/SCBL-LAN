//! Bounded observation only: never calls aim, camera, damage or HUD setters.
//! The context pixel input is not a world point or proof of the final rendered
//! dot. D4CB80 may select a viewport-centre branch before 1A61D50 transforms it.
use crate::equipment_native::{float, word};
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU32, Ordering},
};
const MAX_RECORDS: u32 = 120;
static RECORDS: AtomicU32 = AtomicU32::new(0);
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    native_visible: u8,
    kind: i32,
    pixel: [f32; 3],
}
#[derive(Default)]
struct Gate {
    last: Option<Sample>,
    at: u64,
}
impl Gate {
    fn accept(&mut self, s: Sample, now: u64) -> bool {
        if s.pixel.iter().any(|v| !v.is_finite() || v.abs() > 100000.0) {
            return false;
        }
        let transition = self
            .last
            .map(|old| (old.native_visible, old.kind) != (s.native_visible, s.kind))
            .unwrap_or(true);
        if !transition && now.saturating_sub(self.at) < 1000 {
            return false;
        }
        if self.last == Some(s) {
            return false;
        }
        self.last = Some(s);
        self.at = now;
        true
    }
}
thread_local! { static GATE: RefCell<Gate> = RefCell::new(Gate::default()); }
pub(crate) unsafe fn observe(component: usize, native_visible: u8, kind: i32, now: u64) {
    if RECORDS.load(Ordering::Relaxed) >= MAX_RECORDS {
        return;
    }
    let Some(context) = word(component, 0x61c).map(|v| v as usize) else {
        return;
    };
    let (Some(x), Some(y), Some(z)) = (
        float(context, 0xbcc),
        float(context, 0xbd0),
        float(context, 0xbd4),
    ) else {
        return;
    };
    let s = Sample {
        native_visible,
        kind,
        pixel: [x, y, z],
    };
    if !GATE.with(|g| g.borrow_mut().accept(s, now)) {
        return;
    }
    if RECORDS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < MAX_RECORDS).then_some(n + 1)
        })
        .is_err()
    {
        return;
    }
    crate::host_log(hooks_plugin_api::LOG_INFO,&format!(
      "SCBL_AIM_TRANSITION native_visible={native_visible} native_kind={kind} context_pixel=({x:.2},{y:.2},{z:.2}) source=context-before-native-projection mutates_aim=false revision=44"));
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(visible: u8, kind: i32, x: f32) -> Sample {
        Sample {
            native_visible: visible,
            kind,
            pixel: [x, 500.0, 0.0],
        }
    }
    #[test]
    fn transitions_are_observed_without_a_mouse_button() {
        let mut g = Gate::default();
        assert!(g.accept(sample(0, 3, 900.0), 1));
        assert!(g.accept(sample(1, 5, 850.0), 2));
        assert!(g.accept(sample(0, 3, 900.0), 3));
    }
    #[test]
    fn same_state_motion_is_rate_limited_and_identical_frames_are_deduplicated() {
        let mut g = Gate::default();
        assert!(g.accept(sample(0, 3, 900.0), 1));
        assert!(!g.accept(sample(0, 3, 901.0), 2));
        assert!(g.accept(sample(0, 3, 901.0), 1001));
        assert!(!g.accept(sample(0, 3, 901.0), 9000));
    }
    #[test]
    fn corrupt_coordinates_are_not_emitted() {
        for x in [f32::NAN, f32::INFINITY, 100001.0] {
            let mut g = Gate::default();
            assert!(!g.accept(sample(0, 3, x), 10));
            assert!(g.accept(sample(0, 3, 100.0), 11));
        }
    }
}
