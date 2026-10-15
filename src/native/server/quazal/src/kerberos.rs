/// Kerberos-related functionality for authentication.
use std::io::Cursor;

use hmac::digest::Update;
use hmac::Hmac;
use hmac::Mac;
use md5::Digest;
use md5::Md5;
use sodiumoxide::crypto::secretbox;

use crate::prudp::packet::Rc4;
use crate::rmc::basic::FromStream;
use crate::rmc::basic::FromStreamError;
use crate::rmc::basic::ReadStream;
use crate::rmc::basic::ToStream;

/// The size of the session key in bytes.
pub const SESSION_KEY_SIZE: usize = 16;

/// The internal representation of a Kerberos ticket.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, ToStream, FromStream)]
pub struct KerberosTicketInternal {
    /// The principal ID.
    pub principle_id: u32,
    /// The expiration time of the ticket.
    pub valid_until: u64,
    /// The session key.
    pub session_key: [u8; SESSION_KEY_SIZE],
}

impl KerberosTicketInternal {
    /// Seals the ticket using a secret key.
    fn seal(&self, key: &secretbox::Key) -> Vec<u8> {
        let n = secretbox::gen_nonce();
        let mut c = secretbox::seal(&self.to_bytes(), &n, key);

        let mut res = Vec::with_capacity(c.len() + secretbox::NONCEBYTES);
        res.extend_from_slice(n.as_ref());
        res.append(&mut c);
        res
    }

    /// Opens a sealed ticket using a secret key.
    pub fn open(data: &[u8], key: &secretbox::Key) -> Result<Self, FromStreamError> {
        if data.len() < secretbox::NONCEBYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("got {} bytes, required at least {}", data.len(), secretbox::NONCEBYTES),
            )
            .into());
        }
        let n = secretbox::Nonce::from_slice(&data[..secretbox::NONCEBYTES]).unwrap();
        let data = &data[secretbox::NONCEBYTES..];
        let data = secretbox::open(data, &n, key).map_err(|()| std::io::Error::new(std::io::ErrorKind::InvalidData, "open failed"))?;
        let mut stream = ReadStream::from_bytes(data);
        stream.read()
    }
}

/// A Kerberos ticket.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct KerberosTicket {
    /// The session key.
    pub session_key: [u8; SESSION_KEY_SIZE],
    /// The principal ID.
    pub pid: u32,
    /// The internal ticket.
    pub internal: KerberosTicketInternal,
}

impl KerberosTicket {
    /// Derives a key from a peer PID and a password.
    fn derive_key(peer_pid: u32, password: Option<&str>) -> Vec<u8> {
        // derive key
        let count = 65000 + (peer_pid % 1024);
        let mut key = password.unwrap_or("UbiDummyPwd").as_bytes().to_vec();

        for _ in 0..count {
            let h = Md5::new().chain(key).finalize();
            key = h.to_vec();
        }
        key
    }

    /// Creates a `KerberosTicket` from a byte buffer.
    pub fn from_bytes(buf: &[u8], peer_pid: u32, password: Option<&str>, key: &secretbox::Key) -> Result<Self, FromStreamError> {
        let off = buf.len().checked_sub(Md5::output_size()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "ticket MAC is missing")
        })?;
        let (buf, received_mac) = buf.split_at(off);

        let obf_key = Self::derive_key(peer_pid, password);
        let mut mac: Hmac<Md5> = Hmac::new_from_slice(&obf_key).unwrap();
        Mac::update(&mut mac, buf);
        mac.verify_slice(received_mac).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "ticket MAC mismatch")
        })?;
        let buf: Vec<u8> = Rc4::new(&obf_key).zip(buf).map(|(a, b)| a ^ b).collect();
        let mut rdr = ReadStream::from_reader(Cursor::new(buf));

        let session_key = FromStream::from_stream(&mut rdr)?;
        let pid = FromStream::from_stream(&mut rdr)?;
        // as_bytes writes the sealed internal ticket as a length-prefixed Vec.
        // Feeding that prefix to secretbox::open shifts its nonce and always fails.
        let internal: Vec<u8> = rdr.read()?;
        if !rdr.read_all()?.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "trailing ticket data").into());
        }
        let internal = KerberosTicketInternal::open(&internal, key)?;

        Ok(Self { session_key, pid, internal })
    }

    /// Converts the ticket to a byte vector.
    #[must_use]
    pub fn as_bytes(&self, peer_pid: u32, password: Option<&str>, key: &secretbox::Key) -> Vec<u8> {
        let mut buf = self.session_key.to_vec();
        buf.append(&mut self.pid.to_bytes());
        buf.append(&mut self.internal.seal(key).to_bytes());

        let key = Self::derive_key(peer_pid, password);

        let mut buf: Vec<u8> = Rc4::new(&key).zip(&buf).map(|(a, b)| a ^ b).collect();

        let mut mac: Hmac<Md5> = Hmac::new_from_slice(&key).unwrap();
        Mac::update(&mut mac, &buf);
        buf.extend(mac.finalize().into_bytes());

        buf
    }
}

/// Non-secret binding identity; do not log the underlying session key.
#[must_use]
pub fn ticket_fingerprint(key:&[u8])->[u8;32] {sodiumoxide::crypto::hash::sha256::hash(key).0}


#[cfg(test)]
mod maintenance_ticket_codec_tests {
    use super::*;
    fn fixture()->(KerberosTicket,secretbox::Key) {
        sodiumoxide::init().unwrap();
        let key=secretbox::Key([0x35;32]);let session=[0x42;SESSION_KEY_SIZE];
        (KerberosTicket{session_key:session,pid:0x1000,internal:KerberosTicketInternal{principle_id:1000,valid_until:u64::MAX,session_key:session}},key)
    }
    #[test]
    fn same_input_ticket_codec_observation() {
        let(t,key)=fixture();let bytes=t.as_bytes(1000,None,&key);
        let roundtrip=KerberosTicket::from_bytes(&bytes,1000,None,&key).is_ok();
        let short_panics=std::panic::catch_unwind(||KerberosTicket::from_bytes(&[],1000,None,&key)).is_err();
        let mut tampered=bytes.clone();let last=tampered.len()-1;tampered[last]^=0x80;
        let tampered_rejected=KerberosTicket::from_bytes(&tampered,1000,None,&key).is_err();
        println!("TICKET_CODEC roundtrip={roundtrip} empty_input_panics={short_panics} tampered_mac_rejected={tampered_rejected} serialized_bytes={} live_game=false",bytes.len());
    }
    #[test]
    fn password_and_default_ticket_roundtrips_keep_wire_contract() {
        let(t,key)=fixture();
        for password in [None,Some("fixture-only-password")] {
            let bytes=t.as_bytes(1000,password,&key);assert_eq!(bytes.len(),108);
            let result=KerberosTicket::from_bytes(&bytes,1000,password,&key).unwrap();
            assert_eq!(result.pid,t.pid);assert_eq!(result.session_key,t.session_key);
            assert_eq!(result.internal.principle_id,t.internal.principle_id);assert_eq!(result.internal.session_key,t.internal.session_key);
            assert_eq!(result.internal.valid_until,t.internal.valid_until);
            // Independently inspect the serializer's length-prefixed inner blob.
            let secret=KerberosTicket::derive_key(1000,password);
            let plain:Vec<u8>=Rc4::new(&secret).zip(&bytes[..bytes.len()-16]).map(|(a,b)|a^b).collect();
            let mut stream=ReadStream::from_bytes(&plain);let _: [u8;16]=stream.read().unwrap();let _:u32=stream.read().unwrap();
            let sealed:Vec<u8>=stream.read().unwrap();assert_eq!(sealed.len(),68);assert!(stream.read_all().unwrap().is_empty());
            assert_eq!(KerberosTicketInternal::open(&sealed,&key).unwrap().principle_id,1000);
        }
    }
    #[test]
    fn truncated_tickets_return_errors_without_panicking() {
        let(t,key)=fixture();let bytes=t.as_bytes(1000,None,&key);
        for len in 0..bytes.len() {
            let result=std::panic::catch_unwind(||KerberosTicket::from_bytes(&bytes[..len],1000,None,&key));
            assert!(result.is_ok(),"input length {len} panicked");assert!(result.unwrap().is_err());
        }
    }
    #[test]
    fn changed_ciphertext_mac_password_or_principal_is_rejected() {
        let(t,key)=fixture();let bytes=t.as_bytes(1000,Some("fixture-only-password"),&key);
        for offset in [0,20,bytes.len()-1] {
            let mut damaged=bytes.clone();damaged[offset]^=1;assert!(KerberosTicket::from_bytes(&damaged,1000,Some("fixture-only-password"),&key).is_err());
        }
        assert!(KerberosTicket::from_bytes(&bytes,1000,Some("wrong-fixture-password"),&key).is_err());
        assert!(KerberosTicket::from_bytes(&bytes,1001,Some("fixture-only-password"),&key).is_err());
    }
    #[test]
    fn authenticated_malformed_inner_length_or_trailing_data_is_rejected() {
        let(t,key)=fixture();let bytes=t.as_bytes(1000,None,&key);let secret=KerberosTicket::derive_key(1000,None);
        let original:Vec<u8>=Rc4::new(&secret).zip(&bytes[..bytes.len()-16]).map(|(a,b)|a^b).collect();
        for trailing in [false,true] {
            let mut plain=original.clone();
            if trailing {plain.push(0x55);}else{plain[20..24].copy_from_slice(&u32::MAX.to_le_bytes());}
            let mut sealed:Vec<u8>=Rc4::new(&secret).zip(&plain).map(|(a,b)|a^b).collect();
            let mut mac:Hmac<Md5>=Hmac::new_from_slice(&secret).unwrap();Mac::update(&mut mac,&sealed);sealed.extend(mac.finalize().into_bytes());
            assert!(KerberosTicket::from_bytes(&sealed,1000,None,&key).is_err());
        }
    }

}
