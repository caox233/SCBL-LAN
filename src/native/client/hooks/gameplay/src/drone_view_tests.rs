use super::*;
use std::cell::Cell;
static LOCK: Mutex<()> = Mutex::new(());
#[test]
fn live_gear_clear_retains_frozen_drone_view_and_reentry_discards_it() {
    let _lock = LOCK.lock().unwrap();
    let mut s = Objects::new();
    unsafe {
        s.save();
        freeze(s.body());
        assert!(VIEWS.lock().unwrap()[&s.body()].frozen);
        for _ in 0..8 {
            sight_with(s.body(), s.target(), |_, _| Some(true));
        }
        clear_tracking(s.body());
        assert!(VIEWS.lock().unwrap()[&s.body()].samples.is_empty());
        assert!(VIEWS.lock().unwrap()[&s.body()].frozen);
        for i in 0..8 {
            assert_eq!(
                sight_with(s.body(), s.target(), |_, _| Some(true)),
                (i >= 3) as u8
            );
        }
        // Local merc visibility is zero, but own drone camera can still see the spy.
        assert_eq!(s.target[0xbc0 / 4], 0);
        forget(s.body());
        assert_eq!(
            sight_with(s.body(), s.target(), |_, _| panic!("stale camera")),
            0
        );
        s.save();
        s.body[0x5dc / 4] = 0;
        clear_tracking(s.body());
        assert!(!VIEWS.lock().unwrap().contains_key(&s.body()));
    }
}
#[test]
fn frozen_or_transient_camera_never_erases_last_same_identity_view() {
    let _lock = LOCK.lock().unwrap();
    let mut s = Objects::new();
    s.body[0x910 / 4] = 2;
    READY.store(true, Ordering::Release);
    unsafe {
        s.save();
        // Controller camera is temporarily null (as during transition/rebuild).
        observe(s.body());
        assert!(VIEWS.lock().unwrap().contains_key(&s.body()));
        freeze(s.body());
        observe(s.body());
        assert!(VIEWS.lock().unwrap()[&s.body()].frozen);
        // Same address with a new network identity must never inherit the camera.
        s.node[0x70 / 4] += 1;
        clear_tracking(s.body());
        assert!(!VIEWS.lock().unwrap().contains_key(&s.body()));
    }
    READY.store(false, Ordering::Release);
}
#[test]
fn ten_metre_cap_uses_drone_origin_in_three_dimensions() {
    let mut s = Objects::new();
    for (p, expected) in [
        ([999., 0., 0.], true),
        ([1000., 0., 0.], true),
        ([1000.01, 0., 0.], false),
        ([0., 0., 1000.01], false),
        ([600., 800., 0.], true),
        ([601., 800., 0.], false),
    ] {
        for (i, v) in p.iter().enumerate() {
            s.target[(0x9c / 4) + i] = f32::to_bits(*v);
        }
        unsafe {
            assert_eq!(within_mark_range(s.body(), s.target(), mark_limit_cm(2).unwrap()), expected);
        }
    }
    s.owner[0x9c / 4] = 1000f32.to_bits();
    s.target[0x9c / 4] = 1001f32.to_bits();
    s.target[0xa0 / 4] = 0;
    s.target[0xa4 / 4] = 0;
    unsafe {
        assert!(!within_mark_range(s.body(), s.target(), mark_limit_cm(2).unwrap()));
    }
}
#[test]
fn controlled_and_parked_mark_ranges_do_not_bleed_on_reentry() {
    let mut s = Objects::new();
    for (state, metres, expected) in [
        (2, 9.99, true),
        (2, 10.0, true),
        (2, 10.01, false),
        (1, 9.99, true),
        (1, 10.0, true),
        (1, 10.01, true),
        (1, 29.99, true),
        (1, 30.0, true),
        (1, 30.01, false),
        (2, 10.01, false), // re-entering control restores the original cap
        (2, 30.0, false),
    ] {
        s.body[0x910 / 4] = state;
        s.target[0x9c / 4] = (metres as f32 * 100.0).to_bits();
        s.target[0xa0 / 4] = 0;
        s.target[0xa4 / 4] = 0;
        let limit = mark_limit_cm(state).unwrap();
        assert_eq!(unsafe { within_mark_range(s.body(), s.target(), limit) }, expected,
            "state={state} metres={metres}");
    }
    assert_eq!(mark_limit_cm(0), None);
    assert_eq!(mark_limit_cm(3), None);
    assert_eq!(mark_limit_cm(4), None);
}
fn forward() -> View {
    let mut projection = [0.; 16];
    projection[3] = 1.;
    projection[4] = 1.;
    projection[9] = 1.;
    let mut screen = [0.; 16];
    screen[0] = 50.;
    screen[5] = -50.;
    screen[12] = 50.;
    screen[13] = 50.;
    View {
        projection,
        screen,
        viewport: [100., 100.],
        origin: [0.; 3],
        body: [0.; 3],
    }
}
#[test]
fn parked_frustum_fixes_view_not_merc_position_or_rotation() {
    let v = forward();
    assert!(v.contains([100., 0., 0.], [0.; 3]));
    assert!(!v.contains([-100., 0., 0.], [0.; 3]));
    assert!(!v.contains([100., 100., 0.], [0.; 3]));
    assert!(!v.contains([100., 0., 90.], [0.; 3]));
    for _merc_yaw in [0, 90, 180, 270] {
        assert!(v.contains([100., 0., 0.], [0.; 3]));
    }
    assert!(v.contains([110., 0., 0.], [10., 0., 0.]));
    assert_eq!(v.origin_at([10., 20., 30.]), [10., 20., 30.]);
    assert!(!v.contains([f32::NAN, 0., 0.], [0.; 3]));
}
struct Objects {
    body: Vec<u32>,
    owner: Vec<u32>,
    controller: Vec<u32>,
    ability: Vec<u32>,
    armor: Vec<u32>,
    node: Vec<u32>,
    target: Vec<u32>,
    target_node: Vec<u32>,
}
impl Objects {
    fn new() -> Self {
        let mut s = Self {
            body: vec![0; 0xc00 / 4],
            owner: vec![0; 0x1500 / 4],
            controller: vec![0; 0x800 / 4],
            ability: vec![0; 0xc0 / 4],
            armor: vec![0; 0x100 / 4],
            node: vec![0; 0x80 / 4],
            target: vec![0; 0x2000 / 4],
            target_node: vec![0; 0x80 / 4],
        };
        s.body[0] = 0x02a88f54;
        s.body[0x910 / 4] = 1;
        s.body[0x5dc / 4] = 600;
        s.body[0x9fc / 4] = s.owner.as_ptr() as u32;
        s.body[0x8c / 4] = s.owner.as_ptr() as u32;
        s.body[0x2c0 / 4] = s.node.as_ptr() as u32;
        s.node[0x70 / 4] = 7;
        s.node[0x74 / 4] = 9;
        s.owner[0] = 0x02ad2abc;
        s.owner[0x938 / 4] = s.controller.as_ptr() as u32;
        s.owner[0xcf4 / 4] = s.armor.as_ptr() as u32;
        s.controller[0x4c / 4] = 1 << 8;
        s.armor[0] = 0x02ad42ac;
        s.armor[0x5c / 4] = s.ability.as_ptr() as u32;
        s.ability[0] = 0x02a3f14c;
        s.ability[0xa0 / 4] = s.body.as_ptr() as u32;
        s.ability[0x6c / 4] = s.owner.as_ptr() as u32;
        s.ability[0x40 / 4] = 8;
        s.ability[0x7c / 4] = 0.8f32.to_bits();
        s.target[0] = 0x02ad3234;
        s.target[0x9c / 4] = 100f32.to_bits();
        s.target[0xbc4 / 4] = 0.5f32.to_bits();
        s.target[0x2c0 / 4] = s.target_node.as_ptr() as u32;
        s.target_node[0x70 / 4] = 11;
        s
    }
    fn body(&self) -> usize {
        self.body.as_ptr() as usize
    }
    fn target(&self) -> usize {
        self.target.as_ptr() as usize
    }
    unsafe fn save(&self) {
        VIEWS.lock().unwrap().insert(
            self.body(),
            Saved {
                id: identity(self.body()).unwrap(),
                view: forward(),
                samples: BTreeMap::new(),
                frozen: false,
            },
        );
    }
}
#[test]
fn visibility_uses_drone_bones_threshold_and_never_mutates_target_player_visibility() {
    let _lock = LOCK.lock().unwrap();
    let s = Objects::new();
    unsafe {
        s.save();
    }
    let before = s.target.clone();
    for i in 0..8 {
        let r = unsafe {
            sight_with(s.body(), s.target(), |bone, origin| {
                assert_eq!(bone, BONES[i]);
                assert_eq!(origin, [0.; 3]);
                Some(true)
            })
        };
        assert_eq!(r, (i >= 3) as u8);
    }
    assert_eq!(s.target, before);
    for i in 0..8 {
        let r = unsafe { sight_with(s.body(), s.target(), |_, _| Some(false)) };
        assert_eq!(r, (i < 4) as u8);
    }
    assert_eq!(s.target, before);
    forget(s.body());
}
#[test]
fn missing_view_occlusion_and_reused_source_fail_closed_not_merc_fallback() {
    let _lock = LOCK.lock().unwrap();
    let mut s = Objects::new();
    let unused = |_, _| -> Option<bool> { panic!("must not query unrelated camera") };
    unsafe {
        assert_eq!(sight_with(s.body(), s.target(), unused), 0);
        s.save();
    }
    s.target[0x9c / 4] = (-100f32).to_bits();
    unsafe {
        assert_eq!(sight_with(s.body(), s.target(), unused), 0);
    }
    s.target[0x9c / 4] = 100f32.to_bits();
    for _ in 0..8 {
        unsafe {
            assert_eq!(sight_with(s.body(), s.target(), |_, _| None), 0);
        }
    }
    s.node[0x70 / 4] += 1;
    unsafe {
        assert_eq!(sight_with(s.body(), s.target(), unused), 0);
    }
    forget(s.body());
}
#[test]
fn dead_emp_empty_and_other_owners_cannot_keep_parked_scan() {
    let _lock = LOCK.lock().unwrap();
    let mut s = Objects::new();
    unsafe {
        s.save();
        assert!(identity(s.body()).is_some());
    }
    for (field, old, bad) in [
        (0x910, 1, 3),
        (0x5dc, 600, 0),
        (0x9fc, s.owner.as_ptr() as u32, 0),
        (0x4b0, 0, 0x80000000),
    ] {
        s.body[field / 4] = bad;
        unsafe {
            assert!(identity(s.body()).is_none());
            assert_eq!(sight_with(s.body(), s.target(), |_, _| panic!()), 0);
        }
        s.body[field / 4] = old;
    }
    s.ability[0x7c / 4] = 0;
    unsafe {
        assert!(identity(s.body()).is_none());
    }
    s.ability[0x7c / 4] = 0.8f32.to_bits();
    s.controller[0x4c / 4] = 2 << 8;
    unsafe {
        assert!(identity(s.body()).is_none());
    }
    forget(s.body());
}
unsafe fn alloc(code: &[u8]) -> usize {
    let p = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 0x40) as usize;
    assert_ne!(p, 0);
    std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len());
    p
}
fn raw(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var("SCBL_R58_C13_FIXTURE").unwrap())
            .join(format!("{name}.bin")),
    )
    .unwrap()
}
fn rel(code: &mut [u8], at: usize, o: usize, to: usize) {
    assert_eq!(code[o], 0xe8);
    code[o + 1..o + 5]
        .copy_from_slice(&(to as u32).wrapping_sub((at + o + 5) as u32).to_le_bytes());
}
fn absolute(code: &mut [u8], from: u32, to: u32) {
    let pat = from.to_le_bytes();
    let positions = (0..code.len() - 3)
        .filter(|&i| code[i..i + 4] == pat)
        .collect::<Vec<_>>();
    assert!(!positions.is_empty());
    for i in positions {
        code[i..i + 4].copy_from_slice(&to.to_le_bytes());
    }
}
thread_local! {static MERC_SEES:Cell<bool>=const{Cell::new(false)};static DRONE_SEES:Cell<bool>=const{Cell::new(true)};static VIEWPORT:Cell<usize>=const{Cell::new(0)};static ENGINE:Cell<usize>=const{Cell::new(0)};static IMMUNE:Cell<bool>=const{Cell::new(false)};}
unsafe extern "thiscall" fn no_team(_: usize, _: usize) -> u32 {
    0
}
unsafe extern "thiscall" fn alive(_: usize) -> u32 {
    0
}
unsafe extern "thiscall" fn eligible(_: usize) -> u32 {
    1
}
unsafe extern "thiscall" fn immune(_: usize) -> u8 {
    (!IMMUNE.with(|x| x.get())) as u8
}
unsafe extern "C" fn engine() -> usize {
    ENGINE.with(|x| x.get())
}
unsafe extern "thiscall" fn controller(_: usize) -> usize {
    1
}
unsafe extern "thiscall" fn camera(_: usize) -> usize {
    1
}
unsafe extern "thiscall" fn viewport(_: usize) -> usize {
    VIEWPORT.with(|x| x.get())
}
unsafe extern "thiscall" fn project(_: usize, out: *mut [f32; 3], _: usize) -> usize {
    *out = [
        if MERC_SEES.with(|x| x.get()) {
            50.
        } else {
            500.
        },
        50.,
        1.,
    ];
    out as usize
}
unsafe extern "thiscall" fn bridge(_: usize, t: usize) -> u8 {
    let p = floats(t, 0x9c).unwrap();
    (DRONE_SEES.with(|x| x.get()) && forward().contains(p, [0.; 3])) as u8
}
#[test]
#[ignore = "exact retail native predicate fixture"]
fn native_predicate_baseline_modified_rollback_camera_truth_table() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let original = raw("filter");
        assert_eq!(format!("{:x}", Sha256::digest(&original)), FILTER_SHA);
        let mut body = vec![0u32; 0xc00 / 4];
        let mut target = vec![0u32; 0xc00 / 4];
        let mut vt = vec![0u32; 0x400 / 4];
        vt[0x330 / 4] = alive as *const () as u32;
        body[0] = vt.as_ptr() as u32;
        target[0xbc0 / 4] = 1f32.to_bits();
        target[0xbc4 / 4] = 0.5f32.to_bits();
        target[0x9c / 4] = 100f32.to_bits();
        let mut engvt = vec![0u32; 0x200 / 4];
        engvt[0x138 / 4] = controller as *const () as u32;
        let eng = [engvt.as_ptr() as u32];
        ENGINE.with(|x| x.set(eng.as_ptr() as usize));
        let mut vp = vec![0u32; 0x500 / 4];
        vp[0x4a0 / 4] = 100;
        vp[0x4a4 / 4] = 100;
        VIEWPORT.with(|x| x.set(vp.as_ptr() as usize));
        let constants = [9000000f64, 10., 8., 100.];
        let cd = 5f32;
        for stage in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let at = alloc(&[]);
            let mut code = if stage == "MODIFIED" {
                window(&original, at, bridge as *const () as usize).unwrap()
            } else {
                original.clone()
            };
            for (o, f) in [
                (0x24, no_team as *const () as usize),
                (0x3f, eligible as *const () as usize),
                (0x1fc, immune as *const () as usize),
                (0x21d, alive as *const () as usize),
                (0x228, alive as *const () as usize),
                (0x22f, alive as *const () as usize),
            ] {
                if !(stage == "MODIFIED" && o == 0x3f) {
                    rel(&mut code, at, o, f);
                }
            }
            // BD0 > 0 prevents unrelated tutorial notification; no gameplay RPC is mocked.
            body[0xbd0 / 4] = 1f32.to_bits();
            if stage != "MODIFIED" {
                for (o, f) in [
                    (0xf5, engine as *const () as usize),
                    (0x112, camera as *const () as usize),
                    (0x126, camera as *const () as usize),
                    (0x12d, project as *const () as usize),
                    (0x134, camera as *const () as usize),
                    (0x13b, viewport as *const () as usize),
                    (0x152, camera as *const () as usize),
                    (0x159, viewport as *const () as usize),
                ] {
                    rel(&mut code, at, o, f);
                }
                for (va, i) in [(0x2997cc0, 1), (0x29a22c8, 2), (0x2995478, 3)] {
                    absolute(&mut code, va, (&constants[i]) as *const f64 as u32);
                }
            }
            absolute(&mut code, 0x2a1bda8, constants.as_ptr() as u32);
            absolute(&mut code, 0x29a2300, &cd as *const f32 as u32);
            std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len());
            FlushInstructionCache(GetCurrentProcess(), at as _, code.len());
            let f: unsafe extern "thiscall" fn(usize, usize) -> u8 = std::mem::transmute(at);
            for merc in [false, true] {
                for drone in [false, true] {
                    MERC_SEES.with(|s| s.set(merc));
                    DRONE_SEES.with(|s| s.set(drone));
                    let r = f(body.as_ptr() as usize, target.as_ptr() as usize);
                    let expected = if stage == "MODIFIED" { drone } else { merc };
                    assert_eq!(r, expected as u8);
                    println!("C13_CAMERA {stage} merc={merc} drone={drone} acquired={r}");
                }
            }
            MERC_SEES.with(|s| s.set(true));
            DRONE_SEES.with(|s| s.set(true));
            IMMUNE.with(|s| s.set(true));
            assert_eq!(f(body.as_ptr() as usize, target.as_ptr() as usize), 0);
            IMMUNE.with(|s| s.set(false));
            target[0x9c / 4] = 3100f32.to_bits();
            assert_eq!(f(body.as_ptr() as usize, target.as_ptr() as usize), 0);
            target[0x9c / 4] = 100f32.to_bits();
            VirtualFree(at as _, 0, 0x8000);
        }
    }
}
#[test]
#[ignore = "exact retail native predicate fixture"]
fn clone_refuses_unknown_bytes_and_changes_only_three_view_gates_and_call_relocation() {
    let raw = raw("filter");
    let out = window(&raw, 0x10000000, 0x11000000).unwrap();
    for i in 0..raw.len() {
        let allowed = (0x3d..0x64).contains(&i)
            || (0xf5..0x1f9).contains(&i)
            || [0x24, 0x1fc, 0x21d, 0x228, 0x22f, 0x241]
                .iter()
                .any(|o| (*o + 1..*o + 5).contains(&i));
        if !allowed {
            assert_eq!(raw[i], out[i], "{i:x}");
        }
    }
    let mut bad = raw.clone();
    bad[0x3f] ^= 1;
    assert!(window(&bad, 0x10000000, 0x11000000).is_none());
}

#[test]
#[ignore = "native projection numeric ABI"]
fn copied_projection_matrix_matches_native_563770_without_camera_calls() {
    unsafe {
        let mut code = raw("project");
        assert_eq!(code.len(), 498);
        let b = alloc(&[]);
        let constants = [
            (0x2d75ba0u32, 0f64.to_le_bytes().to_vec()),
            (0x2989888, 1f64.to_le_bytes().to_vec()),
            (
                0x29898b0,
                [0x80000000u32; 4]
                    .iter()
                    .flat_map(|x| x.to_le_bytes())
                    .collect(),
            ),
            (0x299ad20, 0x322bcc77u32.to_le_bytes().to_vec()),
            (0x299ad90, 0xb22bcc77u32.to_le_bytes().to_vec()),
        ];
        for (i, (addr, value)) in constants.into_iter().enumerate() {
            let p = b + 0x500 + i * 32;
            std::ptr::copy_nonoverlapping(value.as_ptr(), p as *mut u8, value.len());
            absolute(&mut code, addr, p as u32);
        }
        std::ptr::copy_nonoverlapping(code.as_ptr(), b as *mut u8, code.len());
        let f: unsafe extern "thiscall" fn(usize, *mut [f32; 4], f32, f32, f32) -> usize =
            std::mem::transmute(b);
        let mut mat = vec![0f32; 0x200 / 4];
        let mut m = [0f32; 16];
        for (i, x) in m.iter_mut().enumerate() {
            *x = (i as f32 * 0.173) - 0.2;
        }
        m[15] = 100.;
        mat[0x160 / 4..0x1a0 / 4].copy_from_slice(&m);
        for i in 0..100 {
            let p = [i as f32 * 0.23, 1.2 - i as f32 * 0.1, i as f32 * 0.07];
            let mut out = [0.; 4];
            f(mat.as_ptr() as usize, &mut out, p[0], p[1], p[2]);
            let w = transform(&m, p, 3);
            assert_eq!(out[3].to_bits(), w.to_bits());
            let inv = (1f64 / w as f64) as f32;
            for k in 0..3 {
                let expected = (transform(&m, p, k) as f64 * inv as f64) as f32;
                assert_eq!(
                    out[k].to_bits(),
                    expected.to_bits(),
                    "projection component {k}"
                );
            }
        }
        VirtualFree(b as _, 0, 0x8000);
        println!("C13_VIEW projection=native_numeric_equivalent cases=100 no_camera_mutation=true");
    }
}

unsafe extern "thiscall" fn render_mesh(target: usize) -> usize {
    *((target + 0x1f0) as *const u32) as usize
}
#[test]
#[ignore = "exact retail render-recency gate and drone predicate"]
fn c16_parked_render_recency_regression_preserves_hidden_and_native_immunity() {
    let _lock = LOCK.lock().unwrap();
    let dir = std::path::PathBuf::from(std::env::var("SCBL_R58_C16_FIXTURE").unwrap());
    let gate = std::fs::read(dir.join("render_gate.bin")).unwrap();
    let recent = std::fs::read(dir.join("recent_frame.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&gate)),
        "e9ffd7b9f69455d6e787d4908884f3505e09d4ef6924854ea542580b13f448cb"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&recent)),
        "c2d3efa5099454671547223535d962118438334ba0951a2b63acbb60c90d187c"
    );
    unsafe {
        let storage = alloc(&[]);
        let counter = storage + 0x500;
        (counter as *mut u64).write_unaligned(100_000);
        let mut rec = recent.clone();
        absolute(&mut rec, 0x3107ec0, counter as u32);
        absolute(&mut rec, 0x3107ec4, (counter + 4) as u32);
        std::ptr::copy_nonoverlapping(rec.as_ptr(), storage as *mut u8, rec.len());
        let native_gate = storage + 0x100;
        let mut g = gate.clone();
        assert_eq!(g[0x16], 0xe9);
        g[0x17..0x1b].copy_from_slice(
            &(storage as u32)
                .wrapping_sub((native_gate + 0x1b) as u32)
                .to_le_bytes(),
        );
        std::ptr::copy_nonoverlapping(g.as_ptr(), native_gate as *mut u8, g.len());
        FlushInstructionCache(GetCurrentProcess(), storage as _, 4096);
        let predicate: unsafe extern "thiscall" fn(usize) -> u32 = std::mem::transmute(native_gate);

        let mut mesh = vec![0u32; 0xa0 / 4];
        let mut target = vec![0u32; 0xc00 / 4];
        let mut target_vt = vec![0u32; 0x200 / 4];
        target_vt[0x154 / 4] = render_mesh as *const () as u32;
        target[0] = target_vt.as_ptr() as u32;
        target[0x1f0 / 4] = mesh.as_mut_ptr() as u32;
        target[0xbc0 / 4] = 0; // absent from the merc's current screen
        target[0xbc4 / 4] = 0.1f32.to_bits();
        target[0x9c / 4] = 100f32.to_bits();
        for age in [0u64, 1, 2, 3, 37565] {
            let last = 100_000 - age;
            mesh[0x8c / 4] = last as u32;
            mesh[0x90 / 4] = (last >> 32) as u32;
            assert_eq!(predicate(target.as_ptr() as usize), (age <= 1) as u32);
        }
        let mut body = vec![0u32; 0xc00 / 4];
        let mut vt = vec![0u32; 0x400 / 4];
        vt[0x330 / 4] = alive as *const () as u32;
        body[0] = vt.as_ptr() as u32;
        body[0xbd0 / 4] = 1f32.to_bits();
        let raw = raw("filter");
        let distance = 9_000_000f64;
        let cooldown = 5f32;
        DRONE_SEES.with(|v| v.set(true));
        IMMUNE.with(|v| v.set(false));
        for stage in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let at = alloc(&[]);
            let mut code = window(&raw, at, bridge as *const () as usize).unwrap();
            if stage != "MODIFIED" {
                // Exact C15 clone still called 8BC900 before the drone-view bridge.
                code[0x3d..0x48].copy_from_slice(&raw[0x3d..0x48]);
                rel(&mut code, at, 0x3f, native_gate);
            } else if code[0x3f] == 0xe8 {
                // Red test against the unmodified C15 production function.
                rel(&mut code, at, 0x3f, native_gate);
            }
            for (o, f) in [
                (0x24, no_team as *const () as usize),
                (0x1fc, immune as *const () as usize),
                (0x21d, alive as *const () as usize),
                (0x228, alive as *const () as usize),
                (0x22f, alive as *const () as usize),
            ] {
                rel(&mut code, at, o, f);
            }
            absolute(&mut code, 0x2a1bda8, &distance as *const f64 as u32);
            absolute(&mut code, 0x29a2300, &cooldown as *const f32 as u32);
            std::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len());
            FlushInstructionCache(GetCurrentProcess(), at as _, code.len());
            let f: unsafe extern "thiscall" fn(usize, usize) -> u8 = std::mem::transmute(at);
            for age in [0u64, 1, 2, 3, 37565] {
                mesh[0x8c / 4] = (100_000 - age) as u32;
                target[0x5c / 4] = 0;
                let before = mesh.clone();
                let result = f(body.as_ptr() as usize, target.as_ptr() as usize);
                println!("C16_RECENCY {stage} age={age} drone_sees=true marked_candidate={result}");
                assert_eq!(result, (stage == "MODIFIED" || age <= 1) as u8);
                assert_eq!(mesh, before, "must not fake a render timestamp");
                target[0x5c / 4] = 1;
                assert_eq!(
                    f(body.as_ptr() as usize, target.as_ptr() as usize),
                    0,
                    "native hidden flag"
                );
                target[0x5c / 4] = 0;
            }
            IMMUNE.with(|v| v.set(true));
            assert_eq!(f(body.as_ptr() as usize, target.as_ptr() as usize), 0);
            IMMUNE.with(|v| v.set(false));
            DRONE_SEES.with(|v| v.set(false));
            assert_eq!(f(body.as_ptr() as usize, target.as_ptr() as usize), 0);
            DRONE_SEES.with(|v| v.set(true));
            VirtualFree(at as _, 0, 0x8000);
        }
        VirtualFree(storage as _, 0, 0x8000);
    }
}

#[test]
fn c16_live_frozen_projection_keeps_parked_target_inside_after_merc_turns_away() {
    let view = View {
        projection: [
            -0.678785502910614,
            0.10404238849878311,
            0.34746554493904114,
            0.3474586009979248,
            0.25914865732192993,
            0.2725171744823456,
            0.9101130962371826,
            0.9100948572158813,
            0.0,
            1.258315920829773,
            -0.22583569586277008,
            -0.22583116590976715,
            -5586.94287109375,
            -604.3671264648438,
            -826.2571411132812,
            -824.2406005859375,
        ],
        screen: [
            1103.9998779296875,
            0.0,
            -0.0,
            0.0,
            0.0,
            -620.9999389648438,
            0.0,
            -0.0,
            -0.0,
            0.0,
            0.9999999403953552,
            0.0,
            1103.9998779296875,
            620.9999389648438,
            -0.0,
            0.9999999403953552,
        ],
        viewport: [2208.0, 1242.0],
        origin: [-6859.6318359375, 3591.46875, 269.66412353515625],
        body: [-6812.53125, 3716.6171875, 224.44651794433594],
    };
    assert!(view.contains(
        [-6670.98388671875, 4076.0, 65.5],
        [-6828.09814453125, 3704.37548828125, 212.41653442382812]
    ));
}
