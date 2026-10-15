use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::net::SocketAddr;

use quazal::ContentServer;
use quazal::Context;
use quazal::OnlineConfig;
use quazal::Service;
use serde::Deserialize;
use serde::Serialize;
use slog::error;
use slog::Logger;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Deserialize, Serialize, Default, Clone, Copy)]
pub struct DebugConfig {
    pub mark_all_as_online: bool,
    pub force_joins: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(flatten)]
    pub quazal: quazal::Config,
    pub api_server: SocketAddr,
    pub debug: DebugConfig,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Parsing error: {0}")]
    Deserialize(#[from] toml::de::Error),
    #[error("Serializing error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Config {
    pub fn load_from_file<P: AsRef<std::path::Path>>(path: P) -> Result<Self, Error> {
        let data = std::fs::read_to_string(path)?;
        let w: Config = toml::from_str(&data)?;
        Ok(w)
    }

    pub fn save_to_file<P: AsRef<std::path::Path>>(&self, path: P) -> Result<(), Error> {
        let data = toml::to_string_pretty(self)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    pub fn load_from_file_or_default<P: AsRef<std::path::Path>>(logger: &Logger, path: P) -> eyre::Result<Self> {
        let e = match Self::load_from_file(path.as_ref()) {
            Ok(cfg) => return Ok(cfg),
            Err(e) => e,
        };

        if !matches!(&e, Error::IO(error) if error.kind() == std::io::ErrorKind::NotFound) {
            return Err(e.into());
        }

        error!(logger, "Couldn't load service file, generating default"; "error" => %e);

        Self::create_default_file(path).map_err(Into::into)
    }

    // Only the missing-file fallback may use this. Explicit save_to_file keeps
    // its existing overwrite contract; fallback creation must never overwrite.
    fn create_default_file<P: AsRef<std::path::Path>>(path: P) -> Result<Self, Error> {
        let cfg = Self::default();
        let data = toml::to_string_pretty(&cfg)?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        std::io::Write::write_all(&mut file, data.as_bytes())?;
        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut online_cfg = OnlineConfig::default();
        online_cfg.listen.set_ip(std::net::IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)));

        let server_ip = std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        let mut content_srv = ContentServer::default();
        content_srv.listen.set_ip(online_cfg.listen.ip());

        let mut ctx = Context::splinter_cell_blacklist();
        ctx.listen.set_ip(online_cfg.listen.ip());
        ctx.listen.set_port(21126);
        let mut secure_ctx = ctx.clone();

        let mut content_server_addr = content_srv.listen;
        content_server_addr.set_ip(server_ip);
        secure_ctx.settings.insert(String::from("storage_host"), content_server_addr.to_string());
        if let Some(path) = content_srv.files.keys().next() {
            secure_ctx.settings.insert(String::from("storage_path"), path.clone());
        }
        secure_ctx.listen.set_port(ctx.listen.port() + 1);

        let mut secure_server_addr = secure_ctx.listen;
        secure_server_addr.set_ip(server_ip);
        ctx.secure_server_addr = Some(secure_server_addr);

        let quazal_config = quazal::Config {
            services: ["onlineconfig", "content", "sc_bl_secure", "sc_bl_auth"].into_iter().map(String::from).collect(),
            service: HashMap::from([
                ("sc_bl_auth".to_string(), Service::Authentication(ctx)),
                ("sc_bl_secure".to_string(), Service::Secure(secure_ctx)),
                ("onlineconfig".to_string(), Service::Config(online_cfg)),
                ("content".to_string(), Service::Content(content_srv)),
            ]),
        };

        Config {
            api_server: "0.0.0.0:50051".parse().unwrap(),
            quazal: quazal_config,
            debug: DebugConfig::default(),
        }
    }
}


#[cfg(test)]
mod safe_default_tests {
    use super::*;
    use std::{fs,path::PathBuf,sync::atomic::{AtomicU64,Ordering}};
    static SERIAL: AtomicU64=AtomicU64::new(0);
    fn directory()->PathBuf{
        let time=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path=std::env::temp_dir().join(format!("scbl-config-{}-{time}-{}",std::process::id(),SERIAL.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();path
    }
    #[test]
    fn missing_default_is_created_but_existing_bytes_are_never_replaced(){
        let root=directory();let path=root.join("service.toml");let log=Logger::root(slog::Discard,slog::o!());
        assert!(Config::load_from_file_or_default(&log,&path).is_ok());assert!(Config::load_from_file(&path).is_ok());
        let bytes=fs::read(&path).unwrap();
        // Deterministically cover a file appearing between missing-file read and creation.
        assert!(matches!(Config::create_default_file(&path),Err(Error::IO(e)) if e.kind()==std::io::ErrorKind::AlreadyExists));
        assert_eq!(fs::read(&path).unwrap(),bytes);
        for bytes in [vec![0xff,0xfe],b"broken = [".to_vec()] {
            fs::write(&path,&bytes).unwrap();assert!(Config::load_from_file_or_default(&log,&path).is_err());assert_eq!(fs::read(&path).unwrap(),bytes);
        }
    }
    #[test]
    fn directory_and_default_creation_failures_are_not_success(){
        let root=directory();let log=Logger::root(slog::Discard,slog::o!());
        assert!(Config::load_from_file_or_default(&log,&root).is_err());
        let missing=root.join("missing/service.toml");assert!(Config::load_from_file_or_default(&log,&missing).is_err());assert!(!missing.exists());
    }
    #[cfg(windows)]
    #[test]
    fn sharing_read_failure_preserves_configuration_and_does_not_use_defaults(){
        use std::os::windows::fs::OpenOptionsExt;
        let root=directory();let path=root.join("service.toml");let log=Logger::root(slog::Discard,slog::o!());
        let cfg=Config{api_server:"127.0.0.1:34567".parse().unwrap(),..Config::default()};cfg.save_to_file(&path).unwrap();let bytes=fs::read(&path).unwrap();
        let lock=fs::OpenOptions::new().read(true).share_mode(2|4).open(&path).unwrap();
        assert!(Config::load_from_file_or_default(&log,&path).is_err());drop(lock);assert_eq!(fs::read(&path).unwrap(),bytes);
        assert_eq!(Config::load_from_file_or_default(&log,&path).unwrap().api_server.port(),34567);
    }
}
