type TestServer<'a> = Server<'a, Box<dyn FnMut(ClientInfo<()>)>, fn(ClientInfo<()>), ()>;
use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
static PACKETS: AtomicUsize = AtomicUsize::new(0);
struct StopFixture;
#[expect(
    clippy::needless_pass_by_value,
    reason = "PRUDP user_handler requires an owned QPacket"
)]
fn control(_: &Logger, packet: QPacket, _: SocketAddr, _: &UdpSocket) {
    if packet.payload == b"retirement-stop" {
        std::panic::panic_any(StopFixture);
    }
    PACKETS.fetch_add(1, Ordering::Relaxed);
}
struct Owned {
    address: SocketAddr,
    stop: Vec<u8>,
    handle: Option<std::thread::JoinHandle<bool>>,
}
impl Owned {
    fn stop(&mut self) -> bool {
        let Some(handle) = self.handle.take() else {
            return true;
        };
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..3 {
            let _ = socket.send_to(&self.stop, self.address);
        }
        handle.join().unwrap_or(false)
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn exercise(busy: bool) -> (usize, usize) {
    PACKETS.store(0, Ordering::Relaxed);
    let ctx = Context::splinter_cell_blacklist();
    let server_ctx = ctx.clone();
    let expired = Arc::new(AtomicUsize::new(0));
    let fresh = Arc::new(AtomicUsize::new(0));
    let e = Arc::clone(&expired);
    let f = Arc::clone(&fresh);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let logger = Logger::root(slog::Discard, o!());
        let registry = StreamHandlerRegistry::new(logger.clone());
        let mut server: TestServer<'_> = Server::new(logger, &server_ctx, registry);
        server.bind("127.0.0.1:0").unwrap();
        let address = server.socket.as_ref().unwrap().local_addr().unwrap();
        for (id, old) in [(90u32, true), (91, false)] {
            let mut ci = ClientInfo::new(address);
            ci.connection_id = Some(ConnectionID(id));
            if old {
                ci.last_seen = Instant::now()
                    .checked_sub(SESSION_TIMEOUT + Duration::from_secs(1))
                    .unwrap();
            }
            server.client_registry.clients.insert(id, RefCell::new(ci));
            server
                .client_registry
                .connection_id_session_ids
                .insert(ConnectionID(id), Signature(id));
        }
        server.expired_client_handler = Some(Box::new(move |ci| {
            if ci.connection_id == Some(ConnectionID(90)) {
                e.fetch_add(1, Ordering::Relaxed);
            } else {
                f.fetch_add(1, Ordering::Relaxed);
            }
        }));
        server.user_handler = Some(control);
        ready_tx.send(address).unwrap();
        match catch_unwind(AssertUnwindSafe(|| server.serve())) {
            Err(payload) => payload.is::<StopFixture>(),
            Ok(()) => false,
        }
    });
    let address = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let packet = |payload: &[u8]| {
        QPacket {
            source: VPort {
                port: 15,
                stream_type: packet::StreamType::RVSec,
            },
            destination: VPort {
                port: 1,
                stream_type: packet::StreamType::RVSec,
            },
            packet_type: PacketType::User,
            payload: payload.to_vec(),
            flags: PacketFlag::HasSize.into(),
            ..Default::default()
        }
        .to_bytes(&ctx)
    };
    let mut owned = Owned {
        address,
        stop: packet(b"retirement-stop"),
        handle: Some(handle),
    };
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let wire = packet(b"retirement-traffic");
    let start = Instant::now();
    let mut previous = start;
    let mut max_gap = Duration::ZERO;
    let mut sent = 0;
    while start.elapsed() < Duration::from_millis(2500) {
        if busy {
            socket.send_to(&wire, address).unwrap();
            let now = Instant::now();
            max_gap = max_gap.max(now.saturating_duration_since(previous));
            previous = now;
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let retired = expired.load(Ordering::Relaxed);
    let live = fresh.load(Ordering::Relaxed);
    let received = PACKETS.load(Ordering::Relaxed);
    if busy {
        assert!(
            sent >= 100 && received >= 100 && max_gap < Duration::from_millis(500),
            "invalid traffic fixture:{sent}/{received}/{max_gap:?}"
        );
    }
    assert!(owned.stop());
    println!("CLEANUP_TRAFFIC busy={busy} sent={sent} received={received} max_gap_ms={} owned_server_stopped=true",max_gap.as_millis());
    (retired, live)
}
#[test]
fn maintenance_pending_busy_loop_cleanup_observation() {
    sodiumoxide::init().unwrap();
    let mut violations = 0;
    for busy in [true, false] {
        let (expired, fresh) = exercise(busy);
        let valid = expired == 1 && fresh == 0;
        if !valid {
            violations += 1;
        }
        println!("CLEANUP_CADENCE busy={busy} expired_callbacks={expired} fresh_callbacks={fresh} valid={valid}");
    }
    println!("CLEANUP_CADENCE_SUMMARY cases=2 violations={violations} actual_serve_loop=true synthetic_age=true");
    assert_eq!(violations, 0);
}
