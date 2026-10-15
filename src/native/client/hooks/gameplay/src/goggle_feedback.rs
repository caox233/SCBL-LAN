//! Private native detector feedback. Body IDs are never sender authentication.
//! Current native envelope From + a bounded live roster are mandatory.
use crate::ats_marks::Role;
use crate::goggle_exposure::{ActualDetections, Source};
pub(crate) const PREFIX: &str = "~SCBLX1:";
pub(crate) const MAX_TARGETS: usize = 8;
pub(crate) const LEASE_MS: u64 = 400;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Person {
    pub actor: u64,
    pub peer: u32,
    pub team: u32,
    pub role: Role,
}
#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub scope: [u64; 4],
    pub match_key: u64,
    pub now: u64,
    pub local: u64,
    pub people: Vec<Person>,
    // Complete, bounded IDs copied from the current native session peer list.
    // Unlike people, this includes connected peers whose actor proxy is pending.
    pub session_peers: Vec<u32>,
}
// Logical match identity is separate from reusable native object addresses.
// All private feedback caches use this key; wire IDs and sequence policy stay native.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CacheScope {
    native: [u64; 4],
    match_key: u64,
}
impl Context {
    pub(crate) fn cache_scope(&self) -> CacheScope {
        CacheScope { native: self.scope, match_key: self.match_key }
    }
    pub fn valid(&self) -> bool {
        self.match_key != 0
            && self.local != 0
            && self.people.len() <= 32
            && self.session_peers.len() <= 32
            && self.session_peers.iter().enumerate().all(|(i, peer)| {
                *peer != u32::MAX && !self.session_peers[..i].contains(peer)
            })
            && self.scope.iter().all(|v| *v != 0)
            && self.people.iter().filter(|p| p.actor == self.local).count() == 1
            && self.people.iter().enumerate().all(|(i, p)| {
                p.actor != 0
                    && p.peer != u32::MAX
                    && self.session_peers.contains(&p.peer)
                    && p.team < 255
                    && !self.people[..i]
                        .iter()
                        .any(|q| q.actor == p.actor || q.peer == p.peer)
            })
    }
    pub fn person(&self, id: u64) -> Option<&Person> {
        self.people.iter().find(|p| p.actor == id)
    }
    fn is_spy_opponent(&self, actor: u64, source_team: u32) -> bool {
        self.person(actor)
            .is_some_and(|p| p.role == Role::Spy && p.team != source_team)
    }
    pub fn source(&self, actor: u64, from: u32) -> Option<&Person> {
        if !self.valid() {
            return None;
        }
        self.person(actor)
            .filter(|p| p.peer == from && p.role == Role::Merc)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Report {
    pub match_key: u64,
    pub actor: u64,
    pub seq: u32,
    pub detector: Source,
    pub targets: Vec<u64>,
}
impl Report {
    fn valid(&self) -> bool {
        self.match_key != 0
            && self.actor != 0
            && self.targets.len() <= MAX_TARGETS
            && self
                .targets
                .iter()
                .enumerate()
                .all(|(i, t)| *t != 0 && !self.targets[..i].contains(t))
    }
    pub fn encode(&self) -> Option<Vec<u16>> {
        if !self.valid() {
            return None;
        }
        let mut raw = Vec::with_capacity(86);
        raw.extend(self.match_key.to_le_bytes());
        raw.extend(self.actor.to_le_bytes());
        raw.extend(self.seq.to_le_bytes());
        raw.push(self.detector as u8);
        raw.push(self.targets.len() as u8);
        for t in &self.targets {
            raw.extend(t.to_le_bytes());
        }
        let mut out: Vec<u16> = PREFIX.encode_utf16().collect();
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut bits = 0u32;
        let mut count = 0;
        for byte in raw {
            bits = (bits << 8) | byte as u32;
            count += 8;
            while count >= 6 {
                count -= 6;
                out.push(TABLE[((bits >> count) & 63) as usize] as u16);
            }
        }
        if count > 0 {
            out.push(TABLE[((bits << (6 - count)) & 63) as usize] as u16);
        }
        (out.len() <= 144).then_some(out)
    }
    pub fn decode(text: &[u16]) -> Option<Self> {
        if text.len() > 144
            || text.len() < PREFIX.len() + 30
            || !text.starts_with(&PREFIX.encode_utf16().collect::<Vec<_>>())
        {
            return None;
        }
        let mut raw = Vec::with_capacity(86);
        let mut bits = 0u32;
        let mut count = 0;
        for &c in &text[PREFIX.len()..] {
            let v = match c {
                65..=90 => c - 65,
                97..=122 => c - 97 + 26,
                48..=57 => c - 48 + 52,
                45 => 62,
                95 => 63,
                _ => return None,
            };
            bits = (bits << 6) | v as u32;
            count += 6;
            if count >= 8 {
                count -= 8;
                raw.push((bits >> count) as u8);
            }
        }
        if raw.len() < 22 || raw.len() > 86 || bits & ((1 << count) - 1) != 0 {
            return None;
        }
        let n = raw[21] as usize;
        if n > MAX_TARGETS || raw.len() != 22 + n * 8 {
            return None;
        }
        let detector = match raw[20] {
            0 => Source::Rfd,
            1 => Source::Ats,
            2 => Source::Bms,
            _ => return None,
        };
        let report = Self {
            match_key: u64::from_le_bytes(raw[0..8].try_into().ok()?),
            actor: u64::from_le_bytes(raw[8..16].try_into().ok()?),
            seq: u32::from_le_bytes(raw[16..20].try_into().ok()?),
            detector,
            targets: raw[22..]
                .chunks_exact(8)
                .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
                .collect(),
        };
        // One canonical encoding: reject trailing zero groups and alternate lengths.
        (report.valid() && report.encode()?.as_slice() == text).then_some(report)
    }
}

/// Precise, non-identifying diagnostics. Returning a reason never changes admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReceiptRejection {
    Context,
    Report,
    Match,
    Source,
    Targets,
    Sequence,
    Capacity,
}
impl ReceiptRejection {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Context => "invalid_context",
            Self::Report => "invalid_report",
            Self::Match => "different_match",
            Self::Source => "source_owner_or_role",
            Self::Targets => "target_role_team_or_roster",
            Self::Sequence => "stale_or_duplicate_sequence",
            Self::Capacity => "receipt_capacity",
        }
    }
}

#[derive(Default)]
pub(crate) struct Receiver {
    scope: Option<CacheScope>,
    last: u64,
    entries: Vec<Received>,
}
struct Received {
    from: u32,
    report: Report,
    at: u64,
}
impl Receiver {
    fn sync(&mut self, c: &Context) -> bool {
        if !c.valid() {
            *self = Self::default();
            return false;
        }
        if self.scope != Some(c.cache_scope()) || c.now < self.last {
            self.entries.clear();
            self.scope = Some(c.cache_scope());
        }
        self.last = c.now;
        self.entries.retain_mut(|e| {
            // A real departure or a newly resolved conflicting identity retires
            // this watermark. Pending proxies alone must not reopen old sequences.
            if !c.session_peers.contains(&e.from) || c.people.iter().any(|p| {
                (p.peer == e.from && p.actor != e.report.actor)
                    || (p.actor == e.report.actor && p.peer != e.from)
            }) {
                return false;
            }
            if let Some(source) = c.person(e.report.actor).filter(|p| p.role == Role::Merc) {
                e.report.targets.retain(|id| c.is_spy_opponent(*id, source.team));
            } else {
                // Retain only the sequence, never a positive from an unresolved
                // or no-longer-Merc source. Restoration needs a fresh report.
                e.report.targets.clear();
            }
            // Admission still requires a current actor/From mapping. At most
            // three detectors per session peer survive; 32 peers bound this to 96.
            true
        });
        true
    }
    #[cfg(test)]
    pub fn receive(&mut self, c: &Context, from: u32, r: Report) -> bool {
        self.receive_report(c, from, r).is_ok()
    }
    pub(crate) fn receive_report(&mut self, c: &Context, from: u32, r: Report) -> Result<(), ReceiptRejection> {
        // Preserve the original order and state effects, including sync before
        // validating a report. Reasons do not weaken anti-replay or identity gates.
        if !self.sync(c) {
            return Err(ReceiptRejection::Context);
        }
        if !r.valid() {
            return Err(ReceiptRejection::Report);
        }
        if r.match_key != c.match_key {
            return Err(ReceiptRejection::Match);
        }
        let Some(source) = c.source(r.actor, from) else {
            return Err(ReceiptRejection::Source);
        };
        if r.targets.iter().any(|id| !c.is_spy_opponent(*id, source.team)) {
            return Err(ReceiptRejection::Targets);
        }
        let old = self
            .entries
            .iter_mut()
            .find(|e| e.report.actor == r.actor && e.report.detector == r.detector);
        if let Some(old) = old {
            let delta = r.seq.wrapping_sub(old.report.seq);
            if delta == 0 || delta >= 0x80000000 {
                return Err(ReceiptRejection::Sequence);
            }
            *old = Received { from, report: r, at: c.now };
        } else {
            if self.entries.len() >= 96 {
                return Err(ReceiptRejection::Capacity);
            }
            self.entries.push(Received { from, report: r, at: c.now });
        }
        Ok(())
    }
    pub fn detections(&mut self, c: &Context) -> ActualDetections {
        let mut out = ActualDetections::default();
        if !self.sync(c) || !c.person(c.local).is_some_and(|p| p.role == Role::Spy) {
            return out;
        }
        for e in &self.entries {
            if c.now.saturating_sub(e.at) < LEASE_MS
                && e.report.match_key == c.match_key
                && e.report.targets.contains(&c.local)
            {
                out.record(e.report.detector, true);
            }
        }
        out
    }
}
#[derive(Default)]
pub(crate) struct Publisher {
    scope: Option<CacheScope>,
    last: u64,
    seq: u32,
    sources: [Published; 3],
}
#[derive(Default)]
struct Published {
    positives: Vec<(u64, u64)>,
    last_sent: Option<(u64, Vec<u64>)>,
}
impl Publisher {
    pub(crate) fn invalidate_samples(&mut self) {
        // The receiving peer can keep its anti-replay watermark while this
        // sender briefly loses a valid roster/clock/life snapshot. Clear all
        // detection and throttle state, but never reuse a wire sequence in
        // this publisher's lifetime. No receiver validation is weakened.
        *self = Self { seq: self.seq, ..Self::default() };
    }
    pub fn sample(
        &mut self,
        c: &Context,
        detector: Source,
        targets: &[u64],
        partial: bool,
    ) -> Option<Report> {
        if !c.valid() || targets.len() > MAX_TARGETS {
            self.invalidate_samples();
            return None;
        }
        let local = c.person(c.local)?;
        if local.role != Role::Merc {
            // This publisher lost detector ownership. Remote receiver state is
            // separate; clear only outgoing samples and preserve the sequence.
            self.invalidate_samples();
            return None;
        }
        if targets.iter().any(|id| !c.is_spy_opponent(*id, local.team)) {
            return None;
        }
        if self.scope != Some(c.cache_scope()) || c.now < self.last {
            self.invalidate_samples();
            self.scope = Some(c.cache_scope());
        }
        self.last = c.now;
        let state = &mut self.sources[detector as usize];
        if !partial {
            state.positives.clear();
        }
        state
            .positives
            .retain(|(id, t)| c.now.saturating_sub(*t) < 200 && c.is_spy_opponent(*id, local.team));
        for id in targets {
            if let Some(x) = state.positives.iter_mut().find(|x| x.0 == *id) {
                x.1 = c.now;
            } else {
                state.positives.push((*id, c.now));
            }
        }
        if state.positives.len() > MAX_TARGETS {
            state.positives.clear();
            return None;
        }
        let mut ids: Vec<_> = state.positives.iter().map(|x| x.0).collect();
        ids.sort_unstable();
        // At most ten messages/sec/detector; silence when there has never been a positive.
        if state.last_sent.as_ref().is_some_and(|(t, last)| {
            c.now.saturating_sub(*t) < 100 || (last.is_empty() && ids.is_empty())
        }) || state.last_sent.is_none() && ids.is_empty()
        {
            return None;
        }
        state.last_sent = Some((c.now, ids.clone()));
        self.seq = self.seq.wrapping_add(1);
        Some(Report {
            match_key: c.match_key,
            actor: c.local,
            seq: self.seq,
            detector,
            targets: ids,
        })
    }
}
#[cfg(test)]
#[path = "goggle_feedback_tests.rs"]
mod tests;
