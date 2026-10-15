//! Real crate, real codec and owned loopback datagrams; no server.serve/services.
//! Registration belongs identically in RED and GREEN prudp.rs.
use super::*;
use sodiumoxide::crypto::secretbox;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

type TestServer<'a> = Server<'a, Box<dyn FnMut(ClientInfo<()>)>, Box<dyn FnMut(ClientInfo<()>)>, ()>;
type Calls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

struct Echo { calls: Calls }
impl StreamHandler<()> for Echo {
    fn handle(&self, _logger: &Logger, _ctx: &Context, ci: &mut ClientInfo<()>, data: &[u8],
        _clients: &ClientRegistry<()>, _socket: &UdpSocket) -> Result<Vec<u8>, packet::Error> {
        self.calls.lock().unwrap().push((ci.user_id.expect("real encrypted Connect must authenticate"), data.to_vec()));
        Ok(data.to_vec())
    }
}

fn ports() -> (VPort, VPort) {
    (VPort { port: 15, stream_type: packet::StreamType::RVSec },
     VPort { port: 1, stream_type: packet::StreamType::RVSec })
}

fn socket(ip: &str) -> UdpSocket {
    let s = UdpSocket::bind(format!("{ip}:0")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    s
}

fn make<'a>(ctx: &'a Context, logger: &Logger) -> (TestServer<'a>, Calls, Arc<AtomicUsize>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let closes = Arc::new(AtomicUsize::new(0));
    let mut registry = StreamHandlerRegistry::new(logger.clone());
    registry.register(ports().1, Box::new(Echo { calls: Arc::clone(&calls) }));
    let mut server: TestServer<'a> = Server::new(logger.clone(), ctx, registry);
    server.socket = Some(socket("127.0.0.1"));
    let observed = Arc::clone(&closes);
    server.disconnect_handler = Some(Box::new(move |_| { observed.fetch_add(1, Ordering::Relaxed); }));
    (server, calls, closes)
}

/// Exercise the same datagram decoding, checksum and dispatch stages as serve,
/// deriving client from recv_from rather than supplying a made-up peer address.
fn dispatch(server: &mut TestServer<'_>, logger: &Logger, sender: &UdpSocket, request: QPacket) {
    let wire = request.to_bytes(server.ctx);
    let destination = server.socket.as_ref().unwrap().local_addr().unwrap();
    assert!(destination.ip().is_loopback());
    assert!(sender.local_addr().unwrap().ip().is_loopback());
    assert_eq!(sender.send_to(&wire, destination).unwrap(), wire.len());
    let mut buf = [0u8; 4096];
    let (n, actual_source) = server.socket.as_ref().unwrap().recv_from(&mut buf).unwrap();
    assert_eq!(actual_source, sender.local_addr().unwrap());
    assert_eq!(&buf[..n], wire.as_slice());
    let (decoded, used) = QPacket::from_bytes(server.ctx, &buf[..n]).unwrap();
    assert_eq!(usize::try_from(used).unwrap(), n);
    decoded.validate(server.ctx, &buf[..n]).unwrap();
    assert_eq!(decoded.payload, request.payload);
    server.handle_packet(logger, decoded, actual_source);
}

fn receive(server: &TestServer<'_>, receiver: &UdpSocket) -> QPacket {
    let mut buf = [0u8; 4096];
    let (n, source) = receiver.recv_from(&mut buf).unwrap();
    assert_eq!(source, server.socket.as_ref().unwrap().local_addr().unwrap());
    let (p, used) = QPacket::from_bytes(server.ctx, &buf[..n]).unwrap();
    assert_eq!(usize::try_from(used).unwrap(), n);
    p.validate(server.ctx, &buf[..n]).unwrap();
    p
}

/// All sends in handle_packet are synchronous. Wait a bounded interval as well
/// to detect an unexpected ACK/response on either the owner or the other socket.
fn unexpected_datagrams(receiver: &UdpSocket) -> usize {
    receiver.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let mut count = 0;
    let mut buf = [0u8; 4096];
    loop {
        match receiver.recv_from(&mut buf) {
            Ok(_) => count += 1,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
            Err(e) => panic!("unexpected loopback receive error: {e}"),
        }
    }
    receiver.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    count
}

#[derive(Clone, Copy)]
struct Peer { signature: u32, session: u8 }

fn syn(server: &mut TestServer<'_>, logger: &Logger, owner: &UdpSocket) -> u32 {
    let (source, destination) = ports();
    dispatch(server, logger, owner, QPacket { source, destination,
        packet_type: PacketType::Syn, conn_signature: Some(0),
        flags: PacketFlag::HasSize.into(), ..Default::default() });
    let ack = receive(server, owner);
    assert!(ack.flags.contains(PacketFlag::Ack));
    let signature = ack.conn_signature.unwrap();
    assert_eq!(*server.new_clients.get(&signature).unwrap().address(), owner.local_addr().unwrap());
    signature
}

fn connect_request(ctx: &Context, signature: u32, session: u8, user: u32) -> QPacket {
    let (source, destination) = ports();
    let key = [0x57; 16];
    let ticket = KerberosTicketInternal { principle_id: user, valid_until: u64::MAX, session_key: key };
    let nonce = secretbox::gen_nonce();
    let mut sealed = nonce.as_ref().to_vec();
    sealed.extend(secretbox::seal(&ticket.to_bytes(), &nonce, &ctx.ticket_key));
    let mut challenge = user.to_bytes();
    challenge.extend(7u32.to_bytes());
    challenge.extend(0x1234_5678u32.to_bytes());
    let mut payload = sealed.to_bytes();
    payload.extend(crypt_key(&key, &challenge).to_bytes());
    QPacket { source, destination, packet_type: PacketType::Connect,
        conn_signature: Some(0x3456_0000 + user), signature, session_id: session,
        sequence: 1, payload, flags: PacketFlag::HasSize.into(), ..Default::default() }
}

fn finish_connect(server: &mut TestServer<'_>, logger: &Logger, owner: &UdpSocket,
    signature: u32, session: u8, user: u32) -> Peer {
    let request = connect_request(server.ctx, signature, session, user);
    dispatch(server, logger, owner, request);
    let ack = receive(server, owner);
    assert!(ack.flags.contains(PacketFlag::Ack));
    let response: Vec<u8> = ReadStream::from_bytes(&ack.payload).read().unwrap();
    assert_eq!(response, 0x1234_5679u32.to_bytes());
    assert!(!server.new_clients.contains_key(&signature));
    let ci = server.client_registry.clients.get(&signature).unwrap().borrow();
    assert_eq!(ci.user_id, Some(user));
    assert_eq!(ci.client_session, session);
    assert_eq!(ci.server_session, ack.session_id);
    assert_eq!(*ci.address(), owner.local_addr().unwrap());
    assert!(ci.connection_id.is_some());
    assert!(ci.ticket_fingerprint.is_some());
    Peer { signature, session }
}

fn connect(server: &mut TestServer<'_>, logger: &Logger, owner: &UdpSocket, session: u8, user: u32) -> Peer {
    let signature = syn(server, logger, owner);
    finish_connect(server, logger, owner, signature, session, user)
}

fn request(peer: Peer, kind: PacketType, sequence: u16, fragment: u8, payload: &[u8]) -> QPacket {
    let (source, destination) = ports();
    QPacket { source, destination, packet_type: kind, signature: peer.signature,
        session_id: peer.session, sequence, fragment_id: (kind == PacketType::Data).then_some(fragment),
        payload: payload.to_vec(), flags: PacketFlag::HasSize | PacketFlag::NeedAck,
        ..Default::default() }
}

fn receive_data(server: &TestServer<'_>, owner: &UdpSocket, expected: &[u8]) -> (QPacket, QPacket) {
    let ack = receive(server, owner);
    let response = receive(server, owner);
    assert!(ack.flags.contains(PacketFlag::Ack));
    assert_eq!(response.packet_type, PacketType::Data);
    assert!(!response.flags.contains(PacketFlag::Ack));
    assert_eq!(response.payload, expected);
    (ack, response)
}

#[derive(Debug, PartialEq, Eq)]
struct ClientSnapshot {
    server_sequence: u16, client_sequence: u16,
    client_signature: Option<u32>, server_signature: u32,
    client_session: u8, server_session: u8,
    fragments: BTreeMap<u8, Vec<u8>>, fragment_bytes: usize,
    fragment_start: Option<u16>, fragment_time: Option<Instant>,
    responses: BTreeMap<u16, Vec<QPacket>>, response_order: Vec<u16>, response_bytes: usize,
    address: SocketAddr, last_seen: Instant, connection_id: Option<u32>, user: Option<u32>,
    fingerprint: Option<[u8; 32]>,
}
fn client_snapshot(ci: &ClientInfo<()>) -> ClientSnapshot {
    ClientSnapshot {
        server_sequence: ci.server_sequence_id, client_sequence: ci.client_sequence_id,
        client_signature: ci.client_signature, server_signature: ci.server_signature,
        client_session: ci.client_session, server_session: ci.server_session,
        fragments: ci.packet_fragments.iter().map(|(k,v)| (*k,v.clone())).collect(),
        fragment_bytes: ci.packet_fragment_bytes, fragment_start: ci.packet_fragment_start_sequence,
        fragment_time: ci.packet_fragment_started_at,
        responses: ci.response_cache.iter().map(|(k,v)| (*k,v.clone())).collect(),
        response_order: ci.response_cache_order.iter().copied().collect(), response_bytes: ci.response_cache_bytes,
        address: *ci.address(), last_seen: ci.last_seen, connection_id: ci.connection_id.map(u32::from),
        user: ci.user_id, fingerprint: ci.ticket_fingerprint,
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    pending: BTreeMap<u32, ClientSnapshot>, clients: BTreeMap<u32, ClientSnapshot>,
    connections: BTreeMap<u32, u32>, next_connection: u32,
    calls: Vec<(u32, Vec<u8>)>, closes: usize,
}
fn snapshot(server: &TestServer<'_>, calls: &Calls, closes: &AtomicUsize) -> Snapshot {
    Snapshot {
        pending: server.new_clients.iter().map(|(k,ci)| (*k,client_snapshot(ci))).collect(),
        clients: server.client_registry.clients.iter().map(|(k,ci)| (*k,client_snapshot(&ci.borrow()))).collect(),
        connections: server.client_registry.connection_id_session_ids.iter().map(|(k,v)| (u32::from(*k),v.0)).collect(),
        next_connection: server.next_conn_id.load(Ordering::Relaxed),
        calls: calls.lock().unwrap().clone(), closes: closes.load(Ordering::Relaxed),
    }
}

#[derive(Clone, Copy, Debug)]
enum Attempt { Data, FirstFragment, FinalFragment, Replay, Ping, Disconnect }

fn rejected_established(attempt: Attempt) {
    sodiumoxide::init().unwrap();
    let ctx = Context::splinter_cell_blacklist();
    let logger = Logger::root(slog::Discard, o!());
    let (mut server, calls, closes) = make(&ctx, &logger);
    let owner = socket("127.0.0.1");
    let other_port = socket("127.0.0.1");
    let other_ip = socket("127.0.0.2");
    let peer = connect(&mut server, &logger, &owner, 7, 42);
    let _independent = connect(&mut server, &logger, &other_port, 9, 43);
    assert_ne!(owner.local_addr().unwrap().port(), other_port.local_addr().unwrap().port());
    dispatch(&mut server, &logger, &owner, request(peer, PacketType::Data, 10, 0, b"cached"));
    receive_data(&server, &owner, b"cached");
    if !matches!(attempt, Attempt::Data) {
        dispatch(&mut server, &logger, &owner, request(peer, PacketType::Data, 20, 1, b"first"));
        assert!(receive(&server, &owner).flags.contains(PacketFlag::Ack));
    }
    let probe = match attempt {
        Attempt::Data => request(peer, PacketType::Data, 30, 0, b"new"),
        Attempt::FirstFragment => request(peer, PacketType::Data, 40, 1, b"replace"),
        Attempt::FinalFragment => request(peer, PacketType::Data, 21, 0, b"tail"),
        Attempt::Replay => request(peer, PacketType::Data, 10, 0, b"cached"),
        Attempt::Ping => request(peer, PacketType::Ping, 30, 0, b""),
        Attempt::Disconnect => request(peer, PacketType::Disconnect, 30, 0, b""),
    };
    for (label, sender, wrong_session) in [
        ("other_ip", &other_ip, false), ("other_port", &other_port, false), ("wrong_session", &owner, true),
    ] {
        let mut packet = probe.clone();
        if wrong_session { packet.session_id = peer.session.wrapping_add(1); }
        let before = snapshot(&server, &calls, &closes);
        dispatch(&mut server, &logger, sender, packet);
        let after = snapshot(&server, &calls, &closes);
        let datagrams = unexpected_datagrams(&owner) + unexpected_datagrams(&other_port) + unexpected_datagrams(&other_ip);
        println!("PEER_OWNERSHIP case={attempt:?} source={label} unchanged={} unexpected_datagrams={datagrams}", before == after);
        assert_eq!(after, before, "{attempt:?}/{label} changed connection, cache, fragment, invocation or callback state");
        assert_eq!(datagrams, 0, "{attempt:?}/{label} emitted ACK or response");
    }
}

#[test] fn established_data_rejects_wrong_ip_port_and_session() { rejected_established(Attempt::Data); }
#[test] fn first_fragment_rejects_wrong_ip_port_and_session() { rejected_established(Attempt::FirstFragment); }
#[test] fn final_fragment_rejects_wrong_ip_port_and_session() { rejected_established(Attempt::FinalFragment); }
#[test] fn cached_replay_rejects_wrong_ip_port_and_session() { rejected_established(Attempt::Replay); }
#[test] fn ping_rejects_wrong_ip_port_and_session() { rejected_established(Attempt::Ping); }
#[test] fn disconnect_rejects_wrong_ip_port_and_session() { rejected_established(Attempt::Disconnect); }

#[test]
fn pending_connect_rejects_wrong_ip_and_port_without_consuming_syn() {
    sodiumoxide::init().unwrap();
    let ctx = Context::splinter_cell_blacklist();
    let logger = Logger::root(slog::Discard, o!());
    let (mut server, calls, closes) = make(&ctx, &logger);
    let owner = socket("127.0.0.1");
    let other_port = socket("127.0.0.1");
    let other_ip = socket("127.0.0.2");
    let _independent = connect(&mut server, &logger, &other_port, 9, 43);
    let signature = syn(&mut server, &logger, &owner);
    for (label, sender) in [("other_ip", &other_ip), ("other_port", &other_port)] {
        let before = snapshot(&server, &calls, &closes);
        // Connect establishes client_session; pending's default 0 is NOT a valid
        // session to enforce here. Only the SYN owner's endpoint is already bound.
        let probe = connect_request(&ctx, signature, 7, 42);
        dispatch(&mut server, &logger, sender, probe);
        let after = snapshot(&server, &calls, &closes);
        let datagrams = unexpected_datagrams(&owner) + unexpected_datagrams(&other_port) + unexpected_datagrams(&other_ip);
        println!("PEER_OWNERSHIP case=Connect source={label} unchanged={} unexpected_datagrams={datagrams}", before == after);
        assert_eq!(after, before, "foreign Connect consumed pending SYN or mutated an established peer");
        assert_eq!(datagrams, 0, "foreign Connect emitted a response");
    }
    finish_connect(&mut server, &logger, &owner, signature, 7, 42);
    assert_eq!(server.client_registry.clients.len(), 2);
    assert_eq!(server.client_registry.connection_id_session_ids.len(), 2);
    assert_eq!(closes.load(Ordering::Relaxed), 0);
}

#[test]
fn two_real_connections_keep_client_sessions_fragments_replays_and_disconnect_independent() {
    sodiumoxide::init().unwrap();
    let ctx = Context::splinter_cell_blacklist();
    let logger = Logger::root(slog::Discard, o!());
    let (mut server, calls, closes) = make(&ctx, &logger);
    let a = socket("127.0.0.1");
    let b = socket("127.0.0.1");
    let pa = connect(&mut server, &logger, &a, 7, 42);
    let pb = connect(&mut server, &logger, &b, 9, 43);
    assert_ne!(pa.signature, pb.signature);
    // Real SYN+encrypted Connect above created these CIs. Set only outbound
    // session values deterministically to test the direction distinction without
    // relying on random server_session differing from client_session by chance.
    server.client_registry.clients.get(&pa.signature).unwrap().borrow_mut().server_session = 201;
    server.client_registry.clients.get(&pb.signature).unwrap().borrow_mut().server_session = 202;
    let b_before = client_snapshot(&server.client_registry.clients.get(&pb.signature).unwrap().borrow());
    dispatch(&mut server, &logger, &a, request(pa, PacketType::Ping, 2, 0, b""));
    let ack = receive(&server, &a);
    assert!(ack.flags.contains(PacketFlag::Ack));
    assert_eq!(ack.session_id, 201);
    assert_eq!(client_snapshot(&server.client_registry.clients.get(&pb.signature).unwrap().borrow()), b_before);
    let initial = request(pa, PacketType::Data, 10, 0, b"a");
    dispatch(&mut server, &logger, &a, initial.clone());
    let (_, first) = receive_data(&server, &a, b"a");
    assert_eq!(first.session_id, 201);
    dispatch(&mut server, &logger, &a, initial);
    let (_, replay) = receive_data(&server, &a, b"a");
    assert_eq!(replay, first);
    assert_eq!(calls.lock().unwrap().as_slice(), &[(42, b"a".to_vec())]);
    dispatch(&mut server, &logger, &b, request(pb, PacketType::Data, 10, 0, b"b"));
    assert_eq!(receive_data(&server, &b, b"b").1.session_id, 202);
    dispatch(&mut server, &logger, &a, request(pa, PacketType::Data, 20, 1, b"first"));
    assert!(receive(&server, &a).flags.contains(PacketFlag::Ack));
    assert_eq!(server.client_registry.clients.get(&pa.signature).unwrap().borrow().packet_fragment_bytes, 5);
    assert!(server.client_registry.clients.get(&pb.signature).unwrap().borrow().packet_fragments.is_empty());
    dispatch(&mut server, &logger, &a, request(pa, PacketType::Data, 21, 0, b"tail"));
    receive_data(&server, &a, b"firsttail");
    assert!(server.client_registry.clients.get(&pa.signature).unwrap().borrow().packet_fragments.is_empty());
    assert_eq!(calls.lock().unwrap().as_slice(), &[(42, b"a".to_vec()), (43, b"b".to_vec()), (42, b"firsttail".to_vec())]);
    let b_before = client_snapshot(&server.client_registry.clients.get(&pb.signature).unwrap().borrow());
    let a_connection = server.client_registry.clients.get(&pa.signature).unwrap().borrow().connection_id.unwrap();
    dispatch(&mut server, &logger, &a, request(pa, PacketType::Disconnect, 22, 0, b""));
    assert!(receive(&server, &a).flags.contains(PacketFlag::Ack));
    assert!(!server.client_registry.clients.contains_key(&pa.signature));
    assert!(server.client_registry.client_by_connection_id(a_connection).is_none());
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    assert_eq!(client_snapshot(&server.client_registry.clients.get(&pb.signature).unwrap().borrow()), b_before);
    dispatch(&mut server, &logger, &b, request(pb, PacketType::Data, 11, 0, b"still-live"));
    receive_data(&server, &b, b"still-live");
    assert_eq!(server.client_registry.clients.len(), 1);
    assert_eq!(server.client_registry.connection_id_session_ids.len(), 1);
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    assert_eq!(unexpected_datagrams(&a) + unexpected_datagrams(&b), 0);
    println!("PEER_OWNERSHIP valid=two_real_connections client_session_direction=true cached_replay=true fragments=true callback_once=true");
}
