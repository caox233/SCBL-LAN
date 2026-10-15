use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::OnceLock;

use hooks_config::Config;
use hooks_config::Hook;
use retour::static_detour;
use tracing::info;
use tracing::instrument;

use crate::addresses::Addresses;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RmcMessage {
    protocol_id: u32,
    method_id: u32,
}

// Bound abandoned diagnostic records; native protocol calls never depend on admission.
const MAX_TRACKED_MESSAGES: usize = 4096;
static RMC_MESSAGES: OnceLock<Mutex<HashMap<usize, RmcMessage>>> = OnceLock::new();

const RMC_MAP_STR: &str = include_str!("../../assets/rmc.txt");
type MethodMap = HashMap<u32, &'static str>;
type ProtocolMap = HashMap<u32, (&'static str, MethodMap)>;
static RMC_MAPS: OnceLock<ProtocolMap> = OnceLock::new();

fn parse_rmc_str() -> ProtocolMap {
    RMC_MAP_STR
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut iter = line.split(' ');
            let protocol_id: u32 = iter.next().unwrap().parse().unwrap();
            let method_id: u32 = iter.next().unwrap().parse().unwrap();
            let protocol_name = iter.next().unwrap();
            let method_name = iter.next().unwrap();

            (protocol_id, protocol_name, method_id, method_name)
        })
        .fold(HashMap::new(), |mut acc, (protocol_id, protocol_name, method_id, method_name)| {
            let (_, meth_map) = acc.entry(protocol_id).or_insert((protocol_name, HashMap::new()));
            meth_map.insert(method_id, method_name);
            acc
        })
}

fn resolve_protocol_method(protocol_id: u32, method_id: u32) -> String {
    let map = RMC_MAPS.get_or_init(parse_rmc_str);
    let Some((proto_name, meth_map)) = map.get(&protocol_id) else {
        return format!("{protocol_id}.{method_id}");
    };
    let Some(meth_name) = meth_map.get(&method_id) else {
        return format!("{proto_name}.{method_id}");
    };
    format!("{proto_name}.{meth_name}")
}

impl RmcMessage {
    fn get_for_pointer(ptr: *const c_void) -> Option<std::sync::MappedMutexGuard<'static, RmcMessage>> {
        let map_mutex = RMC_MESSAGES.get_or_init(|| Mutex::new(HashMap::new()));
        // A broken diagnostics lock must not abort the native operation.
        let mut map = map_mutex.lock().ok()?;
        let key = ptr as usize;
        if !map.contains_key(&key) && map.len() >= MAX_TRACKED_MESSAGES { return None; }
        map.entry(key).or_default();
        Some(std::sync::MutexGuard::map(map, |map| map.get_mut(&key).unwrap()))
    }

    fn initialize(ptr: *const c_void, protocol_id: u32) {
        if let Some(mut message) = Self::get_for_pointer(ptr) {
            *message = Self { protocol_id, method_id: 0 };
        }
    }

    fn take_for_pointer(ptr: *const c_void) -> Self {
        // Retire and snapshot under one lock, before logging/native callbacks can
        // reuse the same address. Unknown sends do not allocate a trace entry.
        RMC_MESSAGES.get().and_then(|map| map.lock().ok()?.remove(&(ptr as usize))).unwrap_or_default()
    }

    fn clear() {
        if let Some(map) = RMC_MESSAGES.get() {
            if let Ok(mut map) = map.lock() { map.clear(); }
        }
    }
}

static_detour! {
    static InitMessageHook:  extern "cdecl" fn(*mut c_void, u32, u32);
    static AddMethodIDHook:  extern "cdecl" fn(*mut c_void, u32);
    static SendRMCMessageHook:  extern "thiscall" fn(*mut c_void, *mut c_void, *mut c_void) -> usize;
}

fn init_message_hook(message: *mut c_void, protocol_id: u32, x: u32) {
    RmcMessage::initialize(message.cast_const(), protocol_id);
    info!("AsmRmcInit message={message:?} protocol_id=0x{protocol_id:x} init_arg={x}");
    InitMessageHook.call(message, protocol_id, x);
}

fn add_method_id_hook(message: *mut c_void, method_id: u32) {
    let protocol_id = RmcMessage::get_for_pointer(message.cast_const()).map(|mut rmc_msg| {
        rmc_msg.method_id = method_id;
        rmc_msg.protocol_id
    }).unwrap_or(0);
    info!(
        "AsmRmcMethod message={message:?} protocol_id=0x{protocol_id:x} method_id=0x{method_id:x} resolved={}",
        resolve_protocol_method(protocol_id, method_id)
    );
    AddMethodIDHook.call(message, method_id);
}

#[instrument(skip_all)]
fn send_rmc_message_hook(protocol: *mut c_void, call_ctx: *mut c_void, message: *mut c_void) -> usize {
    let metadata = RmcMessage::take_for_pointer(message.cast_const());
    let resolved = resolve_protocol_method(metadata.protocol_id, metadata.method_id);
    info!("Sending rmc message: {resolved}");
    info!(
        "AsmRmcSend phase=enter protocol={protocol:?} call_ctx={call_ctx:?} message={message:?} protocol_id=0x{:x} method_id=0x{:x} resolved={resolved}",
        metadata.protocol_id, metadata.method_id,
    );
    let result = SendRMCMessageHook.call(protocol, call_ctx, message);
    info!(
        "AsmRmcSend phase=leave protocol={protocol:?} call_ctx={call_ctx:?} message={message:?} protocol_id=0x{:x} method_id=0x{:x} resolved={resolved} result=0x{result:x}",
        metadata.protocol_id, metadata.method_id,
    );
    result
}

pub unsafe fn init_hooks(config: &Config, addr: &Addresses) {
    super::configurable_hook!(config, Hook::RMCMessages, InitMessageHook; addr.func_rmc_init_message => init_message_hook);
    super::configurable_hook!(config, Hook::RMCMessages, AddMethodIDHook; addr.func_rmc_add_method_id => add_method_id_hook);
    super::configurable_hook!(config, Hook::RMCMessages, SendRMCMessageHook; addr.func_rmc_send_message => send_rmc_message_hook);
}

pub unsafe fn deinit_hooks(config: &Config) {
    super::disable_configurable_hook!(config, Hook::RMCMessages, InitMessageHook);
    super::disable_configurable_hook!(config, Hook::RMCMessages, AddMethodIDHook);
    super::disable_configurable_hook!(config, Hook::RMCMessages, SendRMCMessageHook);
    RmcMessage::clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(n:usize)->*const c_void {n as *const c_void}
    #[test] fn reinitialization_resets_method_on_pointer_reuse() {
        RmcMessage::clear();RmcMessage::initialize(p(1),7);
        RmcMessage::get_for_pointer(p(1)).unwrap().method_id=9;
        RmcMessage::initialize(p(1),8);
        assert_eq!(RmcMessage::take_for_pointer(p(1)),RmcMessage{protocol_id:8,method_id:0});
    }
    #[test] fn take_retires_before_next_identity_is_created() {
        RmcMessage::clear();RmcMessage::initialize(p(2),7);
        let first=RmcMessage::take_for_pointer(p(2));RmcMessage::initialize(p(2),8);
        assert_eq!(first.protocol_id,7);assert_eq!(RmcMessage::take_for_pointer(p(2)).protocol_id,8);
    }
    #[test] fn unknown_send_does_not_create_record() {
        RmcMessage::clear();assert_eq!(RmcMessage::take_for_pointer(p(3)),RmcMessage::default());
        assert!(RMC_MESSAGES.get().is_none_or(|m|m.lock().unwrap().is_empty()));
    }
    #[test] fn abandoned_records_are_bounded_but_existing_identity_remains_updatable() {
        RmcMessage::clear();for n in 1..=MAX_TRACKED_MESSAGES {RmcMessage::initialize(p(n),7);}
        assert!(RmcMessage::get_for_pointer(p(MAX_TRACKED_MESSAGES+1)).is_none());
        RmcMessage::initialize(p(1),8);assert_eq!(RmcMessage::take_for_pointer(p(1)).protocol_id,8);
        assert!(RmcMessage::get_for_pointer(p(MAX_TRACKED_MESSAGES+1)).is_some());RmcMessage::clear();
    }
    #[test] fn clear_releases_abandoned_metadata() {
        RmcMessage::clear();RmcMessage::initialize(p(4),7);RmcMessage::clear();
        assert_eq!(RmcMessage::take_for_pointer(p(4)),RmcMessage::default());
    }
    #[test] fn dictionary_and_numeric_fallback_stay_compatible() {
        assert_eq!(resolve_protocol_method(55,1),"PlayerStatsProtocol.WriteStats");
        assert_eq!(resolve_protocol_method(0x7000_0000,7),"1879048192.7");
        assert_eq!(parse_rmc_str().values().map(|(_,methods)|methods.len()).sum::<usize>(),209);
    }
}
