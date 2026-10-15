// File-backed tests of the product startup loader. No DLL is loaded and no socket is opened.
use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture { root: PathBuf }
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("scbl-catalog-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        let mut components = Vec::new();
        for (role, relative) in ROLE_PATHS {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = format!("MZ fixture SCBL_RELEASE_V1|{role}|{RELEASE_ID}|{COMPAT_ID}|END_SCBl artifact").into_bytes();
            fs::write(&path, &bytes).unwrap();
            components.push(serde_json::json!({"role": role,"path": relative,"release_id":RELEASE_ID,"compat_id":COMPAT_ID,"bytes":bytes.len(),"sha256":hex(&Sha256::digest(&bytes))}));
        }
        let data = serde_json::json!({"schema":1,"release_id":RELEASE_ID,"compat_id":COMPAT_ID,"components":components});
        fs::create_dir_all(root.join("package")).unwrap();
        fs::write(root.join("package/RELEASE-CATALOG.json"),serde_json::to_vec_pretty(&data).unwrap()).unwrap();
        Self { root }
    }
    fn load(&self) -> io::Result<ReleaseIdentity> { verify_server_package(&self.root.join("runtime/SCBL.LAN.Server.exe")) }
    fn edit(&self, f: impl FnOnce(&mut serde_json::Value)) {
        let path = self.root.join("package/RELEASE-CATALOG.json");
        let mut data: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        f(&mut data); fs::write(path,serde_json::to_vec(&data).unwrap()).unwrap();
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); } }
#[test]
fn exact_package_yields_immutable_raw_catalog_identity() {
    let f = Fixture::new();
    let digest = hex(&Sha256::digest(&fs::read(f.root.join("package/RELEASE-CATALOG.json")).unwrap()));
    assert_eq!(f.load().unwrap().storage_key(),format!("1|{RELEASE_ID}|{COMPAT_ID}|{digest}"));
}
#[test]
fn missing_and_malformed_catalog_fail_closed() {
    let f = Fixture::new();
    for bytes in [b"{}".as_slice(), b"{broken", b"null"] { fs::write(f.root.join("package/RELEASE-CATALOG.json"),bytes).unwrap(); assert!(f.load().is_err()); }
    fs::remove_file(f.root.join("package/RELEASE-CATALOG.json")).unwrap(); assert!(f.load().is_err());
}
#[test]
fn missing_mixed_duplicate_or_unknown_roles_fail_closed() {
    for mode in 0..6 {
        let f=Fixture::new();
        f.edit(|v| match mode {
            0 => { v["components"].as_array_mut().unwrap().pop(); },
            1 => v["components"][1]["release_id"]="old".into(),
            2 => v["components"][1]["compat_id"]="other".into(),
            3 => v["components"][1]["role"]="launcher".into(),
            4 => v["components"][1]["path"]="../uplay_r1_loader.dll".into(),
            _ => v["unknown"]="ignored?".into(),
        }); assert!(f.load().is_err(),"mode={mode}");
    }
}
#[test]
fn each_role_missing_or_hash_changed_fails_even_for_official() {
    for (_,relative) in ROLE_PATHS { for missing in [true,false] {
        let f=Fixture::new();let path=f.root.join(relative);
        if missing {fs::remove_file(&path).unwrap();} else {let mut b=fs::read(&path).unwrap();b[0]^=1;fs::write(path,b).unwrap();}
        assert!(f.load().is_err(),"{relative} missing={missing}");
    }}
}
#[test]
fn rehashed_old_embedded_identity_is_still_rejected() {
    let f=Fixture::new();let path=f.root.join("runtime/SCBL.LAN.Server.exe");let bytes=format!("MZ SCBL_RELEASE_V1|server|old|{COMPAT_ID}|END_SCBl").into_bytes();
    fs::write(path,&bytes).unwrap();f.edit(|v|{v["components"][3]["bytes"]=(bytes.len() as u64).into();v["components"][3]["sha256"]=hex(&Sha256::digest(&bytes)).into();});assert!(f.load().is_err());
}
#[test]
fn current_executable_must_be_the_catalog_server_role() {
    let f=Fixture::new();let other=f.root.join("other.exe");fs::copy(f.root.join("runtime/SCBL.LAN.Server.exe"),&other).unwrap();
    assert!(verify_server_package(&other).is_err());
}
#[test]
fn catalog_is_bounded_and_duplicate_json_fields_fail() {
    let f=Fixture::new();fs::write(f.root.join("package/RELEASE-CATALOG.json"),vec![b' ';16*1024+1]).unwrap();assert!(f.load().is_err());
    fs::write(f.root.join("package/RELEASE-CATALOG.json"),b"{\"schema\":1,\"schema\":1}").unwrap();assert!(f.load().is_err());
}
#[test]
fn managed_utf16_identity_is_accepted_without_loading_gameplay() {
    let f=Fixture::new();let bytes:Vec<u8>=format!("SCBL_RELEASE_V1|launcher|{RELEASE_ID}|{COMPAT_ID}|END_SCBl").encode_utf16().flat_map(u16::to_le_bytes).collect();
    fs::write(f.root.join("SCBL.LAN.exe"),&bytes).unwrap();f.edit(|v|{v["components"][0]["bytes"]=(bytes.len() as u64).into();v["components"][0]["sha256"]=hex(&Sha256::digest(&bytes)).into();});assert!(f.load().is_ok());
}

#[test]
fn rehashed_foreign_role_marker_is_rejected_ascii_and_utf16() {
    for wide in [false,true] {
        let f=Fixture::new();let path=f.root.join("runtime/scbl_gameplay.dll");
        let mut bytes=fs::read(&path).unwrap();
        let foreign=format!("SCBL_RELEASE_V1|main|{RELEASE_ID}|{COMPAT_ID}|END_SCBl");
        if wide {bytes.extend(foreign.encode_utf16().flat_map(u16::to_le_bytes));} else {bytes.extend(foreign.as_bytes());}
        fs::write(path,&bytes).unwrap();
        f.edit(|v|{v["components"][2]["bytes"]=(bytes.len() as u64).into();v["components"][2]["sha256"]=hex(&Sha256::digest(&bytes)).into();});
        assert!(f.load().is_err());
    }
}

#[test]
fn duplicate_own_marker_fails_and_chunk_split_single_marker_passes() {
    for duplicate in [false,true] {
        let f=Fixture::new();let path=f.root.join("runtime/scbl_gameplay.dll");
        let marker=format!("SCBL_RELEASE_V1|gameplay|{RELEASE_ID}|{COMPAT_ID}|END_SCBl");
        let mut bytes=vec![b'x';65536-15];bytes.extend(marker.as_bytes());
        if duplicate {bytes.extend(marker.as_bytes());}
        fs::write(path,&bytes).unwrap();f.edit(|v|{v["components"][2]["bytes"]=(bytes.len() as u64).into();v["components"][2]["sha256"]=hex(&Sha256::digest(&bytes)).into();});
        assert_eq!(f.load().is_err(),duplicate);
    }
}
#[test]
fn python_producer_catalog_round_trips_through_actual_rust_loader() {
    let f=Fixture::new();
    let golden:serde_json::Value=serde_json::from_str(include_str!("release_catalog_layout_v1_fixture.json")).unwrap();
    for (relative,value) in golden["files"].as_object().unwrap() {
        let path=f.root.join(relative);fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path,value.as_str().unwrap().as_bytes()).unwrap();
    }
    fs::write(f.root.join("package/RELEASE-CATALOG.json"),golden["catalog"].as_str().unwrap().as_bytes()).unwrap();
    let release=golden["release_id"].as_str().unwrap();let compat=golden["compat_id"].as_str().unwrap();
    assert_eq!(verify_package_expected(&f.root,release,compat).unwrap().storage_key(),
        golden["expected_storage_key"].as_str().unwrap());
    if RELEASE_ID != release || COMPAT_ID != compat {assert!(f.load().is_err());}
}
#[test]
fn historical_python_layout_is_rejected_even_with_matching_identity() {
    let f=Fixture::new();
    // Original golden bytes stay immutable. Matching its identity deliberately
    // isolates the rejection to its old component paths, not the release bump.
    let golden:serde_json::Value=serde_json::from_str(include_str!("release_catalog_python_fixture.json")).unwrap();
    for (relative,value) in golden["files"].as_object().unwrap() {
        let path=f.root.join(relative);fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path,value.as_str().unwrap().as_bytes()).unwrap();
    }
    fs::write(f.root.join("package/RELEASE-CATALOG.json"),golden["catalog"].as_str().unwrap().as_bytes()).unwrap();
    let error=verify_package_expected(&f.root,"scbl-lan-1.0.0-beta1","scbl-lan-v1").unwrap_err();
    assert!(error.to_string().contains("mixed role, path or release identity"));
}
#[test]
fn flat_layout_and_metadata_subdirectory_are_exact_not_fallbacks() {
    assert_eq!(ROLE_PATHS,[("launcher","SCBL.LAN.exe"),("main","runtime/uplay_r1_loader.dll"),
        ("gameplay","runtime/scbl_gameplay.dll"),("server","runtime/SCBL.LAN.Server.exe")]);
    let f=Fixture::new();assert!(f.load().is_ok());
    // A root-level catalog neither shadows the current one nor rescues its absence.
    fs::write(f.root.join("RELEASE-CATALOG.json"),b"not the current catalog").unwrap();
    assert!(f.load().is_ok());
    let current=f.root.join("package/RELEASE-CATALOG.json");let bytes=fs::read(&current).unwrap();
    fs::write(f.root.join("RELEASE-CATALOG.json"),&bytes).unwrap();fs::remove_file(current).unwrap();
    assert!(f.load().is_err());
    assert!(verify_package(&f.root.join("package")).is_err());
}
#[test]
fn server_at_old_depth_is_not_found_by_ancestor_search() {
    let f=Fixture::new();assert!(f.load().is_ok());
    let deeper=f.root.join("runtime/0.4.0/SCBL.LAN.Server.exe");
    fs::create_dir_all(deeper.parent().unwrap()).unwrap();
    fs::copy(f.root.join("runtime/SCBL.LAN.Server.exe"),&deeper).unwrap();
    assert!(verify_server_package(&deeper).is_err());
    assert!(f.load().is_ok());
}
