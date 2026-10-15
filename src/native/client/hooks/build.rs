fn quote(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    format!("\"{}\"", s)
}

fn locate_cmake() -> std::path::PathBuf {
    use std::path::PathBuf;
    use std::process::Command;

    if Command::new("cmake").arg("--version").output().is_ok() {
        return PathBuf::from("cmake");
    }

    let program_files_x86 = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .expect("ProgramFiles(x86) is unavailable while locating Visual Studio CMake");
    let vswhere = program_files_x86.join("Microsoft Visual Studio").join("Installer").join("vswhere.exe");
    let output = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.CMake.Project",
            "-find",
            r"Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe",
        ])
        .output()
        .expect("failed to run Visual Studio Installer vswhere for CMake");
    assert!(output.status.success(), "vswhere failed while locating Visual Studio CMake");
    let path = String::from_utf8(output.stdout)
        .expect("vswhere returned a non-UTF-8 CMake path")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
        .expect("Visual Studio CMake component is unavailable");
    assert!(path.is_file(), "vswhere returned a missing CMake executable: {}", path.display());
    path
}

fn build_builtin_audio() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hash;
    use std::hash::Hasher;
    use std::path::PathBuf;
    use std::process::Command;

    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.ends_with("windows-msvc") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source_dir = manifest_dir.join("audio");
    let mut build_identity = DefaultHasher::new();
    manifest_dir.hash(&mut build_identity);
    std::env::var_os("PROFILE").hash(&mut build_identity);
    let build_dir = std::env::temp_dir().join("scbl-lan-audio-build").join(format!("{:016x}", build_identity.finish()));
    let cmake = locate_cmake();

    let configure = Command::new(&cmake)
        .arg("-S")
        .arg(&source_dir)
        .arg("-B")
        .arg(&build_dir)
        .args(["-A", "Win32"])
        .status()
        .expect("failed to start CMake for the built-in SCBL audio layer");
    assert!(configure.success(), "CMake configure failed for the built-in SCBL audio layer");

    let build = Command::new(&cmake)
        .arg("--build")
        .arg(&build_dir)
        .args(["--config", "Release", "--target", "scbl_audio_builtin"])
        .status()
        .expect("failed to build the built-in SCBL audio layer");
    assert!(build.success(), "CMake build failed for the built-in SCBL audio layer");

    println!("cargo:rustc-link-search=native={}", build_dir.join("Release").display());
    println!("cargo:rustc-link-lib=static=scbl_audio_builtin");
    println!("cargo:rustc-link-lib=ole32");
    println!("cargo:rustc-link-lib=uuid");
    println!("cargo:rerun-if-changed={}", source_dir.display());
}

pub fn main() {
    build_builtin_audio();
    winres::WindowsResource::new().compile().unwrap();

    let preload = std::fs::read_to_string("preload.dat").unwrap();
    let preload = preload.lines().collect::<Vec<_>>();
    let preload = format!("pub const PRELOAD: [&str; {}] = [\n    {}\n];\n", preload.len(), quote(&preload.join("\",\n    \"")),);
    std::fs::write(format!("{}/preload.rs", std::env::var("OUT_DIR").unwrap()), preload).unwrap();
}
