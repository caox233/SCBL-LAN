//! Presentation policy for actual goggle-detection results, not a detector.
//! This module is test-only until the native detector-to-target bridge is proven.
//! Do not feed proximity, equipped goggles or the native tagged bit as detections.
//! No pawn/mark fields, packets, native timers, or other HUD state are written.

pub(crate) const RELEASE_HOLD_MS: u64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Rfd,
    Ats,
    Bms,
}

/// Complete results for this frame. A transport adapter must expire missing
/// reports before constructing this value. The policy does not invent leases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ActualDetections(u8);
impl ActualDetections {
    pub(crate) fn record(&mut self, source: Source, detected: bool) {
        let bit = 1 << source as u8;
        if detected {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
    fn any(self) -> bool {
        self.0 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Life {
    pub scene: u64,
    pub spawn: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub life: Life,
    pub now_ms: u64,
    pub dx11_mod: bool,
    pub local_alive_spy: bool,
    pub tactical_goggles_on: bool,
    /// Unknown is not "unmarked": never cover an unreadable native warning.
    pub native_marked: Option<bool>,
    pub detections: ActualDetections,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presentation {
    /// Leave the retail warning alone (includes unknown native state).
    Native,
    Hidden,
    Exposed,
}

#[derive(Default)]
pub(crate) struct Warning {
    life: Option<Life>,
    last_ms: Option<u64>,
    last_detected_ms: Option<u64>,
}

impl Warning {
    pub(crate) fn update(&mut self, f: Frame) -> Presentation {
        // A presentation hold may not survive another player life/scene or a
        // clock reset. No pointers are retained by this state machine.
        if self.life != Some(f.life) || self.last_ms.is_some_and(|last| f.now_ms < last) {
            self.last_detected_ms = None;
        }
        self.life = Some(f.life);
        self.last_ms = Some(f.now_ms);
        if f.native_marked != Some(false) {
            self.last_detected_ms = None;
            return Presentation::Native;
        }
        if !f.dx11_mod
            || !f.local_alive_spy
            || !f.tactical_goggles_on
            || f.life.scene == 0
            || f.life.spawn == 0
        {
            self.last_detected_ms = None;
            return Presentation::Hidden;
        }
        if f.detections.any() {
            self.last_detected_ms = Some(f.now_ms);
            return Presentation::Exposed;
        }
        // Presentation-only release hold: every confirmed detection renews it.
        // Missing detections never renew it, and the native detector/mark is
        // not extended. A short silent movement stays in one UI/sound episode.
        if self
            .last_detected_ms
            .is_some_and(|last| f.now_ms.saturating_sub(last) < RELEASE_HOLD_MS)
        {
            Presentation::Exposed
        } else {
            self.last_detected_ms = None;
            Presentation::Hidden
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Language {
    English,
    TraditionalChinese,
}
pub(crate) fn text(language: Language) -> Vec<u16> {
    // Native 18BF320 consumes UTF-16, not an ANSI string or a UTF-8 pointer.
    match language {
        Language::English => "EXPOSED",
        Language::TraditionalChinese => "已暴露",
    }
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect()
}

#[cfg(test)]
#[path = "goggle_exposure_tests.rs"]
mod tests;
