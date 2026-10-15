use super::*;
include!(concat!(env!("SCBL_R58_C71_FIXTURE"), "/native_fixture.rs"));

struct Fixture {
    core: Vec<u32>,
    pawn: Vec<u32>,
    pc: Vec<u32>,
    camera: Vec<u32>,
    mode: Vec<u32>,
    input: Vec<u32>,
    device: Vec<u32>,
    native: usize,
}
impl Fixture {
    unsafe fn new() -> Self {
        let mut f = Self {
            core: vec![0; 0x100 / 4],
            pawn: vec![0; 0x1800 / 4],
            pc: vec![0; 0x700 / 4],
            camera: vec![0; 0x800 / 4],
            mode: vec![0; 0x420 / 4],
            input: vec![0; 0x80 / 4],
            device: vec![0; 0x80 / 4],
            native: native_fixture(),
        };
        f.core[0x1c / 4] = f.camera.as_ptr() as u32;
        f.core[0x20 / 4] = f.pc.as_ptr() as u32;
        f.core[0x24 / 4] = f.pawn.as_ptr() as u32;
        f.core[0x2c / 4] = f.mode.as_ptr() as u32;
        f.pawn[0x374 / 4] = 0xa0000;
        f.pawn[0x938 / 4] = f.pc.as_ptr() as u32;
        f.pawn[0x93c / 4] = f.input.as_ptr() as u32;
        f.pc[0x60 / 4] = 0x40000;
        f.pc[0x310 / 4] = f.pawn.as_ptr() as u32;
        f.pc[0x520 / 4] = f.pawn.as_ptr() as u32;
        f.pc[0x6a4 / 4] = f.camera.as_ptr() as u32;
        f.pc[0x4d8 / 4] = f.device.as_ptr() as u32;
        f.camera[0x61c / 4] = f.core.as_ptr() as u32;
        f.camera[0x354 / 4] = 0.25f32.to_bits();
        f.camera[0x34c / 4] = 0.9f32.to_bits();
        f.camera[0x350 / 4] = 0.5f32.to_bits();
        f.camera[0x348 / 4] = 0.5f32.to_bits();
        f.camera[0x358 / 4] = 0.6f32.to_bits();
        f.camera[0x35c / 4] = 2f32.to_bits();
        f.camera[0x360 / 4] = 0.2f32.to_bits();
        f.camera[0x364 / 4] = 8f32.to_bits();
        f.mode[0] = MERC_MODE_VT;
        f.mode[1] = f.core.as_ptr() as u32;
        f.input[0x48 / 4] = 0xc0010004;
        f.device[0x28 / 4] = 4;
        f
    }
    unsafe fn original(&self) -> f32 {
        std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize) -> f32>(self.native)(
            self.core.as_ptr() as usize,
        )
    }
    unsafe fn apply(&self, original: f32) -> f32 {
        finish(
            self.core.as_ptr() as usize,
            original,
            std::mem::transmute::<usize, MapFn>(self.native + 0x400),
        )
    }
    fn bytes(&self) -> Vec<Vec<u32>> {
        vec![
            self.core.clone(),
            self.pawn.clone(),
            self.pc.clone(),
            self.camera.clone(),
            self.mode.clone(),
            self.input.clone(),
            self.device.clone(),
        ]
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::System::Memory::VirtualFree(
                self.native as _,
                0,
                windows::Win32::System::Memory::MEM_RELEASE,
            )
            .unwrap();
        }
    }
}

#[test]
fn c71_same_input_fixture() {
    unsafe {
        let mut f = Fixture::new();
        for value in [0.0f32, 0.25, 0.37140095, 0.49519286, 0.5, 0.75, 1.0] {
            f.camera[0x354 / 4] = value.to_bits();
            f.input[0x48 / 4] = 0x10006;
            let walk = f.original();
            let before = f.bytes();
            assert_eq!(f.apply(walk).to_bits(), walk.to_bits());
            assert_eq!(f.bytes(), before);
            f.input[0x48 / 4] = 0xc0010004;
            let sprint = f.original();
            let before = f.bytes();
            let corrected = f.apply(sprint);
            assert!((corrected - walk).abs() < 0.000001);
            assert_eq!(f.bytes(), before);
            println!("C71_SAME_INPUT slider={value:.8} native_walk={walk:.8} native_sprint={sprint:.8} candidate_sprint={corrected:.8} pawn_and_settings_unchanged=true real_game=false");
        }
    }
}
#[test]
fn c71_preserves_original_base_multiplier() {
    unsafe {
        let f = Fixture::new();
        assert!((f.apply(0.75) - 0.45).abs() < 1e-6);
    }
}
#[test]
fn c71_neutral_is_bit_exact() {
    unsafe {
        let mut f = Fixture::new();
        f.camera[0x354 / 4] = 0.5f32.to_bits();
        for n in [0.1, 0.75, 1.0, 2.0, 9.0] {
            assert_eq!(f.apply(n).to_bits(), n.to_bits());
        }
    }
}
#[test]
fn c71_gamepad_unchanged() {
    unsafe {
        let mut f = Fixture::new();
        f.device[0x28 / 4] = 0;
        assert_eq!(f.apply(1.25), 1.25);
    }
}
#[test]
fn c71_spy_campaign_and_non_first_person_unchanged() {
    unsafe {
        let mut f = Fixture::new();
        for flags in [0, 0x40000, 0x60000, 0x80000, 0xe0000] {
            f.pawn[0x374 / 4] = flags;
            assert_eq!(f.apply(1.25), 1.25);
        }
    }
}
#[test]
fn c71_no_bypass_and_positive_state_unchanged() {
    unsafe {
        let mut f = Fixture::new();
        f.input[0x48 / 4] = 0;
        assert_eq!(f.apply(1.25), 1.25);
        f.input[0x48 / 4] = 0x80000000;
        f.pawn[0x17a0 / 4] = 0.1f32.to_bits();
        assert_eq!(f.apply(1.25), 1.25);
    }
}
#[test]
fn c71_nan_or_inf_inputs_unchanged() {
    unsafe {
        let mut f = Fixture::new();
        for offset in [0x354, 0x360, 0x364] {
            let old = f.camera[offset / 4];
            for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                f.camera[offset / 4] = v.to_bits();
                assert_eq!(f.apply(1.25), 1.25);
            }
            f.camera[offset / 4] = old;
        }
        f.pawn[0x17a0 / 4] = f32::NAN.to_bits();
        assert_eq!(f.apply(1.25), 1.25);
    }
}
#[test]
fn c71_invalid_original_bit_exact() {
    unsafe {
        let f = Fixture::new();
        for v in [0.0f32, -0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(f.apply(v).to_bits(), v.to_bits());
        }
    }
}
#[test]
fn c71_owner_mismatch_and_remote_unchanged() {
    unsafe {
        let mut f = Fixture::new();
        for offset in [0x310, 0x520, 0x6a4, 0x60] {
            let old = f.pc[offset / 4];
            f.pc[offset / 4] = 0;
            assert_eq!(f.apply(1.25), 1.25);
            f.pc[offset / 4] = old;
        }
        f.pc[0x4c / 4] = 0x200;
        assert_eq!(f.apply(1.25), 1.25);
    }
}
#[test]
fn c71_unrelated_camera_mode_unchanged() {
    unsafe {
        let mut f = Fixture::new();
        f.mode[0] = 0x2ae6bcc;
        assert_eq!(f.apply(1.25), 1.25);
        f.mode[0] = MERC_MODE_VT;
        f.mode[1] = 0;
        assert_eq!(f.apply(1.25), 1.25);
    }
}
#[test]
fn c71_invalid_pointers_fail_closed() {
    unsafe {
        let f = Fixture::new();
        for p in [0usize, 1, 0xffff, usize::MAX] {
            assert_eq!(
                finish(
                    p,
                    1.25,
                    std::mem::transmute::<usize, MapFn>(f.native + 0x400)
                ),
                1.25
            );
        }
    }
}
#[test]
fn c71_invalid_ranges_fail_closed() {
    unsafe {
        let mut f = Fixture::new();
        for (off, v) in [
            (0x354, -0.1f32),
            (0x354, 1.1),
            (0x360, 0.0),
            (0x360, 1.1),
            (0x364, 0.9),
        ] {
            let old = f.camera[off / 4];
            f.camera[off / 4] = v.to_bits();
            assert_eq!(f.apply(1.25), 1.25);
            f.camera[off / 4] = old;
        }
    }
}
#[test]
fn c71_no_cumulative_or_third_person_gain() {
    unsafe {
        let mut f = Fixture::new();
        for v in [0.0f32, 0.5, 1.0] {
            f.camera[0x34c / 4] = v.to_bits();
            for _ in 0..100 {
                let orig = f.original();
                assert!((f.apply(orig) - 0.6).abs() < 1e-6);
            }
        }
    }
}
