/// This module handles the PRUDP protocol, which is a custom reliable UDP protocol.
pub mod packet;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::net::{self};
use std::sync::atomic::AtomicU32;
use std::time::Duration;
use std::time::Instant;

use slog::debug;
use slog::error;
use slog::o;
use slog::Logger;

use self::packet::crypt_key;
use self::packet::PacketFlag;
use self::packet::PacketType;
use self::packet::QPacket;
use self::packet::StreamHandler;
use self::packet::StreamHandlerRegistry;
use self::packet::VPort;
use crate::kerberos::KerberosTicketInternal;
use crate::rmc::basic::ReadStream;
use crate::rmc::basic::ToStream;
use crate::ClientInfo;
use crate::ConnectionID;
use crate::Context;
use crate::Signature;

const MAX_PAYLOAD_SIZE: usize = 1000;
// The wire fragment ID is one byte; zero is the final fragment.
pub(crate) const MAX_OUTBOUND_MESSAGE_BYTES: usize = MAX_PAYLOAD_SIZE * (u8::MAX as usize + 1);
const MAX_INBOUND_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_CACHED_RESPONSES: usize = 64;
const MAX_CACHED_RESPONSE_BYTES: usize = 512 * 1024;
const FRAGMENT_ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
const CLIENT_CLEANUP_INTERVAL: Duration = Duration::from_secs(1);
// Resource admission limits for fallible-close transports, not room/player limits.
// Closing owners remain part of clients and count exactly once in the total.
const MAX_FALLIBLE_TRANSPORT_OWNERS: usize = 256;
const MAX_FALLIBLE_PENDING_SYNS: usize = 64;
const MAX_SIGNATURE_RESERVATION_ATTEMPTS: usize = 8;

fn reset_fragment_state<T>(ci: &mut ClientInfo<T>) {
    ci.packet_fragments.clear();
    ci.packet_fragment_bytes = 0;
    ci.packet_fragment_start_sequence = None;
    ci.packet_fragment_started_at = None;
}

fn expire_fragment_state<T>(ci: &mut ClientInfo<T>) {
    if ci.packet_fragment_started_at.is_some_and(|started| started.elapsed() > FRAGMENT_ASSEMBLY_TIMEOUT) {
        reset_fragment_state(ci);
    }
}

fn cache_fragment<T>(ci: &mut ClientInfo<T>, fragment_id: u8, sequence: u16, payload: Vec<u8>) -> Result<bool, &'static str> {
    expire_fragment_state(ci);
    if fragment_id == 0 {
        return Err("fragment id outside protocol limit");
    }

    // The Blacklist client numbers payload chunks from one upward while the
    // reliable packet sequence also increases. Fragment N therefore belongs
    // to the group whose first sequence is sequence - (N - 1). Fragment zero
    // follows the highest numbered chunk.
    let start_sequence = sequence.wrapping_sub(u16::from(fragment_id - 1));
    if ci.packet_fragment_start_sequence != Some(start_sequence) {
        reset_fragment_state(ci);
        ci.packet_fragment_start_sequence = Some(start_sequence);
        ci.packet_fragment_started_at = Some(Instant::now());
    }

    if let Some(existing) = ci.packet_fragments.get(&fragment_id) {
        return if *existing == payload { Ok(false) } else { Err("conflicting duplicate fragment") };
    }

    let total = ci.packet_fragment_bytes.checked_add(payload.len()).ok_or("fragment byte count overflow")?;
    if total > MAX_INBOUND_MESSAGE_BYTES {
        return Err("fragmented message exceeds byte limit");
    }

    ci.packet_fragments.insert(fragment_id, payload);
    ci.packet_fragment_bytes = total;
    Ok(true)
}

fn finish_fragmented_payload<T>(ci: &mut ClientInfo<T>, sequence: u16, final_payload: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    expire_fragment_state(ci);
    if final_payload.len() > MAX_INBOUND_MESSAGE_BYTES {
        return Err("final fragment exceeds byte limit");
    }
    if ci.packet_fragments.is_empty() {
        return Ok(final_payload);
    }
    let highest = *ci.packet_fragments.keys().max().ok_or("empty fragment group")?;
    if ci
        .packet_fragment_start_sequence
        .is_none_or(|start_sequence| start_sequence.wrapping_add(u16::from(highest)) != sequence)
    {
        return Err("fragment group sequence mismatch");
    }
    if ci.packet_fragments.len() != usize::from(highest) {
        return Err("fragment group has a missing id");
    }
    let total = ci.packet_fragment_bytes.checked_add(final_payload.len()).ok_or("reassembled byte count overflow")?;
    if total > MAX_INBOUND_MESSAGE_BYTES {
        return Err("reassembled message exceeds byte limit");
    }

    let fragments = std::mem::take(&mut ci.packet_fragments);
    ci.packet_fragment_bytes = 0;
    ci.packet_fragment_start_sequence = None;
    ci.packet_fragment_started_at = None;
    let mut output = Vec::new();
    output.try_reserve_exact(total).map_err(|_| "reassembled message allocation failed")?;
    // The Blacklist client sends fragment one first and increases the id for
    // every subsequent payload chunk, then terminates the group with zero.
    for fragment_id in 1..=highest {
        output.extend_from_slice(fragments.get(&fragment_id).ok_or("fragment group has a missing id")?);
    }
    output.extend_from_slice(&final_payload);
    Ok(output)
}

fn cache_response<T>(ci: &mut ClientInfo<T>, request_sequence: u16, responses: Vec<QPacket>) {
    if responses.is_empty() {
        return;
    }

    let Some(response_bytes) = responses.iter().try_fold(0usize, |total, response| total.checked_add(response.payload.len())) else {
        return;
    };
    if response_bytes > MAX_CACHED_RESPONSE_BYTES {
        return;
    }

    if let Some(previous) = ci.response_cache.insert(request_sequence, responses) {
        let previous_bytes = previous.iter().map(|response| response.payload.len()).sum::<usize>();
        ci.response_cache_bytes = ci.response_cache_bytes.saturating_sub(previous_bytes);
        ci.response_cache_order.retain(|sequence| *sequence != request_sequence);
    }
    ci.response_cache_bytes += response_bytes;
    ci.response_cache_order.push_back(request_sequence);

    while ci.response_cache_order.len() > MAX_CACHED_RESPONSES || ci.response_cache_bytes > MAX_CACHED_RESPONSE_BYTES {
        if let Some(sequence) = ci.response_cache_order.pop_front() {
            if let Some(response) = ci.response_cache.remove(&sequence) {
                let response_bytes = response.iter().map(|packet| packet.payload.len()).sum::<usize>();
                ci.response_cache_bytes = ci.response_cache_bytes.saturating_sub(response_bytes);
            }
        }
    }
}

fn cached_response<T>(ci: &ClientInfo<T>, request_sequence: u16) -> Option<Vec<QPacket>> {
    ci.response_cache.get(&request_sequence).cloned()
}

fn prepare_response<T>(mut response: QPacket, ci: &mut ClientInfo<T>) -> QPacket {
    response.sequence = ci.server_sequence_id;
    ci.server_sequence_id = ci.server_sequence_id.wrapping_add(1);
    response.flags.insert(PacketFlag::HasSize);
    response.flags.insert(PacketFlag::NeedAck);
    response.flags.insert(PacketFlag::Reliable);
    response.signature = ci.client_signature.unwrap_or_default();
    response.session_id = ci.server_session;
    response
}

/// A registry for clients.
#[derive(Default)]
pub struct ClientRegistry<T> {
    clients: HashMap<u32, RefCell<ClientInfo<T>>>,
    connection_id_session_ids: HashMap<ConnectionID, Signature>,
    closing_clients: BTreeMap<u32, PendingClose>,
}

impl<T> ClientRegistry<T> {
    /// Returns a client by its connection ID.
    #[must_use]
    pub fn client_by_connection_id(&self, conn_id: ConnectionID) -> Option<&RefCell<ClientInfo<T>>> {
        self.connection_id_session_ids.get(&conn_id)
            .filter(|sig| !self.closing_clients.contains_key(&sig.0))
            .and_then(|sig| self.clients.get(&sig.0))
    }
}

/// Why a connection has entered terminal cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The peer explicitly disconnected.
    Disconnect,
    /// The transport liveness timeout elapsed.
    Expired,
}

#[derive(Clone, Copy)]
struct PendingClose {
    reason: CloseReason,
    next_attempt: Instant,
    retry_delay: Duration,
}

impl PendingClose {
    fn new(reason: CloseReason) -> Self {
        Self { reason, next_attempt: Instant::now(), retry_delay: Duration::from_secs(1) }
    }
}

/// A PRUDP server.
pub struct Server<'a, ECH, DH, T = ()>
where
    ECH: FnMut(ClientInfo<T>),
    DH: FnMut(ClientInfo<T>),
{
    logger: slog::Logger,
    registry: StreamHandlerRegistry<T>,
    socket: Option<net::UdpSocket>,
    ctx: &'a Context,
    new_clients: HashMap<u32, ClientInfo<T>>,
    client_registry: ClientRegistry<T>,
    /// A handler for user-defined packets.
    pub user_handler: Option<fn(logger: &Logger, packet: QPacket, client: SocketAddr, sock: &net::UdpSocket)>,
    /// A handler for expired clients.
    pub expired_client_handler: Option<ECH>,
    /// A handler for disconnected clients.
    pub disconnect_handler: Option<DH>,
    /// Fallible close acknowledgement. False retains the terminal transport owner.
    pub close_handler: Option<Box<dyn FnMut(&ClientInfo<T>, CloseReason) -> bool + 'a>>,
    close_retry_cursor: Option<u32>,
    next_conn_id: AtomicU32,
}

impl<ECH, DH, T> Server<'_, ECH, DH, T>
where
    T: Default,
    ECH: FnMut(ClientInfo<T>),
    DH: FnMut(ClientInfo<T>),
{
    /// Creates a new PRUDP server.
    #[must_use]
    pub fn new(logger: slog::Logger, ctx: &Context, registry: StreamHandlerRegistry<T>) -> Server<'_, ECH, DH, T> {
        let client_registry = ClientRegistry {
            clients: HashMap::default(),
            connection_id_session_ids: HashMap::default(),
            closing_clients: BTreeMap::new(),
        };
        Server {
            logger,
            registry,
            socket: None,
            ctx,
            new_clients: HashMap::default(),
            client_registry,
            user_handler: None,
            expired_client_handler: None,
            disconnect_handler: None,
            close_handler: None,
            close_retry_cursor: None,
            next_conn_id: AtomicU32::new(0x3AAA_AAAA),
        }
    }

    /// Registers a stream handler for a virtual port.
    pub fn register(&mut self, vport: VPort, protocol: Box<dyn StreamHandler<T>>) {
        self.registry.register(vport, protocol);
    }

    /// Binds the server to a socket address.
    pub fn bind<A: net::ToSocketAddrs>(&mut self, addrs: A) -> io::Result<()> {
        self.socket = Some(net::UdpSocket::bind(addrs)?);
        info!(self.logger, "Listening on {}", self.socket.as_ref().unwrap().local_addr().unwrap());
        Ok(())
    }

    /// Starts the server's main loop.
    pub fn serve(mut self) {
        let socket = self.socket.as_ref().expect("UDP socket required").try_clone().expect("Couldn't clone socket");
        socket.set_read_timeout(Some(CLIENT_CLEANUP_INTERVAL)).expect("error setting read timeout");
        let mut buf = vec![0u8; 1024];
        let mut last_cleanup = Instant::now();
        'outer: loop {
            // Other clients' traffic must not postpone stale connection cleanup.
            if last_cleanup.elapsed() >= CLIENT_CLEANUP_INTERVAL {
                self.clear_clients();
                last_cleanup = Instant::now();
            }
            let (nread, client) = match socket.recv_from(&mut buf) {
                Ok(x) => x,
                Err(e) => {
                    if !matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
                        error!(self.logger, "recv_from failed: {}", e);
                    }
                    continue;
                }
            };
            let logger = self.logger.new(o!("client" => client));
            let mut data = &buf[..nread];

            while !data.is_empty() {
                let (packet, nparsed) = match QPacket::from_bytes(self.ctx, data) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(logger, "Invalid packet received"; "error" =>  %e);
                        continue 'outer;
                    }
                };
                #[allow(clippy::cast_possible_truncation)]
                let (packet_data, next_data) = data.split_at(nparsed as usize);
                data = next_data;
                trace!(logger, "-> {:02x?}", packet_data);

                if let Err(e) = packet.validate(self.ctx, packet_data) {
                    error!(logger, "Invalid packet received: {:?}", packet; "error" =>  %e);
                    continue;
                }

                self.handle_packet(&logger.new(o!("seq" => packet.sequence, "session" => packet.session_id)), packet, client);
            }
        }
    }

    /// Handles a received packet.
    fn handle_packet(&mut self, logger: &Logger, packet: QPacket, client: SocketAddr) {
        debug!(logger, "packet: {:?}", packet);
        if packet.flags.contains(PacketFlag::Ack) {
            debug!(logger, "Received ACK");
            return;
        }
        // A signature identifies a connection, but does not authorize a different
        // endpoint or a different inbound session to mutate it. Guard before ACK,
        // liveness, fragment/replay state, stream dispatch or retirement changes.
        // Outbound packets use server_session; inbound packets use client_session.
        if matches!(packet.packet_type, PacketType::Data | PacketType::Ping | PacketType::Disconnect) {
            let owned = self.client_registry.clients.get(&packet.signature).is_some_and(|ci| {
                let ci = ci.borrow();
                *ci.address() == client && ci.client_session == packet.session_id
            });
            if !owned || self.client_registry.closing_clients.contains_key(&packet.signature) {
                return;
            }
        }
        match packet.packet_type {
            PacketType::Syn => self.handle_syn(logger, packet, client),
            PacketType::Connect => self.handle_connect(logger, packet, client),
            PacketType::Data => self.handle_data(logger, packet, client),
            PacketType::Disconnect => {
                let Some(ci) = self.client_registry.clients.get(&packet.signature) else {
                    return;
                };
                info!(logger, "Client disconnected"; "signature" => packet.signature, "session" => packet.session_id);
                if self.send_ack(logger, &client, &packet, &ci.borrow(), false).is_err() {
                    // ignore
                }
                self.try_retire_client(packet.signature, CloseReason::Disconnect);
            }
            PacketType::Ping => {
                let Some(ci) = self.client_registry.clients.get(&packet.signature) else {
                    return;
                };
                ci.borrow_mut().seen();
                if self.send_ack(logger, &client, &packet, &ci.borrow(), false).is_err() {
                    // ignore
                }
            }
            PacketType::User => {
                if self.user_handler.is_none() {
                    error!(logger, "unsupported user packet");
                } else {
                    (self.user_handler.as_ref().unwrap())(logger, packet, client, self.socket.as_ref().unwrap());
                }
            }
            PacketType::Route | PacketType::Raw => {
                warn!(
                    logger,
                    "Dropping unsupported PRUDP packet type";
                    "packet_type" => format!("{:?}", packet.packet_type),
                    "client" => client.to_string(),
                );
            }
        }
    }

    /// Handles a data packet.
    fn handle_data(&mut self, logger: &Logger, packet: QPacket, client: SocketAddr) {
        #![allow(clippy::cast_possible_truncation)]

        debug!(logger, "Handling data packet");
        let Some(ci) = self.client_registry.clients.get(&packet.signature) else {
            warn!(logger, "client is unknown!");
            return;
        };
        let logger = logger.new(o!("pid" => ci.borrow().user_id));
        let ci = &mut ci.borrow_mut();
        ci.seen();
        if let Err(e) = self.send_ack(&logger, &client, &packet, &*ci, false) {
            error!(logger, "Error sending ack"; "error" => %e);
        } else {
            debug!(logger, "Send ack");
        }
        if packet.fragment_id.is_none_or(|fragment_id| fragment_id == 0) && self.replay_cached_response(&logger, &client, packet.sequence, ci) {
            return;
        }
        let payload = if let Some(fid) = packet.fragment_id {
            if fid != 0 {
                match cache_fragment(ci, fid, packet.sequence, packet.payload) {
                    Ok(true) => info!(logger, "Caching fragment {}", fid),
                    Ok(false) => debug!(logger, "Ignoring duplicate fragment {}", fid),
                    Err(reason) => {
                        warn!(logger, "Dropping fragmented PRUDP message"; "reason" => reason, "fragment" => fid);
                        reset_fragment_state(ci);
                    }
                }
                return;
            }

            match finish_fragmented_payload(ci, packet.sequence, packet.payload) {
                Ok(payload) => payload,
                Err(reason) => {
                    warn!(logger, "Dropping fragmented PRUDP message"; "reason" => reason, "fragment" => fid);
                    reset_fragment_state(ci);
                    return;
                }
            }
        } else {
            packet.payload
        };
        let diagnostic_request = match crate::rmc::Packet::from_bytes(&payload) {
            Ok(crate::rmc::Packet::Request(request)) if matches!((request.protocol_id, request.method_id), (11, 1 | 4) | (5003, 1)) => {
                Some((request.protocol_id, request.method_id, request.call_id))
            }
            _ => None,
        };
        let resp = self
            .registry
            .handle_packet(&logger, self.ctx, ci, &packet.destination, &payload, &self.client_registry, self.socket.as_ref().unwrap());
        match resp {
            Some(Ok(payload)) => {
                let chunks = payload.chunks(MAX_PAYLOAD_SIZE);
                let mut responses = Vec::with_capacity(chunks.len());
                for (fid, chunk) in (0..chunks.len()).rev().zip(chunks) {
                    let response = prepare_response(
                        QPacket {
                            source: packet.destination,
                            destination: packet.source,
                            packet_type: PacketType::Data,
                            payload: chunk.to_vec(),
                            fragment_id: Some(fid as u8),
                            ..Default::default()
                        },
                        ci,
                    );
                    responses.push(response);
                }
                cache_response(ci, packet.sequence, responses.clone());
                let fragment_count = responses.len();
                for response in responses {
                    let response_sequence = response.sequence;
                    let fragment_index = response.fragment_id.unwrap_or_default();
                    if let Err(e) = self.send_packet(&logger, &client, response) {
                        error!(logger, "Error sending response"; "error" => %e);
                    } else {
                        trace!(logger, "Send response");
                        if let Some((protocol_id, method_id, call_id)) = diagnostic_request {
                            info!(logger, "RMC diagnostic response sent";
                                "protocol_id" => protocol_id,
                                "method_id" => method_id,
                                "call" => call_id,
                                "request_sequence" => packet.sequence,
                                "response_sequence" => response_sequence,
                                "fragment_index" => fragment_index,
                                "fragment_count" => fragment_count);
                        }
                    }
                }
            }
            None => {
                error!(logger, "No handler found");
            }
            Some(Err(_)) => {
                error!(logger, "Handler failed");
            }
        }
    }

    fn replay_cached_response(&self, logger: &Logger, client: &SocketAddr, request_sequence: u16, ci: &mut ClientInfo<T>) -> bool {
        let Some(responses) = cached_response(ci, request_sequence) else {
            return false;
        };
        reset_fragment_state(ci);
        info!(
            logger,
            "Replaying cached response for duplicate request";
            "request_sequence" => request_sequence,
            "response_packets" => responses.len(),
        );
        for response in responses {
            if let Err(e) = self.send_packet(logger, client, response) {
                error!(logger, "Error replaying cached response"; "error" => %e);
            }
        }
        true
    }

    // Reserve before ACK: accepted pending SYNs consume a total-owner slot.
    // CONNECT only promotes that reservation; it must not apply this new-slot gate.
    fn reserve_syn_client<F>(&mut self, mut next_candidate: F) -> Option<u32>
    where
        F: FnMut() -> Option<ClientInfo<T>>,
    {
        if self.close_handler.is_some()
            && (self.new_clients.len() >= MAX_FALLIBLE_PENDING_SYNS
                || self.new_clients.len().saturating_add(self.client_registry.clients.len()) >= MAX_FALLIBLE_TRANSPORT_OWNERS)
        {
            return None;
        }
        for _ in 0..MAX_SIGNATURE_RESERVATION_ATTEMPTS {
            let ci = next_candidate()?;
            let signature = ci.server_signature;
            // Applies to every transport: a random collision cannot replace an
            // active/closing owner, nor an earlier pending handshake.
            if self.client_registry.clients.contains_key(&signature) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = self.new_clients.entry(signature) {
                slot.insert(ci);
                return Some(signature);
            }
        }
        None
    }

    /// Handles a SYN packet.
    fn handle_syn(&mut self, logger: &Logger, mut packet: QPacket, client: SocketAddr) {
        debug!(logger, "Handling syn packet");
        let Some(sig) = self.reserve_syn_client(|| Some(ClientInfo::new(client))) else {
            return;
        };

        packet.conn_signature = Some(sig);

        let ci = self.new_clients.get(&sig).unwrap();

        if let Err(e) = self.send_ack(logger, &client, &packet, ci, false) {
            error!(logger, "Error sending syn ack packet"; "error" => %e);
        }
    }

    /// Handles a CONNECT packet.
    fn handle_connect(&mut self, logger: &Logger, mut packet: QPacket, client: SocketAddr) {
        debug!(logger, "Handling connect packet");
        let Some(signature) = packet.conn_signature else {
            error!(logger, "Client {:x} did not provide a connection signature. This should not happen", packet.signature);
            return;
        };

        // Reserve the vacant active key before consuming pending state. A stale
        // CONNECT cannot overwrite a live/closing ticket or consume its namesake SYN.
        let std::collections::hash_map::Entry::Vacant(active_slot) = self.client_registry.clients.entry(packet.signature) else {
            return;
        };

        // SYN already bound the pending connection to this socket endpoint.
        // Connect establishes client_session, so the pending default session is
        // deliberately not compared here. Do not consume another peer's SYN.
        if self.new_clients.get(&packet.signature).is_some_and(|ci| *ci.address() != client) {
            return;
        }
        let Some(mut ci) = self.new_clients.remove(&packet.signature) else {
            warn!(logger, "Unknown client {:x} tried to connect. Ignoring the attempt", packet.signature);
            return;
        };
        ci.client_signature = Some(signature);
        ci.server_session = rand::random();
        ci.client_session = packet.session_id;

        let ci = {
            active_slot.insert(RefCell::new(ci));
            let ci = self.client_registry.clients.get(&packet.signature).unwrap();
            ci
        };

        if !packet.payload.is_empty() {
            let data = std::mem::take(&mut packet.payload);
            let mut s = ReadStream::from_bytes(&data);
            let ticket_key = &self.ctx.ticket_key;
            let closing_clients = &self.client_registry.closing_clients;
            let clients = &self.client_registry.clients;
            let cids = &mut self.client_registry.connection_id_session_ids;
            let next_conn_id = &mut self.next_conn_id;
            let res = move || -> Result<_, crate::rmc::basic::FromStreamError> {
                let ticket: Vec<u8> = s.read()?;
                let request_data: Vec<u8> = s.read()?;

                let ti = KerberosTicketInternal::open(&ticket, ticket_key)?;

                if ti.valid_until
                    < std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .as_ref()
                        .map(std::time::Duration::as_secs)
                        .unwrap_or_default()
                {
                    return Ok(Some(vec![]));
                }
                let data = crypt_key(ti.session_key.as_ref(), &request_data);

                #[allow(clippy::items_after_statements)]
                #[derive(FromStream, Debug)]
                struct ConnectData {
                    _user_pid: u32,
                    _connection_id: u32,
                    challenge: u32,
                }

                let cd: ConnectData = ReadStream::from_bytes(&data).read()?;
                let fingerprint = crate::kerberos::ticket_fingerprint(&ti.session_key);
                // A closing owner will delete this exact ticket row. Until that
                // succeeds, a second transport must not publish the same ticket.
                // This is not a user ban: a different ticket or user may connect.
                let ticket_is_closing = closing_clients.keys().any(|signature| {
                    clients.get(signature).is_some_and(|owner| {
                        let owner = owner.borrow();
                        owner.user_id == Some(ti.principle_id) && owner.ticket_fingerprint == Some(fingerprint)
                    })
                });
                if ticket_is_closing {
                    return Ok(None);
                }
                let id = next_conn_id.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                ci.borrow_mut().user_id.replace(ti.principle_id);
                ci.borrow_mut().ticket_fingerprint = Some(fingerprint);
                ci.borrow_mut().connection_id.replace(ConnectionID(id));
                cids.insert(ci.borrow().connection_id.unwrap(), Signature(packet.signature));

                let resp = cd.challenge.wrapping_add(1);
                let resp = resp.to_bytes();

                Ok(Some(resp.to_bytes()))
            }();

            match res {
                Ok(Some(resp)) => packet.payload = resp,
                Ok(None) => {
                    // Only our unpublished, anonymous reservation is discarded.
                    // No connection id was issued and no success ACK is sent.
                    self.client_registry.clients.remove(&packet.signature);
                    return;
                }
                Err(e) => error!(logger, "Error parsing ticket"; "error" => %e),
            }
        }

        packet.conn_signature = Some(0);

        if let Err(e) = self.send_ack(logger, &client, &packet, &ci.borrow(), !packet.payload.is_empty()) {
            error!(logger, "Error sending syn ack packet"; "error" => %e);
        }
        info!(logger, "New client connected"; "signature" => packet.signature, "session" => packet.session_id);
    }

    /// Sends a packet to a client.
    fn send_packet(&self, logger: &Logger, src: &SocketAddr, resp: QPacket) -> Result<usize, Box<dyn std::error::Error>> {
        send_packet(logger, self.ctx, src, self.socket.as_ref().unwrap(), resp)
    }

    /// Sends an ACK packet to a client.
    fn send_ack(&self, logger: &Logger, src: &SocketAddr, packet: &QPacket, ci: &ClientInfo<T>, keep_payload: bool) -> Result<usize, Box<dyn std::error::Error>> {
        let mut resp = packet.clone();
        resp.source = packet.destination;
        resp.destination = packet.source;
        resp.flags = PacketFlag::Ack | PacketFlag::HasSize;
        resp.signature = ci.client_signature.unwrap_or_default();
        resp.session_id = ci.server_session;
        if !keep_payload {
            resp.payload.clear();
        }
        resp.sequence = packet.sequence;
        self.send_packet(logger, src, resp)
    }

    /// Deterministic candidates for the same reservation branch used by SYN.
    /// Bypasses random generation and ACK only, not capacity/collision decisions.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    pub fn maintenance_reserve_syn(&mut self, address: SocketAddr, signatures: &[u32]) -> Option<u32> {
        let mut signatures = signatures.iter();
        self.reserve_syn_client(|| signatures.next().map(|&signature| {
            let mut ci = ClientInfo::new(address);
            ci.server_signature = signature;
            ci
        }))
    }

    /// Resource limits installed for fallible-close transports, not game room size.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    #[must_use]
    pub fn maintenance_capacity_limits(&self) -> Option<(usize, usize)> {
        self.close_handler.as_ref().map(|_| (MAX_FALLIBLE_TRANSPORT_OWNERS, MAX_FALLIBLE_PENDING_SYNS))
    }

    /// Observe the original SYN endpoint without mutating a pending reservation.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    #[must_use]
    pub fn maintenance_pending_address(&self, signature: u32) -> Option<SocketAddr> {
        self.new_clients.get(&signature).map(|ci| *ci.address())
    }

    /// Seed a synthetic transport owner without opening a socket (maintenance tests only).
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    pub fn maintenance_seed_connected(&mut self, signature: u32, mut ci: ClientInfo<T>, expired: bool) {
        assert!(!self.client_registry.clients.contains_key(&signature));
        ci.server_signature = signature;
        ci.connection_id = Some(ConnectionID(signature));
        if expired {
            ci.last_seen = Instant::now() - SESSION_TIMEOUT - Duration::from_secs(1);
        }
        self.client_registry.connection_id_session_ids.insert(ConnectionID(signature), Signature(signature));
        self.client_registry.clients.insert(signature, RefCell::new(ci));
    }

    /// Run the production expiry/retry tick without a receive loop.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    pub fn maintenance_tick(&mut self) {
        self.clear_clients();
    }

    /// Deliver an already-parsed packet through the production guard and dispatch.
    /// Tests use only rejection paths; accepted packets still require a real socket.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    pub fn maintenance_dispatch(&mut self, packet: QPacket, address: SocketAddr) {
        self.handle_packet(&self.logger.clone(), packet, address);
    }

    /// Report owned clients, connection indices, pending SYNs and terminal owners.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    #[must_use]
    pub fn maintenance_counts(&self) -> (usize, usize, usize, usize) {
        (self.client_registry.clients.len(), self.client_registry.connection_id_session_ids.len(), self.new_clients.len(), self.client_registry.closing_clients.len())
    }

    /// Resolve through the real registry lookup used by NAT/notification callers.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    #[must_use]
    pub fn maintenance_routable(&self, signature: u32) -> bool {
        self.client_registry.client_by_connection_id(ConnectionID(signature)).is_some()
    }

    /// Advance only the scheduling clock; SQL callbacks and work timing stay real.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    pub fn maintenance_advance_retry_clock(&mut self, elapsed: Duration) {
        for pending in self.client_registry.closing_clients.values_mut() {
            pending.next_attempt = pending.next_attempt.checked_sub(elapsed).unwrap_or_else(Instant::now);
        }
    }

    /// Observe whether rejected traffic mutated the exact retained owner.
    #[cfg(any(test, feature = "maintenance-test-seams"))]
    #[must_use]
    pub fn maintenance_owner(&self, signature: u32) -> Option<String> {
        self.client_registry.clients.get(&signature).map(|ci| {
            let ci = ci.borrow();
            format!("{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}", ci.user_id, ci.ticket_fingerprint, ci.connection_id,
                ci.last_seen, ci.packet_fragment_bytes, ci.response_cache_bytes, ci.client_sequence_id, ci.server_sequence_id)
        })
    }

    // A transport is not the ticket: two authenticated connections may share
    // one ticket row. Only the last live owner may request its application close.
    // A borrow conflict is unknown, not permission to delete the shared ticket.
    fn has_live_shared_ticket_owner(&self, signature: u32) -> Result<bool, ()> {
        let owner = self.client_registry.clients.get(&signature).ok_or(())?;
        let owner = owner.try_borrow().map_err(|_| ())?;
        let (Some(user_id), Some(fingerprint)) = (owner.user_id, owner.ticket_fingerprint) else {
            return Ok(false);
        };
        let now = Instant::now();
        for (other_signature, other) in &self.client_registry.clients {
            if *other_signature == signature || self.client_registry.closing_clients.contains_key(other_signature) {
                continue;
            }
            let other = other.try_borrow().map_err(|_| ())?;
            if now.saturating_duration_since(other.last_seen) <= SESSION_TIMEOUT
                && other.user_id == Some(user_id) && other.ticket_fingerprint == Some(fingerprint)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // A terminal owner remains in the registry, but cannot consume any more
    // session packets. A failed application close must retain the exact ticket.
    fn try_retire_client(&mut self, signature: u32, reason: CloseReason) {
        self.client_registry.closing_clients.entry(signature).or_insert_with(|| PendingClose::new(reason));
        let Some(ci) = self.client_registry.clients.get(&signature) else {
            self.client_registry.closing_clients.remove(&signature);
            return;
        };
        let completed = if self.close_handler.is_none() {
            true
        } else {
            match self.has_live_shared_ticket_owner(signature) {
                Ok(true) => true, // Retire only this transport; the live peer owns the ticket.
                Ok(false) => ci.try_borrow().is_ok_and(|ci| {
                    self.close_handler.as_mut().unwrap()(&ci, reason)
                }),
                Err(()) => false, // Conservatively retain and back off until borrowing is safe.
            }
        };
        if !completed {
            let pending = self.client_registry.closing_clients.get_mut(&signature).unwrap();
            pending.next_attempt = Instant::now() + pending.retry_delay;
            pending.retry_delay = (pending.retry_delay * 2).min(Duration::from_secs(30));
            return;
        }
        let ci = self.client_registry.clients.remove(&signature).unwrap().into_inner();
        self.client_registry.closing_clients.remove(&signature);
        if let Some(conn_id) = ci.connection_id {
            if self.client_registry.connection_id_session_ids.get(&conn_id) == Some(&Signature(signature)) {
                self.client_registry.connection_id_session_ids.remove(&conn_id);
            }
        }
        match reason {
            CloseReason::Expired => {
                if let Some(handler) = self.expired_client_handler.as_mut() { handler(ci); }
            }
            CloseReason::Disconnect => {
                if let Some(handler) = self.disconnect_handler.as_mut() { handler(ci); }
            }
        }
    }

    /// Clears expired clients, retaining failed closes for a later timer tick.
    fn clear_clients(&mut self) {
        let now = Instant::now();
        self.new_clients.retain(|_, ci| now.saturating_duration_since(ci.last_seen) <= SESSION_TIMEOUT);
        for (signature, ci) in &self.client_registry.clients {
            if ci.try_borrow().is_ok_and(|ci| now.saturating_duration_since(ci.last_seen) > SESSION_TIMEOUT) {
                self.client_registry.closing_clients.entry(*signature).or_insert_with(|| PendingClose::new(CloseReason::Expired));
            }
        }
        // An elapsed budget cannot preempt one blocking callback. It does prevent
        // accumulating 32 five-second SQLite waits in one transport tick. Retry
        // deadlines reduce repeat work/logging; cursor rotation prevents starvation.
        // Legacy infallible consumers retain their old cadence.
        let started = Instant::now();
        let fallible = self.close_handler.is_some();
        let mut attempts = 0;
        for _ in 0..self.client_registry.closing_clients.len() {
            let next = self.close_retry_cursor.and_then(|cursor| {
                self.client_registry.closing_clients.range((std::ops::Bound::Excluded(cursor), std::ops::Bound::Unbounded)).next()
            }).or_else(|| self.client_registry.closing_clients.first_key_value()).map(|(&signature, &pending)| (signature, pending));
            let Some((signature, pending)) = next else { break; };
            self.close_retry_cursor = Some(signature);
            if pending.next_attempt <= Instant::now() {
                self.try_retire_client(signature, pending.reason);
                attempts += 1;
            }
            if fallible && (attempts >= 32 || started.elapsed() >= Duration::from_millis(25)) {
                break;
            }
        }
    }

}

/// Sends a request to a client.
pub fn send_request<T>(
    logger: &Logger,
    ctx: &Context,
    src: &SocketAddr,
    socket: &UdpSocket,
    mut req: QPacket,
    ci: &mut ClientInfo<T>,
) -> Result<usize, Box<dyn std::error::Error>> {
    req.sequence = ci.client_sequence_id;
    ci.client_sequence_id = ci.client_sequence_id.wrapping_add(1);
    req.flags.insert(PacketFlag::HasSize);
    req.flags.insert(PacketFlag::NeedAck);
    req.flags.insert(PacketFlag::Reliable);
    req.signature = ci.server_signature;
    req.session_id = ci.client_session;
    send_packet(logger, ctx, src, socket, req)
}

/// Sends a packet to a client.
pub(crate) fn send_packet(logger: &Logger, ctx: &Context, src: &SocketAddr, socket: &UdpSocket, mut resp: QPacket) -> Result<usize, Box<dyn std::error::Error>> {
    if matches!(resp.packet_type, PacketType::Data) {
        resp.use_compression = true;
        if resp.fragment_id.is_none() {
            resp.fragment_id = Some(0);
        }
    }
    resp.flags.insert(PacketFlag::HasSize);
    trace!(logger, "<- {:?}", resp);
    let data = &resp.to_bytes(ctx);
    trace!(logger, "<- {:02x?}", data);
    #[cfg(feature = "maintenance-test-seams")]
    if let Some(size) = fragment_boundary_capture::collect(data) { return Ok(size); }
    let sz = socket.send_to(data, src)?;
    assert_eq!(sz, data.len());
    Ok(sz)
}

#[cfg(test)]
mod fragment_tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use super::*;

    type TestServer<'a> = Server<'a, fn(ClientInfo<()>), fn(ClientInfo<()>), ()>;

    struct CountingHandler {
        calls: Arc<AtomicUsize>,
    }

    impl StreamHandler<()> for CountingHandler {
        fn handle(
            &self,
            _logger: &Logger,
            _ctx: &Context,
            _ci: &mut ClientInfo<()>,
            data: &[u8],
            _client_registry: &ClientRegistry<()>,
            _socket: &UdpSocket,
        ) -> Result<Vec<u8>, packet::Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(data.to_vec())
        }
    }

    fn client() -> ClientInfo<()> {
        ClientInfo::new("127.0.0.1:3074".parse().unwrap())
    }

    #[test]
    fn unsupported_route_and_raw_packets_are_dropped_without_panicking() {
        let logger = Logger::root(slog::Discard, o!());
        let ctx = Context::default();
        let registry = StreamHandlerRegistry::new(logger.clone());
        let mut server: TestServer<'_> = Server::new(logger.clone(), &ctx, registry);
        let client_address = "127.0.0.1:3074".parse().unwrap();

        for packet_type in [PacketType::Route, PacketType::Raw] {
            server.handle_packet(
                &logger,
                QPacket {
                    packet_type,
                    ..Default::default()
                },
                client_address,
            );
        }
    }

    #[test]
    fn reassembles_real_sender_order_with_increasing_sequences() {
        let mut ci = client();
        assert_eq!(cache_fragment(&mut ci, 1, 100, b"a".to_vec()), Ok(true));
        assert_eq!(cache_fragment(&mut ci, 2, 101, b"b".to_vec()), Ok(true));
        let payload = finish_fragmented_payload(&mut ci, 102, b"c".to_vec()).unwrap();
        assert_eq!(payload, b"abc");
        assert!(ci.packet_fragments.is_empty());
    }

    #[test]
    fn eleven_fragments_follow_the_blacklist_login_sequence_pattern() {
        let mut ci = client();
        for fragment_id in 1u8..=11 {
            let sequence = 500u16 + u16::from(fragment_id - 1);
            let byte = b'a' + (fragment_id - 1);
            assert_eq!(cache_fragment(&mut ci, fragment_id, sequence, vec![byte]), Ok(true));
        }
        let payload = finish_fragmented_payload(&mut ci, 511, b"z".to_vec()).unwrap();
        assert_eq!(payload, b"abcdefghijkz");
    }

    #[test]
    fn rejects_missing_fragment_without_allocating_from_payload_data() {
        let mut ci = client();
        assert_eq!(cache_fragment(&mut ci, 2, 101, b"b".to_vec()), Ok(true));
        assert_eq!(finish_fragmented_payload(&mut ci, 102, b"c".to_vec()), Err("fragment group has a missing id"));
    }

    #[test]
    fn reassembles_nonzero_fragments_that_arrive_out_of_order() {
        let mut ci = client();
        assert_eq!(cache_fragment(&mut ci, 2, 101, b"b".to_vec()), Ok(true));
        assert_eq!(cache_fragment(&mut ci, 1, 100, b"a".to_vec()), Ok(true));
        let payload = finish_fragmented_payload(&mut ci, 102, b"c".to_vec()).unwrap();
        assert_eq!(payload, b"abc");
    }

    #[test]
    fn a_new_final_sequence_discards_stale_fragments() {
        let mut ci = client();
        assert_eq!(cache_fragment(&mut ci, 1, 100, b"old".to_vec()), Ok(true));
        assert_eq!(cache_fragment(&mut ci, 1, 200, b"new".to_vec()), Ok(true));
        let payload = finish_fragmented_payload(&mut ci, 201, b"end".to_vec()).unwrap();
        assert_eq!(payload, b"newend");
    }

    #[test]
    fn duplicate_fragment_does_not_inflate_retained_bytes() {
        let mut ci = client();
        assert_eq!(cache_fragment(&mut ci, 1, 100, b"same".to_vec()), Ok(true));
        let retained = ci.packet_fragment_bytes;
        assert_eq!(cache_fragment(&mut ci, 1, 100, b"same".to_vec()), Ok(false));
        assert_eq!(ci.packet_fragment_bytes, retained);
    }

    #[test]
    fn mismatched_final_sequence_is_rejected() {
        let mut ci = client();
        assert_eq!(cache_fragment(&mut ci, 1, 100, b"a".to_vec()), Ok(true));
        assert_eq!(finish_fragmented_payload(&mut ci, 102, b"b".to_vec()), Err("fragment group sequence mismatch"));
    }

    #[test]
    fn oversized_fragment_group_is_rejected() {
        let mut ci = client();
        let oversized = vec![0u8; MAX_INBOUND_MESSAGE_BYTES + 1];
        assert_eq!(cache_fragment(&mut ci, 1, 100, oversized), Err("fragmented message exceeds byte limit"));
    }

    #[test]
    fn duplicate_request_reuses_original_response_sequence() {
        let mut ci = client();
        ci.client_signature = Some(0x1234_5678);
        ci.server_session = 7;

        let response = prepare_response(QPacket::default(), &mut ci);
        assert_eq!(response.sequence, 1);
        assert_eq!(ci.server_sequence_id, 2);
        cache_response(&mut ci, 41, vec![response.clone()]);

        let replay = cached_response(&ci, 41).unwrap();
        assert_eq!(replay, vec![response]);
        assert_eq!(ci.server_sequence_id, 2);

        let next_response = prepare_response(QPacket::default(), &mut ci);
        assert_eq!(next_response.sequence, 2);
    }

    #[test]
    fn response_cache_evicts_oldest_request() {
        let mut ci = client();
        let max_cached_sequence = u16::try_from(MAX_CACHED_RESPONSES).expect("response cache limit fits u16");
        for sequence in 0..=max_cached_sequence {
            cache_response(&mut ci, sequence, vec![QPacket::default()]);
        }

        assert!(cached_response(&ci, 0).is_none());
        assert!(cached_response(&ci, 1).is_some());
        assert!(cached_response(&ci, max_cached_sequence).is_some());
        assert_eq!(ci.response_cache.len(), MAX_CACHED_RESPONSES);
    }

    #[test]
    fn response_cache_is_bounded_by_payload_bytes() {
        let mut ci = client();
        let half_limit = vec![0u8; MAX_CACHED_RESPONSE_BYTES / 2];
        for sequence in 1..=3 {
            cache_response(
                &mut ci,
                sequence,
                vec![QPacket {
                    payload: half_limit.clone(),
                    ..Default::default()
                }],
            );
        }

        assert!(cached_response(&ci, 1).is_none());
        assert!(cached_response(&ci, 2).is_some());
        assert!(cached_response(&ci, 3).is_some());
        assert_eq!(ci.response_cache_bytes, MAX_CACHED_RESPONSE_BYTES);
    }

    #[test]
    fn duplicate_data_packet_replays_without_calling_handler_again() {
        sodiumoxide::init().unwrap();
        let logger = Logger::root(slog::Discard, o!());
        let ctx = Context::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let vport = VPort {
            stream_type: packet::StreamType::RVSec,
            port: 1,
        };
        let mut registry = StreamHandlerRegistry::new(logger.clone());
        registry.register(vport, Box::new(CountingHandler { calls: Arc::clone(&calls) }));
        let mut server: TestServer<'_> = Server::new(logger.clone(), &ctx, registry);
        server.bind("127.0.0.1:0").unwrap();

        let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client_address = client_socket.local_addr().unwrap();
        let signature = 0x1020_3040;
        let mut ci = ClientInfo::new(client_address);
        ci.client_signature = Some(0x5060_7080);
        ci.server_session = 7;
        server.client_registry.clients.insert(signature, RefCell::new(ci));

        let request = QPacket {
            source: vport,
            destination: vport,
            packet_type: PacketType::Data,
            signature,
            sequence: 41,
            payload: b"login".to_vec(),
            ..Default::default()
        };
        server.handle_data(&logger, request.clone(), client_address);
        server.handle_data(&logger, request, client_address);

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let ci = server.client_registry.clients.get(&signature).unwrap().borrow();
        assert_eq!(ci.server_sequence_id, 2);
        assert_eq!(cached_response(&ci, 41).unwrap()[0].sequence, 1);
    }
}


#[cfg(test)]
mod maintenance_connect_tests {
    use super::*;
    use sodiumoxide::crypto::secretbox;
    type TestServer<'a> = Server<'a,fn(ClientInfo<()>),fn(ClientInfo<()>),()>;
    fn connect_case(valid_until:u64,corrupt:bool,truncated:bool,claimed_pid:u32,expected:bool) {
        sodiumoxide::init().unwrap();
        let logger=Logger::root(slog::Discard,o!());
        let ctx=Context::splinter_cell_blacklist();
        let registry=StreamHandlerRegistry::new(logger.clone());
        let mut server:TestServer<'_>=Server::new(logger.clone(),&ctx,registry);
        server.socket=Some(UdpSocket::bind("127.0.0.1:0").unwrap());
        let receiver=UdpSocket::bind("127.0.0.1:0").unwrap();receiver.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let addr=receiver.local_addr().unwrap();let mut buf=[0u8;2048];
        let source=VPort{port:15,stream_type:packet::StreamType::RVSec};
        let destination=VPort{port:1,stream_type:packet::StreamType::RVSec};
        let syn=QPacket{source,destination,packet_type:PacketType::Syn,conn_signature:Some(0),flags:PacketFlag::HasSize.into(),..Default::default()};
        let wire=syn.to_bytes(&ctx);let(decoded,_)=QPacket::from_bytes(&ctx,&wire).unwrap();decoded.validate(&ctx,&wire).unwrap();
        server.handle_packet(&logger,decoded,addr);
        let(n,_)=receiver.recv_from(&mut buf).unwrap();let(ack,_)=QPacket::from_bytes(&ctx,&buf[..n]).unwrap();
        let signature=ack.conn_signature.unwrap();
        let key=[0x57;16];
        let ticket=KerberosTicketInternal{principle_id:42,valid_until,session_key:key};
        let nonce=secretbox::gen_nonce();let mut sealed=nonce.as_ref().to_vec();sealed.extend(secretbox::seal(&ticket.to_bytes(),&nonce,&ctx.ticket_key));
        if corrupt {let i=sealed.len()-1;sealed[i]^=0x80;}
        let mut data=claimed_pid.to_bytes();data.extend(7u32.to_bytes());data.extend(u32::MAX.to_bytes());
        if truncated {data.truncate(7);}
        let encrypted=crypt_key(&key,&data);let mut payload=sealed.to_bytes();payload.extend(encrypted.to_bytes());
        let packet=QPacket{source,destination,packet_type:PacketType::Connect,conn_signature:Some(0x1234),signature,session_id:7,sequence:1,payload,flags:PacketFlag::HasSize.into(),..Default::default()};
        let wire=packet.to_bytes(&ctx);let(decoded,_)=QPacket::from_bytes(&ctx,&wire).unwrap();decoded.validate(&ctx,&wire).unwrap();assert_eq!(decoded.payload,packet.payload,"RVSec envelope must preserve inner ticket data");
        server.handle_packet(&logger,decoded,addr);
        let(n,_)=receiver.recv_from(&mut buf).unwrap();let(ack,_)=QPacket::from_bytes(&ctx,&buf[..n]).unwrap();
        let ci=server.client_registry.clients.get(&signature).unwrap().borrow();
        assert_eq!(ci.user_id,if expected {Some(42)} else {None});
        assert_eq!(ci.ticket_fingerprint,if expected {Some(crate::kerberos::ticket_fingerprint(&key))} else {None});
        assert_eq!(ci.connection_id.is_some(),expected);
        if expected {let response:Vec<u8>=ReadStream::from_bytes(&ack.payload).read().unwrap();assert_eq!(response,0u32.to_bytes());}
        println!("CONNECT valid_identity={expected} claimed_pid={claimed_pid} corrupt={corrupt} truncated={truncated} loopback_only=true live_game=false");
    }
    #[test] fn valid_encrypted_connect_binds_only_verified_ticket(){connect_case(u64::MAX,false,false,42,true);}
    #[test] fn expired_ticket_does_not_publish_identity(){connect_case(0,false,false,42,false);}
    #[test] fn tampered_ticket_does_not_publish_identity(){connect_case(u64::MAX,true,false,42,false);}
    #[test] fn truncated_challenge_does_not_publish_identity(){connect_case(u64::MAX,false,true,42,false);}
    #[test] fn client_claim_cannot_replace_ticket_principal(){connect_case(u64::MAX,false,false,999,true);}
}


#[cfg(test)]
#[path = "prudp/sequence_tests.rs"]
mod maintenance_sequence_observer;

#[cfg(test)]
#[path = "prudp/retirement_tests.rs"]
mod maintenance_retirement_observer;

#[cfg(test)]
#[path = "prudp/cleanup_cadence_tests.rs"]
mod maintenance_cleanup_cadence;

#[cfg(test)]
#[path = "prudp/peer_ownership_tests.rs"]
mod maintenance_peer_ownership;

#[cfg(feature = "maintenance-test-seams")]
#[path = "prudp/fragment_boundary_capture.rs"]
pub mod fragment_boundary_capture;
