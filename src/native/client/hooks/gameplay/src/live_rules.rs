//! Permanent host owns hooks, native objects and state buffers; children implement
//! either legacy pure rules or bounded stateful feature dispatch.
//! A worker validates sibling DLL+SHA, loads a hash-named shadow, then publishes
//! one immutable table. Old generations remain mapped until process exit, so a
//! callback can finish on its original table without FreeLibrary/use-after-free.
//! Limit distinct loads to bound retained mappings. Never join under DllMain.
use super::feature_contract::*;
use super::feature_runtime::Runtime;
use super::rule_contract::*;
use super::rule_defaults;
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::TryLockError;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

const FILE_NAME: &str = "scbl_mod_rules.dll";
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOADED: usize = 64;
type Published = Arc<RwLock<Option<Arc<Loaded>>>>;
static ACTIVE: OnceLock<Published> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, output: *mut u16, length: u32) -> u32;
    fn LoadLibraryW(path: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

struct Loaded {
    // The OS mapping is deliberately retained for this process, including after
    // table replacement. No destructor may unload a module on the game thread.
    _module: usize,
    digest: String,
    api: RuleApi,
    runtime: Option<Runtime>,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|_| "source-unavailable".to_string())?;
    if file.metadata().map_err(|_| "source-metadata")?.len() > max {
        return Err("source-too-large".into());
    }
    let mut data = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut data)
        .map_err(|_| "source-read")?;
    if data.len() as u64 > max {
        return Err("source-too-large".into());
    }
    Ok(data)
}

fn verified_source(root: &Path) -> Result<(Vec<u8>, String), String> {
    // Only this explicitly resolved host directory. No Desktop, environment,
    // working-directory, old mod-runtime or cross-directory fallback.
    let sidecar = root.join(format!("{FILE_NAME}.sha256"));
    let before = read_bounded(&sidecar, 96)?;
    let text = std::str::from_utf8(&before)
        .map_err(|_| "sha-encoding")?
        .trim();
    if text.len() != 64 || !text.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("sha-format".into());
    }
    let data = read_bounded(&root.join(FILE_NAME), MAX_BYTES)?;
    let after = read_bounded(&sidecar, 96)?;
    let digest = sha(&data);
    if before != after || !text.eq_ignore_ascii_case(&digest) {
        return Err("sha-mismatch-or-update-in-progress".into());
    }
    if !x86_dll(&data) {
        return Err("not-x86-dll".into());
    }
    Ok((data, digest))
}

fn x86_dll(data: &[u8]) -> bool {
    if data.len() < 64 || &data[..2] != b"MZ" {
        return false;
    }
    let offset = u32::from_le_bytes(data[60..64].try_into().unwrap()) as usize;
    let Some(header) = offset.checked_add(24).and_then(|end| data.get(offset..end)) else {
        return false;
    };
    &header[..4] == b"PE\0\0"
        && header[4..6] == [0x4c, 0x01]
        && u16::from_le_bytes(header[22..24].try_into().unwrap()) & 0x2000 != 0
}

fn valid_api(api: &RuleApi) -> bool {
    api.abi == RULE_ABI
        && api.size == std::mem::size_of::<RuleApi>() as u32
        && api.revision != 0
        && api.capabilities == (CAP_NAMEPLATES | CAP_ATS_RANGE)
        && api.nameplate.is_some()
        && api.ats_range.is_some()
}

unsafe fn valid_outputs(api: &RuleApi) -> bool {
    let nameplate = api.nameplate.unwrap();
    for input in [
        NameplateInput::default(),
        NameplateInput {
            viewer_role: 2,
            target_role: 1,
            bms_equipped: 1,
            intel_marked: 0,
        },
        NameplateInput {
            viewer_role: 1,
            target_role: 2,
            bms_equipped: 0,
            intel_marked: 1,
        },
    ] {
        let output = nameplate(&input);
        if output > 1 || (input.viewer_role == 0 && output != 0) {
            return false;
        }
    }
    let range = api.ats_range.unwrap();
    for shoes in [0, 1] {
        for native in [0.0, 20.0, 1000.0] {
            let result = range(&AtsInput {
                native_range: native,
                silent_shoes: shoes,
            });
            if !result.is_finite() || result < 0.0 || result > native {
                return false;
            }
            if shoes == 0 && result != native {
                return false;
            }
        }
    }
    true
}

struct Manager {
    published: Published,
    pending: Option<(String, u8)>,
    rejected: Option<String>,
    retained_modules: Vec<usize>,
    valid_generations: Vec<Arc<Loaded>>,
}

impl Manager {
    fn new(published: Published) -> Self {
        Self {
            published,
            pending: None,
            rejected: None,
            retained_modules: Vec::new(),
            valid_generations: Vec::new(),
        }
    }
    fn observe(&mut self, digest: &str) -> bool {
        if self
            .published
            .read()
            .ok()
            .and_then(|a| a.as_ref().map(|x| x.digest == digest))
            .unwrap_or(false)
            || self.rejected.as_deref() == Some(digest)
        {
            self.pending = None;
            return false;
        }
        let count = self
            .pending
            .as_ref()
            .filter(|(hash, _)| hash == digest)
            .map_or(1, |(_, count)| count.saturating_add(1));
        self.pending = Some((digest.into(), count));
        count >= 2
    }
    fn poll(&mut self, root: &Path) -> Result<Option<(u32, String)>, String> {
        let (bytes, digest) = match verified_source(root) {
            Ok(value) => value,
            Err(error) => {
                self.pending = None;
                return Err(error);
            }
        };
        if !self.observe(&digest) {
            return Ok(None);
        }
        self.pending = None;
        let existing = self
            .valid_generations
            .iter()
            .find(|x| x.digest == digest)
            .cloned();
        let loaded = if let Some(loaded) = existing {
            loaded
        } else {
            match self.load(root, bytes, &digest) {
                Ok(loaded) => loaded,
                Err(reason) => {
                    self.rejected = Some(digest);
                    return Err(reason);
                }
            }
        };
        // Never wait on game callbacks or replace their state mid-call.
        let mut guard = match self.published.try_write() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                self.pending = Some((digest, 2));
                return Ok(None);
            }
            Err(TryLockError::Poisoned(_)) => return Err("publication-lock".into()),
        };
        if let Some(runtime) = &loaded.runtime {
            let old = guard.as_ref().and_then(|x| x.runtime.as_ref());
            if let Err(reason) = runtime.migrate_from(old) {
                self.rejected = Some(digest);
                return Err(reason.into());
            }
        }
        *guard = Some(loaded.clone());
        self.rejected = None;
        Ok(Some((
            loaded
                .runtime
                .as_ref()
                .map_or(loaded.api.revision, |r| r.api.revision),
            loaded.digest.clone(),
        )))
    }
    fn load(&mut self, root: &Path, bytes: Vec<u8>, digest: &str) -> Result<Arc<Loaded>, String> {
        if self.retained_modules.len() >= MAX_LOADED {
            return Err("generation-limit-restart-required".into());
        }
        let cache = root
            .join(".scbl-rule-cache")
            .join(std::process::id().to_string());
        fs::create_dir_all(&cache).map_err(|_| "shadow-directory")?;
        let shadow = cache.join(format!("rules-{digest}.dll"));
        if !shadow.exists() {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&shadow)
                .map_err(|_| "shadow-create")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "shadow-write")?;
        }
        if read_bounded(&shadow, MAX_BYTES)? != bytes {
            return Err("shadow-hash-mismatch".into());
        }
        let wide: Vec<u16> = shadow.as_os_str().encode_wide().chain(Some(0)).collect();
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            return Err("load-library".into());
        }
        self.retained_modules.push(module as usize);
        let address = unsafe { GetProcAddress(module, RULE_EXPORT.as_ptr()) };
        if address.is_null() {
            return Err("missing-rules-export".into());
        }
        let get: GetApi = unsafe { std::mem::transmute(address) };
        let mut api = RuleApi::default();
        let result = unsafe { get(RULE_ABI, &mut api, std::mem::size_of::<RuleApi>() as u32) };
        if result != 0 || !valid_api(&api) {
            return Err("rules-abi".into());
        }
        if !unsafe { valid_outputs(&api) } {
            return Err("rules-output-contract".into());
        }
        // Extension is optional only when absent (legacy rules). A present but
        // malformed extension rejects the WHOLE generation, never half-publishes.
        let feature_address = unsafe { GetProcAddress(module, FEATURE_EXPORT.as_ptr()) };
        let runtime = if feature_address.is_null() {
            None
        } else {
            let get: GetFeatureApi = unsafe { std::mem::transmute(feature_address) };
            let mut feature_api = FeatureApi::default();
            if unsafe {
                get(
                    FEATURE_ABI,
                    &mut feature_api,
                    std::mem::size_of::<FeatureApi>() as u32,
                )
            } != 0
            {
                return Err("features-get-api".into());
            }
            Some(Runtime::new(feature_api).map_err(str::to_owned)?)
        };
        let loaded = Arc::new(Loaded {
            _module: module as usize,
            digest: digest.into(),
            api,
            runtime,
        });
        self.valid_generations.push(loaded.clone());
        Ok(loaded)
    }
}

// The read lease covers the whole native->child call, not just table lookup.
// Worker activation is try_write; it cannot migrate state until readers drain.
fn with_generation<T>(state: &Published, f: impl FnOnce(&Loaded) -> T) -> Option<T> {
    let lease = state.try_read().ok()?;
    let loaded = lease.as_ref()?;
    Some(f(loaded))
}
pub(crate) fn nameplate(input: NameplateInput) -> bool {
    let fallback = || rule_defaults::nameplate(input) == 1;
    if !development_reload_enabled() {
        return fallback();
    }
    let state = active();
    with_generation(state, |active| {
        if let Some(runtime) = &active.runtime {
            let mut packet = FeaturePacket::new(EVENT_NAMEPLATE);
            packet.words[..4].copy_from_slice(&[
                input.viewer_role,
                input.target_role,
                input.bms_equipped,
                input.intel_marked,
            ]);
            return runtime
                .invoke(packet)
                .map_or_else(fallback, |p| p.words[0] == 1);
        }
        match unsafe { active.api.nameplate.unwrap()(&input) } {
            0 => false,
            1 => true,
            _ => fallback(),
        }
    })
    .unwrap_or_else(fallback)
}

// Stable DX11 adapter still selects native ATS event kinds (not event producers).
pub(crate) fn ats_range(input: AtsInput) -> f32 {
    let fallback = rule_defaults::ats_range(input);
    if !development_reload_enabled() {
        return fallback;
    }
    let state = active();
    with_generation(state, |active| {
        if let Some(runtime) = &active.runtime {
            let mut packet = FeaturePacket::new(EVENT_ATS_RANGE);
            packet.words[..2].copy_from_slice(&[input.native_range.to_bits(), input.silent_shoes]);
            return runtime
                .invoke(packet)
                .map_or(fallback, |p| f32::from_bits(p.words[0]));
        }
        let result = unsafe { active.api.ats_range.unwrap()(&input) };
        if result.is_finite()
            && result >= 0.0
            && result <= input.native_range
            && (input.silent_shoes == 1 || result == input.native_range)
        {
            result
        } else {
            fallback
        }
    })
    .unwrap_or(fallback)
}

fn host_directory() -> Option<PathBuf> {
    unsafe {
        let mut module = std::ptr::null_mut();
        // Pin the permanent host before starting its process-lifetime worker.
        if GetModuleHandleExW(5, host_directory as *const () as *const u16, &mut module) == 0 {
            return None;
        }
        let mut wide = vec![0u16; 32768];
        let n = GetModuleFileNameW(module, wide.as_mut_ptr(), wide.len() as u32) as usize;
        if n == 0 || n >= wide.len() {
            return None;
        }
        PathBuf::from(std::ffi::OsString::from_wide(&wide[..n]))
            .parent()
            .map(Path::to_owned)
    }
}

fn builtin() -> Arc<Loaded> {
    Arc::new(Loaded {
        _module: 0,
        digest: "builtin-r38".into(),
        api: RuleApi {
            revision: 38,
            ..RuleApi::default()
        },
        runtime: Some(
            Runtime::new(super::feature_defaults::api(38))
                .expect("compiled gameplay policy must pass its own ABI probes"),
        ),
    })
}
fn active() -> &'static Published {
    ACTIVE.get_or_init(|| Arc::new(RwLock::new(Some(builtin()))))
}
pub(crate) fn development_reload_enabled() -> bool {
    cfg!(feature = "dev-hot-reload")
}
pub(crate) fn init() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let state = active().clone();
    if !development_reload_enabled() {
        crate::host_log(
            hooks_plugin_api::LOG_INFO,
            "SCBL_RULES source=builtin revision=38 development_reload=false",
        );
        return;
    }
    let Some(root) = host_directory() else {
        crate::host_log(
            hooks_plugin_api::LOG_WARNING,
            "SCBL_RULES fallback=builtin reason=host-directory",
        );
        return;
    };
    let result=std::thread::Builder::new().name("scbl-rules".into()).spawn(move || {
        let mut manager=Manager::new(state);
        let mut last_error=None;
        while !STOP.load(Ordering::Acquire) {
            match manager.poll(&root) {
                Ok(Some((revision,digest))) => {
                    last_error=None;
                    crate::host_log(hooks_plugin_api::LOG_INFO,&format!("SCBL_RULES switched=true revision={revision} sha256={digest} source=host-sibling native_hooks=unchanged publication=drained-state-transaction"));
                }
                Err(reason) if last_error.as_ref() != Some(&reason) => {
                    crate::host_log(hooks_plugin_api::LOG_WARNING,&format!("SCBL_RULES switched=false keep=current-or-builtin reason={reason}"));
                    last_error=Some(reason);
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    if result.is_err() {
        crate::host_log(
            hooks_plugin_api::LOG_WARNING,
            "SCBL_RULES fallback=builtin reason=worker-start",
        );
    }
}
pub(crate) fn stop() {
    // No join, FreeLibrary, or wait for game callbacks under loader lock.
    STOP.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_has_a_complete_builtin_without_loading_a_child() {
        assert_eq!(
            development_reload_enabled(),
            cfg!(feature = "dev-hot-reload")
        );
        let b = builtin();
        assert_eq!(b._module, 0);
        let runtime = b.runtime.as_ref().unwrap();
        assert_eq!(runtime.api.events, SUPPORTED_EVENTS);
        let mut p = FeaturePacket::new(EVENT_MELEE_SPEED);
        p.words[0] = 1;
        assert_eq!(f32::from_bits(runtime.invoke(p).unwrap().words[0]), 1.20);
        p = FeaturePacket::new(EVENT_DRONE_EMP);
        p.words[..2].copy_from_slice(&[100, 100]);
        assert_eq!(runtime.invoke(p).unwrap().words[0], 50);
        p = FeaturePacket::new(EVENT_RFD);
        p.words[..4].copy_from_slice(&[1, 1, 1, 8]);
        assert_eq!(runtime.invoke(p).unwrap().words[0], 1);
    }
    #[test]
    fn source_fingerprint_requires_two_stable_observations() {
        let mut m = Manager::new(Arc::new(RwLock::new(None)));
        assert!(!m.observe("a"));
        assert!(!m.observe("b"));
        assert!(m.observe("b"));
        m.rejected = Some("b".into());
        assert!(!m.observe("b"));
        assert!(!m.observe("a"));
        assert!(m.observe("a"));
    }
    #[test]
    fn malformed_pe_headers_fail_before_library_load() {
        for bytes in [vec![], vec![0; 64], vec![0xff; 128]] {
            assert!(!x86_dll(&bytes));
        }
        let mut p = vec![0; 88];
        p[..2].copy_from_slice(b"MZ");
        p[60..64].copy_from_slice(&64u32.to_le_bytes());
        p[64..68].copy_from_slice(b"PE\0\0");
        p[68..70].copy_from_slice(&0x14cu16.to_le_bytes());
        p[86..88].copy_from_slice(&0x2000u16.to_le_bytes());
        assert!(x86_dll(&p));
        p[68] = 0x64;
        assert!(!x86_dll(&p));
        p[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!x86_dll(&p));
    }
    #[test]
    fn empty_or_incompatible_api_cannot_be_published() {
        let mut api = RuleApi::default();
        assert!(!valid_api(&api));
        api.abi = RULE_ABI;
        api.size = std::mem::size_of::<RuleApi>() as u32;
        api.capabilities = 3;
        api.revision = 1;
        assert!(!valid_api(&api));
    }
    #[test]
    #[ignore = "requires generated, x86 native DLL fixtures; run explicitly"]
    fn actual_dll_switch_rejection_and_rollback_in_one_process() {
        let fixtures = PathBuf::from(
            std::env::var("SCBL_RULE_TEST_FIXTURES").expect("fixture directory required"),
        );
        let root = fixtures.join(format!("same-process-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let publish = |name: &str| {
            let bytes = fs::read(fixtures.join(name)).unwrap();
            fs::write(root.join(FILE_NAME), &bytes).unwrap();
            fs::write(root.join(format!("{FILE_NAME}.sha256")), sha(&bytes)).unwrap();
        };
        let shared: Published = Arc::new(RwLock::new(None));
        let mut manager = Manager::new(shared.clone());
        publish("rules-a.dll");
        assert!(manager.poll(&root).unwrap().is_none());
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 1001);
        let old = shared.read().unwrap().clone().unwrap();
        let input = AtsInput {
            native_range: 20.0,
            silent_shoes: 1,
        };
        assert_eq!(unsafe { old.api.ats_range.unwrap()(&input) }, 20.0);
        publish("rules-b.dll");
        assert!(manager.poll(&root).unwrap().is_none());
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 1002);
        let new = shared.read().unwrap().clone().unwrap();
        assert_eq!(unsafe { new.api.ats_range.unwrap()(&input) }, 10.0);
        assert_eq!(unsafe { old.api.ats_range.unwrap()(&input) }, 20.0); // in-flight old generation still valid
        fs::write(root.join(format!("{FILE_NAME}.sha256")), "incomplete").unwrap();
        assert!(manager.poll(&root).is_err());
        assert_eq!(shared.read().unwrap().as_ref().unwrap().api.revision, 1002);
        publish("rules-bad-abi.dll");
        manager.poll(&root).unwrap();
        assert!(manager.poll(&root).is_err());
        assert_eq!(shared.read().unwrap().as_ref().unwrap().api.revision, 1002);
        assert!(manager.poll(&root).unwrap().is_none()); // rejected fingerprint is not retried
        publish("rules-bad-output.dll");
        manager.poll(&root).unwrap();
        assert!(manager.poll(&root).is_err());
        assert_eq!(shared.read().unwrap().as_ref().unwrap().api.revision, 1002);
        publish("rules-a.dll");
        manager.poll(&root).unwrap();
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 1001);
        assert_eq!(manager.retained_modules.len(), 4); // rollback reused original module, no extra load
        println!("HOT_RELOAD_PROOF pid={} generation=1001 result=20 -> generation=1002 result=10; old-call=20; bad-SHA/ABI/output retained1002; rollback1001 result=20",std::process::id());
    }
    #[test]
    #[ignore = "requires r25 generated x86 DLL fixtures; run explicitly"]
    fn stateful_native_dll_handoff_drains_calls_and_preserves_state() {
        let fixtures = PathBuf::from(std::env::var("SCBL_FEATURE_TEST_FIXTURES").unwrap());
        let root = fixtures.join(format!("same-process-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let publish = |name: &str| {
            let bytes = fs::read(fixtures.join(name)).unwrap();
            fs::write(root.join(FILE_NAME), &bytes).unwrap();
            fs::write(root.join(format!("{FILE_NAME}.sha256")), sha(&bytes)).unwrap();
        };
        let shared: Published = Arc::new(RwLock::new(None));
        let mut manager = Manager::new(shared.clone());
        publish("features-a.dll");
        assert!(manager.poll(&root).unwrap().is_none());
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 2501);
        let old = shared.read().unwrap().clone().unwrap();
        let started: unsafe extern "system" fn() -> u32 = unsafe {
            std::mem::transmute(GetProcAddress(
                old._module as _,
                b"fixture_started\0".as_ptr(),
            ))
        };
        let release: unsafe extern "system" fn() = unsafe {
            std::mem::transmute(GetProcAddress(
                old._module as _,
                b"fixture_release\0".as_ptr(),
            ))
        };
        let mut p = FeaturePacket::new(EVENT_ATS_RANGE);
        p.words[0] = 20.0f32.to_bits();
        p.words[1] = 1;
        p.words[15] = 1;
        let calls = shared.clone();
        let call = std::thread::spawn(move || {
            with_generation(&calls, |active| {
                active.runtime.as_ref().unwrap().invoke(p).unwrap()
            })
            .unwrap()
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while unsafe { started() } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture callback did not start"
            );
            std::thread::yield_now();
        }
        publish("features-b.dll");
        assert!(manager.poll(&root).unwrap().is_none());
        assert!(manager.poll(&root).unwrap().is_none()); // a real A callback is in flight
        assert_eq!(
            shared
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .runtime
                .as_ref()
                .unwrap()
                .api
                .revision,
            2501
        );
        unsafe { release() };
        assert_eq!(f32::from_bits(call.join().unwrap().words[0]), 10.0);
        assert_eq!(old.runtime.as_ref().unwrap().state().words[0], 1);
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 2502);
        p.words[15] = 0;
        let b = shared.read().unwrap().clone().unwrap();
        assert_eq!(b.runtime.as_ref().unwrap().state().words[0], 1);
        let result =
            with_generation(&shared, |g| g.runtime.as_ref().unwrap().invoke(p).unwrap()).unwrap();
        assert_eq!(f32::from_bits(result.words[0]), 5.0); // new implementation code
        assert_eq!(b.runtime.as_ref().unwrap().state().words[0], 2);
        for (file, reason) in [
            ("features-bad-prepare.dll", "features-state-migration"),
            ("features-bad-abi.dll", "features-abi"),
            ("features-bad-output.dll", "features-output-contract"),
        ] {
            publish(file);
            manager.poll(&root).unwrap();
            assert_eq!(manager.poll(&root).unwrap_err(), reason);
            assert_eq!(shared.read().unwrap().as_ref().unwrap().digest, b.digest);
            assert_eq!(b.runtime.as_ref().unwrap().state().words[0], 2);
        }
        fs::write(root.join(format!("{FILE_NAME}.sha256")), "partial").unwrap();
        assert!(manager.poll(&root).is_err());
        assert_eq!(shared.read().unwrap().as_ref().unwrap().digest, b.digest);
        publish("features-a.dll");
        manager.poll(&root).unwrap();
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 2501);
        assert_eq!(old.runtime.as_ref().unwrap().state().words[0], 2); // migrate B -> A, not stale1
        let result =
            with_generation(&shared, |g| g.runtime.as_ref().unwrap().invoke(p).unwrap()).unwrap();
        assert_eq!(f32::from_bits(result.words[0]), 10.0);
        assert_eq!(old.runtime.as_ref().unwrap().state().words[0], 3);
        assert_eq!(manager.retained_modules.len(), 5);
        println!("STATEFUL_HOT_PROOF pid={} A2501=10; in-flight call defers B; drained handoff state1; B2502=5/state2; bad migration/ABI/output/SHA retained B/state2; rollback A=10/state3; mappings=5 no unload",std::process::id());
    }
    #[test]
    #[ignore = "requires the frozen release rules DLL; run explicitly after build"]
    fn actual_release_child_has_legacy_parity_and_stateful_dispatch() {
        let source = PathBuf::from(std::env::var("SCBL_RELEASE_RULES_DLL").unwrap());
        let root = source
            .parent()
            .unwrap()
            .join(format!("release-proof-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let bytes = fs::read(&source).unwrap();
        fs::write(root.join(FILE_NAME), &bytes).unwrap();
        fs::write(root.join(format!("{FILE_NAME}.sha256")), sha(&bytes)).unwrap();
        let shared: Published = Arc::new(RwLock::new(None));
        let mut manager = Manager::new(shared.clone());
        manager.poll(&root).unwrap();
        assert_eq!(manager.poll(&root).unwrap().unwrap().0, 32);
        let loaded = shared.read().unwrap().clone().unwrap();
        assert_eq!(loaded.api.revision, 22); // still usable by deployed r24 host
        for viewer in 0..=3 {
            for target in 0..=3 {
                for bms in 0..=1 {
                    for intel in 0..=1 {
                        let input = NameplateInput {
                            viewer_role: viewer,
                            target_role: target,
                            bms_equipped: bms,
                            intel_marked: intel,
                        };
                        let expected = rule_defaults::nameplate(input);
                        assert_eq!(unsafe { loaded.api.nameplate.unwrap()(&input) }, expected);
                        let mut p = FeaturePacket::new(EVENT_NAMEPLATE);
                        p.words[..4].copy_from_slice(&[viewer, target, bms, intel]);
                        assert_eq!(
                            with_generation(&shared, |g| g
                                .runtime
                                .as_ref()
                                .unwrap()
                                .invoke(p)
                                .unwrap())
                            .unwrap()
                            .words[0],
                            expected
                        );
                    }
                }
            }
        }
        for native in [0.0f32, 20.0, 1500.0, 6000.0] {
            for shoes in [0, 1] {
                let input = AtsInput {
                    native_range: native,
                    silent_shoes: shoes,
                };
                let expected = rule_defaults::ats_range(input);
                assert_eq!(unsafe { loaded.api.ats_range.unwrap()(&input) }, expected);
                let mut p = FeaturePacket::new(EVENT_ATS_RANGE);
                p.words[..2].copy_from_slice(&[native.to_bits(), shoes]);
                assert_eq!(
                    f32::from_bits(
                        with_generation(&shared, |g| g
                            .runtime
                            .as_ref()
                            .unwrap()
                            .invoke(p)
                            .unwrap())
                        .unwrap()
                        .words[0]
                    ),
                    expected
                );
            }
        }
        assert_eq!(loaded.runtime.as_ref().unwrap().state().words[0], 72);

        let rt = loaded.runtime.as_ref().unwrap();
        let mut speed = FeaturePacket::new(crate::feature_contract::EVENT_MELEE_SPEED);
        speed.words[0] = 1;
        assert_eq!(f32::from_bits(rt.invoke(speed).unwrap().words[0]), 1.3);
        let mut emp = FeaturePacket::new(crate::feature_contract::EVENT_DRONE_EMP);
        for (hp, expected) in [(100, 50), (51, 50), (50, 0), (49, 0)] {
            emp.words[..2].copy_from_slice(&[hp, 100]);
            assert_eq!(rt.invoke(emp).unwrap().words[0], expected);
        }

        let mut melee = FeaturePacket::new(crate::feature_contract::EVENT_MELEE_RANGE);
        melee.words[..3].copy_from_slice(&[270.0f32.to_bits(), 1.5f32.to_bits(), 1]);
        assert_eq!(f32::from_bits(rt.invoke(melee).unwrap().words[0]), 270.0);
        let mut rfd = FeaturePacket::new(crate::feature_contract::EVENT_RFD);
        rfd.words[..4].copy_from_slice(&[1, 1, 1, 8]);
        assert_eq!(rt.invoke(rfd).unwrap().words[0], 0);
        rfd.words[1] = 0;
        assert_eq!(rt.invoke(rfd).unwrap().words[0], 1);
        let mut stamina = FeaturePacket::new(crate::feature_contract::EVENT_STAMINA);
        stamina.words[..5].copy_from_slice(&[
            1,
            7,
            4.0f32.to_bits(),
            10.0f32.to_bits(),
            0.016f32.to_bits(),
        ]);
        assert_eq!(
            f32::from_bits(rt.invoke(stamina).unwrap().words[1]),
            crate::feature_defaults::STAMINA_OPACITY
        );
        stamina.words[2] = 8.0f32.to_bits();
        assert_eq!(
            f32::from_bits(rt.invoke(stamina).unwrap().words[1]),
            crate::feature_defaults::STAMINA_OPACITY
        );
        melee.words[..3].copy_from_slice(&[189.0f32.to_bits(), 1.05f32.to_bits(), 1]);
        assert!((f32::from_bits(rt.invoke(melee).unwrap().words[0]) - 189.0).abs() < 0.001);
        rfd.words[..4].copy_from_slice(&[1, 1, 3, 9]);
        assert_eq!(rt.invoke(rfd).unwrap().words[0], 0); // native discharge, not just casting
        rfd.words[3] = 1;
        assert_eq!(rt.invoke(rfd).unwrap().words[0], 1); // no cooldown-wide immunity
        rfd.words[3] = 9;

        // Exercise actual predecessor/current DLLs in one process, preserving the
        // visible-at-80% stamina latch across rollback and reapplication.
        let previous = PathBuf::from(std::env::var("SCBL_PREVIOUS_RULES_DLL").unwrap());
        for (path, revision, allowed, opacity, reach) in [
            (
                &previous,
                30,
                0,
                crate::feature_defaults::STAMINA_OPACITY,
                189.0,
            ),
            (
                &source,
                32,
                0,
                crate::feature_defaults::STAMINA_OPACITY,
                189.0,
            ),
        ] {
            let raw = fs::read(path).unwrap();
            fs::write(root.join(FILE_NAME), &raw).unwrap();
            fs::write(root.join(format!("{FILE_NAME}.sha256")), sha(&raw)).unwrap();
            manager.poll(&root).unwrap();
            assert_eq!(manager.poll(&root).unwrap().unwrap().0, revision);
            let generation = shared.read().unwrap().clone().unwrap();
            let active = generation.runtime.as_ref().unwrap();
            let mut head = FeaturePacket::new(crate::feature_contract::EVENT_HEAD_EMP);
            head.words[..4].copy_from_slice(&[
                6f32.to_bits(),
                6f32.to_bits(),
                1f32.to_bits(),
                3f32.to_bits(),
            ]);
            if revision == 30 {
                assert!(active.invoke(head).is_none());
            } else {
                assert_eq!(f32::from_bits(active.invoke(head).unwrap().words[0]), 1.0);
            }
            assert_eq!(f32::from_bits(active.invoke(speed).unwrap().words[0]), 1.3);
            emp.words[0] = 100;
            assert_eq!(active.invoke(emp).unwrap().words[0], 50);
            assert_eq!(active.invoke(rfd).unwrap().words[0], allowed);
            assert_eq!(
                f32::from_bits(active.invoke(stamina).unwrap().words[1]),
                opacity
            );
            assert!((f32::from_bits(active.invoke(melee).unwrap().words[0]) - reach).abs() < 0.001);
        }
        println!("RELEASE_EQUIPMENT_PROOF actual DLL r32 headEMP native-first1.0; rollback r30 head event absent; reapply r32 restored; speed1.3 EMP100->50 unchanged; native reach189 throughout; RFD release9 blocked/cooldown1 native, alpha0.58 and stamina80percent latch preserved throughout");
        println!("RELEASE_CHILD_PROOF revision=32 legacy=22 nameplate64+ATS8 both-dispatch-paths parity; state=72 sha256={}",sha(&bytes));
    }
}

/// Stable builtins are always present; only explicit development builds may replace them.
pub(crate) fn feature(input: FeaturePacket) -> Option<FeaturePacket> {
    // Shipping policies cannot be replaced. Do not let an unrelated HUD state
    // lease drop EMP/melee rules back to native behavior. Keep child generation
    // leases and transactional validation unchanged in explicit development builds.
    if !development_reload_enabled() && input.event != EVENT_STAMINA {
        return super::feature_defaults::stateless(&input)
            .filter(|output| super::feature_runtime::valid_output(&input, output));
    }
    let guard = active().try_read().ok()?;
    let loaded = guard.as_ref()?;
    let runtime = loaded.runtime.as_ref()?;
    if runtime.api.events & input.event == 0 {
        return None;
    }
    runtime.invoke(input)
}

#[cfg(test)]
mod shipping_policy_tests {
    use super::*;
    #[test]
    fn stateless_shipping_rules_survive_busy_state_but_development_keeps_its_lease() {
        let generation = active().read().unwrap().as_ref().unwrap().clone();
        let mut emp = FeaturePacket::new(EVENT_DRONE_EMP);
        emp.words[..2].copy_from_slice(&[100, 100]);
        let mut speed = FeaturePacket::new(EVENT_MELEE_SPEED);
        speed.words[0] = 1;
        generation.runtime.as_ref().unwrap().with_state_lease(|| {
            if development_reload_enabled() {
                assert!(feature(emp).is_none());
                assert!(feature(speed).is_none());
            } else {
                assert_eq!(feature(emp).unwrap().words[0], 50);
                assert_eq!(f32::from_bits(feature(speed).unwrap().words[0]), 1.20);
            }
            assert!(feature(FeaturePacket::new(EVENT_STAMINA)).is_none());
        });
    }
    #[test]
    fn shipping_stateless_rules_do_not_read_the_generation_lock() {
        let _lease = active().write().unwrap();
        let mut emp = FeaturePacket::new(EVENT_DRONE_EMP);
        emp.words[..2].copy_from_slice(&[100, 100]);
        if development_reload_enabled() {
            assert!(feature(emp).is_none());
        } else {
            assert_eq!(feature(emp).unwrap().words[0], 50);
        }
        assert!(nameplate(NameplateInput {
            viewer_role: 2,
            target_role: 1,
            bms_equipped: 1,
            intel_marked: 0
        }));
        assert_eq!(
            ats_range(AtsInput {
                native_range: 20.0,
                silent_shoes: 1
            }),
            10.0
        );
    }
}
