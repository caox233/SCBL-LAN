#![deny(clippy::pedantic)]
#![feature(iter_intersperse)]

#[macro_use]
extern crate quazal_macros;
#[macro_use]
extern crate slog;

use std::fs;
use std::io;
use std::io::Write;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use quazal::prudp::packet::QPacket;
use quazal::ClientInfo;
use quazal::Context;
use sc_bl_protocols as protocols;
use slog::Drain;
use slog::Logger;
use sloggers::Build;
use storage::Storage;

const DEFAULT_MP_DATA: &str = include_str!("../assets/mp_balancing.ini");

const SERVER_PID: u32 = 0x1000;

macro_rules! rmc_err {
    ($e:expr, $log:expr, $msg:literal) => (
        $e.map_err(|e| {
            slog::error!($log, $msg; "error" => ?e);
            quazal::rmc::Error::InternalError
        })
    )
}

/// Checks if a client is logged in and returns their user ID.
///
/// Returns an `AccessDenied` error if the client is not logged in.
fn login_required<T>(ci: &ClientInfo<T>) -> quazal::rmc::Result<u32> {
    ci.user_id.ok_or(quazal::rmc::Error::AccessDenied)
}

mod api;
mod challenge;
mod clan;
mod config;
mod lan_runtime;
mod game_session;
mod game_session_ex;
mod gameplay_modes;
mod ladder;
mod locale;
mod nat_traversal;
mod overlord_challenge;
mod overlord_core;
mod overlord_news;
mod player_stats;
mod privileges;
mod secure;
mod simple_http;
mod storage;
mod ticket;
mod tracking;
mod tracking_ext;
mod ubi_acc_mgmt;
mod uplay_win;
mod user_storage;

use crate::config::Config;

type NativeServer<'a> = quazal::prudp::Server<'a, fn(ClientInfo), fn(ClientInfo)>;

// Shared by the actual secure listener and maintenance expiry fixtures. The
// transport may forget this exact owner only after its close transaction commits.
fn configure_secure_close_handler<'a>(server: &mut NativeServer<'a>, logger: &'a Logger, storage: &'a Storage) {
    server.close_handler = Some(Box::new(move |ci, reason| {
        let Some(user_id) = ci.user_id else { return true; };
        match storage.try_close_secure_ticket(user_id, ci.ticket_fingerprint) {
            Ok(storage::SecureCloseAttempt::Committed { preserved }) => {
                info!(logger, "Secure close committed"; "user_id" => user_id, "reason" => ?reason, "preserved" => preserved);
                true
            }
            Ok(storage::SecureCloseAttempt::Deferred) => false,
            Err(e) => {
                error!(logger, "Secure close deferred; retaining ticket owner"; "user_id" => user_id, "reason" => ?reason, "error" => %e);
                false
            }
        }
    }));
}

#[cfg(all(test, feature = "maintenance-test-seams"))]
mod secure_close_retry_tests;

/// Starts a Quazal server (either secure or authentication).
///
/// This function sets up the necessary protocols and handlers for the server
/// and then enters the server loop.
fn start_server(logger: &slog::Logger, ctx: &Context, storage: &Arc<Storage>, is_secure: bool) -> io::Result<()> {
    use quazal::prudp::packet::StreamHandlerRegistry;
    use quazal::prudp::packet::StreamType;
    use quazal::prudp::packet::VPort;
    use quazal::prudp::Server;
    use quazal::rmc::RVSecHandler;

    let mut handler = RVSecHandler::<()>::new(logger.clone());

    if is_secure {
        handler.register_protocol(challenge::new_protocol());
        handler.register_protocol(clan::new_protocol());
        handler.register_protocol(game_session_ex::new_protocol(Arc::clone(storage)));
        handler.register_protocol(game_session::new_protocol(Arc::clone(storage)));
        handler.register_protocol(ladder::new_protocol());
        handler.register_protocol(locale::new_protocol());
        handler.register_protocol(nat_traversal::new_protocol());
        handler.register_protocol(overlord_challenge::new_protocol());
        handler.register_protocol(overlord_core::new_protocol());
        handler.register_protocol(overlord_news::new_protocol());
        handler.register_protocol(player_stats::new_protocol());
        handler.register_protocol(privileges::new_protocol());
        handler.register_protocol(secure::new_protocol(Arc::clone(storage)));
        handler.register_protocol(tracking_ext::new_protocol());
        handler.register_protocol(tracking::new_protocol());
        handler.register_protocol(ubi_acc_mgmt::new_protocol(Arc::clone(storage)));
        handler.register_protocol(uplay_win::new_protocol());
        handler.register_protocol(user_storage::new_protocol());
    } else {
        handler.register_protocol(ticket::new_protocol(Arc::clone(storage)));
    }

    let mut registry = StreamHandlerRegistry::new(logger.clone());
    registry.register(
        VPort {
            stream_type: StreamType::RVSec,
            port: ctx.vport,
        },
        Box::new(handler),
    );

    let mut server: NativeServer<'_> = Server::new(logger.clone(), ctx, registry);
    if is_secure {
        // Only the secure/game connection owns online presence and room state.
        // The short-lived authentication connection must not clear an active
        // secure session when it disconnects or expires.
        configure_secure_close_handler(&mut server, logger, storage);
        server.user_handler = Some(handle_user_packet);
    }
    server.bind(ctx.listen)?;
    lan_runtime::mark_ready(if is_secure { 8 } else { 4 });
    server.serve();
    Ok(())
}

/// Handles user-specific RMC packets.
///
/// This function is a placeholder for handling user-specific RMC packets.
fn handle_user_packet(logger: &Logger, packet: QPacket, client: SocketAddr, socket: &UdpSocket) {
    if packet.source.port != 1 || packet.destination.port != 1 {
        warn!(
            logger,
            "Dropping user packet with unexpected virtual ports";
            "client" => client.to_string(),
            "source_port" => packet.source.port,
            "destination_port" => packet.destination.port,
        );
        return;
    }

    let payload = packet.payload;

    let mut response = payload;
    if let Err(error) = write!(&mut response, "udp:/address={};port={}\0", client.ip(), client.port()) {
        error!(logger, "Failed to build user packet response"; "client" => client.to_string(), "error" => %error);
        return;
    }
    if let Err(error) = socket.send_to(&response, client) {
        warn!(logger, "Failed to send user packet response"; "client" => client.to_string(), "error" => %error);
    }
}

/// Builds a terminal logger with a configurable log level.
fn build_term_logger() -> Logger {
    sloggers::terminal::TerminalLoggerBuilder::new()
        .level(
            #[allow(clippy::match_same_arms)]
            match std::env::var("RUST_LOG").unwrap_or_else(|_| String::from("info")).as_str() {
                "debug" => sloggers::types::Severity::Debug,
                "trace" => sloggers::types::Severity::Trace,
                "info" => sloggers::types::Severity::Info,
                "error" => sloggers::types::Severity::Error,
                "critical" => sloggers::types::Severity::Critical,
                "warning" => sloggers::types::Severity::Warning,
                _ => sloggers::types::Severity::Trace,
            },
        )
        .format(sloggers::types::Format::Compact)
        .build()
        .unwrap()
}

/// Rotates log files, keeping a specified number of backups.
fn rotate_log_files<S: AsRef<Path>>(fname: S, i: i32) -> io::Result<PathBuf> {
    let fname = fname.as_ref();
    if fname.exists() {
        let maybe_iteration = fname.extension().and_then(|s| s.to_str()).and_then(|s| s.parse::<i32>().ok());
        let new_fname = match maybe_iteration {
            Some(j) if j == i - 1 => format!("{}.{}", fname.file_stem().and_then(|f| f.to_str()).unwrap(), i),
            _ => format!("{}.{}", fname.display(), i),
        };
        if i < 10 {
            rotate_log_files(&new_fname, i + 1)?;
        }
        fs::rename(fname, new_fname)?;
    }
    Ok(fname.to_path_buf())
}

/// Builds a file logger that writes to a JSON file.
fn build_file_logger() -> Logger {
    let fname = rotate_log_files("server.log.json", 1).unwrap();
    sloggers::file::FileLoggerBuilder::new(fname)
        .truncate()
        .level(sloggers::types::Severity::Info)
        .format(sloggers::types::Format::Json)
        .build()
        .unwrap()
}

/// Ensures that the necessary data directory and files exist.
fn ensure_data_dir() -> io::Result<()> {
    let mp_ini = std::env::current_dir()?.join("data").join("mp_balancing.ini");
    if mp_ini.exists() {
        return Ok(());
    }
    fs::create_dir_all(mp_ini.parent().unwrap())?;
    fs::write(mp_ini, DEFAULT_MP_DATA)?;
    Ok(())
}

#[derive(argh::FromArgs)]
/// dedicated server
struct Args {
    /// path to config file (default: service.toml)
    #[argh(option, short = 'c', long = "config")]
    config_path: Option<PathBuf>,
    /// create a new independent LAN config for this IPv4, then exit
    #[argh(option)]
    init_lan: Option<std::net::Ipv4Addr>,
    /// independent server data directory (default: executable directory/lan-server-data)
    #[argh(option)]
    data_dir: Option<PathBuf>,
    /// use alternate ports only for automated development smoke tests
    #[argh(switch)]
    smoke_ports: bool,

    /// started through launcher
    #[argh(switch)]
    launcher: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct RequiredServiceExit {
    name: String,
    reason: String,
}

fn spawn_required_service<F>(name: String, exit_sender: mpsc::Sender<RequiredServiceExit>, run: F) -> io::Result<std::thread::JoinHandle<()>>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    std::thread::Builder::new().name(name.clone()).spawn(move || {
        let reason = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
            Ok(Ok(())) => String::from("returned unexpectedly"),
            Ok(Err(error)) => error,
            Err(_) => String::from("panicked"),
        };
        let _ = exit_sender.send(RequiredServiceExit { name, reason });
    })
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let args = argh::from_env::<Args>();
    // Fail closed before data creation, database mutation, listeners or readiness.
    let verified_release = server_api::release_catalog::verify_server_package(&std::env::current_exe()?)?;
    sodiumoxide::init().map_err(|_| eyre::eyre!("crypto initialization failed"))?;
    let root = args.data_dir.clone().unwrap_or(std::env::current_exe()?.parent().unwrap().join("lan-server-data"));
    std::fs::create_dir_all(&root)?;
    std::env::set_current_dir(std::fs::canonicalize(&root)?)?;
    #[cfg(target_os = "windows")]
    let _instance_lock = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(false).share_mode(0)
            .open("server.lock")
            .map_err(|error| eyre::eyre!("This LAN data directory is already in use or cannot be locked: {error}"))?
    };
    if let Some(ip) = args.init_lan {
        lan_runtime::initialize(ip, args.smoke_ports)?;
        println!("SCBL LAN server initialized. No old data imported.");
        return Ok(());
    }
    if !Path::new("service.toml").is_file() { return Err(eyre::eyre!("Run --init-lan YOUR_IPV4 first")); }

    let logger = if args.launcher {
        Logger::root(build_file_logger(), o!())
    } else {
        let logger = build_term_logger();
        Logger::root(slog::Duplicate(logger, build_file_logger()).fuse(), o!())
    };

    {
        let logger = logger.clone();
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            error!(logger, "Panic occurred: {panic_info}");
            old_hook(panic_info);
        }));
    }

    let storage = Arc::new(Storage::init(logger.clone(), verified_release.clone())?);

    let config_filename = args.config_path.unwrap_or_else(|| PathBuf::from("service.toml"));

    let config = Config::load_from_file(&config_filename)?;
    lan_runtime::configure_ports(&config);

    ensure_data_dir()?;

    warn!(logger, "Clearing stale sessions");
    storage.invalidate_sessions()?;

    let (service_exit_sender, service_exit_receiver) = mpsc::channel();
    let mut threads = Vec::new();
    for (name, svc) in config.quazal.into_services()? {
        let logger = logger.new(o!("service" => name.clone()));
        info!(logger, "Loaded LAN service {}", name);
        let storage = Arc::clone(&storage);
        let handle = match svc {
            quazal::Service::Authentication(ctx) => spawn_required_service(name, service_exit_sender.clone(), move || {
                start_server(&logger, &ctx, &storage, false).map_err(|error| format!("authentication server failed: {error}"))
            }),
            quazal::Service::Secure(ctx) => spawn_required_service(name, service_exit_sender.clone(), move || {
                start_server(&logger, &ctx, &storage, true).map_err(|error| format!("secure server failed: {error}"))
            }),
            quazal::Service::Config(cfg) => spawn_required_service(name, service_exit_sender.clone(), move || {
                if cfg.listen.port() != 80 {
                    warn!(
                        logger,
                        "Unexpected port {} used for the config server. Clients are expecting port 80. Adjust in the service config or make sure to redirect traffic accordingly",
                        cfg.listen.port()
                    );
                }
                simple_http::serve(&logger, cfg.listen, &cfg.content()).map_err(|error| format!("config server failed: {error}"))
            }),
            quazal::Service::Content(srv) => spawn_required_service(name, service_exit_sender.clone(), move || {
                simple_http::serve_many(&logger, srv.listen, &srv.files).map_err(|error| format!("content server failed: {error}"))
            }),
        };
        threads.push(handle?);
    }

    threads.push(spawn_required_service(String::from("api"), service_exit_sender.clone(), move || {
        let logger = logger.new(o!("service" => "api"));
        let runtime = tokio::runtime::Runtime::new().map_err(|error| format!("API runtime creation failed: {error}"))?;
        runtime
            .block_on(api::start_server(logger, storage, config.api_server, Arc::new(config.debug), false, verified_release))
            .map_err(|error| format!("API server failed: {error:?}"))
    })?);

    drop(service_exit_sender);
    let exit = service_exit_receiver
        .recv()
        .map_err(|error| eyre::eyre!("required service supervision channel failed: {error}"))?;
    Err(eyre::eyre!("required service {} exited: {}", exit.name, exit.reason))
}

#[cfg(test)]
mod user_packet_tests {
    use super::*;

    #[test]
    fn untrusted_virtual_ports_are_dropped_without_panicking() {
        let logger = Logger::root(slog::Discard, slog::o!());
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client: SocketAddr = "127.0.0.1:9".parse().unwrap();

        for (source_port, destination_port) in [(0, 1), (1, 0), (15, 15)] {
            let mut packet = QPacket::default();
            packet.source.port = source_port;
            packet.destination.port = destination_port;
            handle_user_packet(&logger, packet, client, &socket);
        }
    }

    #[test]
    fn required_service_return_is_reported_to_the_main_supervisor() {
        let (sender, receiver) = mpsc::channel();
        let handle = spawn_required_service(String::from("test-service"), sender, || Ok(())).unwrap();
        handle.join().unwrap();
        assert_eq!(
            receiver.recv().unwrap(),
            RequiredServiceExit {
                name: String::from("test-service"),
                reason: String::from("returned unexpectedly"),
            }
        );
    }
}

#[cfg(test)]
#[path = "remaining_auth_logging_red.rs"]
mod remaining_auth_logging_red;

#[cfg(test)]
#[path = "remaining_auth_metadata_tests.rs"]
mod remaining_auth_metadata_tests;
