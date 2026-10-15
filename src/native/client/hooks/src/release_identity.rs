//! Local whole-package verification is performed off DllMain, once per game process.
//! The pipe-bound frame is a trusted deployment reference, not remote attestation.
use std::io::{self,Read};
use std::path::{Path,PathBuf};
use std::sync::OnceLock;
use sha2::{Digest,Sha256};
use server_api::release_catalog::{verify_package,ROLE_PATHS,RELEASE_ID,COMPAT_ID};
use server_api::release_contract::ReleaseIdentity;
use server_api::users::{PackageIdentity,ReleaseComponent};

#[used]
#[no_mangle]
pub static SCBL_MAIN_RELEASE_IDENTITY: &str = server_api::release_catalog::MAIN_MARKER;
static LOADED_MAIN: OnceLock<PathBuf> = OnceLock::new();
static LOADED_GAMEPLAY: OnceLock<PathBuf> = OnceLock::new();
static VERIFIED: OnceLock<Result<PackageIdentity,()>> = OnceLock::new();
fn invalid()->io::Error {io::Error::new(io::ErrorKind::InvalidData,"PACKAGE_IDENTITY_INVALID: reinstall the matched offline package")}
/// Capture the real loaded module identity, not the process EXE or a caller's path.
/// No disk I/O, catalog parsing, hashing or network is performed under the loader lock.
pub(crate) fn capture_loaded_main(module:Option<windows::Win32::Foundation::HMODULE>) {
    use std::os::windows::ffi::OsStringExt;
    if let Some(module)=module {
        let mut path=vec![0u16;32768];
        let count=unsafe {windows::Win32::System::LibraryLoader::GetModuleFileNameW(module,&mut path)} as usize;
        if count>0 && count<path.len() {let _=LOADED_MAIN.set(PathBuf::from(std::ffi::OsString::from_wide(&path[..count])));}
    }
}
/// Observe the HMODULE Windows actually returned, including preloaded modules.
/// Keep this loader-lock path memory-only; hashing happens at login admission.
fn loaded_module_path(module: windows::Win32::Foundation::HMODULE) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if module.0 == 0 { return Err(invalid()); }
    let mut path = vec![0u16; 32768];
    let count = unsafe { windows::Win32::System::LibraryLoader::GetModuleFileNameW(module, &mut path) } as usize;
    if count == 0 || count >= path.len() { return Err(invalid()); }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&path[..count]));
    if !path.is_absolute() { return Err(invalid()); }
    Ok(path)
}
fn pin_gameplay_path(slot: &OnceLock<PathBuf>, path: PathBuf) -> io::Result<()> {
    // The plugin loader retains this module for the process lifetime. Never
    // silently replace its observation with a later request or package path.
    if slot.get_or_init(|| path.clone()) != &path { return Err(invalid()); }
    Ok(())
}
pub(crate) fn capture_loaded_gameplay(module: windows::Win32::Foundation::HMODULE) -> io::Result<()> {
    pin_gameplay_path(&LOADED_GAMEPLAY, loaded_module_path(module)?)
}
fn plain(path:&Path)->io::Result<()> {
    for ancestor in path.ancestors() {
        let info=std::fs::symlink_metadata(ancestor)?;
        if info.file_type().is_symlink(){return Err(invalid());}
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if info.file_attributes()&0x400!=0{return Err(invalid());}
        }
    }
    if !std::fs::metadata(path)?.is_file(){return Err(invalid());}Ok(())
}
fn hash(path:&Path)->io::Result<(u64,Vec<u8>)> {
    plain(path)?;let mut input=std::fs::File::open(path)?;let mut digest=Sha256::new();let mut size=0;
    let mut bytes=[0u8;65536];loop {let n=input.read(&mut bytes)?;if n==0{break;}digest.update(&bytes[..n]);size+=n as u64;}
    Ok((size,digest.finalize().to_vec()))
}
pub(crate) fn to_declaration(digest:String)->PackageIdentity {
    let role=||Some(ReleaseComponent{release_id:RELEASE_ID.into(),compat_id:COMPAT_ID.into()});
    PackageIdentity{schema:1,release_id:RELEASE_ID.into(),compat_id:COMPAT_ID.into(),manifest_sha256:digest,
        launcher:role(),main_dll:role(),gameplay_dll:role(),server:role()}
}
fn verify(reference:Option<&hooks_config::ReleaseConfig>,loaded_main:&Path)->io::Result<PackageIdentity> {
    let reference=reference.ok_or_else(invalid)?;
    // Relative paths must not follow a changed game's working directory.
    if !reference.package_root.is_absolute()||!loaded_main.is_absolute(){return Err(invalid());}
    let declared=ReleaseIdentity::new(reference.schema,&reference.release_id,&reference.compat_id,&reference.manifest_sha256).map_err(|_|invalid())?;
    let actual=verify_package(&reference.package_root)?;
    actual.admit(Some(&declared)).map_err(|_|invalid())?;
    // A correctly catalogued arbitrary package alone does not prove this loaded DLL.
    // Deployed and packaged paths differ intentionally; byte length and hash must match.
    if hash(loaded_main)?!=hash(&reference.package_root.join(ROLE_PATHS[1].1))? {return Err(invalid());}
    Ok(to_declaration(reference.manifest_sha256.clone()))
}
fn verify_admission(
    reference: Option<&hooks_config::ReleaseConfig>,
    loaded_main: &Path,
    gameplay_required: bool,
    loaded_gameplay: Option<&Path>,
) -> io::Result<PackageIdentity> {
    let identity = verify(reference, loaded_main)?;
    match (gameplay_required, loaded_gameplay) {
        (true, Some(path)) if path.is_absolute() => {
            let root = &reference.ok_or_else(invalid)?.package_root;
            if hash(path)? != hash(&root.join(ROLE_PATHS[2].1))? { return Err(invalid()); }
        }
        // Official verifies the packaged role but must not load a gameplay DLL.
        (false, None) => {}
        _ => return Err(invalid()),
    }
    Ok(identity)
}
pub(crate) fn declaration()->io::Result<PackageIdentity> {
    // The same snapshot is retained across login/relogin with the existing process nonce.
    // Failure also remains failure; replacing metadata cannot upgrade this running process.
    VERIFIED.get_or_init(|| {
        let cfg=hooks_config::get().ok_or(())?;
        let main=LOADED_MAIN.get().ok_or(())?;
        verify_admission(cfg.release.as_ref(),main,cfg.gameplay_mode == hooks_config::GameplayMode::Mod,
            LOADED_GAMEPLAY.get().map(PathBuf::as_path)).map_err(|_|())
    }).clone().map_err(|()|invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {root:PathBuf,loaded:PathBuf,reference:hooks_config::ReleaseConfig}
    impl Fixture {
        fn new()->Self {
            use std::sync::atomic::{AtomicU64,Ordering};static NEXT:AtomicU64=AtomicU64::new(0);
            let root=std::env::temp_dir().join(format!("scbl-release-provider-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
            std::fs::create_dir_all(&root).unwrap();
            let mut rows=Vec::new();
            for (role,path) in ROLE_PATHS {
                let file=root.join(path);std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                let bytes=format!("MZ fixture {}",server_api::release_catalog::role_marker(role).unwrap()).into_bytes();
                std::fs::write(&file,&bytes).unwrap();
                let digest=Sha256::digest(&bytes).iter().map(|v|format!("{v:02x}")).collect::<String>();
                rows.push(format!(r#"{{"role":"{role}","path":"{path}","release_id":"{RELEASE_ID}","compat_id":"{COMPAT_ID}","bytes":{},"sha256":"{digest}"}}"#,bytes.len()));
            }
            let raw=format!(r#"{{"schema":1,"release_id":"{RELEASE_ID}","compat_id":"{COMPAT_ID}","components":[{}]}}"#,rows.join(","));
            std::fs::create_dir_all(root.join("package")).unwrap();
            std::fs::write(root.join("package/RELEASE-CATALOG.json"),&raw).unwrap();
            let digest=Sha256::digest(raw.as_bytes()).iter().map(|v|format!("{v:02x}")).collect();
            let loaded=root.join("game-copy/uplay_r1_loader.dll");std::fs::create_dir_all(loaded.parent().unwrap()).unwrap();std::fs::copy(root.join(ROLE_PATHS[1].1),&loaded).unwrap();
            Self{root:root.clone(),loaded,reference:hooks_config::ReleaseConfig{schema:1,package_root:root,release_id:RELEASE_ID.into(),compat_id:COMPAT_ID.into(),manifest_sha256:digest}}
        }
    }
    impl Drop for Fixture {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.root);}}
    #[test] fn exact_deployed_main_and_all_roles_are_required_without_loading_gameplay() {
        let f=Fixture::new();let identity=verify(Some(&f.reference),&f.loaded).unwrap();
        assert_eq!(identity,to_declaration(f.reference.manifest_sha256.clone()));
        // The gameplay fixture is data, not a loadable DLL. Official must still inspect it.
        std::fs::write(f.root.join(ROLE_PATHS[2].1),"tampered").unwrap();
        assert!(verify(Some(&f.reference),&f.loaded).is_err());
    }
    #[test] fn catalog_package_does_not_substitute_for_loaded_main() {
        let f=Fixture::new();std::fs::write(&f.loaded,"stale main DLL").unwrap();assert!(verify(Some(&f.reference),&f.loaded).is_err());
    }
    #[test] fn missing_or_changed_declaration_cannot_upgrade_loaded_game() {
        let mut f=Fixture::new();assert!(verify(None,&f.loaded).is_err());
        f.reference.manifest_sha256="0".repeat(64);assert!(verify(Some(&f.reference),&f.loaded).is_err());
        f.reference.package_root=PathBuf::from(".");assert!(verify(Some(&f.reference),&f.loaded).is_err());
    }

    fn gameplay_copy(f: &Fixture, name: &str) -> PathBuf {
        let path=f.root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(f.root.join(ROLE_PATHS[2].1),&path).unwrap();
        path
    }
    #[test] fn mod_requires_observed_gameplay_not_only_package_copy() {
        let f=Fixture::new();
        assert!(verify_admission(Some(&f.reference),&f.loaded,true,None).is_err());
    }
    #[test] fn matched_gameplay_deployed_outside_package_is_admitted() {
        let f=Fixture::new();let gameplay=gameplay_copy(&f,"deployed/scbl_gameplay.dll");
        assert!(verify_admission(Some(&f.reference),&f.loaded,true,Some(&gameplay)).is_ok());
    }
    #[test] fn stale_override_dll_is_not_substituted_by_current_package_copy() {
        let f=Fixture::new();let gameplay=gameplay_copy(&f,"override/scbl_gameplay.dll");
        std::fs::write(&gameplay,"old gameplay").unwrap();
        assert!(verify_admission(Some(&f.reference),&f.loaded,true,Some(&gameplay)).is_err());
    }
    #[test] fn same_length_changed_gameplay_is_rejected() {
        let f=Fixture::new();let gameplay=gameplay_copy(&f,"override/scbl_gameplay.dll");
        let mut bytes=std::fs::read(&gameplay).unwrap();bytes[0]^=1;
        std::fs::write(&gameplay,bytes).unwrap();
        assert!(verify_admission(Some(&f.reference),&f.loaded,true,Some(&gameplay)).is_err());
    }
    #[test] fn actual_preloaded_path_wins_over_requested_matched_path() {
        let f=Fixture::new();let requested=gameplay_copy(&f,"requested/scbl_gameplay.dll");
        let actual=gameplay_copy(&f,"preloaded/scbl_gameplay.dll");
        std::fs::write(&actual,"preloaded different implementation").unwrap();
        assert!(verify_admission(Some(&f.reference),&f.loaded,true,Some(&requested)).is_ok());
        assert!(verify_admission(Some(&f.reference),&f.loaded,true,Some(&actual)).is_err());
    }
    #[test] fn missing_relative_or_directory_gameplay_is_rejected() {
        let f=Fixture::new();
        for path in [f.root.join("missing.dll"),PathBuf::from("scbl_gameplay.dll"),f.root.clone()] {
            assert!(verify_admission(Some(&f.reference),&f.loaded,true,Some(&path)).is_err(),"{path:?}");
        }
    }
    #[test] fn official_requires_no_gameplay_load_but_still_verifies_packaged_role() {
        let f=Fixture::new();
        assert!(verify_admission(Some(&f.reference),&f.loaded,false,None).is_ok());
        let gameplay=gameplay_copy(&f,"unexpected/scbl_gameplay.dll");
        assert!(verify_admission(Some(&f.reference),&f.loaded,false,Some(&gameplay)).is_err());
        std::fs::write(f.root.join(ROLE_PATHS[2].1),"tampered").unwrap();
        assert!(verify_admission(Some(&f.reference),&f.loaded,false,None).is_err());
    }
    #[test] fn gameplay_directory_junction_is_rejected_without_reading_foreign_data() {
        let f=Fixture::new();let path=gameplay_copy(&f,"actual/scbl_gameplay.dll");
        let junction=f.root.join("linked");
        let status=std::process::Command::new("cmd.exe").args(["/d","/c","mklink","/J"])
            .arg(&junction).arg(path.parent().unwrap()).output().unwrap();
        assert!(status.status.success(),"{}",String::from_utf8_lossy(&status.stderr));
        let result=verify_admission(Some(&f.reference),&f.loaded,true,Some(&junction.join("scbl_gameplay.dll")));
        std::fs::remove_dir(&junction).unwrap();
        assert!(result.is_err());
    }
    #[test] fn producer_uses_loaded_module_handle_before_plugin_initialize() {
        let loader=include_str!("plugin_loader.rs");
        let load=loader.find("let module = LoadLibraryW").unwrap();
        let capture=loader.find("capture_loaded_gameplay(module)").expect("actual module capture absent");
        let initialize=loader.find("let result = initialize(&host, config);").unwrap();
        assert!(load<capture && capture<initialize);
    }
    #[test] fn declaration_uses_mode_and_captured_gameplay_off_loader_lock() {
        let provider=include_str!("release_identity.rs");
        let body=provider.split("pub(crate) fn declaration()").nth(1).unwrap().split("#[cfg(test)]").next().unwrap();
        assert!(body.contains("verify_admission("));
        assert!(body.contains("GameplayMode::Mod"));
        assert!(body.contains("LOADED_GAMEPLAY.get()"));
    }

    #[test] fn module_capture_uses_real_windows_handle_not_requested_file() {
        use windows::core::w;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        let handle = unsafe { GetModuleHandleW(w!("kernel32.dll")) }.unwrap();
        let path = loaded_module_path(handle).unwrap();
        assert!(path.is_absolute());
        assert_eq!(path.file_name().unwrap().to_string_lossy().to_lowercase(), "kernel32.dll");
        assert!(loaded_module_path(windows::Win32::Foundation::HMODULE(0)).is_err());
    }
    #[test] fn captured_gameplay_cannot_be_replaced_by_later_request() {
        let slot = OnceLock::new();
        let original = PathBuf::from(r"C:\observed\scbl_gameplay.dll");
        assert!(pin_gameplay_path(&slot, original.clone()).is_ok());
        assert!(pin_gameplay_path(&slot, original.clone()).is_ok());
        assert!(pin_gameplay_path(&slot, PathBuf::from(r"C:\different\scbl_gameplay.dll")).is_err());
        assert_eq!(slot.get(), Some(&original));
    }

    #[test] fn package_root_is_not_metadata_directory_and_root_catalog_is_not_fallback() {
        let mut f=Fixture::new();assert!(verify(Some(&f.reference),&f.loaded).is_ok());
        let catalog=f.root.join("package/RELEASE-CATALOG.json");
        std::fs::write(f.root.join("RELEASE-CATALOG.json"),"stale root metadata").unwrap();
        assert!(verify(Some(&f.reference),&f.loaded).is_ok());
        std::fs::copy(&catalog,f.root.join("RELEASE-CATALOG.json")).unwrap();
        std::fs::remove_file(catalog).unwrap();
        assert!(verify(Some(&f.reference),&f.loaded).is_err());
        f.reference.package_root=f.root.join("package");
        assert!(verify(Some(&f.reference),&f.loaded).is_err());
    }
}
