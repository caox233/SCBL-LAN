//! Native ChatString's DataString property accepts at most 80 UTF16 units,
//! even though the outer sender accepts 144. Keep all fragments <=80, bounded
//! and invisible to native chat; publish nothing until a complete valid report.
use crate::ats_marks::Role;
use crate::goggle_feedback::{Context, Report, PREFIX};
const FRAGMENT_PREFIX: &str = "~SCBLF1:";
pub(crate) const NATIVE_LIMIT: usize = 80;
const PIECE: usize = 61;
const MAX_PARTS: usize = 3;
const ASSEMBLY_MS: u64 = 300;
pub(crate) fn reserved(text: &[u16]) -> bool {
    [PREFIX, FRAGMENT_PREFIX]
        .iter()
        .any(|p| text.starts_with(&p.encode_utf16().collect::<Vec<_>>()))
}
pub(crate) fn packets(report: &Report) -> Option<Vec<Vec<u16>>> {
    let text = report.encode()?;
    if text.len() <= NATIVE_LIMIT {
        return Some(vec![text]);
    }
    let total = text.len().div_ceil(PIECE);
    if total > MAX_PARTS {
        return None;
    }
    Some(
        text.chunks(PIECE)
            .enumerate()
            .map(|(i, piece)| {
                let mut out: Vec<_> = format!("{FRAGMENT_PREFIX}{:08X}{}{}:", report.seq, i, total)
                    .encode_utf16()
                    .collect();
                out.extend(piece);
                out
            })
            .collect(),
    )
}
struct Fragment {
    seq: u32,
    index: usize,
    total: usize,
    piece: Vec<u16>,
}
fn parse(text: &[u16]) -> Option<Fragment> {
    if text.len() < 20
        || text.len() > NATIVE_LIMIT
        || !text.starts_with(&FRAGMENT_PREFIX.encode_utf16().collect::<Vec<_>>())
        || text[18] != b':' as u16
    {
        return None;
    }
    let mut seq = 0u32;
    for ch in &text[8..16] {
        let d = match *ch {
            48..=57 => *ch - 48,
            65..=70 => *ch - 65 + 10,
            _ => return None,
        };
        seq = (seq << 4) | d as u32;
    }
    let index = text[16].checked_sub(48)? as usize;
    let total = text[17].checked_sub(48)? as usize;
    if !(2..=MAX_PARTS).contains(&total)
        || index >= total
        || text[19..].iter().any(|v| !(32..=126).contains(v))
    {
        return None;
    }
    let piece = text[19..].to_vec();
    if index + 1 < total && piece.len() != PIECE {
        return None;
    }
    Some(Fragment {
        seq,
        index,
        total,
        piece,
    })
}
#[derive(Default)]
pub(crate) struct Inbox {
    scope: Option<crate::goggle_feedback::CacheScope>,
    last: u64,
    entries: Vec<Assembly>,
}
struct Assembly {
    from: u32,
    seq: u32,
    total: usize,
    at: u64,
    parts: Vec<Option<Vec<u16>>>,
}
impl Inbox {
    pub fn receive(&mut self, c: &Context, from: u32, text: &[u16]) -> Option<Report> {
        if !c.valid() {
            *self = Self::default();
            return None;
        }
        if self.scope != Some(c.cache_scope()) || c.now < self.last {
            self.entries.clear();
            self.scope = Some(c.cache_scope());
        }
        self.last = c.now;
        self.entries.retain(|e| {
            c.now.saturating_sub(e.at) < ASSEMBLY_MS
                && c.people
                    .iter()
                    .any(|p| p.peer == e.from && p.role == Role::Merc)
        });
        // Native envelope owner must be a current live Merc before allocating assembly.
        if !c
            .people
            .iter()
            .any(|p| p.peer == from && p.role == Role::Merc)
            || text.len() > NATIVE_LIMIT
        {
            return None;
        }
        if text.starts_with(&PREFIX.encode_utf16().collect::<Vec<_>>()) {
            return Report::decode(text);
        }
        let f = parse(text)?;
        let index = if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.from == from && e.seq == f.seq)
        {
            i
        } else {
            if self.entries.len() >= 32 {
                return None;
            }
            self.entries.push(Assembly {
                from,
                seq: f.seq,
                total: f.total,
                at: c.now,
                parts: vec![None; f.total],
            });
            self.entries.len() - 1
        };
        let e = &mut self.entries[index];
        if e.total != f.total {
            self.entries.remove(index);
            return None;
        }
        if let Some(old) = &e.parts[f.index] {
            if *old != f.piece {
                self.entries.remove(index);
            }
            return None; // duplicates never refresh age
        }
        e.parts[f.index] = Some(f.piece);
        if e.parts.iter().any(|p| p.is_none()) {
            return None;
        }
        let e = self.entries.remove(index);
        let joined: Vec<_> = e.parts.into_iter().flatten().flatten().collect();
        let report = Report::decode(&joined)?;
        (report.seq == e.seq).then_some(report)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::goggle_exposure::{ActualDetections, Source};
    use crate::goggle_feedback::{Person, Receiver};
    fn context() -> Context {
        Context {
            scope: [1, 2, 20, 3],
            match_key: 50,
            now: 100,
            local: 20,
            session_peers: std::iter::once(7).chain(20..28).collect(),
            people: std::iter::once(Person {
                actor: 10,
                peer: 7,
                role: Role::Merc,
                team: 1,
            })
            .chain((20..28).map(|actor| Person {
                actor,
                peer: actor as u32,
                role: Role::Spy,
                team: 2,
            }))
            .collect(),
        }
    }
    fn report(n: usize) -> Report {
        Report {
            match_key: 50,
            actor: 10,
            seq: 42,
            detector: Source::Rfd,
            targets: (20..20 + n as u64).collect(),
        }
    }
    #[test]
    fn every_supported_target_count_fits_native_field_and_roundtrips() {
        for n in 0..=8 {
            let r = report(n);
            let packets = packets(&r).unwrap();
            let mut inbox = Inbox::default();
            let mut result = None;
            for p in &packets {
                assert!(p.len() <= 80 && reserved(p));
                result = inbox.receive(&context(), 7, p);
            }
            assert_eq!(result, Some(r));
        }
    }
    #[test]
    fn out_of_order_fragments_do_not_publish_partial_receipts() {
        let r = report(8);
        let packets = packets(&r).unwrap();
        assert_eq!(packets.len(), 3);
        let c = context();
        let mut inbox = Inbox::default();
        let mut receiver = Receiver::default();
        assert!(inbox.receive(&c, 7, &packets[2]).is_none());
        assert!(inbox.receive(&c, 7, &packets[0]).is_none());
        assert_eq!(receiver.detections(&c), ActualDetections::default());
        let complete = inbox.receive(&c, 7, &packets[1]).unwrap();
        assert_eq!(complete, r);
        assert!(receiver.receive(&c, 7, complete));
        assert_ne!(receiver.detections(&c), ActualDetections::default());
    }
    #[test]
    fn duplicates_do_not_keep_partial_assembly_alive() {
        let ps = packets(&report(8)).unwrap();
        let mut inbox = Inbox::default();
        let mut c = context();
        assert!(inbox.receive(&c, 7, &ps[0]).is_none());
        c.now = 399;
        assert!(inbox.receive(&c, 7, &ps[0]).is_none());
        c.now = 400;
        assert!(inbox.receive(&c, 7, &ps[1]).is_none());
        assert!(inbox.receive(&c, 7, &ps[2]).is_none());
    }
    #[test]
    fn wrong_origin_reset_conflicting_fragment_and_oversize_are_rejected() {
        let ps = packets(&report(8)).unwrap();
        let mut c = context();
        let mut inbox = Inbox::default();
        for p in &ps {
            assert!(inbox.receive(&c, 20, p).is_none());
        }
        assert!(inbox.receive(&c, 7, &vec![65; 81]).is_none());
        inbox.receive(&c, 7, &ps[0]);
        let mut conflict = ps[0].clone();
        conflict[25] ^= 1;
        assert!(inbox.receive(&c, 7, &conflict).is_none());
        assert!(inbox.receive(&c, 7, &ps[1]).is_none());
        assert!(inbox.receive(&c, 7, &ps[2]).is_none());
        c.scope[3] += 1;
        assert!(inbox.receive(&c, 7, &ps[0]).is_none());
        assert_eq!(inbox.entries.len(), 1);
    }
    #[test]
    fn normal_chat_is_never_reserved() {
        for s in ["hello", "~SCBLX", "~SCBLF0:", "队伍聊天"] {
            assert!(!reserved(&s.encode_utf16().collect::<Vec<_>>()));
        }
    }
    #[test]
    fn feedback_feature_is_independent_of_sampling_and_long_battery() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("goggle-feedback = []"));
        assert!(manifest.contains("native-observation = []"));
        assert!(manifest.contains("drone-capture-long-battery = [\"native-observation\"]"));
        let lib = include_str!("lib.rs").replace("\r\n", "\n");
        assert!(lib.contains(
            "#[cfg(any(test, feature = \"goggle-feedback\"))]\nmod goggle_feedback_native;"
        ));
    }
}
