use super::*;
struct Fixture {
    view: Vec<u32>,
    state: Vec<u32>,
    output: Vec<u32>,
    sources: Vec<u32>,
    profile: Vec<u32>,
}
impl Fixture {
    fn new(mode: u32, cm: f32) -> Self {
        let mut f = Self {
            view: vec![0; 0x53c / 4],
            state: vec![0; 0x3c / 4],
            output: vec![0x3f800000; 0x268 / 4],
            sources: vec![0; 47 * 0x30 / 4],
            profile: vec![0x3f000000; 0x268 / 4],
        };
        f.view[0x538 / 4] = f.state.as_ptr() as u32;
        f.state[0] = f.sources.as_ptr() as u32;
        f.state[1] = f.output.as_ptr() as u32;
        f.state[0x1c / 4] = mode;
        f.sources[0] = f.profile.as_ptr() as u32;
        f.output[VIEW_DISTANCE / 4] = cm.to_bits();
        f
    }
    unsafe fn apply(&mut self) -> Option<(u32, f32, f32)> {
        super::apply(self.view.as_ptr() as usize)
    }
}
#[test]
fn thermal_30m_emf_and_sonar_remain_40m() {
    assert_eq!(distance_cm(8), Some(3000.0));
    assert_eq!(distance_cm(9), Some(4000.0));
    assert_eq!(distance_cm(7), Some(4000.0));
}
#[test]
fn c70_only_requested_blended_fields_change() {
    for mode in [7, 8, 9] {
        let mut f = Fixture::new(mode, 6400.0);
        let before = f.output.clone();
        let sources = f.sources.clone();
        let profile = f.profile.clone();
        unsafe {
            assert_eq!(f.apply(), Some((mode, 6400.0, distance_cm(mode).unwrap())));
        }
        for (i, &v) in before.iter().enumerate() {
            assert_eq!(
                f.output[i],
                if i == VIEW_DISTANCE / 4 {
                    distance_cm(mode).unwrap().to_bits()
                } else if mode == 7 && i == SONAR_WAVE_SPEED / 4 {
                    SONAR_SPEED_CM_S.to_bits()
                } else if mode == 7 && i == SONAR_PERSISTENCE / 4 {
                    SONAR_PERSISTENCE_S.to_bits()
                } else {
                    v
                }
            );
        }
        assert_eq!(f.sources, sources);
        assert_eq!(f.profile, profile);
    }
}
#[test]
fn c67_native_normal_merc_and_campaign_modes_unchanged() {
    for mode in (0..32).filter(|v| ![7, 8, 9].contains(v)) {
        let mut f = Fixture::new(mode, 6400.0);
        let before = f.output.clone();
        unsafe {
            assert_eq!(f.apply(), None);
        }
        assert_eq!(f.output, before);
    }
}
#[test]
fn c67_native_reblend_and_mode_change() {
    let mut f = Fixture::new(8, 6400.0);
    unsafe {
        f.apply();
    }
    assert_eq!(f.output[VIEW_DISTANCE / 4], 3000f32.to_bits());
    f.output.copy_from_slice(&f.profile);
    f.state[0x1c / 4] = 9;
    unsafe {
        f.apply();
    }
    assert_eq!(f.output[VIEW_DISTANCE / 4], 4000f32.to_bits());
    f.output.copy_from_slice(&f.profile);
    f.state[0x1c / 4] = 0;
    unsafe {
        assert_eq!(f.apply(), None);
    }
    assert_eq!(f.output, f.profile);
}
#[test]
fn c67_source_profile_alias_is_rejected() {
    for slot in [0, 14, 15, 16, 46] {
        let mut f = Fixture::new(8, 6400.0);
        f.sources[slot * 0x30 / 4] = f.output.as_ptr() as u32;
        let before = f.output.clone();
        unsafe {
            assert_eq!(f.apply(), None);
        }
        assert_eq!(f.output, before);
    }
}
#[test]
fn c67_bad_inputs_fail_open_to_native() {
    unsafe {
        assert_eq!(apply(0), None);
        assert_eq!(apply(usize::MAX - 3), None);
    }
    for cm in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let mut f = Fixture::new(7, cm);
        let before = f.output.clone();
        unsafe {
            assert_eq!(f.apply(), None);
        }
        assert_eq!(f.output, before);
    }
    for offset in [0, 1] {
        let mut f = Fixture::new(8, 6400.0);
        f.state[offset] = 0;
        unsafe {
            assert_eq!(f.apply(), None);
        }
    }
}
#[test]
fn c67_sets_requested_range_even_if_native_shorter() {
    for mode in [7, 8, 9] {
        let mut f = Fixture::new(mode, 1000.0);
        unsafe {
            assert!(f.apply().is_some());
        }
        assert_eq!(
            f.output[VIEW_DISTANCE / 4],
            distance_cm(mode).unwrap().to_bits()
        );
    }
}
#[test]
fn c67_idempotent_without_touching_adjacent_fields() {
    let mut f = Fixture::new(7, 6400.0);
    unsafe {
        f.apply();
    }
    let before = f.output.clone();
    unsafe {
        assert_eq!(f.apply(), Some((7, 4000.0, 4000.0)));
    }
    assert_eq!(f.output, before);
}
#[test]
fn c67_native_contract_fixtures() {
    use sha2::{Digest, Sha256};
    let root = std::path::PathBuf::from(
        std::env::var("SCBL_R58_C67_FIXTURE").expect("C67 native fixtures"),
    );
    for &(a, n, h) in CONTRACTS {
        let bytes = std::fs::read(root.join(format!("native-{a:08x}.bin"))).unwrap();
        assert_eq!(bytes.len(), n);
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), h);
    }
}

#[test]
fn c68_native_speed_field_and_update_contracts() {
    use sha2::{Digest, Sha256};
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C68_FIXTURE").unwrap());
    for &(a, n, h) in SONAR_CONTRACTS {
        let bytes = std::fs::read(root.join(format!("native-{a:08x}.bin"))).unwrap();
        assert_eq!(bytes.len(), n);
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), h);
    }
}
#[test]
fn c68_invalid_speed_rejects_both_writes_without_touching_native() {
    for speed in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let mut f = Fixture::new(7, 6400.0);
        f.output[SONAR_WAVE_SPEED / 4] = speed.to_bits();
        let before = f.output.clone();
        unsafe {
            assert_eq!(f.apply(), None);
        }
        assert_eq!(f.output, before);
    }
}
#[test]
fn c68_sonar_never_writes_source_aliases() {
    for slot in [0, 14, 15, 16, 46] {
        let mut f = Fixture::new(7, 6400.0);
        f.sources[slot * 0x30 / 4] = f.output.as_ptr() as u32;
        let before = f.output.clone();
        unsafe {
            assert_eq!(f.apply(), None);
        }
        assert_eq!(f.output, before);
    }
}
#[test]
fn c70_native_reblend_clears_speed_and_persistence_when_leaving_sonar() {
    let mut f = Fixture::new(7, 6400.0);
    f.profile[SONAR_WAVE_SPEED / 4] = 1950f32.to_bits();
    f.profile[SONAR_PERSISTENCE / 4] = 4f32.to_bits();
    unsafe {
        f.apply();
    }
    assert_eq!(f.output[SONAR_WAVE_SPEED / 4], 2000f32.to_bits());
    for mode in [8, 9, 0, 1, 12] {
        f.output.copy_from_slice(&f.profile);
        f.state[0x1c / 4] = mode;
        unsafe {
            f.apply();
        }
        assert_eq!(f.output[SONAR_WAVE_SPEED / 4], 1950f32.to_bits());
        assert_eq!(f.output[SONAR_PERSISTENCE / 4], 4f32.to_bits());
    }
}
#[test]
fn c70_repeated_native_blends_only_change_requested_persistence() {
    let mut f = Fixture::new(7, 5000.0);
    for _ in 0..1000 {
        f.output.copy_from_slice(&f.profile);
        f.output[VIEW_DISTANCE / 4] = 4000f32.to_bits();
        f.output[SONAR_WAVE_SPEED / 4] = 2000f32.to_bits();
        f.output[0x1b0 / 4] = 4f32.to_bits();
        f.output[0x1d8 / 4] = 5f32.to_bits();
        let before = f.output.clone();
        unsafe {
            f.apply();
        }
        for (i, &v) in before.iter().enumerate() {
            if i != VIEW_DISTANCE / 4 && i != SONAR_WAVE_SPEED / 4 && i != SONAR_PERSISTENCE / 4 {
                assert_eq!(f.output[i], v);
            }
        }
        assert_eq!(f.output[VIEW_DISTANCE / 4], 4000f32.to_bits());
        assert_eq!(f.output[SONAR_WAVE_SPEED / 4], 2000f32.to_bits());
    }
}

unsafe fn native_wave_copy() -> (usize, usize) {
    use sha2::{Digest, Sha256};
    use windows::Win32::System::Memory::{
        VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C68_FIXTURE").unwrap());
    let code = std::fs::read(root.join("native-00a2e7f0.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&code)),
        "7ee88a2a43d7acdb67e3692c4d5f60bce0fa30ec01d818ccc9e74f207fd36d5c"
    );
    let p = VirtualAlloc(
        None,
        0x2000,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_EXECUTE_READWRITE,
    ) as usize;
    assert_ne!(p, 0);
    std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len());
    let data: [u8; 16] = [
        0, 0, 32, 65, 102, 111, 114, 99, 101, 76, 111, 119, 86, 105, 100, 101,
    ];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4096) as *mut u8, 16);
    let data: [u8; 16] = [
        0, 64, 28, 70, 80, 251, 173, 157, 40, 73, 35, 64, 65, 99, 116, 105,
    ];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4112) as *mut u8, 16);
    let data: [u8; 16] = [0, 0, 0, 0, 0, 0, 89, 64, 0, 0, 150, 66, 0, 0, 150, 67];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4128) as *mut u8, 16);
    let data: [u8; 16] = [0, 80, 195, 71, 0, 0, 0, 0, 0, 0, 0, 128, 235, 81, 240, 63];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4144) as *mut u8, 16);
    let data: [u8; 16] = [0, 0, 200, 66, 0, 0, 0, 0, 0, 0, 0, 224, 81, 184, 158, 63];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4160) as *mut u8, 16);
    let data: [u8; 16] = [
        205, 204, 204, 61, 0, 0, 0, 0, 0, 0, 0, 160, 153, 153, 185, 63,
    ];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4176) as *mut u8, 16);
    let data: [u8; 16] = [
        0, 0, 0, 160, 153, 153, 185, 63, 137, 136, 8, 61, 114, 97, 110, 100,
    ];
    std::ptr::copy_nonoverlapping(data.as_ptr(), (p + 4192) as *mut u8, 16);
    ((p + 117) as *mut u32).write_unaligned((p + 4096) as u32);
    ((p + 125) as *mut u32).write_unaligned((p + 4112) as u32);
    ((p + 197) as *mut u32).write_unaligned((p + 4128) as u32);
    ((p + 205) as *mut u32).write_unaligned((p + 4144) as u32);
    ((p + 213) as *mut u32).write_unaligned((p + 4160) as u32);
    ((p + 337) as *mut u32).write_unaligned((p + 4176) as u32);
    ((p + 358) as *mut u32).write_unaligned((p + 4192) as u32);
    (p, code.len())
}
unsafe fn measure_native_cycle(view: usize, state: &mut [u32]) -> f32 {
    use windows::Win32::System::Memory::{VirtualFree, MEM_RELEASE};
    let (p, _) = native_wave_copy();
    let update: unsafe extern "cdecl" fn(f32, usize) = std::mem::transmute(p);
    state[0x20 / 4] = 1;
    state[0x34 / 4] = 0;
    state[0x38 / 4] = 0;
    let mut seconds = 0.0;
    for step in 1..=1000 {
        update(0.01, view);
        if state[0x34 / 4] & 255 == 0 {
            seconds = step as f32 * 0.01;
            break;
        }
    }
    assert!(seconds > 0.0);
    VirtualFree(p as _, 0, MEM_RELEASE).unwrap();
    seconds
}

#[test]
fn c70_executed_native_wave_cycle_uses_native_caps() {
    let mut f = Fixture::new(7, 5000.0);
    f.output[SONAR_WAVE_SPEED / 4] = 2000f32.to_bits();
    f.output[0x188 / 4] = 20000f32.to_bits();
    f.output[0x1b0 / 4] = 4f32.to_bits();
    f.output[0x1d8 / 4] = 5f32.to_bits();
    let baseline = unsafe { measure_native_cycle(f.view.as_ptr() as usize, &mut f.state) };
    unsafe {
        f.apply();
    }
    let modified = unsafe { measure_native_cycle(f.view.as_ptr() as usize, &mut f.state) };
    assert!((baseline - 7.5).abs() < 0.02);
    assert!((modified - 7.0).abs() < 0.02);
    assert_eq!(f.output[0x1b0 / 4], 5f32.to_bits());
    assert_eq!(f.output[0x1d8 / 4], 5f32.to_bits());
    println!("C70_NATIVE_WAVE baseline_speed=2000 modified_speed=2000 baseline_cycle_s={baseline:.2} modified_cycle_s={modified:.2} persistence_s=5 glow_s=5 native_cap=10000 real_game=false");
}

#[test]
fn c70_invalid_persistence_rejects_all_writes() {
    for value in [-1.0, f32::NAN, f32::INFINITY] {
        let mut f = Fixture::new(7, 6400.0);
        f.output[SONAR_PERSISTENCE / 4] = value.to_bits();
        let before = f.output.clone();
        unsafe {
            assert_eq!(f.apply(), None);
        }
        assert_eq!(f.output, before);
    }
}
#[test]
fn c70_exact_requested_values_and_zero_native_persistence() {
    let mut f = Fixture::new(7, 4000.0);
    f.output[SONAR_PERSISTENCE / 4] = 0f32.to_bits();
    f.output[0x1d8 / 4] = 5f32.to_bits();
    let original = f.profile.clone();
    unsafe {
        assert!(f.apply().is_some());
    }
    assert_eq!(f.output[VIEW_DISTANCE / 4], 4000f32.to_bits());
    assert_eq!(f.output[SONAR_WAVE_SPEED / 4], 2000f32.to_bits());
    assert_eq!(f.output[SONAR_PERSISTENCE / 4], 5f32.to_bits());
    assert_eq!(f.output[0x1d8 / 4], 5f32.to_bits());
    assert_eq!(f.profile, original);
}
#[test]
fn c70_pulse_and_cap_hook_integrations_removed() {
    let range = include_str!("spy_goggle_range.rs");
    let vision = include_str!("vision_transparency.rs");
    assert!(!range.contains("mod sonar_pulse"));
    assert!(!range.contains("sonar_pulse::"));
    assert!(!range.contains("init_sonar_limits"));
    assert!(!range.contains("RawDetour"));
    assert!(!vision.contains("sonar_pulse::"));
}
