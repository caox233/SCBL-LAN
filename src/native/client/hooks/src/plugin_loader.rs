use std::ffi::c_char;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::OnceLock;

use hooks_config::Config;
use hooks_config::GameplayMode;
use hooks_plugin_api::hud::{HudFrame, HudSnapshotFn, HUD_EXPORT};
use hooks_plugin_api::HandleEventFn;
use hooks_plugin_api::HostApiV1;
use hooks_plugin_api::InitializeFn;
use hooks_plugin_api::PluginConfigV1;
use hooks_plugin_api::ShutdownFn;
use hooks_plugin_api::ABI_VERSION;
use hooks_plugin_api::GAMEPLAY_FEATURE_BITS_DEFAULT;
use hooks_plugin_api::HANDLE_EVENT_EXPORT;
use hooks_plugin_api::INITIALIZE_EXPORT;
use hooks_plugin_api::LOG_ERROR;
use hooks_plugin_api::LOG_WARNING;
use hooks_plugin_api::SHUTDOWN_EXPORT;
use tracing::error;
use tracing::info;
use tracing::warn;
use windows::core::PCSTR;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::Win32::System::LibraryLoader::LoadLibraryW;

struct LoadedPlugin {
    _module: HMODULE,
    shutdown: ShutdownFn,
    handle_event: Option<HandleEventFn>,
    hud_snapshot: Option<HudSnapshotFn>,
}

unsafe impl Send for LoadedPlugin {}

static LOADED_PLUGINS: OnceLock<Mutex<Vec<LoadedPlugin>>> = OnceLock::new();
static BUILTIN_AUDIO_STATE: AtomicU8 = AtomicU8::new(0);

unsafe extern "system" {
    fn scbl_audio_builtin_initialize(host: *const HostApiV1, config: *const PluginConfigV1) -> i32;
    fn scbl_audio_builtin_shutdown();
}

unsafe extern "system" fn host_log(level: u32, message: *const c_char, message_len: u32) {
    if message.is_null() || message_len == 0 {
        return;
    }
    let Ok(length) = usize::try_from(message_len) else {
        return;
    };
    let bytes = std::slice::from_raw_parts(message.cast::<u8>(), length);
    let text = String::from_utf8_lossy(bytes);
    match level {
        LOG_ERROR => error!("{text}"),
        LOG_WARNING => warn!("{text}"),
        _ => info!("{text}"),
    }
}

fn gameplay_feature_bits(mode: GameplayMode) -> u64 {
    if mode == GameplayMode::Mod {
        GAMEPLAY_FEATURE_BITS_DEFAULT
    } else {
        0
    }
}

fn gameplay_requested(mode: GameplayMode) -> bool {
    mode == GameplayMode::Mod
}

fn plugin_directory(default_directory: &Path) -> PathBuf {
    std::env::var_os("SCBL_LAN_PLUGIN_DIR").map_or_else(|| default_directory.to_path_buf(), PathBuf::from)
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn plugin_config(config: &Config, world_raycast: Option<usize>) -> (PluginConfigV1, Vec<u16>) {
    let loadout_sidecar_path = crate::uplay_r1_loader::save::extended_loadout_sidecar_path().unwrap_or_default();
    let loadout_sidecar_path: Vec<u16> = loadout_sidecar_path.as_os_str().encode_wide().collect();
    let plugin_config = PluginConfigV1 {
        abi_version: ABI_VERSION,
        struct_size: u32::try_from(std::mem::size_of::<PluginConfigV1>()).unwrap_or(u32::MAX),
        feature_bits: gameplay_feature_bits(config.gameplay_mode),
        world_raycast: world_raycast.and_then(|address| u32::try_from(address).ok()).unwrap_or(0),
        fov_factor_bits: config.features.fov_factor.to_bits(),
        loadout_sidecar_path: loadout_sidecar_path.as_ptr(),
        loadout_sidecar_path_len: u32::try_from(loadout_sidecar_path.len()).unwrap_or(0),
        reserved2: 0,
    };
    (plugin_config, loadout_sidecar_path)
}

pub unsafe fn ensure_builtin_audio_initialized() {
    if BUILTIN_AUDIO_STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return;
    }
    let Some(config) = crate::config::get() else {
        BUILTIN_AUDIO_STATE.store(3, Ordering::Release);
        warn!("SCBL_AUDIO_BUILTIN loaded=false reason=config-unavailable");
        return;
    };
    let addresses = crate::addresses::get();
    let (plugin_config, _path_backing) = plugin_config(config, addresses.func_world_raycast);
    let host = HostApiV1 {
        abi_version: ABI_VERSION,
        struct_size: u32::try_from(std::mem::size_of::<HostApiV1>()).unwrap_or(u32::MAX),
        log: Some(host_log),
    };
    let result = scbl_audio_builtin_initialize(&host, &plugin_config);
    if result == 0 {
        if BUILTIN_AUDIO_STATE.compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            info!("SCBL_AUDIO_BUILTIN loaded=true abi={ABI_VERSION} lifecycle=uplay-startup");
        } else {
            // Shutdown claimed state 1 without waiting under the loader lock.
            // The initializing thread owns the unpublished native object and
            // must issue its signal-only shutdown instead of resurrecting it.
            scbl_audio_builtin_shutdown();
            info!("SCBL_AUDIO_BUILTIN loaded=false reason=shutdown-during-initialize");
        }
    } else {
        BUILTIN_AUDIO_STATE.store(3, Ordering::Release);
        warn!("SCBL_AUDIO_BUILTIN loaded=false result={result} fallback=retail-xaudio27");
    }
}

unsafe fn load_plugin(plugin_directory: &Path, file_name: &str, config: &PluginConfigV1) -> Result<LoadedPlugin, String> {
    let path = plugin_directory.join(OsStr::new(file_name));
    if !path.is_file() {
        return Err(format!("missing path={}", path.display()));
    }
    let wide = wide_path(&path);
    let module = LoadLibraryW(PCWSTR(wide.as_ptr())).map_err(|error| format!("load-failed path={} error={error}", path.display()))?;
    crate::release_identity::capture_loaded_gameplay(module)
        .map_err(|error| format!("module-identity-unavailable path={} error={error}", path.display()))?;
    let initialize_address = GetProcAddress(module, PCSTR(INITIALIZE_EXPORT.as_ptr()));
    let shutdown_address = GetProcAddress(module, PCSTR(SHUTDOWN_EXPORT.as_ptr()));
    let event_address = GetProcAddress(module, PCSTR(HANDLE_EVENT_EXPORT.as_ptr()));
    let hud_address = GetProcAddress(module, PCSTR(HUD_EXPORT.as_ptr()));
    let (Some(initialize_address), Some(shutdown_address)) = (initialize_address, shutdown_address) else {
        return Err(format!("exports-missing path={}", path.display()));
    };
    let initialize: InitializeFn = std::mem::transmute(initialize_address);
    let shutdown: ShutdownFn = std::mem::transmute(shutdown_address);
    let handle_event = event_address.map(|address| std::mem::transmute(address));
    let hud_snapshot = hud_address.map(|address| std::mem::transmute(address));
    let host = HostApiV1 {
        abi_version: ABI_VERSION,
        struct_size: u32::try_from(std::mem::size_of::<HostApiV1>()).unwrap_or(u32::MAX),
        log: Some(host_log),
    };
    let result = initialize(&host, config);
    if result != 0 {
        return Err(format!("initialize-failed path={} result={result}", path.display()));
    }
    info!("SCBL_PLUGIN_HUD available={} abi=1", hud_snapshot.is_some());
    Ok(LoadedPlugin {
        _module: module,
        shutdown,
        handle_event,
        hud_snapshot,
    })
}

pub unsafe fn init(config: &Config, world_raycast: Option<usize>, default_directory: &Path) -> Result<(), String> {
    let bits = gameplay_feature_bits(config.gameplay_mode);
    let (plugin_config, _path_backing) = plugin_config(config, world_raycast);
    let plugin_directory = plugin_directory(default_directory);
    let mut requested = Vec::new();
    if gameplay_requested(config.gameplay_mode) {
        requested.push(("gameplay", "scbl_gameplay.dll"));
    }
    let loaded_plugins = LOADED_PLUGINS.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut loaded_plugins) = loaded_plugins.lock() else {
        error!("SCBL_PLUGIN_LAYER enabled=false reason=state-lock-poisoned");
        return Err("Gameplay plugin state lock unavailable".into());
    };
    for (kind, file_name) in requested {
        match load_plugin(&plugin_directory, file_name, &plugin_config) {
            Ok(plugin) => {
                loaded_plugins.push(plugin);
                info!("SCBL_PLUGIN_LAYER kind={kind} loaded=true abi={ABI_VERSION} feature_bits=0x{bits:016X}");
            }
            Err(reason) => {
                return Err(format!("SCBL_PLUGIN_LAYER kind={kind} loaded=false reason={reason}"));
            }
        }
    }
    Ok(())
}

// Keep the loaded-plugin lease only while copying the POD frame. Rendering must
// neither wait on plugin shutdown nor retain a callback beyond this lease.
fn snapshot_from(plugins: &Mutex<Vec<LoadedPlugin>>) -> Option<HudFrame> {
    let plugins = plugins.try_lock().ok()?;
    for plugin in plugins.iter() {
        let Some(snapshot) = plugin.hud_snapshot else { continue };
        let mut frame = HudFrame::default();
        let result = unsafe { snapshot(&mut frame, std::mem::size_of::<HudFrame>() as u32) };
        if result == 0 && frame.valid() {
            return Some(frame);
        }
    }
    None
}

pub(crate) fn hud_snapshot() -> Option<HudFrame> {
    snapshot_from(LOADED_PLUGINS.get()?)
}

pub fn notify(event: u32) {
    let Some(loaded_plugins) = LOADED_PLUGINS.get() else {
        return;
    };
    let Ok(loaded_plugins) = loaded_plugins.lock() else {
        return;
    };
    for plugin in loaded_plugins.iter() {
        if let Some(handle_event) = plugin.handle_event {
            unsafe { handle_event(event) };
        }
    }
}

pub unsafe fn deinit() {
    if let Some(loaded_plugins) = LOADED_PLUGINS.get() {
        if let Ok(mut loaded_plugins) = loaded_plugins.lock() {
            for plugin in loaded_plugins.drain(..).rev() {
                (plugin.shutdown)();
            }
        }
    }
    if BUILTIN_AUDIO_STATE.swap(3, Ordering::AcqRel) == 2 {
        // The native shutdown entry only signals process-lifetime workers; it
        // does not wait or unload while DllMain holds the loader lock.
        scbl_audio_builtin_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_mode_is_the_single_gameplay_layer_switch() {
        assert!(gameplay_requested(GameplayMode::Mod));
        assert_eq!(gameplay_feature_bits(GameplayMode::Mod), GAMEPLAY_FEATURE_BITS_DEFAULT);
        assert!(!gameplay_requested(GameplayMode::Official));
        assert_eq!(gameplay_feature_bits(GameplayMode::Official), 0);
    }

    #[test]
    fn hooks_build_only_the_static_builtin_xaudio_layer() {
        let build_script = include_str!("../build.rs");
        assert!(build_script.contains("scbl_audio_builtin"));
        let audio_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("audio");
        let cmake = std::fs::read_to_string(audio_root.join("CMakeLists.txt")).unwrap();
        assert!(cmake.contains("add_library(scbl_audio_builtin STATIC"));
        assert!(!cmake.contains("add_library(scbl_audio SHARED"));
        assert!(audio_root.join("src/plugin.cpp").exists());
    }

    unsafe extern "system" fn test_shutdown() {}
    unsafe extern "system" fn test_hud(out: *mut HudFrame, size: u32) -> i32 {
        assert_eq!(size, std::mem::size_of::<HudFrame>() as u32);
        let mut frame = HudFrame::default();
        frame.count = 1;
        frame.rects[0] = hooks_plugin_api::hud::HudRect { x: 0.1, y: 0.2, w: 0.3, h: 0.01, rgba: 0x7fd3df91 };
        out.write(frame);
        0
    }
    unsafe extern "system" fn invalid_hud(out: *mut HudFrame, size: u32) -> i32 {
        test_hud(out, size); (*out).count = 9; 0
    }
    unsafe extern "system" fn busy_hud(out: *mut HudFrame, size: u32) -> i32 {
        test_hud(out, size); -2
    }
    fn owned_hud_plugin(snapshot: Option<HudSnapshotFn>) -> LoadedPlugin {
        LoadedPlugin { _module: HMODULE::default(), shutdown: test_shutdown, handle_event: None, hud_snapshot: snapshot }
    }
    #[test]
    fn stamina_hud_optional_callback_copies_only_successful_valid_frames() {
        for callback in [None, Some(invalid_hud as HudSnapshotFn), Some(busy_hud as HudSnapshotFn)] {
            assert!(snapshot_from(&Mutex::new(vec![owned_hud_plugin(callback)])).is_none());
        }
        let plugins=Mutex::new(vec![owned_hud_plugin(Some(test_hud))]);
        let frame=snapshot_from(&plugins).unwrap();
        assert_eq!(frame.count,1);assert_eq!(frame.rects[0].rgba,0x7fd3df91);
        assert!(snapshot_from(&Mutex::new(Vec::new())).is_none());
    }
    #[test]
    fn stamina_hud_busy_or_retired_plugin_does_not_block_or_reuse_a_frame() {
        let plugins=Mutex::new(vec![owned_hud_plugin(Some(test_hud))]);
        let mut lease=plugins.lock().unwrap();
        assert!(snapshot_from(&plugins).is_none());
        lease.clear();drop(lease);
        assert!(snapshot_from(&plugins).is_none());
    }
}
