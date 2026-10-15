//! Local matched-package integrity, not publisher authentication or remote attestation.
//! The trusted offline deployment boundary supplies the package; policy is immutable per process.
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use crate::release_contract::ReleaseIdentity;
include!(concat!(env!("OUT_DIR"), "/release_identity.rs"));

pub const ROLE_PATHS: [(&str, &str); 4] = [
    ("launcher", "SCBL.LAN.exe"),
    ("main", "runtime/uplay_r1_loader.dll"),
    ("gameplay", "runtime/scbl_gameplay.dll"),
    ("server", "runtime/SCBL.LAN.Server.exe"),
];
pub fn role_marker(role: &str) -> Option<String> {
    marker_for(role, RELEASE_ID, COMPAT_ID)
}
fn marker_for(role: &str, release: &str, compat: &str) -> Option<String> {
    if !ROLE_PATHS.iter().any(|(known, _)| *known == role) { return None; }
    // Do not reference all four generated literals here: each binary must retain
    // only its own marker, not inherit false role provenance from this verifier.
    Some(format!("SCBL_RELEASE_V1|{role}|{release}|{compat}|END_SCBl"))
}
fn complete_marker_role(bytes: &[u8], wide: bool) -> Option<String> {
    let mut text=String::new();
    let stride=if wide {2} else {1};
    for index in (0..bytes.len().min(400)).step_by(stride) {
        let byte=bytes[index];
        if !byte.is_ascii() || byte==0 || (wide && bytes.get(index+1)!=Some(&0)) {return None;}
        text.push(char::from(byte));
        if text.ends_with("|END_SCBl") {break;}
    }
    let parts:Vec<_>=text.split('|').collect();
    if parts.len()!=5 || parts[0]!="SCBL_RELEASE_V1" || parts[4]!="END_SCBl"
        || !ROLE_PATHS.iter().any(|(known,_)| *known==parts[1]) {return None;}
    if parts[2..4].iter().any(|s|s.is_empty() || s.len()>64 || !s.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'.'|b'_'|b'-'))) {return None;}
    Some(parts[1].to_owned())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog { schema: u32, release_id: String, compat_id: String, components: Vec<Component> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Component { role: String, path: String, release_id: String, compat_id: String, bytes: u64, sha256: String }
fn invalid(message: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, format!("PACKAGE_CATALOG_INVALID: {}",message.into())) }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn regular_path(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        let metadata=fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() { return Err(invalid("links are not package files")); }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 { return Err(invalid("reparse package path")); }
        }
    }
    if !fs::metadata(path)?.is_file() { return Err(invalid("package component is not a regular file")); }
    Ok(())
}
fn verify_file(path: &Path, row: &Component, release: &str, compat: &str) -> io::Result<()> {
    regular_path(path)?;
    if row.bytes == 0 || fs::metadata(path)?.len() != row.bytes { return Err(invalid(format!("size mismatch: {}",row.role))); }
    let expected=marker_for(&row.role,release,compat).ok_or_else(||invalid("unknown role"))?;
    let prefix=format!("SCBL_RELEASE_V1|{}|",row.role);
    let patterns=[(prefix.as_bytes().to_vec(),expected.as_bytes().to_vec()),
        (prefix.encode_utf16().flat_map(u16::to_le_bytes).collect(),expected.encode_utf16().flat_map(u16::to_le_bytes).collect())];
    let mut file=File::open(path)?;let mut digest=Sha256::new();let mut size=0u64;
    let mut buffer=[0u8;65536];let mut tail=Vec::new();let mut markers=0usize;
    loop {
        let count=file.read(&mut buffer)?;
        digest.update(&buffer[..count]);size+=count as u64;
        tail.extend_from_slice(&buffer[..count]);
        let scan=if count==0 {tail.len()} else {tail.len().saturating_sub(512)};
        for (prefix,expected) in &patterns { for offset in 0..scan {
            if tail[offset..].starts_with(prefix) {
                if !tail[offset..].starts_with(expected) {return Err(invalid(format!("embedded identity mismatch: {}",row.role)));}
                markers+=1;
            }
        }}
        for (prefix,wide) in [(b"SCBL_RELEASE_V1|".to_vec(),false),
            ("SCBL_RELEASE_V1|".encode_utf16().flat_map(u16::to_le_bytes).collect(),true)] {
            for offset in 0..scan {
                if tail[offset..].starts_with(&prefix) {
                    if let Some(role)=complete_marker_role(&tail[offset..],wide) {
                        if role!=row.role {return Err(invalid(format!("foreign role identity marker: {}/{role}",row.role)));}
                    }
                }
            }
        }
        if count==0 {break;} tail.drain(..scan);
    }
    if markers!=1 {return Err(invalid(format!("missing or ambiguous embedded identity: {}",row.role)));}
    if size!=row.bytes || hex(&digest.finalize())!=row.sha256 {return Err(invalid(format!("hash mismatch: {}",row.role)));}
    Ok(())
}
/// Read every role as data only. Official never loads the gameplay DLL to validate its presence.
/// No environment override, hot reload, fallback policy or public digest trust anchor exists.
pub fn verify_package(root: &Path) -> io::Result<ReleaseIdentity> {
    verify_package_expected(root,RELEASE_ID,COMPAT_ID)
}
// Golden fixtures use this exact verifier with an explicit fixture identity;
// layout is never parameterized. Production always enters through verify_package.
fn verify_package_expected(root: &Path, release: &str, compat: &str) -> io::Result<ReleaseIdentity> {
    let catalog_path=root.join("package/RELEASE-CATALOG.json");regular_path(&catalog_path)?;
    let mut raw=Vec::new();File::open(catalog_path)?.take(16*1024+1).read_to_end(&mut raw)?;
    if raw.len()>16*1024 {return Err(invalid("catalog exceeds 16 KiB"));}
    let catalog:Catalog=serde_json::from_slice(&raw).map_err(|e|invalid(format!("malformed catalog: {e}")))?;
    if catalog.schema!=1 || catalog.release_id!=release || catalog.compat_id!=compat || catalog.components.len()!=4 {return Err(invalid("schema, embedded release or role count mismatch"));}
    for (row,(role,path)) in catalog.components.iter().zip(ROLE_PATHS) {
        if row.role!=role || row.path!=path || row.release_id!=catalog.release_id || row.compat_id!=catalog.compat_id {
            return Err(invalid("mixed role, path or release identity"));
        }
        if row.sha256.len()!=64 || !row.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {return Err(invalid("malformed component digest"));}
        verify_file(&root.join(path),row,release,compat)?;
    }
    ReleaseIdentity::new(1,&catalog.release_id,&catalog.compat_id,&hex(&Sha256::digest(&raw))).map_err(|e|invalid(e.to_string()))
}
/// Standalone servers must remain in the complete matched portable package.
/// A copied server alone or a catalog found by searching arbitrary ancestors is not accepted.
pub fn verify_server_package(executable: &Path) -> io::Result<ReleaseIdentity> {
    let root=executable.ancestors().nth(2).ok_or_else(||invalid("server requires complete portable package layout"))?;
    let declared=root.join(ROLE_PATHS[3].1);
    regular_path(executable)?;
    if fs::canonicalize(executable)?!=fs::canonicalize(declared)? {return Err(invalid("server executable is not catalog server role"));}
    // Keep this role's exact prebuild marker reachable in the real server binary.
    std::hint::black_box(SERVER_MARKER);
    verify_package(root)
}
#[cfg(test)]
#[path="release_catalog_tests.rs"]
mod tests;
