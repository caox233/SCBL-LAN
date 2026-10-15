use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use tracing::info;
use tracing::instrument;
use url::Url;

#[cfg(target_os = "windows")]
mod saves;
#[cfg(target_os = "windows")]
pub use saves::SaveGameExt;

#[cfg(target_os = "windows")]
mod msgbox {
    use std::ffi::CString;

    use windows::core::PCSTR;
    use windows::Win32::UI::WindowsAndMessaging::MessageBoxA;
    use windows::Win32::UI::WindowsAndMessaging::MB_OK;

    pub fn show_msgbox(msg: &str, caption: &str) {
        let msg = CString::new(msg).unwrap();
        let caption = CString::new(caption).unwrap();
        unsafe {
            MessageBoxA(None, PCSTR(msg.as_ptr().cast::<u8>()), PCSTR(caption.as_ptr().cast::<u8>()), MB_OK);
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();
pub static URL: OnceLock<Url> = OnceLock::new();

fn default_overlay() -> bool {
    true
}

fn default_fov_factor() -> f32 {
    1.2
}

macro_rules! enum_gui {
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[cfg(feature = $feature:literal)])?
                #[label=$label:literal]
                $field:ident,
            )*
        }
    ) => {
        $(#[$attr])*
        $vis enum $name {
            $(
                $(#[cfg(feature = $feature)])?
                $field,
            )*
        }

        impl $name {
            enum_gui!(@1 $name, [$($(#[cfg(feature = $feature)])?$name::$field),*]);
            enum_gui!(@2 [$($(#[cfg(feature = $feature)])?$label),*]);
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(
                        $(#[cfg(feature = $feature)])?
                        $name::$field => write!(f, "{}::{}", stringify!($name), stringify!($field)),
                    )*
                }
            }
        }
    };

    (@1 $name:ident, $value:expr) => {
        pub const VARIANTS: [$name; $value.len()] = $value;
    };

    (@2 $value:expr) => {
        pub const LABELS: [&'static str; $value.len()] = $value;
    };
}

enum_gui! {
    #[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash)]
    #[serde(rename_all = "PascalCase")]
    pub enum Hook {
        #[label="Log internal messages"]
        Printer,
        #[label="Log when a state is exiting"]
        LeaveState,
        #[label="Log what state is coming up next"]
        NextState,
        #[label="Log NetResultBase"]
        NetResultBase,
        #[label="Log goals"]
        Goal,
        #[label="Log next step"]
        SetStep,
        #[label="Log new threads"]
        Thread,
        #[label="Log state changes"]
        ChangeState,
        #[label="Enforce LAN mode?"]
        NetCore,
        #[label="Log NetResultCore"]
        NetResultCore,
        #[label="Log NetResultSession"]
        NetResultSession,
        #[label="Log NetResultRdvSession"]
        NetResultRdvSession,
        #[label="Log NetResultLobby"]
        NetResultLobby,
        #[label="Log IP:PORT used by Storm"]
        StormHostPortToString,
        #[label="Enforce IP returned by GetAdaptersInfo"]
        GetAdaptersInfo,
        #[label="Enforce IP returned by gethostbyname"]
        Gethostbyname,
        #[label="Log generated IDs"]
        GenerateID,
        #[label="Log storm states"]
        StormSetState,
        #[label="Log storm state transitions"]
        StormStateMachineActionExecute,
        #[label="Log storm errors"]
        StormErrorFormatter,
        #[label="Log gear destructors (LARGE)"]
        GearStrDestructor,
        #[label="Log storm events"]
        StormEventDispatcher,
        #[label="Log storm udp packets"]
        StormPackets,
        #[label="Log RMC messages"]
        RMCMessages,
        #[cfg(feature = "modding")]
        #[label="Override packaged files"]
        OverridePackaged,
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Config {
    // Optional at parse time: local mouse/FOV repairs must not depend on LAN admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseConfig>,
    pub user: User,
    #[serde(default)]
    #[serde(skip_serializing_if = "Save::is_default")]
    pub save: Save,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward_calls: Vec<String>,
    #[serde(default)]
    pub forward_all_calls: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub internal_command_line: String,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub enable_hooks: HashSet<Hook>,
    #[serde(default)]
    pub enable_all_hooks: bool,
    #[serde(default = "default_overlay")]
    pub enable_overlay: bool,
    pub config_server: Option<String>,
    pub api_server: url::Url,
    #[serde(default)]
    #[serde(skip_serializing_if = "Networking::is_default")]
    pub networking: Networking,
    #[serde(default)]
    #[serde(skip_serializing_if = "Logging::is_default")]
    pub logging: Logging,
    #[serde(default)]
    pub auto_join_invite: bool,
    #[serde(default)]
    pub gameplay_mode: GameplayMode,
    #[serde(default)]
    pub features: Features,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct ReleaseConfig {
    pub schema: u32,
    pub package_root: PathBuf,
    pub release_id: String,
    pub compat_id: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GameplayMode {
    #[default]
    Official,
    Mod,
}

impl GameplayMode {
    pub const fn as_config_value(self) -> &'static str {
        match self {
            Self::Official => "Official",
            Self::Mod => "MOD",
        }
    }

    fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("MOD") {
            Self::Mod
        } else {
            Self::Official
        }
    }
}

impl Serialize for GameplayMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_config_value())
    }
}

impl<'de> Deserialize<'de> for GameplayMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        Ok(value.as_str().map_or(Self::Official, Self::parse))
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Features {
    #[serde(default = "default_fov_factor")]
    pub fov_factor: f32,
}

impl Default for Features {
    fn default() -> Self {
        Self { fov_factor: default_fov_factor() }
    }
}

#[derive(Default, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    pub username: String,
    pub password: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cd_keys: Vec<String>,
    pub account_id: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum SaveDir {
    #[default]
    Roaming,
    Custom(String),
}

impl SaveDir {
    pub fn is_default(&self) -> bool {
        matches!(self, SaveDir::Roaming)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "PascalCase")]
pub struct Save {
    #[serde(default)]
    #[serde(skip_serializing_if = "SaveDir::is_default")]
    pub save_dir: SaveDir,
}

impl Save {
    pub fn is_default(&self) -> bool {
        self.save_dir.is_default()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct SaveGame {
    pub slot_id: usize,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct Networking {
    pub ip_address: Option<std::net::Ipv4Addr>,
    pub adapter: Option<String>,
}

impl Networking {
    pub fn is_default(&self) -> bool {
        self.ip_address.is_none() && self.adapter.is_none()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, PartialOrd, Eq, Ord, Copy)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warning,
    Error,
}

impl AsRef<str> for LogLevel {
    fn as_ref(&self) -> &str {
        match self {
            LogLevel::Trace => "Trace",
            LogLevel::Debug => "Debug",
            LogLevel::Info => "Info",
            LogLevel::Warning => "Warning",
            LogLevel::Error => "Error",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct Logging {
    #[serde(default)]
    pub level: LogLevel,
}

impl Logging {
    pub fn is_default(&self) -> bool {
        matches!(self.level, LogLevel::Info)
    }
}

fn apply_scbl_required_hooks(cfg: &mut Config) {
    cfg.enable_hooks.insert(Hook::GetAdaptersInfo);
    cfg.enable_hooks.insert(Hook::Gethostbyname);
    // StormEventDispatcher only observes retail dispatch/queue/handler calls.
    // Keep it available through EnableHooks, but do not install it in every
    // normal session merely to collect reverse-engineering diagnostics.
    for hook in [
        Hook::Goal,
        Hook::SetStep,
        Hook::NextState,
        Hook::ChangeState,
        Hook::LeaveState,
        Hook::NetResultBase,
        Hook::NetResultCore,
        Hook::NetResultSession,
        Hook::NetResultRdvSession,
        Hook::NetResultLobby,
        Hook::RMCMessages,
    ] {
        cfg.enable_hooks.insert(hook);
    }
    info!("SCBL_ABI31_JOIN_DIAGNOSTICS enabled=true");
}

fn parse_scbl_config(content: &str) -> anyhow::Result<Config> {
    let mut cfg = toml::from_str::<Config>(content).map_err(|_| anyhow::anyhow!("Invalid SCBL LAN configuration received from launch pipe"))?;
    validate_user_identity(&cfg.user)?;
    apply_scbl_required_hooks(&mut cfg);
    info!("Loaded SCBL LAN hooks config from launch pipe");
    Ok(cfg)
}

fn validate_user_identity(user: &User) -> anyhow::Result<()> {
    for (field, value) in [
        ("User.Username", user.username.as_str()),
        ("User.AccountId", user.account_id.as_str()),
    ] {
        if value.trim().is_empty() {
            anyhow::bail!("{field} must be explicitly configured and non-empty");
        }
        if value.contains('\0') {
            anyhow::bail!("{field} cannot contain NUL");
        }
    }
    // Passwords are opaque: match the launcher/server nonempty policy, without
    // trimming whitespace. Native GetPassword retains its separate capacity check.
    if user.password.is_empty() || user.password.contains('\0') {
        anyhow::bail!("User.Password must be non-empty and cannot contain NUL");
    }
    Ok(())
}

#[cfg(test)]
const TEST_CONFIG: &str = r#"
# Where to find the config server
ConfigServer = "127.0.0.1"
# Where to find the api server (typically the same as the config server)
ApiServer = "http://127.0.0.1:50051"
# Automatically join invites without user intervention
AutoJoinInvite = false
# Explicit player-selected gameplay mode. Missing or unknown values are Official.
GameplayMode = "Official"

[User]
Username = "test-user"
Password = "test-password"
AccountId = "test-account"

[Save]
SaveDir = "Roaming"

[Features]
FovFactor = 1.2
"#;

pub fn get() -> Option<&'static Config> {
    CONFIG.get()
}

#[cfg(target_os = "windows")]
fn get_adapter_infos<T>(f: impl Fn(*mut windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_INFO) -> anyhow::Result<T>) -> anyhow::Result<T> {
    #![allow(clippy::cast_possible_truncation, clippy::crosspointer_transmute)]

    use anyhow::bail;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Foundation::WIN32_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::GetAdaptersInfo;
    use windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_INFO;
    use windows::Win32::System::Diagnostics::Debug::FormatMessageW;
    use windows::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_ALLOCATE_BUFFER;

    let mut adapterinfo = vec![0u8; std::mem::size_of::<IP_ADAPTER_INFO>()];
    let mut size = adapterinfo.len() as u32;
    let mut res = WIN32_ERROR(unsafe { GetAdaptersInfo(Some(adapterinfo.as_mut_ptr().cast()), &mut size) });
    if res == ERROR_BUFFER_OVERFLOW {
        adapterinfo.resize(size as usize, 0);
        res = WIN32_ERROR(unsafe { GetAdaptersInfo(Some(adapterinfo.as_mut_ptr().cast()), &mut size) });
    }
    match res {
        ERROR_BUFFER_OVERFLOW => {
            bail!("Couldn't allocate enough memory for network adapters");
        }
        ERROR_SUCCESS => f(adapterinfo.as_mut_ptr().cast()),
        _ => unsafe {
            let mut buffer_ptr: PWSTR = PWSTR::null();
            let ptr_ptr: *mut PWSTR = &mut buffer_ptr;
            let chars = FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER, None, res.0, 0, std::mem::transmute::<*mut PWSTR, PWSTR>(ptr_ptr), 256, None);
            let msg = if chars > 0 {
                String::from_utf16(std::slice::from_raw_parts(buffer_ptr.0, chars as _))?
            } else {
                String::from("unknown")
            };
            LocalFree(std::mem::transmute::<PWSTR, HLOCAL>(buffer_ptr))?;
            bail!("Couldn't enumerate adapters: {}", msg);
        },
    }
}

#[cfg(target_os = "windows")]
fn get_adapter_addresses<T>(f: impl Fn(*mut windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_ADDRESSES_LH) -> anyhow::Result<T>) -> anyhow::Result<T> {
    #![allow(clippy::cast_possible_truncation, clippy::crosspointer_transmute, clippy::cast_lossless)]

    use anyhow::bail;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Foundation::WIN32_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::GetAdaptersAddresses;
    use windows::Win32::NetworkManagement::IpHelper::GAA_FLAG_SKIP_ANYCAST;
    use windows::Win32::NetworkManagement::IpHelper::GAA_FLAG_SKIP_DNS_SERVER;
    use windows::Win32::NetworkManagement::IpHelper::GAA_FLAG_SKIP_MULTICAST;
    use windows::Win32::NetworkManagement::IpHelper::GAA_FLAG_SKIP_UNICAST;
    use windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_ADDRESSES_LH;
    use windows::Win32::Networking::WinSock::AF_INET;
    use windows::Win32::System::Diagnostics::Debug::FormatMessageW;
    use windows::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_ALLOCATE_BUFFER;

    let mut adapter_addresses = vec![0u8; std::mem::size_of::<IP_ADAPTER_ADDRESSES_LH>()];
    let mut size = adapter_addresses.len() as u32;
    let mut res = WIN32_ERROR(unsafe {
        GetAdaptersAddresses(
            AF_INET.0 as u32,
            GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
            None,
            Some(adapter_addresses.as_mut_ptr().cast()),
            &mut size,
        )
    });
    if res == ERROR_BUFFER_OVERFLOW {
        adapter_addresses.resize(size as usize, 0);
        res = WIN32_ERROR(unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                None,
                Some(adapter_addresses.as_mut_ptr().cast()),
                &mut size,
            )
        });
    }
    match res {
        ERROR_BUFFER_OVERFLOW => {
            bail!("Couldn't allocate enough memory for network adapters");
        }
        ERROR_SUCCESS => f(adapter_addresses.as_mut_ptr().cast()),
        _ => unsafe {
            let mut buffer_ptr: PWSTR = PWSTR::null();
            let ptr_ptr: *mut PWSTR = &mut buffer_ptr;
            let chars = FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER, None, res.0, 0, std::mem::transmute::<*mut PWSTR, PWSTR>(ptr_ptr), 256, None);
            let msg = if chars > 0 {
                String::from_utf16(std::slice::from_raw_parts(buffer_ptr.0, chars as _))?
            } else {
                String::from("unknown")
            };
            LocalFree(std::mem::transmute::<PWSTR, HLOCAL>(buffer_ptr))?;
            bail!("Couldn't enumerate adapters: {}", msg);
        },
    }
}

#[cfg(target_os = "windows")]
fn find_ipaddress_for_adapter(target_adapter: &str) -> anyhow::Result<Option<std::net::Ipv4Addr>> {
    use std::ffi::CStr;
    use std::net::Ipv4Addr;

    use tracing::debug;
    use tracing::warn;
    use windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_ADDRESSES_LH;
    use windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_INFO;
    use windows::Win32::Networking::WinSock::AF_INET;
    use windows::Win32::Networking::WinSock::SOCKADDR_IN;

    if cfg!(feature = "GetAdapterInfos") {
        get_adapter_infos(|adapterinfo| {
            let mut result = None;
            let mut next_adapter: *mut IP_ADAPTER_INFO = adapterinfo;
            while let Some(current_adapter) = unsafe { next_adapter.as_ref() } {
                let adapter_name = CStr::from_bytes_until_nul(&current_adapter.Description)?;
                debug!("{adapter_name:?} == {target_adapter:?}");
                if adapter_name.to_str()? == target_adapter {
                    result = Some(CStr::from_bytes_until_nul(&current_adapter.IpAddressList.IpAddress.String)?.to_str()?.parse()?);
                    break;
                }
                next_adapter = current_adapter.Next;
            }
            if result.is_none() {
                warn!("No adapter {target_adapter:?} found");
            }
            Ok(result)
        })
    } else {
        get_adapter_addresses(|adapter_addresses| {
            let mut result = None;
            let mut next_adapter: *mut IP_ADAPTER_ADDRESSES_LH = adapter_addresses;
            while let Some(current_adapter) = unsafe { next_adapter.as_ref() } {
                let adapter_name = unsafe { current_adapter.FriendlyName.to_string()? };
                debug!("{adapter_name:?} == {target_adapter:?}");
                if adapter_name.as_str() == target_adapter {
                    if let Some(ip) = unsafe { current_adapter.FirstUnicastAddress.as_ref() } {
                        #[allow(clippy::cast_ptr_alignment)]
                        let sockaddr = unsafe { ip.Address.lpSockaddr.cast::<SOCKADDR_IN>().as_ref().unwrap() };
                        if sockaddr.sin_family == AF_INET {
                            result = Some(Ipv4Addr::new(
                                unsafe { sockaddr.sin_addr.S_un.S_un_b.s_b1 },
                                unsafe { sockaddr.sin_addr.S_un.S_un_b.s_b2 },
                                unsafe { sockaddr.sin_addr.S_un.S_un_b.s_b3 },
                                unsafe { sockaddr.sin_addr.S_un.S_un_b.s_b4 },
                            ));
                            break;
                        }
                        warn!("Can't handle family {:?}", sockaddr.sin_family);
                    }
                }
                next_adapter = current_adapter.Next;
            }
            if result.is_none() {
                warn!("No adapter {target_adapter:?} found");
            }
            Ok(result)
        })
    }
}

pub fn get_or_load(path: impl AsRef<Path>) -> anyhow::Result<&'static Config> {
    _get_or_load(path.as_ref())
}

#[instrument]
fn _get_or_load(path: &Path) -> anyhow::Result<&'static Config> {
    if let Some(cfg) = get() {
        return Ok(cfg);
    }
    let content = match fs::read_to_string(path.with_file_name("scbl-lan.toml")) {
        Ok(content) => content,
        #[cfg(target_os = "windows")]
        Err(ref err) if err.kind() == std::io::ErrorKind::NotFound => {
            msgbox::show_msgbox("scbl-lan.toml launch reference not found.\n\nRestart the game from SCBL LAN and keep the launcher open during game startup.\nDo not copy an old scbl.toml or put a password in this file.", "SCBL LAN launch session missing");
            std::process::exit(1);
        }
        Err(err) => return Err(err.into()),
    };
    #[cfg(target_os = "windows")]
    let content = lan_pipe_config(&content)?;
    let mut cfg: Config = parse_scbl_config(&content)?;
    if cfg.user.cd_keys.is_empty() {
        info!("Passing startup to original dll");
        cfg.forward_calls.push("UPLAY_Startup".into());
        cfg.forward_calls.push("UPLAY_Quit".into());
    }

    #[cfg(target_os = "windows")]
    if cfg.networking.ip_address.is_none() && cfg.networking.adapter.is_some() {
        let target_adapter = cfg.networking.adapter.as_ref().unwrap();
        info!("Getting IP address of adapter {target_adapter:?}");
        cfg.networking.ip_address = find_ipaddress_for_adapter(target_adapter)?;
    }

    if let Some(ip) = &cfg.networking.ip_address {
        info!("Enforcing {ip} for networking");
    }

    // if let Some(ref api_server) = cfg.api_server {
    crate::URL.set(cfg.api_server.clone()).map_err(|cfg| anyhow::anyhow!("Couldn't store api url {:?}", cfg))?;
    // }
    CONFIG.set(cfg).map_err(|cfg| anyhow::anyhow!("Couldn't store config {:?}", cfg))?;

    get().ok_or_else(|| anyhow::anyhow!("Config not loaded"))
}

pub fn get_config_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().join("scbl-lan.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub fn test_default_config() {
        let cfg: Config = parse_scbl_config(TEST_CONFIG).unwrap();
        assert_eq!(cfg.gameplay_mode, GameplayMode::Official);
        assert_eq!(cfg.features, Features::default());
        println!("{}", toml::to_string_pretty(&cfg).unwrap());
    }

    #[test]
    fn gameplay_mode_uses_only_the_explicit_config_field_and_fails_closed() {
        let official: Config = parse_scbl_config(TEST_CONFIG).unwrap();
        let mod_mode: Config = parse_scbl_config(&TEST_CONFIG.replace("GameplayMode = \"Official\"", "GameplayMode = \"MOD\"")).unwrap();
        let unknown: Config = parse_scbl_config(&TEST_CONFIG.replace("GameplayMode = \"Official\"", "GameplayMode = \"legacy\"")).unwrap();
        let missing: Config = parse_scbl_config(&TEST_CONFIG.replace("GameplayMode = \"Official\"\n", "")).unwrap();

        assert_eq!(official.gameplay_mode, GameplayMode::Official);
        assert_eq!(mod_mode.gameplay_mode, GameplayMode::Mod);
        assert_eq!(unknown.gameplay_mode, GameplayMode::Official);
        assert_eq!(missing.gameplay_mode, GameplayMode::Official);
        assert_eq!(
            toml::to_string(&mod_mode).unwrap().lines().find(|line| line.starts_with("GameplayMode =")).unwrap(),
            "GameplayMode = \"MOD\""
        );
    }

    #[test]
    fn fov_is_the_only_per_install_gameplay_value() {
        let content = TEST_CONFIG.replace("FovFactor = 1.2", "FovFactor = 1.25");
        let cfg: Config = parse_scbl_config(&content).unwrap();
        assert_eq!(cfg.features.fov_factor, 1.25);
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        assert!(serialized.contains("FovFactor = 1.25"));
        assert!(!serialized.contains("WeaponObstructionFix"));
        assert!(!serialized.contains("TickrateDiagnostics"));
    }

    #[test]
    fn scbl_toml_uses_only_the_standard_sectioned_schema() {
        let content = r#"
ConfigServer = "192.168.1.252"
ApiServer = "http://192.168.1.252:50051/"
AutoJoinInvite = false
EnableOverlay = true

[User]
Username = "POWERPC"
Password = "secret"
AccountId = "powerpc-account"

[Networking]
IpAddress = "10.144.144.2"

[Logging]
Level = "INFO"
"#;
        let cfg = parse_scbl_config(content).unwrap();
        assert_eq!(cfg.user.username, "POWERPC");
        assert_eq!(cfg.user.account_id, "powerpc-account");
        assert_eq!(cfg.networking.ip_address.unwrap().to_string(), "10.144.144.2");
        assert!(cfg.enable_hooks.contains(&Hook::GetAdaptersInfo));
        assert!(cfg.enable_hooks.contains(&Hook::Gethostbyname));
        assert!(cfg.enable_hooks.contains(&Hook::RMCMessages));
        assert!(!cfg.enable_hooks.contains(&Hook::StormEventDispatcher));
    }

    #[test]
    fn storm_event_dispatcher_diagnostics_remain_explicitly_opt_in() {
        let content = r#"
ConfigServer = "192.168.1.252"
ApiServer = "http://192.168.1.252:50051/"
EnableHooks = ["StormEventDispatcher"]

[User]
Username = "diagnostic-user"
Password = "test-only"
AccountId = "diagnostic-account"
"#;
        let cfg = parse_scbl_config(content).unwrap();
        assert!(cfg.enable_hooks.contains(&Hook::StormEventDispatcher));
    }

    #[test]
    fn retired_flat_auth_format_is_rejected() {
        let retired = r#"
Username = "POWERPC"
Password = "secret"
AccountId = "powerpc-account"
ConfigServer = "192.168.1.252"
ApiServer = "http://192.168.1.252:50051/"
BindIP = "10.144.144.2"
NetworkMode = "PublicTunnel"
"#;
        assert!(parse_scbl_config(retired).is_err());
        assert!(get_config_path("C:/game").ends_with("scbl-lan.toml"));
    }

    #[test]
    fn user_identity_must_be_explicit_and_non_empty() {
        for invalid in [
            TEST_CONFIG.replace("Username = \"test-user\"\n", ""),
            TEST_CONFIG.replace("Password = \"test-password\"\n", ""),
            TEST_CONFIG.replace("AccountId = \"test-account\"\n", ""),
            TEST_CONFIG.replace("Username = \"test-user\"", "Username = \"   \""),
            TEST_CONFIG.replace("AccountId = \"test-account\"", "AccountId = \"   \""),
        ] {
            assert!(parse_scbl_config(&invalid).is_err());
        }
    }

    #[test]
    fn missing_cd_keys_do_not_create_a_synthetic_product_key() {
        let cfg = parse_scbl_config(TEST_CONFIG).unwrap();
        assert!(cfg.user.cd_keys.is_empty());
        assert!(!toml::to_string(&cfg).unwrap().contains("ABCD-EFGH-IJKL-MNOP"));
    }
}


#[cfg(target_os = "windows")]
mod launch_pipe;

#[cfg(target_os = "windows")]
fn lan_pipe_config(content: &str) -> anyhow::Result<String> {
    launch_pipe::receive(content)
}


impl std::fmt::Debug for User { fn fmt(&self, f: &mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str("User { identity: [redacted], password: [redacted] }") } }


#[cfg(test)]
mod launch_identity_boundary_tests {
    use super::*;

    #[test]
    fn nul_identity_fields_are_rejected_without_logging_values() {
        for index in 0..3 {
            for value in ["\0", "\0prefix", "mid\0dle", "suffix\0", "玩家\0甲"] {
                let mut cfg: Config = toml::from_str(TEST_CONFIG).unwrap();
                match index {
                    0 => cfg.user.username = value.into(),
                    1 => cfg.user.account_id = value.into(),
                    _ => cfg.user.password = value.into(),
                }
                let encoded = toml::to_string(&cfg).unwrap();
                let error = parse_scbl_config(&encoded).unwrap_err().to_string();
                assert!(error.contains("NUL"));
                assert!(!error.contains(value));
            }
        }
    }

    #[test]
    fn nonempty_passwords_preserve_whitespace_and_native_gate_ownership() {
        for password in [" ", "   ", "\t", " \t\n ", " padded ", "玩家🙂", &"x".repeat(63), &"x".repeat(64)] {
            let mut cfg: Config = toml::from_str(TEST_CONFIG).unwrap();
            cfg.user.password = password.into();
            let parsed = parse_scbl_config(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(parsed.user.password, password);
        }
        // Accepting a parsed 64-byte password does not authorize native copying;
        // the launcher preflight and GetPassword still enforce their 63-byte gate.
    }
}

#[cfg(test)]
mod optional_release_config_tests {
    use super::*;
    #[test]
    fn missing_lan_release_preserves_standalone_features() {
        let cfg=parse_scbl_config(TEST_CONFIG).unwrap();
        assert!(cfg.release.is_none());
        assert_eq!(cfg.features.fov_factor,1.2);
        assert_eq!(cfg.gameplay_mode,GameplayMode::Official);
        // Parsing local repair settings does not evaluate package admission.
        let mod_cfg=parse_scbl_config(&TEST_CONFIG.replace("Official","MOD")).unwrap();
        assert!(mod_cfg.release.is_none());
        assert_eq!(mod_cfg.features.fov_factor,1.2);
    }
    #[test]
    fn release_reference_is_typed_and_roundtrips_without_changing_user() {
        let text=format!("{}\n[Release]\nSchema = 1\nPackageRoot = 'C:\\package'\nReleaseId = 'release-1'\nCompatId = 'compat-1'\nManifestSha256 = '{}'\n",TEST_CONFIG,"a".repeat(64));
        let cfg=parse_scbl_config(&text).unwrap();
        assert_eq!(cfg.release.as_ref().unwrap().schema,1);
        assert_eq!(cfg.user.username,"test-user");
        assert_eq!(parse_scbl_config(&toml::to_string(&cfg).unwrap()).unwrap(),cfg);
    }
}
