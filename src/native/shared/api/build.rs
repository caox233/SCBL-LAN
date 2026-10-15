use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let identity_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../../../../release-identity.json");
    println!("cargo:rerun-if-changed={}", identity_path.display());
    let identity: serde_json::Value = serde_json::from_slice(&std::fs::read(identity_path)?)?;
    let release = identity["release_id"].as_str().ok_or("release_id missing")?;
    let compat = identity["compat_id"].as_str().ok_or("compat_id missing")?;
    for value in [release, compat] {
        if value.is_empty() || value.len() > 64 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) {
            return Err("invalid prebuild release identity".into());
        }
    }
    let mut generated = format!("pub const RELEASE_ID: &str = {release:?};\npub const COMPAT_ID: &str = {compat:?};\n");
    for role in ["launcher", "main", "gameplay", "server"] {
        let marker = format!("SCBL_RELEASE_V1|{role}|{release}|{compat}|END_SCBl");
        generated.push_str(&format!("pub const {}_MARKER: &str = {marker:?};\n", role.to_uppercase()));
    }
    std::fs::write(out_dir.join("release_identity.rs"), generated)?;

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("friends_descriptor.bin"))
        .compile_protos(&["proto/friends.proto"], &["proto/"])?;

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("users_descriptor.bin"))
        .compile_protos(&["proto/users.proto"], &["proto/"])?;

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("misc_descriptor.bin"))
        .compile_protos(&["proto/misc.proto"], &["proto/"])?;

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("games_descriptor.bin"))
        .compile_protos(&["proto/games.proto"], &["proto/"])?;
    Ok(())
}
