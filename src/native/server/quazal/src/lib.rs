#![feature(seek_stream_len, proc_macro_hygiene, cursor_split, associated_type_defaults)]
#![deny(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

#[macro_use]
extern crate slog;
#[macro_use]
extern crate quazal_macros;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Instant;

use derive_more::Display;
use derive_more::Error as DeriveError;

pub mod config;
pub mod kerberos;
pub mod prudp;
pub mod rmc;

pub use crate::config::*;

/// Represents an error that can occur in the application.
#[derive(Debug, Display, DeriveError)]
pub enum Error {
    /// The requested service was not found.
    #[display("Service {_0} not found")]
    ServiceNotFound(#[error(not(source))] String),
    /// An invalid value was provided.
    InvalidValue,
}

/// Represents a unique connection identifier.
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub struct ConnectionID(u32);

impl From<ConnectionID> for u32 {
    fn from(value: ConnectionID) -> Self {
        value.0
    }
}

impl FromStr for ConnectionID {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ConnectionID(s.parse()?))
    }
}

/// Represents a signature for a client or server.
#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
struct Signature(u32);

/// Holds information about a client connection.
#[derive(Debug)]
pub struct ClientInfo<T = ()> {
    /// The server's sequence ID for the connection.
    server_sequence_id: u16,
    /// The client's sequence ID for the connection.
    client_sequence_id: u16,
    /// The client's signature, if available.
    client_signature: Option<u32>,
    /// The server's signature.
    server_signature: u32,
    /// The client's session ID.
    client_session: u8,
    /// The server's session ID.
    server_session: u8,
    /// Packet fragments retained for the current inbound PRUDP message.
    packet_fragments: HashMap<u8, Vec<u8>>,
    /// Total bytes retained in `packet_fragments`.
    packet_fragment_bytes: usize,
    /// Sequence of fragment id one for the current inbound group.
    packet_fragment_start_sequence: Option<u16>,
    /// When the current fragment group started.
    packet_fragment_started_at: Option<Instant>,
    /// Recently generated responses, keyed by the final request sequence.
    /// Reliable PRUDP retransmissions must replay the original response
    /// sequence instead of executing the request again with a new sequence.
    response_cache: HashMap<u16, Vec<prudp::packet::QPacket>>,
    /// Insertion order used to keep `response_cache` bounded.
    response_cache_order: VecDeque<u16>,
    /// Payload bytes currently retained by `response_cache`.
    response_cache_bytes: usize,
    /// The client's network address.
    address: SocketAddr,
    /// The time the client was last seen.
    last_seen: Instant,
    /// The client's connection ID, if available.
    pub connection_id: Option<ConnectionID>,
    /// The client's user ID, if available.
    pub user_id: Option<u32>,
    /// SHA-256 identity of the validated Kerberos session key (never the key).
    pub ticket_fingerprint: Option<[u8;32]>,
    /// Additional user-defined data.
    pub additional: T,
}

impl<T> ClientInfo<T> {
    /// Creates a new `ClientInfo` for a given address.
    #[must_use]
    pub fn new(address: SocketAddr) -> ClientInfo<T>
    where
        T: Default,
    {
        ClientInfo {
            server_sequence_id: 1,
            client_sequence_id: 1,
            client_signature: None,
            server_signature: rand::random(),
            client_session: Default::default(),
            server_session: Default::default(),
            user_id: None,
            ticket_fingerprint: None,
            packet_fragments: HashMap::default(),
            packet_fragment_bytes: 0,
            packet_fragment_start_sequence: None,
            packet_fragment_started_at: None,
            response_cache: HashMap::default(),
            response_cache_order: VecDeque::default(),
            response_cache_bytes: 0,
            address,
            additional: Default::default(),
            last_seen: std::time::Instant::now(),
            connection_id: None,
        }
    }

    /// Returns the client's network address.
    pub fn address(&self) -> &SocketAddr {
        &self.address
    }

    /// Updates the last seen time for the client.
    pub fn seen(&mut self) {
        self.last_seen = std::time::Instant::now();
    }
}
