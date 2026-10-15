//! Actual configuration/packet boundaries. No sockets, user files, or replacement crypto.
use quazal::{Context, prudp::packet::{Error, PacketType, QPacket, StreamType, VPort}};
use std::panic::{catch_unwind, AssertUnwindSafe};

const KEY_ERROR: &str = "crypto_key must contain 1..=256 bytes";

fn from_key(key: &str) -> Result<Context, toml::de::Error> {
    // JSON's quoted string form is also valid TOML for the synthetic inputs here.
    let quoted = serde_json::to_string(key).unwrap();
    toml::from_str(&format!("crypto_key = {quoted}\n"))
}
fn wire(ctx: &Context) -> Vec<u8> {
    QPacket {
        source: VPort { port: 1, stream_type: StreamType::RVSec },
        destination: VPort { port: 1, stream_type: StreamType::RVSec },
        packet_type: PacketType::Ping,
        payload: b"synthetic-key-boundary".to_vec(),
        ..Default::default()
    }.to_bytes(ctx)
}

#[test]
fn crypto_key_default_and_access_key_contract_unchanged() {
    let default: Context = toml::from_str("").unwrap();
    assert_eq!(default.crypto_key, b"CD&ML");
    assert!(default.access_key.is_empty());
    for key in [String::new(), "X".repeat(257)] {
        let quoted = serde_json::to_string(&key).unwrap();
        let ctx: Context = toml::from_str(&format!("access_key = {quoted}\n")).unwrap();
        assert_eq!(ctx.access_key, key.as_bytes());
        assert_eq!(ctx.crypto_key, b"CD&ML");
    }
}

#[test]
fn crypto_key_ascii_one_and_256_are_preserved() {
    for key in ["K".to_owned(), "X".repeat(256)] {
        let ctx = from_key(&key).unwrap();
        assert_eq!(ctx.crypto_key, key.as_bytes());
        let encoded = toml::to_string(&ctx).unwrap();
        let decoded: Context = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.crypto_key, key.as_bytes());
    }
}

#[test]
fn crypto_key_zero_and_257_are_explicitly_rejected() {
    for key in [String::new(), "X".repeat(257)] {
        let error = from_key(&key).unwrap_err().to_string();
        assert!(error.contains(KEY_ERROR), "{error}");
    }
}

#[test]
fn crypto_key_unicode_limit_counts_utf8_bytes() {
    let exact = "é".repeat(128);
    assert_eq!(exact.len(), 256);
    assert_eq!(exact.chars().count(), 128);
    assert_eq!(from_key(&exact).unwrap().crypto_key, exact.as_bytes());
    let over = format!("{exact}X");
    assert_eq!(over.len(), 257);
    assert_eq!(over.chars().count(), 129);
    let error = from_key(&over).unwrap_err().to_string();
    assert!(error.contains(KEY_ERROR), "{error}");
}

#[test]
fn crypto_key_valid_custom_packets_roundtrip_without_fallback() {
    let default = Context::default();
    let default_wire = wire(&default);
    for key in ["K".to_owned(), "X".repeat(256), "é".repeat(128)] {
        let ctx = from_key(&key).unwrap();
        assert_eq!(ctx.crypto_key, key.as_bytes());
        let bytes = wire(&ctx);
        assert_ne!(bytes, default_wire, "legal custom key must not silently use default");
        let (packet, consumed) = QPacket::from_bytes(&ctx, &bytes).unwrap();
        assert_eq!(consumed as usize, bytes.len());
        packet.validate(&ctx, &bytes).unwrap();
        assert_eq!(packet.payload, b"synthetic-key-boundary");
    }
}

#[test]
fn crypto_key_programmatic_invalid_context_returns_error_not_panic() {
    let good = Context::default();
    let bytes = wire(&good);
    for len in [0, 257] {
        let constructed = Context { crypto_key: vec![b'X'; len], ..good.clone() };
        let mut mutated = good.clone();
        mutated.crypto_key = vec![b'X'; len];
        for ctx in [constructed, mutated] {
            let result = catch_unwind(AssertUnwindSafe(|| QPacket::from_bytes(&ctx, &bytes)));
            let result = result.expect("public Context bypass must not reach RC4 assertion");
            match result {
                Err(Error::IO(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                    assert_eq!(error.to_string(), KEY_ERROR);
                }
                other => panic!("invalid configured key must be a normal input error, got {other:?}"),
            }
        }
    }
}
