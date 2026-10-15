//! Actual OverlordNews -> RVSec -> PRUDP -> encoded packet boundary.
//! Root runs only issue45_ with --test-threads=1; cwd is process-global.
//! No serve loop, send/send_to, real account, existing config, DB or device.
use super::*;
use std::path::PathBuf;
use quazal::prudp::fragment_boundary_capture::Capture;
use quazal::prudp::packet::{PacketFlag, PacketType, QPacket, StreamHandlerRegistry, StreamType, VPort};
use quazal::prudp::Server;
use quazal::rmc::{Packet, Request, Response, ResponseData, RVSecHandler};
use serde_json::{json, Value};

type TestServer<'a> = Server<'a, fn(quazal::ClientInfo<()>), fn(quazal::ClientInfo<()>), ()>;

struct OwnedCwd { prior: PathBuf }
impl OwnedCwd {
    fn enter(name: &str, news: Option<&Value>) -> Self {
        let base = PathBuf::from(std::env::var_os("SCBL_FRAGMENT_TEST_DIR").expect("root runner owns test output"));
        assert!(base.is_absolute() && base.is_dir());
        let path = base.join(name);
        std::fs::create_dir(&path).expect("case directory must be fresh");
        std::fs::create_dir(path.join("data")).unwrap();
        if let Some(news) = news {
            std::fs::write(path.join("data/news.json"), serde_json::to_vec(news).unwrap()).unwrap();
        }
        let prior = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self { prior }
    }
}
impl Drop for OwnedCwd {
    fn drop(&mut self) { std::env::set_current_dir(&self.prior).expect("restore cwd"); }
}

fn news_item() -> Value {
    json!({"maybe_id":1,"unk2":0,"unk3":0,"unk4":0,"unk5":"","unk6":0,"unk7":0,
           "expiration_time":0,"title":"","link":"","description":""})
}
fn frame(news: &Value) -> std::io::Result<Vec<u8>> {
    let news: Vec<NewsItem> = serde_json::from_value(news.clone()).unwrap();
    Ok(Response {protocol_id:5002,result:Ok(ResponseData {
        call_id:77,method_id:1,data:news.try_to_bytes()?
    })}.to_bytes())
}
fn exact_news(bytes: usize) -> Value {
    // Use actual serializers only to size input. Fragment validity oracle below is
    // independent of chunk-loop implementation and never narrows usize via as u8.
    let mut news = json!([news_item(),news_item(),news_item()]);
    let mut remaining = bytes.checked_sub(frame(&news).unwrap().len()).unwrap();
    for item in news.as_array_mut().unwrap() {
        for field in ["title","link","description"] {
            if remaining == 0 { break; }
            let growth = remaining.min(40_001);
            assert!(growth > 1, "fixed fixture target must not leave only terminator");
            item[field] = json!("N".repeat(growth - 1));
            remaining -= growth;
        }
    }
    assert_eq!(remaining, 0);
    assert_eq!(frame(&news).unwrap().len(), bytes);
    news
}

fn run_case(name: &str, news: Option<Value>, expected_bytes: Option<usize>, must_error: bool) {
    let _cwd = OwnedCwd::enter(name, news.as_ref());
    sodiumoxide::init().unwrap();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let ctx = Context::splinter_cell_blacklist();
    let port = VPort { stream_type:StreamType::RVSec,port:ctx.vport };
    let mut rmc = RVSecHandler::<()>::new(logger.clone());
    // Same enabled producer factory as start_server's secure branch.
    rmc.register_protocol(super::new_protocol());
    let mut registry = StreamHandlerRegistry::new(logger.clone());
    registry.register(port,Box::new(rmc));
    let mut server: TestServer<'_> = Server::new(logger,&ctx,registry);
    // Exactly one owned ephemeral socket for the real &UdpSocket API parameter.
    // The transport send leaf below is captured before socket.send_to.
    server.bind("127.0.0.1:0").unwrap();
    let address = "127.0.0.1:31001".parse().unwrap(); // Synthetic identity only; never bind/contact this endpoint.
    let signature = 0x4510_0001;
    let mut ci = quazal::ClientInfo::<()>::new(address);
    ci.user_id = Some(1000); // No DB/account read; production login_required remains in the path.
    server.maintenance_seed_connected(signature,ci,false);
    let capture = Capture::begin();
    let request = QPacket {source:port,destination:port,packet_type:PacketType::Data,
        signature,session_id:0,sequence:77,fragment_id:Some(0),
        payload:Request {protocol_id:5002,call_id:77,method_id:1,parameters:Vec::new()}.to_bytes(),
        ..Default::default()};
    server.maintenance_dispatch(request,address);
    let wire = capture.take();
    let mut responses = Vec::new();
    let mut ack_count = 0;
    for bytes in &wire {
        let (packet,consumed) = QPacket::from_bytes(&ctx,bytes).expect("actual encoded packet must parse");
        assert_eq!(consumed as usize,bytes.len());
        packet.validate(&ctx,bytes).expect("actual encoded checksum must validate");
        if packet.flags.contains(PacketFlag::Ack) {
            ack_count += 1;
        } else {
            assert_eq!(packet.packet_type,PacketType::Data);
            responses.push(packet);
        }
    }
    assert_eq!(ack_count,1,"actual request traverses ACK path once");
    assert!(!responses.is_empty(),"silent missing dispatch is not controlled rejection");
    let ids: Vec<_> = responses.iter().map(|p| p.fragment_id.unwrap()).collect();
    let payload: Vec<_> = responses.iter().flat_map(|p| p.payload.iter().copied()).collect();
    let Packet::Response(decoded) = Packet::from_bytes(&payload).expect("concatenated emitted data is complete RMC") else {
        panic!("expected RMC response");
    };
    assert_eq!(decoded.protocol_id,5002);
    let success = decoded.result.is_ok();
    if let Ok(data) = &decoded.result { assert_eq!((data.call_id,data.method_id),(77,1)); }
    else { assert_eq!(decoded.result.as_ref().unwrap_err().call_id,77); }
    if success {
        if let Some(expected) = expected_bytes { assert_eq!(payload.len(),expected,"real producer size"); }
        if let Some(news) = &news { assert_eq!(payload,frame(news).expect("valid strings serialize")); }
    }
    let zeros = ids.iter().filter(|&&id| id == 0).count();
    let unique = ids.iter().copied().collect::<std::collections::HashSet<_>>().len();
    let descending = ids.windows(2).all(|w| w[0].checked_sub(1)==Some(w[1]));
    let well_formed = ids.len() <= 256 && zeros == 1 && unique == ids.len()
        && ids.last()==Some(&0) && descending;
    let expected_success = !must_error;
    // For overflow only, an explicit RMC failure response is acceptable. This
    // does not accept "no packet", partial payload, missing producer or setup failure.
    let overflow = expected_bytes.is_some_and(|bytes| bytes > 256_000);
    let outcome_ok = if overflow { well_formed } else { well_formed && success == expected_success };
    println!("ISSUE45_JSON {}",json!({"case":name,"producer":"OverlordNews.get_news",
        "requested_frame_bytes":expected_bytes,"emitted_payload_bytes":payload.len(),
        "data_packets":ids.len(),"ack_packets":ack_count,"first_fragment":ids.first(),
        "zero_fragments":zeros,"unique_fragment_ids":unique,"descending":descending,
        "rmc_success":success,"pass":outcome_ok,"fragment_ids":ids}));
    assert!(outcome_ok,"u8 fragment IDs must encode one terminal fragment and no wrap or duplicate");
}

#[test]
fn issue45_default_news_control() { run_case("default_news",None,None,false); }
#[test]
fn issue45_exact_256000_bytes_control() {
    run_case("exact_256000",Some(exact_news(256_000)),Some(256_000),false);
}
#[test]
fn issue45_256001_bytes_must_not_wrap() {
    run_case("over_256001",Some(exact_news(256_001)),Some(256_001),false);
}
#[test]
fn issue45_overlong_string_is_controlled_error() {
    let mut news = json!([news_item()]);
    news[0]["title"] = json!("N".repeat(65_535));
    assert!(frame(&news).is_err(),"per-string limit is separate from aggregate fragmentation");
    run_case("overlong_string",Some(news),None,true);
}
