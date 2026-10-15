//! Private opt-in DX11 feedback through native ChatString transport.
//! Reserved team 255 keeps control messages out of unmodified retail chat UI.
//! No sockets, no pawn flags/mark writes, no display-name identity or local echo.
use crate::equipment_native::{readable, signature, word};
use crate::goggle_exposure::{ActualDetections, Source};
use crate::goggle_feedback::{Publisher, Receiver};
use crate::goggle_feedback_wire::{self, Inbox};
use crate::goggle_peer_frame;
use retour::static_detour;
use std::cell::Cell;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};
static READY: AtomicBool = AtomicBool::new(false);
static LOG_BITS: AtomicU32 = AtomicU32::new(0);
static INTEL_LOG_BITS: AtomicU32 = AtomicU32::new(0);
fn intel_log(receive: bool, active: bool, accepted: bool) {
    let bit = 1u32 << ((u32::from(receive) * 4) + (u32::from(active) * 2) + u32::from(accepted));
    if INTEL_LOG_BITS.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        tracing::info!(
            "SCBL_MOD_INTEL_RF revision=C51 receive={} active={} native_transport_accepted={}",
            receive,
            active,
            accepted
        );
    }
}
fn once(bit: u32, message: &str) {
    if LOG_BITS.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        tracing::info!("SCBL_MOD_EXPOSURE_BRIDGE {}", message);
    }
}

// One line per detector/reason for this DLL lifetime: bounded to 21 lines.
// No player IDs, envelope IDs, names, payload, or addresses enter these messages.
static REJECTION_LOG_BITS: AtomicU32 = AtomicU32::new(0);
fn rejection_bit(source: Source, reason: crate::goggle_feedback::ReceiptRejection) -> u32 {
    1u32 << (source as u32 * 7 + reason as u32)
}
fn rejection_log(source: Source, reason: crate::goggle_feedback::ReceiptRejection) {
    let bit = rejection_bit(source, reason);
    if REJECTION_LOG_BITS.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        let source = match source { Source::Rfd => "RFD", Source::Ats => "ATS", Source::Bms => "BMS" };
        tracing::info!("SCBL_MOD_EXPOSURE_BRIDGE source={} receipt_rejected={}", source, reason.label());
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
#[derive(Default)]
struct State {
    publisher: Publisher,
    receiver: Receiver,
    inbox: Inbox,
    intel: crate::intel_rfd_wire::State,
}
impl State {
    fn invalidate_context(&mut self) {
        // Clear untrusted samples and partial receipts, not wire sequence history.
        self.publisher.invalidate_samples();
        self.receiver = Receiver::default();
        self.inbox = Inbox::default();
        self.intel.invalidate_samples();
    }
}
#[repr(C)]
struct FString {
    data: *const u16,
    count: i32,
    capacity: i32,
}
static_detour! {
    static Set: unsafe extern "thiscall" fn(usize,usize,usize,u32,u32);
    static Receive: unsafe extern "thiscall" fn(usize,usize);
}
thread_local! {static OUTBOUND:Cell<usize>=const{Cell::new(0)};}
struct Scope(usize);
impl Drop for Scope {
    fn drop(&mut self) {
        OUTBOUND.with(|s| s.set(self.0));
    }
}
fn set(this: usize, name: usize, text: usize, team: u32, team_only: u32) {
    // Match the exact synchronous FString argument, not chat text supplied by users.
    let ours = OUTBOUND.with(|s| s.get() != 0 && s.get() == text);
    if ours {
        once(1 << 22, "reserved_team_applied=true");
    }
    unsafe {
        Set.call(
            this,
            name,
            text,
            if ours { 255 } else { team },
            if ours { 1 } else { team_only },
        );
    }
}
unsafe fn byte(p: usize, o: usize) -> Option<u8> {
    let p = p.checked_add(o)?;
    readable(p, 1).then(|| (p as *const u8).read())
}
fn receive(net: usize, message: usize) {
    unsafe {
        if READY.load(Ordering::Acquire)
            && word(message, 0) == Some(0x2b18344)
            && byte(message, 0x54) == Some(255)
            && byte(message, 0x64).is_some_and(|v| v != 0)
        {
            if let Some(text) = goggle_peer_frame::text(message) {
                if goggle_feedback_wire::reserved(&text) || crate::intel_rfd_wire::reserved(&text) {
                    once(1 << 16, "reserved_receipt=true");
                    match (word(message, 4), goggle_peer_frame::current()) {
                        (Some(from), Some(frame)) if frame.net == net => {
                            once(1 << 17, "receipt_frame_valid=true");
                            if let Ok(mut lock) = STATE.try_lock() {
                                let state = lock.get_or_insert_with(State::default);
                                if crate::intel_rfd_wire::reserved(&text) {
                                    if let Some(r) = crate::intel_rfd_wire::Report::decode(&text) {
                                        let accepted = state.intel.receive(&frame.context, from, r);
                                        intel_log(true, r.active, accepted);
                                    }
                                    return;
                                }
                                if let Some(report) =
                                    state.inbox.receive(&frame.context, from, &text)
                                {
                                    let source = report.detector;
                                    match state.receiver.receive_report(&frame.context, from, report) {
                                        Ok(()) => once(1 << (source as u32 + 8), source_label(source as u32, true)),
                                        Err(reason) => rejection_log(source, reason),
                                    }
                                }
                            }
                        }
                        _ => once(1 << 19, "receipt_rejected=current_native_frame"),
                    }
                    return; // reserved control message never enters chat UI
                }
            }
        }
        Receive.call(net, message); // ordinary global/team chat exactly once
    }
}
pub(crate) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}
pub(crate) unsafe fn intel_publish(pawn: usize, active: bool) {
    if !ready() || OUTBOUND.with(|s| s.get() != 0) {
        return;
    }
    let Some(frame) = goggle_peer_frame::current().filter(|f| f.pawn == pawn) else {
        return;
    };
    let r = STATE.try_lock().ok().and_then(|mut s| {
        s.get_or_insert_with(State::default)
            .intel
            .publish(&frame.context, active)
    });
    let Some(r) = r else {
        return;
    };
    let mut text = r.encode();
    text.push(0);
    let string = FString {
        data: text.as_ptr(),
        count: text.len() as i32,
        capacity: text.len() as i32,
    };
    let old = OUTBOUND.with(|s| s.replace(&string as *const _ as usize));
    let _scope = Scope(old);
    let send: unsafe extern "thiscall" fn(usize, *const FString, i32) -> i32 =
        std::mem::transmute(0xed0f80usize);
    let accepted = send(frame.manager, &string, 1);
    intel_log(false, active, accepted != 0); // Send result is NOT a remote receipt.
}
pub(crate) unsafe fn intel_remote(pawn: usize) -> Option<bool> {
    if !ready() {
        return None;
    }
    let frame = goggle_peer_frame::current()?;
    if pawn == frame.pawn {
        return None;
    }
    let peer = word(pawn, 0x2c0)? as usize;
    let actor = u64::from(word(peer, 0x70)?) | (u64::from(word(peer, 0x74)?) << 32);
    if !frame
        .context
        .person(actor)
        .is_some_and(|p| p.role == crate::ats_marks::Role::Spy)
    {
        return None;
    }
    STATE.try_lock().ok().map(|mut s| {
        s.get_or_insert_with(State::default)
            .intel
            .active(&frame.context, actor)
    })
}
fn source_label(source: u32, received: bool) -> &'static str {
    match (source, received) {
        (0, false) => "source=RFD native_send_accepted=true receipt_unconfirmed=true",
        (1, false) => "source=ATS native_send_accepted=true receipt_unconfirmed=true",
        (2, false) => "source=BMS native_send_accepted=true receipt_unconfirmed=true",
        (0, true) => "source=RFD native_receipt_valid=true",
        (1, true) => "source=ATS native_receipt_valid=true",
        (2, true) => "source=BMS native_receipt_valid=true",
        _ => "source=unknown",
    }
}
/// Only collector callbacks call this; an empty complete result withdraws.
/// ATS alternates entries: positive-only partial results have a bounded 200 ms hold.
pub(crate) fn observe(
    source: Source,
    world: usize,
    pawn: usize,
    now: u64,
    targets: &[u64],
    partial: bool,
) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    unsafe {
        let Some(frame) = goggle_peer_frame::current() else {
            once(1 << 20, "send_skipped=current_native_frame");
            return;
        };
        once(1 << 21, "send_frame_valid=true");
        if (frame.world, frame.pawn, frame.context.now) != (world, pawn, now) {
            return;
        }
        let report = STATE.try_lock().ok().and_then(|mut s| {
            s.get_or_insert_with(State::default).publisher.sample(
                &frame.context,
                source,
                targets,
                partial,
            )
        });
        let Some(packets) = report.as_ref().and_then(goggle_feedback_wire::packets) else {
            return;
        };
        if OUTBOUND.with(|s| s.get() != 0) {
            return;
        }
        let send: unsafe extern "thiscall" fn(usize, *const FString, i32) -> i32 =
            std::mem::transmute(0xed0f80usize);
        let mut all_accepted = true;
        for mut text in packets {
            debug_assert!(text.len() <= goggle_feedback_wire::NATIVE_LIMIT);
            text.push(0);
            let string = FString {
                data: text.as_ptr(),
                count: text.len() as i32,
                capacity: text.len() as i32,
            };
            let old = OUTBOUND.with(|s| s.replace(&string as *const _ as usize));
            let _scope = Scope(old);
            all_accepted &= send(frame.manager, &string, 1) as u8 != 0;
        }
        if all_accepted {
            once(1 << source as u32, source_label(source as u32, false));
        }
    }
}
pub(crate) fn detections(world: usize, pawn: usize, now: u64) -> ActualDetections {
    if !READY.load(Ordering::Acquire) {
        return ActualDetections::default();
    }
    unsafe {
        let Some(frame) = goggle_peer_frame::current() else {
            if let Ok(mut s) = STATE.try_lock() {
                if let Some(state) = s.as_mut() {
                    state.invalidate_context();
                }
            }
            return ActualDetections::default();
        };
        if (frame.world, frame.pawn, frame.context.now) != (world, pawn, now) {
            return ActualDetections::default();
        }
        STATE
            .try_lock()
            .ok()
            .map(|mut s| {
                s.get_or_insert_with(State::default)
                    .receiver
                    .detections(&frame.context)
            })
            .unwrap_or_default()
    }
}
const CONTRACTS: &[(usize, usize, &str)] = &[
    (
        0x20df1c0,
        68,
        "a7ec1b68c51b37d7df47727c4f4433e50f7dad0c35d62460fbf8a50903a738c0",
    ),
    (
        0xed0f80,
        285,
        "fd04a38294fc29aad21273afa9220da5a952dca2ce182db7862d6045ad56814d",
    ),
    (
        0x10b5110,
        319,
        "581ef3578c1fd05aa3f229bf89ff329f8bde8bce090fe9325daae99bda0183bc",
    ),
    (
        0x104ed60,
        284,
        "2c39cb5ca133aa09c22e7c7a369d4059c280a4332db26069066daec7cd9d5c22",
    ),
    (
        0x10960e0,
        355,
        "8d7647d10feefe47e9243d1b76db9d8187703cb9130843560eec19ad8ceeb979",
    ),
    (
        0x7f3099,
        20,
        "9453bbde9bd259988ca0a7fb5e9a9ad708a431dc94a6c98c63f35db4edb1e13a",
    ),
    (
        0x83b32b,
        40,
        "2da89d240f07f810aa0ded07bb7f7c24a1a060199749c127a30bda7ae989a41c",
    ),
];
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860 || READY.load(Ordering::Acquire) {
        return;
    }
    if !CONTRACTS.iter().all(|&(p, n, h)| signature(p, n, h)) {
        once(1 << 23, "init_rejected=native_contract");
        return;
    }
    if Set.initialize(std::mem::transmute::<usize,unsafe extern "thiscall" fn(usize,usize,usize,u32,u32)>(0x10960e0),set).and_then(|h|h.enable()).is_err(){return;}
    if Receive
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize, usize)>(0x104ed60),
            receive,
        )
        .and_then(|h| h.enable())
        .is_err()
    {
        let _ = Set.disable();
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_MOD_EXPOSURE_BRIDGE opt_in=goggle-feedback native_From=true bounded_lease_ms=400 live_verified=false");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Receive.disable();
    let _ = Set.disable();
    if let Ok(mut s) = STATE.try_lock() {
        *s = None;
    } // never block loader-lock shutdown
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    fn fixture() -> Vec<u8> {
        std::fs::read(
            std::path::PathBuf::from(std::env::var("SCBL_R58_C37_FIXTURE").expect("C37 fixture"))
                .join("native-010960e0.bin"),
        )
        .unwrap()
    }
    #[test]
    fn setter_contract_has_exact_callee_pop_and_ignorable_result() {
        let raw = fixture();
        assert_eq!(raw.len(), 355);
        assert_eq!(
            format!("{:x}", Sha256::digest(&raw)),
            "8d7647d10feefe47e9243d1b76db9d8187703cb9130843560eec19ad8ceeb979"
        );
        assert_eq!(&raw[352..], &[0xc2, 0x10, 0]);
        assert_eq!(std::mem::size_of::<FString>(), 12);
    }
    #[test]
    fn executable_retail_setter_encodes_reserved_team_and_dirty_bits() {
        let raw = fixture();
        let block = &raw[0x11a..0x150];
        // Preserve exact retail scalar field writes; bypass string allocator/SEH.
        // Four thiscall stack arguments keep native EBP offsets unchanged.
        let mut code = vec![0x55, 0x8b, 0xec, 0x56, 0x8b, 0xf1];
        code.extend(block);
        code.extend([0x5e, 0xc9, 0xc2, 0x10, 0]);
        unsafe {
            let mem = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
            assert!(!mem.is_null());
            std::ptr::copy_nonoverlapping(code.as_ptr(), mem as *mut u8, code.len());
            let f: unsafe extern "thiscall" fn(usize, usize, usize, u32, u32) =
                std::mem::transmute(mem);
            let mut msg = [0u32; 26];
            let mut team_dirty = 0u8;
            let mut only_dirty = 0u8;
            msg[0x4c / 4] = &mut team_dirty as *mut _ as u32;
            msg[0x5c / 4] = &mut only_dirty as *mut _ as u32;
            f(msg.as_mut_ptr() as usize, 0, 0, 255, 1);
            let bytes = std::slice::from_raw_parts(msg.as_ptr() as *const u8, 0x68);
            assert_eq!(
                (bytes[0x54], bytes[0x64], bytes[0x50], bytes[0x60]),
                (255, 255, 1, 1)
            );
            assert_eq!((team_dirty, only_dirty), (1, 1));
            f(msg.as_mut_ptr() as usize, 0, 0, 2, 0);
            let bytes = std::slice::from_raw_parts(msg.as_ptr() as *const u8, 0x68);
            assert_eq!((bytes[0x54], bytes[0x64]), (2, 0));
            VirtualFree(mem, 0, MEM_RELEASE).unwrap();
        }
    }
    #[test]
    fn outgoing_identity_scope_is_nested_and_restored() {
        assert_eq!(OUTBOUND.with(|s| s.get()), 0);
        {
            let old = OUTBOUND.with(|s| s.replace(100));
            let _s = Scope(old);
            assert_eq!(OUTBOUND.with(|s| s.get()), 100);
            {
                let old = OUTBOUND.with(|s| s.replace(200));
                let _s = Scope(old);
                assert_eq!(OUTBOUND.with(|s| s.get()), 200);
            }
            assert_eq!(OUTBOUND.with(|s| s.get()), 100);
        }
        assert_eq!(OUTBOUND.with(|s| s.get()), 0);
    }
    #[test]
    fn native_internal_string_setter_rejects_81_even_when_outer_sender_accepts_it() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COPIES: AtomicU32 = AtomicU32::new(0);
        unsafe extern "thiscall" fn equal(_: usize, _: usize) -> u8 {
            0
        }
        unsafe extern "thiscall" fn copy(_: usize, _: usize) {
            COPIES.fetch_add(1, Ordering::SeqCst);
        }
        let path =
            std::path::PathBuf::from(std::env::var("SCBL_R58_C38_FIXTURE").expect("C38 fixture"))
                .join("native-020df1c0.bin");
        let raw = std::fs::read(path).unwrap();
        assert_eq!(raw.len(), 68);
        assert_eq!(
            format!("{:x}", Sha256::digest(&raw)),
            "a7ec1b68c51b37d7df47727c4f4433e50f7dad0c35d62460fbf8a50903a738c0"
        );
        unsafe {
            let mem = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
            assert!(!mem.is_null());
            std::ptr::copy_nonoverlapping(raw.as_ptr(), mem as *mut u8, raw.len());
            for (offset, target) in [(0x1e, equal as usize), (0x38, copy as usize)] {
                assert_eq!(raw[offset], 0xe8);
                ((mem as usize + offset + 1) as *mut u32)
                    .write_unaligned((target as u32).wrapping_sub(mem as u32 + offset as u32 + 5));
            }
            let f: unsafe extern "thiscall" fn(usize, usize) = std::mem::transmute(mem);
            for (length, accepted) in [
                (0, true),
                (1, true),
                (79, true),
                (80, true),
                (81, false),
                (123, false),
                (144, false),
            ] {
                let buffer = [1u32, length];
                let input = [0u32, 0, buffer.as_ptr() as u32];
                let mut property = [0u32; 8];
                let before = COPIES.load(Ordering::SeqCst);
                f(property.as_mut_ptr() as usize, input.as_ptr() as usize);
                assert_eq!(
                    COPIES.load(Ordering::SeqCst) - before,
                    u32::from(accepted),
                    "{length}"
                );
                assert_eq!(property[2] as u8, u8::from(accepted), "{length}");
            }
            VirtualFree(mem, 0, MEM_RELEASE).unwrap();
        }
    }

    #[test]
    fn receipt_diagnostic_bits_are_disjoint_and_bounded_per_source_reason() {
        use crate::goggle_feedback::ReceiptRejection;
        let mut seen=0u32;let mut lines=0;
        let once=AtomicU32::new(0);
        for source in [Source::Rfd,Source::Ats,Source::Bms] {
            for reason in [ReceiptRejection::Context,ReceiptRejection::Report,ReceiptRejection::Match,
                ReceiptRejection::Source,ReceiptRejection::Targets,ReceiptRejection::Sequence,ReceiptRejection::Capacity] {
                let bit=rejection_bit(source,reason);assert!(bit.is_power_of_two());assert_eq!(seen&bit,0);seen|=bit;
                for _ in 0..1000 {if once.fetch_or(bit,Ordering::Relaxed)&bit==0 {lines+=1;}}
            }
        }
        assert_eq!(seen.count_ones(),21);assert_eq!(lines,21);assert_eq!(seen,(1<<21)-1);
        println!("RECEIPT_DIAGNOSTIC_LOG attempts=21000 lines=21 per_source_reason=true identifiers_logged=false");
    }

    #[test]
    fn receipt_diagnostics_emit_bounded_source_specific_messages() {
        use crate::goggle_feedback::ReceiptRejection;
        use std::sync::Arc;
        use tracing::{Event,Metadata,Subscriber,span::{Attributes,Id,Record},field::{Field,Visit}};
        #[derive(Clone)] struct Sink(Arc<Mutex<Vec<String>>>);
        impl Subscriber for Sink {
            fn enabled(&self,_:&Metadata<'_>)->bool {true}
            fn new_span(&self,_:&Attributes<'_>)->Id {Id::from_u64(1)}
            fn record(&self,_:&Id,_:&Record<'_>) {}
            fn record_follows_from(&self,_:&Id,_:&Id) {}
            fn enter(&self,_:&Id) {}
            fn exit(&self,_:&Id) {}
            fn event(&self,event:&Event<'_>) {
                struct Message(String);
                impl Visit for Message {fn record_debug(&mut self,field:&Field,value:&dyn std::fmt::Debug) {
                    if field.name()=="message" {self.0=format!("{value:?}");}
                }}
                let mut message=Message(String::new());event.record(&mut message);
                self.0.lock().unwrap().push(message.0);
            }
        }
        let output=Arc::new(Mutex::new(Vec::new()));
        let old=REJECTION_LOG_BITS.swap(0,Ordering::SeqCst);
        tracing::subscriber::with_default(Sink(output.clone()),|| {
            for source in [Source::Rfd,Source::Ats,Source::Bms] {
                for reason in [ReceiptRejection::Context,ReceiptRejection::Report,ReceiptRejection::Match,
                    ReceiptRejection::Source,ReceiptRejection::Targets,ReceiptRejection::Sequence,ReceiptRejection::Capacity] {
                    for _ in 0..1000 {rejection_log(source,reason);}
                }
            }
        });
        REJECTION_LOG_BITS.store(old,Ordering::SeqCst);
        let output=output.lock().unwrap();assert_eq!(output.len(),21);
        for source in ["RFD","ATS","BMS"] {
            for reason in ["invalid_context","invalid_report","different_match","source_owner_or_role",
                "target_role_team_or_roster","stale_or_duplicate_sequence","receipt_capacity"] {
                let expected=format!("SCBL_MOD_EXPOSURE_BRIDGE source={source} receipt_rejected={reason}");
                assert_eq!(output.iter().filter(|s|**s==expected).count(),1);
            }
        }
        for line in output.iter() {println!("RECEIPT_LOG {line}");}
    }
}


#[cfg(test)]
mod native_context_state_tests {
    use super::*;
    use crate::ats_marks::Role;
    use crate::goggle_feedback::{Context, Person};
    #[test]
    fn invalid_context_clears_samples_without_restarting_outgoing_sequences() {
        let merc = Context { scope: [1, 2, 10, 1], match_key: 55, now: 1000, local: 10,
            session_peers: vec![7, 20],
            people: vec![Person { actor: 10, peer: 7, team: 1, role: Role::Merc },
                         Person { actor: 20, peer: 20, team: 2, role: Role::Spy }] };
        let spy = Context { scope: [1, 2, 20, 1], local: 20, ..merc.clone() };
        let mut state = State::default();
        let exposure = state.publisher.sample(&merc, Source::Rfd, &[20], false).unwrap();
        let intel = state.intel.publish(&spy, true).unwrap();
        assert!(state.receiver.receive_report(&spy, 7, exposure.clone()).is_ok());
        assert!(state.intel.receive(&spy, 20, intel));
        assert_ne!(state.receiver.detections(&spy), ActualDetections::default());
        assert!(state.intel.active(&spy, 20));
        state.invalidate_context();
        state.invalidate_context();
        assert_eq!(state.receiver.detections(&spy), ActualDetections::default());
        assert!(!state.intel.active(&spy, 20));
        assert!(state.publisher.sample(&merc, Source::Rfd, &[], true).is_none());
        let fresh = state.publisher.sample(&merc, Source::Rfd, &[20], false).unwrap();
        assert_eq!(fresh.seq, exposure.seq + 1);
        assert_eq!(state.intel.publish(&spy, true).unwrap().seq, intel.seq + 1);
    }
}
