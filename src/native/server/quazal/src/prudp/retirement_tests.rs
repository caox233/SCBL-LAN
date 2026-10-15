use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
type TestServer<'a> =
    Server<'a, Box<dyn FnMut(ClientInfo<()>)>, Box<dyn FnMut(ClientInfo<()>)>, ()>;
fn make<'a>(ctx: &'a Context, logger: &Logger) -> TestServer<'a> {
    let mut server = Server::new(
        logger.clone(),
        ctx,
        StreamHandlerRegistry::new(logger.clone()),
    );
    server.bind("127.0.0.1:0").unwrap();
    server
}
fn connected(server: &mut TestServer<'_>, signature: u32, address: SocketAddr, old: bool) {
    let mut ci = ClientInfo::new(address);
    ci.server_signature = signature;
    ci.connection_id = Some(ConnectionID(signature));
    ci.client_signature = Some(signature + 100);
    if old {
        ci.last_seen = Instant::now()
            .checked_sub(SESSION_TIMEOUT + Duration::from_secs(1))
            .unwrap();
    }
    server
        .client_registry
        .clients
        .insert(signature, RefCell::new(ci));
    server
        .client_registry
        .connection_id_session_ids
        .insert(ConnectionID(signature), Signature(signature));
}
fn pending_expiry(
    ctx: &Context,
    logger: &Logger,
    address: SocketAddr,
    receiver: &UdpSocket,
) -> bool {
    let mut buf = [0u8; 2048];
    let mut server = make(ctx, logger);
    for old in [true, false] {
        let packet = QPacket {
            source: VPort {
                port: 15,
                stream_type: packet::StreamType::RVSec,
            },
            destination: VPort {
                port: 1,
                stream_type: packet::StreamType::RVSec,
            },
            packet_type: PacketType::Syn,
            conn_signature: Some(0),
            ..Default::default()
        };
        server.handle_packet(logger, packet, address);
        let (n, _) = receiver.recv_from(&mut buf).unwrap();
        let (ack, _) = QPacket::from_bytes(ctx, &buf[..n]).unwrap();
        let signature = ack.conn_signature.unwrap();
        if old {
            server.new_clients.get_mut(&signature).unwrap().last_seen = Instant::now()
                .checked_sub(SESSION_TIMEOUT + Duration::from_secs(1))
                .unwrap();
        }
    }
    server.clear_clients();
    let pending = server.new_clients.len();
    let fresh = server
        .new_clients
        .values()
        .filter(|ci| ci.last_seen.elapsed() < SESSION_TIMEOUT)
        .count();
    let valid = pending == 1 && fresh == 1;
    println!("RETIREMENT case=pending_expiry pending={pending} fresh={fresh} valid={valid}");
    valid
}
fn connected_expiry(ctx: &Context, logger: &Logger, address: SocketAddr, callback: bool) -> bool {
    let mut server = make(ctx, logger);
    connected(&mut server, 10, address, true);
    connected(&mut server, 11, address, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    if callback {
        server.expired_client_handler = Some(Box::new(move |ci| {
            assert_eq!(ci.connection_id, Some(ConnectionID(10)));
            observed.fetch_add(1, Ordering::Relaxed);
        }));
    }
    server.clear_clients();
    server.clear_clients();
    let clients = server.client_registry.clients.len();
    let indexes = server.client_registry.connection_id_session_ids.len();
    let live = server
        .client_registry
        .client_by_connection_id(ConnectionID(11))
        .is_some();
    let callbacks = calls.load(Ordering::Relaxed);
    let valid = clients == 1 && indexes == 1 && live && callbacks == usize::from(callback);
    println!("RETIREMENT case=connected_expiry callback={callback} clients={clients} indexes={indexes} live={live} callbacks={callbacks} valid={valid}");
    valid
}
fn disconnect_retirement(
    ctx: &Context,
    logger: &Logger,
    address: SocketAddr,
    receiver: &UdpSocket,
    callback: bool,
) -> bool {
    let mut buf = [0u8; 2048];
    let mut server = make(ctx, logger);
    connected(&mut server, 20, address, false);
    connected(&mut server, 21, address, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    if callback {
        server.disconnect_handler = Some(Box::new(move |ci| {
            assert_eq!(ci.connection_id, Some(ConnectionID(20)));
            observed.fetch_add(1, Ordering::Relaxed);
        }));
    }
    let packet = QPacket {
        source: VPort {
            port: 15,
            stream_type: packet::StreamType::RVSec,
        },
        destination: VPort {
            port: 1,
            stream_type: packet::StreamType::RVSec,
        },
        packet_type: PacketType::Disconnect,
        signature: 20,
        ..Default::default()
    };
    server.handle_packet(logger, packet.clone(), address);
    let (n, _) = receiver.recv_from(&mut buf).unwrap();
    let (ack, _) = QPacket::from_bytes(ctx, &buf[..n]).unwrap();
    assert!(ack.flags.contains(PacketFlag::Ack));
    server.handle_packet(logger, packet, address);
    let clients = server.client_registry.clients.len();
    let indexes = server.client_registry.connection_id_session_ids.len();
    let live = server
        .client_registry
        .client_by_connection_id(ConnectionID(21))
        .is_some();
    let callbacks = calls.load(Ordering::Relaxed);
    let valid = clients == 1 && indexes == 1 && live && callbacks == usize::from(callback);
    println!("RETIREMENT case=disconnect callback={callback} clients={clients} indexes={indexes} live={live} callbacks={callbacks} valid={valid}");
    valid
}
#[test]
fn maintenance_pending_and_connected_retirement_observation() {
    sodiumoxide::init().unwrap();
    let ctx = Context::splinter_cell_blacklist();
    let logger = Logger::root(slog::Discard, o!());
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let address = receiver.local_addr().unwrap();
    let mut violations = usize::from(!pending_expiry(&ctx, &logger, address, &receiver));
    for callback in [false, true] {
        violations += usize::from(!connected_expiry(&ctx, &logger, address, callback));
    }
    for callback in [false, true] {
        violations += usize::from(!disconnect_retirement(
            &ctx, &logger, address, &receiver, callback,
        ));
    }
    println!("RETIREMENT_SUMMARY cases=5 violations={violations} native_methods=true loopback_only=true synthetic_age=true");
    assert_eq!(violations, 0);
}
