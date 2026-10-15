use prost::Message;
use crate::{misc, users};

#[test]
fn invitation_and_session_field_numbers_keep_native_identity() {
    // id=150, kind=HostMigrationFollow(5), session{id=7,data=[aa,bb],flags=3,invite_only=true}.
    let wire = [0x08,0x96,0x01,0x20,0x05,0x2a,0x0a,0x08,0x07,0x12,0x02,0xaa,0xbb,0x18,0x03,0x20,0x01];
    let event=misc::InviteEvent::decode(wire.as_slice()).unwrap();
    assert_eq!(event.id,150);assert_eq!(event.kind,5);assert!(!event.force_join);assert!(event.sender.is_none());
    let session=event.game_session.as_ref().unwrap();assert_eq!((session.id,session.flags,session.invite_only),(7,3,true));assert_eq!(session.data,[0xaa,0xbb]);
    assert_eq!(event.encode_to_vec(),wire);
    assert_eq!([misc::InviteKind::Unspecified as i32,misc::InviteKind::PrivateRoom as i32,misc::InviteKind::LobbyParty as i32,misc::InviteKind::PartyFollow as i32,misc::InviteKind::HostMigrationFollow as i32],[0,1,2,3,5]);
    println!("WIRE invitation_golden_bytes={} kinds=0,1,2,3,5",wire.len());
}

#[test]
fn additive_unknown_fields_preserve_known_values_without_reclassifying_kind() {
    let wire=[0x08,0x2a,0x20,0x63,0xa0,0x06,0x07]; // invite 42, unknown kind 99, field100=7
    let event=misc::InviteEvent::decode(wire.as_slice()).unwrap();
    assert_eq!(event.id,42);assert_eq!(event.kind,99);assert!(misc::InviteKind::try_from(event.kind).is_err());
    // Prost discards unknown fields; consumers must not treat unknown enum as a private invite.
    assert_eq!(event.encode_to_vec(),[0x08,0x2a,0x20,0x63]);
}

#[test]
fn client_product_stays_at_tag_ten_separate_from_account_fields() {
    let wire=[0x0a,0x01,b'u',0x12,0x01,b'p',0x52,0x01,b'x']; // synthetic u/p, product x
    let req=users::LoginRequest::decode(wire.as_slice()).unwrap();
    assert_eq!((&*req.username,&*req.password,&*req.client_product),("u","p","x"));
    assert!(crate::gameplay_contract::ClientContract::parse(&req.client_product).is_none());
    let mut old=wire[..6].to_vec();old.extend_from_slice(&[0xa0,0x06,1]);
    assert!(users::LoginRequest::decode(old.as_slice()).unwrap().client_product.is_empty());
}

#[test]
fn malformed_wire_fails_as_decode_error_without_panicking() {
    let inputs: &[&[u8]]=&[
        &[0x00],                         // invalid field zero
        &[0x08,0x80],                    // truncated varint
        &[0x2a,0x80],                    // truncated message length
        &[0x2a,0xff,0xff,0xff,0xff,0x0f],// body absent despite huge length
        &[0x12,0x03,0x0a,0x01,0xff],     // invalid UTF8 in nested sender
        &[0x0f],                         // invalid wire type
    ];
    for wire in inputs {
        let result=std::panic::catch_unwind(|| misc::InviteEvent::decode(*wire));
        assert!(matches!(result,Ok(Err(_))),"malformed fixture {wire:02x?}");
    }
    println!("WIRE malformed_inputs={} panics=0",inputs.len());
}

#[test]
fn p2p_request_and_response_keep_distinct_challenge_tags() {
    assert_eq!(misc::TestP2pRequest{challenge:vec![0xaa,0xbb]}.encode_to_vec(),[0x0a,0x02,0xaa,0xbb]);
    assert_eq!(misc::TestP2pResponse{challenge:vec![0xaa,0xbb]}.encode_to_vec(),[0x12,0x02,0xaa,0xbb]);
}
