//! Permanent issue45 extension: real RVSec framing, PRUDP fragmentation and replay.
//! Only the protocol producer is synthetic; it returns a counted, sized byte vector.
//! The existing feature-gated send leaf captures actual encoded packets, not replies.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use quazal::prudp::fragment_boundary_capture::Capture;
use quazal::prudp::packet::{PacketFlag, PacketType, QPacket, StreamHandlerRegistry, StreamType, VPort};
use quazal::prudp::{ClientRegistry, Server};
use quazal::rmc::{Packet, Protocol, Request, Response, ResponseData, RVSecHandler};
use quazal::{ClientInfo, Context};
use serde_json::json;

type TestServer<'a> = Server<'a, fn(ClientInfo<()>), fn(ClientInfo<()>), ()>;

struct CountedProducer {
    id: u16,
    frame_bytes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

fn success_frame(id: u16, call: u32, data: Vec<u8>) -> Vec<u8> {
    Response { protocol_id: id, result: Ok(ResponseData { call_id: call, method_id: 1, data }) }.to_bytes()
}

impl Protocol<()> for CountedProducer {
    fn id(&self) -> u16 { self.id }
    fn name(&self) -> String { "SyntheticCountedBoundaryProducer".to_owned() }
    fn num_methods(&self) -> u32 { 1 }
    fn method_name(&self, method: u32) -> Option<String> {
        (method == 1).then(|| "counted_bytes".to_owned())
    }
    fn handle(
        &self, _logger: &slog::Logger, _ctx: &Context, ci: &mut ClientInfo<()>,
        request: &Request, _registry: &ClientRegistry<()>, _socket: &std::net::UdpSocket,
    ) -> quazal::rmc::Result<Vec<u8>> {
        assert_eq!(ci.user_id, Some(1000));
        assert_eq!((request.protocol_id, request.method_id), (self.id, 1));
        assert!(request.parameters.is_empty());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let overhead = success_frame(self.id, request.call_id, Vec::new()).len();
        Ok(vec![b'N'; self.frame_bytes.load(Ordering::SeqCst).checked_sub(overhead).unwrap()])
    }
}

struct DecodedWire {
    ack_count: usize,
    ids: Vec<u8>,
    payload: Vec<u8>,
    response: Response,
}

fn decode_wire(ctx: &Context, wire: &[Vec<u8>], sequence: u16) -> DecodedWire {
    let mut ack_count = 0;
    let mut ids = Vec::new();
    let mut payload = Vec::new();
    for bytes in wire {
        let (packet, consumed) = QPacket::from_bytes(ctx, bytes).expect("real output codec parses");
        assert_eq!(consumed as usize, bytes.len());
        packet.validate(ctx, bytes).expect("real output checksum validates");
        assert_eq!(packet.packet_type, PacketType::Data);
        if packet.flags.contains(PacketFlag::Ack) {
            ack_count += 1;
            assert_eq!(packet.sequence, sequence);
            assert!(packet.payload.is_empty());
        } else {
            ids.push(packet.fragment_id.expect("outgoing DATA includes fragment identity"));
            payload.extend_from_slice(&packet.payload);
        }
    }
    assert_eq!(ack_count, 1, "one ACK for each actual request dispatch, including retry");
    assert!(!ids.is_empty(), "silent skip is not controlled rejection");
    let Packet::Response(response) = Packet::from_bytes(&payload).expect("complete actual RMC frame") else {
        panic!("response must not turn into request");
    };
    DecodedWire { ack_count, ids, payload, response }
}

fn assert_response(decoded: &DecodedWire, id: u16, call: u32, frame_bytes: usize) {
    assert_eq!(decoded.response.protocol_id, id);
    // Oracle uses the fixed public wire contract, not the implementation constant.
    assert!(decoded.ids.len() <= 256);
    assert_eq!(decoded.ids.iter().filter(|&&v| v == 0).count(), 1);
    assert_eq!(decoded.ids.iter().copied().collect::<std::collections::HashSet<_>>().len(), decoded.ids.len());
    assert_eq!(decoded.ids.last(), Some(&0));
    assert!(decoded.ids.windows(2).all(|w| w[0].checked_sub(1) == Some(w[1])));
    if frame_bytes > 256_000 {
        let error = decoded.response.result.as_ref().expect_err("overflow must be explicit RMC failure");
        assert_eq!(error.error_code, 0x8001_0012, "InternalError, not a substituted parsing/unknown-protocol error");
        assert_eq!(error.call_id, call);
        assert_eq!(decoded.ids, [0]);
        assert_eq!(decoded.payload.len(), if id < 127 { 14 } else { 16 });
    } else {
        let data = decoded.response.result.as_ref().expect("legal serialized frame must stay successful");
        assert_eq!((data.call_id, data.method_id), (call, 1));
        assert_eq!(decoded.payload.len(), frame_bytes);
        let overhead = success_frame(id, call, Vec::new()).len();
        assert_eq!(overhead, if id < 127 { 14 } else { 16 });
        assert_eq!(data.data, vec![b'N'; frame_bytes - overhead]);
        assert_eq!(decoded.payload, success_frame(id, call, data.data.clone()));
        let expected_fragments = frame_bytes.div_ceil(1000);
        assert_eq!(decoded.ids.len(), expected_fragments);
        assert_eq!(usize::from(decoded.ids[0]), expected_fragments - 1);
    }
}

fn request(ctx: &Context, port: VPort, signature: u32, id: u16, call: u32, sequence: u16) -> QPacket {
    let packet = QPacket {
        source: port, destination: port, packet_type: PacketType::Data,
        signature, session_id: 0, sequence, fragment_id: Some(0),
        payload: Request { protocol_id: id, call_id: call, method_id: 1, parameters: Vec::new() }.to_bytes(),
        ..Default::default()
    };
    // Inbound socket/receive loop is bypassed, but not the real QPacket codec.
    let bytes = packet.to_bytes(ctx);
    let (decoded, consumed) = QPacket::from_bytes(ctx, &bytes).unwrap();
    assert_eq!(consumed as usize, bytes.len());
    decoded.validate(ctx, &bytes).unwrap();
    decoded
}

fn boundary_and_retry(name: &str, id: u16, frame_bytes: usize) {
    let base = PathBuf::from(std::env::var_os("SCBL_FRAGMENT_TEST_DIR").expect("maintain owns synthetic output"));
    assert!(base.is_absolute() && base.is_dir());
    let output = base.join(name);
    std::fs::create_dir(&output).expect("case output must be fresh; no reuse/overwrite");
    sodiumoxide::init().unwrap();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let ctx = Context::splinter_cell_blacklist();
    let port = VPort { stream_type: StreamType::RVSec, port: ctx.vport };
    let calls = Arc::new(AtomicUsize::new(0));
    let producer_size = Arc::new(AtomicUsize::new(frame_bytes));
    let mut handler = RVSecHandler::<()>::new(logger.clone());
    handler.register_protocol(Box::new(CountedProducer {
        id, frame_bytes: producer_size.clone(), calls: calls.clone(),
    }));
    let mut registry = StreamHandlerRegistry::new(logger.clone());
    registry.register(port, Box::new(handler));
    let mut server: TestServer<'_> = Server::new(logger, &ctx, registry);
    // An owned ephemeral socket satisfies the existing API; capture prevents sends.
    server.bind("127.0.0.1:0").unwrap();
    let address = "127.0.0.1:31001".parse().unwrap();
    let signature = 0x4510_0002;
    let mut ci = ClientInfo::<()>::new(address);
    ci.user_id = Some(1000);
    server.maintenance_seed_connected(signature, ci, false);
    let capture = Capture::begin();
    let original_request = request(&ctx, port, signature, id, 77, 77);
    server.maintenance_dispatch(original_request.clone(), address);
    let first_wire = capture.take();
    let first = decode_wire(&ctx, &first_wire, 77);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "real producer called once on first request");
    server.maintenance_dispatch(original_request, address);
    let retry_wire = capture.take();
    let retry = decode_wire(&ctx, &retry_wire, 77);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cache retry must not rerun producer");
    assert_eq!(first_wire, retry_wire, "complete encoded wire, ACK and all DATA, must replay identically");
    assert_response(&first, id, 77, frame_bytes);
    assert_response(&retry, id, 77, frame_bytes);
    let overflow = frame_bytes > 256_000;
    let mut recovery_ack = None;
    if overflow {
        producer_size.store(64, Ordering::SeqCst);
        server.maintenance_dispatch(request(&ctx, port, signature, id, 78, 78), address);
        let next_wire = capture.take();
        let next = decode_wire(&ctx, &next_wire, 78);
        assert_response(&next, id, 78, 64);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "new request must execute, not inherit cached error");
        assert_ne!(next_wire, first_wire);
        recovery_ack = Some(next.ack_count);
    }
    let observation = json!({"case":name,"protocol_id":id,"requested_frame_bytes":frame_bytes,
        "first_ack_packets":first.ack_count,"retry_ack_packets":retry.ack_count,
        "data_packets":first.ids.len(),"emitted_payload_bytes":first.payload.len(),
        "error_code":first.response.result.as_ref().err().map(|e|e.error_code),
        "identical_retry_wire":true,"producer_calls_after_retry":1,
        "producer_calls_final":calls.load(Ordering::SeqCst),"recovery_ack_packets":recovery_ack,
        "recovery_call":if overflow {Some(78)} else {None},"pass":true});
    std::fs::write(output.join("OBSERVATION.json"), serde_json::to_vec_pretty(&observation).unwrap()).unwrap();
    println!("ISSUE45_JSON {}", observation);
}

#[test]
fn issue45_short_126_exact_boundary_cached_once() {
    boundary_and_retry("short_126_exact_retry", 126, 256_000);
}
#[test]
fn issue45_short_126_overflow_internal_error_and_recovery() {
    boundary_and_retry("short_126_overflow_retry", 126, 256_001);
}
#[test]
fn issue45_extended_5002_exact_boundary_cached_once() {
    boundary_and_retry("extended_5002_exact_retry", 5002, 256_000);
}
#[test]
fn issue45_extended_5002_overflow_internal_error_and_recovery() {
    boundary_and_retry("extended_5002_overflow_retry", 5002, 256_001);
}
