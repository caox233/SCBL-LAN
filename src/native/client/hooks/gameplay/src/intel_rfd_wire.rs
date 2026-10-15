//! C50: source-authenticated, short-lived Intel RF state. Uses the already
//! validated private MOD transport, never a native mark or replicated pawn bit.
use crate::{ats_marks::Role, goggle_feedback::Context};
pub(crate) const PREFIX: &str = "~SCBLI1:";
const REFRESH: u64 = 200;
const LEASE: u64 = 750;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub match_key: u64,
    pub actor: u64,
    pub seq: u32,
    pub active: bool,
}
impl Report {
    pub fn encode(self) -> Vec<u16> {
        format!(
            "{PREFIX}{:016X}{:016X}{:08X}{}",
            self.match_key,
            self.actor,
            self.seq,
            u8::from(self.active)
        )
        .encode_utf16()
        .collect()
    }
    pub fn decode(text: &[u16]) -> Option<Self> {
        if text.len() != 49 || !reserved(text) {
            return None;
        }
        let s = String::from_utf16(text).ok()?;
        let r = Self {
            match_key: u64::from_str_radix(s.get(8..24)?, 16).ok()?,
            actor: u64::from_str_radix(s.get(24..40)?, 16).ok()?,
            seq: u32::from_str_radix(s.get(40..48)?, 16).ok()?,
            active: match s.get(48..49)? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
        };
        (r.match_key != 0 && r.actor != 0 && r.encode() == text).then_some(r)
    }
}
pub(crate) fn reserved(t: &[u16]) -> bool {
    t.starts_with(&PREFIX.encode_utf16().collect::<Vec<_>>())
}
#[derive(Default)]
pub(crate) struct State {
    scope: Option<crate::goggle_feedback::CacheScope>,
    last: u64,
    sent: Option<(u64, bool)>,
    seq: u32,
    entries: Vec<(u32, Report, u64)>,
}
impl State {
    pub(crate) fn invalidate_samples(&mut self) {
        // Peers may retain their anti-replay watermark through local context loss.
        *self = Self { seq: self.seq, ..Self::default() };
    }
    fn sync(&mut self, c: &Context) -> bool {
        if !c.valid() {
            self.invalidate_samples();
            return false;
        }
        if self.scope != Some(c.cache_scope()) || c.now < self.last {
            *self = Self {
                scope: Some(c.cache_scope()),
                // Native actor keys can outlive a local HUD/life refresh. Do
                // not restart sequence numbers and make peers reject refreshes.
                seq: self.seq,
                ..Self::default()
            };
        }
        self.last = c.now;
        self.entries.retain(|(from, r, _)| {
            c.person(r.actor)
                .is_some_and(|p| p.peer == *from && p.role == Role::Spy)
        });
        true
    }
    pub fn publish(&mut self, c: &Context, active: bool) -> Option<Report> {
        if !self.sync(c) || !c.person(c.local).is_some_and(|p| p.role == Role::Spy) {
            return None;
        }
        if self
            .sent
            .is_some_and(|(at, old)| old == active && c.now.saturating_sub(at) < REFRESH)
        {
            return None;
        }
        self.seq = self.seq.wrapping_add(1);
        self.sent = Some((c.now, active));
        Some(Report {
            match_key: c.match_key,
            actor: c.local,
            seq: self.seq,
            active,
        })
    }
    pub fn receive(&mut self, c: &Context, from: u32, r: Report) -> bool {
        if !self.sync(c)
            || r.match_key != c.match_key
            || !c
                .person(r.actor)
                .is_some_and(|p| p.peer == from && p.role == Role::Spy)
        {
            return false;
        }
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|(_, old, _)| old.actor == r.actor)
        {
            let d = r.seq.wrapping_sub(e.1.seq);
            if d == 0 || d >= 0x80000000 {
                return false;
            }
            *e = (from, r, c.now);
        } else {
            if self.entries.len() >= 32 {
                return false;
            }
            self.entries.push((from, r, c.now));
        }
        true
    }
    pub fn active(&mut self, c: &Context, actor: u64) -> bool {
        self.sync(c)
            && self.entries.iter().any(|(_, r, at)| {
                r.actor == actor
                    && r.match_key == c.match_key
                    && r.active
                    && c.now.saturating_sub(*at) < LEASE
            })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::goggle_feedback::Person;
    fn context() -> Context {
        Context {
            scope: [1, 2, 3, 4],
            match_key: 50,
            now: 10,
            local: 3,
            session_peers: vec![7, 8],
            people: vec![
                Person {
                    actor: 3,
                    peer: 7,
                    team: 1,
                    role: Role::Spy,
                },
                Person {
                    actor: 4,
                    peer: 8,
                    team: 2,
                    role: Role::Merc,
                },
            ],
        }
    }
    #[test]
    fn bounded_owner_packet_no_caster_no_rf() {
        let mut s = State::default();
        let c = context();
        let r = s.publish(&c, false).unwrap();
        assert_eq!(r.encode().len(), 49);
        assert_eq!(Report::decode(&r.encode()), Some(r));
        let mut bad = r.encode();
        bad.push(48);
        assert!(Report::decode(&bad).is_none());
        assert!(!s.receive(&c, 8, Report { active: true, ..r }));
        assert!(s.receive(&c, 7, r));
        assert!(!s.active(&c, 3));
        assert!(!s.receive(&c, 7, Report { active: true, ..r })); // duplicate/replay
    }
    #[test]
    fn actual_target_edges_refresh_withdraw_and_lease() {
        let mut tx = State::default();
        let mut rx = State::default();
        let mut c = context();
        let r = tx.publish(&c, true).unwrap();
        assert!(rx.receive(&c, 7, r));
        assert!(rx.active(&c, 3));
        c.now += 100;
        assert!(tx.publish(&c, true).is_none());
        let off = tx.publish(&c, false).unwrap();
        assert!(rx.receive(&c, 7, off));
        assert!(!rx.active(&c, 3));
        let on = tx.publish(&c, true).unwrap();
        assert!(rx.receive(&c, 7, on));
        c.now += 750;
        assert!(!rx.active(&c, 3));
        assert!(!rx.receive(&c, 7, on));
        let next = tx.publish(&c, true).unwrap();
        assert!(rx.receive(&c, 7, next));
        assert!(rx.active(&c, 3));
        c.scope[0] += 1;
        assert!(!rx.active(&c, 3));
    }
    #[test]
    fn match_role_and_identity_change_reject_stale_source() {
        let mut s = State::default();
        let mut c = context();
        let r = Report {
            match_key: 50,
            actor: 3,
            seq: 1,
            active: true,
        };
        assert!(!s.receive(&c, 7, Report { match_key: 51, ..r }));
        assert!(!s.receive(&c, 8, Report { actor: 4, ..r }));
        assert!(s.receive(&c, 7, r));
        c.people[0].peer = 9;
        c.session_peers[0] = 9;
        assert!(c.valid());
        assert!(!s.active(&c, 3));
        c.people[0].peer = 7;
        c.session_peers[0] = 7;
        assert!(c.valid());
        assert!(!s.active(&c, 3));
    }
    #[test]
    fn publisher_sequence_survives_life_refresh_and_temporary_missing_context() {
        let mut tx = State::default();
        let mut rx = State::default();
        let mut c = context();
        let first = tx.publish(&c, true).unwrap();
        assert!(rx.receive(&c, 7, first));
        let mut source = c.clone();
        source.scope[3] += 1;
        source.now += 10;
        let refreshed = tx.publish(&source, true).unwrap();
        assert!(refreshed.seq > first.seq);
        c.now += 10;
        assert!(rx.receive(&c, 7, refreshed));
        let mut invalid = source.clone();
        invalid.local = 0;
        assert!(tx.publish(&invalid, true).is_none());
        source.now += 10;
        let recovered = tx.publish(&source, false).unwrap();
        assert!(recovered.seq > refreshed.seq);
        c.now += 10;
        assert!(rx.receive(&c, 7, recovered));
        assert!(!rx.active(&c, 3));
    }
}
