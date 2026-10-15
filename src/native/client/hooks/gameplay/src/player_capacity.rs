use std::ffi::c_void;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Mutex;

use hooks_addresses::Addresses;
use retour::static_detour;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

use crate::host_log;

const MOD_MAX_PLAYERS: u32 = 12;
const ACTIVE_LOBBY_SETTINGS_PTR_OFFSET: usize = 0x40c;
const INLINE_LOBBY_SETTINGS_OFFSET: usize = 0x42c;
const SESSION_LOBBY_SETTINGS_OFFSET: usize = 0x594;
const LOBBY_MAX_PLAYERS_OFFSET: usize = 0x20;

#[derive(Clone)]
struct AppliedPatch {
    address: usize,
    original: Vec<u8>,
    replacement: Vec<u8>,
}

static APPLIED_PATCHES: Mutex<Vec<AppliedPatch>> = Mutex::new(Vec::new());

static_detour! {
    static LobbySettingsPlayersHook: unsafe extern "thiscall" fn(*mut c_void) -> usize;
}

fn executable_path() -> Result<PathBuf, String> {
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetModuleFileNameW(HMODULE::default(), &mut buffer) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("host-executable-path-unavailable".to_string());
    }
    buffer.truncate(length);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn FlushInstructionCache(process: *mut c_void, address: *const c_void, size: usize) -> i32;
}

unsafe fn flush_code(address: usize, size: usize) -> bool {
    FlushInstructionCache(GetCurrentProcess(), address as _, size) != 0
}

unsafe fn write_bytes(address: usize, bytes: &[u8]) -> bool {
    let pointer = address as *mut u8;
    let Ok(protection) = region::protect_with_handle(pointer, bytes.len(), region::Protection::READ_WRITE) else {
        return false;
    };
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
    let matches = std::slice::from_raw_parts(address as *const u8, bytes.len()) == bytes;
    drop(protection);
    matches && flush_code(address, bytes.len())
}

unsafe fn lobby_settings_block(owner: *mut c_void) -> *mut u8 {
    let active = std::ptr::read_unaligned(owner.cast::<u8>().add(ACTIVE_LOBBY_SETTINGS_PTR_OFFSET).cast::<*mut u8>());
    if active.is_null() {
        owner.cast::<u8>().add(INLINE_LOBBY_SETTINGS_OFFSET)
    } else {
        active.add(SESSION_LOBBY_SETTINGS_OFFSET)
    }
}

fn lobby_settings_players(owner: *mut c_void) -> usize {
    if !owner.is_null() {
        unsafe {
            let settings = lobby_settings_block(owner);
            let max_players = settings.add(LOBBY_MAX_PLAYERS_OFFSET).cast::<u32>();
            let previous = std::ptr::read_unaligned(max_players);
            if previous != MOD_MAX_PLAYERS {
                std::ptr::write_unaligned(max_players, MOD_MAX_PLAYERS);
                tracing::info!("SCBL_MOD_PLAYER_CAPACITY applied owner={owner:?} settings={settings:?} previous={previous} maximum={MOD_MAX_PLAYERS}");
            }
        }
    }
    unsafe { LobbySettingsPlayersHook.call(owner) }
}

fn validate_patch_plan(addresses: &Addresses) -> Result<(), String> {
    if addresses.func_lobby_settings_players.is_none() {
        return Err("lobby-settings-entry-unavailable".to_string());
    }
    if addresses.lobby_party_capacity_patches.is_empty() {
        return Err("party-capacity-patches-unavailable".to_string());
    }
    for (address, expected, replacement) in &addresses.lobby_party_capacity_patches {
        if expected.is_empty() || expected.len() != replacement.len() {
            return Err(format!("invalid-patch-shape address={address:08X}"));
        }
        let current = unsafe { std::slice::from_raw_parts(*address as *const u8, expected.len()) };
        if current != expected {
            return Err(format!("patch-signature-mismatch address={address:08X}"));
        }
    }
    Ok(())
}

unsafe fn restore_patches_with(
    applied: &mut Vec<AppliedPatch>,
    mut write: impl FnMut(usize, &[u8]) -> bool,
    mut flush: impl FnMut(usize, usize) -> bool,
) -> bool {
    // Remove only an observed successful restore. Unknown/partial/foreign bytes
    // remain untouched with their undo record intact, blocking a new transaction.
    for index in (0..applied.len()).rev() {
        let patch = &applied[index];
        let current = std::slice::from_raw_parts(patch.address as *const u8, patch.original.len());
        let restored = if current == patch.original {
            flush(patch.address, patch.original.len())
        } else if current == patch.replacement {
            write(patch.address, &patch.original)
                && std::slice::from_raw_parts(patch.address as *const u8, patch.original.len()) == patch.original
        } else {
            false
        };
        if restored { applied.remove(index); }
    }
    applied.is_empty()
}

unsafe fn restore_applied_patches() -> bool {
    let Ok(mut applied) = APPLIED_PATCHES.lock() else {
        tracing::warn!("SCBL_MOD_PLAYER_CAPACITY restore=false reason=patch-state-poisoned");
        return false;
    };
    let restored = restore_patches_with(&mut applied, |a, b| write_bytes(a, b), |a, n| flush_code(a, n));
    if !restored {
        tracing::warn!("SCBL_MOD_PLAYER_CAPACITY restore=false pending={} foreign_bytes_untouched=true", applied.len());
    }
    restored
}

pub unsafe fn init() -> Result<(), String> {
    // Keep retail mode limits unless explicitly built for the experimental
    // extension. Do not resolve addresses, install the detour, or patch code.
    // This is a startup policy, not a live-room capacity migration.
    if !cfg!(feature = "extended-player-capacity") {
        host_log(
            hooks_plugin_api::LOG_INFO,
            "SCBL_MOD_PLAYER_CAPACITY enabled=false reason=extension-disabled native_limits=true",
        );
        return Ok(());
    }
    if LobbySettingsPlayersHook.is_enabled() {
        return Err("lobby-settings-hook-already-active".to_string());
    }
    if !restore_applied_patches() {
        return Err("pending-patch-restoration".to_string());
    }
    let path = executable_path()?;
    let addresses = hooks_addresses::get_from_path(&path).map_err(|error| format!("address-resolution-failed error={error}"))?;
    validate_patch_plan(&addresses)?;

    let target = addresses.func_lobby_settings_players.expect("validated lobby settings entry");
    let target = std::mem::transmute::<usize, unsafe extern "thiscall" fn(*mut c_void) -> usize>(target);
    LobbySettingsPlayersHook
        .initialize(target, lobby_settings_players)
        .and_then(|hook| hook.enable())
        .map_err(|error| format!("lobby-settings-hook-failed error={error:?}"))?;

    let mut applied = match APPLIED_PATCHES.lock() {
        Ok(applied) => applied,
        Err(_) => {
            let _ = LobbySettingsPlayersHook.disable();
            return Err("patch-state-poisoned".to_string());
        }
    };
    for (address, expected, replacement) in &addresses.lobby_party_capacity_patches {
        // Hook setup is outside our patch-state lock. Recheck each site before
        // claiming it; preflight alone cannot authorize overwriting new bytes.
        if std::slice::from_raw_parts(*address as *const u8, expected.len()) != expected {
            drop(applied);
            let _ = LobbySettingsPlayersHook.disable();
            restore_applied_patches();
            return Err(format!("patch-signature-changed address={address:08X}"));
        }
        // The writer can fail after changing bytes (including cache sync).
        // Journal the attempted write first so cleanup never loses its owner.
        applied.push(AppliedPatch {
            address: *address,
            original: expected.clone(),
            replacement: replacement.clone(),
        });
        if !write_bytes(*address, replacement) {
            drop(applied);
            let _ = LobbySettingsPlayersHook.disable();
            restore_applied_patches();
            return Err(format!("patch-write-failed address={address:08X}"));
        }
    }
    let patch_count = applied.len();
    drop(applied);
    host_log(
        hooks_plugin_api::LOG_INFO,
        &format!("SCBL_MOD_PLAYER_CAPACITY enabled=true maximum={MOD_MAX_PLAYERS} patches={patch_count}"),
    );
    Ok(())
}

pub unsafe fn deinit() {
    let _ = LobbySettingsPlayersHook.disable();
    restore_applied_patches();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "extended-player-capacity"))]
    #[test]
    fn default_init_preserves_native_capacity_without_hooks() {
        assert!(!LobbySettingsPlayersHook.is_enabled());
        assert!(APPLIED_PATCHES.lock().unwrap().is_empty());
        assert!(unsafe { init() }.is_ok());
        assert!(!LobbySettingsPlayersHook.is_enabled());
        assert!(APPLIED_PATCHES.lock().unwrap().is_empty());
    }

    #[test]
    fn mod_capacity_is_twelve_participants() {
        assert_eq!(MOD_MAX_PLAYERS, 12);
    }

    #[test]
    fn restore_only_matches_the_replacement_bytes() {
        let patch = AppliedPatch {
            address: 0x1234,
            original: vec![0x83, 0xF8, 0x01],
            replacement: vec![0x83, 0xF8, 0x0C],
        };
        assert_eq!(patch.original.len(), patch.replacement.len());
        assert_ne!(patch.original, patch.replacement);
    }
}

#[cfg(test)]
#[path = "player_capacity_transaction_tests.rs"]
mod transaction_tests;
