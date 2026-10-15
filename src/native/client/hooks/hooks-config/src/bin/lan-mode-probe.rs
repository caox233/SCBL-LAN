// Fresh subprocess regression helper; never shipped as a game executable.
fn main() -> anyhow::Result<()> {
    let exe=std::env::current_exe()?;
    let cfg=hooks_config::get_or_load(hooks_config::get_config_path(exe.parent().unwrap()))?;
    let plugin=std::env::var_os("SCBL_LAN_PLUGIN_DIR");
    if cfg.gameplay_mode==hooks_config::GameplayMode::Mod {
        let root=std::path::PathBuf::from(plugin.ok_or_else(||anyhow::anyhow!("Missing MOD plugin directory"))?);
        if !root.join("scbl_gameplay.dll").is_file() {anyhow::bail!("Missing gameplay plugin");}
        println!("NATIVE_MODE MOD plugin_directory=true identity_present={}",!cfg.user.account_id.is_empty());
    } else {
        if plugin.is_some() {anyhow::bail!("Official inherited MOD directory");}
        println!("NATIVE_MODE Official plugin_directory=false identity_present={}",!cfg.user.account_id.is_empty());
    }
    Ok(())
}
