#![feature(unboxed_closures, tuple_trait, c_variadic, once_cell_try, mapped_lock_guards)]
#![deny(clippy::pedantic)]

use std::ffi::c_char;
use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::File;
use std::os::raw::c_void;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

use addresses::Addresses;
use hooks_config as config;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::instrument;
use tracing::level_filters::LevelFilter;
use windows::core::PCSTR;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::SystemServices::DLL_PROCESS_DETACH;
use windows::Win32::System::SystemServices::DLL_THREAD_ATTACH;
use windows::Win32::System::SystemServices::DLL_THREAD_DETACH;
use windows::Win32::UI::WindowsAndMessaging::MessageBoxA;
use windows::Win32::UI::WindowsAndMessaging::MB_OK;

// The launcher checks this marker before deploying the DLL. A file-version match
// is not sufficient because an older Hooks build can share the same product
// version while still expecting the retired 5th_auth.dat contract.
#[used]
#[no_mangle]
pub static SCBL_HOOKS_CONFIG_PROTOCOL: [u8; 31] = *b"SCBL_HOOKS_CONFIG=scbl.toml.v1\0";

// Local test overrides are accepted only when this embedded Cargo version is
// not older than the signed test-channel Hooks baseline. Stable never loads a
// local override.
#[used]
#[no_mangle]
pub static SCBL_HOOKS_COMPONENT_VERSION: &str = concat!("SCBL_HOOKS_VERSION=", env!("CARGO_PKG_VERSION"), "\0");

// The SCBL launcher sets this process-scoped marker immediately before it
// starts the game. If our proxy DLL is left installed and the game is later
// started by the retail launcher, every Uplay export must be passed through to
// Ubisoft's original loader without loading SCBL configuration or game hooks.
static RETAIL_PASSTHROUGH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn scbl_launch_marker_present(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

pub(crate) fn retail_passthrough() -> bool {
    RETAIL_PASSTHROUGH.load(std::sync::atomic::Ordering::Acquire)
}

mod addresses;
mod api;
mod release_identity;
mod dll_utils;
mod fusion_compat;
mod input_mouse;
mod hooks;
mod macros;
mod overlay;
mod plugin_loader;
mod uplay_r1_loader;

use macros::fatal_error;

unsafe fn writemem(ptr: *mut u8, data: &[u8]) {
    if let Ok(_handle) = region::protect_with_handle(ptr, data.len(), region::Protection::READ_WRITE) {
        std::ptr::copy(data.as_ptr(), ptr, data.len());
    }
}

unsafe fn patch_url(new_server: &str, addrs: &Addresses) {
    let Ok(new_server_cstr) = CString::new(new_server) else {
        fatal_error!("Invalid config_server");
    };
    let g_cfg_client = addrs.global_onlineconfig_client as *mut *mut *mut u8;
    if g_cfg_client.is_null() {
        fatal_error!(
            "Couldn't adjust config server";
            "Couldn't adjust config server: g_cfg_client is null"
        );
    }
    let g_cfg_client = *g_cfg_client;
    info!("Global onlineconfig client: {:?}", g_cfg_client);
    if g_cfg_client.is_null() {
        info!("Patching rodata with {}", new_server);
        writemem(addrs.onlineconfig_url as *mut u8, new_server_cstr.as_bytes_with_nul());
        return;
    }
    let hostname_ptr = g_cfg_client.add(0x24);
    info!("->hostname: {:?}", hostname_ptr);
    if hostname_ptr.is_null() {
        fatal_error!(
            "Couldn't adjust config server";
            "Couldn't adjust config server: g_cfg_client->hostname is null"
        );
    }
    let hostname_ptr = *hostname_ptr;
    info!("hostname: {:?}", hostname_ptr);
    if hostname_ptr.is_null() {
        fatal_error!(
            "Couldn't adjust config server";
            "Couldn't adjust config server: *g_cfg_client->hostname is null"
        );
    }
    info!("Patching heap with {}", new_server);
    writemem(hostname_ptr, new_server_cstr.as_bytes_with_nul());
}

#[link(name = "legacy_stdio_definitions")]
extern "C" {
    #[cfg(all(target_family = "windows", target_env = "msvc"))]
    fn vsnprintf(buffer: *mut c_char, count: usize, format: *const c_char, args: std::ffi::VaList<'_, '_>) -> i32;
}

unsafe extern "C" fn debug_print(fmt: *const c_char, mut args: ...) {
    let mut buf: Vec<u8> = vec![0; 4096];

    // A va_list must go to the v-form; forwarding it as one variadic argument
    // formats its pointer instead of the caller's values. Keep the original
    // list untouched for the possible second pass.
    let mut required = usize::try_from(vsnprintf(buf.as_mut_ptr().cast(), buf.len(), fmt, args.clone().as_va_list()))
        .expect("debug_print formatting failed");
    if required >= buf.len() {
        // The CRT count excludes the NUL. reserve() alone would leave len()
        // unchanged and make the later diagnostic slice invalid.
        buf.resize(required.checked_add(1).expect("debug_print length overflow"), 0);
        required = usize::try_from(vsnprintf(buf.as_mut_ptr().cast(), buf.len(), fmt, args.as_va_list()))
            .expect("debug_print formatting failed");
        assert!(required < buf.len(), "debug_print formatted length changed during retry");
    }

    let formatted = CStr::from_bytes_with_nul(&buf[..=required]);

    // This optional fault diagnostic remains deliberately fatal.
    panic!("{formatted:?}");
}

unsafe fn enable_debug_print(addrs: &Addresses) {
    let Some((func_ptr, ref to_nop)) = addrs.debug_print else {
        return;
    };

    let target = func_ptr as *mut unsafe extern "C" fn(*const c_char, ...);

    *target = debug_print;

    for addr in to_nop {
        writemem(addr.start as *mut u8, &vec![0xccu8; addr.end - addr.start]);
        // writemem(addr.start as *mut u8, &vec![0x90u8; addr.end - addr.start]);
    }
}

#[instrument]
fn check_orig_library(hmodule: Option<HMODULE>) {
    let myself = get_dll_path(hmodule).unwrap_or_else(|error| {
        fatal_error!("Couldn't locate the hooks DLL: {error}");
    });
    let orig = myself.with_file_name("uplay_r1_loader.orig.dll");

    debug!("Myself: {:?}", myself);
    debug!("Original library path: {:?}", orig);
    if !orig.exists() {
        fatal_error!("Original library not found at {orig:?}");
    }

    let my_info = dll_utils::get_dll_info(&myself).unwrap_or_else(|error| {
        fatal_error!("Couldn't read hooks DLL metadata at {myself:?}: {error}");
    });
    let orig_info = dll_utils::get_dll_info(&orig).unwrap_or_else(|error| {
        fatal_error!("Couldn't read original DLL metadata at {orig:?}: {error}");
    });
    info!("My info: {:?}", my_info);
    info!("Orig info: {:?}", orig_info);

    // Neither upstream nor CN proxy may be used as its own original loader.
    if matches!(orig_info.company.as_deref(), Some("5th Echelon" | "5th Echelon CN")) {
        fatal_error!("uplay_r1_loader.orig.dll is invalid: Not Ubisoft version");
    }
}

#[instrument(skip_all)]
fn init(hmodule: Option<HMODULE>) {
    // Only capture our loaded module path here; no catalog I/O or network under DllMain.
    release_identity::capture_loaded_main(hmodule);
    let launch_marker = std::env::var_os("SCBL_LAN_CLIENT_DATA_DIR");
    if !scbl_launch_marker_present(launch_marker.as_deref()) {
        RETAIL_PASSTHROUGH.store(true, std::sync::atomic::Ordering::Release);
        return;
    }

    let dir = get_target_dir(hmodule);
    let reload_handle = init_log(&dir);
    check_orig_library(hmodule);
    if let Some(cmdline) = get_arguments() {
        info!("Cmdline: {}", cmdline);
    }
    let path = config::get_config_path(&dir);
    info!("Config path={:?}", path);
    let config = match config::get_or_load(path) {
        Err(e) => {
            fatal_error!("Config couldn't be loaded: {e}");
        }
        Ok(cfg) => cfg,
    };

    let level = match config.logging.level {
        config::LogLevel::Trace => tracing_subscriber::filter::LevelFilter::TRACE,
        config::LogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
        config::LogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
        config::LogLevel::Warning => tracing_subscriber::filter::LevelFilter::WARN,
        config::LogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
    };
    reload_handle.reload(tracing_subscriber::EnvFilter::default().add_directive(level.into())).unwrap();

    let addr = addresses::get();

    unsafe {
        hooks::init(config, &addr);
        // FOV is a core presentation preference shared by Official and MOD.
        // It must not depend on the optional gameplay DLL being loaded.
        fusion_compat::init(config.features.fov_factor);
        if let Some(world) = addr.func_world_raycast {
            input_mouse::init(world);
        }
        if let Err(reason) = plugin_loader::init(config, addr.func_world_raycast, &dir) {
            fatal_error!("SCBL LAN MOD initialization failed: {reason}");
        }
        if let Some(config_server) = config.config_server.as_ref() {
            #[cfg(not(feature = "patch-free"))]
            patch_url(config_server, &addr);
        } else {
            info!("Keeping original config server");
        }

        if !config.internal_command_line.is_empty() {
            if let Some(ptr) = addr.unreal_commandline {
                let ptr = ptr as *mut u8;
                let count = if config.internal_command_line.len() > 11 {
                    11
                } else {
                    config.internal_command_line.len()
                };
                ptr.copy_from_nonoverlapping(config.internal_command_line.as_ptr(), count);
            } else {
                error!("Can't set command line. Address is missing");
            }
        }

        enable_debug_print(&addr);
    }

    // needs to be done in a separate thread, otherwise it'll block indefinitely
    if let Err(error) = std::thread::Builder::new()
        .name(String::from("login-thread"))
        .spawn(move || {
            // try to login once. relogins are attempted by the update thread later on
            let _ = api::login(&config.user.username, &config.user.password);
        })
    {
        // The process-owned update service retains its reconnect/relogin path.
        // Do not unwind through DllMain for a recoverable worker creation error.
        error!("Initial login thread unavailable: {error}; API maintenance will retry login");
    }
}

fn deinit(hmodule: Option<HMODULE>) {
    if retail_passthrough() {
        return;
    }

    // disable tracing as TLS is already destroyed at this point.
    // No tracing calls/instruments are allowed before this point!!
    let _guard = tracing::dispatcher::set_default(&tracing::dispatcher::Dispatch::none());
    let dir = get_target_dir(hmodule);
    let path = config::get_config_path(dir);
    info!("Config path={:?}", path);
    let config = match config::get_or_load(path) {
        Err(e) => {
            fatal_error!("Config couldn't be loaded: {e}");
        }
        Ok(cfg) => cfg,
    };
    unsafe {
        input_mouse::deinit();
        fusion_compat::deinit();
        hooks::deinit(config);
        plugin_loader::deinit();
    }
}

struct FileWriter(PathBuf);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FileWriter {
    type Writer = tracing_subscriber::fmt::writer::EitherWriter<File, std::io::Sink>;

    fn make_writer(&'a self) -> Self::Writer {
        // A log path may disappear or become locked while the game is running.
        // Do not panic (or log recursively) from the logging infrastructure.
        // Retry on the next event so a temporary failure does not disable logs.
        match File::options().create(true).append(true).open(&self.0) {
            Ok(file) => tracing_subscriber::fmt::writer::EitherWriter::A(file),
            Err(_) => tracing_subscriber::fmt::writer::EitherWriter::B(std::io::sink()),
        }
    }
}

fn show_msgbox(msg: &str, caption: &str) {
    let msg = CString::new(msg).unwrap();
    let caption = CString::new(caption).unwrap();
    unsafe {
        MessageBoxA(None, PCSTR(msg.as_ptr().cast::<u8>()), PCSTR(caption.as_ptr().cast::<u8>()), MB_OK);
    }
}

fn get_executable(hinst: HMODULE) -> Option<PathBuf> {
    let mut path = vec![0u16; 4096];
    let sz = unsafe { GetModuleFileNameW(hinst, &mut path) } as usize;
    let err = std::io::Error::last_os_error();
    if err.raw_os_error().unwrap_or_default() != 0 {
        return None;
    };
    let path = OsString::from_wide(&path[..sz]);
    // cannot fail
    Some(PathBuf::from(path))
}

fn get_arguments() -> Option<String> {
    let cmdline = unsafe { windows::Win32::System::Environment::GetCommandLineW().to_string() };
    cmdline.ok()
}

fn get_dll_path(hinst: Option<HMODULE>) -> anyhow::Result<PathBuf> {
    hinst.ok_or(anyhow::anyhow!("no hinst")).and_then(|hinst| {
        let mut path = vec![0u16; 4096];
        let sz = unsafe { GetModuleFileNameW(hinst, &mut path) } as usize;
        let err = std::io::Error::last_os_error();
        if err.raw_os_error().unwrap_or_default() != 0 {
            return Err(err.into());
        };
        let path = OsString::from_wide(&path[..sz]);
        // cannot fail
        let path = PathBuf::from(path);
        Ok(path)
    })
}

fn get_target_dir(hinst: Option<HMODULE>) -> PathBuf {
    get_dll_path(hinst).map_or_else(
        |_| std::env::current_dir().unwrap_or_default(),
        |mut p| {
            p.pop();
            p
        },
    )
}

type ReconfigurableLogger = tracing_subscriber::reload::Handle<
    tracing_subscriber::EnvFilter,
    tracing_subscriber::layer::Layered<
        tracing_subscriber::fmt::Layer<tracing_subscriber::Registry, tracing_subscriber::fmt::format::DefaultFields, tracing_subscriber::fmt::format::Format, FileWriter>,
        tracing_subscriber::Registry,
    >,
>;

fn init_log(target_dir: &Path) -> ReconfigurableLogger {
    let configured_log_dir = std::env::var_os("SCBL_LAN_CLIENT_DATA_DIR").map(PathBuf::from).map(|path| path.join("logs").join("game"));
    let log_dir = configured_log_dir
        .filter(|path| std::fs::create_dir_all(path).is_ok())
        .unwrap_or_else(|| target_dir.to_path_buf());
    let path = log_dir.join("bl-tracing.log");
    let _ = std::fs::remove_file(&path);
    let subscriber_builder = tracing_subscriber::FmtSubscriber::builder()
        .with_writer(FileWriter(path))
        .with_ansi(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_line_number(true)
        .with_env_filter(tracing_subscriber::EnvFilter::builder().with_default_directive(LevelFilter::INFO.into()).from_env_lossy())
        .with_filter_reloading();
    let reload_handle = subscriber_builder.reload_handle();
    subscriber_builder.init();
    tracing::event!(tracing::Level::INFO, "attaching");

    std::panic::set_hook(Box::new(|panic_info| {
        let mut expl = String::new();

        let message = match (panic_info.payload().downcast_ref::<&str>(), panic_info.payload().downcast_ref::<String>()) {
            (Some(s), _) => Some((*s).to_string()),
            (_, Some(s)) => Some(s.to_string()),
            (None, None) => None,
        };

        let cause = match message {
            Some(m) => m,
            None => "Unknown".into(),
        };

        match panic_info.location() {
            Some(location) => {
                expl.push_str(&format!("Panic occurred in file '{}' at line {}", location.file(), location.line()));
            }
            None => expl.push_str("Panic location unknown."),
        }
        let msg = format!("{expl}\n{cause}");

        tracing::error!("{}", msg);

        show_msgbox(&msg, "PANIC");

        unsafe {
            std::arch::asm!("int3");
        }

        std::process::exit(1);
    }));
    reload_handle
}

#[no_mangle]
unsafe extern "system" fn DllMain(hinst: HMODULE, reason: u32, reserved: *mut c_void) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => init(Some(hinst)),
        // ExitProcess has already stopped the other threads; they may have
        // abandoned our locks. Let Windows reclaim process resources instead
        // of running hooks, logging or user32/COM cleanup under the loader lock.
        // Keep the existing explicit-unload / failed-load cleanup path.
        DLL_PROCESS_DETACH if reserved.is_null() => deinit(Some(hinst)),
        DLL_PROCESS_DETACH => {}
        DLL_THREAD_ATTACH | DLL_THREAD_DETACH => {}
        _ => {
            fatal_error!("Unexpected reason: {reason}");
        }
    };
    BOOL::from(true)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::scbl_launch_marker_present;

    #[test]
    fn only_a_non_empty_scbl_process_marker_enables_hooks() {
        assert!(!scbl_launch_marker_present(None));
        assert!(!scbl_launch_marker_present(Some(OsStr::new(""))));
        assert!(scbl_launch_marker_present(Some(OsStr::new("C:\\SCBL\\temp"))));
    }
}


#[cfg(test)]
mod file_writer_tests {
    use super::FileWriter;
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use tracing_subscriber::fmt::MakeWriter;

    fn own_test_directory() -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("scbl-log-open-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn file_writer_preserves_append_and_recovers_after_open_failure() {
        let root = own_test_directory();
        let parent = root.join("late");
        let path = parent.join("log.txt");
        let owner = FileWriter(path.clone());
        owner.make_writer().write_all(b"unavailable entry\n").unwrap();
        assert!(!parent.exists());
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, b"prefix\n").unwrap();
        owner.make_writer().write_all(b"available entry\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"prefix\navailable entry\n");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&parent).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn file_writer_directory_and_sharing_violation_do_not_panic() {
        let root = own_test_directory();
        FileWriter(root.clone()).make_writer().write_all(b"discarded\n").unwrap();
        let path = root.join("held.log");
        std::fs::write(&path, b"unchanged\n").unwrap();
        let held = std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&path).unwrap();
        FileWriter(path.clone()).make_writer().write_all(b"discarded\n").unwrap();
        drop(held);
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged\n");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
}
