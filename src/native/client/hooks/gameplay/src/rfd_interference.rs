//! Local RFD presentation receipt. No gameplay marks or remote notification.
use crate::goggle_exposure::{Language, Presentation};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub life: (usize, usize, u64),
    pub now: u64,
    pub enabled: bool,
    pub fake: bool,
}
static CURRENT: Mutex<Option<Receipt>> = Mutex::new(None);
pub(crate) fn publish(r: Option<Receipt>) {
    if let Ok(mut s) = CURRENT.lock() {
        *s = r;
    }
}
pub(crate) fn receipt(life: (usize, usize, u64), now: u64) -> Option<Receipt> {
    let r = (*CURRENT.lock().ok()?)?;
    (r.life == life && now.checked_sub(r.now).is_some_and(|age| age <= 250)).then_some(r)
}
pub(crate) fn text(language: Language) -> Vec<u16> {
    match language {
        Language::English => "INTERFERENCE",
        Language::TraditionalChinese => "被干擾",
    }
    .encode_utf16()
    .chain([0])
    .collect()
}

#[derive(Default)]
pub(crate) struct Warning {
    life: Option<(usize, usize, u64)>,
    last: Option<u64>,
    detected: Option<u64>,
}
impl Warning {
    pub(crate) fn update(
        &mut self,
        life: (usize, usize, u64),
        now: u64,
        alive: bool,
        marked: Option<bool>,
        r: Option<Receipt>,
    ) -> Presentation {
        if self.life != Some(life) || self.last.is_some_and(|p| now < p) {
            self.detected = None;
        }
        self.life = Some(life);
        self.last = Some(now);
        if marked != Some(false) {
            self.detected = None;
            return Presentation::Native;
        }
        let r =
            r.filter(|r| r.life == life && now.checked_sub(r.now).is_some_and(|age| age <= 250));
        if !alive || life.0 == 0 || life.1 == 0 || life.2 == 0 || !r.is_some_and(|r| r.enabled) {
            self.detected = None;
            return Presentation::Hidden;
        }
        if r.is_some_and(|r| r.fake) {
            self.detected = Some(now);
        }
        if self.detected.is_some_and(|t| now.saturating_sub(t) < 2000) {
            Presentation::Exposed // shared animation state; label is selected separately
        } else {
            self.detected = None;
            Presentation::Hidden
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_receipt_not_equipment_or_blink_drives_stable_warning() {
        let life = (1, 2, 3);
        let mut w = Warning::default();
        let at = |now, fake| {
            Some(Receipt {
                life,
                now,
                enabled: true,
                fake,
            })
        };
        assert_eq!(
            w.update(life, 0, true, Some(false), at(0, false)),
            Presentation::Hidden
        );
        for now in [0, 70, 500, 570, 670, 740, 8000] {
            assert_eq!(
                w.update(life, now, true, Some(false), at(now, true)),
                Presentation::Exposed
            );
        }
        assert_eq!(
            w.update(life, 9999, true, Some(false), at(9999, false)),
            Presentation::Exposed
        );
        assert_eq!(
            w.update(life, 10000, true, Some(false), at(10000, false)),
            Presentation::Hidden
        );
        println!("C49_INTERFERENCE native_fake=true label_stable=true loss_hold_ms=2000 equipped_only=false");
    }
    #[test]
    fn stale_cross_life_disabled_dead_and_native_priority_never_show_fake_warning() {
        let life = (1, 2, 3);
        let mut w = Warning::default();
        let r = Receipt {
            life,
            now: 100,
            enabled: true,
            fake: true,
        };
        for (l, n, a, m, r) in [
            (life, 99, true, Some(false), Some(r)),
            (life, 351, true, Some(false), Some(r)),
            ((1, 2, 4), 100, true, Some(false), Some(r)),
            (life, 100, false, Some(false), Some(r)),
            (
                life,
                100,
                true,
                Some(false),
                Some(Receipt {
                    enabled: false,
                    ..r
                }),
            ),
            (life, 100, true, Some(false), None),
        ] {
            assert_eq!(w.update(l, n, a, m, r), Presentation::Hidden);
        }
        for marked in [None, Some(true)] {
            assert_eq!(
                w.update(life, 100, true, marked, Some(r)),
                Presentation::Native
            );
        }
    }
    #[test]
    fn language_is_exact_native_utf16() {
        assert_eq!(
            String::from_utf16_lossy(&text(Language::TraditionalChinese)),
            "被干擾\0"
        );
        assert_eq!(
            String::from_utf16_lossy(&text(Language::English)),
            "INTERFERENCE\0"
        );
    }
}
