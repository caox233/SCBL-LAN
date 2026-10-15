use std::io::{self, Cursor, Read, Seek, SeekFrom};
use quazal::{Context, prudp::packet::{Error, PacketFlag, PacketType, QPacket, StreamType, VPort}};

fn packet(kind: PacketType, sized: bool) -> QPacket {
    QPacket {
        source: VPort { port: 1, stream_type: StreamType::RVSec },
        destination: VPort { port: 2, stream_type: StreamType::RVSec },
        packet_type: kind,
        flags: if sized { PacketFlag::HasSize.into() } else { Default::default() },
        conn_signature: matches!(kind, PacketType::Syn | PacketType::Connect).then_some(0x12345678),
        fragment_id: (kind == PacketType::Data).then_some(0),
        sequence: 17,
        ..Default::default()
    }
}
const KINDS: [PacketType; 8] = [PacketType::Syn, PacketType::Connect, PacketType::Data,
    PacketType::Disconnect, PacketType::Ping, PacketType::User, PacketType::Route, PacketType::Raw];

#[test]
fn every_empty_packet_truncation_is_an_error_not_a_panic() {
    let ctx = Context::splinter_cell_blacklist();
    let mut panics = Vec::new();
    let mut accepted = Vec::new();
    let mut checked = 0;
    for kind in KINDS {
        for sized in [false, true] {
            let wire = packet(kind, sized).to_bytes(&ctx);
            for len in 0..wire.len() {
                checked += 1;
                match std::panic::catch_unwind(|| QPacket::from_bytes(&ctx, &wire[..len])) {
                    Err(_) => panics.push((kind, sized, len)),
                    Ok(Ok(_)) => accepted.push((kind, sized, len)),
                    Ok(Err(_)) => {}
                }
            }
        }
    }
    println!("PRUDP truncations={checked} panics={panics:?} accepted={accepted:?}");
    assert!(panics.is_empty(), "truncated network input must return an error");
    assert!(accepted.is_empty());
}

struct FailingSeek(Cursor<Vec<u8>>);
impl Read for FailingSeek { fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.0.read(buf) } }
impl Seek for FailingSeek { fn seek(&mut self, _: SeekFrom) -> io::Result<u64> { Err(io::Error::other("fixture seek failure")) } }
#[test]
fn reader_seek_failure_is_propagated() {
    let ctx = Context::splinter_cell_blacklist();
    let wire = packet(PacketType::Ping, false).to_bytes(&ctx);
    let outcome = std::panic::catch_unwind(|| QPacket::from_reader(&ctx, &mut FailingSeek(Cursor::new(wire))));
    println!("PRUDP seek_error_panics={}", outcome.is_err());
    assert!(matches!(outcome, Ok(Err(Error::IO(ref e))) if e.kind() == io::ErrorKind::Other));
}

#[test]
fn valid_empty_packets_keep_exact_wire_bytes() {
    let ctx = Context::splinter_cell_blacklist();
    for kind in KINDS { for sized in [false, true] {
        let wire = packet(kind, sized).to_bytes(&ctx);
        let (decoded, consumed) = QPacket::from_bytes(&ctx, &wire).unwrap();
        decoded.validate(&ctx, &wire).unwrap();
        assert_eq!(consumed as usize, wire.len());
        assert_eq!(decoded.to_bytes(&ctx), wire);
    }}
    println!("PRUDP valid_empty_wire_roundtrips=16");
}

#[test]
fn sized_compressed_and_plain_packets_remain_separable() {
    let ctx = Context::splinter_cell_blacklist();
    let mut joined = Vec::new();
    let mut expected = Vec::new();
    for compressed in [false, true] {
        let mut p = packet(PacketType::Data, true);
        p.payload = b"native wire fixture payload".repeat(10);
        p.use_compression = compressed;
        let wire = p.to_bytes(&ctx);
        joined.extend_from_slice(&wire);
        expected.push((wire, p.payload));
    }
    let mut remaining = joined.as_slice();
    for (wire, payload) in expected {
        let (parsed, n) = QPacket::from_bytes(&ctx, remaining).unwrap();
        assert_eq!(n as usize, wire.len());
        parsed.validate(&ctx, &remaining[..n as usize]).unwrap();
        assert_eq!(parsed.payload, payload);
        assert_eq!(parsed.to_bytes(&ctx), wire);
        remaining = &remaining[n as usize..];
    }
    assert!(remaining.is_empty());
}

#[test]
fn incomplete_size_prefix_or_payload_is_rejected() {
    let ctx = Context::splinter_cell_blacklist();
    for claimed in [1u16, 256, u16::MAX] {
        let mut wire = packet(PacketType::Ping, true).to_bytes(&ctx);
        wire[10..12].copy_from_slice(&claimed.to_le_bytes());
        assert!(QPacket::from_bytes(&ctx, &wire).is_err());
    }
}
