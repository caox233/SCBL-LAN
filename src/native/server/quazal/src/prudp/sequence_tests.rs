use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
#[test]
fn maintenance_outbound_sequence_profile_observation() {
    sodiumoxide::init().unwrap();
    let logger = Logger::root(slog::Discard, o!());
    let ctx = Context::splinter_cell_blacklist();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = receiver.local_addr().unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut panics = 0;
    let mut packets = 0;
    let mut controls = 0;
    for start in [0u16, 1, 65534, 65535] {
        let mut ci: ClientInfo<()> = ClientInfo::new(address);
        ci.client_sequence_id = start;
        ci.server_signature = 0x1234_5678;
        ci.client_session = 7;
        for step in 0..3u16 {
            let req = QPacket {
                source: VPort {
                    port: 1,
                    stream_type: packet::StreamType::RVSec,
                },
                destination: VPort {
                    port: 15,
                    stream_type: packet::StreamType::RVSec,
                },
                packet_type: PacketType::Data,
                payload: b"sequence-fixture".to_vec(),
                ..Default::default()
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                send_request(&logger, &ctx, &address, &sender, req, &mut ci)
            }));
            match outcome {
                Ok(Ok(_)) => {
                    let mut buffer = [0u8; 2048];
                    let (n, _) = receiver.recv_from(&mut buffer).unwrap();
                    let (q, _) = QPacket::from_bytes(&ctx, &buffer[..n]).unwrap();
                    q.validate(&ctx, &buffer[..n]).unwrap();
                    assert_eq!(q.sequence, start.wrapping_add(step));
                    assert_eq!(q.payload, b"sequence-fixture");
                    assert_eq!(q.signature, 0x1234_5678);
                    assert_eq!(q.session_id, 7);
                    assert!(q.flags.contains(PacketFlag::Reliable));
                    assert!(q.flags.contains(PacketFlag::NeedAck));
                    packets += 1;
                    println!(
                        "SEQUENCE start={start} step={step} outcome=sent wire={} next={}",
                        q.sequence, ci.client_sequence_id
                    );
                }
                Err(_) => {
                    panics += 1;
                    println!(
                        "SEQUENCE start={start} step={step} outcome=panic next={}",
                        ci.client_sequence_id
                    );
                }
                Ok(Err(e)) => panic!("fixture UDP send failed:{e}"),
            }
        }
        ci.server_sequence_id = start;
        for step in 0..3u16 {
            let response = prepare_response(QPacket::default(), &mut ci);
            assert_eq!(response.sequence, start.wrapping_add(step));
            controls += 1;
        }
    }
    println!("SEQUENCE_SUMMARY starts=4 attempts=12 panics={panics} packets={packets} response_controls={controls} debug_assertions={} loopback_only=true",cfg!(debug_assertions));
    assert_eq!(panics, 0);
}
