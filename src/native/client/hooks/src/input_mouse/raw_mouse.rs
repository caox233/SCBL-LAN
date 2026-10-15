//! C72 input accounting. No smoothing, sensitivity/FOV/time scaling or global
//! Axis changes. Only the native mouse-look consumption boundary drains this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Identity {
    pub viewport: usize,
    pub window: usize,
    pub pawn: usize,
    pub mode: usize,
}

#[derive(Debug)]
pub(super) struct Accumulator {
    pub identity: Identity,
    pub active: bool,
    pub failed: bool,
    // Absolute input uses native handling until fresh relative motion and two
    // eligible frame boundaries without another absolute report. No replay.
    pub recovering: bool,
    relative_ready: bool,
    quiet_frames: u8,
    absolute_in_frame: bool,
    x: i64,
    y: i64,
    pub reports: u32,
}
impl Accumulator {
    pub const fn new() -> Self {
        Self {
            identity: Identity {
                viewport: 0,
                window: 0,
                pawn: 0,
                mode: 0,
            },
            active: false,
            failed: false,
            recovering: false,
            relative_ready: false,
            quiet_frames: 0,
            absolute_in_frame: false,
            x: 0,
            y: 0,
            reports: 0,
        }
    }
    pub fn clear(&mut self) {
        self.x = 0;
        self.y = 0;
        self.reports = 0;
    }
    pub fn suspend(&mut self) {
        self.active = false;
        self.relative_ready = false;
        self.quiet_frames = 0;
        self.absolute_in_frame = false;
        self.clear();
    }
    pub fn fail(&mut self) {
        self.failed = true;
        self.suspend();
    }
    pub fn add(&mut self, flags: u16, x: i32, y: i32) {
        if self.failed || (!self.active && !self.recovering) {
            return;
        }
        // Absolute coordinates are never injected as relative counts. This is
        // a recoverable device-mode transition, not a corrupt native contract.
        if flags & 1 != 0 {
            self.suspend();
            self.recovering = true;
            self.absolute_in_frame = true;
            return;
        }
        if self.recovering {
            if i64::from(x).abs() > 8_000_000 || i64::from(y).abs() > 8_000_000 {
                self.fail();
            } else if x != 0 || y != 0 {
                self.relative_ready = true;
            }
            return; // Native handles the whole probe packet; never replay it.
        }
        let nx = self.x + i64::from(x);
        let ny = self.y + i64::from(y);
        if nx.abs() > 8_000_000 || ny.abs() > 8_000_000 {
            self.fail();
            return;
        }
        self.x = nx;
        self.y = ny;
        if x != 0 || y != 0 {
            self.reports = self.reports.saturating_add(1);
        }
    }
    pub fn frame(&mut self, identity: Identity, eligible: bool) -> Option<(f32, f32, u32)> {
        if self.failed || !eligible {
            self.suspend();
            self.identity = identity;
            return None;
        }
        if self.recovering {
            if identity != self.identity {
                self.suspend();
                self.identity = identity;
                return None;
            }
            if self.absolute_in_frame {
                self.absolute_in_frame = false;
                self.quiet_frames = 0;
                return None; // The absolute report's own frame is not quiet.
            }
            self.quiet_frames = self.quiet_frames.saturating_add(1);
            if self.relative_ready && self.quiet_frames >= 2 {
                self.recovering = false;
                self.relative_ready = false;
                self.quiet_frames = 0;
                self.clear();
                self.active = true;
            }
            return None; // Rearm only; not even the trigger packet is injected.
        }
        if !self.active || identity != self.identity {
            self.clear();
            self.identity = identity;
            self.active = true;
            return None; // Never replay movement across a focus/pawn/mode boundary.
        }
        let result = (self.x as f32, self.y as f32, self.reports);
        self.clear();
        Some(result)
    }
    pub fn wants_raw(&self, viewport: usize, foreground: bool) -> bool {
        (self.active || self.recovering)
            && !self.failed && foreground && self.identity.viewport == viewport
    }
    pub fn suppress_legacy(&self, viewport: usize, foreground: bool) -> bool {
        self.active && !self.failed && foreground && self.identity.viewport == viewport
    }
}

// GetRawInputData uses the native x86 header (16 bytes), not the distinct
// WOW64 GetRawInputBuffer wire layout. Decode only a verified RAWMOUSE packet.
pub(super) fn decode_x86(bytes: &[u8]) -> Option<(u16, i32, i32)> {
    if bytes.len() < 40 {
        return None;
    }
    let u32_at = |o| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    if u32_at(0) != 0 || u32_at(4) < 40 || u32_at(4) as usize > bytes.len() {
        return None;
    }
    Some((
        u16::from_le_bytes(bytes[16..18].try_into().unwrap()),
        i32::from_le_bytes(bytes[28..32].try_into().unwrap()),
        i32::from_le_bytes(bytes[32..36].try_into().unwrap()),
    ))
}

#[cfg(test)]
#[path = "raw_mouse_tests.rs"]
mod tests;
