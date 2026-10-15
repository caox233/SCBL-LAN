use std::collections::HashSet;
use std::ffi::c_char;
use std::ffi::c_int;
use std::ffi::c_void;
use std::ffi::CStr;
use std::net::Ipv4Addr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use dll_syringe::function::FunctionPtr;
use retour::static_detour;
use tracing::debug;
use tracing::info;
use tracing::instrument;
use tracing::warn;
use windows::core::s;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::Networking::WinSock::WSAGetLastError;
use windows::Win32::Networking::WinSock::WSASetLastError;
use windows::Win32::Networking::WinSock::AF_INET;
use windows::Win32::Networking::WinSock::SOCKADDR;
use windows::Win32::Networking::WinSock::WSA_ERROR;
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::Win32::System::LibraryLoader::LoadLibraryA;

use crate::addresses::Addresses;
use crate::config;
use crate::config::Config;
use crate::config::Hook;

static_detour! {
    static SomeEventHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
    static SomeEvent2Hook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
    static BindSocketHook: unsafe extern "stdcall" fn(usize, *const SOCKADDR, c_int) -> c_int;
    static ConnectHook: unsafe extern "stdcall" fn(usize, *const SOCKADDR, c_int) -> c_int;
    static WsaConnectHook: unsafe extern "stdcall" fn(usize, *const SOCKADDR, c_int, *mut c_void, *mut c_void, *mut c_void, *mut c_void) -> c_int;
    static CloseSocketHook: unsafe extern "stdcall" fn(usize) -> c_int;
    static SendToHook: unsafe extern "stdcall" fn(usize, *const c_char, c_int, c_int, *const SOCKADDR, c_int) -> c_int;
    static RecvFromHook: unsafe extern "stdcall" fn(usize, *const c_char, c_int, c_int, *const SOCKADDR, *mut c_int) -> c_int;
    static EventMaybeQueuePopHook: unsafe extern "thiscall" fn(usize) -> *const  *const *const c_void;
    static EventHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> usize;
}

static_detour! {
    static SessionLeaveMapLookupHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static HostMigrationEndProducerAHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static HostMigrationEndProducerBHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,usize,usize,usize,usize,usize,usize) -> *mut c_void;
    static HostMigrationUpdateHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void);
    static SessionRestartInEventHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,usize,usize) -> *mut c_void;
    static SessionRestartProducerHook: unsafe extern "C" fn(*mut c_void,u32,u32,u32,u32) -> *mut c_void;
    static SessionPeerConnectedHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static SessionLocalTopologyHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static SessionPeerJoiningHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static SessionPeerGoneHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static SessionPeerUnreachableHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static SessionExitHostMigrationHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
    static SessionWaitingRestartHandlerHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
}

static_detour! {
    static HostMigrationGameStateUpdateHook: unsafe extern "thiscall" fn(*mut c_void);
}

static_detour! {
    static HostElectAcknowledgeRouterHook: unsafe extern "thiscall" fn(*mut c_void,*mut c_void,*mut c_void) -> *mut c_void;
}

type NativeHostElectAcknowledgeHandler = unsafe extern "thiscall" fn(*mut c_void, *mut c_void, usize, usize, usize, usize, usize) -> *mut c_void;
type NativeContinueHostMigrationParticipant = unsafe extern "thiscall" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
type NativeMigrationParticipantPresent = unsafe extern "thiscall" fn(*mut c_void, u32) -> u8;
type NativeMigrationParticipantIdentity = unsafe extern "thiscall" fn(*mut c_void, u32) -> *mut c_void;
type NativeMigrationPlatformReady = unsafe extern "thiscall" fn(*mut c_void) -> u8;
type NativeMigrationPlatformHost = unsafe extern "thiscall" fn(*mut c_void) -> u32;

const NATIVE_HOST_ELECT_ACKNOWLEDGE_HANDLER_ADDR: usize = 0x020A_DDD0;
const NATIVE_CONTINUE_HOST_MIGRATION_PARTICIPANT_ADDR: usize = 0x0207_5C60;
const NATIVE_MIGRATION_PARTICIPANT_PRESENT_ADDR: usize = 0x007D_2B20;
const NATIVE_MIGRATION_PARTICIPANT_IDENTITY_ADDR: usize = 0x007D_2BC0;
const NATIVE_PLATFORM_TABLE_ADDR: usize = 0x0338_3DF8;

static LOG_STORM_PACKETS: AtomicBool = AtomicBool::new(false);
static LOG_STORM_EVENTS: AtomicBool = AtomicBool::new(false);

const WSAEWOULDBLOCK_CODE: c_int = 10035;
const WSAEINPROGRESS_CODE: c_int = 10036;
const WSAEALREADY_CODE: c_int = 10037;
const RECV_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct RecvErrorBurst {
    socket: usize,
    error_code: c_int,
    last_reported: Instant,
    last_observed: Instant,
    suppressed: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum RecvErrorLogDecision {
    ReportFirst,
    ReportOngoing { suppressed: u32 },
    Suppress,
}

#[derive(Debug, Default)]
struct RecvErrorLogState {
    active: Option<RecvErrorBurst>,
}

impl RecvErrorLogState {
    // recvfrom can repeat the same UDP reset while a retired migration peer is
    // still selected. Preserve the first and periodic evidence while keeping
    // changes in either socket or error code immediately visible. A quiet
    // interval starts a new burst so old suppressed counts cannot leak into it.
    fn observe(&mut self, socket: usize, error_code: c_int, now: Instant) -> RecvErrorLogDecision {
        if let Some(active) = self.active.as_mut() {
            if active.socket == socket && active.error_code == error_code {
                if now.saturating_duration_since(active.last_observed) >= RECV_ERROR_LOG_INTERVAL {
                    active.last_reported = now;
                    active.last_observed = now;
                    active.suppressed = 0;
                    return RecvErrorLogDecision::ReportFirst;
                }
                active.last_observed = now;
                if now.saturating_duration_since(active.last_reported) < RECV_ERROR_LOG_INTERVAL {
                    active.suppressed = active.suppressed.saturating_add(1);
                    return RecvErrorLogDecision::Suppress;
                }
                let suppressed = active.suppressed;
                active.last_reported = now;
                active.suppressed = 0;
                return RecvErrorLogDecision::ReportOngoing { suppressed };
            }
        }
        self.active = Some(RecvErrorBurst {
            socket,
            error_code,
            last_reported: now,
            last_observed: now,
            suppressed: 0,
        });
        RecvErrorLogDecision::ReportFirst
    }
}

static RECV_ERROR_LOG_STATE: Mutex<RecvErrorLogState> = Mutex::new(RecvErrorLogState { active: None });

fn to_hex_stream(data: &[u8]) -> String {
    data.iter().fold(String::new(), |mut output, b| {
        use std::fmt::Write;
        let _ = write!(output, "{b:02x}");
        output
    })
}

fn deref_addr<'a, T>(addr: *const T) -> Option<&'a T> {
    if !addr.is_aligned() {
        return None;
    }
    unsafe { addr.as_ref() }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrInRaw {
    sin_family: u16,
    sin_port: [u8; 2],
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

static BOUND_SOCKETS: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();

fn bound_sockets() -> &'static Mutex<HashSet<usize>> {
    BOUND_SOCKETS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn get_bind_ip() -> Option<Ipv4Addr> {
    config::get()?.networking.ip_address
}

// Inspect only complete IPv4 socket addresses. Incomplete inputs must reach
// Winsock unchanged so a configured bind address cannot turn WSAEFAULT into success.
fn copy_socket_bytes(source: *const c_void, destination: &mut [u8]) -> bool {
    if source.is_null() { return false; }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn ReadProcessMemory(process: *mut c_void, source: *const c_void,
            destination: *mut c_void, size: usize, copied: *mut usize) -> i32;
        fn GetLastError() -> u32;
        fn SetLastError(error: u32);
    }
    let mut copied = 0;
    unsafe {
        let error = GetLastError();
        let ok = ReadProcessMemory(GetCurrentProcess(), source, destination.as_mut_ptr().cast(), destination.len(), &mut copied);
        SetLastError(error);
        ok != 0 && copied == destination.len()
    }
}

fn read_sockaddr_ipv4(addr: *const SOCKADDR, len: c_int) -> Option<SockAddrInRaw> {
    if addr.is_null() || usize::try_from(len).ok()? < std::mem::size_of::<SockAddrInRaw>() {
        return None;
    }
    // A complete OS copy is a snapshot, not ownership of caller storage.
    let mut bytes = [0u8; std::mem::size_of::<SockAddrInRaw>()];
    if !copy_socket_bytes(addr.cast(), &mut bytes) { return None; }
    // All bit patterns are valid for this integer/byte-only C layout.
    let value = unsafe { bytes.as_ptr().cast::<SockAddrInRaw>().read_unaligned() };
    (value.sin_family == AF_INET.0).then_some(value)
}

fn log_socket_payload(label: &str, source: *const c_char, len: c_int) {
    if source.is_null() || len <= 0 { return; }
    // Bound diagnostics independently of the original API's full buffer length.
    let mut bytes = [0u8; 4096];
    let count = (len as usize).min(bytes.len());
    if !copy_socket_bytes(source.cast(), &mut bytes[..count]) {
        info!("{label}: <unreadable payload; bytes={len}>");
    } else if count < len as usize {
        info!("{label}: {} [truncated to {count} of {len} bytes]", to_hex_stream(&bytes[..count]));
    } else {
        info!("{label}: {}", to_hex_stream(&bytes[..count]));
    }
}

#[cfg(test)]
fn sockaddr_ipv4(addr: *const SOCKADDR, len: c_int) -> Option<Ipv4Addr> {
    read_sockaddr_ipv4(addr, len).map(|value| Ipv4Addr::from(value.sin_addr))
}

#[cfg(test)]
fn sockaddr_port(addr: *const SOCKADDR, len: c_int) -> Option<u16> {
    read_sockaddr_ipv4(addr, len).map(|value| u16::from_be_bytes(value.sin_port))
}

#[cfg(test)]
fn sockaddr_port_bytes(addr: *const SOCKADDR, len: c_int) -> [u8; 2] {
    read_sockaddr_ipv4(addr, len).map_or([0, 0], |value| value.sin_port)
}

fn is_loopback(ip: Ipv4Addr) -> bool {
    ip.octets()[0] == 127
}

fn is_socket_marked_bound(socket: usize) -> bool {
    bound_sockets().lock().map_or(false, |sockets| sockets.contains(&socket))
}

fn mark_socket_bound(socket: usize) {
    if let Ok(mut sockets) = bound_sockets().lock() {
        sockets.insert(socket);
    }
}

fn unmark_socket_bound(socket: usize) {
    if let Ok(mut sockets) = bound_sockets().lock() {
        sockets.remove(&socket);
    }
}

fn make_bind_addr(bind_ip: Ipv4Addr, port: [u8; 2]) -> SockAddrInRaw {
    SockAddrInRaw {
        sin_family: 2,
        sin_port: port,
        sin_addr: bind_ip.octets(),
        sin_zero: [0; 8],
    }
}

fn should_rewrite_explicit_bind(original_ip: Ipv4Addr, bind_ip: Ipv4Addr) -> bool {
    original_ip != bind_ip && !is_loopback(original_ip)
}

fn packet_logging_enabled() -> bool {
    LOG_STORM_PACKETS.load(Ordering::Relaxed)
}

fn is_connect_pending_error(error: WSA_ERROR) -> bool {
    matches!(error.0, WSAEWOULDBLOCK_CODE | WSAEINPROGRESS_CODE | WSAEALREADY_CODE)
}

fn restore_wsa_last_error(error: WSA_ERROR) {
    unsafe { WSASetLastError(error.0) };
}

fn recv_error_log_decision(socket: usize, error: WSA_ERROR) -> RecvErrorLogDecision {
    RECV_ERROR_LOG_STATE
        .lock()
        .map_or(RecvErrorLogDecision::ReportFirst, |mut state| state.observe(socket, error.0, Instant::now()))
}

fn force_bind_socket(socket: usize, reason: &str, target: Option<SockAddrInRaw>) {
    let Some(bind_ip) = get_bind_ip() else {
        return;
    };
    if is_socket_marked_bound(socket) {
        return;
    }
    let Some(target) = target else { return; };
    let target_ip = Ipv4Addr::from(target.sin_addr);
    let target_port = u16::from_be_bytes(target.sin_port);
    let bind_addr = make_bind_addr(bind_ip, [0, 0]);
    let result = unsafe {
        BindSocketHook.call(
            socket,
            (&bind_addr as *const SockAddrInRaw).cast::<SOCKADDR>(),
            std::mem::size_of::<SockAddrInRaw>() as c_int,
        )
    };
    if result == 0 {
        mark_socket_bound(socket);
        info!("PublicVirtualNet socket bind [{reason}]: socket={socket}, local={bind_ip}:0, target={target_ip}:{target_port}");
    } else {
        let error = unsafe { WSAGetLastError() };
        warn!("PublicVirtualNet socket bind failed [{reason}]: socket={socket}, local={bind_ip}:0, target={target_ip}:{target_port}, wsa_error={error:?}");
        restore_wsa_last_error(error);
    }
}

// Winsock detours deliberately do not use #[instrument]. Dropping a tracing span after
// WSASetLastError can call other Windows APIs and overwrite the caller-visible error.
fn bind_socket(socket: usize, name: *const SOCKADDR, namelen: c_int) -> c_int {
    let requested = read_sockaddr_ipv4(name, namelen);
    let requested_ip = requested.map(|value| Ipv4Addr::from(value.sin_addr));
    let requested_port = requested.map_or(0, |value| u16::from_be_bytes(value.sin_port));
    let Some(bind_ip) = get_bind_ip() else {
        let result = unsafe { BindSocketHook.call(socket, name, namelen) };
        if result == 0 {
            mark_socket_bound(socket);
        }
        return result;
    };
    let Some(original_ip) = requested_ip else {
        let result = unsafe { BindSocketHook.call(socket, name, namelen) };
        if result == 0 {
            mark_socket_bound(socket);
        }
        return result;
    };
    let result = if should_rewrite_explicit_bind(original_ip, bind_ip) {
        let port = requested.expect("validated IPv4 snapshot").sin_port;
        let bind_addr = make_bind_addr(bind_ip, port);
        info!("PublicVirtualNet explicit bind rewrite: socket={socket}, requested={original_ip}:{requested_port}, effective={bind_ip}:{requested_port}");
        unsafe {
            BindSocketHook.call(
                socket,
                (&bind_addr as *const SockAddrInRaw).cast::<SOCKADDR>(),
                std::mem::size_of::<SockAddrInRaw>() as c_int,
            )
        }
    } else {
        unsafe { BindSocketHook.call(socket, name, namelen) }
    };
    if result == 0 {
        mark_socket_bound(socket);
    } else {
        let error = unsafe { WSAGetLastError() };
        warn!("PublicVirtualNet explicit bind failed: socket={socket}, requested={original_ip}:{requested_port}, configured={bind_ip}, wsa_error={error:?}");
        restore_wsa_last_error(error);
    }
    result
}

fn connect_socket(socket: usize, name: *const SOCKADDR, namelen: c_int) -> c_int {
    let target = read_sockaddr_ipv4(name, namelen);
    let target_ip = target.map(|value| Ipv4Addr::from(value.sin_addr));
    let target_port = target.map(|value| u16::from_be_bytes(value.sin_port));
    force_bind_socket(socket, "connect", target);
    let result = unsafe { ConnectHook.call(socket, name, namelen) };
    if result != 0 {
        let error = unsafe { WSAGetLastError() };
        if is_connect_pending_error(error) {
            info!("PublicVirtualNet connect pending: socket={socket}, target={target_ip:?}:{target_port:?}, wsa_error={error:?}");
        } else {
            warn!("PublicVirtualNet connect failed: socket={socket}, target={target_ip:?}:{target_port:?}, wsa_error={error:?}");
        }
        restore_wsa_last_error(error);
    } else if packet_logging_enabled() {
        info!("PublicVirtualNet connect succeeded: socket={socket}, target={target_ip:?}:{target_port:?}");
    }
    result
}

fn wsa_connect_socket(socket: usize, name: *const SOCKADDR, namelen: c_int, caller_data: *mut c_void, callee_data: *mut c_void, sqos: *mut c_void, gqos: *mut c_void) -> c_int {
    let target = read_sockaddr_ipv4(name, namelen);
    let target_ip = target.map(|value| Ipv4Addr::from(value.sin_addr));
    let target_port = target.map(|value| u16::from_be_bytes(value.sin_port));
    force_bind_socket(socket, "WSAConnect", target);
    let result = unsafe { WsaConnectHook.call(socket, name, namelen, caller_data, callee_data, sqos, gqos) };
    if result != 0 {
        let error = unsafe { WSAGetLastError() };
        if is_connect_pending_error(error) {
            info!("PublicVirtualNet WSAConnect pending: socket={socket}, target={target_ip:?}:{target_port:?}, wsa_error={error:?}");
        } else {
            warn!("PublicVirtualNet WSAConnect failed: socket={socket}, target={target_ip:?}:{target_port:?}, wsa_error={error:?}");
        }
        restore_wsa_last_error(error);
    } else if packet_logging_enabled() {
        info!("PublicVirtualNet WSAConnect succeeded: socket={socket}, target={target_ip:?}:{target_port:?}");
    }
    result
}

fn close_socket(socket: usize) -> c_int {
    let result = unsafe { CloseSocketHook.call(socket) };
    if result == 0 {
        unmark_socket_bound(socket);
    } else {
        let error = unsafe { WSAGetLastError() };
        warn!("PublicVirtualNet closesocket failed: socket={socket}, wsa_error={error:?}");
        restore_wsa_last_error(error);
    }
    result
}

fn get_storm_event_name<'a>(instance_addr: *const c_void) -> Option<&'a CStr> {
    let vtable_addr = *deref_addr(instance_addr.cast::<*const *const usize>())?;
    if vtable_addr.is_null() {
        return None;
    }
    let event_type_method_addr = *deref_addr(unsafe { vtable_addr.add(1) })?;
    if event_type_method_addr.is_null() {
        return None;
    }
    let op = unsafe { std::ptr::read_unaligned(event_type_method_addr.cast::<u16>()) };
    // 83 D3 cmp r/m32, imm8
    if op != 0x3d83 {
        return None;
    }
    let imm = unsafe { *event_type_method_addr.cast::<u8>().add(2 + std::mem::size_of::<*const u8>()) };
    if imm != 0 {
        return None;
    }
    let mut id_addr = *deref_addr(unsafe { std::ptr::read_unaligned(event_type_method_addr.cast::<u16>().add(1).cast::<*const *const c_char>()) })?;
    if id_addr.is_null() {
        let func_ptr: extern "thiscall" fn(*const c_void) -> *const *const c_char = unsafe { std::mem::transmute(event_type_method_addr) };
        id_addr = *deref_addr((func_ptr)(instance_addr))?;
    }
    unsafe { Some(CStr::from_ptr(id_addr)) }
}

unsafe fn event_probe_read_u32(base: *const c_void, offset: usize) -> Option<u32> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<u32>()))
    }
}

unsafe fn event_probe_read_u8(base: *const c_void, offset: usize) -> Option<u8> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset)))
    }
}

unsafe fn event_probe_read_ptr(base: *const c_void, offset: usize) -> *const c_void {
    if base.is_null() {
        std::ptr::null()
    } else {
        std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<*const c_void>())
    }
}

fn migration_event_category(name: &str) -> Option<&'static str> {
    if name.contains("SetMigrationMode") {
        Some("migration-mode")
    } else if name.contains("MasterRole") || name.contains("GoneMaster") || name.contains("MasterDeleted") {
        Some("object-authority")
    } else if name.contains("MementoActivation") || name.contains("RecreateReplica") {
        Some("object-recovery")
    } else if name.contains("CreateReplica") || name.contains("ReplicaCreated") || name.contains("ReplicaReady") || name.contains("DeleteReplica") {
        Some("replica-lifecycle")
    } else if name.contains("HostMigration")
        || name.contains("SessionHost")
        || name.contains("HostElected")
        || name.contains("HostTransfer")
        || name.contains("SessionWaitingRestart")
        || name.contains("SessionExitHostMigration")
    {
        Some("session-migration")
    } else if name.contains("PeerGone") || name.contains("PeerUnreachable") || name.contains("NetworkInactivityRaised") || name.contains("SessionDissolve") {
        Some("peer-loss")
    } else {
        None
    }
}

fn migration_event_probe(marker: &str, owner: *const c_void, event: *const c_void, event_name: &str) {
    let Some(category) = migration_event_category(event_name) else {
        return;
    };

    // Storm event classes do not share a common payload length. Keep this
    // generic probe pointer-only: a HostMigration event observed in the wild
    // was shorter than the previously guessed 0x30-byte layout and reading its
    // +0x2c field crashed the game. Exact payload decoding belongs only in a
    // class-specific hook whose ABI and object size have both been proven.
    info!("SCBL_MIGRATION marker={marker:?} category={category} owner={owner:?} event={event:?} event_name={event_name:?} payload_decoded=false",);
}

fn storm_event_probe(marker: &str, owner: *const c_void, event: *const c_void) {
    let event_name = get_storm_event_name(event)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("?"));
    debug!("{marker} owner={owner:?} event={event:?} event_name={event_name:?} payload_decoded=false");
    migration_event_probe(marker, owner, event, &event_name);
}

#[instrument]
fn some_event_hook(this: *mut c_void, a: *mut c_void, b: *mut c_void) -> *mut c_void {
    storm_event_probe("AsmStormDispatch1 phase=enter", this as *const c_void, b as *const c_void);
    let result = unsafe { SomeEventHook.call(this, a, b) };
    debug!("AsmStormDispatch1 phase=leave this={this:?} arg_a={a:?} event={b:?} result={result:?}");
    result
}

#[instrument(skip(this, arg1, arg2, arg3))]
fn some_event2(this: *mut c_void, arg1: *mut c_void, arg2: *mut c_void, arg3: *mut c_void, arg4: *mut c_void, arg5: *mut c_void) -> *mut c_void {
    storm_event_probe("AsmStormDispatch2 phase=enter", this as *const c_void, arg3 as *const c_void);
    debug!("AsmStormDispatch2 args this={this:?} arg1={arg1:?} arg2={arg2:?} event={arg3:?} arg4={arg4:?} arg5={arg5:?}");
    let result = unsafe { SomeEvent2Hook.call(this, arg1, arg2, arg3, arg4, arg5) };
    debug!("AsmStormDispatch2 phase=leave this={this:?} event={arg3:?} result={result:?}");
    result
}

fn sendto(s: usize, buf: *const c_char, len: c_int, flag: c_int, to: *const SOCKADDR, tolen: c_int) -> c_int {
    let target = read_sockaddr_ipv4(to, tolen);
    force_bind_socket(s, "sendto", target);
    let target_ip = target.map(|value| Ipv4Addr::from(value.sin_addr));
    let target_port = target.map(|value| u16::from_be_bytes(value.sin_port));
    if packet_logging_enabled() && target_port == Some(13000) {
        info!("PublicVirtualNet sendto: socket={s}, target={target_ip:?}:{target_port:?}, bytes={len}");
        log_socket_payload("sendto", buf, len);
    }
    let result = unsafe { SendToHook.call(s, buf, len, flag, to, tolen) };
    if result < 0 {
        let error = unsafe { WSAGetLastError() };
        warn!("PublicVirtualNet sendto failed: socket={s}, target={target_ip:?}:{target_port:?}, bytes={len}, wsa_error={error:?}");
        restore_wsa_last_error(error);
    }
    result
}

fn recvfrom(s: usize, buf: *const c_char, len: c_int, flag: c_int, from: *const SOCKADDR, fromlen: *mut c_int) -> c_int {
    let outlen = unsafe { RecvFromHook.call(s, buf, len, flag, from, fromlen) };
    if outlen < 0 {
        let error = unsafe { WSAGetLastError() };
        match recv_error_log_decision(s, error) {
            RecvErrorLogDecision::ReportFirst => {
                warn!("PublicVirtualNet recvfrom failed: socket={s}, requested_bytes={len}, wsa_error={error:?}");
            }
            RecvErrorLogDecision::ReportOngoing { suppressed } => {
                warn!(
                    "PublicVirtualNet recvfrom still failing: socket={s}, requested_bytes={len}, wsa_error={error:?}, suppressed_repeats={suppressed}, report_interval_ms={}",
                    RECV_ERROR_LOG_INTERVAL.as_millis()
                );
            }
            RecvErrorLogDecision::Suppress => {}
        }
        restore_wsa_last_error(error);
        return outlen;
    }
    let source_len = if fromlen.is_null() { 0 } else { unsafe { *fromlen } };
    let source = read_sockaddr_ipv4(from, source_len);
    let source_ip = source.map(|value| Ipv4Addr::from(value.sin_addr));
    let source_port = source.map(|value| u16::from_be_bytes(value.sin_port));
    if packet_logging_enabled() && source_port == Some(13000) && outlen > 0 {
        info!("PublicVirtualNet recvfrom: socket={s}, source={source_ip:?}:{source_port:?}, bytes={outlen}");
        log_socket_payload("recvfrom", buf, outlen.min(len));
    }
    outlen
}

#[instrument]
fn event_queue_pop(this: usize) -> *const *const *const c_void {
    let res = unsafe { EventMaybeQueuePopHook.call(this) };
    let node = if res.is_null() { std::ptr::null() } else { unsafe { *res.add(1) } };
    let event = if node.is_null() { std::ptr::null() } else { unsafe { *node } };
    debug!("AsmStormQueuePop queue=0x{this:x} result={res:?} node={node:?} event={event:?}");
    if !event.is_null() {
        storm_event_probe("AsmStormQueuePop event", this as *const c_void, event);
    }
    res
}

fn event_handler(this: *mut c_void, param_1: *mut c_void, param_2: *mut c_void, param_3: *mut c_void, param_4: *mut c_void, param_5: *mut c_void) -> usize {
    let focused_event_name = get_storm_event_name(param_3).map(|event_name| event_name.to_string_lossy().into_owned());
    if let Some(event_name) = focused_event_name.as_deref() {
        migration_event_probe("AsmStormEventHandler phase=enter", this, param_3, event_name);
        if migration_event_category(event_name).is_some() {
            info!(
                "SCBL_MIGRATION_ROUTER phase=enter event_name={event_name:?} dispatcher={this:?} param1={param_1:?} param2={param_2:?} event={param_3:?} param4={param_4:?} param5={param_5:?}"
            );
        }
    }
    if LOG_STORM_EVENTS.load(Ordering::Relaxed) {
        storm_event_probe("AsmStormEventHandler phase=enter", this as *const c_void, param_3 as *const c_void);
        debug!("AsmStormEventHandler args this={this:?} param1={param_1:?} param2={param_2:?} event={param_3:?} param4={param_4:?} param5={param_5:?}");
    }
    let result = unsafe { EventHandlerHook.call(this, param_1, param_2, param_3, param_4, param_5) };
    if let Some(event_name) = focused_event_name.as_deref() {
        if migration_event_category(event_name).is_some() {
            info!("SCBL_MIGRATION_ROUTER phase=leave event_name={event_name:?} dispatcher={this:?} event={param_3:?} result=0x{result:x}");
        }
    }
    if LOG_STORM_EVENTS.load(Ordering::Relaxed) {
        debug!("AsmStormEventHandler phase=leave this={this:?} event={param_3:?} result=0x{result:x}");
    }
    result
}

unsafe fn session_leave_map_entry(this: *const c_void, generation: u32) -> Option<(*const c_void, *const c_void)> {
    if this.is_null() {
        return None;
    }
    let sentinel = this.cast::<u8>().add(0x30);
    let mut node = event_probe_read_ptr(this, 0x38).cast::<u8>();
    let mut candidate = std::ptr::null();
    let mut visited = std::collections::HashSet::new();
    while !node.is_null() && node != sentinel {
        // Observational lookup must not trap native routing on a repeated link.
        if !visited.insert(node as usize) { return None; }
        let key = event_probe_read_u32(node.cast(), 0x10)?;
        if key < generation {
            node = event_probe_read_ptr(node.cast(), 0x04).cast::<u8>();
        } else {
            candidate = node;
            node = event_probe_read_ptr(node.cast(), 0x00).cast::<u8>();
        }
    }
    if candidate.is_null() || candidate == sentinel || event_probe_read_u32(candidate.cast(), 0x10)? != generation {
        return None;
    }
    Some((candidate.cast(), event_probe_read_ptr(candidate.cast(), 0x14)))
}

fn session_leave_map_lookup(this: *mut c_void, result_slot: *mut c_void, event: *mut c_void) -> *mut c_void {
    let generation = unsafe { event_probe_read_u32(event, 0x18) };
    let entry = generation.and_then(|value| unsafe { session_leave_map_entry(this, value) });
    info!(
        "SCBL_MIGRATION_CONTEXT phase=session-leave-lookup generation={generation:?} coordinator={this:?} root={:?} entry={:?} owner={:?} behavior_changed=false",
        unsafe { event_probe_read_ptr(this, 0x38) },
        entry.map(|value| value.0),
        entry.map(|value| value.1),
    );
    let result = unsafe { SessionLeaveMapLookupHook.call(this, result_slot, event) };
    info!("SCBL_MIGRATION_CONTEXT phase=session-leave-result generation={generation:?} result={result:?} behavior_changed=false");
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostMigrationUpdateSnapshot {
    flags_04: Option<u8>,
    flags_05: Option<u8>,
    flags_06: Option<u8>,
    generation: Option<u32>,
    local_participant: Option<u32>,
    migration_host: Option<u32>,
    retry_count: Option<u32>,
    retry_deadline: Option<u32>,
    terminal_state: Option<u32>,
}

static HOST_MIGRATION_UPDATE_SNAPSHOTS: Mutex<Vec<(usize, usize, HostMigrationUpdateSnapshot, Instant)>> = Mutex::new(Vec::new());

unsafe fn migration_participant_acknowledgements(this: *const c_void) -> Vec<(u32, u32, u8)> {
    let mut result = Vec::new();
    if this.is_null() {
        return result;
    }
    let sentinel = this.cast::<u8>().add(0x78);
    let root = event_probe_read_ptr(sentinel.cast(), 0x08).cast::<u8>();
    let mut pending = vec![root];
    let mut visited = Vec::new();
    while let Some(node) = pending.pop() {
        if node.is_null() || node == sentinel || visited.contains(&(node as usize)) || result.len() >= 12 {
            continue;
        }
        visited.push(node as usize);
        pending.push(event_probe_read_ptr(node.cast(), 0x00).cast::<u8>());
        pending.push(event_probe_read_ptr(node.cast(), 0x04).cast::<u8>());
        let participant = event_probe_read_ptr(node.cast(), 0x14);
        if !participant.is_null() {
            result.push((
                event_probe_read_u32(participant, 0xbc).unwrap_or(u32::MAX),
                event_probe_read_u32(participant, 0xb4).unwrap_or(u32::MAX),
                event_probe_read_u8(participant, 0xc1).unwrap_or(0),
            ));
        }
    }
    result.sort_unstable_by_key(|entry| entry.0);
    result
}

unsafe fn migration_participant_by_id(this: *const c_void, participant_id: u32) -> Option<*mut c_void> {
    if this.is_null() {
        return None;
    }
    let sentinel = this.cast::<u8>().add(0x78);
    let root = event_probe_read_ptr(sentinel.cast(), 0x08).cast::<u8>();
    let mut pending = vec![root];
    let mut visited = Vec::new();
    while let Some(node) = pending.pop() {
        if node.is_null() || node == sentinel || visited.contains(&(node as usize)) || visited.len() >= 12 {
            continue;
        }
        visited.push(node as usize);
        pending.push(event_probe_read_ptr(node.cast(), 0x00).cast::<u8>());
        pending.push(event_probe_read_ptr(node.cast(), 0x04).cast::<u8>());
        let participant = event_probe_read_ptr(node.cast(), 0x14);
        if !participant.is_null() && event_probe_read_u32(participant, 0xbc) == Some(participant_id) {
            return Some(participant.cast_mut());
        }
    }
    None
}

fn should_bridge_nonhost_restart(snapshot: &HostMigrationUpdateSnapshot) -> bool {
    let (Some(flags_04), Some(flags_05), Some(flags_06), Some(generation), Some(local), Some(host), Some(terminal_state)) = (
        snapshot.flags_04,
        snapshot.flags_05,
        snapshot.flags_06,
        snapshot.generation,
        snapshot.local_participant,
        snapshot.migration_host,
        snapshot.terminal_state,
    ) else {
        return false;
    };
    generation > 0 && terminal_state == 2 && flags_04 & 0x42 == 0x42 && flags_05 & 0x20 == 0 && flags_06 & 0x01 == 0 && local != u32::MAX && host != u32::MAX && local != host
}

unsafe fn host_migration_snapshot(this: *const c_void) -> HostMigrationUpdateSnapshot {
    HostMigrationUpdateSnapshot {
        flags_04: event_probe_read_u8(this, 0x04),
        flags_05: event_probe_read_u8(this, 0x05),
        flags_06: event_probe_read_u8(this, 0x06),
        generation: event_probe_read_u32(this, 0x34),
        local_participant: event_probe_read_u32(this, 0x3c),
        migration_host: event_probe_read_u32(this, 0x1b0),
        retry_count: event_probe_read_u32(this, 0x278),
        retry_deadline: event_probe_read_u32(this, 0x27c),
        terminal_state: event_probe_read_u32(this, 0x284),
    }
}

unsafe fn continue_missing_nonhost_restart(this: *mut c_void, session: *mut c_void, snapshot: &HostMigrationUpdateSnapshot) -> bool {
    if !should_bridge_nonhost_restart(snapshot) || session.is_null() {
        return false;
    }
    let Some(host_id) = snapshot.migration_host else {
        return false;
    };
    let Some(host_participant) = migration_participant_by_id(this, host_id) else {
        warn!("SCBL_MIGRATION_NONHOST_CONTINUE phase=fail-closed owner={this:?} session={session:?} host_id={host_id} reason=host-participant-not-found behavior_changed=false");
        return false;
    };
    let host_state = event_probe_read_u32(host_participant, 0xb4);
    let host_ack = event_probe_read_u8(host_participant, 0xc1);
    if host_state != Some(3) || !host_ack.is_some_and(|value| value & 0x02 != 0) {
        warn!("SCBL_MIGRATION_NONHOST_CONTINUE phase=fail-closed owner={this:?} session={session:?} host_id={host_id} host_state={host_state:?} host_ack={host_ack:?} reason=host-participant-not-ready behavior_changed=false");
        return false;
    }

    let Some(flags_05) = event_probe_read_u8(this, 0x05) else {
        return false;
    };
    this.cast::<u8>().add(0x05).write_volatile(flags_05 | 0x20);
    let mut result_slot = 0usize;
    let continue_participant: NativeContinueHostMigrationParticipant = std::mem::transmute(NATIVE_CONTINUE_HOST_MIGRATION_PARTICIPANT_ADDR);
    continue_participant(this, (&mut result_slot as *mut usize).cast(), session, host_participant);
    info!(
        "SCBL_MIGRATION_NONHOST_CONTINUE phase=native-complete owner={this:?} session={session:?} host_id={host_id} host_participant={host_participant:?} result={result_slot} snapshot={:?} participants={:?} behavior_changed=true",
        host_migration_snapshot(this),
        migration_participant_acknowledgements(this),
    );
    true
}

fn unique_owner(mut candidates: impl Iterator<Item = usize>) -> Option<usize> {
    let owner = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    Some(owner)
}

fn resolve_pending_nonhost_owner(generation: u32) -> Option<usize> {
    let snapshots = HOST_MIGRATION_UPDATE_SNAPSHOTS.lock().ok()?;
    let now = Instant::now();
    let candidates = snapshots
        .iter()
        .filter(|(_, _, snapshot, observed_at)| {
            now.saturating_duration_since(*observed_at) <= Duration::from_secs(5) && snapshot.generation == Some(generation) && should_bridge_nonhost_restart(snapshot)
        })
        .map(|(owner, _, _, _)| *owner);
    unique_owner(candidates)
}

fn host_elect_acknowledge_router(this: *mut c_void, result_slot: *mut c_void, event: *mut c_void) -> *mut c_void {
    let generation = unsafe { event_probe_read_u32(event, 0x18) };
    let native_owner = generation.and_then(|value| unsafe { session_leave_map_entry(this, value) }).map(|entry| entry.1);
    if native_owner.is_some_and(|owner| !owner.is_null()) {
        let owner = native_owner.unwrap();
        let event_words = unsafe {
            [
                event_probe_read_u32(event, 0x08),
                event_probe_read_u32(event, 0x0c),
                event_probe_read_u32(event, 0x10),
                event_probe_read_u32(event, 0x14),
                event_probe_read_u32(event, 0x18),
                event_probe_read_u32(event, 0x1c),
                event_probe_read_u32(event, 0x20),
            ]
        };
        let participants_before = unsafe { migration_participant_acknowledgements(owner) };
        info!(
            "SCBL_MIGRATION_HOST_ELECT_ACK_ROUTE phase=native-enter generation={generation:?} coordinator={this:?} owner={owner:?} event_words_08_20={event_words:?} participants={participants_before:?} behavior_changed=false"
        );
        let result = unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
        info!(
            "SCBL_MIGRATION_HOST_ELECT_ACK_ROUTE phase=native-leave generation={generation:?} coordinator={this:?} owner={owner:?} participants={:?} behavior_changed=false",
            unsafe { migration_participant_acknowledgements(owner) }
        );
        return result;
    }

    let Some(generation) = generation else {
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    };
    let Some(owner) = resolve_pending_nonhost_owner(generation) else {
        warn!("SCBL_MIGRATION_HOST_ELECT_ACK_ROUTE phase=fail-closed generation={generation} coordinator={this:?} reason=no-unique-live-owner behavior_changed=false");
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    };

    let coordinator_context = unsafe { event_probe_read_ptr(this, 0x0c) };
    let session_context = unsafe { event_probe_read_ptr(coordinator_context, 0x14) } as usize;
    let Some(event_08) = (unsafe { event_probe_read_u32(event, 0x08) }) else {
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    };
    let Some(event_0c) = (unsafe { event_probe_read_u32(event, 0x0c) }) else {
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    };
    let Some(event_10) = (unsafe { event_probe_read_u32(event, 0x10) }) else {
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    };
    let Some(event_1c) = (unsafe { event_probe_read_u32(event, 0x1c) }) else {
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    };
    if session_context == 0 {
        warn!("SCBL_MIGRATION_HOST_ELECT_ACK_ROUTE phase=fail-closed generation={generation} coordinator={this:?} reason=null-session-context behavior_changed=false");
        return unsafe { HostElectAcknowledgeRouterHook.call(this, result_slot, event) };
    }

    info!(
        "SCBL_MIGRATION_HOST_ELECT_ACK_ROUTE phase=bridge generation={generation} coordinator={this:?} owner={:?} session_context=0x{session_context:x} behavior_changed=true",
        owner as *mut c_void
    );
    let handler: NativeHostElectAcknowledgeHandler = unsafe { std::mem::transmute(NATIVE_HOST_ELECT_ACKNOWLEDGE_HANDLER_ADDR) };
    unsafe {
        handler(
            owner as *mut c_void,
            result_slot,
            session_context,
            event_1c as usize,
            event_08 as usize,
            event_0c as usize,
            event_10 as usize,
        )
    }
}

fn session_restart_producer(result_slot: *mut c_void, generation: u32, target: u32, arg_14: u32, arg_18: u32) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_RESTART_PRODUCER phase=enter result_slot={result_slot:?} generation={generation} target=0x{target:08x} arg_14=0x{arg_14:08x} arg_18=0x{arg_18:08x} behavior_changed=false"
    );
    let result = unsafe { SessionRestartProducerHook.call(result_slot, generation, target, arg_14, arg_18) };
    info!("SCBL_MIGRATION_RESTART_PRODUCER phase=leave result={result:?} generation={generation} behavior_changed=false");
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostMigrationGameStateSnapshot {
    workflow: Option<usize>,
    phase: Option<u32>,
    generation: Option<u32>,
    local_participant: Option<u32>,
    role: Option<u32>,
    pending_operation: Option<usize>,
    workflow_flags: [Option<u8>; 3],
    workflow_participants: [Option<u32>; 4],
    platform_interface: Option<usize>,
    platform_session_key: Option<usize>,
    platform_session_words_04_0c: [Option<u32>; 3],
    platform_value_418: Option<u32>,
    platform_flags_428_42c: [Option<u8>; 5],
    platform_record_words_00_08: [Option<u32>; 3],
}

static HOST_MIGRATION_GAME_STATE_SNAPSHOTS: Mutex<Vec<(usize, HostMigrationGameStateSnapshot)>> = Mutex::new(Vec::new());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostMigrationPhase5Snapshot {
    new_host_participant: u32,
    local_participant: u32,
    participant_present: u8,
    identity_object: Option<usize>,
    identity_ready: Option<u8>,
    participant_platform_id: Option<u64>,
    identity_words_00_2c: [Option<u32>; 12],
    platform_index: u32,
    platform_interface: Option<usize>,
    platform_record: Option<usize>,
    platform_record_valid: Option<u8>,
    platform_record_host_id: Option<u32>,
    platform_record_route: Option<u32>,
    platform_ready_fn: Option<usize>,
    platform_host_fn: Option<usize>,
    platform_ready: Option<u8>,
    platform_host_id: Option<u32>,
    identities_match: Option<bool>,
}

static HOST_MIGRATION_PHASE5_SNAPSHOTS: Mutex<Vec<(usize, HostMigrationPhase5Snapshot)>> = Mutex::new(Vec::new());

unsafe fn host_migration_game_state_snapshot(this: *const c_void) -> HostMigrationGameStateSnapshot {
    let workflow = event_probe_read_ptr(this, 0x610);
    let platform_index = event_probe_read_u32(this, 0x5f0).unwrap_or(u32::MAX);
    let platform_table = event_probe_read_ptr(NATIVE_PLATFORM_TABLE_ADDR as *const c_void, 0);
    let platform_interface = if platform_index == u32::MAX {
        std::ptr::null()
    } else {
        event_probe_read_ptr(platform_table, 0x24 + platform_index as usize * 4)
    };
    let platform_session_key = event_probe_read_ptr(platform_interface, 0x40c);
    let platform_record = event_probe_read_ptr(platform_interface, 0x430);
    HostMigrationGameStateSnapshot {
        workflow: (!workflow.is_null()).then_some(workflow as usize),
        phase: event_probe_read_u32(workflow, 0x04),
        generation: event_probe_read_u32(this, 0x550),
        local_participant: event_probe_read_u32(this, 0x55c),
        role: event_probe_read_u32(this, 0x5f0),
        pending_operation: {
            let value = event_probe_read_ptr(this, 0x618);
            (!value.is_null()).then_some(value as usize)
        },
        workflow_flags: [
            event_probe_read_u8(workflow, 0x28),
            event_probe_read_u8(workflow, 0x29),
            event_probe_read_u8(workflow, 0x2c),
        ],
        workflow_participants: [
            event_probe_read_u32(workflow, 0x20),
            event_probe_read_u32(workflow, 0x24),
            event_probe_read_u32(workflow, 0x30),
            event_probe_read_u32(workflow, 0x34),
        ],
        platform_interface: (!platform_interface.is_null()).then_some(platform_interface as usize),
        platform_session_key: (!platform_session_key.is_null()).then_some(platform_session_key as usize),
        platform_session_words_04_0c: [
            event_probe_read_u32(platform_session_key, 0x04),
            event_probe_read_u32(platform_session_key, 0x08),
            event_probe_read_u32(platform_session_key, 0x0c),
        ],
        platform_value_418: event_probe_read_u32(platform_interface, 0x418),
        platform_flags_428_42c: [
            event_probe_read_u8(platform_interface, 0x428),
            event_probe_read_u8(platform_interface, 0x429),
            event_probe_read_u8(platform_interface, 0x42a),
            event_probe_read_u8(platform_interface, 0x42b),
            event_probe_read_u8(platform_interface, 0x42c),
        ],
        platform_record_words_00_08: [
            event_probe_read_u32(platform_record, 0x00),
            event_probe_read_u32(platform_record, 0x04),
            event_probe_read_u32(platform_record, 0x08),
        ],
    }
}

unsafe fn host_migration_phase5_snapshot(this: *mut c_void, state: &HostMigrationGameStateSnapshot) -> Option<HostMigrationPhase5Snapshot> {
    if state.phase != Some(5) {
        return None;
    }
    let new_host_participant = state.workflow_participants[0]?;
    let local_participant = state.local_participant?;
    if new_host_participant == local_participant || new_host_participant == u32::MAX {
        return None;
    }

    let participant_present_fn: NativeMigrationParticipantPresent = std::mem::transmute(NATIVE_MIGRATION_PARTICIPANT_PRESENT_ADDR);
    let participant_identity_fn: NativeMigrationParticipantIdentity = std::mem::transmute(NATIVE_MIGRATION_PARTICIPANT_IDENTITY_ADDR);
    let participant_present = participant_present_fn(this, new_host_participant);
    let identity_object = participant_identity_fn(this, new_host_participant);
    let identity_ready = event_probe_read_u8(identity_object, 0x0a);
    let participant_platform_id = match (event_probe_read_u32(identity_object, 0x1c), event_probe_read_u32(identity_object, 0x20)) {
        (Some(low), Some(high)) => Some((u64::from(high) << 32) | u64::from(low)),
        _ => None,
    };
    let identity_words_00_2c = [
        event_probe_read_u32(identity_object, 0x00),
        event_probe_read_u32(identity_object, 0x04),
        event_probe_read_u32(identity_object, 0x08),
        event_probe_read_u32(identity_object, 0x0c),
        event_probe_read_u32(identity_object, 0x10),
        event_probe_read_u32(identity_object, 0x14),
        event_probe_read_u32(identity_object, 0x18),
        event_probe_read_u32(identity_object, 0x1c),
        event_probe_read_u32(identity_object, 0x20),
        event_probe_read_u32(identity_object, 0x24),
        event_probe_read_u32(identity_object, 0x28),
        event_probe_read_u32(identity_object, 0x2c),
    ];

    let platform_index = state.role?;
    let platform_table = event_probe_read_ptr(NATIVE_PLATFORM_TABLE_ADDR as *const c_void, 0);
    let platform_interface = event_probe_read_ptr(platform_table, 0x24 + platform_index as usize * 4);
    let platform_vtable = event_probe_read_ptr(platform_interface, 0);
    let platform_record = event_probe_read_ptr(platform_interface, 0x430);
    let platform_ready_fn = event_probe_read_ptr(platform_vtable, 0x60);
    let platform_host_fn = event_probe_read_ptr(platform_vtable, 0x64);
    let platform_ready = if platform_ready_fn.is_null() || platform_interface.is_null() {
        None
    } else {
        let function: NativeMigrationPlatformReady = std::mem::transmute(platform_ready_fn);
        Some(function(platform_interface.cast_mut()))
    };
    let platform_host_id = if platform_ready == Some(0) || platform_host_fn.is_null() || platform_interface.is_null() {
        None
    } else {
        let function: NativeMigrationPlatformHost = std::mem::transmute(platform_host_fn);
        Some(function(platform_interface.cast_mut()))
    };
    let identities_match = match (participant_platform_id, platform_host_id) {
        (Some(participant_id), Some(platform_id)) => Some(participant_id == u64::from(platform_id)),
        _ => None,
    };

    Some(HostMigrationPhase5Snapshot {
        new_host_participant,
        local_participant,
        participant_present,
        identity_object: (!identity_object.is_null()).then_some(identity_object as usize),
        identity_ready,
        participant_platform_id,
        identity_words_00_2c,
        platform_index,
        platform_interface: (!platform_interface.is_null()).then_some(platform_interface as usize),
        platform_record: (!platform_record.is_null()).then_some(platform_record as usize),
        platform_record_valid: event_probe_read_u8(platform_record, 0x00),
        platform_record_host_id: event_probe_read_u32(platform_record, 0x04),
        platform_record_route: event_probe_read_u32(platform_record, 0x08),
        platform_ready_fn: (!platform_ready_fn.is_null()).then_some(platform_ready_fn as usize),
        platform_host_fn: (!platform_host_fn.is_null()).then_some(platform_host_fn as usize),
        platform_ready,
        platform_host_id,
        identities_match,
    })
}

// These two caches suppress duplicate diagnostics only; never evict the
// functional pending-owner records through this size policy.
fn record_migration_diagnostic<T: Copy + PartialEq>(snapshots: &Mutex<Vec<(usize, T)>>, owner: usize, snapshot: T) -> bool {
    let Ok(mut snapshots) = snapshots.lock() else { return true; };
    if let Some((_, previous)) = snapshots.iter_mut().find(|(key, _)| *key == owner) {
        if *previous == snapshot { return false; }
        *previous = snapshot;
    } else {
        if snapshots.len() >= 256 { snapshots.remove(0); }
        snapshots.push((owner, snapshot));
    }
    true
}

fn log_host_migration_phase5_snapshot(this: *mut c_void, snapshot: HostMigrationPhase5Snapshot) {
    if record_migration_diagnostic(&HOST_MIGRATION_PHASE5_SNAPSHOTS, this as usize, snapshot) {
        info!("SCBL_MIGRATION_PHASE5_GATE owner={this:?} snapshot={snapshot:?} behavior_changed=false");
    }
}

fn missing_platform_host_record_repair(state: &HostMigrationGameStateSnapshot, phase5: &HostMigrationPhase5Snapshot) -> Option<(usize, u32, u32)> {
    if state.phase != Some(5)
        || state.pending_operation.is_some()
        || phase5.new_host_participant == phase5.local_participant
        || phase5.participant_present != 1
        || phase5.identity_ready != Some(1)
        || phase5.platform_record_valid != Some(0)
        || phase5.platform_record_host_id != Some(0)
        || phase5.platform_record_route != Some(0)
        || state.platform_session_words_04_0c[0] != Some(1)
        || state.platform_flags_428_42c[0] != Some(1)
        || state.platform_flags_428_42c[1] != Some(1)
        || state.platform_flags_428_42c[4] != Some(0)
    {
        return None;
    }

    let platform_id = phase5.participant_platform_id?;
    if platform_id == 0 || platform_id > u64::from(u32::MAX) {
        return None;
    }
    let route = state.platform_session_words_04_0c[1]?;
    if route == 0 || route == u32::MAX {
        return None;
    }
    Some((phase5.platform_record?, platform_id as u32, route))
}

unsafe fn repair_missing_platform_host_record(this: *mut c_void, state: &HostMigrationGameStateSnapshot, phase5: &HostMigrationPhase5Snapshot) {
    let Some((record, host_id, route)) = missing_platform_host_record_repair(state, phase5) else {
        return;
    };
    let record = record as *mut u8;
    record.add(0x04).cast::<u32>().write_unaligned(host_id);
    record.add(0x08).cast::<u32>().write_unaligned(route);
    record.write_volatile(1);
    info!(
        "SCBL_MIGRATION_PLATFORM_HOST_RECORD_REPAIR owner={this:?} record={record:?} new_host_participant={} host_id={host_id} route={route} phase=5 behavior_changed=true",
        phase5.new_host_participant,
    );
}

fn host_migration_game_state_update(this: *mut c_void) {
    unsafe { HostMigrationGameStateUpdateHook.call(this) };
    let snapshot = unsafe { host_migration_game_state_snapshot(this) };
    if let Some(phase5_snapshot) = unsafe { host_migration_phase5_snapshot(this, &snapshot) } {
        log_host_migration_phase5_snapshot(this, phase5_snapshot);
        unsafe { repair_missing_platform_host_record(this, &snapshot, &phase5_snapshot) };
    }
    if !record_migration_diagnostic(&HOST_MIGRATION_GAME_STATE_SNAPSHOTS, this as usize, snapshot) {
        return;
    }
    info!("SCBL_MIGRATION_GAME_STATE owner={this:?} snapshot={snapshot:?} behavior_changed=false");
}

fn host_migration_update(this: *mut c_void, context: *mut c_void) {
    let snapshot = unsafe { host_migration_snapshot(this) };
    let now = Instant::now();
    let changed = if let Ok(mut snapshots) = HOST_MIGRATION_UPDATE_SNAPSHOTS.lock() {
        // resolve_pending_nonhost_owner already excludes these records. Retire
        // them rather than retaining stale object addresses for the DLL lifetime.
        snapshots.retain(|(_, _, _, observed_at)| now.saturating_duration_since(*observed_at) <= Duration::from_secs(5));
        if let Some((_, previous_context, previous, observed_at)) = snapshots.iter_mut().find(|(owner, _, _, _)| *owner == this as usize) {
            let changed = *previous != snapshot;
            *previous = snapshot;
            *previous_context = context as usize;
            *observed_at = now;
            changed
        } else {
            snapshots.push((this as usize, context as usize, snapshot, now));
            true
        }
    } else { false };
    if changed {
        info!("SCBL_MIGRATION_UPDATE owner={this:?} context={context:?} snapshot={snapshot:?} behavior_changed=false");
    }
    unsafe { HostMigrationUpdateHook.call(this, context) };
    let post_snapshot = unsafe { host_migration_snapshot(this) };
    unsafe { continue_missing_nonhost_restart(this, context, &post_snapshot) };
}

fn session_restart_in_event_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, context: *mut c_void, arg_14: usize, arg_18: usize) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_RESTART_IN_EVENT phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} context={context:?} arg_14=0x{arg_14:x} arg_18=0x{arg_18:x} flags_04={:?} flags_05={:?} flags_06={:?} generation={:?} local_participant={:?} migration_host={:?} participants={:?} behavior_changed=false",
        unsafe { event_probe_read_u8(this, 0x04) },
        unsafe { event_probe_read_u8(this, 0x05) },
        unsafe { event_probe_read_u8(this, 0x06) },
        unsafe { event_probe_read_u32(this, 0x34) },
        unsafe { event_probe_read_u32(this, 0x3c) },
        unsafe { event_probe_read_u32(this, 0x1b0) },
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionRestartInEventHandlerHook.call(this, result_slot, session, context, arg_14, arg_18) };
    info!(
        "SCBL_MIGRATION_RESTART_IN_EVENT phase=leave owner={this:?} result={result:?} flags_06={:?} participants={:?} behavior_changed=false",
        unsafe { event_probe_read_u8(this, 0x06) },
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_peer_connected_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_TOPOLOGY kind=peer-connected phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionPeerConnectedHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_TOPOLOGY kind=peer-connected phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_local_topology_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_TOPOLOGY kind=local-topology phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionLocalTopologyHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_TOPOLOGY kind=local-topology phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_peer_joining_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_TOPOLOGY kind=peer-joining phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionPeerJoiningHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_TOPOLOGY kind=peer-joining phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_peer_gone_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_PEER_RETIRE kind=gone phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionPeerGoneHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_PEER_RETIRE kind=gone phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_peer_unreachable_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_PEER_RETIRE kind=unreachable phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionPeerUnreachableHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_PEER_RETIRE kind=unreachable phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_exit_host_migration_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_ACK kind=exit-host-migration phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionExitHostMigrationHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_ACK kind=exit-host-migration phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn session_waiting_restart_handler(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, participant: *mut c_void, event: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_ACK kind=waiting-restart phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} participant={participant:?} event={event:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    let result = unsafe { SessionWaitingRestartHandlerHook.call(this, result_slot, session, participant, event) };
    info!(
        "SCBL_MIGRATION_ACK kind=waiting-restart phase=leave owner={this:?} result={result:?} participants={:?} behavior_changed=false",
        unsafe { migration_participant_acknowledgements(this) },
    );
    result
}

fn host_migration_end_producer_a(this: *mut c_void, result_slot: *mut c_void, session: *mut c_void, context: *mut c_void) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_END_PRODUCER producer=A phase=enter owner={this:?} result_slot={result_slot:?} session={session:?} context={context:?} owner_word_20={:?} owner_word_24={:?}",
        unsafe { event_probe_read_u32(this, 0x20) },
        unsafe { event_probe_read_u32(this, 0x24) },
    );
    let result = unsafe { HostMigrationEndProducerAHook.call(this, result_slot, session, context) };
    info!("SCBL_MIGRATION_END_PRODUCER producer=A phase=leave owner={this:?} result={result:?}");
    result
}

#[allow(clippy::too_many_arguments)]
fn host_migration_end_producer_b(
    this: *mut c_void,
    result_slot: *mut c_void,
    arg_c: usize,
    arg_10: usize,
    arg_14: usize,
    arg_18: usize,
    arg_1c: usize,
    arg_20: usize,
) -> *mut c_void {
    info!(
        "SCBL_MIGRATION_END_PRODUCER producer=B phase=enter owner={this:?} result_slot={result_slot:?} flags_04={:?} flags_05={:?} flags_06={:?} owner_word_08={:?} args=[0x{arg_c:x},0x{arg_10:x},0x{arg_14:x},0x{arg_18:x},0x{arg_1c:x},0x{arg_20:x}]",
        unsafe { event_probe_read_u8(this, 0x04) },
        unsafe { event_probe_read_u8(this, 0x05) },
        unsafe { event_probe_read_u8(this, 0x06) },
        unsafe { event_probe_read_u32(this, 0x08) },
    );
    let result = unsafe { HostMigrationEndProducerBHook.call(this, result_slot, arg_c, arg_10, arg_14, arg_18, arg_1c, arg_20) };
    info!("SCBL_MIGRATION_END_PRODUCER producer=B phase=leave owner={this:?} result={result:?}");
    result
}

pub unsafe fn init_hooks(config: &Config, addr: &Addresses) {
    LOG_STORM_PACKETS.store(config.enable_all_hooks || config.enable_hooks.contains(&Hook::StormPackets), Ordering::Relaxed);
    let log_storm_events = config.enable_all_hooks || config.enable_hooks.contains(&Hook::StormEventDispatcher);
    LOG_STORM_EVENTS.store(log_storm_events, Ordering::Relaxed);
    if log_storm_events {
        info!("StormEventDispatcher diagnostics enabled: focused_migration_level=info verbose_event_level=debug");
    }
    super::configurable_hook!(config, Hook::StormEventDispatcher, SomeEventHook; addr.func_storm_event_dispatch => some_event_hook);
    super::configurable_hook!(config, Hook::StormEventDispatcher, SomeEvent2Hook; addr.func_storm_event_dispatch2 => some_event2);
    super::configurable_hook!(config, Hook::StormEventDispatcher, EventMaybeQueuePopHook; addr.func_storm_event_maybe_queue_pop => event_queue_pop);
    // Keep the native event handler installed without changing its behavior.
    // Full migration tracing remains explicitly opt-in diagnostics.
    super::hook!(EventHandlerHook, addr.func_storm_event_handler, event_handler);
    // Retail DX11 only: trace the exact generation-keyed lookup that gates the
    // native HostMigrationEnd producer. The detour is observational.
    let session_leave_map_lookup_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_C250);
    super::hook!(SessionLeaveMapLookupHook, session_leave_map_lookup_addr, session_leave_map_lookup);
    let host_migration_end_producer_a_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x0209_22B0);
    super::hook!(HostMigrationEndProducerAHook, host_migration_end_producer_a_addr, host_migration_end_producer_a);
    let host_migration_end_producer_b_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_0150);
    super::hook!(HostMigrationEndProducerBHook, host_migration_end_producer_b_addr, host_migration_end_producer_b);
    let host_migration_update_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_B240);
    super::hook!(HostMigrationUpdateHook, host_migration_update_addr, host_migration_update);
    let host_migration_game_state_update_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x0089_E650);
    super::hook!(HostMigrationGameStateUpdateHook, host_migration_game_state_update_addr, host_migration_game_state_update);
    let session_restart_in_event_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_1200);
    super::hook!(SessionRestartInEventHandlerHook, session_restart_in_event_handler_addr, session_restart_in_event_handler);
    let session_restart_producer_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x01F9_F8E0);
    super::hook!(SessionRestartProducerHook, session_restart_producer_addr, session_restart_producer);
    let host_elect_acknowledge_router_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020B_4040);
    super::hook!(HostElectAcknowledgeRouterHook, host_elect_acknowledge_router_addr, host_elect_acknowledge_router);
    let session_peer_connected_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x0207_F990);
    super::hook!(SessionPeerConnectedHandlerHook, session_peer_connected_handler_addr, session_peer_connected_handler);
    let session_local_topology_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_EA40);
    super::hook!(SessionLocalTopologyHandlerHook, session_local_topology_handler_addr, session_local_topology_handler);
    let session_peer_joining_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_EB10);
    super::hook!(SessionPeerJoiningHandlerHook, session_peer_joining_handler_addr, session_peer_joining_handler);
    let session_peer_gone_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_E780);
    super::hook!(SessionPeerGoneHandlerHook, session_peer_gone_handler_addr, session_peer_gone_handler);
    let session_peer_unreachable_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_1500);
    super::hook!(SessionPeerUnreachableHandlerHook, session_peer_unreachable_handler_addr, session_peer_unreachable_handler);
    let session_exit_host_migration_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_16F0);
    super::hook!(
        SessionExitHostMigrationHandlerHook,
        session_exit_host_migration_handler_addr,
        session_exit_host_migration_handler
    );
    let session_waiting_restart_handler_addr = (addr.func_storm_event_handler == Some(0x020F_1D40)).then_some(0x020A_1820);
    super::hook!(SessionWaitingRestartHandlerHook, session_waiting_restart_handler_addr, session_waiting_restart_handler);
    if let Ok(lib) = LoadLibraryA(s!("ws2_32.dll")) {
        let bind_addr = GetProcAddress(lib, s!("bind"));
        let connect_addr = GetProcAddress(lib, s!("connect"));
        let wsa_connect_addr = GetProcAddress(lib, s!("WSAConnect"));
        let close_socket_addr = GetProcAddress(lib, s!("closesocket"));
        let sendto_addr = GetProcAddress(lib, s!("sendto"));
        let recvfrom_addr = GetProcAddress(lib, s!("recvfrom"));

        if config.networking.ip_address.is_some() {
            info!("PublicVirtualNet enabling mandatory Winsock binding hooks for configured Networking.IpAddress");
            super::hook!(BindSocketHook, bind_addr, bind_socket);
            super::hook!(ConnectHook, connect_addr, connect_socket);
            super::hook!(WsaConnectHook, wsa_connect_addr, wsa_connect_socket);
            super::hook!(CloseSocketHook, close_socket_addr, close_socket);
            super::hook!(SendToHook, sendto_addr, sendto);
            super::hook!(RecvFromHook, recvfrom_addr, recvfrom);
        } else {
            if let Some(sendto_addr) = sendto_addr {
                super::configurable_hook!(config, Hook::StormPackets, SendToHook; Some(sendto_addr.as_ptr()) => sendto);
            }
            if let Some(recvfrom_addr) = recvfrom_addr {
                super::configurable_hook!(config, Hook::StormPackets, RecvFromHook; Some(recvfrom_addr.as_ptr()) => recvfrom);
            }
        }
        let _ = FreeLibrary(lib);
    }
}

pub unsafe fn deinit_hooks(config: &Config) {
    super::disable_configurable_hook!(config, Hook::StormEventDispatcher, SomeEventHook);
    super::disable_configurable_hook!(config, Hook::StormEventDispatcher, SomeEvent2Hook);
    super::disable_configurable_hook!(config, Hook::StormEventDispatcher, EventMaybeQueuePopHook);
    let _ = EventHandlerHook.disable();
    let _ = SessionLeaveMapLookupHook.disable();
    let _ = HostMigrationEndProducerAHook.disable();
    let _ = HostMigrationEndProducerBHook.disable();
    let _ = HostMigrationUpdateHook.disable();
    let _ = HostMigrationGameStateUpdateHook.disable();
    let _ = SessionRestartInEventHandlerHook.disable();
    let _ = SessionRestartProducerHook.disable();
    let _ = HostElectAcknowledgeRouterHook.disable();
    let _ = SessionPeerConnectedHandlerHook.disable();
    let _ = SessionLocalTopologyHandlerHook.disable();
    let _ = SessionPeerJoiningHandlerHook.disable();
    let _ = SessionPeerGoneHandlerHook.disable();
    let _ = SessionPeerUnreachableHandlerHook.disable();
    let _ = SessionExitHostMigrationHandlerHook.disable();
    let _ = SessionWaitingRestartHandlerHook.disable();
    let _ = BindSocketHook.disable();
    let _ = ConnectHook.disable();
    let _ = WsaConnectHook.disable();
    let _ = CloseSocketHook.disable();
    let _ = SendToHook.disable();
    let _ = RecvFromHook.disable();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_diagnostic_cache_bounds_only_log_deduplication() {
        let cache = Mutex::new(Vec::<(usize, u32)>::new());
        for owner in 0..512 { assert!(record_migration_diagnostic(&cache, owner, 7)); }
        assert_eq!(cache.lock().unwrap().len(), 256);
        assert!(!record_migration_diagnostic(&cache, 511, 7));
        assert!(record_migration_diagnostic(&cache, 511, 8));
        assert!(record_migration_diagnostic(&cache, 0, 7));
        assert_eq!(cache.lock().unwrap().len(), 256);
    }


    #[test]
    fn socket_address_helpers_read_owned_ipv4_metadata() {
        let value = make_bind_addr(Ipv4Addr::new(10, 20, 30, 40), 13000u16.to_be_bytes());
        let address = (&value as *const SockAddrInRaw).cast::<SOCKADDR>();
        let len = std::mem::size_of::<SockAddrInRaw>() as c_int;
        assert_eq!(sockaddr_ipv4(address, len), Some(Ipv4Addr::new(10, 20, 30, 40)));
        assert_eq!(sockaddr_port(address, len), Some(13000));
        assert_eq!(sockaddr_port_bytes(address, len), 13000u16.to_be_bytes());
        assert!(read_sockaddr_ipv4(address, len - 1).is_none());
    }


    #[test]
    fn test_storm_event_name() {
        let name = b"hello world\0";
        let name_addr = name.as_ptr();
        let id_obj = [name_addr];
        let id_addr: [u8; std::mem::size_of::<*const u8>()] = unsafe { std::mem::transmute(id_obj.as_ptr()) };
        let mut func = vec![0x83, 0x3d];
        for c in id_addr {
            func.push(c);
        }
        func.push(0);
        let vtable = [std::ptr::null(), func.as_ptr()];
        let instance = [vtable.as_ptr()];
        let evt_name = get_storm_event_name(instance.as_ptr().cast());
        assert!(evt_name.is_some());
        assert!(evt_name.unwrap().to_str().unwrap() == "hello world");
    }

    #[test]
    fn migration_events_are_classified_for_focused_logs() {
        assert_eq!(migration_event_category("storm::echo::object::inEvent::SetMigrationMode"), Some("migration-mode"));
        assert_eq!(migration_event_category("storm::echo::object::outEvent::MasterRoleMigrated"), Some("object-authority"));
        assert_eq!(migration_event_category("storm::echo::session::outEvent::HostMigrationStart"), Some("session-migration"));
        assert_eq!(migration_event_category("storm::echo::session::outEvent::PeerGone"), Some("peer-loss"));
        assert_eq!(migration_event_category("storm::echo::voicechat::outEvent::ChatterReady"), None);
    }

    #[test]
    fn migration_acknowledgement_events_are_classified_for_focused_logs() {
        assert_eq!(migration_event_category("storm::echo::session::SessionWaitingRestartMessage"), Some("session-migration"));
        assert_eq!(migration_event_category("storm::echo::session::SessionExitHostMigrationMessage"), Some("session-migration"));
    }

    fn restart_bridge_snapshot(flags_04: u8, flags_05: u8, flags_06: u8, local_participant: u32, migration_host: u32, terminal_state: u32) -> HostMigrationUpdateSnapshot {
        HostMigrationUpdateSnapshot {
            flags_04: Some(flags_04),
            flags_05: Some(flags_05),
            flags_06: Some(flags_06),
            generation: Some(1),
            local_participant: Some(local_participant),
            migration_host: Some(migration_host),
            retry_count: Some(0),
            retry_deadline: Some(0),
            terminal_state: Some(terminal_state),
        }
    }

    #[test]
    fn nonhost_restart_bridge_matches_only_the_observed_missing_restart_state() {
        assert!(should_bridge_nonhost_restart(&restart_bridge_snapshot(0x4b, 0x11, 0xf4, 2, 1, 2)));
        assert!(!should_bridge_nonhost_restart(&restart_bridge_snapshot(0x4b, 0x11, 0xf4, 1, 1, 2)));
        assert!(!should_bridge_nonhost_restart(&restart_bridge_snapshot(0x4b, 0x31, 0xf4, 2, 1, 2)));
        assert!(!should_bridge_nonhost_restart(&restart_bridge_snapshot(0x09, 0x11, 0xf4, 2, 1, 2)));
        assert!(!should_bridge_nonhost_restart(&restart_bridge_snapshot(0x4b, 0x11, 0xf4, 2, 1, 1)));
    }

    fn platform_record_repair_fixture() -> (HostMigrationGameStateSnapshot, HostMigrationPhase5Snapshot) {
        (
            HostMigrationGameStateSnapshot {
                workflow: Some(0x1000),
                phase: Some(5),
                generation: Some(2),
                local_participant: Some(2),
                role: Some(1),
                pending_operation: None,
                workflow_flags: [Some(0), Some(0), Some(0)],
                workflow_participants: [Some(1), Some(0), Some(1), Some(0)],
                platform_interface: Some(0x2000),
                platform_session_key: Some(0x3000),
                platform_session_words_04_0c: [Some(1), Some(1172), Some(24)],
                platform_value_418: Some(u32::MAX),
                platform_flags_428_42c: [Some(1), Some(1), Some(0), Some(0), Some(0)],
                platform_record_words_00_08: [Some(0), Some(0), Some(0)],
            },
            HostMigrationPhase5Snapshot {
                new_host_participant: 1,
                local_participant: 2,
                participant_present: 1,
                identity_object: Some(0x4000),
                identity_ready: Some(1),
                participant_platform_id: Some(1005),
                identity_words_00_2c: [Some(0); 12],
                platform_index: 1,
                platform_interface: Some(0x2000),
                platform_record: Some(0x5000),
                platform_record_valid: Some(0),
                platform_record_host_id: Some(0),
                platform_record_route: Some(0),
                platform_ready_fn: Some(0x6000),
                platform_host_fn: Some(0x7000),
                platform_ready: Some(0),
                platform_host_id: None,
                identities_match: None,
            },
        )
    }

    #[test]
    fn platform_host_record_repair_requires_completed_nonhost_platform_migration() {
        let (state, phase5) = platform_record_repair_fixture();
        assert_eq!(missing_platform_host_record_repair(&state, &phase5), Some((0x5000, 1005, 1172)));

        let mut pending = state;
        pending.pending_operation = Some(0x8000);
        assert_eq!(missing_platform_host_record_repair(&pending, &phase5), None);

        let mut invalid_record = phase5;
        invalid_record.platform_record_host_id = Some(999);
        assert_eq!(missing_platform_host_record_repair(&state, &invalid_record), None);

        let mut unsupported = state;
        unsupported.platform_flags_428_42c[1] = Some(0);
        assert_eq!(missing_platform_host_record_repair(&unsupported, &phase5), None);
    }

    #[test]
    fn host_elect_ack_route_requires_exactly_one_live_owner() {
        assert_eq!(unique_owner([0x1234usize].into_iter()), Some(0x1234));
        assert_eq!(unique_owner(std::iter::empty()), None);
        assert_eq!(unique_owner([0x1234usize, 0x5678].into_iter()), None);
    }

    #[test]
    fn explicit_bind_rewrite_preserves_loopback_and_virtual_ip() {
        let virtual_ip = Ipv4Addr::new(10, 66, 0, 3);
        assert!(should_rewrite_explicit_bind(Ipv4Addr::UNSPECIFIED, virtual_ip));
        assert!(should_rewrite_explicit_bind(Ipv4Addr::new(192, 168, 1, 20), virtual_ip));
        assert!(!should_rewrite_explicit_bind(virtual_ip, virtual_ip));
        assert!(!should_rewrite_explicit_bind(Ipv4Addr::LOCALHOST, virtual_ip));
    }

    #[test]
    fn nonblocking_connect_errors_are_pending_not_fatal() {
        assert!(is_connect_pending_error(WSA_ERROR(WSAEWOULDBLOCK_CODE)));
        assert!(is_connect_pending_error(WSA_ERROR(WSAEINPROGRESS_CODE)));
        assert!(is_connect_pending_error(WSA_ERROR(WSAEALREADY_CODE)));
        assert!(!is_connect_pending_error(WSA_ERROR(10054)));
        assert!(!is_connect_pending_error(WSA_ERROR(10061)));
    }

    #[test]
    fn continuous_recv_error_burst_is_summarized_after_interval() {
        let mut state = RecvErrorLogState::default();
        let now = Instant::now();
        assert_eq!(state.observe(4992, 10054, now), RecvErrorLogDecision::ReportFirst);
        assert_eq!(state.observe(4992, 10054, now + Duration::from_secs(1)), RecvErrorLogDecision::Suppress);
        assert_eq!(state.observe(4992, 10054, now + Duration::from_secs(9)), RecvErrorLogDecision::Suppress);
        assert_eq!(
            state.observe(4992, 10054, now + RECV_ERROR_LOG_INTERVAL),
            RecvErrorLogDecision::ReportOngoing { suppressed: 2 }
        );
    }

    #[test]
    fn recv_error_after_quiet_interval_starts_a_fresh_burst() {
        let mut state = RecvErrorLogState::default();
        let now = Instant::now();
        assert_eq!(state.observe(4992, 10054, now), RecvErrorLogDecision::ReportFirst);
        assert_eq!(state.observe(4992, 10054, now + Duration::from_secs(1)), RecvErrorLogDecision::Suppress);
        assert_eq!(state.observe(4992, 10054, now + Duration::from_secs(12)), RecvErrorLogDecision::ReportFirst);
        assert_eq!(state.observe(4992, 10054, now + Duration::from_secs(13)), RecvErrorLogDecision::Suppress);
        assert_eq!(
            state.observe(4992, 10054, now + Duration::from_secs(22)),
            RecvErrorLogDecision::ReportOngoing { suppressed: 1 }
        );
    }

    #[test]
    fn recv_error_socket_or_code_change_starts_a_fresh_burst() {
        let mut state = RecvErrorLogState::default();
        let now = Instant::now();
        assert_eq!(state.observe(4992, 10054, now), RecvErrorLogDecision::ReportFirst);
        assert_eq!(state.observe(4992, 10061, now + Duration::from_secs(11)), RecvErrorLogDecision::ReportFirst);
        assert_eq!(state.observe(5000, 10061, now + Duration::from_secs(12)), RecvErrorLogDecision::ReportFirst);
    }
}

#[cfg(test)]
#[path = "sockaddr_tests.rs"]
mod sockaddr_boundary_tests;
