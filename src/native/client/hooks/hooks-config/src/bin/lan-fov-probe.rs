fn main() -> anyhow::Result<()> {
    let config=hooks_config::get_or_load(std::env::current_dir()?.join("scbl-lan.toml"))?;
    let expected: f32=std::env::var("SCBL_LAN_TEST_FOV")?.parse()?;
    anyhow::ensure!((config.features.fov_factor-expected).abs()<0.00001,"FOV mismatch");
    println!("FOV_NATIVE {:.2} PASS",config.features.fov_factor);
    Ok(())
}
