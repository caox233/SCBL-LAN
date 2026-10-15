// Isolated regression process only; not included in the user package.
fn main() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let path = hooks_config::get_config_path(executable.parent().unwrap());
    if !path.exists() {
        println!("CONFIG_REFERENCE_MISSING scbl-lan.toml");
        std::process::exit(20);
    }
    let cfg = hooks_config::get_or_load(path)?;
    if cfg.gameplay_mode != hooks_config::GameplayMode::Official || cfg.user.account_id.is_empty() {
        anyhow::bail!("Invalid synthetic configuration");
    }
    println!("NATIVE_HOOK_CONFIG_OK official=true identity_present=true");
    Ok(())
}
