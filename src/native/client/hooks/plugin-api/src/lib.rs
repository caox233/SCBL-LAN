#![no_std]

use core::ffi::c_char;

pub const ABI_VERSION: u32 = 1;
pub const INITIALIZE_EXPORT: &[u8] = b"scbl_plugin_initialize\0";
pub const SHUTDOWN_EXPORT: &[u8] = b"scbl_plugin_shutdown\0";
pub const HANDLE_EVENT_EXPORT: &[u8] = b"scbl_plugin_handle_event\0";
pub const VALIDATE_EXPORT: &[u8] = b"scbl_plugin_validate\0";
pub const RESUME_EXPORT: &[u8] = b"scbl_plugin_resume\0";
pub const EVENT_NATIVE_SAVE_OPENED: u32 = 1;

pub const FEATURE_WEAPON_OBSTRUCTION_FIX: u64 = 1 << 0;
pub const FEATURE_EXTENDED_LOADOUTS: u64 = 1 << 1;
pub const FEATURE_HIDE_EXTENDED_LOADOUTS_IN_MATCH: u64 = 1 << 2;
pub const FEATURE_INTEL_SUIT_DISRUPTOR_IMMUNITY: u64 = 1 << 3;
pub const FEATURE_MERC_SPRINT_VERTICAL_LOOK: u64 = 1 << 4;
// Every completed, player-facing feature carried by scbl_gameplay.dll.
pub const GAMEPLAY_FEATURE_BITS_DEFAULT: u64 = FEATURE_WEAPON_OBSTRUCTION_FIX
    | FEATURE_EXTENDED_LOADOUTS
    | FEATURE_HIDE_EXTENDED_LOADOUTS_IN_MATCH
    | FEATURE_INTEL_SUIT_DISRUPTOR_IMMUNITY
    | FEATURE_MERC_SPRINT_VERTICAL_LOOK;

pub const LOG_INFO: u32 = 2;
pub const LOG_WARNING: u32 = 3;
pub const LOG_ERROR: u32 = 4;

pub type HostLogFn =
    unsafe extern "system" fn(level: u32, message: *const c_char, message_len: u32);

#[repr(C)]
pub struct HostApiV1 {
    pub abi_version: u32,
    pub struct_size: u32,
    pub log: Option<HostLogFn>,
}

#[repr(C)]
pub struct PluginConfigV1 {
    pub abi_version: u32,
    pub struct_size: u32,
    pub feature_bits: u64,
    pub world_raycast: u32,
    pub fov_factor_bits: u32,
    pub loadout_sidecar_path: *const u16,
    pub loadout_sidecar_path_len: u32,
    pub reserved2: u32,
}

pub type InitializeFn =
    unsafe extern "system" fn(host: *const HostApiV1, config: *const PluginConfigV1) -> i32;
pub type ValidateFn =
    unsafe extern "system" fn(host: *const HostApiV1, config: *const PluginConfigV1) -> i32;
pub type ResumeFn =
    unsafe extern "system" fn(host: *const HostApiV1, config: *const PluginConfigV1) -> i32;
pub type ShutdownFn = unsafe extern "system" fn();
pub type HandleEventFn = unsafe extern "system" fn(event: u32);

pub fn host_api_compatible(host: &HostApiV1) -> bool {
    host.abi_version == ABI_VERSION
        && host.struct_size as usize >= core::mem::size_of::<HostApiV1>()
}

pub fn plugin_config_compatible(config: &PluginConfigV1) -> bool {
    config.abi_version == ABI_VERSION
        && config.struct_size as usize >= core::mem::size_of::<PluginConfigV1>()
}

/// Optional drawing ABI; no change to the plugin lifecycle ABI.
pub mod hud;

#[cfg(test)]
mod abi_contract_tests;
