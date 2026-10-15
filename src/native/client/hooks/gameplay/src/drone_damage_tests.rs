use super::*;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Mutex,
};
static LOCK: Mutex<()> = Mutex::new(());
static EFFECTS: AtomicU32 = AtomicU32::new(0);
static SOUNDS: AtomicU32 = AtomicU32::new(0);
static FALLS: AtomicU32 = AtomicU32::new(0);
static REMOVALS: AtomicU32 = AtomicU32::new(0);
static SCORES: AtomicU32 = AtomicU32::new(0);
unsafe fn put(p: usize, x: &[u8]) {
    std::ptr::copy_nonoverlapping(x.as_ptr(), p as *mut u8, x.len());
}
unsafe fn jump(p: usize, to: usize) {
    let mut x = vec![0xe9];
    x.extend((to as u32).wrapping_sub(p as u32 + 5).to_le_bytes());
    put(p, &x);
}
unsafe extern "thiscall" fn audio(_: usize, id: u32, _: u32, _: f32, _: u32, _: u32) {
    if id == 99 {
        SOUNDS.fetch_add(1, Ordering::SeqCst);
    }
}
unsafe extern "thiscall" fn fall(p: usize) {
    *((p + 0x910) as *mut u8) = 3;
    FALLS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn remove(p: usize) {
    *((p + 0x910) as *mut u8) = 0;
    REMOVALS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn score(_: usize, _: usize, _: usize) {
    SCORES.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn spawn(
    _: usize,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
    _: u32,
) -> usize {
    EFFECTS.fetch_add(1, Ordering::SeqCst);
    0
}
fn raw(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var("SCBL_R58_C13_FIXTURE").unwrap())
            .join(format!("{name}.bin")),
    )
    .unwrap()
}
#[test]
#[ignore = "exact retail damage windows"]
fn c13_native_bullet_damage_preserves_hp_score_and_nonlethal_fx_but_terminal_falls() {
    let _l = LOCK.lock().unwrap();
    unsafe {
        let base = VirtualAlloc(std::ptr::null_mut(), 0x3000000, 0x3000, 0x40) as usize;
        assert_ne!(base, 0);
        let a = |v: usize| base + v - 0x400000;

        for (va, x) in [
            (0x8c86f0, &[0xb8, 1, 0, 0, 0, 0xc3][..]),
            (0x8dcba0, &[0x31, 0xc0, 0xc3][..]),
            (0xb07790, &[0xc2, 8, 0][..]),
            (0x8b5e30, &[0x31, 0xc0, 0xc3][..]),
            (0x8b5e60, &[0x31, 0xc0, 0xc3][..]),
            (0xdf8a10, &[0xc2, 8, 0][..]),
            (0xe23fa0, &[0xc2, 8, 0][..]),
            (0x8ba690, &[0xc2, 4, 0][..]),
            (0xaf5f60, &[0xc3][..]),
            (0xe8ca30, &[0x31, 0xc0, 0xc3][..]),
            (0x8f1340, &[0xc2, 8, 0][..]),
            (0xe96d70, &[0xc3][..]),
        ] {
            put(a(va), x);
        }
        jump(a(0xe96eb0), remove as *const () as usize);
        jump(a(0xf135f0), score as *const () as usize);
        *(a(0x2989888) as *mut f64) = 1.;
        *(a(0x2995478) as *mut f64) = 100.;
        *(a(0x29a66b8) as *mut f64) = 360.;
        *(a(0x2998548) as *mut f64) = 65536.;
        let vt = a(DRONE as usize);
        put(base + 0x130, &[0xc2, 20, 0]);
        *((vt + 0x35c) as *mut u32) = (base + 0x130) as u32;
        put(base + 0x100, &[0xb8, 1, 0, 0, 0, 0xc2, 8, 0]);
        put(base + 0x110, &[0xc2, 4, 0]);
        put(base + 0x120, &[0xc2, 8, 0]);
        *((vt + 0x698) as *mut u32) = (base + 0x100) as u32;
        *((vt + 0x314) as *mut u32) = (base + 0x110) as u32;
        *((vt + 0x2cc) as *mut u32) = (base + 0x120) as u32;
        *((vt + 0x368) as *mut u32) = audio as *const () as u32;
        // Native generic death callback reaches the actor's actual virtual5EC.
        let mut death = raw("death_event");
        for o in [149usize] {
            let v = u32::from_le_bytes(death[o..o + 4].try_into().unwrap());
            death[o..o + 4].copy_from_slice(&(a(v as usize) as u32).to_le_bytes());
        }
        put(a(0xdf8370), &death);
        put(a(0x8b5b90), &[0xc2, 20, 0]);
        *((vt + 0x4f0) as *mut u32) = a(0xdf8370) as u32;
        let mut wvt = vec![0u32; 0xa0 / 4];
        wvt[0x9c / 4] = spawn as *const () as u32;
        let world = [wvt.as_ptr() as u32];
        let mut config = vec![0u32; 0x700 / 4];
        let t = [1u32];
        let p = [0u32; 3];
        let mut body = vec![0u32; 0xc40 / 4];
        let mut fx = [0u32; 9];
        for stage in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            for (name, spec, off) in [
                ("mp_damage", &MP, 0usize),
                ("parent_damage", &PARENT, 0x200),
                ("actor_damage", &ACTOR, 0x400),
                ("drone_death", &DEAD, 0xc00),
            ] {
                let at = base + 0x1000 + off;
                let bytes = raw(name);
                let modified = stage == "MODIFIED";
                let mut code = match (name, modified) {
                    ("mp_damage", true) => mp_window(&bytes, at, base + 0x1200).unwrap(),
                    ("parent_damage", true) => {
                        parent_window(&bytes, at, base + 0x1400, a(0xf135f0)).unwrap()
                    }
                    ("actor_damage", true) => actor_window(&bytes, at).unwrap(),
                    ("drone_death", true) => {
                        dead_window(&bytes, at, fall as *const () as usize).unwrap()
                    }
                    _ => copied(&bytes, spec, at).unwrap(),
                };
                for &(o, d) in spec.calls {
                    if modified
                        && (name == "mp_damage" && o == 0x130
                            || name == "parent_damage" && (o == 0x74 || o == 0x90)
                            || name == "drone_death" && (o < 0x82 || o >= 0x116))
                    {
                        continue;
                    }
                    relative(&mut code, o, at, a(d));
                }
                if !modified && name == "mp_damage" {
                    relative(&mut code, 0x130, at, base + 0x1200);
                }
                if !modified && name == "parent_damage" {
                    relative(&mut code, 0x74, at, base + 0x1400);
                }
                for &o in relocs(name) {
                    if modified && name == "drone_death" && o >= 0x116 {
                        continue;
                    }
                    let v = u32::from_le_bytes(code[o..o + 4].try_into().unwrap());
                    code[o..o + 4].copy_from_slice(&(a(v as usize) as u32).to_le_bytes());
                }
                put(at, &code);
            }
            *((vt + 0x5ec) as *mut u32) = (base + 0x1c00) as u32;
            let f: Damage = std::mem::transmute(base + 0x1000);
            for d in [100u32, 600, 900] {
                body.fill(0);
                fx.fill(0);
                fx[1] = 0f32.to_bits();
                fx[2] = 11;
                body[0] = vt as u32;
                body[0x4c / 4] = 3 << 8;
                body[0x910 / 4] = 2;
                body[0x310 / 4] = 4;
                body[0x5dc / 4] = 600;
                body[0x5e0 / 4] = 600;
                body[0x5c4 / 4] = fx.as_ptr() as u32;
                body[0x5c8 / 4] = 1;
                body[0x144 / 4] = config.as_mut_ptr() as u32;
                body[0x148 / 4] = world.as_ptr() as u32;
                body[0x8c0 / 4] = 99;
                for c in [&EFFECTS, &SOUNDS, &FALLS, &REMOVALS, &SCORES] {
                    c.store(0, Ordering::SeqCst);
                }
                let result = f(
                    body.as_mut_ptr() as usize,
                    d,
                    t.as_ptr() as usize,
                    p.as_ptr() as usize,
                    p.as_ptr() as usize,
                    0,
                    0,
                );
                let counts = [&EFFECTS, &SOUNDS, &FALLS, &REMOVALS, &SCORES]
                    .map(|c| c.load(Ordering::SeqCst));
                let terminal = d >= 600;
                assert_eq!(result, d);
                assert_eq!(body[0x5dc / 4], 600u32.saturating_sub(d));
                assert_eq!(
                    counts,
                    if !terminal {
                        [1, 0, 0, 0, 0]
                    } else if stage == "MODIFIED" {
                        [0, 0, 1, 0, 1]
                    } else {
                        [1, 1, 0, 1, 1]
                    }
                );
                println!(
                    "C13_DAMAGE {stage} input={d} hp={} fx/sound/fall/remove/score={counts:?}",
                    body[0x5dc / 4]
                );
            }
        }
        VirtualFree(base as _, 0, 0x8000);
    }
}
#[test]
#[ignore = "exact retail damage windows"]
fn windows_reject_bad_signature_and_only_change_declared_ranges() {
    for (name, spec) in [
        ("mp_damage", &MP),
        ("parent_damage", &PARENT),
        ("actor_damage", &ACTOR),
        ("drone_death", &DEAD),
    ] {
        let mut b = raw(name);
        assert!(copied(&b, spec, 0x40000000).is_some());
        b[10] ^= 1;
        assert!(copied(&b, spec, 0x40000000).is_none());
    }
}

fn relocs(name: &str) -> &'static [usize] {
    match name {
        "mp_damage" => &[71, 390],
        "parent_damage" => &[],
        "actor_damage" => &[410, 422, 674, 701, 827, 871, 900, 1435, 1589],
        "drone_death" => &[264, 322],
        _ => panic!("fixture name"),
    }
}

#[test]
#[ignore = "retail gadget-death secondary explosion path"]
fn native_gadget_death_secondary_explosion_and_delayed_detonation_are_removed() {
    let _l = LOCK.lock().unwrap();
    unsafe {
        let base = VirtualAlloc(std::ptr::null_mut(), 0x3000000, 0x3000, 0x40) as usize;
        assert_ne!(base, 0);
        let a = |v: usize| base + v - 0x400000;
        put(a(0x8b5e30), &[0xb8, 1, 0, 0, 0, 0xc3]);
        put(a(0x8b5e60), &[0x31, 0xc0, 0xc3]);
        put(a(0xc94fc0), &[0x31, 0xc0, 0xc3]);
        put(a(0xe66590), &[0x31, 0xc0, 0xc2, 8, 0]);
        let mut vt = vec![0u32; 0x800 / 4];
        vt[0x368 / 4] = audio as *const () as u32;
        put(base + 0x200, &[0x31, 0xc0, 0xc3]);
        vt[0x758 / 4] = (base + 0x200) as u32;
        let mut wvt = vec![0u32; 0xa0 / 4];
        wvt[0x9c / 4] = spawn as *const () as u32;
        let world = [wvt.as_ptr() as u32];
        let mut physics = vec![0u32; 0x80 / 4];
        physics[0x34 / 4] = 12;
        let mut owner = vec![0u32; 0xa00 / 4];
        owner[0x4c / 4] = 2 << 8;
        let mut config = vec![0u32; 0x700 / 4];
        let mut mode = vec![0u32; 0x400 / 4];
        mode[0x37c / 4] = 1;
        config[0x6d0 / 4] = mode.as_ptr() as u32;
        let mut body = vec![0u32; 0xc00 / 4];
        body[0] = vt.as_ptr() as u32;
        body[0x9fc / 4] = owner.as_ptr() as u32;
        body[0x144 / 4] = config.as_ptr() as u32;
        body[0x148 / 4] = world.as_ptr() as u32;
        body[0x610 / 4] = physics.as_ptr() as u32;
        body[0x8a0 / 4] = 99;
        let raw = raw("gadget_death");
        let at = base + 0x1000;
        for phase in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            body[0x7c4 / 4] = 0;
            EFFECTS.store(0, Ordering::SeqCst);
            SOUNDS.store(0, Ordering::SeqCst);
            let mut code = if phase == "MODIFIED" {
                gadget_window(&raw, at).unwrap()
            } else {
                copied(&raw, &GADGET, at).unwrap()
            };
            for &(o, d) in GADGET.calls {
                if phase == "MODIFIED" && (0x2cd..0x55c).contains(&o) {
                    continue;
                }
                relative(&mut code, o, at, a(d));
            }
            for &o in GADGET_RELOCS {
                if phase == "MODIFIED" && (0x2cd..0x55c).contains(&o) {
                    continue;
                }
                let old = u32::from_le_bytes(code[o..o + 4].try_into().unwrap());
                code[o..o + 4].copy_from_slice(&(a(old as usize) as u32).to_le_bytes());
            }
            put(at, &code);
            let f: unsafe extern "thiscall" fn(usize, usize, u32) = std::mem::transmute(at);
            f(body.as_mut_ptr() as usize, 0, 600);
            let counts = [
                EFFECTS.load(Ordering::SeqCst),
                SOUNDS.load(Ordering::SeqCst),
                u32::from(body[0x7c4 / 4] & 0x800000 != 0),
            ];
            assert_eq!(counts, if phase == "MODIFIED" { [0; 3] } else { [1; 3] });
            println!("C13_GADGET {phase} fx/audio/deferred_explosion={counts:?}");
        }
        VirtualFree(base as _, 0, 0x8000);
    }
}

const GADGET_RELOCS: &[usize] = &[
    27, 273, 335, 349, 557, 579, 585, 606, 693, 741, 746, 751, 781, 801, 862, 1021, 1038, 1280,
];
