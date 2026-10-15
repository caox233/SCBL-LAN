pub fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut resource = winres::WindowsResource::new();
        if let Ok(version) = std::env::var("SCBL_DEDICATED_VERSION") {
            let parts: Vec<u64> = version
                .split('.')
                .map(|part| part.parse::<u64>().expect("SCBL_DEDICATED_VERSION must be numeric"))
                .collect();
            assert!(
                parts.len() == 3 && parts.iter().all(|part| *part <= u16::MAX as u64),
                "SCBL_DEDICATED_VERSION must be X.Y.Z"
            );
            let numeric = (parts[0] << 48) | (parts[1] << 32) | (parts[2] << 16);
            resource
                .set("FileVersion", &format!("{version}.0"))
                .set("ProductVersion", &version)
                .set_version_info(winres::VersionInfo::FILEVERSION, numeric)
                .set_version_info(winres::VersionInfo::PRODUCTVERSION, numeric);
        }
        resource.compile().unwrap();
    }
}
