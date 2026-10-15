// Verification helper only. Not distributed as a game binary.
fn main() -> anyhow::Result<()> {
    let path=std::env::args().nth(1).ok_or_else(||anyhow::anyhow!("config path required"))?;
    let cfg=hooks_config::get_or_load(path)?;
    println!("PIPE_CONFIG_OK official={} bind={} identity_present={}",cfg.gameplay_mode==hooks_config::GameplayMode::Official,
        cfg.networking.ip_address.unwrap(),!cfg.user.password.is_empty() && !cfg.user.account_id.is_empty());
    Ok(())
}
