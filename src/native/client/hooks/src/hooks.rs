use std::collections::HashMap;
use std::ffi::c_char;
use std::ffi::c_void;
use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::OsString;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::sync::Mutex;
use std::sync::OnceLock;

use retour::static_detour;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::instrument;
use tracing::warn;
use windows::core::s;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_INVALID_DATA;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Foundation::HANDLE;
use self::adapter_filter::IP_ADAPTER_INFO;
use windows::Win32::Networking::WinSock::gethostname;
use windows::Win32::Networking::WinSock::HOSTENT;
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::Win32::System::LibraryLoader::LoadLibraryA;
use windows::Win32::System::Threading::GetThreadId;
use windows::Win32::System::Threading::SetThreadDescription;

use crate::addresses::Addresses;
use crate::config;
use crate::config::Config;
use crate::config::Hook;
use crate::hooks::utils::SomeOrQuestionmark;

mod adapter_filter;
#[cfg(feature = "lan-goal-origin")]
mod goal_origin;
mod datatypes;
pub(crate) mod native_dialog;
mod network_replication;
mod quazal;
mod storm;
mod utils;

use self::datatypes::GearBasicString;
use self::datatypes::MaybeGoal;
use self::datatypes::NetFiniteState;
use self::datatypes::NetFiniteStateID;
use self::datatypes::NetFiniteStateMachine;
use self::datatypes::QuazalStep;

pub(crate) static mut NET_CORE_ADDR: Option<hooks_addresses::Address> = None;

#[repr(C)]
struct SomeStormAddrType {
    vtable: *const c_void,
    addr: u32,
    port: u16,
}

static_detour! {
    static StateAcceptInviteEnterHook: unsafe extern "thiscall" fn(*mut c_void);
}

static_detour! {
    static PrinterHook: unsafe extern "thiscall" fn(*mut c_void, *const i8) -> *mut c_void;
    static LeaveStateHook: unsafe extern "thiscall" fn(*mut NetFiniteState, *mut c_void);
    static NextStateHook: unsafe extern "thiscall" fn(*mut NetFiniteStateMachine, *mut c_void, usize);
    static NetResultBaseHook: unsafe extern "thiscall" fn(*mut c_void, *mut GearBasicString);
    static SomethingWithGoalHook: unsafe extern "thiscall" fn(*mut *mut c_void, usize, *mut MaybeGoal, usize, usize);
    static QuazalStepSequenceJobSetStateHook: unsafe extern "thiscall" fn(*mut c_void, *mut QuazalStep);
    static ThreadStarterHook: unsafe extern "thiscall" fn(*mut c_void);
    static ChangeStateHook: unsafe extern "thiscall" fn(*mut MaybeGoal, *mut NetFiniteState, *mut NetFiniteStateID);
    static NetCoreHook: unsafe extern "thiscall" fn(*mut c_void) -> *mut c_void;
    static NetResultCoreHook: unsafe extern "thiscall" fn(*mut c_void, usize, *mut GearBasicString) -> *mut c_void;
    static NetResultSessionHook: unsafe extern "thiscall" fn(*mut c_void, usize, *mut GearBasicString) -> *mut c_void;
    static NetResultRdvSessionHook: unsafe extern "thiscall" fn(*mut c_void, usize, *mut GearBasicString) -> *mut c_void;
    static NetResultLobbyHook: unsafe extern "thiscall" fn(*mut c_void, usize, *mut GearBasicString) -> *mut c_void;
}

#[repr(C)]
struct StormStateMachine {
    vtable: *const c_void,
    // and more
}

#[repr(C)]
struct StormStateMachineAction {
    vtable: *const c_void,
    callback: *const c_void,
    offset: *const c_void,
    state_machine: *const StormStateMachine,
}

#[repr(C)]
struct StormObject {
    name: *const c_char,
    // and more
}

#[repr(C)]
struct StormEventVtable {
    maybe_destructor: *const c_void,
    global_event: extern "thiscall" fn(*const StormEvent) -> &'static StormObject,
    // and more
}

#[repr(C)]
struct StormEvent {
    vtable: &'static StormEventVtable,
    // and more
}

static_detour! {
    // Retail DX9/DX11 share these thiscall shapes. The admission routine is
    // called with (StormSession*, join_request*) and has no consumed return
    // value; the lock/unlock routines use the same single stack argument.
    static StormJoinAdmissionTraceHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
    static StormSessionLockTraceHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
    static StormSessionUnlockTraceHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
}

static_detour! {
    // Verified from the exact retail DX11 binary. StateJoin::OnEnter receives
    // only ECX=this. The generated JoinSession body receives the adjusted
    // GameSessionProtocol proxy in ECX plus (call_context, GameSessionKey*) and
    // returns its submission result in AL.
    static AsmStateJoinEnterHook: unsafe extern "thiscall" fn(*mut c_void);
    static AsmJoinSessionRmcHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void, *mut c_void) -> bool;

    // .39 verified signatures: callback/update each consume one stack argument
    // (ret 4); snapshot application consumes two stack arguments (ret 8).
    static AsmJoinCompletionEventHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
    static AsmStateJoinUpdateHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
    static AsmStormSessionSnapshotApplyHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void, *mut c_void);

    // .41 verified signatures from the exact retail DX11 binary:
    // - global router consumes one stack argument and returns with `ret 4`;
    // - route filter is thiscall(service, remainder) -> AL and returns `ret 4`;
    // - StateJoin reset is thiscall(state, arg) and returns `ret 4`.
    static AsmRdvCompletionRouterHook: unsafe extern "stdcall" fn(*mut c_void);
    static AsmRdvRouteFilterHook: unsafe extern "thiscall" fn(*mut c_void, u32) -> bool;
    static AsmStateJoinResetHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);

    // .42 verified signatures:
    // - event fanout is thiscall(bus, event) and returns with ret 4;
    // - listener factory is cdecl(owner, service) -> listener and caller pops 8.
    static AsmRdvEventFanoutHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
    static AsmRdvListenerFactoryHook: unsafe extern "cdecl" fn(*mut c_void, *mut c_void) -> *mut c_void;
}

static_detour! {
    static GetAdaptersInfoHook: unsafe extern "stdcall" fn(*mut IP_ADAPTER_INFO, *mut u32) -> u32;
    static GethostbynameHook: unsafe extern "stdcall" fn(*const c_char) -> *mut HOSTENT;
    static GenerateIDHook: unsafe extern "thiscall" fn(*mut NetFiniteStateID, *const i8, bool, *mut c_void);
    static StormSetStateHook: unsafe extern "thiscall" fn(*mut StormStateMachine, *mut NetFiniteStateID);
    static StormStateMachineActionExecuteHook: unsafe extern "thiscall" fn(*mut StormStateMachineAction, *mut  *mut StormEvent, *mut StormEvent);
    static StormErrorFormatter: unsafe extern "thiscall" fn(*mut c_void, *mut GearBasicString) -> *mut GearBasicString;
    static GearStrDestructor: unsafe extern "thiscall" fn(*mut GearBasicString, *mut c_void);
    static AnotherGearStrDestructorHook: unsafe extern "thiscall" fn(*mut GearBasicString, *mut c_void);
    static SomeGearStrConstructor: unsafe extern "thiscall" fn(*mut GearBasicString, *mut c_char) -> *mut GearBasicString;
    static StormHostPortToStringHook: unsafe extern "thiscall" fn(*mut SomeStormAddrType, *mut c_void, *mut c_void) -> *mut c_void;
}

unsafe fn trace_read_u8(base: *mut c_void, offset: usize) -> Option<u8> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset)))
    }
}

unsafe fn trace_read_u32(base: *mut c_void, offset: usize) -> Option<u32> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<u32>()))
    }
}

#[instrument(skip_all)]
fn storm_join_admission_trace(session: *mut c_void, request: *mut c_void) {
    unsafe {
        info!(
            "StormJoinAdmissionTrace phase=enter session={session:?} request={request:?} locked={:?} status_5dc={:?} status_5dd={:?} value_5d8={:?} mode_5f0={:?} flag_5a8={:?} value_58c={:?} value_5b4={:?} value_604={:?} ptr_608={:?} ptr_610={:?} request_1c={:?} request_20={:?} request_34={:?} request_3c={:?}",
            trace_read_u8(session, 0x5de),
            trace_read_u8(session, 0x5dc),
            trace_read_u8(session, 0x5dd),
            trace_read_u32(session, 0x5d8),
            trace_read_u32(session, 0x5f0),
            trace_read_u8(session, 0x5a8),
            trace_read_u32(session, 0x58c),
            trace_read_u32(session, 0x5b4),
            trace_read_u32(session, 0x604),
            trace_read_u32(session, 0x608),
            trace_read_u32(session, 0x610),
            trace_read_u32(request, 0x1c),
            trace_read_u32(request, 0x20),
            trace_read_u32(request, 0x34),
            trace_read_u32(request, 0x3c),
        );

        StormJoinAdmissionTraceHook.call(session, request);

        info!(
            "StormJoinAdmissionTrace phase=leave session={session:?} request={request:?} locked={:?} status_5dc={:?} status_5dd={:?} value_5d8={:?} mode_5f0={:?} value_604={:?}",
            trace_read_u8(session, 0x5de),
            trace_read_u8(session, 0x5dc),
            trace_read_u8(session, 0x5dd),
            trace_read_u32(session, 0x5d8),
            trace_read_u32(session, 0x5f0),
            trace_read_u32(session, 0x604),
        );
    }
}

#[instrument(skip_all)]
fn storm_session_lock_trace(session: *mut c_void, argument: *mut c_void) {
    unsafe {
        let before = trace_read_u8(session, 0x5de);
        info!(
            "StormSessionLockTrace action=lock phase=enter session={session:?} argument={argument:?} locked_before={before:?} status_5dc={:?} status_5dd={:?} mode_5f0={:?}",
            trace_read_u8(session, 0x5dc),
            trace_read_u8(session, 0x5dd),
            trace_read_u32(session, 0x5f0),
        );
        StormSessionLockTraceHook.call(session, argument);
        info!(
            "StormSessionLockTrace action=lock phase=leave session={session:?} argument={argument:?} locked_before={before:?} locked_after={:?}",
            trace_read_u8(session, 0x5de),
        );
    }
}

#[instrument(skip_all)]
fn storm_session_unlock_trace(session: *mut c_void, argument: *mut c_void) {
    unsafe {
        let before = trace_read_u8(session, 0x5de);
        info!(
            "StormSessionLockTrace action=unlock phase=enter session={session:?} argument={argument:?} locked_before={before:?} status_5dc={:?} status_5dd={:?} mode_5f0={:?}",
            trace_read_u8(session, 0x5dc),
            trace_read_u8(session, 0x5dd),
            trace_read_u32(session, 0x5f0),
        );
        StormSessionUnlockTraceHook.call(session, argument);
        info!(
            "StormSessionLockTrace action=unlock phase=leave session={session:?} argument={argument:?} locked_before={before:?} locked_after={:?}",
            trace_read_u8(session, 0x5de),
        );
    }
}

unsafe fn asm_trace_read_u8(base: *mut c_void, offset: usize) -> Option<u8> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset)))
    }
}

unsafe fn asm_trace_read_u16(base: *mut c_void, offset: usize) -> Option<u16> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<u16>()))
    }
}

unsafe fn asm_trace_read_u32(base: *mut c_void, offset: usize) -> Option<u32> {
    if base.is_null() {
        None
    } else {
        Some(std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<u32>()))
    }
}

unsafe fn asm_trace_read_ptr(base: *mut c_void, offset: usize) -> *mut c_void {
    if base.is_null() {
        std::ptr::null_mut()
    } else {
        std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<*mut c_void>())
    }
}

unsafe fn asm_trace_offset_ptr(base: *mut c_void, offset: usize) -> *mut c_void {
    if base.is_null() {
        std::ptr::null_mut()
    } else {
        base.cast::<u8>().add(offset).cast::<c_void>()
    }
}

unsafe fn asm_resolve_state_join_storm_session(state_join: *mut c_void) -> (*mut c_void, Option<u32>, Option<u32>, *mut c_void, Option<u32>, *mut c_void) {
    let owner = asm_trace_read_ptr(state_join, 0x08);
    let status_328 = asm_trace_read_u32(owner, 0x328);
    let status_158 = asm_trace_read_u32(owner, 0x158);
    let slot = if matches!(status_328, Some(4..=6)) {
        asm_trace_offset_ptr(owner, 0x220)
    } else if matches!(status_158, Some(4..=6)) {
        asm_trace_offset_ptr(owner, 0x50)
    } else {
        std::ptr::null_mut()
    };
    let class_tag = asm_trace_read_u32(slot, 0x24);
    let storm_session = if class_tag == Some(0x0b) {
        asm_trace_read_ptr(slot, 0x10)
    } else {
        std::ptr::null_mut()
    };
    (owner, status_328, status_158, slot, class_tag, storm_session)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JoinWaitSnapshot {
    service: usize,
    session_id: Option<u32>,
    flag_429: Option<u8>,
    flag_42a: Option<u8>,
    flag_42b: Option<u8>,
    flag_42c: Option<u8>,
    state_mode: Option<u32>,
}

static ASM_JOIN_WAIT_SNAPSHOTS: OnceLock<Mutex<HashMap<usize, JoinWaitSnapshot>>> = OnceLock::new();
static ASM_JOIN_WAIT_HEARTBEATS: OnceLock<Mutex<HashMap<usize, u64>>> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
struct PrivateJoinCompletionBridge {
    service: usize,
    session_id: u32,
    completed: bool,
}

static ASM_PRIVATE_JOIN_COMPLETION_BRIDGES: OnceLock<Mutex<HashMap<usize, PrivateJoinCompletionBridge>>> = OnceLock::new();

fn asm_private_join_bridge_arm(state_join: *mut c_void, service: *mut c_void, session_id: u32) {
    let map = ASM_PRIVATE_JOIN_COMPLETION_BRIDGES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.insert(
        state_join as usize,
        PrivateJoinCompletionBridge {
            service: service as usize,
            session_id,
            completed: false,
        },
    );
    info!("PrivateJoinBridge phase=armed state_join={state_join:?} service={service:?} session_id={session_id} delay_heartbeats=120");
}

fn asm_private_join_bridge_disarm(state_join: *mut c_void, reason: &str) {
    // Both counters belong to one join attempt, not the reusable native address.
    if let Some(map) = ASM_JOIN_WAIT_HEARTBEATS.get() {
        map.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&(state_join as usize));
    }
    if let Some(map) = ASM_JOIN_WAIT_SNAPSHOTS.get() {
        map.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&(state_join as usize));
    }
    let map = ASM_PRIVATE_JOIN_COMPLETION_BRIDGES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = map.remove(&(state_join as usize)) {
        info!(
            "PrivateJoinBridge phase=disarmed state_join={state_join:?} service=0x{:x} session_id={} completed={} reason={reason}",
            state.service, state.session_id, state.completed,
        );
    }
}

unsafe fn asm_private_join_bridge_try_complete(state_join: *mut c_void, heartbeat: u64, snapshot: JoinWaitSnapshot) -> bool {
    if heartbeat < 120 {
        return false;
    }

    let should_write = {
        let map = ASM_PRIVATE_JOIN_COMPLETION_BRIDGES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(state) = map.get_mut(&(state_join as usize)) else {
            return false;
        };
        if state.completed
            || snapshot.service != state.service
            || snapshot.session_id != Some(state.session_id)
            || snapshot.flag_429 != Some(0)
            || snapshot.flag_42a != Some(0)
            || snapshot.flag_42b != Some(1)
            || snapshot.flag_42c != Some(0)
            || snapshot.state_mode != Some(0)
        {
            false
        } else {
            state.completed = true;
            true
        }
    };

    if !should_write {
        if heartbeat == 120 || heartbeat % 300 == 0 {
            info!(
                "PrivateJoinBridge phase=waiting state_join={state_join:?} heartbeat={heartbeat} service=0x{:x} session_id={:?} flag_429={:?} flag_42a={:?} flag_42b={:?} flag_42c={:?} state_mode={:?}",
                snapshot.service, snapshot.session_id, snapshot.flag_429, snapshot.flag_42a, snapshot.flag_42b, snapshot.flag_42c, snapshot.state_mode,
            );
        }
        return false;
    }

    let service = snapshot.service as *mut c_void;
    let flag_42a = service.cast::<u8>().add(0x42a);
    let before = std::ptr::read_volatile(flag_42a);
    if before != 0 {
        info!(
            "PrivateJoinBridge phase=skip-write state_join={state_join:?} service={service:?} session_id={:?} heartbeat={heartbeat} reason=flag-already-set flag_42a={before}",
            snapshot.session_id,
        );
        return false;
    }

    std::ptr::write_volatile(flag_42a, 1);
    let after = std::ptr::read_volatile(flag_42a);
    info!(
        "PrivateJoinBridge phase=write-complete state_join={state_join:?} service={service:?} session_id={:?} heartbeat={heartbeat} flag_42a_before={before} flag_42a_after={after} exact_private_guard=true one_shot=true",
        snapshot.session_id,
    );
    after == 1
}

fn asm_join_wait_heartbeat(state_join: *mut c_void) -> u64 {
    let map = ASM_JOIN_WAIT_HEARTBEATS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let count = map.entry(state_join as usize).or_insert(0);
    *count = (*count).saturating_add(1);
    *count
}

unsafe fn asm_join_wait_snapshot(state_join: *mut c_void) -> JoinWaitSnapshot {
    let service = asm_trace_read_ptr(state_join, 0x30);
    let session_key = asm_trace_read_ptr(service, 0x40c);
    JoinWaitSnapshot {
        service: service as usize,
        session_id: asm_trace_read_u32(session_key, 0x08),
        flag_429: asm_trace_read_u8(service, 0x429),
        flag_42a: asm_trace_read_u8(service, 0x42a),
        flag_42b: asm_trace_read_u8(service, 0x42b),
        flag_42c: asm_trace_read_u8(service, 0x42c),
        state_mode: asm_trace_read_u32(state_join, 0x40),
    }
}

fn asm_should_log_join_wait(state_join: *mut c_void, snapshot: JoinWaitSnapshot) -> bool {
    let map = ASM_JOIN_WAIT_SNAPSHOTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.insert(state_join as usize, snapshot) != Some(snapshot)
}

#[instrument(skip_all)]
fn asm_state_join_enter(state_join: *mut c_void) {
    asm_private_join_bridge_disarm(state_join, "statejoin-enter");
    unsafe {
        let service = asm_trace_read_ptr(state_join, 0x30);
        let protocol_proxy = asm_trace_read_ptr(service, 0x404);
        let session_key = asm_trace_read_ptr(service, 0x40c);
        let (owner, status_328, status_158, slot, class_tag, storm_session) = asm_resolve_state_join_storm_session(state_join);
        let storm_flag_5a8_before = asm_trace_read_u8(storm_session, 0x5a8);
        let flag_42a_before = asm_trace_read_u8(service, 0x42a);
        let flag_42b_before = asm_trace_read_u8(service, 0x42b);
        let flag_42c_before = asm_trace_read_u8(service, 0x42c);
        info!(
            "AsmStateJoinBranch phase=before state_join={state_join:?} owner={owner:?} owner_status_328={status_328:?} owner_status_158={status_158:?} slot={slot:?} class_tag={class_tag:?} storm_session={storm_session:?} storm_flag_5a0={:?} storm_value_5a4={:?} storm_flag_5a8={storm_flag_5a8_before:?} storm_flag_5a9={:?} storm_value_5b4={:?} storm_value_5d8={:?} storm_status_5dc={:?} storm_status_5dd={:?} storm_mode_5f0={:?} storm_value_604={:?} storm_ptr_610={:?} service={service:?} flag_42a={flag_42a_before:?} flag_42b={flag_42b_before:?} flag_42c={flag_42c_before:?}",
            asm_trace_read_u8(storm_session, 0x5a0),
            asm_trace_read_u32(storm_session, 0x5a4),
            asm_trace_read_u8(storm_session, 0x5a9),
            asm_trace_read_u32(storm_session, 0x5b4),
            asm_trace_read_u32(storm_session, 0x5d8),
            asm_trace_read_u8(storm_session, 0x5dc),
            asm_trace_read_u8(storm_session, 0x5dd),
            asm_trace_read_u32(storm_session, 0x5f0),
            asm_trace_read_u32(storm_session, 0x604),
            asm_trace_read_ptr(storm_session, 0x610),
        );
        info!(
            "AsmStateJoinEnter phase=enter state_join={state_join:?} state_owner={:?} service={service:?} service_vtable={:?} protocol_proxy={protocol_proxy:?} session_key={session_key:?} type_id={:?} session_id={:?} flag_42a={:?} flag_42b={:?} flag_42c={:?}",
            asm_trace_read_ptr(state_join, 0x08),
            asm_trace_read_ptr(service, 0x00),
            asm_trace_read_u32(session_key, 0x04),
            asm_trace_read_u32(session_key, 0x08),
            asm_trace_read_u8(service, 0x42a),
            asm_trace_read_u8(service, 0x42b),
            asm_trace_read_u8(service, 0x42c),
        );
        AsmStateJoinEnterHook.call(state_join);
        let flag_42a_after = asm_trace_read_u8(service, 0x42a);
        let flag_42b_after = asm_trace_read_u8(service, 0x42b);
        let flag_42c_after = asm_trace_read_u8(service, 0x42c);
        let branch = if flag_42c_before == Some(0) && storm_flag_5a8_before != Some(0) && flag_42b_before == Some(0) && flag_42b_after == Some(1) {
            "storm-flag-skip-rmc-wait-completion-event"
        } else if flag_42c_before == Some(0) && storm_flag_5a8_before == Some(0) {
            "normal-join-rmc-submission-path"
        } else {
            "alternate-statejoin-path"
        };
        let bridge_type_id = asm_trace_read_u32(session_key, 0x04);
        let bridge_session_id = asm_trace_read_u32(session_key, 0x08);
        let bridge_eligible = branch == "storm-flag-skip-rmc-wait-completion-event"
            && bridge_type_id == Some(1)
            && bridge_session_id.is_some_and(|value| value != 0)
            && flag_42a_after == Some(0)
            && flag_42b_after == Some(1)
            && flag_42c_after == Some(0);
        if bridge_eligible {
            asm_private_join_bridge_arm(state_join, service, bridge_session_id.expect("guarded exact private session id"));
        } else {
            asm_private_join_bridge_disarm(state_join, "statejoin-not-eligible");
            info!(
                "PrivateJoinBridge phase=not-armed state_join={state_join:?} service={service:?} branch={branch} type_id={bridge_type_id:?} session_id={bridge_session_id:?} flag_42a={flag_42a_after:?} flag_42b={flag_42b_after:?} flag_42c={flag_42c_after:?}"
            );
        }
        info!(
            "AsmStateJoinBranch phase=after state_join={state_join:?} service={service:?} storm_session={storm_session:?} storm_flag_5a8_before={storm_flag_5a8_before:?} flag_42a_before={flag_42a_before:?} flag_42a_after={flag_42a_after:?} flag_42b_before={flag_42b_before:?} flag_42b_after={flag_42b_after:?} flag_42c_before={flag_42c_before:?} flag_42c_after={flag_42c_after:?} branch={branch}"
        );
        info!(
            "AsmStateJoinEnter phase=leave state_join={state_join:?} service={service:?} session_key={session_key:?} type_id={:?} session_id={:?} flag_42a={:?} flag_42b={:?} flag_42c={:?}",
            asm_trace_read_u32(session_key, 0x04),
            asm_trace_read_u32(session_key, 0x08),
            asm_trace_read_u8(service, 0x42a),
            asm_trace_read_u8(service, 0x42b),
            asm_trace_read_u8(service, 0x42c),
        );
    }
}

#[instrument(skip_all)]
fn asm_join_session_rmc(protocol: *mut c_void, call_ctx: *mut c_void, session_key: *mut c_void) -> bool {
    unsafe {
        info!(
            "AsmJoinSessionBuild phase=enter protocol={protocol:?} protocol_id_word={:?} call_ctx={call_ctx:?} session_key={session_key:?} type_id={:?} session_id={:?} body_va=0x022BE610 method_id=0x16",
            asm_trace_read_u16(protocol, 0x18),
            asm_trace_read_u32(session_key, 0x04),
            asm_trace_read_u32(session_key, 0x08),
        );
        let result = AsmJoinSessionRmcHook.call(protocol, call_ctx, session_key);
        info!(
            "AsmJoinSessionBuild phase=leave protocol={protocol:?} call_ctx={call_ctx:?} session_key={session_key:?} type_id={:?} session_id={:?} submitted={result}",
            asm_trace_read_u32(session_key, 0x04),
            asm_trace_read_u32(session_key, 0x08),
        );
        result
    }
}

#[instrument(skip_all)]
fn asm_join_completion_event(service: *mut c_void, event: *mut c_void) {
    unsafe {
        let session_key = asm_trace_read_ptr(service, 0x40c);
        let event_code = asm_trace_read_u32(event, 0x08);
        let event_remainder = event_code.map(|value| value % 1000);
        let event_session_id = asm_trace_read_u32(event, 0x0c);
        let event_host_id = asm_trace_read_u32(event, 0x04);
        let current_session_id = asm_trace_read_u32(session_key, 0x08);
        let before_42a = asm_trace_read_u8(service, 0x42a);
        let before_42b = asm_trace_read_u8(service, 0x42b);
        let before_42c = asm_trace_read_u8(service, 0x42c);
        let record_before = asm_trace_read_ptr(service, 0x430);
        let record_valid_before = asm_trace_read_u8(record_before, 0x00);
        let record_host_id_before = asm_trace_read_u32(record_before, 0x04);
        let record_route_before = asm_trace_read_u32(record_before, 0x08);
        let predicted = match event_remainder {
            Some(3) if event_session_id == current_session_id => "session-event-match-set-42a",
            Some(3) => "session-event-id-mismatch",
            Some(1) if before_42c == Some(0) => "store-join-event-payload",
            Some(1) => "join-event-ignored-flag42c",
            _ => "unrelated-event",
        };
        info!(
            "AsmJoinCompletionEvent phase=enter service={service:?} event={event:?} event_word_0={:?} event_host_id={event_host_id:?} event_code={event_code:?} event_remainder={event_remainder:?} event_route_or_session_id={event_session_id:?} session_key={session_key:?} type_id={:?} current_session_id={current_session_id:?} flag_42a={before_42a:?} flag_42b={before_42b:?} flag_42c={before_42c:?} record={record_before:?} record_valid={record_valid_before:?} record_host_id={record_host_id_before:?} record_route={record_route_before:?} predicted={predicted}",
            asm_trace_read_u32(event, 0x00),
            asm_trace_read_u32(session_key, 0x04),
        );
        AsmJoinCompletionEventHook.call(service, event);
        let after_42a = asm_trace_read_u8(service, 0x42a);
        let after_42b = asm_trace_read_u8(service, 0x42b);
        let after_42c = asm_trace_read_u8(service, 0x42c);
        let record_after = asm_trace_read_ptr(service, 0x430);
        let record_valid_after = asm_trace_read_u8(record_after, 0x00);
        let record_host_id_after = asm_trace_read_u32(record_after, 0x04);
        let record_route_after = asm_trace_read_u32(record_after, 0x08);
        info!(
            "AsmJoinCompletionEvent phase=leave service={service:?} event={event:?} event_host_id={event_host_id:?} event_code={event_code:?} event_remainder={event_remainder:?} event_route_or_session_id={event_session_id:?} current_session_id={current_session_id:?} flag_42a_before={before_42a:?} flag_42a_after={after_42a:?} flag_42b_before={before_42b:?} flag_42b_after={after_42b:?} flag_42c_before={before_42c:?} flag_42c_after={after_42c:?} record_before={record_before:?} record_after={record_after:?} record_valid_before={record_valid_before:?} record_valid_after={record_valid_after:?} record_host_id_before={record_host_id_before:?} record_host_id_after={record_host_id_after:?} record_route_before={record_route_before:?} record_route_after={record_route_after:?} changed_42a={} changed_record={} predicted={predicted}",
            before_42a != after_42a,
            record_valid_before != record_valid_after
                || record_host_id_before != record_host_id_after
                || record_route_before != record_route_after,
        );
    }
}

#[instrument(skip_all)]
fn asm_state_join_update(state_join: *mut c_void, update_arg: *mut c_void) {
    unsafe {
        let mut before = asm_join_wait_snapshot(state_join);
        let log_before = asm_should_log_join_wait(state_join, before);
        let heartbeat = asm_join_wait_heartbeat(state_join);
        if asm_private_join_bridge_try_complete(state_join, heartbeat, before) {
            before = asm_join_wait_snapshot(state_join);
        }
        if before.flag_42b == Some(1) && before.flag_42a == Some(0) && (heartbeat == 1 || heartbeat % 300 == 0) {
            info!(
                "AsmStateJoinWaitHeartbeat state_join={state_join:?} service=0x{:x} session_id={:?} heartbeat={heartbeat} flag_429={:?} flag_42a={:?} flag_42b={:?} flag_42c={:?} state_mode={:?}",
                before.service, before.session_id, before.flag_429, before.flag_42a, before.flag_42b, before.flag_42c, before.state_mode,
            );
        }
        if log_before {
            info!(
                "AsmStateJoinWait phase=enter state_join={state_join:?} update_arg={update_arg:?} service=0x{:x} session_id={:?} flag_429={:?} flag_42a={:?} flag_42b={:?} flag_42c={:?} state_mode={:?} waiting_for_completion_event={}",
                before.service,
                before.session_id,
                before.flag_429,
                before.flag_42a,
                before.flag_42b,
                before.flag_42c,
                before.state_mode,
                before.flag_42b == Some(1) && before.flag_42a == Some(0),
            );
        }
        AsmStateJoinUpdateHook.call(state_join, update_arg);
        let after = asm_join_wait_snapshot(state_join);
        let log_after = asm_should_log_join_wait(state_join, after);
        if log_before || log_after || before != after {
            info!(
                "AsmStateJoinWait phase=leave state_join={state_join:?} update_arg={update_arg:?} service=0x{:x} session_id={:?} flag_429={:?} flag_42a={:?} flag_42b={:?} flag_42c={:?} state_mode={:?} changed={} waiting_for_completion_event={}",
                after.service,
                after.session_id,
                after.flag_429,
                after.flag_42a,
                after.flag_42b,
                after.flag_42c,
                after.state_mode,
                before != after,
                after.flag_42b == Some(1) && after.flag_42a == Some(0),
            );
        }
    }
}

#[instrument(skip_all)]
fn asm_storm_session_snapshot_apply(storm_session: *mut c_void, apply_arg: *mut c_void, source_snapshot: *mut c_void) {
    unsafe {
        let before_5a8 = asm_trace_read_u8(storm_session, 0x5a8);
        let source_2a0 = asm_trace_read_u8(source_snapshot, 0x2a0);
        info!(
            "AsmStormSessionSnapshot phase=enter storm_session={storm_session:?} apply_arg={apply_arg:?} source_snapshot={source_snapshot:?} source_flag_2a0={source_2a0:?} source_value_298={:?} source_flag_2d4={:?} source_value_2dc={:?} source_value_2e0={:?} dest_flag_5a8={before_5a8:?} dest_flag_5a9={:?} dest_value_5a4={:?} dest_value_5b4={:?} dest_mode_5f0={:?} dest_value_604={:?} dest_ptr_610={:?}",
            asm_trace_read_u32(source_snapshot, 0x298),
            asm_trace_read_u8(source_snapshot, 0x2d4),
            asm_trace_read_u32(source_snapshot, 0x2dc),
            asm_trace_read_u32(source_snapshot, 0x2e0),
            asm_trace_read_u8(storm_session, 0x5a9),
            asm_trace_read_u32(storm_session, 0x5a4),
            asm_trace_read_u32(storm_session, 0x5b4),
            asm_trace_read_u32(storm_session, 0x5f0),
            asm_trace_read_u32(storm_session, 0x604),
            asm_trace_read_ptr(storm_session, 0x610),
        );
        AsmStormSessionSnapshotApplyHook.call(storm_session, apply_arg, source_snapshot);
        let after_5a8 = asm_trace_read_u8(storm_session, 0x5a8);
        info!(
            "AsmStormSessionSnapshot phase=leave storm_session={storm_session:?} source_snapshot={source_snapshot:?} source_flag_2a0={source_2a0:?} dest_flag_5a8_before={before_5a8:?} dest_flag_5a8_after={after_5a8:?} changed_5a8={} dest_flag_5a9={:?} dest_value_5a4={:?} dest_value_5b4={:?} dest_mode_5f0={:?}",
            before_5a8 != after_5a8,
            asm_trace_read_u8(storm_session, 0x5a9),
            asm_trace_read_u32(storm_session, 0x5a4),
            asm_trace_read_u32(storm_session, 0x5b4),
            asm_trace_read_u32(storm_session, 0x5f0),
        );
    }
}

unsafe fn asm_rdv_registry() -> *mut c_void {
    std::ptr::read_unaligned(0x03383df8usize as *const *mut c_void)
}

unsafe fn asm_rdv_service_slot(registry: *mut c_void, index: usize) -> *mut c_void {
    if registry.is_null() || index >= 2 {
        std::ptr::null_mut()
    } else {
        asm_trace_read_ptr(registry, 0x24 + index * 4)
    }
}

unsafe fn asm_rdv_current_session_id(service: *mut c_void) -> Option<u32> {
    let session_key = asm_trace_read_ptr(service, 0x40c);
    asm_trace_read_u32(session_key, 0x08)
}

unsafe fn asm_rdv_vslot(service: *mut c_void, offset: usize) -> *mut c_void {
    let vtable = asm_trace_read_ptr(service, 0x00);
    asm_trace_read_ptr(vtable, offset)
}

#[instrument(skip_all)]
fn asm_rdv_completion_router(event: *mut c_void) {
    unsafe {
        let event_code = asm_trace_read_u32(event, 0x08);
        let event_class = event_code.map(|value| value / 1000);
        let event_remainder = event_code.map(|value| value % 1000);
        let callback_session_id = asm_trace_read_u32(event, 0x0c);
        let route_session_id = asm_trace_read_u32(event, 0x10);
        let registry = asm_rdv_registry();
        info!(
            "AsmRdvCompletionRouter phase=enter event={event:?} event_vtable={:?} event_word_4={:?} event_code={event_code:?} event_class={event_class:?} event_remainder={event_remainder:?} callback_session_id_0c={callback_session_id:?} route_session_id_10={route_session_id:?} event_word_14={:?} registry={registry:?} class_routes_rdv={}",
            asm_trace_read_ptr(event, 0x00),
            asm_trace_read_u32(event, 0x04),
            asm_trace_read_u32(event, 0x14),
            event_class == Some(7),
        );
        for index in 0..2 {
            let service = asm_rdv_service_slot(registry, index);
            let flag_429 = asm_trace_read_u8(service, 0x429);
            let flag_42a = asm_trace_read_u8(service, 0x42a);
            let flag_42b = asm_trace_read_u8(service, 0x42b);
            let flag_42c = asm_trace_read_u8(service, 0x42c);
            let current_session_id = asm_rdv_current_session_id(service);
            let predicted_filter = event_remainder == Some(3) || flag_429 == Some(1);
            let predicted_session_match = route_session_id.is_some() && route_session_id == current_session_id;
            let predicted_call_vslot_70 = event_class == Some(7) && predicted_filter && predicted_session_match;
            info!(
                "AsmRdvCompletionRouteSlot phase=before index={index} registry={registry:?} service={service:?} service_vtable={:?} vslot_6c={:?} vslot_70={:?} routes_join_completion={} current_session_id={current_session_id:?} route_session_id_10={route_session_id:?} callback_session_id_0c={callback_session_id:?} event_remainder={event_remainder:?} flag_429={flag_429:?} flag_42a={flag_42a:?} flag_42b={flag_42b:?} flag_42c={flag_42c:?} predicted_filter={predicted_filter} predicted_session_match={predicted_session_match} predicted_call_vslot_70={predicted_call_vslot_70}",
                asm_trace_read_ptr(service, 0x00),
                asm_rdv_vslot(service, 0x6c),
                asm_rdv_vslot(service, 0x70),
                asm_rdv_vslot(service, 0x70) as usize == 0x007bdfb0,
            );
        }
        AsmRdvCompletionRouterHook.call(event);
        info!(
            "AsmRdvCompletionRouter phase=leave event={event:?} event_code={event_code:?} event_class={event_class:?} event_remainder={event_remainder:?} callback_session_id_0c={callback_session_id:?} route_session_id_10={route_session_id:?}"
        );
    }
}

#[instrument(skip_all)]
fn asm_rdv_route_filter(service: *mut c_void, event_remainder: u32) -> bool {
    unsafe {
        let before_429 = asm_trace_read_u8(service, 0x429);
        let before_42a = asm_trace_read_u8(service, 0x42a);
        let before_42b = asm_trace_read_u8(service, 0x42b);
        let before_42c = asm_trace_read_u8(service, 0x42c);
        let current_session_id = asm_rdv_current_session_id(service);
        let result = AsmRdvRouteFilterHook.call(service, event_remainder);
        info!(
            "AsmRdvRouteFilter service={service:?} service_vtable={:?} vslot_70={:?} routes_join_completion={} event_remainder={event_remainder} current_session_id={current_session_id:?} flag_429={before_429:?} flag_42a={before_42a:?} flag_42b={before_42b:?} flag_42c={before_42c:?} result={result}",
            asm_trace_read_ptr(service, 0x00),
            asm_rdv_vslot(service, 0x70),
            asm_rdv_vslot(service, 0x70) as usize == 0x007bdfb0,
        );
        result
    }
}

#[instrument(skip_all)]
fn asm_state_join_reset(state_join: *mut c_void, reset_arg: *mut c_void) {
    unsafe {
        let service = asm_trace_read_ptr(state_join, 0x30);
        let session_id = asm_rdv_current_session_id(service);
        let before_42a = asm_trace_read_u8(service, 0x42a);
        let before_42b = asm_trace_read_u8(service, 0x42b);
        let before_42c = asm_trace_read_u8(service, 0x42c);
        info!(
            "AsmStateJoinReset phase=enter state_join={state_join:?} reset_arg={reset_arg:?} service={service:?} session_id={session_id:?} flag_42a={before_42a:?} flag_42b={before_42b:?} flag_42c={before_42c:?}"
        );
        asm_private_join_bridge_disarm(state_join, "statejoin-reset");
        AsmStateJoinResetHook.call(state_join, reset_arg);
        info!(
            "AsmStateJoinReset phase=leave state_join={state_join:?} reset_arg={reset_arg:?} service={service:?} session_id={session_id:?} flag_42a_before={before_42a:?} flag_42a_after={:?} flag_42b_before={before_42b:?} flag_42b_after={:?} flag_42c_before={before_42c:?} flag_42c_after={:?}",
            asm_trace_read_u8(service, 0x42a),
            asm_trace_read_u8(service, 0x42b),
            asm_trace_read_u8(service, 0x42c),
        );
    }
}

#[instrument(skip_all)]
fn asm_rdv_event_fanout(bus: *mut c_void, event: *mut c_void) {
    unsafe {
        let event_code = asm_trace_read_u32(event, 0x08);
        let event_class = event_code.map(|value| value / 1000);
        let event_remainder = event_code.map(|value| value % 1000);
        let callback_session_id = asm_trace_read_u32(event, 0x0c);
        let route_session_id = asm_trace_read_u32(event, 0x10);
        info!(
            "AsmRdvEventFanout phase=enter bus={bus:?} bus_vtable={:?} event={event:?} event_vtable={:?} event_code={event_code:?} event_class={event_class:?} event_remainder={event_remainder:?} callback_session_id_0c={callback_session_id:?} route_session_id_10={route_session_id:?} map_20={:?} map_38={:?} map_50={:?} map_68={:?} map_78={:?} map_88={:?} class_routes_rdv={}",
            asm_trace_read_ptr(bus, 0x00),
            asm_trace_read_ptr(event, 0x00),
            asm_trace_read_ptr(bus, 0x20),
            asm_trace_read_ptr(bus, 0x38),
            asm_trace_read_ptr(bus, 0x50),
            asm_trace_read_ptr(bus, 0x68),
            asm_trace_read_ptr(bus, 0x78),
            asm_trace_read_ptr(bus, 0x88),
            event_class == Some(7),
        );
        AsmRdvEventFanoutHook.call(bus, event);
        info!(
            "AsmRdvEventFanout phase=leave bus={bus:?} event={event:?} event_code={event_code:?} event_class={event_class:?} event_remainder={event_remainder:?} callback_session_id_0c={callback_session_id:?} route_session_id_10={route_session_id:?}"
        );
    }
}

#[instrument(skip_all)]
fn asm_rdv_listener_factory(owner: *mut c_void, service: *mut c_void) -> *mut c_void {
    unsafe {
        info!(
            "AsmRdvListenerFactory phase=enter owner={owner:?} service={service:?} service_vtable={:?}",
            asm_trace_read_ptr(service, 0x00),
        );
        let listener = AsmRdvListenerFactoryHook.call(owner, service);
        info!(
            "AsmRdvListenerFactory phase=leave owner={owner:?} service={service:?} listener={listener:?} listener_vtable={:?} listener_owner_30={:?} expected_router_vtable={} expected_router_slot={:?}",
            asm_trace_read_ptr(listener, 0x00),
            asm_trace_read_ptr(listener, 0x30),
            asm_trace_read_ptr(listener, 0x00) as usize == 0x029bcb48,
            asm_trace_read_ptr(0x029bcb48usize as *mut c_void, 0x04),
        );
        listener
    }
}

// PrivateInviteControllerBridge49: the retail StateAcceptInvite::Enter
// chooses MatchController from owner+0x4e4 and PartyController from owner+0x4e0.
// Both slots hold the same 0x3c0-byte session descriptor type. Kind 1 carries
// an already-established private game transport but retail places the decoded
// descriptor in the Party slot, so that one path transfers ownership. Kind 2
// must remain in the Party slot: retail joins that transport first and only
// queues JoinMatch after the Storm session is connected.
fn state_accept_invite_enter_target(addr: &Addresses) -> Option<hooks_addresses::Address> {
    match addr.func_net_finite_state_machine_next_state {
        // Supported retail DX11 hashes c6b9f330... and c52b3d09... share code.
        Some(0x0083_4ff0) => Some(0x0087_a2e0),
        _ => None,
    }
}

#[allow(clippy::cast_ptr_alignment)]
fn state_accept_invite_enter(state: *mut c_void) {
    let expected_session_id = crate::uplay_r1_loader::take_private_invite_match_target();
    if let Some(expected_session_id) = expected_session_id {
        if state.is_null() {
            error!("PrivateInviteControllerBridge49 phase=skip reason=null-state expected_session_id={expected_session_id} behavior_changed=false");
        } else {
            unsafe {
                let owner = state.cast::<u8>().add(0x30).cast::<*mut c_void>().read();
                if owner.is_null() {
                    error!("PrivateInviteControllerBridge49 phase=skip reason=null-owner state={state:p} expected_session_id={expected_session_id} behavior_changed=false");
                } else {
                    let party_slot = owner.cast::<u8>().add(0x4e0).cast::<*mut c_void>();
                    let match_slot = owner.cast::<u8>().add(0x4e4).cast::<*mut c_void>();
                    let party_descriptor = party_slot.read();
                    let match_descriptor = match_slot.read();
                    info!(
                        "PrivateInviteControllerBridge49 phase=inspect state={state:p} owner={owner:p} party_descriptor={party_descriptor:p} match_descriptor={match_descriptor:p} expected_session_id={expected_session_id}"
                    );
                    if !party_descriptor.is_null() && match_descriptor.is_null() {
                        match_slot.write(party_descriptor);
                        party_slot.write(std::ptr::null_mut());
                        info!(
                            "PrivateInviteControllerBridge49 phase=move-party-to-match descriptor={party_descriptor:p} expected_session_id={expected_session_id} destination=NetOnlineMatchController behavior_changed=true"
                        );
                        crate::uplay_r1_loader::acknowledge_private_invite_match_target(expected_session_id);
                    } else if party_descriptor.is_null() && !match_descriptor.is_null() {
                        info!(
                            "PrivateInviteControllerBridge49 phase=already-match descriptor={match_descriptor:p} expected_session_id={expected_session_id} destination=NetOnlineMatchController behavior_changed=false"
                        );
                        crate::uplay_r1_loader::acknowledge_private_invite_match_target(expected_session_id);
                    } else {
                        warn!(
                            "PrivateInviteControllerBridge49 phase=skip reason=slot-invariant party_descriptor={party_descriptor:p} match_descriptor={match_descriptor:p} expected_session_id={expected_session_id} behavior_changed=false"
                        );
                    }
                }
            }
        }
    }
    unsafe { StateAcceptInviteEnterHook.call(state) }
}

#[instrument(skip_all)]
fn printer(x: *mut c_void, msg: *const i8) -> *mut c_void {
    // log!("printer called: {:08x} {:08x}", x as usize, msg as usize);
    if !msg.is_null() {
        let msg = unsafe { CStr::from_ptr(msg) };
        if !msg.is_empty() {
            info!("{}", msg.to_string_lossy());
        }
    }
    unsafe { PrinterHook.call(x, msg) }
}

#[instrument(skip_all)]
fn leave_state(x: *mut NetFiniteState, y: *mut c_void) {
    info!("Leaving state {}", utils::state_ptr_to_name(x));
    unsafe { LeaveStateHook.call(x, y) }
}

#[instrument(skip_all)]
fn next_state(sm: *mut NetFiniteStateMachine, y: *mut c_void, z: usize) {
    let next_state_id = {
        let state_info = y as *const usize;
        if state_info.is_null() {
            None
        } else {
            Some(unsafe { *state_info } as u32)
        }
    };
    let map = utils::hashes();
    if !sm.is_null() {
        let sm_name = unsafe { (*sm).get_statemachine_name() };
        let current_state = unsafe { (*sm).current_state };
        let vtable = unsafe { (*sm).vtable };
        let last_state = unsafe { (*sm).last_state };
        if let Some((_id, name)) = next_state_id.and_then(|id| Some((id, map.get(&(id as usize))?))) {
            info!(
                "Next state: {name} StateMachine(inst={sm:?}, vtable={vtable:?}, name={sm_name}) current={} last={}",
                utils::state_ptr_to_name(current_state),
                utils::state_ptr_to_name(last_state)
            );
        } else {
            info!(
                "Next state: StateMachine(inst={sm:?}, vtable={vtable:?}, name={sm_name}) current={} last={}",
                utils::state_ptr_to_name(current_state),
                utils::state_ptr_to_name(last_state)
            );
        }
    }
    unsafe { NextStateHook.call(sm, y, z) };
}

#[instrument(skip_all)]
fn net_result_base(this: *mut c_void, string: *mut GearBasicString) {
    unsafe {
        if !string.is_null() && string.is_aligned() && !(*string).internal.is_null() {
            let string2 = &*string;
            let internal = &*string2.internal;
            let cstr = internal.as_str();
            info!("NetResultBase: text={cstr}");
        }

        NetResultBaseHook.call(this, string);
    }
}

#[instrument(skip_all)]
fn something_with_goal(this: *mut *mut c_void, a1: usize, mg: *mut MaybeGoal, a3: usize, a4: usize) {
    unsafe {
        if !mg.is_null() {
            let goal = (*mg).name();
            #[cfg(feature = "lan-goal-origin")]
            goal_origin::trace_goal(goal.map(CStr::to_bytes), this as usize);
            info!(
                "GoalQueueTrace: goal={:?} this={:?} owner={:?} a1={:#010x} mg={:?} a3={:#010x} a4={:#010x}",
                goal, //.to_string_lossy(),
                this,
                if this.is_null() { std::ptr::null() } else { *this },
                a1,
                mg,
                a3,
                a4,
            );
        }
        SomethingWithGoalHook.call(this, a1, mg, a3, a4);
    }
}

#[instrument(skip_all)]
fn quazal_stepsequencejob_setstep(step_sequence_job: *mut c_void, step: *mut QuazalStep) {
    unsafe {
        if !step.is_null() && step.is_aligned() && !(*step).description.is_null() {
            let desc = CStr::from_ptr((*step).description);
            info!("Next job step: {} (callback: {:?})", desc.to_string_lossy(), (*step).callback);
        }

        QuazalStepSequenceJobSetStateHook.call(step_sequence_job, step);
    }
}

#[instrument]
fn set_thread_name(worker: *mut c_void) {
    unsafe {
        let name = worker.cast::<i8>().offset(0x18).cast_const();
        let cstr = CStr::from_ptr(name);
        info!("new worker: {:?}", cstr);
        let ostr = OsString::from(cstr.to_string_lossy().into_owned());
        let mut widename: Vec<_> = ostr.encode_wide().collect();
        widename.push(0);
        // The starter may reuse the native name storage. Keep only owned text
        // across it; the thread handle is initialized by the original call.
        ThreadStarterHook.call(worker);
        let thread_handle = HANDLE(*worker.cast::<isize>().offset(1));

        debug!("Thread handle: {:?} Thread id: {}", thread_handle, GetThreadId(thread_handle));

        let _ = SetThreadDescription(thread_handle, PCWSTR::from_raw(widename.as_ptr()));
    }
}

#[instrument(skip_all)]
fn change_state(goal_ptr: *mut MaybeGoal, state_ptr: *mut NetFiniteState, next_state_ptr: *mut NetFiniteStateID) {
    unsafe {
        if let Some(((goal, state), next_state)) = goal_ptr.as_ref().zip(state_ptr.as_ref()).zip(next_state_ptr.as_ref()) {
            let goal_name = goal.name();
            let state_id = state.get_state_id();
            info!(
                "Goal {:?} with {}, state={}(id={:x}), next_state_id={}",
                goal_name,
                goal.unknown,
                state.get_state_name(),
                state_id,
                utils::id_to_name(next_state.id as usize),
            );
        }
        ChangeStateHook.call(goal_ptr, state_ptr, next_state_ptr);
    }
}

#[instrument(skip_all)]
fn net_core(inst: *mut c_void) -> *mut c_void {
    let inst = unsafe { NetCoreHook.call(inst) };
    // unsafe {
    //     let lanmode = inst.cast::<u8>().offset(0x5c8);
    //     assert_eq!(*lanmode, 0);
    //     *lanmode = 1;
    // };
    unsafe {
        NET_CORE_ADDR = Some(inst as hooks_addresses::Address);
    }
    inst
}

fn net_result_core(this: *mut c_void, code: usize, text: *mut GearBasicString) -> *mut c_void {
    unsafe {
        if !text.is_null() && text.is_aligned() && !(*text).internal.is_null() {
            let string2 = &*text;
            let internal = &*string2.internal;
            let cstr = internal.as_str();
            info!("NetResultCore: code={code:x} text={cstr}");
        } else {
            info!("NetResultCore: code={code:x} text=NULL");
        }
    }
    unsafe { NetResultCoreHook.call(this, code, text) }
}

fn net_result_session(this: *mut c_void, code: usize, text: *mut GearBasicString) -> *mut c_void {
    unsafe {
        if !text.is_null() && text.is_aligned() && !(*text).internal.is_null() {
            let string2 = &*text;
            let internal = &*string2.internal;
            let cstr = internal.as_str();
            info!("NetResultSession: code={code:x} text={cstr}");
        } else {
            info!("NetResultSession: code={code:x} text=NULL");
        }
    }
    unsafe { NetResultSessionHook.call(this, code, text) }
}

fn net_result_rdv_session(this: *mut c_void, code: usize, text: *mut GearBasicString) -> *mut c_void {
    unsafe {
        if !text.is_null() && text.is_aligned() && !(*text).internal.is_null() {
            let string2 = &*text;
            let internal = &*string2.internal;
            let cstr = internal.as_str();
            info!("NetResultRdvSession: code={code:x} text={cstr}");
        } else {
            info!("NetResultRdvSession: code={code:x} text=NULL");
        }
    }
    unsafe { NetResultRdvSessionHook.call(this, code, text) }
}

fn net_result_lobby(this: *mut c_void, code: usize, text: *mut GearBasicString) -> *mut c_void {
    unsafe {
        if !text.is_null() && text.is_aligned() && !(*text).internal.is_null() {
            let string2 = &*text;
            let internal = &*string2.internal;
            let cstr = internal.as_str();
            info!("NetResultLobby: code={code:x} text={cstr}");
        } else {
            info!("NetResultLobby: code={code:x} text=NULL");
        }
    }
    unsafe { NetResultLobbyHook.call(this, code, text) }
}

#[instrument(skip_all)]
fn storm_host_port_to_str(this: *mut SomeStormAddrType, x: *mut c_void, y: *mut c_void) -> *mut c_void {
    unsafe {
        let host = (*this).addr;
        let port = (*this).port;
        info!(
            "Storm uses {}.{}.{}.{}:{}",
            host & 0xff,
            (host >> 8) & 0xff,
            (host >> 16) & 0xff,
            (host >> 24) & 0xff,
            port.swap_bytes(),
        );
        StormHostPortToStringHook.call(this, x, y)
    }
}

#[instrument(skip_all)]
fn get_adapters_info(adapter_info: *mut IP_ADAPTER_INFO, sizepointer: *mut u32) -> u32 {
    let res = unsafe { GetAdaptersInfoHook.call(adapter_info, sizepointer) };

    // Windows output is undefined for every failure, not only BUFFER_OVERFLOW.
    if res != ERROR_SUCCESS.0 {
        return res;
    }

    let Some(target) = config::get().and_then(|cfg| cfg.networking.ip_address) else {return res;};
    if sizepointer.is_null() {return ERROR_INVALID_DATA.0;}
    match unsafe {adapter_filter::filter_adapters(adapter_info, sizepointer.read_unaligned() as usize, target)} {
        Ok(true) => debug!("SCBL_LAN_ADAPTER_FILTER selected=true win32_record_bytes=640"),
        Ok(false) => error!("Configured adapter not found; Windows output left unchanged"),
        Err(reason) => {
            error!("SCBL_LAN_ADAPTER_FILTER invalid output: {reason}");
            return ERROR_INVALID_DATA.0;
        }
    }

    res
}

struct SelectedHostEntry {
    entry: HOSTENT,
    address: [u8; 4],
    addresses: [*mut c_char; 2],
}

thread_local! {
    // Match Winsock's per-thread result lifetime, without modifying its storage.
    static SELECTED_HOST_ENTRY: std::cell::RefCell<Option<Box<SelectedHostEntry>>> =
        const { std::cell::RefCell::new(None) };
}

fn clear_selected_host_entry() {
    let _ = SELECTED_HOST_ENTRY.try_with(|slot| { slot.borrow_mut().take(); });
}

unsafe fn select_host_ipv4(ent: *mut HOSTENT, target: std::net::Ipv4Addr) -> *mut HOSTENT {
    if ent.is_null() || (*ent).h_addrtype != 2 || (*ent).h_length != 4 || (*ent).h_addr_list.is_null() {
        return ent;
    }
    let octets = target.octets();
    let mut addresses = (*ent).h_addr_list;
    while !(*addresses).is_null() {
        let address = std::ptr::read_unaligned((*addresses).cast::<[u8; 4]>());
        if address == octets {
            // Names/aliases retain the native result's lifetime. Only the chosen
            // IPv4 and pointer list need owned storage; never write through ent.
            let mut selected = Box::new(SelectedHostEntry {
                entry: std::ptr::read(ent),
                address,
                addresses: [std::ptr::null_mut(); 2],
            });
            selected.addresses[0] = selected.address.as_mut_ptr().cast();
            selected.entry.h_addr_list = selected.addresses.as_mut_ptr();
            let result = &mut selected.entry as *mut HOSTENT;
            // A later thread destructor may call Winsock after our TLS is gone.
            // Keep the native result in that case, never return a dropped copy.
            return SELECTED_HOST_ENTRY.try_with(|slot| {
                *slot.borrow_mut() = Some(selected);
                result
            }).unwrap_or(ent);
        }
        addresses = addresses.add(1);
    }
    ent
}

// No tracing span teardown after a failed lookup: its WSA last-error must survive.
fn gethostbyname(name: *const c_char) -> *mut HOSTENT {
    clear_selected_host_entry();
    let given_host = if name.is_null() { None } else { Some(unsafe { CStr::from_ptr(name) }) };
    info!("called with {:?}", given_host);
    let ent = unsafe { GethostbynameHook.call(name) };
    if ent.is_null() {
        return ent;
    }
    let Some(target) = config::get().and_then(|cfg| cfg.networking.ip_address) else {
        return ent;
    };

    // NULL and empty names are documented local-host requests.
    if let Some(given_host) = given_host.filter(|host| !host.is_empty()) {
        let mut hostname = [0u8; 1024];
        if unsafe { gethostname(&mut hostname) } != 0 {
            error!("error calling gethostname");
            return ent;
        }
        let Ok(hostname) = CStr::from_bytes_until_nul(&hostname) else {
            return ent;
        };
        if hostname != given_host {
            warn!("given host doesn't match {:?}", hostname);
            return ent;
        }
    }
    unsafe { select_host_ipv4(ent, target) }
}

fn generate_id(this: *mut NetFiniteStateID, name_ptr: *const i8, insensitive: bool, b: *mut c_void) {
    let name = unsafe {
        if name_ptr.is_null() {
            None
        } else {
            // Do not pair the output ID with reused input storage after the call.
            CStr::from_ptr(name_ptr).to_str().ok().map(str::to_owned)
        }
    };
    unsafe { GenerateIDHook.call(this, name_ptr, insensitive, b) };

    if let Some(this) = unsafe { this.as_ref() } {
        if let Some(name) = name {
            static CREATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            let mut path = std::env::current_exe().unwrap();
            path.set_file_name("maphashes.txt");
            let file = if CREATED.swap(true, std::sync::atomic::Ordering::AcqRel) {
                std::fs::File::options().append(true).open(path)
            } else {
                std::fs::File::create(path)
            };
            let name = if insensitive { name.to_lowercase() } else { name.into() };
            if let Ok(mut f) = file {
                let _ = writeln!(f, "{name}\t{:x}", this.id);
                let _ = f.flush();
            }
        }
    }
}

#[instrument(skip_all)]
fn storm_set_state(this: *mut StormStateMachine, state_id: *mut NetFiniteStateID) {
    let vtable = unsafe { this.as_ref() }.map(|this| this.vtable);
    let state_name = unsafe { state_id.as_ref() }.map(datatypes::NetFiniteStateID::name);
    info!(
        "Setting next storm state: {} (StateMachine(vtable={:?}))",
        SomeOrQuestionmark(state_name),
        SomeOrQuestionmark(vtable),
    );
    unsafe { StormSetStateHook.call(this, state_id) }
}

#[instrument]
fn storm_statemachineaction_execute(this: *mut StormStateMachineAction, unknown1: *mut *mut StormEvent, unknown2: *mut StormEvent) {
    let (transition, vtable) = unsafe { this.as_ref() }
        .map(|this| (this.callback, unsafe { this.state_machine.as_ref() }.map(|sm| sm.vtable)))
        .unzip();
    let event = unsafe {
        unknown2
            .as_ref()
            .map(|evt| (evt.vtable.global_event)(evt).name)
            .filter(|n| !n.is_null())
            .map(|n| CStr::from_ptr(n))
    };

    info!(
        "Executing transition: {:?} (StateMachine(vtable={:?})) event={:?}",
        SomeOrQuestionmark(transition),
        SomeOrQuestionmark(vtable.flatten()),
        SomeOrQuestionmark(event),
    );

    unsafe { StormStateMachineActionExecuteHook.call(this, unknown1, unknown2) }
}

#[instrument]
fn storm_some_error_formatter(this: *mut c_void, out: *mut GearBasicString) -> *mut GearBasicString {
    let out = unsafe { StormErrorFormatter.call(this, out) };
    if !out.is_null() {
        info!("storm error: {}", unsafe { &*out }.as_str());
    }
    out
}

fn trace_gear_str_destructor(this: *mut GearBasicString) {
    if let Some(c) = unsafe { this.as_ref() } {
        let s = c.as_str();
        if !s.is_empty() {
            info!("~GearStr({s:?})");
        }
    }
}

#[instrument(skip_all)]
fn gear_str_destructor(this: *mut GearBasicString, x: *mut c_void) {
    trace_gear_str_destructor(this);
    unsafe { GearStrDestructor.call(this, x) }
}

#[instrument(skip_all)]
fn another_gear_str_destructor(this: *mut GearBasicString, x: *mut c_void) {
    trace_gear_str_destructor(this);
    unsafe { AnotherGearStrDestructorHook.call(this, x) }
}

// Address tables may name one native destructor twice. One address needs one owner;
// otherwise disabling the second detour can restore the first detour's jump.
fn gear_str_destructor_targets(
    primary: Option<usize>,
    secondary: Option<usize>,
) -> (Option<usize>, Option<usize>) {
    let primary = primary.or(secondary);
    (primary, secondary.filter(|address| Some(*address) != primary))
}

#[instrument(skip_all)]
fn gear_str_constructor(this: *mut GearBasicString, x: *mut c_char) -> *mut GearBasicString {
    if !x.is_null() {
        let s = unsafe { CStr::from_ptr(x.cast_const()) };
        info!("GearStr({s:?})");
    }
    unsafe { SomeGearStrConstructor.call(this, x) }
}

#[cfg(feature = "modding")]
static_detour! {
    static ArcOpenFileHook: unsafe extern "thiscall" fn(*mut c_void, *mut i8) -> usize;
}

#[cfg(feature = "modding")]
static FILE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn is_modded() -> bool {
    #[cfg(feature = "modding")]
    {
        if FILE_COUNTER.load(std::sync::atomic::Ordering::Relaxed) > 1 {
            error!("GAME IS MODDED");
            return true;
        }
    }
    false
}

#[cfg(feature = "modding")]
include!(concat!(env!("OUT_DIR"), "/preload.rs"));

#[cfg(feature = "modding")]
fn check_file_name(fname: &str) -> bool {
    let normalized = fname.to_lowercase().replace('\\', "/");
    if !normalized.starts_with("../../data/") {
        return false;
    }
    let normalized = &normalized[11..];
    for p in PRELOAD {
        if p == normalized {
            return false;
        }
    }

    true
}

#[instrument(skip_all)]
#[cfg(feature = "modding")]
fn arc_open_file(this: *mut c_void, fname: *mut i8) -> usize {
    if !fname.is_null() {
        let cstr = unsafe { CStr::from_ptr(fname.cast_const()) };
        if let Ok(fname) = cstr.to_str() {
            if check_file_name(fname) && std::fs::metadata(fname).is_ok() {
                warn!("Overriding packaged {fname}");
                FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return 0;
            }
        }
    }
    unsafe { ArcOpenFileHook.call(this, fname) }
}

#[cfg(feature = "patch-free")]
static_detour! {
    static GetAddrinfoHook: unsafe extern "stdcall" fn(*const c_char, *const c_char, *const c_void, *mut *mut c_void) -> i32;
}

#[cfg(feature = "patch-free")]
fn getaddrinfo(p_node_name: *const c_char, p_service_name: *const c_char, p_hints: *const c_void, pp_result: *mut *mut c_void) -> i32 {
    // Either name may be omitted. Let Winsock handle these inputs unchanged.
    if p_node_name.is_null() || p_service_name.is_null() {
        return unsafe { GetAddrinfoHook.call(p_node_name, p_service_name, p_hints, pp_result) };
    }
    let node_name = unsafe { CStr::from_ptr(p_node_name) };
    let service_name = unsafe { CStr::from_ptr(p_service_name) };
    info!("getaddrinfo({node_name:?}, {service_name:?}, {p_hints:?}, {pp_result:?})");
    if let Ok("onlineconfigservice.ubi.com") = node_name.to_str() {
        if let Ok("80") = service_name.to_str() {
            if let Some(srv) = config::get().and_then(|cfg| cfg.config_server.as_ref()) {
                if let Ok(srv) = CString::new(srv.clone()) {
                    info!("redirecting onlineconfigservice.ubi.com to {srv:?}");
                    unsafe {
                        return GetAddrinfoHook.call(srv.as_ptr(), p_service_name, p_hints, pp_result);
                    }
                }
            }
        }
    }

    unsafe { GetAddrinfoHook.call(p_node_name, p_service_name, p_hints, pp_result) }
}

unsafe fn hook_with_name<T, F>(hook: &retour::StaticDetour<T>, target: Option<T>, f: F, name: &str)
where
    T: retour::Function,
    F: Fn<T::Arguments, Output = T::Output> + Send + 'static,
    <T as retour::Function>::Arguments: std::marker::Tuple,
{
    let Some(target) = target else {
        error!("Address for hook {name} missing");
        return;
    };
    let res = hook.initialize(target, f).and_then(|h| h.enable());
    if let Err(err) = res {
        error!("Hook {} failed: {:?}", name, err);
    } else {
        info!("Hook {} enabled with address {:?}", name, target.to_ptr());
    }
}

unsafe fn optional_trace_hook_with_name<T, F>(hook: &retour::StaticDetour<T>, target: Option<T>, f: F, name: &str)
where
    T: retour::Function,
    F: Fn<T::Arguments, Output = T::Output> + Send + 'static,
    <T as retour::Function>::Arguments: std::marker::Tuple,
{
    let Some(target) = target else {
        info!("Optional diagnostic hook {name} is unavailable for this game build");
        return;
    };
    let res = hook.initialize(target, f).and_then(|h| h.enable());
    if let Err(err) = res {
        error!("Optional diagnostic hook {} failed: {:?}", name, err);
    } else {
        info!("Optional diagnostic hook {} enabled with address {:?}", name, target.to_ptr());
    }
}

macro_rules! hook {
    ($hook:expr, $addr:expr, $func:ident) => {
        $crate::hooks::hook_with_name(&$hook, $addr.map(|a| unsafe { ::std::mem::transmute(a) }), $func, stringify!($hook));
    };
}

macro_rules! optional_trace_hook {
    ($hook:expr, $addr:expr, $func:ident) => {
        $crate::hooks::optional_trace_hook_with_name(&$hook, $addr.map(|a| unsafe { ::std::mem::transmute(a) }), $func, stringify!($hook));
    };
}

macro_rules! configurable_hook {
    ($config: expr, $cfg: expr, $hook: expr ; $addr: expr => $cb: ident) => {
        if $config.enable_all_hooks || $config.enable_hooks.contains(&$cfg) {
            $crate::hooks::hook!($hook, $addr, $cb);
        }
    };
}

pub unsafe fn init(config: &Config, addr: &Addresses) {
    configurable_hook!(config, Hook::ChangeState, ChangeStateHook ; addr.func_goal_change_state => change_state);
    let (gear_destructor, other_gear_destructor) = gear_str_destructor_targets(
        addr.func_gear_str_destructor,
        addr.func_another_gear_str_destructor,
    );
    configurable_hook!(config, Hook::GearStrDestructor, GearStrDestructor ; gear_destructor => gear_str_destructor);
    configurable_hook!(config, Hook::GearStrDestructor, SomeGearStrConstructor ; addr.func_some_gear_str_constructor => gear_str_constructor);
    if other_gear_destructor.is_some() {
        configurable_hook!(config, Hook::GearStrDestructor, AnotherGearStrDestructorHook ; other_gear_destructor => another_gear_str_destructor);
    }
    configurable_hook!(config, Hook::GenerateID, GenerateIDHook ; addr.func_generate_id => generate_id);
    configurable_hook!(config, Hook::Goal, SomethingWithGoalHook ; addr.func_something_with_goal => something_with_goal);
    configurable_hook!(config, Hook::LeaveState, LeaveStateHook ; addr.func_net_finite_state_leave_state => leave_state);
    configurable_hook!(config, Hook::NetResultBase, NetResultBaseHook ; addr.func_net_result_base => net_result_base);
    configurable_hook!(config, Hook::NetResultCore, NetResultCoreHook ; addr.func_net_result_core => net_result_core);
    configurable_hook!(config, Hook::NetResultLobby, NetResultLobbyHook ; addr.func_net_result_lobby => net_result_lobby);
    configurable_hook!(config, Hook::NetResultRdvSession, NetResultRdvSessionHook ; addr.func_net_result_rdv_session => net_result_rdv_session);
    configurable_hook!(config, Hook::NetResultSession, NetResultSessionHook ; addr.func_net_result_session => net_result_session);
    configurable_hook!(config, Hook::NextState, NextStateHook ; addr.func_net_finite_state_machine_next_state => next_state);
    configurable_hook!(config, Hook::Printer, PrinterHook ; addr.func_printer => printer);
    configurable_hook!(config, Hook::SetStep, QuazalStepSequenceJobSetStateHook ; addr.func_quazal_stepsequencejob_setstep => quazal_stepsequencejob_setstep);
    configurable_hook!(config, Hook::StormErrorFormatter, StormErrorFormatter ; addr.func_storm_some_error_formatter => storm_some_error_formatter);
    configurable_hook!(config, Hook::StormHostPortToString, StormHostPortToStringHook ; addr.func_storm_host_port_to_str => storm_host_port_to_str);
    configurable_hook!(config, Hook::StormSetState, StormSetStateHook ; addr.func_storm_maybe_set_state => storm_set_state);
    configurable_hook!(config, Hook::StormStateMachineActionExecute, StormStateMachineActionExecuteHook ;
        addr.func_storm_statemachineaction_execute => storm_statemachineaction_execute);
    configurable_hook!(config, Hook::Thread, ThreadStarterHook ; addr.func_thread_starter => set_thread_name);
    #[cfg(feature = "modding")]
    configurable_hook!(config, Hook::OverridePackaged, ArcOpenFileHook; addr.func_open_file_from_archive => arc_open_file);

    // always enable these hooks
    hook!(NetCoreHook, addr.func_net_core, net_core);
    hook!(StormJoinAdmissionTraceHook, addr.func_storm_join_admission_trace, storm_join_admission_trace);
    hook!(StormSessionLockTraceHook, addr.func_storm_session_lock_trace, storm_session_lock_trace);
    hook!(StormSessionUnlockTraceHook, addr.func_storm_session_unlock_trace, storm_session_unlock_trace);
    hook!(AsmStateJoinEnterHook, addr.func_state_join_on_enter_trace, asm_state_join_enter);
    hook!(AsmJoinSessionRmcHook, addr.func_join_session_rmc_trace, asm_join_session_rmc);
    hook!(AsmJoinCompletionEventHook, addr.func_join_completion_event_trace, asm_join_completion_event);
    hook!(AsmStateJoinUpdateHook, addr.func_state_join_update_trace, asm_state_join_update);
    hook!(
        AsmStormSessionSnapshotApplyHook,
        addr.func_storm_session_snapshot_apply_trace,
        asm_storm_session_snapshot_apply
    );
    hook!(AsmRdvCompletionRouterHook, addr.func_rdv_completion_router_trace, asm_rdv_completion_router);
    hook!(AsmRdvRouteFilterHook, addr.func_rdv_route_filter_trace, asm_rdv_route_filter);
    hook!(AsmStateJoinResetHook, addr.func_state_join_reset_trace, asm_state_join_reset);
    // These .42 proof hooks are diagnostic-only. Their addresses were never
    // established for every retail executable, so absence is not a runtime error.
    optional_trace_hook!(AsmRdvEventFanoutHook, addr.func_rdv_event_fanout_trace, asm_rdv_event_fanout);
    optional_trace_hook!(AsmRdvListenerFactoryHook, addr.func_rdv_listener_factory_trace, asm_rdv_listener_factory);
    // if config.enable_all_hooks || config.enable_hooks.contains(&Hook::GetAdaptersInfo)
    {
        let lib = LoadLibraryA(s!("iphlpapi.dll")).unwrap();
        let addr = GetProcAddress(lib, s!("GetAdaptersInfo"));
        hook!(GetAdaptersInfoHook, addr, get_adapters_info);
    }
    // if config.enable_all_hooks || config.enable_hooks.contains(&Hook::Gethostbyname)
    {
        let lib = LoadLibraryA(s!("ws2_32.dll")).unwrap();
        let addr = GetProcAddress(lib, s!("gethostbyname"));
        hook!(GethostbynameHook, addr, gethostbyname);
    }
    #[cfg(feature = "patch-free")]
    {
        let lib = LoadLibraryA(s!("ws2_32.dll")).unwrap();
        let addr = GetProcAddress(lib, s!("getaddrinfo"));
        hook!(GetAddrinfoHook, addr, getaddrinfo);
    }

    let state_accept_invite_enter_addr = state_accept_invite_enter_target(addr);
    hook!(StateAcceptInviteEnterHook, state_accept_invite_enter_addr, state_accept_invite_enter);

    storm::init_hooks(config, addr);
    quazal::init_hooks(config, addr);
    info!(
        "SCBL_GAMEPLAY_MODE mode={:?} gameplay_dll={} save_namespace={} network_replication={}",
        config.gameplay_mode,
        config.gameplay_mode == hooks_config::GameplayMode::Mod,
        if config.gameplay_mode == hooks_config::GameplayMode::Mod { "MOD" } else { "Official" },
        network_replication::enabled_for_mode(config.gameplay_mode == hooks_config::GameplayMode::Mod)
    );
    network_replication::init(addr, config.gameplay_mode == hooks_config::GameplayMode::Mod);
    native_dialog::init(addr);
}

macro_rules! disable_configurable_hook {
    ($config:expr, $cfg: expr, $hook: expr) => {
        if $config.enable_all_hooks || $config.enable_hooks.contains(&$cfg) {
            let _ = $hook.disable();
        }
    };
}

pub(crate) use configurable_hook;
pub(crate) use disable_configurable_hook;
pub(crate) use hook;

pub unsafe fn deinit(config: &Config) {
    disable_configurable_hook!(config, Hook::ChangeState, ChangeStateHook);
    disable_configurable_hook!(config, Hook::GearStrDestructor, GearStrDestructor);
    disable_configurable_hook!(config, Hook::GearStrDestructor, AnotherGearStrDestructorHook);
    disable_configurable_hook!(config, Hook::GearStrDestructor, SomeGearStrConstructor);
    disable_configurable_hook!(config, Hook::GenerateID, GenerateIDHook);
    let _ = GetAdaptersInfoHook.disable();
    let _ = GethostbynameHook.disable();
    disable_configurable_hook!(config, Hook::Goal, SomethingWithGoalHook);
    disable_configurable_hook!(config, Hook::LeaveState, LeaveStateHook);
    let _ = NetCoreHook.disable();
    disable_configurable_hook!(config, Hook::NetResultBase, NetResultBaseHook);
    disable_configurable_hook!(config, Hook::NetResultCore, NetResultCoreHook);
    disable_configurable_hook!(config, Hook::NetResultLobby, NetResultLobbyHook);
    disable_configurable_hook!(config, Hook::NetResultRdvSession, NetResultRdvSessionHook);
    disable_configurable_hook!(config, Hook::NetResultSession, NetResultSessionHook);
    disable_configurable_hook!(config, Hook::NextState, NextStateHook);
    disable_configurable_hook!(config, Hook::Printer, PrinterHook);
    disable_configurable_hook!(config, Hook::SetStep, QuazalStepSequenceJobSetStateHook);
    disable_configurable_hook!(config, Hook::StormErrorFormatter, StormErrorFormatter);
    disable_configurable_hook!(config, Hook::StormHostPortToString, StormHostPortToStringHook);
    disable_configurable_hook!(config, Hook::StormSetState, StormSetStateHook);
    disable_configurable_hook!(config, Hook::StormStateMachineActionExecute, StormStateMachineActionExecuteHook);
    disable_configurable_hook!(config, Hook::Thread, ThreadStarterHook);
    #[cfg(feature = "modding")]
    disable_configurable_hook!(config, Hook::OverridePackaged, ArcOpenFileHook);
    let _ = StormJoinAdmissionTraceHook.disable();
    let _ = StormSessionLockTraceHook.disable();
    let _ = StormSessionUnlockTraceHook.disable();
    let _ = AsmStateJoinEnterHook.disable();
    let _ = AsmJoinSessionRmcHook.disable();
    let _ = AsmJoinCompletionEventHook.disable();
    let _ = AsmStateJoinUpdateHook.disable();
    let _ = AsmStormSessionSnapshotApplyHook.disable();
    let _ = AsmRdvCompletionRouterHook.disable();
    let _ = AsmRdvRouteFilterHook.disable();
    let _ = AsmStateJoinResetHook.disable();
    // These hooks are installed independently of the diagnostic hook allowlist.
    let _ = AsmRdvEventFanoutHook.disable();
    let _ = AsmRdvListenerFactoryHook.disable();
    #[cfg(feature = "patch-free")]
    let _ = GetAddrinfoHook.disable();
    let _ = StateAcceptInviteEnterHook.disable();
    storm::deinit_hooks(config);
    quazal::deinit_hooks(config);
    network_replication::deinit();
    native_dialog::deinit();
}

#[cfg(test)]
mod main_hook_cleanup_tests {
    #[test]
    fn always_installed_hooks_have_unconditional_cleanup() {
        let source = include_str!("hooks.rs");
        let teardown = source.split_once("pub unsafe fn deinit(config: &Config) {").unwrap().1
            .split_once("#[cfg(test)]").unwrap().0;
        for name in ["NetCoreHook", "GetAdaptersInfoHook", "GethostbynameHook",
            "AsmRdvEventFanoutHook", "AsmRdvListenerFactoryHook"] {
            assert!(teardown.contains(&format!("let _ = {name}.disable();")), "{name}");
        }
        for name in ["NetCore", "GetAdaptersInfo", "Gethostbyname"] {
            assert!(!teardown.contains(&format!("disable_configurable_hook!(config, Hook::{name},")), "{name}");
        }
        assert!(teardown.contains("#[cfg(feature = \"patch-free\")]\n    let _ = GetAddrinfoHook.disable();"));
    }
}

#[cfg(test)]
mod login_path_regression_tests {
    #[test]
    fn core_settings_request_is_not_detoured_for_diagnostics() {
        let source = include_str!("hooks.rs");
        let forbidden_detour = ["CoreSettings", "RequestHook"].concat();
        let forbidden_probe = ["core_settings", "_request_probe"].concat();

        assert!(!source.contains(&forbidden_detour));
        assert!(!source.contains(&forbidden_probe));
    }
}


#[cfg(test)]
mod join_wait_attempt_lifetime_tests {
    use super::*;

    fn snapshot() -> JoinWaitSnapshot {
        JoinWaitSnapshot {
            service: 0, session_id: Some(7), flag_429: Some(0),
            flag_42a: Some(0), flag_42b: Some(1), flag_42c: Some(0), state_mode: Some(0),
        }
    }

    #[test]
    fn disarm_clears_this_attempt_counter_and_snapshot() {
        let mut owner = 0u8;
        let state = (&mut owner as *mut u8).cast();
        assert_eq!(asm_join_wait_heartbeat(state), 1);
        assert!(asm_should_log_join_wait(state, snapshot()));
        assert!(!asm_should_log_join_wait(state, snapshot()));
        asm_private_join_bridge_disarm(state, "unit-reset");
        assert_eq!(asm_join_wait_heartbeat(state), 1);
        assert!(asm_should_log_join_wait(state, snapshot()));
        asm_private_join_bridge_disarm(state, "unit-cleanup");
    }

    #[test]
    fn disarm_preserves_other_attempt_bookkeeping() {
        let mut owners = [0u8; 2];
        let first = owners.as_mut_ptr().cast();
        let other = unsafe { owners.as_mut_ptr().add(1).cast() };
        asm_private_join_bridge_arm(first, std::ptr::null_mut(), 7);
        asm_private_join_bridge_arm(other, std::ptr::null_mut(), 8);
        assert_eq!(asm_join_wait_heartbeat(first), 1);
        assert_eq!(asm_join_wait_heartbeat(other), 1);
        asm_private_join_bridge_disarm(first, "unit-reset");
        assert_eq!(asm_join_wait_heartbeat(other), 2);
        assert!(ASM_PRIVATE_JOIN_COMPLETION_BRIDGES.get().unwrap().lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()).contains_key(&(other as usize)));
        asm_private_join_bridge_disarm(other, "unit-cleanup");
    }
}


#[cfg(test)]
mod resolver_contract_tests {
    use super::*;

    unsafe fn entry(list: *mut *mut c_char) -> HOSTENT {
        let mut e = std::mem::zeroed::<HOSTENT>();
        e.h_addrtype = 2;
        e.h_length = 4;
        e.h_addr_list = list;
        e
    }

    #[test]
    fn selected_address_does_not_mutate_native_result() {
        unsafe {
            clear_selected_host_entry();
            let mut first = [127u8, 0, 0, 1];
            let mut second = [127u8, 0, 0, 2];
            let mut list = [first.as_mut_ptr().cast(), second.as_mut_ptr().cast(), std::ptr::null_mut()];
            let mut native = entry(list.as_mut_ptr());
            let original_list = list;
            let out = select_host_ipv4(&mut native, std::net::Ipv4Addr::new(127, 0, 0, 2));
            assert_ne!(out, &mut native as *mut HOSTENT);
            assert_eq!(native.h_addr_list, list.as_mut_ptr());
            assert_eq!(list, original_list);
            assert_eq!(std::ptr::read_unaligned((*(*out).h_addr_list).cast::<[u8; 4]>()), second);
            assert!((*(*out).h_addr_list.add(1)).is_null());
            assert_eq!((*out).h_addrtype, 2);
            assert_eq!((*out).h_length, 4);
            clear_selected_host_entry();
        }
    }

    #[test]
    fn absent_target_preserves_native_result() {
        unsafe {
            let mut address = [127u8, 0, 0, 1];
            let mut list = [address.as_mut_ptr().cast(), std::ptr::null_mut()];
            let mut native = entry(list.as_mut_ptr());
            let p = &mut native as *mut HOSTENT;
            assert_eq!(select_host_ipv4(p, std::net::Ipv4Addr::new(127, 0, 0, 2)), p);
            assert_eq!(list[0], address.as_mut_ptr().cast());
        }
    }

    #[test]
    fn unsupported_or_empty_results_are_not_rewritten() {
        unsafe {
            let target = std::net::Ipv4Addr::LOCALHOST;
            assert!(select_host_ipv4(std::ptr::null_mut(), target).is_null());
            let mut native = entry(std::ptr::null_mut());
            let p = &mut native as *mut HOSTENT;
            assert_eq!(select_host_ipv4(&mut native, target), p);
            let mut list = [std::ptr::null_mut()];
            native.h_addr_list = list.as_mut_ptr();
            assert_eq!(select_host_ipv4(&mut native, target), p);
            native.h_addrtype = 23;
            native.h_length = 16;
            assert_eq!(select_host_ipv4(&mut native, target), p);
        }
    }

    #[test]
    fn filtered_results_are_thread_local() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [1u8, 2].into_iter().map(|last| {
            let barrier = barrier.clone();
            std::thread::spawn(move || unsafe {
                let mut address = [127u8, 0, 0, last];
                let mut list = [address.as_mut_ptr().cast(), std::ptr::null_mut()];
                let mut native = entry(list.as_mut_ptr());
                let out = select_host_ipv4(&mut native, std::net::Ipv4Addr::from(address));
                barrier.wait();
                assert_eq!(std::ptr::read_unaligned((*(*out).h_addr_list).cast::<[u8; 4]>()), address);
                assert_eq!(native.h_addr_list, list.as_mut_ptr());
                clear_selected_host_entry();
            })
        }).collect();
        for thread in threads { thread.join().unwrap(); }
    }

    #[test]
    fn destroyed_thread_storage_preserves_native_result() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        struct LateProbe(Arc<AtomicBool>);
        impl Drop for LateProbe {
            fn drop(&mut self) {
                clear_selected_host_entry();
                unsafe {
                    let mut ip = [127u8, 0, 0, 1];
                    let mut list = [ip.as_mut_ptr().cast(), std::ptr::null_mut()];
                    let mut native = entry(list.as_mut_ptr());
                    let p = &mut native as *mut HOSTENT;
                    assert_eq!(select_host_ipv4(p, std::net::Ipv4Addr::LOCALHOST), p);
                }
                self.0.store(true, Ordering::SeqCst);
            }
        }
        thread_local! {
            static EARLY: std::cell::RefCell<Option<LateProbe>> = const { std::cell::RefCell::new(None) };
        }
        let ran = Arc::new(AtomicBool::new(false));
        let observed = ran.clone();
        std::thread::spawn(move || unsafe {
            EARLY.with(|slot| *slot.borrow_mut() = Some(LateProbe(observed)));
            let mut ip = [127u8, 0, 0, 1];
            let mut list = [ip.as_mut_ptr().cast(), std::ptr::null_mut()];
            let mut native = entry(list.as_mut_ptr());
            let out = select_host_ipv4(&mut native, std::net::Ipv4Addr::LOCALHOST);
            assert_ne!(out, &mut native as *mut HOSTENT);
            clear_selected_host_entry();
        }).join().unwrap();
        assert!(ran.load(Ordering::SeqCst));
    }
}


#[cfg(test)]
mod gear_registration_tests {
    use super::gear_str_destructor_targets;

    #[test]
    fn absent_destructors_stay_absent() {
        assert_eq!(gear_str_destructor_targets(None, None), (None, None));
    }

    #[test]
    fn primary_only_keeps_original_slot() {
        assert_eq!(gear_str_destructor_targets(Some(0x10), None), (Some(0x10), None));
    }

    #[test]
    fn secondary_only_gets_an_initialized_primary_owner() {
        assert_eq!(gear_str_destructor_targets(None, Some(0x20)), (Some(0x20), None));
    }

    #[test]
    fn dx9_alias_has_one_owner() {
        assert_eq!(gear_str_destructor_targets(Some(0x4c58a0), Some(0x4c58a0)), (Some(0x4c58a0), None));
    }

    #[test]
    fn dx11_alias_has_one_owner() {
        assert_eq!(gear_str_destructor_targets(Some(0x41f630), Some(0x41f630)), (Some(0x41f630), None));
    }

    #[test]
    fn distinct_destructors_keep_separate_owners() {
        assert_eq!(gear_str_destructor_targets(Some(0x10), Some(0x20)), (Some(0x10), Some(0x20)));
    }

    #[test]
    fn distinct_order_is_not_sorted_or_swapped() {
        assert_eq!(gear_str_destructor_targets(Some(0x20), Some(0x10)), (Some(0x20), Some(0x10)));
    }
}
