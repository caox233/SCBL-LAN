//! Implements the `NatTraversalProtocolServer` for handling NAT traversal requests,
//! such as initiating probes to other clients.

use quazal::prudp::packet::PacketType;
use quazal::prudp::packet::QPacket;
use quazal::prudp::packet::StreamType;
use quazal::prudp::packet::VPort;
use quazal::prudp::ClientRegistry;
use quazal::rmc::basic::ToStream;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
use quazal::ClientInfo;
use quazal::Context;
use slog::Logger;

use crate::login_required;
use crate::protocols::nat_traversal::nat_traversal_protocol::InitiateProbeRequest;
use crate::protocols::nat_traversal::nat_traversal_protocol::NatTraversalProtocolMethod;
use crate::protocols::nat_traversal::nat_traversal_protocol::NatTraversalProtocolServer;
use crate::protocols::nat_traversal::nat_traversal_protocol::NatTraversalProtocolServerTrait;
use crate::protocols::nat_traversal::nat_traversal_protocol::RequestProbeInitiationExtRequest;
use crate::protocols::nat_traversal::nat_traversal_protocol::RequestProbeInitiationExtResponse;
use crate::protocols::nat_traversal::nat_traversal_protocol::NAT_TRAVERSAL_PROTOCOL_ID;

/// Implementation of the `NatTraversalProtocolServerTrait` for NAT traversal operations.
struct NatTraversalProtocolServerImpl;

impl<T> NatTraversalProtocolServerTrait<T> for NatTraversalProtocolServerImpl {
    /// Handles the `RequestProbeInitiationExt` request.
    ///
    /// This method is responsible for initiating NAT probes to other clients.
    fn request_probe_initiation_ext(
        &self,
        logger: &Logger,
        ctx: &Context,
        ci: &mut ClientInfo<T>,
        request: RequestProbeInitiationExtRequest,
        client_registry: &ClientRegistry<T>,
        socket: &std::net::UdpSocket,
    ) -> Result<RequestProbeInitiationExtResponse, Error> {
        // Ensure the client is logged in.
        let _user_id = login_required(&*ci)?;
        info!(logger, "Probe initiation requested: {request:?}");

        // Iterate over each target URL provided in the request.
        for url in request.url_target_list.iter() {
            // Extract the connection ID (RVCID) from the URL parameters.
            let Some(conn_id) = url.params.get("RVCID") else {
                warn!(logger, "{url} doesn't include RVCID");
                continue;
            };
            // Parse the connection ID into a u32.
            let Ok(conn_id) = conn_id.parse() else {
                warn!(logger, "{url} doesn't include valid RVCID");
                continue;
            };

            // Dispatch already holds this client's mutable registry borrow.
            // Self-probing has no peer to notify; retain the remaining targets.
            if ci.connection_id == Some(conn_id) {
                continue;
            }

            // Find the target client in the registry using the connection ID.
            let Some(target) = client_registry.client_by_connection_id(conn_id) else {
                warn!(logger, "No client found for RVCID {conn_id:?}");
                continue;
            };

            // Construct the payload for the InitiateProbe request.
            let payload = Request {
                protocol_id: NAT_TRAVERSAL_PROTOCOL_ID,
                call_id: rand::random(), // Generate a random call ID for the probe.
                method_id: NatTraversalProtocolMethod::InitiateProbe as u32,
                parameters: InitiateProbeRequest {
                    url_station_to_probe: request.url_station_to_probe.clone(),
                }
                .try_to_bytes()?,
            }
            .to_bytes();

            // The commented-out section below shows an example of a hardcoded payload.
            // This is kept for reference but is not actively used.
            // let payload = Request {
            //     protocol_id: 14,
            //     call_id: rand::random(),
            //     method_id: 1,
            //     parameters: b"\x0c#\x1d\x00[\x1b\x00\x0015\x19\x00\xbd\xb3\x98\x02\x01\x00\x00\x01\x00\x00\x00".to_vec(),
            // }
            // .to_bytes();

            // Get the address of the target client.
            let addr = { *target.borrow().address() };
            info!(logger, "Sending probe to {url} ({addr})\n{payload:x?}");

            // Create a QPacket for sending the probe.
            let qpacket = QPacket {
                source: VPort {
                    port: 1,
                    stream_type: StreamType::RVSec,
                },
                destination: VPort {
                    port: 15,
                    stream_type: StreamType::RVSec,
                },
                packet_type: PacketType::Data,
                payload,
                ..Default::default()
            };
            // Send the QPacket to the target client.
            if let Err(error) = quazal::prudp::send_request(logger, ctx, &addr, socket, qpacket, &mut *target.borrow_mut()) {
                warn!(logger, "Failed to send NAT traversal probe"; "target" => addr.to_string(), "error" => %error);
                return Err(Error::InternalError);
            }
        }
        Ok(RequestProbeInitiationExtResponse)
    }
}

/// Creates a new boxed `NatTraversalProtocolServer` instance.
///
/// This function is typically used to register the NAT traversal protocol
/// with the server's protocol dispatcher.
pub fn new_protocol<T: 'static>() -> Box<dyn Protocol<T>> {
    Box::new(NatTraversalProtocolServer::new(NatTraversalProtocolServerImpl))
}


#[cfg(test)]
mod maintenance_nat_tests {
    use super::*;
    use std::net::{UdpSocket,SocketAddr};
    use std::time::Duration;
    use std::panic::{catch_unwind,AssertUnwindSafe};
    use quazal::prudp::packet::{PacketFlag,StreamHandler,StreamHandlerRegistry};
    use quazal::rmc::basic::{FromStream,ReadStream};
    use quazal::rmc::types::{QList,StationURL};

    // Test-client implementation only; production crypto visibility is unchanged.
    fn fixture_rc4(key:&[u8],data:&[u8])->Vec<u8> {
        assert!(!key.is_empty());
        let mut state=[0u8;256];
        for (i,value) in state.iter_mut().enumerate() {*value=u8::try_from(i).unwrap();}
        let mut j=0u8;
        for i in 0..256 {j=j.wrapping_add(state[i]).wrapping_add(key[i%key.len()]);state.swap(i,usize::from(j));}
        let(mut i,mut j)=(0u8,0u8);
        data.iter().map(|byte| {
            i=i.wrapping_add(1);j=j.wrapping_add(state[usize::from(i)]);
            state.swap(usize::from(i),usize::from(j));
            byte ^ state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))]
        }).collect()
    }
    struct StopFixture;
    struct Capture;
    impl StreamHandler<()> for Capture {
        fn handle(&self,logger:&Logger,ctx:&Context,ci:&mut ClientInfo<()>,data:&[u8],registry:&ClientRegistry<()>,socket:&UdpSocket)->Result<Vec<u8>,quazal::prudp::packet::Error> {
            if data==b"fixture-connection-id" {return Ok(ci.connection_id.map_or(0,u32::from).to_bytes());}
            let quazal::rmc::Packet::Request(request)=quazal::rmc::Packet::from_bytes(data).unwrap() else {panic!("request expected")};
            // Keep the real Server::handle_data RefMut alive. Only the fixture
            // captures panics so all cases can be observed on the same server.
            let outcome=catch_unwind(AssertUnwindSafe(||new_protocol::<()>().handle(logger,ctx,ci,&request,registry,socket)));
            Ok(vec![match outcome {Ok(Ok(_))=>0,Ok(Err(Error::AccessDenied))=>1,Err(_)=>2,Ok(Err(_))=>3}])
        }
    }
    fn stop_fixture(_: &Logger,_:QPacket,_:SocketAddr,_:&UdpSocket) {std::panic::panic_any(StopFixture);}
    fn ports()->(VPort,VPort) {(VPort{port:15,stream_type:StreamType::RVSec},VPort{port:1,stream_type:StreamType::RVSec})}
    fn recv(socket:&UdpSocket,ctx:&Context)->QPacket {
        let mut buf=[0u8;4096];let(n,_)=socket.recv_from(&mut buf).unwrap();
        let (packet,_)=QPacket::from_bytes(ctx,&buf[..n]).unwrap();packet.validate(ctx,&buf[..n]).unwrap();packet
    }
    fn data_response(socket:&UdpSocket,ctx:&Context)->Vec<u8> {
        loop {let packet=recv(socket,ctx);if packet.packet_type==PacketType::Data && !packet.flags.contains(PacketFlag::Ack) {return packet.payload;}}
    }
    struct Client {socket:UdpSocket,signature:u32,sequence:u16,cid:u32}
    impl Client {
        fn request(&mut self,ctx:&Context,addr:SocketAddr,data:Vec<u8>)->Vec<u8> {
            let(source,destination)=ports();self.sequence+=1;
            let p=QPacket{source,destination,packet_type:PacketType::Data,signature:self.signature,session_id:7,sequence:self.sequence,fragment_id:Some(0),payload:data,flags:PacketFlag::HasSize.into(),..Default::default()};
            self.socket.send_to(&p.to_bytes(ctx),addr).unwrap();data_response(&self.socket,ctx)
        }
        fn connect(ctx:&Context,addr:SocketAddr,pid:Option<u32>)->Self {
            use sodiumoxide::crypto::secretbox;
            let socket=UdpSocket::bind("127.0.0.1:0").unwrap();socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let(source,destination)=ports();
            let syn=QPacket{source,destination,packet_type:PacketType::Syn,conn_signature:Some(0),flags:PacketFlag::HasSize.into(),..Default::default()};
            socket.send_to(&syn.to_bytes(ctx),addr).unwrap();let signature=recv(&socket,ctx).conn_signature.unwrap();
            let mut payload=Vec::new();
            if let Some(pid)=pid {
                let key=[0x57;16];let ticket=quazal::kerberos::KerberosTicketInternal{principle_id:pid,valid_until:u64::MAX,session_key:key};
                let nonce=secretbox::gen_nonce();let mut sealed=nonce.as_ref().to_vec();sealed.extend(secretbox::seal(&ticket.to_bytes(),&nonce,&ctx.ticket_key));
                let mut challenge=pid.to_bytes();challenge.extend(7u32.to_bytes());challenge.extend(41u32.to_bytes());
                payload=sealed.to_bytes();payload.extend(fixture_rc4(&key,&challenge).to_bytes());
            }
            let p=QPacket{source,destination,packet_type:PacketType::Connect,conn_signature:Some(0x1234),signature,session_id:7,sequence:1,payload,flags:PacketFlag::HasSize.into(),..Default::default()};
            socket.send_to(&p.to_bytes(ctx),addr).unwrap();assert_eq!(recv(&socket,ctx).packet_type,PacketType::Connect);
            let mut client=Self{socket,signature,sequence:1,cid:0};let reply=client.request(ctx,addr,b"fixture-connection-id".to_vec());client.cid=ReadStream::from_bytes(&reply).read().unwrap();assert_eq!(client.cid!=0,pid.is_some());client
        }
    }
    struct OwnedServer {addr:SocketAddr,stop:Vec<u8>,handle:Option<std::thread::JoinHandle<bool>>}
    impl OwnedServer {
        fn stop(&mut self)->bool {
            let Some(handle)=self.handle.take() else {return true};
            let socket=UdpSocket::bind("127.0.0.1:0").unwrap();for _ in 0..3 {let _=socket.send_to(&self.stop,self.addr);}
            handle.join().unwrap_or(false)
        }
    }
    impl Drop for OwnedServer {fn drop(&mut self) {let _=self.stop();}}
    #[test]
    fn native_nat_self_target_does_not_panic() {
        assert_eq!(fixture_rc4(b"Key",b"Plaintext"),vec![0xbb,0xf3,0x16,0xe8,0xd9,0x40,0xaf,0x0a,0xd3]);
        sodiumoxide::init().unwrap();let ctx=Context::splinter_cell_blacklist();
        let reservation=UdpSocket::bind("127.0.0.1:0").unwrap();let addr=reservation.local_addr().unwrap();drop(reservation);
        let server_ctx=ctx.clone();let(ready_tx,ready_rx)=std::sync::mpsc::channel();
        let handle=std::thread::spawn(move || {
            let logger=Logger::root(slog::Discard,slog::o!());let mut streams=StreamHandlerRegistry::new(logger.clone());streams.register(ports().1,Box::new(Capture));
            let mut server:quazal::prudp::Server<'_,fn(ClientInfo<()>),fn(ClientInfo<()>),()>=quazal::prudp::Server::new(logger,&server_ctx,streams);
            server.user_handler=Some(stop_fixture);server.bind(addr).unwrap();ready_tx.send(()).unwrap();
            match catch_unwind(AssertUnwindSafe(||server.serve())) {Err(payload)=>payload.is::<StopFixture>(),Ok(())=>false}
        });
        let (source,destination)=ports();let stop=QPacket{source,destination,packet_type:PacketType::User,flags:PacketFlag::HasSize.into(),..Default::default()}.to_bytes(&ctx);
        let mut server=OwnedServer{addr,stop,handle:Some(handle)};ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut caller=Client::connect(&ctx,addr,Some(1));let peer=Client::connect(&ctx,addr,Some(2));let mut anonymous=Client::connect(&ctx,addr,None);
        peer.socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let url=|cid:&str| format!("prudp:/address=127.0.0.1;port=13000;RVCID={cid};type=2").parse::<StationURL>().unwrap();
        let self_url=url(&caller.cid.to_string());let peer_url=url(&peer.cid.to_string());
        let station:StationURL="prudp:/address=127.0.0.1;port=13001;sid=15;type=3".parse().unwrap();
        let mut panics=0;let mut violations=0;
        for case in ["self","peer","self_peer","peer_self","self_twice","unknown","bad_id","no_id","anonymous"] {
            let targets=match case {"self"=>vec![self_url.clone()],"self_peer"=>vec![self_url.clone(),peer_url.clone()],"peer_self"=>vec![peer_url.clone(),self_url.clone()],"self_twice"=>vec![self_url.clone(),self_url.clone()],"unknown"=>vec![url("4294967295")],"bad_id"=>vec![url("bad")],"no_id"=>vec![station.clone()],_=>vec![peer_url.clone()]};
            let req=Request{protocol_id:NAT_TRAVERSAL_PROTOCOL_ID,call_id:1,method_id:NatTraversalProtocolMethod::RequestProbeInitiationExt as u32,parameters:RequestProbeInitiationExtRequest{url_target_list:QList(targets),url_station_to_probe:station.clone()}.to_bytes()};
            let status=if case=="anonymous" {anonymous.request(&ctx,addr,req.to_bytes())}else{caller.request(&ctx,addr,req.to_bytes())};assert_eq!(status.len(),1);let status=status[0];
            let mut forwarded=0;let mut buffer=[0u8;4096];
            loop {
                match peer.socket.recv_from(&mut buffer) {
                    Ok((n,_))=>{let(packet,_)=QPacket::from_bytes(&ctx,&buffer[..n]).unwrap();packet.validate(&ctx,&buffer[..n]).unwrap();
                        if packet.packet_type!=PacketType::Data || packet.flags.contains(PacketFlag::Ack) {continue;}
                        let quazal::rmc::Packet::Request(request)=quazal::rmc::Packet::from_bytes(&packet.payload).unwrap() else {panic!("NAT request expected")};
                        assert_eq!(request.protocol_id,NAT_TRAVERSAL_PROTOCOL_ID);assert_eq!(request.method_id,NatTraversalProtocolMethod::InitiateProbe as u32);
                        let decoded=InitiateProbeRequest::from_bytes(&request.parameters).unwrap();assert_eq!(decoded.url_station_to_probe.scheme,station.scheme);assert_eq!(decoded.url_station_to_probe.address,station.address);assert_eq!(decoded.url_station_to_probe.port,station.port);assert_eq!(decoded.url_station_to_probe.params,station.params);forwarded+=1;}
                    Err(e) if matches!(e.kind(),std::io::ErrorKind::TimedOut|std::io::ErrorKind::WouldBlock)=>break,
                    Err(e)=>panic!("peer receive failed:{e}"),
                }
            }
            let expected_forwarded=usize::from(matches!(case,"peer"|"self_peer"|"peer_self"));let expected_status=u8::from(case=="anonymous");
            if status==2 {panics+=1;}let valid=status==expected_status && forwarded==expected_forwarded;if !valid {violations+=1;}
            println!("NAT_SELF case={case} status={status} forwarded={forwarded} valid={valid} station_preserved=true");
        }
        assert!(server.stop());println!("NAT_SELF_SERVER stopped=true intentional_fixture_stop=true");
        println!("NAT_SELF_SUMMARY cases=9 panics={panics} violations={violations} actual_prudp_dispatch=true loopback_only=true");assert_eq!(violations,0);
    }
}
