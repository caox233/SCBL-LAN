use super::*;
fn tick(t: &mut Tracker, time: f32) {
    t.begin(1, 2, 3, 7, time * SPEED);
}
#[test]
fn c69_curve_clear_then_fade() {
    for (age, a) in [
        (0.0, 1.0),
        (1.49, 1.0),
        (1.5, 1.0),
        (2.25, 0.5),
        (2.99, 1.0 / 150.0),
        (3.0, 0.0),
        (-1.0, 0.0),
        (f64::NAN, 0.0),
    ] {
        assert!((visibility(age) - a).abs() < 0.0001);
    }
}
#[test]
fn c69_near_and_far_hold_independently_across_wrap() {
    let mut t = Tracker::default();
    tick(&mut t, 0.0);
    assert_eq!(t.encounter(11, 21, 500.0, 0.0, 5000.0), Some(1));
    assert_eq!(t.encounter(12, 22, 4800.0, 0.0, 5000.0), Some(1));
    tick(&mut t, 0.2);
    assert_eq!(t.encounter(11, 21, 500.0, 0.0, 5000.0), Some(0));
    tick(&mut t, 1.6);
    assert_eq!(t.encounter(12, 22, 4800.0, 0.0, 5000.0), Some(0));
    tick(&mut t, 2.9);
    t.encounter(11, 21, 500.0, 0.0, 5000.0);
    t.encounter(12, 22, 4800.0, 0.0, 5000.0);
    assert!(t.alpha(1, 11, 21).unwrap() < 0.2);
    assert_eq!(t.alpha(1, 12, 22), Some(1.0));
    tick(&mut t, 0.0);
    assert_eq!(t.cycle, 1);
    assert_eq!(t.encounter(12, 22, 4800.0, 0.0, 5000.0), Some(0));
    assert_eq!(t.alpha(1, 12, 22), Some(1.0)); // new wave does not erase old distant tail
    tick(&mut t, 0.3);
    t.encounter(11, 21, 500.0, 0.0, 5000.0);
    t.encounter(12, 22, 4800.0, 0.0, 5000.0);
    assert_eq!(t.alpha(1, 11, 21), Some(1.0));
    assert!((t.alpha(1, 12, 22).unwrap() - 0.8666667).abs() < 0.001);
    tick(&mut t, 1.6);
    t.encounter(12, 22, 4800.0, 0.0, 5000.0);
    assert_eq!(t.alpha(1, 12, 22), Some(1.0));
    println!("C69 near/far: new wave retains previous far tail; refresh=1.0, not additive");
}
#[test]
fn c69_no_per_frame_refresh_or_permanent_mark() {
    let mut t = Tracker::default();
    for i in 0..290 {
        tick(&mut t, i as f32 / 100.0);
        t.encounter(1, 2, 300.0, 0.0, 5000.0);
    }
    assert!(t.alpha(1, 1, 2).unwrap() < 0.15);
    tick(&mut t, 0.0);
    tick(&mut t, 0.3);
    // A character moved ahead of the new front; the old hit has expired.
    assert_eq!(t.encounter(1, 2, 4500.0, 0.0, 5000.0), Some(1));
    assert_eq!(t.alpha(1, 1, 2), None);
}
#[test]
fn c69_native_range_and_invalid_geometry() {
    let mut t = Tracker::default();
    tick(&mut t, 2.0);
    assert_eq!(t.encounter(1, 2, 5000.0, 0.0, 5000.0), Some(1));
    assert_eq!(t.encounter(1, 2, 4999.0, 0.0, 5000.0), Some(0));
    assert_eq!(t.encounter(1, 2, 5100.0, 0.0, 5000.0), Some(1));
    assert_eq!(t.encounter(1, 2, f32::NAN, 0.0, 5000.0), None);
    assert_eq!(t.encounter(1, 2, 0.0, -1.0, 5000.0), None);
}
#[test]
fn c69_scope_reset_and_identity() {
    for mode in [0, 1, 4, 5, 8, 9] {
        let mut t = Tracker::default();
        tick(&mut t, 1.0);
        t.encounter(1, 2, 100.0, 0.0, 5000.0);
        t.begin(1, 2, 3, mode, 3000.0);
        assert_eq!(t.render, 0);
        assert!(!t.targets.iter().any(|x| x.used));
    }
    let mut t = Tracker::default();
    tick(&mut t, 1.0);
    t.encounter(1, 2, 100.0, 0.0, 5000.0);
    assert_eq!(t.alpha(1, 1, 3), None);
    assert_eq!(t.alpha(9, 1, 2), None);
    t.begin(1, 2, 9, 7, 0.0);
    assert!(!t.targets.iter().any(|x| x.used));
}
#[test]
fn c69_frame_eligibility_must_be_renewed() {
    let mut t = Tracker::default();
    tick(&mut t, 1.0);
    t.encounter(1, 2, 100.0, 0.0, 5000.0);
    tick(&mut t, 1.1);
    assert_eq!(t.alpha(1, 1, 2), None);
}
#[test]
fn c69_storage_bounded_and_no_duplicate_refresh() {
    let mut t = Tracker::default();
    tick(&mut t, 1.0);
    for n in 1..=128 {
        assert_eq!(t.encounter(n, n, 100.0, 0.0, 5000.0), Some(0));
    }
    assert_eq!(t.encounter(129, 129, 100.0, 0.0, 5000.0), None);
    for _ in 0..5 {
        t.encounter(1, 1, 2000.0, 0.0, 5000.0);
    }
    assert_eq!(t.targets[0].hit, 100.0 / 3000.0);
}
fn bank() -> Vec<u32> {
    let mut b = vec![0x12345678; 0xd00 / 4];
    for (i, v) in [0.04f32, 0.0, 0.04, -1.0, 0.000167364, -1.008368]
        .iter()
        .enumerate()
    {
        b[FADE_OFFSETS[i] / 4] = v.to_bits();
    }
    b
}
#[test]
fn c69_bank_only_six_floats_change_and_restore_exactly() {
    unsafe {
        for a in [1.0f32, 0.5, 0.0] {
            let mut b = bank();
            let before = b.clone();
            let p = b.as_mut_ptr() as usize;
            let saved = write_fades(p, a).unwrap();
            for i in 0..b.len() {
                if !FADE_OFFSETS.contains(&(i * 4)) {
                    assert_eq!(b[i], before[i]);
                }
            }
            assert_eq!(f32::from_bits(b[0xbd4 / 4]), 1.0 - a);
            restore(saved);
            assert_eq!(b, before);
        }
    }
}
#[test]
fn c69_new_native_upload_not_overwritten_by_restore() {
    unsafe {
        let mut b = bank();
        let p = b.as_mut_ptr() as usize;
        let saved = write_fades(p, 0.5).unwrap();
        b[0xb90 / 4] = 0.002666f32.to_bits();
        let newer = b.clone();
        restore(saved);
        assert_eq!(b, newer);
    }
}
#[test]
fn c69_thin_visible_wave_palette_and_invalid_input_unchanged() {
    unsafe {
        let mut b = bank();
        b[0xbd0 / 4] = 10.0f32.to_bits();
        let before = b.clone();
        assert!(write_fades(b.as_mut_ptr() as usize, 0.5).is_none());
        assert_eq!(b, before);
        assert!(write_fades(0, f32::NAN).is_none());
    }
}
#[test]
fn c69_native_wave_end_is_only_mode7() {
    unsafe {
        for m in [0, 1, 4, 5, 7, 8, 9] {
            let mut view = vec![0u32; 0x53c / 4];
            let mut state = vec![0u32; 0x3c / 4];
            view[0x538 / 4] = state.as_ptr() as u32;
            state[0x1c / 4] = m;
            state[0x34 / 4] = 0x76543201;
            state[0x38 / 4] = 9000.0f32.to_bits();
            let old = state.clone();
            assert_eq!(finish_wave(view.as_ptr() as usize), m == 7);
            if m == 7 {
                assert_eq!(state[0x34 / 4], 0x76543200);
                assert_eq!(state[0x38 / 4], 0);
            } else {
                assert_eq!(state, old);
            }
        }
    }
}
#[test]
fn c69_native_wave_not_ended_early_or_restarted_here() {
    unsafe {
        let mut v = vec![0u32; 0x53c / 4];
        let mut s = vec![0u32; 0x3c / 4];
        v[0x538 / 4] = s.as_ptr() as u32;
        s[0x1c / 4] = 7;
        for phase in [0.0f32, 8999.9, f32::NAN] {
            s[0x34 / 4] = 1;
            s[0x38 / 4] = phase.to_bits();
            let old = s.clone();
            assert!(!finish_wave(v.as_ptr() as usize));
            assert_eq!(s, old);
        }
        s[0x34 / 4] = 0;
        s[0x38 / 4] = 10000f32.to_bits();
        assert!(!finish_wave(v.as_ptr() as usize));
    }
}
#[test]
fn c69_native_gate_abi_and_branch_replay() {
    unsafe {
        use windows::Win32::System::Memory::{
            VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
        };
        let bytes = include_bytes!(concat!(
            env!("SCBL_R58_C69_FIXTURE"),
            "/native-0174fb40.bin"
        ));
        assert_eq!(bytes.len(), 0xe2);
        let p = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(p, 0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        let call: unsafe extern "thiscall" fn(usize, usize, f32, f32) -> u8 =
            std::mem::transmute(p);
        let mut pass = vec![0u32; 0x20 / 4];
        let mut render = vec![0u32; 0x2370 / 4];
        let mut ctx = vec![0u32; 0x344 / 4];
        let mut obj = vec![0u32; 0x100 / 4];
        pass[0x14 / 4] = render.as_ptr() as u32;
        render[0x20d0 / 4] = ctx.as_ptr() as u32;
        render[0x236c / 4] = pass.as_ptr() as u32;
        ctx[0x340 / 4] = 7;
        obj[0x34 / 4] = 0x400;
        obj[0xd0 / 4] = 4800.0f32.to_bits();
        obj[0x3c / 4] = 0.0f32.to_bits();
        ctx[0x2ec / 4] = 5000.0f32.to_bits();
        assert_eq!(
            call(
                pass.as_ptr() as usize,
                obj.as_ptr() as usize,
                12000.0,
                5000.0
            ),
            0
        );
        TRACK.with(|t| {
            let mut t = t.borrow_mut();
            t.begin(
                render.as_ptr() as usize,
                ctx.as_ptr() as usize,
                3,
                7,
                5000.0,
            );
        });
        assert_eq!(
            gate_override(pass.as_ptr() as usize, obj.as_ptr() as usize, 5000.0),
            Some(0)
        );
        TRACK.with(|t| {
            t.borrow_mut().begin(
                render.as_ptr() as usize,
                ctx.as_ptr() as usize,
                3,
                7,
                8990.0,
            )
        });
        ctx[0x2ec / 4] = 0.0f32.to_bits();
        TRACK.with(|t| {
            t.borrow_mut()
                .begin(render.as_ptr() as usize, ctx.as_ptr() as usize, 3, 7, 0.0)
        });
        assert_eq!(
            call(
                pass.as_ptr() as usize,
                obj.as_ptr() as usize,
                12000.0,
                5000.0
            ),
            1
        );
        assert_eq!(
            gate_override(pass.as_ptr() as usize, obj.as_ptr() as usize, 5000.0),
            Some(0)
        );
        obj[0x34 / 4] = 0;
        assert_eq!(
            gate_override(pass.as_ptr() as usize, obj.as_ptr() as usize, 5000.0),
            None
        );
        TRACK.with(|t| *t.borrow_mut() = Tracker::default());
        println!("native x86 gate: first hit=keep; next-wave native=cull; C69 previous tail=keep; props=native");
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
    super::super::fixture_sonar_limit(p + 0x79);
    (p, code.len())
}

#[test]
fn c69_executed_native_wave_repeats_at_three_seconds() {
    unsafe {
        let (p, _) = native_wave_copy();
        let update: unsafe extern "cdecl" fn(f32, usize) = std::mem::transmute(p);
        let mut v = vec![0u32; 0x53c / 4];
        let mut s = vec![0u32; 0x3c / 4];
        let mut profile = vec![0u32; 0x268 / 4];
        v[0x538 / 4] = s.as_ptr() as u32;
        s[4 / 4] = profile.as_ptr() as u32;
        s[0x1c / 4] = 7;
        profile[0x16c / 4] = 5000f32.to_bits();
        profile[0x17c / 4] = 3000f32.to_bits();
        profile[0x188 / 4] = 20000f32.to_bits();
        profile[0x1b0 / 4] = 4f32.to_bits();
        profile[0x1d8 / 4] = 5f32.to_bits();
        let original = profile.clone();
        for _ in 0..3 {
            s[0x20 / 4] = 1;
            let mut elapsed = 0.0;
            for i in 1..1000 {
                update(0.01, v.as_ptr() as usize);
                if finish_wave(v.as_ptr() as usize) {
                    elapsed = i as f32 * 0.01;
                    break;
                }
            }
            assert!((elapsed - 3.0).abs() < 0.02);
            assert_eq!(s[0x34 / 4] & 255, 0);
            println!(
                "C69 executed native wave period={elapsed:.2}s; native request restarts next wave"
            );
        }
        assert_eq!(profile, original);
    }
}
#[inline(never)]
unsafe extern "thiscall" fn cleanup_fixture(_root: usize, bank: usize) {
    assert_eq!(((bank + 0xb90) as *const f32).read_unaligned(), 0.04);
}
#[test]
fn c69_real_detours_preserve_abi_scope_cleanup_and_disable() {
    unsafe {
        use windows::Win32::System::Memory::{
            VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
        };
        let bytes = include_bytes!(concat!(
            env!("SCBL_R58_C69_FIXTURE"),
            "/native-0174fb40.bin"
        ));
        let p = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(p, 0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        let call: unsafe extern "thiscall" fn(usize, usize, f32, f32) -> u8 =
            std::mem::transmute(p);
        Gate.initialize(call, gate).unwrap();
        Gate.enable().unwrap();
        EndDraw.initialize(cleanup_fixture, end_draw).unwrap();
        EndDraw.enable().unwrap();
        let (wp, _) = native_wave_copy();
        let update: unsafe extern "cdecl" fn(f32, usize) = std::mem::transmute(wp);
        Wave.initialize(update, wave).unwrap();
        Wave.enable().unwrap();
        READY.store(true, Ordering::Release);
        let mut v = vec![0u32; 0x53c / 4];
        let mut s = vec![0u32; 0x3c / 4];
        let mut profile = vec![0u32; 0x268 / 4];
        v[0x538 / 4] = s.as_ptr() as u32;
        s[4 / 4] = profile.as_ptr() as u32;
        s[0x1c / 4] = 7;
        s[0x20 / 4] = 1;
        for (o, f) in [
            (0x16c, 5000f32),
            (0x17c, 3000.0),
            (0x188, 20000.0),
            (0x1b0, 4.0),
            (0x1d8, 5.0),
        ] {
            profile[o / 4] = f.to_bits();
        }
        for _ in 0..300 {
            update(0.01, v.as_ptr() as usize);
            render_clock(profile.as_ptr() as usize).unwrap();
        }
        assert_eq!(s[0x34 / 4] & 255, 0); // cdecl arguments/trampoline and production exit run
        let mut pass = vec![0u32; 0x20 / 4];
        let mut render = vec![0u32; 0x2370 / 4];
        let mut ctx = vec![0u32; 0x344 / 4];
        let mut obj = vec![0u32; 0x100 / 4];
        pass[0x14 / 4] = render.as_ptr() as u32;
        render[0x20d0 / 4] = ctx.as_ptr() as u32;
        render[0x236c / 4] = pass.as_ptr() as u32;
        ctx[0x340 / 4] = 7;
        obj[0x34 / 4] = 0x400;
        obj[0xd0 / 4] = 4800f32.to_bits();
        for phase in [5000f32, 8990.0, 0.0] {
            ctx[0x2ec / 4] = phase.to_bits();
            TRACK.with(|t| {
                t.borrow_mut()
                    .begin(render.as_ptr() as usize, ctx.as_ptr() as usize, 3, 7, phase)
            });
            assert_eq!(
                call(
                    pass.as_ptr() as usize,
                    obj.as_ptr() as usize,
                    12000.0,
                    5000.0
                ),
                0
            );
        }
        let mut b = bank();
        let before = b.clone();
        let bp = b.as_mut_ptr() as usize;
        SAVED.with(|s| *s.borrow_mut() = write_fades(bp, 0.4));
        cleanup_fixture(0, bp);
        assert_eq!(b, before); // thiscall + ret4 cleanup before original
        deinit();
        assert_eq!(
            call(
                pass.as_ptr() as usize,
                obj.as_ptr() as usize,
                12000.0,
                5000.0
            ),
            1
        );
        assert_eq!(
            std::slice::from_raw_parts(p as *const u8, bytes.len()),
            bytes
        );
        println!("C69 real detours: cdecl wave=3s; thiscall gate keeps old tail; cleanup restores all six floats; disable restores native gate bytes/result");
    }
}
#[test]
fn c69_restart_thunk_retains_native_checks_except_mode7_timer() {
    unsafe {
        use windows::Win32::System::Memory::{
            VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
        };
        let p = VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(p, 0);
        let mut bytes = vec![0x55, 0x8b, 0xec, 0x56, 0x8b, 0x4d, 8, 0x8b, 0x75, 12];
        let site = p + bytes.len();
        bytes.extend_from_slice(&[0x0f, 0x57, 0xc0, 0x0f, 0x2f, 0x86, 0x7c, 7, 0, 0]);
        bytes.extend_from_slice(&[0x0f, 0x92, 0xc0, 0x0f, 0xb6, 0xc0, 0x5e, 0xc9, 0xc3]);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        let enabled = Box::new(AtomicBool::new(false));
        let thunk = restart_thunk(site, &*enabled as *const AtomicBool as usize).unwrap();
        let hook = retour::RawDetour::new(site as *const (), thunk as *const ()).unwrap();
        let call: unsafe extern "cdecl" fn(usize, usize) -> u32 = std::mem::transmute(p);
        let mut state = vec![0u32; 0x40 / 4];
        let mut gadget = vec![0u32; 0x800 / 4];
        for installed in [true, false, true, false] {
            if installed {
                hook.enable().unwrap();
            } else {
                hook.disable().unwrap();
            }
            for mode in [0, 1, 4, 5, 7, 8, 9] {
                state[0x1c / 4] = mode;
                for on in [false, true] {
                    enabled.store(on, Ordering::Release);
                    for timer in [-1f32, 0.0, 2.0, 10.0, f32::NAN] {
                        gadget[0x77c / 4] = timer.to_bits();
                        let before = gadget.clone();
                        let blocked = call(state.as_ptr() as usize, gadget.as_ptr() as usize);
                        let expected = if installed && on && mode == 7 {
                            0
                        } else {
                            u32::from(timer > 0.0 || timer.is_nan())
                        };
                        assert_eq!(blocked, expected);
                        assert_eq!(gadget, before);
                    }
                }
            }
        }
        assert_eq!(
            std::slice::from_raw_parts(p as *const u8, bytes.len()),
            bytes
        );
        println!("C69 restart thunk: only enabled mode7 ignores residual timer; no timer/profile writes; other modes and disable byte-exact");
    }
}

#[test]
fn c69_game_time_keeps_fading_when_native_restart_is_delayed() {
    let mut t = Tracker::default();
    let mut c = Clock {
        view: 1,
        profile: 3,
        generation: 4,
        now: 1.6,
        start: 0.0,
        cycle: 1,
        phase: 4800.0,
        active: true,
        attached: true,
    };
    t.begin_clock(1, 2, c);
    t.encounter(11, 21, 4800.0, 0.0, 5000.0);
    c.now = 3.0;
    c.phase = 0.0;
    c.active = false;
    t.begin_clock(1, 2, c);
    t.encounter(11, 21, 4800.0, 0.0, 5000.0);
    assert_eq!(t.alpha(1, 11, 21), Some(1.0));
    c.now = 4.0;
    t.begin_clock(1, 2, c);
    t.encounter(11, 21, 4800.0, 0.0, 5000.0);
    assert!((t.alpha(1, 11, 21).unwrap() - 0.4).abs() < 0.0001);
    c.now = 4.61;
    t.begin_clock(1, 2, c);
    assert_eq!(t.encounter(11, 21, 4800.0, 0.0, 5000.0), Some(1));
    c.now = 10.0;
    c.start = 9.0;
    c.phase = 3000.0;
    c.cycle = 2;
    c.active = true;
    t.begin_clock(1, 2, c);
    assert_eq!(t.encounter(11, 21, 4800.0, 0.0, 5000.0), Some(1));
    c.now = 10.7;
    c.phase = 5100.0;
    t.begin_clock(1, 2, c);
    t.encounter(11, 21, 4800.0, 0.0, 5000.0);
    assert_eq!(t.alpha(1, 11, 21), Some(1.0));
}
#[test]
fn c69_clock_is_per_view_bound_to_private_profile_and_clears_off() {
    unsafe {
        let mut v = vec![0u32; 0x53c / 4];
        let mut s = vec![0u32; 0x3c / 4];
        v[0x538 / 4] = s.as_ptr() as u32;
        s[4 / 4] = 0x123400;
        s[0x1c / 4] = 7;
        s[0x34 / 4] = 1;
        s[0x38 / 4] = 300.0f32.to_bits();
        let vp = v.as_ptr() as usize;
        assert!(!advance_clock(vp, 0.1, true));
        assert!(render_clock(0x345600).is_none());
        assert!(render_clock(0x123400).unwrap().active);
        assert!(advance_clock(vp, 0.1, false));
        ended_clock(vp);
        assert!(!render_clock(0x123400).unwrap().active);
        s[0x1c / 4] = 0;
        advance_clock(vp, 0.1, false);
        assert!(render_clock(0x123400).is_none());
    }
}
