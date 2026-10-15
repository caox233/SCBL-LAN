use super::*;
unsafe extern "thiscall" fn no_grow(_: usize) {}
struct Fixture {
    render: Vec<u32>,
    view: Vec<u32>,
    pass: Vec<u32>,
    selected: Vec<u32>,
    mode_table: Vec<u32>,
    config: Vec<u32>,
    scene: Vec<u32>,
    records: Vec<u32>,
    objects: Vec<Vec<u32>>,
    storage: Vec<Vec<u32>>,
}
const OFF: [usize; 8] = [0x18, 0x9c, 0xa8, 0x24, 0xcc, 0x6c, 0xd8, 0x60];
impl Fixture {
    fn new(mode: u32) -> Self {
        let mut f = Self {
            render: vec![0; 0x2400 / 4],
            view: vec![0; 0x400 / 4],
            pass: vec![0; 0x400 / 4],
            selected: vec![0; 2],
            mode_table: vec![0; 10],
            config: vec![0; 0x400 / 4],
            scene: vec![0; 0x400 / 4],
            records: vec![0; 32],
            objects: (0..4).map(|_| vec![0; 0x220 / 4]).collect(),
            storage: (0..8).map(|_| vec![0; 32]).collect(),
        };
        f.render[0x20d0 / 4] = f.view.as_ptr() as u32;
        f.render[0x236c / 4] = f.pass.as_ptr() as u32;
        f.render[0x14 / 4] = f.config.as_ptr() as u32;
        f.render[0x1c / 4] = f.scene.as_ptr() as u32;
        f.render[0x2208 / 4] = u32::MAX;
        f.pass[0x14 / 4] = f.render.as_ptr() as u32;
        f.pass[0x10 / 4] = f.selected.as_ptr() as u32;
        f.mode_table[mode as usize] = f.selected.as_ptr() as u32;
        f.pass[8 / 4] = f.mode_table.as_ptr() as u32;
        f.selected[0] = if mode == 9 { 0x2c0965c } else { 0x2c094a4 };
        f.view[0x340 / 4] = mode;
        for (i, o) in OFF.iter().enumerate() {
            f.pass[*o / 4] = f.storage[i].as_ptr() as u32;
            f.pass[(*o + 8) / 4] = 32;
        }
        for i in 0..4 {
            f.records[i * 2] = f.objects[i].as_ptr() as u32;
            f.records[i * 2 + 1] = 1;
        }
        f.scene[0xe8 / 4] = f.records.as_ptr() as u32;
        f.scene[0xec / 4] = 4;
        f.objects[0][0x38 / 4] = 0x20000000;
        f.objects[0][0x210 / 4] = 120 << 24;
        f.objects[1][0x38 / 4] = 0x10000000;
        f.objects[1][0x210 / 4] = 120 << 24;
        f.objects[2][0x38 / 4] = 0x20000000;
        f.objects[2][0x34 / 4] = 0x200;
        f.objects[2][0x210 / 4] = 120 << 24;
        f.objects[3][0x34 / 4] = 0x400;
        f.objects[3][0x210 / 4] = 120 << 24;
        let color = if mode == 9 { 0xcc } else { 0x6c };
        f.list(color, &[0, 1, 2, 3]);
        f.list(0xd8, &[0, 1, 2, 3]);
        f.list(0x60, &[0, 1, 2, 3]);
        f
    }
    fn ptr(&self) -> usize {
        self.pass.as_ptr() as usize
    }
    fn list(&mut self, off: usize, indices: &[usize]) {
        let slot = OFF.iter().position(|o| *o == off).unwrap();
        for (j, i) in indices.iter().enumerate() {
            self.storage[slot][j] = self.objects[*i].as_ptr() as u32;
        }
        self.pass[(off + 4) / 4] = indices.len() as u32;
    }
    fn members(&self, off: usize) -> Vec<usize> {
        unsafe {
            let a = Array::load(self.ptr() + off).unwrap();
            (0..a.len)
                .map(|i| {
                    self.objects
                        .iter()
                        .position(|o| o.as_ptr() as usize == a.get(i))
                        .unwrap()
                })
                .collect()
        }
    }
    fn apply(&self) -> Changes {
        unsafe { reroute(self.ptr(), no_grow) }
    }
}

#[test]
fn emf_retains_native_colour_and_removes_only_scene_occlusion() {
    let f = Fixture::new(9);
    let before = f.objects.clone();
    assert_eq!(
        f.apply(),
        Changes {
            primary: 0,
            highlight: 0,
            mask: 2
        }
    );
    assert_eq!(f.members(0xcc), vec![0, 1, 2, 3]);
    assert_eq!(f.members(0xd8), vec![1, 3]);
    assert_eq!(f.objects, before);
}
#[test]
fn thermal_retains_native_colour_and_character_masks() {
    let f = Fixture::new(8);
    assert_eq!(f.apply().mask, 2);
    assert_eq!(f.members(0x6c), vec![0, 1, 2, 3]);
    assert_eq!(f.members(0x60), vec![1, 3]);
}
#[test]
fn primary_geometry_does_not_lose_near_colour() {
    let mut f = Fixture::new(9);
    f.list(0x24, &[0, 1, 2]);
    assert_eq!(f.apply().primary, 2);
    assert_eq!(f.members(0x24), vec![1]);
    assert_eq!(f.members(0xcc), vec![0, 1, 2, 3]);
    assert!(f.members(0x18).is_empty());
}
#[test]
fn all_original_background_lists_untouched() {
    let mut f = Fixture::new(9);
    f.list(0x18, &[3]);
    f.list(0x9c, &[3]);
    f.list(0xa8, &[3]);
    f.list(0x24, &[0, 2]);
    f.apply();
    for off in [0x18, 0x9c, 0xa8] {
        assert_eq!(f.members(off), vec![3]);
    }
}
#[test]
fn no_dependency_on_background_visibility_records_or_distance() {
    let mut f = Fixture::new(9);
    f.config[0x1e4 / 4] = 1 << 16;
    f.render[0x1c / 4] = 0;
    f.scene[0xec / 4] = u32::MAX;
    assert_eq!(f.apply().mask, 2);
    assert_eq!(f.members(0xcc), vec![0, 1, 2, 3]);
}
#[test]
fn failed_colour_registration_preserves_original_primary_and_mask() {
    let mut f = Fixture::new(9);
    f.list(0x24, &[0]);
    f.list(0xcc, &[]);
    f.pass[(0xcc + 8) / 4] = 0;
    assert_eq!(f.apply(), Changes::default());
    assert_eq!(f.members(0x24), vec![0]);
    assert_eq!(f.members(0xd8), vec![0, 1, 2, 3]);
}
#[test]
fn repeated_filter_is_idempotent_without_duplicate_colour() {
    let mut f = Fixture::new(9);
    f.list(0x24, &[0, 2]);
    f.apply();
    assert_eq!(f.apply(), Changes::default());
    assert_eq!(f.members(0xcc), vec![0, 1, 2, 3]);
}
#[test]
fn invalid_active_mask_fails_before_any_mutation() {
    let mut f = Fixture::new(9);
    f.list(0x24, &[0, 2]);
    f.pass[(0xd8 + 8) / 4] = u32::MAX;
    let before = f.pass.clone();
    assert_eq!(f.apply(), Changes::default());
    assert_eq!(f.pass, before);
}
#[test]
fn normal_sonar_and_foreign_view_remain_native() {
    for mode in 0..8 {
        let f = Fixture::new(mode);
        let before = f.pass.clone();
        assert_eq!(f.apply(), Changes::default());
        assert_eq!(f.pass, before);
    }
    let mut f = Fixture::new(9);
    f.selected[0] = 0;
    assert_eq!(f.apply(), Changes::default());
}
#[test]
fn eligible_scene_flags_exclude_characters_and_special_materials() {
    for lo in [0x400, 0x410, 0x10] {
        assert!(!scene_highlight(9, lo, 0x20000000, 120));
    }
    for hi in [0x10000000, 0x20040000, 0x20080000] {
        assert!(!scene_highlight(9, 0, hi, 120));
    }
    for lo in [0x200, 0x100, 0x20, 0x40, 0x48004] {
        assert!(scene_highlight(9, lo, 0x20000000, 120));
    }
    assert!(scene_highlight(9, 0x48004, 0x60001040, 50));
    assert!(!scene_highlight(8, 0, 0, 0));
    assert!(!scene_highlight(8, 0, 0, 255));
}
#[test]
fn state_materials_palette_and_actor_flags_are_not_written() {
    let f = Fixture::new(9);
    let saved = (
        f.render.clone(),
        f.view.clone(),
        f.selected.clone(),
        f.config.clone(),
        f.objects.clone(),
        f.records.clone(),
    );
    f.apply();
    assert_eq!(
        (f.render, f.view, f.selected, f.config, f.objects, f.records),
        saved
    );
}
#[test]
fn no_gpu_state_hook_or_fixed_opacity_or_background_migration() {
    let s = include_str!("vision_transparency.rs");
    for forbidden in [
        "SetState",
        "0x66666666",
        "Scope::enter",
        "Vec<",
        "Background::load",
        "emf.compact",
        "colour.compact",
    ] {
        assert!(!s.contains(forbidden), "{}", forbidden);
    }
    assert!(s.contains("Classify.get().expect(\"published before enable\").call(pass)"));
}
#[test]
fn late_mode_selection_does_not_drop_native_colours() {
    for mode in [8, 9] {
        let mut f = Fixture::new(mode);
        f.pass[0x10 / 4] = 0;
        assert_eq!(f.apply().mask, 2);
    }
}
#[test]
fn primary_only_prop_registers_colour_once_before_mask_removal() {
    let mut f = Fixture::new(9);
    f.list(0xcc, &[1, 3]);
    f.list(0x24, &[0, 0]);
    let a = f.apply();
    assert_eq!(a.primary, 2);
    assert_eq!(a.highlight, 1);
    assert_eq!(f.members(0xcc), vec![1, 3, 0]);
    assert_eq!(f.members(0xd8), vec![1, 2, 3]);
}
#[test]
fn mask_without_matching_colour_remains_native() {
    let mut f = Fixture::new(9);
    f.list(0xcc, &[1, 3]);
    assert_eq!(f.apply(), Changes::default());
    assert_eq!(f.members(0xd8), vec![0, 1, 2, 3]);
}
#[test]
fn other_mode_queues_are_not_modified_or_required() {
    let mut f = Fixture::new(9);
    f.pass[(0x60 + 8) / 4] = u32::MAX;
    f.list(0x6c, &[0, 2]);
    assert_eq!(f.apply().mask, 2);
    assert_eq!(f.members(0x6c), vec![0, 2]);
    assert_eq!(f.pass[(0x60 + 8) / 4], u32::MAX);
}
#[test]
fn mixed_character_and_scene_colour_order_is_preserved() {
    let mut f = Fixture::new(9);
    f.list(0xcc, &[3, 0, 1, 2]);
    f.list(0x24, &[0, 1, 2, 3]);
    f.apply();
    assert_eq!(f.members(0xcc), vec![3, 0, 1, 2]);
    assert_eq!(f.members(0x24), vec![1, 3]);
}
#[test]
fn excluded_material_retains_both_colour_and_occlusion() {
    let mut f = Fixture::new(9);
    f.objects[0][0x34 / 4] |= 0x10;
    f.list(0x24, &[0]);
    f.apply();
    assert_eq!(f.members(0xcc), vec![0, 1, 2, 3]);
    assert_eq!(f.members(0x24), vec![0]);
    assert_eq!(f.members(0xd8), vec![0, 1, 3]);
}
#[test]
fn colour_registration_failure_at_full_capacity_does_not_hide_prop() {
    let mut f = Fixture::new(9);
    f.list(0xcc, &[1; 32]);
    f.list(0x24, &[0]);
    assert_eq!(f.apply(), Changes::default());
    assert_eq!(f.members(0x24), vec![0]);
}
#[test]
fn empty_native_scene_is_noop() {
    let mut f = Fixture::new(9);
    for off in OFF {
        f.list(off, &[]);
    }
    assert_eq!(f.apply(), Changes::default());
}

static NATIVE_ENTRY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
unsafe fn native_classify(f: &Fixture) {
    type F = unsafe extern "thiscall" fn(usize);
    std::mem::transmute::<usize, F>(NATIVE_ENTRY.load(Ordering::Relaxed))(f.ptr());
}
#[test]
fn native_classifier_preserves_colour_for_both_primary_and_secondary() {
    unsafe {
        use windows::Win32::System::Memory::{
            VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
        };
        let base = VirtualAlloc(
            None,
            0x10000,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        );
        assert!(!base.is_null());
        NATIVE_ENTRY.store(base as usize, Ordering::Relaxed);
        let bytes = std::fs::read(
            std::path::PathBuf::from(std::env::var("SCBL_R58_C54_FIXTURE").unwrap())
                .join("native-01755d50.bin"),
        )
        .unwrap();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), base as *mut u8, bytes.len());
        for mode in [9, 8] {
            for (case, primary, hi) in [
                ("secondary", false, 0x20001040u32),
                ("primary", true, 0x20001040),
                ("primary_material", true, 0x60001040),
            ] {
                let mut f = Fixture::new(mode);
                f.config[0x1e4 / 4] = 1 << 16;
                f.scene[0xec / 4] = 1;
                f.objects[0][0x34 / 4] = 0x48004;
                f.objects[0][0x38 / 4] = hi;
                f.objects[0][0x210 / 4] = 50 << 24;
                let first = [f.objects[0].as_ptr() as u32];
                if primary {
                    f.scene[0x70 / 4] = first.as_ptr() as u32;
                    f.scene[0x74 / 4] = 1;
                }
                native_classify(&f);
                let colour = if mode == 9 { 0xcc } else { 0x6c };
                let mask = if mode == 9 { 0xd8 } else { 0x60 };
                assert_eq!(f.members(colour), vec![0]);
                assert_eq!(f.members(mask), vec![0]);
                let before = (f.members(0x24), f.members(colour), f.members(mask));
                f.apply();
                assert_eq!(f.members(colour), vec![0]);
                assert!(f.members(mask).is_empty());
                assert!(f.members(0x24).is_empty());
                assert!(f.members(0x18).is_empty());
                assert!(f.members(0x9c).is_empty());
                println!("C54_NATIVE_REPLAY mode={} case={} before={:?} primary_after={:?} colour_after={:?} depth_after={:?} background_primary={:?} background_regular={:?} real_classifier=true",mode,case,before,f.members(0x24),f.members(colour),f.members(mask),f.members(0x18),f.members(0x9c));
            }
        }
        VirtualFree(base, 0, MEM_RELEASE).unwrap();
    }
}

struct BrightFixture {
    f: Fixture,
    root: Vec<u32>,
    material: Vec<u32>,
    bank: Vec<u32>,
}
impl BrightFixture {
    fn new(mode: u32) -> Self {
        let mut f = Fixture::new(mode);
        let root = vec![0x2c06404; 0x230 / 4];
        f.pass[0] = root.as_ptr() as u32;
        f.selected[1] = root.as_ptr() as u32;
        let mut bank = vec![0xdeadbeef; 0x1000 / 4];
        for (o, v) in [
            (0xc10, [1.0f32; 4]),
            (0xc50, [1.0; 4]),
            (0xc70, [0.0; 4]),
            (0xc80, [0.0; 4]),
            (0xc60, [0.0, 1.0, 0.0, 1.0]),
            (0xca0, [1.0; 4]),
            (0xcb0, [0.0; 4]),
        ] {
            for i in 0..4 {
                bank[o / 4 + i] = v[i].to_bits();
            }
        }
        Self {
            f,
            root,
            material: vec![0x2c06404; 0x230 / 4],
            bank,
        }
    }
    unsafe fn apply(&mut self, object: usize) -> Option<([f32; 3], [f32; 3])> {
        let bank = self.bank.as_mut_ptr() as usize;
        let scope = brightness_scope(self.f.ptr(), bank)?;
        apply_brightness(
            scope,
            self.root.as_ptr() as usize,
            self.f.render.as_ptr() as usize,
            self.f.objects[object].as_ptr() as usize,
            self.material.as_ptr() as usize,
            bank,
        )
    }
    fn upload(&mut self, rgb: [f32; 3]) {
        for (i, v) in rgb.iter().enumerate() {
            self.bank[0xc30 / 4 + i] = v.to_bits();
        }
    }
    fn rgb(&self) -> [f32; 3] {
        std::array::from_fn(|i| f32::from_bits(self.bank[0xc30 / 4 + i]))
    }
}
#[test]
fn brightness_unifies_first_instead_of_multiplying_each_old_peak() {
    for peak in [0.25, 1.0, 2.0, 8.0, 64.0] {
        let out = unified_rgb([peak, peak * 0.5, 0.0]).unwrap();
        assert_eq!(out, [0.3, 0.15, 0.0]);
    }
}
#[test]
fn brightness_preserves_hue_and_excludes_invalid_or_black() {
    assert_eq!(unified_rgb([0.0, 4.0, 2.0]), Some([0.0, 0.3, 0.15]));
    for v in [
        [0.0; 3],
        [f32::NAN, 1.0, 1.0],
        [1.0, f32::INFINITY, 0.0],
        [-1.0, 2.0, 0.0],
    ] {
        assert!(unified_rgb(v).is_none());
    }
}
#[test]
fn brightness_valid_palette_leaves_it_and_material_unchanged() {
    for mode in [8, 9] {
        unsafe {
            let mut b = BrightFixture::new(mode);
            b.upload([8.0, 4.0, 0.0]);
            let mat = b.material.clone();
            let objects = b.f.objects.clone();
            let bank = b.bank.clone();
            assert!(b.apply(0).is_some());
            assert_eq!(
                b.rgb(),
                contrast_rgb_for_mode(b.f.view[0x340 / 4], [8.0, 4.0, 0.0], 1.0).unwrap()
            );
            assert_eq!(b.material, mat);
            assert_eq!(b.f.objects, objects);
            for i in 0..bank.len() {
                if !(0xc30 / 4..0xc3c / 4).contains(&i) {
                    assert_eq!(b.bank[i], bank[i]);
                }
            }
        }
    }
}
#[test]
fn brightness_skips_characters_and_normal_sonar_views() {
    for mode in [8, 9] {
        unsafe {
            let mut b = BrightFixture::new(mode);
            b.upload([8.0, 4.0, 0.0]);
            for i in [1, 3] {
                assert!(b.apply(i).is_none());
                assert_eq!(b.rgb(), [8.0, 4.0, 0.0]);
            }
        }
    }
    for mode in 0..8 {
        unsafe {
            let b = BrightFixture::new(mode);
            assert!(brightness_scope(b.f.ptr(), b.bank.as_ptr() as usize).is_none());
        }
    }
}
#[test]
fn brightness_next_native_upload_restores_character_intensity() {
    unsafe {
        let mut b = BrightFixture::new(9);
        b.upload([8.0, 4.0, 0.0]);
        b.apply(0);
        b.upload([3.0, 1.5, 0.0]);
        assert!(b.apply(1).is_none());
        assert_eq!(b.rgb(), [3.0, 1.5, 0.0]);
        b.upload([8.0, 4.0, 0.0]);
        b.apply(0);
        assert_eq!(
            b.rgb(),
            contrast_rgb_for_mode(b.f.view[0x340 / 4], [8.0, 4.0, 0.0], 1.0).unwrap()
        );
    }
}
#[test]
fn brightness_scope_rejects_foreign_material_missing_bank_and_reconfigured_view() {
    unsafe {
        let mut b = BrightFixture::new(9);
        let bank = b.bank.as_mut_ptr() as usize;
        assert!(brightness_scope(b.f.ptr(), 0).is_none());
        let scope = brightness_scope(b.f.ptr(), bank).unwrap();
        b.f.view[0x340 / 4] = 0;
        assert!(apply_brightness(
            scope,
            b.root.as_ptr() as usize,
            b.f.render.as_ptr() as usize,
            b.f.objects[0].as_ptr() as usize,
            b.material.as_ptr() as usize,
            bank
        )
        .is_none());
        b.f.view[0x340 / 4] = 9;
        b.f.selected[1] = 0;
        assert!(brightness_scope(b.f.ptr(), bank).is_none());
    }
}
#[test]
fn brightness_scope_is_thread_local_and_no_target_for_null_native_result() {
    unsafe {
        let b = BrightFixture::new(9);
        let bank = b.bank.as_ptr() as usize;
        let scope = brightness_scope(b.f.ptr(), bank).unwrap();
        BRIGHTNESS_SCOPE.with(|s| s.set(scope));
        std::thread::spawn(|| assert_eq!(BRIGHTNESS_SCOPE.with(Cell::get).bank, 0))
            .join()
            .unwrap();
        assert!(apply_brightness(
            scope,
            b.root.as_ptr() as usize,
            b.f.render.as_ptr() as usize,
            b.f.objects[0].as_ptr() as usize,
            0,
            bank
        )
        .is_none());
        assert!(apply_brightness(
            scope,
            b.root.as_ptr() as usize,
            b.f.render.as_ptr() as usize,
            b.f.objects[0].as_ptr() as usize,
            b.material.as_ptr() as usize,
            bank + 4
        )
        .is_none());
        BRIGHTNESS_SCOPE.with(|s| s.set(BrightnessScope::default()));
    }
}
#[test]
#[ignore = "retail native-byte fixture"]
fn brightness_native_contracts() {
    use sha2::{Digest, Sha256};
    let e = std::path::PathBuf::from(std::env::var("SCBL_R58_C57_FIXTURE").unwrap());
    for &(a, n, h) in BRIGHTNESS_CONTRACTS {
        let bytes = std::fs::read(e.join(format!("native-{a:08x}.bin"))).unwrap();
        assert_eq!(bytes.len(), n);
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), h);
    }
}

#[test]
fn secondary_colours_cap_bright_values_without_raising_dark_ones() {
    assert_eq!(subdued_rgb([8.0, 4.0, 0.0]), Some([0.6, 0.3, 0.0]));
    assert_eq!(subdued_rgb([1.0, 0.5, 0.0]), Some([0.6, 0.3, 0.0]));
    assert_eq!(subdued_rgb([0.25, 0.125, 0.0]), Some([0.15, 0.075, 0.0]));
    assert_eq!(subdued_rgb([0.0; 3]), Some([0.0; 3]));
    for rgb in [
        [f32::NAN, 0.0, 0.0],
        [0.0, f32::INFINITY, 1.0],
        [-0.1, 1.0, 1.0],
    ] {
        assert!(subdued_rgb(rgb).is_none());
    }
}
#[test]
fn prop_base_only_preserves_native_secondary_palette_and_materials() {
    for mode in [8, 9] {
        unsafe {
            let mut b = BrightFixture::new(mode);
            b.upload([8.0, 4.0, 0.0]);
            for (offset, rgb) in [
                (0xc50, [0.25f32, 0.125, 0.0]),
                (0xc70, [0.0; 3]),
                (0xc80, [8.0, 4.0, 0.0]),
            ] {
                for (i, x) in rgb.iter().enumerate() {
                    b.bank[offset / 4 + i] = x.to_bits();
                }
            }
            let old = b.bank.clone();
            let material = b.material.clone();
            let objects = b.f.objects.clone();
            assert!(b.apply(0).is_some());
            for (offset, rgb) in [
                (
                    0xc30,
                    contrast_rgb_for_mode(mode, [8.0, 4.0, 0.0], 8.25).unwrap(),
                ),
                (0xc50, [0.25, 0.125, 0.0]),
                (0xc70, [0.0; 3]),
                (0xc80, [8.0, 4.0, 0.0]),
            ] {
                for (i, x) in rgb.iter().enumerate() {
                    assert_eq!(b.bank[offset / 4 + i], x.to_bits());
                }
            }
            for i in 0..old.len() {
                if ![0xc30].iter().any(|o| (*o / 4..*o / 4 + 3).contains(&i)) {
                    assert_eq!(b.bank[i], old[i]);
                }
            }
            assert_eq!(b.material, material);
            assert_eq!(b.f.objects, objects);
        }
    }
}
#[test]
fn character_and_normal_view_retain_all_secondary_colour_bytes() {
    for mode in [8, 9] {
        unsafe {
            let mut b = BrightFixture::new(mode);
            b.upload([2.0, 1.0, 0.0]);
            for o in SECONDARY_COLOUR_OFFSETS {
                for i in 0..3 {
                    b.bank[o / 4 + i] = 8.0f32.to_bits();
                }
            }
            let before = b.bank.clone();
            for object in [1, 3] {
                assert!(b.apply(object).is_none());
                assert_eq!(b.bank, before);
            }
            b.f.view[0x340 / 4] = 0;
            assert!(b.apply(0).is_none());
            assert_eq!(b.bank, before);
        }
    }
}
#[test]
fn next_native_palette_upload_prevents_accumulation_and_character_leakage() {
    unsafe {
        let mut b = BrightFixture::new(9);
        let mut native = b.bank.clone();
        for o in [0xc30, 0xc50, 0xc70, 0xc80] {
            native[o / 4] = 1.0f32.to_bits();
            native[o / 4 + 1] = 0.5f32.to_bits();
            native[o / 4 + 2] = 0.0f32.to_bits();
        }
        for _ in 0..10 {
            b.bank.clone_from(&native);
            b.apply(0);
            assert_eq!(f32::from_bits(b.bank[0xc50 / 4]), 1.0);
        }
        b.bank.clone_from(&native);
        assert!(b.apply(1).is_none());
        assert_eq!(b.bank, native);
    }
}
#[test]
#[ignore = "retail native-byte fixture"]
fn secondary_colour_native_registration_matches_upload_offsets() {
    use sha2::{Digest, Sha256};
    let e = std::path::PathBuf::from(std::env::var("SCBL_R58_C58_FIXTURE").unwrap());
    let bytes = std::fs::read(e.join("native-016e58c0.bin")).unwrap();
    assert_eq!(bytes.len(), 0x3bc);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "56e2e397c1c69502ebaec3baee756e58493420921c25b52bf85550693a35f40c"
    );
    for (site, name, field) in [
        (0x16e598busize, 0x2c064e8u32, 0x173u32),
        (0x16e59b2, 0x2c064d8, 0x179),
        (0x16e59d9, 0x2c064cc, 0x176),
    ] {
        let i = site - 0x16e58c0;
        assert_eq!(bytes[i], 0x68);
        assert_eq!(&bytes[i + 1..i + 5], &name.to_le_bytes());
        assert_eq!(bytes[i + 16], 0x68);
        assert_eq!(&bytes[i + 17..i + 21], &field.to_le_bytes());
    }
    assert_eq!(SECONDARY_COLOUR_OFFSETS, [0xc50, 0xc70, 0xc80]);
}

#[test]
fn base_peak_tuning_does_not_retune_secondary_palette() {
    assert_eq!(SCENE_HIGHLIGHT_PEAK, 0.3);
    assert_eq!(SECONDARY_COLOUR_PEAK, 0.6);
    assert!(!SUBDUE_SECONDARY_COLOURS);
    assert_eq!(unified_rgb([8.0, 4.0, 0.0]), Some([0.3, 0.15, 0.0]));
    assert_eq!(subdued_rgb([8.0, 4.0, 0.0]), Some([0.6, 0.3, 0.0]));
}

mod c60_queue_region_tests {
    use super::*;
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(
            p: *mut std::ffi::c_void,
            n: usize,
            t: u32,
            protect: u32,
        ) -> *mut std::ffi::c_void;
        fn VirtualFree(p: *mut std::ffi::c_void, n: usize, t: u32) -> i32;
        fn VirtualProtect(p: *mut std::ffi::c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    }
    struct SplitRegion {
        p: usize,
    }
    impl SplitRegion {
        unsafe fn new() -> Self {
            // Adjacent independent RW reservations: the same protection but distinct
            // VirtualQuery regions, matching the observed game queue's boundary.
            let p = VirtualAlloc(std::ptr::null_mut(), 0x20000, 0x2000, 4) as usize;
            assert_ne!(p, 0);
            assert_ne!(VirtualFree(p as _, 0, 0x8000), 0);
            let a = VirtualAlloc(p as _, 0x10000, 0x3000, 4) as usize;
            assert_eq!(a, p);
            let z = VirtualAlloc((p + 0x10000) as _, 0x10000, 0x3000, 4) as usize;
            if z != p + 0x10000 {
                VirtualFree(p as _, 0, 0x8000);
                panic!("adjacent fixture reservation unavailable");
            }
            Self { p }
        }
        fn boundary(&self) -> usize {
            self.p + 0x10000
        }
        unsafe fn protection(&self, protection: u32) {
            let mut old = 0;
            assert_ne!(
                VirtualProtect(self.boundary() as _, 0x10000, protection, &mut old),
                0
            );
        }
    }
    impl Drop for SplitRegion {
        fn drop(&mut self) {
            unsafe {
                VirtualFree(self.p as _, 0, 0x8000);
                VirtualFree(self.boundary() as _, 0, 0x8000);
            }
        }
    }
    fn replay(mode: u32, tail_only: bool) -> (Changes, bool, bool) {
        unsafe {
            let pages = SplitRegion::new();
            let mut f = super::Fixture::new(mode);
            f.list(0x24, &[0, 1, 2]);
            let off = if mode == 9 { 0xcc } else { 0x6c };
            let start = pages.boundary() - if tail_only { 24 } else { 8 };
            for i in 0..4 {
                put(start, i * 4, f.objects[i].as_ptr() as u32);
            }
            put(f.ptr(), off, start as u32);
            put(f.ptr(), off + 4, 4);
            put(f.ptr(), off + 8, 16);
            let original_single_region_guard = writable(start, 64);
            assert!(!original_single_region_guard);
            let change = f.apply();
            let colours = (0..4).all(|i| raw(start, i * 4) == f.objects[i].as_ptr() as u32);
            let characters = f.members(0x24).contains(&1)
                && f.members(if mode == 9 { 0xd8 } else { 0x60 }).contains(&3);
            (change, colours, characters)
        }
    }
    #[test]
    fn c60_same_input_queue_regions() {
        for mode in [9, 8] {
            for tail_only in [true, false] {
                let (changes, colours, characters) = replay(mode, tail_only);
                println!("C60_QUEUE_REPLAY mode={} capacity_tail_only={} primary_removed={} colour_added={} mask_removed={} colours_preserved={} characters_preserved={} single_region_guard=false game_pixels=false",mode,tail_only,changes.primary,changes.highlight,changes.mask,colours,characters);
                assert!(colours && characters);
            }
        }
    }
    #[test]
    fn adjacent_writable_regions_do_not_disable_emf_or_thermal_routing() {
        for mode in [9, 8] {
            for tail_only in [true, false] {
                let (changes, colours, characters) = replay(mode, tail_only);
                assert_eq!(
                    changes,
                    Changes {
                        primary: 2,
                        highlight: 0,
                        mask: 2
                    }
                );
                assert!(colours && characters);
            }
        }
    }
    #[test]
    fn cross_region_queue_header_is_supported() {
        unsafe {
            let pages = SplitRegion::new();
            let header = pages.boundary() - 4;
            let mut data = [0u32; 4];
            put(header, 0, data.as_mut_ptr() as u32);
            put(header, 4, 0);
            put(header, 8, 4);
            assert!(!writable(header, 12));
            assert!(Array::load(header).is_some());
        }
    }
    #[test]
    fn read_only_noaccess_and_guarded_tail_rejected_before_mutation() {
        unsafe {
            for protection in [2, 1, 0x104] {
                let pages = SplitRegion::new();
                let mut f = super::Fixture::new(9);
                f.list(0x24, &[0, 1, 2]);
                let start = pages.boundary() - 24;
                for i in 0..4 {
                    put(start, i * 4, f.objects[i].as_ptr() as u32);
                }
                put(f.ptr(), 0xcc, start as u32);
                put(f.ptr(), 0xd0, 4);
                put(f.ptr(), 0xd4, 16);
                pages.protection(protection);
                let pass = f.pass.clone();
                let storage = f.storage.clone();
                assert_eq!(f.apply(), Changes::default());
                assert_eq!(f.pass, pass);
                assert_eq!(f.storage, storage);
            }
        }
    }
    #[test]
    fn uncommitted_tail_and_overflow_rejected() {
        unsafe {
            let pages = SplitRegion::new();
            assert_ne!(VirtualFree(pages.boundary() as _, 0x10000, 0x4000), 0);
            let header = [(pages.boundary() - 24) as u32, 4, 16];
            assert!(Array::load(header.as_ptr() as usize).is_none());
            let invalid = [u32::MAX - 3, 1, 16];
            assert!(Array::load(invalid.as_ptr() as usize).is_none());
        }
    }
    #[test]
    fn cross_region_capacity_does_not_bypass_item_limits() {
        unsafe {
            let pages = SplitRegion::new();
            let header = [(pages.boundary() - 8) as u32, 1, (MAX_ITEMS + 1) as u32];
            assert!(Array::load(header.as_ptr() as usize).is_none());
            let invalid = [pages.p as u32, 2, 1];
            assert!(Array::load(invalid.as_ptr() as usize).is_none());
        }
    }
}

#[test]
fn c62_smooth_shoulder_preserves_hue_and_does_not_raise_dim_materials() {
    for gain in [1.0, 2.0, 8.0, 64.0, 1024.0] {
        let mut previous = 0.0;
        for peak in [0.00001, 0.01, 0.1, 0.3, 1.0, 4.0, 16.0, 1000.0] {
            let out = contrast_rgb([peak, peak * 0.5, 0.0], gain).unwrap();
            assert!(out[0] <= peak * 0.5 && out[0] > previous);
            assert!((out[0] * gain) <= PROP_SIGNAL_CEILING + 0.000001);
            assert_eq!(out[1], out[0] * 0.5);
            assert_eq!(out[2], 0.0);
            previous = out[0];
        }
    }
}
#[test]
fn c62_invalid_inputs_fail_before_any_material_write() {
    for mode in [8, 9] {
        for offset in [0xc50, 0xc60, 0xc70, 0xc80, 0xca0, 0xcb0] {
            unsafe {
                let mut f = BrightFixture::new(mode);
                f.upload([8.0, 4.0, 0.0]);
                f.bank[offset / 4] = f32::NAN.to_bits();
                let before = f.bank.clone();
                assert!(f.apply(0).is_none());
                assert_eq!(f.bank, before);
            }
        }
    }
    for gain in [0.0, 0.99, f32::INFINITY, f32::NAN] {
        assert!(contrast_rgb([1.0; 3], gain).is_none());
    }
    for rgb in [[0.0; 3], [-1.0, 1.0, 1.0], [f32::NAN, 1.0, 1.0]] {
        assert!(contrast_rgb(rgb, 1.0).is_none());
    }
    let out = contrast_rgb([f32::MAX; 3], f32::MAX).unwrap();
    assert!(out.iter().all(|v| v.is_finite() && *v >= 0.0));
    assert!(lighting_envelope(
        [[f32::MAX; 3]; 3],
        [f32::MAX; 3],
        [1.0; 3],
        [1.0; 3],
        [1.0; 4]
    )
    .is_none());
}
#[test]
fn c62_native_material_amplification_is_compensated_without_palette_or_distance_writes() {
    for mode in [8, 9] {
        for gain in [1.0f32, 8.0, 64.0] {
            unsafe {
                let mut f = BrightFixture::new(mode);
                f.upload([1.0, 0.5, 0.0]);
                for i in 0..3 {
                    f.bank[0xc80 / 4 + i] = gain.to_bits();
                }
                f.bank[0xc3c / 4] = 0.025f32.to_bits();
                let old = f.bank.clone();
                let envelope = material_envelope(f.bank.as_ptr() as usize).unwrap();
                assert_eq!(envelope, 1.0 + gain);
                assert!(f.apply(0).is_some());
                // C65 EMF uses a perceptual sqrt-gain knee, not a worst-case signal cap.
                let compensated_gain = if mode == 9 { envelope.sqrt() } else { envelope };
                assert!(f.rgb()[0] * compensated_gain < if mode == 9 { 0.40 } else { 0.20 });
                for i in 0..old.len() {
                    if !(0xc30 / 4..0xc3c / 4).contains(&i) {
                        assert_eq!(f.bank[i], old[i]);
                    }
                }
                // The native distance lookup coordinate retains its original range.
                for distance in [1.0, 5.0, 20.0, 50.0] {
                    let old_y = (1.0 - distance * f32::from_bits(old[0xc3c / 4])).clamp(0.0, 1.0);
                    let new_y =
                        (1.0 - distance * f32::from_bits(f.bank[0xc3c / 4])).clamp(0.0, 1.0);
                    assert_eq!(old_y, new_y);
                }
            }
        }
    }
}
#[test]
fn c62_lighting_bound_covers_normalized_static_shader_samples() {
    let palette = [[0.2, 0.1, 0.0], [4.0, 2.0, 1.0], [8.0, 4.0, 2.0]];
    let scale = [2.0, 1.0, 0.5];
    let bias = [0.1, 0.2, 0.3];
    let view = [1.0, 0.8, 0.6];
    let ao = [0.5, 0.7, 0.0, 2.0];
    let bound = lighting_envelope(palette, scale, bias, view, ao).unwrap();
    for normal in [0.0, 0.2, 0.5, 1.0] {
        for shadow in [0.0, 0.5, 1.0] {
            for tex in [0.0f32, 0.2, 0.5, 1.0] {
                for self_ao in [0.0f32, 0.5, 1.0] {
                    for i in 0..3 {
                        let lit = (palette[0][i]
                            + normal * (palette[2][i] + shadow * palette[1][i]))
                            * (tex * tex * scale[i] * view[i] + bias[i])
                            * (ao[0] * self_ao.powf(ao[3]) + ao[1]);
                        assert!(lit.max(1.0) <= bound + 0.00001);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "retail original shader and upload fixtures"]
fn c62_native_shader_and_new_read_only_upload_contracts() {
    use sha2::{Digest, Sha256};
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C62_FIXTURE").unwrap());
    for (file, len, hash) in [
        (
            "native-shaders/ps-2628.dxbc",
            7536,
            "7f6c21fe156da3b367b2f5ce8b12b8b2973aae9ed02915d0b7efadb3714781e5",
        ),
        (
            "native-shaders/ps-2730.dxbc",
            8888,
            "897708c0598a9fc36d2674a9bf1ecf92eede8037a00abd9eb435bd30e01d446a",
        ),
        (
            "native-shaders/ps-2840.dxbc",
            11064,
            "b0833271c95e5138669698da84967d5a51cb00e3115b0eb903efcf1c1875030c",
        ),
        (
            "native-0175bf20.bin",
            27,
            "a220d85f9bdb670761e19abe56c25da384b02bb0069c3168130e353ef560c297",
        ),
    ] {
        let bytes = std::fs::read(root.join(file)).unwrap();
        assert_eq!(bytes.len(), len);
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), hash);
    }
    let bytes = std::fs::read(root.join("native-0175bf20.bin")).unwrap();
    assert_eq!(CONTRAST_UPLOAD_CONTRACT.1, bytes.len());
    assert_eq!(
        CONTRAST_UPLOAD_CONTRACT.2,
        format!("{:x}", Sha256::digest(bytes))
    );
    let shader = std::fs::read_to_string(root.join("native-shaders/ps-2628.txt")).unwrap();
    assert!(shader.contains("mul r2.yzw, r0.wwww, cb0[3].xxyz"));
    assert!(shader.contains("mad_sat r0.y, -v6.w, cb0[3].w, l(1.000000)"));
    assert!(shader.contains("cThermalDiffuseScale;       // Offset:  160"));
    assert!(shader.contains("cThermalDiffuseBias;        // Offset:  176"));
    assert!(shader.contains("cViewModeDiffCol;           // Offset:   32"));
}

#[test]
fn c63_signed_distance_and_stale_view_colour_do_not_skip_or_change_prop_gain() {
    for mode in [8, 9] {
        let mut reference = None;
        for unrelated in [-0.0007246377f32, -1.0, 0.0, 1.0, 64.0, f32::NAN] {
            unsafe {
                let mut f = BrightFixture::new(mode);
                f.upload([0.3137255, 0.147, 0.0246]);
                for i in 0..3 {
                    f.bank[0xc10 / 4 + i] = unrelated.to_bits();
                    f.bank[0xae0 / 4 + i] = unrelated.to_bits();
                    f.bank[0xca0 / 4 + i] = 20.0f32.to_bits();
                }
                let old = f.bank.clone();
                assert!(f.apply(0).is_some());
                if let Some(expected) = reference {
                    assert_eq!(f.rgb(), expected);
                }
                reference = Some(f.rgb());
                for i in 0..old.len() {
                    if !(0xc30 / 4..0xc3c / 4).contains(&i) {
                        assert_eq!(f.bank[i], old[i]);
                    }
                }
            }
        }
    }
}
#[test]
fn c63_current_draw_bound_covers_white_and_normalized_tints() {
    let palette = [[0.1960784, 0.06920415, 0.0], [0.0; 3], [1.0; 3]];
    let scale = [20.0; 3];
    let bias = [0.2; 3];
    let ao = [1.0, 0.0, 0.0, 1.9960784];
    let bound = lighting_envelope(palette, scale, bias, [1.0; 3], ao).unwrap();
    for r in [0.0, 0.2, 0.5, 1.0] {
        for g in [0.0, 0.3, 1.0] {
            for b in [0.0, 0.6, 1.0] {
                assert!(lighting_envelope(palette, scale, bias, [r, g, b], ao).unwrap() <= bound);
            }
        }
    }
    let out = contrast_rgb([0.3137255; 3], bound).unwrap();
    assert!(out[0] * bound < PROP_SIGNAL_CEILING);
}
#[test]
#[ignore = "retail native register and normalized colour producer fixtures"]
fn c63_native_register_mapping_and_colour_producer_contracts() {
    use sha2::{Digest, Sha256};
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C63_FIXTURE").unwrap());
    for &(address, len, hash) in CONTRAST_NATIVE_CONTRACTS {
        let data = std::fs::read(root.join(format!("native-{address:08x}.bin"))).unwrap();
        assert_eq!(data.len(), len);
        assert_eq!(format!("{:x}", Sha256::digest(&data)), hash);
    }
    let table = std::fs::read(root.join("native-031938a8.bin")).unwrap();
    assert_eq!(&table[..17], b"cViewModeDiffCol\0");
    assert_eq!(
        u32::from_le_bytes(table[0x104..0x108].try_into().unwrap()),
        0xae
    );
    let table = std::fs::read(root.join("native-03193068.bin")).unwrap();
    assert_eq!(
        u32::from_le_bytes(table[0x104..0x108].try_into().unwrap()),
        0xc3
    );
    let white = std::fs::read(root.join("native-02d75c00.bin")).unwrap();
    assert_eq!(f32::from_le_bytes(white.try_into().unwrap()), 1.0);
}

#[test]
fn emf_restores_details_without_raising_above_native_material_input() {
    for gain in [1.0, 2.0, 8.0, 24.2, 1000.0] {
        for peak in [0.00001, 0.01, 0.1, 0.3137255, 1.0, 8.0, 1000.0] {
            let base = [peak, peak * 0.5, 0.0];
            let old = contrast_rgb(base, gain).unwrap();
            let new = contrast_rgb_for_mode(9, base, gain).unwrap();
            assert!(new[0] > old[0]);
            assert!(new[0] <= peak && new[0] * gain.sqrt() <= 0.400001);
            assert_eq!(new[1], new[0] * 0.5);
            assert_eq!(new[2], 0.0);
        }
    }
}
#[test]
fn c64_thermal_policy_remains_exactly_c63_and_other_views_fail_closed() {
    for gain in [1.0f32, 8.0, 24.2] {
        for peak in [0.01f32, 0.3137255, 8.0] {
            let old_scale =
                (0.5 / (1.0 + peak as f64 * gain as f64 / (2.0 * 0.20f32 as f64))) as f32;
            assert_eq!(
                contrast_rgb_for_mode(8, [peak; 3], gain).unwrap(),
                [peak * old_scale; 3]
            );
        }
    }
    for mode in [0, 1, 2, 3, 4, 5, 6, 7, 10, u32::MAX] {
        assert!(contrast_rgb_for_mode(mode, [1.0; 3], 1.0).is_none());
    }
    for mode in [8, 9] {
        for rgb in [
            [0.0; 3],
            [-1.0, 1.0, 1.0],
            [f32::NAN; 3],
            [f32::INFINITY; 3],
        ] {
            assert!(contrast_rgb_for_mode(mode, rgb, 1.0).is_none());
        }
    }
}
#[test]
fn c64_draw_diagnostics_are_separate_by_native_stage_and_bounded() {
    for (mode, stage, bucket) in [(8, 2, 0), (8, 4, 1), (9, 7, 2), (9, 8, 3)] {
        assert_eq!(brightness_log_bucket(mode, stage), Some(bucket));
    }
    assert_eq!(brightness_log_bucket(0, 2), None);
    assert_eq!(brightness_log_bucket(8, 7), None);
    assert_eq!(brightness_log_bucket(9, 4), None);
    let logs = AtomicU32::new(0);
    assert!(take_brightness_log(&logs));
    assert!(take_brightness_log(&logs));
    for _ in 0..1000 {
        assert!(!take_brightness_log(&logs));
    }
    assert_eq!(logs.load(Ordering::Relaxed), 2);
}

#[test]
fn c65_replays_actual_c64_emf_draw_and_reduces_overcompression() {
    let base = [1.0f32, 0.5176471, 0.0];
    let envelope = 40.7f32;
    let c64_scale = (0.5 / (1.0 + envelope as f64 / (2.0 * 0.40f32 as f64))) as f32;
    assert!((c64_scale - 0.009638554).abs() < 0.00000001);
    let current = contrast_rgb_for_mode(9, base, envelope).unwrap();
    assert!(current[0] > c64_scale * 5.0 && current[0] < c64_scale * 7.0);
    assert!(current[0] < base[0]);
    assert!((current[1] / current[0] - base[1]).abs() < 0.000001);
    assert_eq!(current[2], 0.0);
}
#[test]
fn c65_emf_low_signals_keep_native_detail_without_a_brightness_floor() {
    for gain in [1.0f32, 8.0, 40.7, 1000.0] {
        let mut previous = 0.0;
        for peak in [0.0000011f32, 0.00001, 0.001, 0.01, 0.1, 1.0, 16.0] {
            let out = contrast_rgb_for_mode(9, [peak, peak * 0.5, 0.0], gain).unwrap();
            assert!(out[0] > previous && out[0] <= peak);
            assert_eq!(out[1], out[0] * 0.5);
            if peak <= 0.00001 {
                assert!(out[0] / peak > 0.999);
            }
            previous = out[0];
        }
    }
    assert!(contrast_rgb_for_mode(9, [0.0; 3], 40.7).is_none());
}
#[test]
fn c65_emf_policy_is_finite_for_extreme_native_inputs() {
    for gain in [1.0f32, f32::MAX] {
        let out = contrast_rgb_for_mode(9, [f32::MAX, 1.0, 0.0], gain).unwrap();
        assert!(out.iter().all(|x| x.is_finite() && *x >= 0.0));
    }
    for gain in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
        assert!(contrast_rgb_for_mode(9, [1.0; 3], gain).is_none());
    }
}

#[test]
fn c66_only_thermal_second_material_family_changes() {
    let base = [0.3137255, 0.14705658, 0.02460593];
    for mode in [8, 9] {
        let old = contrast_rgb_for_mode(mode, base, 8.088236).unwrap();
        for stage in 0..12 {
            for secondary in [false, true] {
                let new = contrast_rgb_for_draw(mode, stage, secondary, base, 8.088236).unwrap();
                let expected = if mode == 8 && stage == 4 && secondary {
                    old.map(|v| v * 0.35)
                } else {
                    old
                };
                assert_eq!(new, expected);
            }
        }
    }
    assert!(contrast_rgb_for_draw(0, 4, true, base, 1.0).is_none());
    assert!(contrast_rgb_for_draw(8, 4, true, [f32::NAN; 3], 1.0).is_none());
    assert!(contrast_rgb_for_draw(8, 4, true, base, f32::INFINITY).is_none());
}
#[test]
fn c66_second_family_changes_only_bank_rgb_not_native_material_or_alpha() {
    unsafe {
        let mut f = BrightFixture::new(8);
        f.f.render[0x233c / 4] = 4;
        f.material[0x18c / 4] = 1;
        f.upload([0.3137255, 0.14705658, 0.02460593]);
        let old = f.bank.clone();
        let material = f.material.clone();
        let objects = f.f.objects.clone();
        let root = f.root.clone();
        let base = f.rgb();
        assert!(f.apply(0).is_some());
        assert_eq!(
            f.rgb(),
            contrast_rgb_for_mode(8, base, 1.0)
                .unwrap()
                .map(|v| v * 0.35)
        );
        for i in 0..old.len() {
            if !(0xc30 / 4..0xc3c / 4).contains(&i) {
                assert_eq!(old[i], f.bank[i]);
            }
        }
        assert_eq!(material, f.material);
        assert_eq!(objects, f.f.objects);
        assert_eq!(root, f.root);
        for object in [1, 3] {
            f.upload(base);
            let old = f.bank.clone();
            assert!(f.apply(object).is_none());
            assert_eq!(old, f.bank);
        }
    }
}
#[test]
fn c66_stage_and_mode_transition_does_not_accumulate_gain() {
    unsafe {
        for mode in [8, 9] {
            let mut f = BrightFixture::new(mode);
            let base = [0.3137255, 0.14705658, 0.02460593];
            for (stage, kind) in [(4, 1), (2, 0), (4, 1), (7, 0), (8, 1), (4, 0)] {
                f.f.render[0x233c / 4] = stage;
                f.material[0x18c / 4] = kind;
                f.upload(base);
                assert!(f.apply(0).is_some());
                let expected = contrast_rgb_for_draw(mode, stage, kind != 0, base, 1.0).unwrap();
                assert_eq!(f.rgb(), expected);
                f.upload(base);
                assert!(f.apply(1).is_none());
                assert_eq!(f.rgb(), base);
            }
        }
    }
}


mod queue_owner_boundary {
use super::*;
unsafe extern "thiscall" fn no_grow(_: usize) {}
struct Fixture {
    render: Vec<u32>,
    view: Vec<u32>,
    pass: Vec<u32>,
    selected: Vec<u32>,
    mode_table: Vec<u32>,
    config: Vec<u32>,
    scene: Vec<u32>,
    records: Vec<u32>,
    objects: Vec<Vec<u32>>,
    storage: Vec<Vec<u32>>,
}
const OFF: [usize; 8] = [0x18, 0x9c, 0xa8, 0x24, 0xcc, 0x6c, 0xd8, 0x60];
impl Fixture {
    fn new(mode: u32) -> Self {
        let mut f = Self {
            render: vec![0; 0x2400 / 4],
            view: vec![0; 0x400 / 4],
            pass: vec![0; 0x400 / 4],
            selected: vec![0; 2],
            mode_table: vec![0; 10],
            config: vec![0; 0x400 / 4],
            scene: vec![0; 0x400 / 4],
            records: vec![0; 32],
            objects: (0..4).map(|_| vec![0; 0x220 / 4]).collect(),
            storage: (0..8).map(|_| vec![0; 32]).collect(),
        };
        f.render[0x20d0 / 4] = f.view.as_ptr() as u32;
        f.render[0x236c / 4] = f.pass.as_ptr() as u32;
        f.render[0x14 / 4] = f.config.as_ptr() as u32;
        f.render[0x1c / 4] = f.scene.as_ptr() as u32;
        f.render[0x2208 / 4] = u32::MAX;
        f.pass[0x14 / 4] = f.render.as_ptr() as u32;
        f.pass[0x10 / 4] = f.selected.as_ptr() as u32;
        f.mode_table[mode as usize] = f.selected.as_ptr() as u32;
        f.pass[8 / 4] = f.mode_table.as_ptr() as u32;
        f.selected[0] = if mode == 9 { 0x2c0965c } else { 0x2c094a4 };
        f.view[0x340 / 4] = mode;
        for (i, o) in OFF.iter().enumerate() {
            f.pass[*o / 4] = f.storage[i].as_ptr() as u32;
            f.pass[(*o + 8) / 4] = 32;
        }
        for i in 0..4 {
            f.records[i * 2] = f.objects[i].as_ptr() as u32;
            f.records[i * 2 + 1] = 1;
        }
        f.scene[0xe8 / 4] = f.records.as_ptr() as u32;
        f.scene[0xec / 4] = 4;
        f.objects[0][0x38 / 4] = 0x20000000;
        f.objects[0][0x210 / 4] = 120 << 24;
        f.objects[1][0x38 / 4] = 0x10000000;
        f.objects[1][0x210 / 4] = 120 << 24;
        f.objects[2][0x38 / 4] = 0x20000000;
        f.objects[2][0x34 / 4] = 0x200;
        f.objects[2][0x210 / 4] = 120 << 24;
        f.objects[3][0x34 / 4] = 0x400;
        f.objects[3][0x210 / 4] = 120 << 24;
        let color = if mode == 9 { 0xcc } else { 0x6c };
        f.list(color, &[0, 1, 2, 3]);
        f.list(0xd8, &[0, 1, 2, 3]);
        f.list(0x60, &[0, 1, 2, 3]);
        f
    }
    fn ptr(&self) -> usize {
        self.pass.as_ptr() as usize
    }
    fn list(&mut self, off: usize, indices: &[usize]) {
        let slot = OFF.iter().position(|o| *o == off).unwrap();
        for (j, i) in indices.iter().enumerate() {
            self.storage[slot][j] = self.objects[*i].as_ptr() as u32;
        }
        self.pass[(off + 4) / 4] = indices.len() as u32;
    }
    fn members(&self, off: usize) -> Vec<usize> {
        unsafe {
            let a = Array::load(self.ptr() + off).unwrap();
            (0..a.len)
                .map(|i| {
                    self.objects
                        .iter()
                        .position(|o| o.as_ptr() as usize == a.get(i))
                        .unwrap()
                })
                .collect()
        }
    }
    fn apply(&self) -> Changes {
        unsafe { reroute(self.ptr(), no_grow) }
    }
}

struct BrightFixture {
    f: Fixture,
    root: Vec<u32>,
    material: Vec<u32>,
    bank: Vec<u32>,
}
impl BrightFixture {
    fn new(mode: u32) -> Self {
        let mut f = Fixture::new(mode);
        let root = vec![0x2c06404; 0x230 / 4];
        f.pass[0] = root.as_ptr() as u32;
        f.selected[1] = root.as_ptr() as u32;
        let mut bank = vec![0xdeadbeef; 0x1000 / 4];
        for (o, v) in [
            (0xc10, [1.0f32; 4]),
            (0xc50, [1.0; 4]),
            (0xc70, [0.0; 4]),
            (0xc80, [0.0; 4]),
            (0xc60, [0.0, 1.0, 0.0, 1.0]),
            (0xca0, [1.0; 4]),
            (0xcb0, [0.0; 4]),
        ] {
            for i in 0..4 {
                bank[o / 4 + i] = v[i].to_bits();
            }
        }
        Self {
            f,
            root,
            material: vec![0x2c06404; 0x230 / 4],
            bank,
        }
    }
    unsafe fn apply(&mut self, object: usize) -> Option<([f32; 3], [f32; 3])> {
        let bank = self.bank.as_mut_ptr() as usize;
        let scope = brightness_scope(self.f.ptr(), bank)?;
        apply_brightness(
            scope,
            self.root.as_ptr() as usize,
            self.f.render.as_ptr() as usize,
            self.f.objects[object].as_ptr() as usize,
            self.material.as_ptr() as usize,
            bank,
        )
    }
    fn upload(&mut self, rgb: [f32; 3]) {
        for (i, v) in rgb.iter().enumerate() {
            self.bank[0xc30 / 4 + i] = v.to_bits();
        }
    }
    fn rgb(&self) -> [f32; 3] {
        std::array::from_fn(|i| f32::from_bits(self.bank[0xc30 / 4 + i]))
    }
}

thread_local! {
 static GROW_MODE: Cell<u8> = const { Cell::new(0) };
 static GROW_PASS: Cell<usize> = const { Cell::new(0) };
 static GROW_NEW: Cell<usize> = const { Cell::new(0) };
 static GROW_VIEW: Cell<usize> = const { Cell::new(0) };
 static GROW_CALLS: Cell<usize> = const { Cell::new(0) };
 static GROW_AFTER: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
 static GROW_BUFFERS: std::cell::RefCell<Vec<(usize,usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}
unsafe fn queue_fingerprint()->Vec<u32>{
 let p=GROW_PASS.with(Cell::get);let mut v=std::slice::from_raw_parts(p as *const u32,0x400/4).to_vec();
 GROW_BUFFERS.with(|r|{for &(p,n) in r.borrow().iter(){v.extend_from_slice(std::slice::from_raw_parts(p as *const u32,n));}});v
}
unsafe extern "thiscall" fn controlled_grow(header:usize){
 GROW_CALLS.with(|n|n.set(n.get()+1));let p=GROW_PASS.with(Cell::get);let mode=GROW_MODE.with(Cell::get);let fresh=GROW_NEW.with(Cell::get);
 let old=raw(header,0)as usize;let len=raw(header,4)as usize;
 std::ptr::copy_nonoverlapping(old as *const u32,fresh as *mut u32,len);
 put(header,0,fresh as u32);put(header,8,64);
 match mode{
  1=>put(GROW_VIEW.with(Cell::get),0x340,0),
  2=>{put(header,0,raw(p,0x24));put(header,8,16);},
  3=>{put(p,0x24,fresh as u32);put(p,0x2c,64);},
  4=>{let _=reroute(p,no_grow);},
  5=>{put(header,4,0);},
  _=>{}
 }
 GROW_AFTER.with(|a|*a.borrow_mut()=queue_fingerprint());
}
pub fn run_queue_fixture()->i32{
 let mut failed=0;let mut observations=0;let mut check=|name:&str,yes:bool|{println!("{name}={yes}");observations+=1;if !yes{failed+=1;}};
 unsafe{
  for mode in [8,9]{
   let mut f=Fixture::new(mode);let c=if mode==9{0xcc}else{0x6c};f.list(0x24,&[0,2]);
   let x=f.apply();check(&format!("normal_{mode}_retains_colour"),x.primary==2&&f.members(c)==vec![0,1,2,3]);
  }
  for case in 0..4{
   let mut f=Fixture::new(9);f.list(0x24,&[0,1,2]);
   match case{
    0=>{f.pass[0xcc/4]=f.pass[0x24/4];f.pass[0xd0/4]=3;},
    1=>{f.pass[0xd8/4]=f.pass[0xcc/4];},
    2=>{f.pass[0xcc/4]=f.pass[0x24/4]+4;f.pass[0xd0/4]=2;f.pass[0xd4/4]=31;},
    _=>{f.pass[0xcc/4]=f.ptr()as u32+0x24;f.pass[0xd0/4]=0;f.pass[0xd4/4]=3;}
   }
   let before=(f.pass.clone(),f.storage.clone());let x=f.apply();
   check(&format!("alias_{case}_no_mutation"),x==Changes::default()&&(f.pass.clone(),f.storage.clone())==before);
  }
  for mode in 0..6u8{
   let mut f=Fixture::new(9);f.list(0x24,&[0,2]);f.list(0xcc,&[1]);f.pass[0xd4/4]=1;
   let mut fresh=vec![0u32;64];GROW_NEW.with(|c|c.set(fresh.as_mut_ptr()as usize));GROW_MODE.with(|c|c.set(mode));GROW_PASS.with(|c|c.set(f.ptr()));GROW_VIEW.with(|c|c.set(f.view.as_ptr()as usize));GROW_CALLS.with(|c|c.set(0));
   GROW_BUFFERS.with(|r|*r.borrow_mut()=f.storage.iter().map(|v|(v.as_ptr()as usize,v.len())).chain(std::iter::once((fresh.as_ptr()as usize,fresh.len()))).collect());
   let x=reroute(f.ptr(),controlled_grow);
   if mode==0{check("normal_grow_relocation_preserves_routing",GROW_CALLS.with(Cell::get)==1&&x.primary==2&&f.members(0xcc)==vec![1,0,2]);}
   else{check(&format!("grow_{mode}_no_stale_writes"),GROW_CALLS.with(Cell::get)==1&&GROW_AFTER.with(|a|*a.borrow()==queue_fingerprint()));}
  }
  for fault in 0..3{
   let mut b=BrightFixture::new(9);b.upload([8.0,4.0,0.0]);let bank=b.bank.as_ptr()as usize;let scope=brightness_scope(b.f.ptr(),bank).unwrap();
   let mut second=b.f.pass.clone();let other_shader=vec![0x2c0965cu32,b.root.as_ptr()as u32];
   match fault{
    0=>b.f.render[0x236c/4]=second.as_mut_ptr()as u32,
    1=>b.f.pass[0]=b.material.as_ptr()as u32,
    _=>b.f.mode_table[9]=other_shader.as_ptr()as u32,
   }
   let before=b.bank.clone();let out=apply_brightness(scope,b.root.as_ptr()as usize,b.f.render.as_ptr()as usize,b.f.objects[0].as_ptr()as usize,b.material.as_ptr()as usize,bank);
   check(&format!("stale_brightness_{fault}_no_write"),out.is_none()&&b.bank==before);
  }
  let b=BrightFixture::new(9);let scope=brightness_scope(b.f.ptr(),b.bank.as_ptr()as usize).unwrap();BRIGHTNESS_SCOPE.with(|s|s.set(scope));READY.store(true,Ordering::Release);BRIGHTNESS_READY.store(true,Ordering::Release);deinit();
  check("stop_clears_scope_and_both_gates",BRIGHTNESS_SCOPE.with(|s|s.get().bank)==0&&!READY.load(Ordering::Acquire)&&!BRIGHTNESS_READY.load(Ordering::Acquire));
  // This original-call endpoint is exercised by the external full-module
  // fixture with a controlled detour leaf. Cargo has no initialized engine trampoline.
  #[cfg(not(test))]
  {
  BRIGHTNESS_SCOPE.with(|s|s.set(scope));classify(b.f.ptr());check("disabled_classify_clears_stale_scope",BRIGHTNESS_SCOPE.with(|s|s.get().bank)==0);
  }
 }
 println!("observations={observations} failed={failed}");if failed==0{0}else{1}
}

#[test]
fn queue_owner_and_brightness_scope_boundaries() {
    assert_eq!(run_queue_fixture(), 0);
}
}

fn large_queue_fixture(mode: u32, n: usize, duplicate_primary: bool, colour_count: usize) -> Fixture {
    let mut f = Fixture::new(mode);
    f.objects = (0..n).map(|_| vec![0; 0x220 / 4]).collect();
    for object in &mut f.objects {
        object[0x38 / 4] = 0x20000000;
        object[0x210 / 4] = 120 << 24;
    }
    for (slot, off) in OFF.iter().enumerate() {
        f.storage[slot].resize(n * 2, 0);
        f.pass[*off / 4] = f.storage[slot].as_ptr() as u32;
        f.pass[(*off + 8) / 4] = (n * 2) as u32;
        f.pass[(*off + 4) / 4] = 0;
    }
    let colour = if mode == 9 { 0xcc } else { 0x6c };
    let mask = if mode == 9 { 0xd8 } else { 0x60 };
    f.list(0x24, &(0..n).map(|i| if duplicate_primary { i % (n / 2) } else { i }).collect::<Vec<_>>());
    f.list(colour, &(0..colour_count).collect::<Vec<_>>());
    f.list(mask, &(0..n).collect::<Vec<_>>());
    f
}

#[test]
fn large_queues_keep_native_colour_order_and_no_duplicate_registration() {
    for (mode, n) in [(8, 512), (9, 512), (9, 2048)] {
        let f = large_queue_fixture(mode, n, false, n);
        let colour = if mode == 9 { 0xcc } else { 0x6c };
        let mask = if mode == 9 { 0xd8 } else { 0x60 };
        assert_eq!(f.apply(), Changes { primary: n as u32, highlight: 0, mask: n as u32 });
        assert_eq!(f.members(0x24), Vec::<usize>::new());
        assert_eq!(f.members(colour), (0..n).collect::<Vec<_>>());
        assert_eq!(f.members(mask), Vec::<usize>::new());
    }
    let f = large_queue_fixture(9, 512, true, 0);
    assert_eq!(f.apply(), Changes { primary: 512, highlight: 256, mask: 256 });
    assert_eq!(f.members(0xcc), (0..256).collect::<Vec<_>>());
    assert_eq!(f.members(0xd8), (256..512).collect::<Vec<_>>());
}

#[test]
fn large_tight_preflight_exact_fit_preserves_order_without_grow() {
    for mode in [8, 9] {
        let n = 128;
        let mut f = large_queue_fixture(mode, n, false, n / 2);
        let colour = if mode == 9 { 0xcc } else { 0x6c };
        let mask = if mode == 9 { 0xd8 } else { 0x60 };
        f.pass[(colour + 8) / 4] = n as u32;
        assert_eq!(f.apply(), Changes { primary: 128, highlight: 64, mask: 128 });
        assert!(f.members(0x24).is_empty());
        assert_eq!(f.members(colour), (0..n).collect::<Vec<_>>());
        assert!(f.members(mask).is_empty());
        assert_eq!(f.apply(), Changes::default());
        assert_eq!(f.members(colour), (0..n).collect::<Vec<_>>());

        let mut full = large_queue_fixture(mode, n, false, n);
        full.pass[(colour + 8) / 4] = n as u32;
        assert_eq!(full.apply(), Changes { primary: 128, highlight: 0, mask: 128 });
        assert_eq!(full.members(colour), (0..n).collect::<Vec<_>>());
    }
}

#[test]
fn large_tight_preflight_duplicate_and_mixed_native_props() {
    for mode in [8, 9] {
        let colour = if mode == 9 { 0xcc } else { 0x6c };
        let mask = if mode == 9 { 0xd8 } else { 0x60 };
        let mut duplicate = large_queue_fixture(mode, 512, true, 0);
        duplicate.pass[(colour + 8) / 4] = 256;
        assert_eq!(duplicate.apply(), Changes { primary: 512, highlight: 256, mask: 256 });
        assert_eq!(duplicate.members(colour), (0..256).collect::<Vec<_>>());
        assert_eq!(duplicate.members(mask), (256..512).collect::<Vec<_>>());

        let n = 256;
        let mut mixed = large_queue_fixture(mode, n, false, 128);
        mixed.pass[(colour + 8) / 4] = n as u32;
        for (i, object) in mixed.objects.iter_mut().enumerate() {
            match i % 3 {
                1 => object[0x38 / 4] = 0x10000000,
                2 => { object[0x38 / 4] = 0; object[0x210 / 4] = 0; }
                _ => {}
            }
        }
        assert_eq!(mixed.apply(), Changes { primary: 86, highlight: 43, mask: 86 });
        let unchanged = (0..n).filter(|i| i % 3 != 0).collect::<Vec<_>>();
        let colours = (0..128).chain((128..n).filter(|i| i % 3 == 0)).collect::<Vec<_>>();
        assert_eq!(mixed.members(0x24), unchanged);
        assert_eq!(mixed.members(mask), unchanged);
        assert_eq!(mixed.members(colour), colours);
    }
}

mod eligibility_block_tests {
    use super::*;
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(p: *mut std::ffi::c_void, n: usize, t: u32, protect: u32) -> *mut std::ffi::c_void;
        fn VirtualFree(p: *mut std::ffi::c_void, n: usize, t: u32) -> i32;
        fn VirtualProtect(p: *mut std::ffi::c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    }
    struct Pages(usize);
    impl Pages {
        unsafe fn new() -> Self {
            let p = VirtualAlloc(std::ptr::null_mut(), 0x3000, 0x3000, 4) as usize;
            assert_ne!(p, 0);
            Self(p)
        }
        unsafe fn protect(&self, offset: usize, protection: u32) {
            let mut old = 0;
            assert_ne!(VirtualProtect((self.0 + offset) as _, 0x1000, protection, &mut old), 0);
        }
        unsafe fn object(&self, offset: usize) -> usize {
            let p = self.0 + offset;
            put(p, 0x34, 0);
            put(p, 0x38, 0x20000000);
            put(p, 0x210, 120 << 24);
            p
        }
    }
    impl Drop for Pages {
        fn drop(&mut self) { unsafe { assert_ne!(VirtualFree(self.0 as _, 0, 0x8000), 0); } }
    }
    #[test]
    pub(super) fn cached_fields_preserve_guard_and_region_boundaries() {
        unsafe {
            let pages = Pages::new();
            // Aligned, same block, cross block, and fields crossing a region boundary.
            let objects = [pages.object(0), pages.object(0x240), pages.object(0xef0), pages.object(0x2240)];
            for protection in [4, 2, 1, 0x104] {
                pages.protect(0x1000, protection);
                for mode in [8, 9] {
                    let mut policy = Policy::new(mode);
                    for p in objects.into_iter().chain([0, 0xff00, usize::MAX - 16]) {
                        let expected = p.checked_add(0x34).is_some_and(|a| readable(a, 0x1e0))
                            && scene_highlight(mode, raw(p, 0x34), raw(p, 0x38), (raw(p, 0x210) >> 24) as u8);
                        assert_eq!(policy.eligible(p), expected, "mode={mode} protection={protection} pointer={p:x}");
                    }
                }
            }
            pages.protect(0x1000, 4);
            assert!(Policy::new(9).eligible(objects[2]));
            pages.protect(0x1000, 1);
            assert!(!Policy::new(9).eligible(objects[2]));
        }
    }
    thread_local! {
        static GROW_OBJECT: Cell<usize> = const { Cell::new(0) };
        static DENY_MEMORY: Cell<bool> = const { Cell::new(false) };
    }
    unsafe extern "thiscall" fn change_object_during_grow(header: usize) {
        let p = GROW_OBJECT.with(Cell::get);
        if DENY_MEMORY.with(Cell::get) {
            let mut old = 0;
            assert_ne!(VirtualProtect(p as _, 0x1000, 1, &mut old), 0);
        } else {
            put(p, 0x34, 0x10);
        }
        // Owned fixture has 32 allocated slots; expose that already-owned capacity.
        put(header, 8, 32);
    }
    fn check_grow(deny: bool) {
        unsafe {
            let pages = Pages::new();
            let object = pages.object(0);
            for mode in [8, 9] {
                pages.protect(0, 4);
                pages.object(0);
                let mut f = Fixture::new(mode);
                let colour = if mode == 9 { 0xcc } else { 0x6c };
                let mask = if mode == 9 { 0xd8 } else { 0x60 };
                f.list(0x24, &[0]); f.list(mask, &[0]); f.list(colour, &[1]);
                put(raw(f.ptr(), 0x24) as usize, 0, object as u32);
                put(raw(f.ptr(), mask) as usize, 0, object as u32);
                put(f.ptr(), colour + 8, 1);
                GROW_OBJECT.with(|p| p.set(object)); DENY_MEMORY.with(|p| p.set(deny));
                let changes = reroute(f.ptr(), change_object_during_grow);
                assert_eq!(changes, Changes::default(), "native Grow changed object; stale eligibility must not remove it");
                assert_eq!(raw(f.ptr(), 0x28), 1);
                assert_eq!(raw(f.ptr(), mask + 4), 1);
                assert_eq!(f.members(colour), vec![1]);
            }
        }
    }
    #[test]
    pub(super) fn native_grow_invalidates_cached_object_flags() { check_grow(false); }
    #[test]
    pub(super) fn native_grow_invalidates_cached_readability() { check_grow(true); }
}

// C11: native 16E2D40/16E2A10 ROOT-PROPS-01 bank bytes, not a shader model.
const C11_CHAR_BANK: &str = "a1a0a03e0096163e6092c93c65cf8b3700000000000000000000000000000000000000000000000000000000000000000000803f000000000000000000000040c9c8483e000000000000000000000000c9c8483fc9c8483fc9c8483f00000000000000400000803f00000000000000000000004100002041000020410000803f0000803e9a99993e0000803e0000803f";
const C11_SURV_BANK: &str = "c9c8c83e334f993e587a9d3d65cf8b37000000000000000000000000000000003a0d6f3e00000000fefd7d3f000000000000803f000000000000000000000040c9c8483ec9c8483ec9c8483e000000000000803f0000803f0000803f00000000000000400000803f0000000000000000000000400000a040000040410000803f9a99193f0000003fcdcc4c3f0000803f";
fn c11_bank(hex: &str) -> Vec<u32> {
    (0..hex.len()).step_by(8).map(|i| {
        let b: Vec<u8> = (i..i+8).step_by(2).map(|j| u8::from_str_radix(&hex[j..j+2],16).unwrap()).collect();
        u32::from_le_bytes(b.try_into().unwrap())
    }).collect()
}
struct C11Merc { b: BrightFixture, owner: Vec<u32> }
impl C11Merc {
    fn new() -> Self {
        let mut b=BrightFixture::new(8); let mut owner=vec![0u32;0x310/4];
        b.f.render[0x233c/4]=4; b.f.objects[3][0x210/4]=0xfb00ff00;
        b.material[0x14/4]=0xa7bb48a1; b.material[0x18c/4]=0x00fbfb01;
        owner[0]=0x02ad2abc; owner[0x20/4]=591547; owner[0x144/4]=0x11110000; owner[0x148/4]=0x22220000;
        owner[0x30c/4]=b.f.objects[3].as_ptr() as u32;
        b.f.objects[3][0x20/4]=owner.as_ptr() as u32;
        b.bank[0xc30/4..0xcc0/4].copy_from_slice(&c11_bank(C11_CHAR_BANK));
        Self{b,owner}
    }
    unsafe fn apply(&mut self, inactive: bool) -> Option<([f32;3],[f32;3])> {
        MERC_VIEW.with(|v| v.set(MercView {active:true,..MercView::empty()}));
        let bank=self.b.bank.as_mut_ptr() as usize;
        let mut scope=brightness_scope(self.b.f.ptr(),bank)?;
        if inactive { scope.bank=0; }
        apply_merc_floor(scope,self.b.root.as_ptr() as usize,self.b.f.render.as_ptr() as usize,
            self.b.f.objects[3].as_ptr() as usize,self.b.material.as_ptr() as usize,bank)
    }
}
#[test]
fn c11_merc_floor_only_twelve_bank_bytes_native_material_and_base_unchanged() {
    unsafe {
        let mut f=C11Merc::new();let before=f.b.bank.clone();let props=f.b.material.clone();
        let proxy=f.b.f.objects.clone();let owner=f.owner.clone();
        let out=f.apply(false).expect("ordinary Merc floor must run");
        assert_eq!(out.0,[0.0;3]);
        for i in 0..3 { assert!((out.1[i]-(200.0f32/255.0/16.0)).abs()<1e-7); }
        for i in 0..before.len() { if !(0xc50/4..0xc5c/4).contains(&i) {assert_eq!(f.b.bank[i],before[i],"byte {}",i*4);} }
        assert_eq!(f.b.material,props);assert_eq!(f.b.f.objects,proxy);assert_eq!(f.owner,owner);
        // Next original upload replaces the copied block, including this floor.
        f.b.bank[0xc30/4..0xcc0/4].copy_from_slice(&c11_bank(C11_SURV_BANK));
        f.b.f.objects[3][0x210/4]=0xfd00ff00;f.b.material[0x14/4]=0x522ee89e;f.b.material[0x18c/4]=0x00fdfd01;
        let next=f.b.bank.clone();assert!(f.apply(false).is_none());assert_eq!(f.b.bank,next);
        println!("C11_MERC floor_changed=12 native_base=true shared_props_unchanged=true next_surv_upload_restores=true");
    }
}
#[test]
fn c11_merc_floor_negative_scope_identity_and_material_controls() {
    unsafe { for case in 0..16 {
        let mut f=C11Merc::new();
        match case {
            0=>{}, // explicitly inactive scope
            1=>{f.b.f.view[0x340/4]=9;},
            2=>{f.b.f.render[0x233c/4]=2;},
            3=>{f.b.f.objects[3][0x210/4]=0xfd00ff00;},
            4=>{f.b.material[0x14/4]=0x522ee89e;},
            5=>{f.b.f.objects[3][0x34/4]=0;f.b.f.objects[3][0x38/4]=0;},
            6=>{f.owner[0]=0x02ad0000;}, // non-Merc (camera/drone/foreign actor)
            7=>{f.owner[0x30c/4]=0;},
            8=>{f.owner[0x144/4]=0;},
            9=>{f.owner[0x148/4]=0;},
            10=>{f.b.material[0x18c/4]=0x00fbfb00;},
            11=>{f.b.bank[0xc80/4]=f32::NAN.to_bits();},
            12=>{f.b.f.objects[3][0x20/4]=1;},
            13=>{f.b.f.selected[0]=0x02c0965c;},
            14=>{f.b.f.objects[3][0x210/4]=0xfb000000;},
            15=>{f.b.f.objects[3][0x210/4]=0xfb008000;},
            _=>unreachable!(),
        }
        let before=f.b.bank.clone(); assert!(f.apply(case==0).is_none(),"case {case}");assert_eq!(f.b.bank,before,"case {case}");
        println!("C11_MERC control={case} writes=0");
    }}
}
#[test]
fn c11_merc_floor_never_darkens_existing_ambient_and_caps_invalid_hdr_light() {
    unsafe {
        let mut f=C11Merc::new(); for i in 0..3 {f.b.bank[0xc50/4+i]=(0.2f32).to_bits();}
        let before=f.b.bank.clone();assert!(f.apply(false).is_none());assert_eq!(f.b.bank,before);
        let mut f=C11Merc::new();for i in 0..3 {f.b.bank[0xc80/4+i]=(100.0f32).to_bits();}
        assert_eq!(f.apply(false).unwrap().1,[0.05;3]);
    }
}

#[test]
fn c11_merc_view_nested_return_invalidates_outer_positive_span_cache() {
    unsafe {
        MERC_VIEW.with(|v| v.set(MercView::empty()));
        with_merc_view(|| {
            MERC_VIEW.with(|v| {let mut s=v.get();s.owners[0]=0x12340000;s.count=1;v.set(s);});
            with_merc_view(|| {MERC_VIEW.with(|v|{assert!(v.get().active);assert_eq!(v.get().count,0);});});
            MERC_VIEW.with(|v|{assert!(v.get().active);assert_eq!(v.get().count,0);assert_eq!(v.get().owners,[0;16]);});
        });
        MERC_VIEW.with(|v|{assert!(!v.get().active);assert_eq!(v.get().count,0);});
    }
}
#[test]
fn c11_merc_view_checks_sixteen_owners_then_fails_closed_and_reuses_success() {
    unsafe {
        let owned:Vec<Vec<u32>>=(0..17).map(|_|vec![0u32;0x310/4]).collect();
        MERC_VIEW.with(|v| v.set(MercView::empty()));
        with_merc_view(|| {
            for item in &owned[..16] {assert!(merc_owner_readable(item.as_ptr() as usize));}
            assert!(merc_owner_readable(owned[0].as_ptr() as usize));
            assert!(!merc_owner_readable(owned[16].as_ptr() as usize));
            MERC_VIEW.with(|v|assert_eq!(v.get().count,16));
        });
    }
}
#[test]
fn c11_merc_view_unreadable_failures_are_also_bounded() {
    unsafe {
        let owned=vec![0u32;0x310/4];MERC_VIEW.with(|v|v.set(MercView::empty()));
        with_merc_view(|| {
            for i in 0..32 {assert!(!merc_owner_readable(i % 2));}
            MERC_VIEW.with(|v|assert_eq!(v.get().count,16));
            assert!(!merc_owner_readable(owned.as_ptr() as usize));
        });
    }
}
