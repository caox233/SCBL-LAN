use super::*;
use super::super::*; // Native adapter fixtures; state stays private to action_flow.
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static FALLS: AtomicU32 = AtomicU32::new(0);
static CLEANUP: AtomicU32 = AtomicU32::new(0);
static PARALYZED: AtomicBool = AtomicBool::new(false);
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, kind: u32, protection: u32) -> *mut c_void;
    fn VirtualFree(p: *mut c_void, n: usize, kind: u32) -> i32;
    fn FlushInstructionCache(p: *mut c_void, a: *const c_void, n: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}
unsafe fn put(p: usize, b: &[u8]) {
    std::ptr::copy_nonoverlapping(b.as_ptr(), p as _, b.len());
}
unsafe fn jump(p: usize, target: usize) {
    let mut b = vec![0xe9];
    b.extend((target as u32).wrapping_sub(p as u32 + 5).to_le_bytes());
    put(p, &b);
}
unsafe extern "thiscall" fn native_fall(p: usize) {
    FALLS.fetch_add(1, Ordering::SeqCst);
    *((p + 0x910) as *mut u8) = 3;
}
unsafe extern "thiscall" fn fall_bridge(p: usize) {
    forced_fall_site(p, || native_fall(p));
}
unsafe extern "thiscall" fn event_a(_: usize, _: usize, _: usize) {
    CLEANUP.fetch_or(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn event_b(_: usize, _: usize, _: usize) {
    CLEANUP.fetch_or(2, Ordering::SeqCst);
}
unsafe extern "thiscall" fn paralyzed(_: usize, _: u32) -> u32 {
    u32::from(PARALYZED.load(Ordering::SeqCst))
}
unsafe extern "thiscall" fn weapon(_: usize, value: u32) {
    assert_eq!(value, 1);
    CLEANUP.fetch_or(4, Ordering::SeqCst);
}
unsafe extern "thiscall" fn equip(_: usize) {
    CLEANUP.fetch_or(8, Ordering::SeqCst);
}
unsafe extern "thiscall" fn movement(_: usize, value: u32) {
    assert_eq!(value, 0);
    CLEANUP.fetch_or(16, Ordering::SeqCst);
}
unsafe extern "thiscall" fn base_exit(p: usize) {
    assert_eq!(*((p + 0x138) as *const u32), 0);
    CLEANUP.fetch_or(32, Ordering::SeqCst);
}

struct Fixture {
    base: usize,
    layout: Layout,
    pawn: Vec<u32>,
    body: Vec<u32>,
    ability: Vec<u32>,
    armor: Vec<u32>,
    state: Vec<u32>,
    controller: Vec<u32>,
    movement: Vec<u32>,
    locks: Vec<u32>,
    code: Vec<u8>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            assert_ne!(VirtualFree(self.base as _, 0, 0x8000), 0);
        }
    }
}
impl Fixture {
    unsafe fn new() -> Self {
        let dir = std::path::PathBuf::from(
            std::env::var("SCBL_R58_EXIT_FIXTURE").expect("retail fixture path"),
        );
        let code = std::fs::read(dir.join("mp_exit.bin")).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&code)), EXIT_SHA);
        let base = VirtualAlloc(std::ptr::null_mut(), 0x3000000, 0x3000, 0x40) as usize;
        assert_ne!(base, 0);
        let delta = base.wrapping_sub(0x400000);
        let addr = |v: usize| v.wrapping_add(delta);
        let mut f = Self {
            base,
            layout: Layout::relocated(delta),
            pawn: vec![0; 0x1b00 / 4],
            body: vec![0; 0xc40 / 4],
            ability: vec![0; 0xc0 / 4],
            armor: vec![0; 0x100 / 4],
            state: vec![0; 0x180 / 4],
            controller: vec![0; 0x800 / 4],
            movement: vec![0; 0x100 / 4],
            locks: vec![0; 0x100 / 4],
            code,
        };
        f.pawn[0] = f.layout.owner;
        f.pawn[0x4c / 4] = 1 << 8;
        f.pawn[0x938 / 4] = f.controller.as_ptr() as u32;
        f.pawn[0x93c / 4] = f.movement.as_ptr() as u32;
        f.pawn[0x90c / 4] = f.locks.as_ptr() as u32;
        f.controller[0x4c / 4] = 2 << 8; // Isolate the camera leaf, not the state transition.
        f.body[0] = f.layout.body;
        f.body[0x9fc / 4] = f.pawn.as_ptr() as u32;
        f.body[0x910 / 4] = 2;
        f.body[0x5dc / 4] = 600;
        f.body[0x5e0 / 4] = 600;
        f.body[0x9c / 4] = 12.0f32.to_bits();
        f.body[0xa0 / 4] = 24.0f32.to_bits();
        f.armor[0] = f.layout.armor;
        f.armor[0x5c / 4] = f.ability.as_ptr() as u32;
        f.pawn[0xcf4 / 4] = f.armor.as_ptr() as u32;
        f.ability[0] = f.layout.ability;
        f.ability[0x28 / 4] = 3;
        f.ability[0x40 / 4] = 9;
        f.ability[0x6c / 4] = f.pawn.as_ptr() as u32;
        f.ability[0xa0 / 4] = f.body.as_ptr() as u32;
        f.ability[0x7c / 4] = 0.73f32.to_bits();
        f.ability[0x3c / 4] = 120f32.to_bits();
        f.ability[0x2c / 4] = 20f32.to_bits();
        f.state[0] = f.layout.state;
        f.state[0x48 / 4] = f.pawn.as_ptr() as u32;
        f.state[0x64 / 4] = f.controller.as_ptr() as u32;
        f.state[0x138 / 4] = f.body.as_ptr() as u32;
        f.locks[0x28 / 4] = 0xdeadbeff;
        *((f.layout.body as usize + 0x754) as *mut u32) = native_fall as *const () as u32;
        put(base + 0x100, &[0xc2, 0x14, 0]); // native rotation setter, thiscall five arguments
        *((f.layout.body as usize + 0x148) as *mut u32) = (base + 0x100) as u32;
        for (va, target) in [
            (0xb07c90, event_a as *const () as usize),
            (0xb07a90, event_b as *const () as usize),
            (0xd5e8e0, paralyzed as *const () as usize),
            (0xdb4e90, weapon as *const () as usize),
            (0xdb9630, equip as *const () as usize),
            (0xe5e6e0, movement as *const () as usize),
            (0x149fe60, base_exit as *const () as usize),
        ] {
            jump(addr(va), target);
        }
        put(addr(0x42d850), &[0xc2, 4, 0]);
        put(addr(0x42d7b0), &[0xc3]);
        put(addr(0x52e7f0), &[0xc2, 8, 0]);
        let active = std::fs::read(dir.join("active.bin")).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&active)),
            "9e95e8ab08c37b6fb58ce04f2548eb2c25b28ef348f40d7d4d9694dde5077daa"
        );
        put(addr(0xe69000), &active);
        let park = std::fs::read(dir.join("park.bin")).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&park)),
            "f502ea7b9ec5932cc7cda7ac1c2f1add95c6c772a527f08f507ed734500c52ce"
        );
        put(addr(0xe68b30), &park);
        // Park's relative camera call remains aligned, but is not entered by this fixture.
        put(addr(0xd61550), &[0xc2, 12, 0]);
        f
    }
    fn state(&self) -> usize {
        self.state.as_ptr() as usize
    }
    fn ability(&self) -> usize {
        self.ability.as_ptr() as usize
    }
    fn body(&self) -> usize {
        self.body.as_ptr() as usize
    }
    unsafe fn request(&self) -> Option<VoluntaryExit> {
        VoluntaryExit::request(self.state(), self.ability(), 11, 11, self.layout, |_| false)
    }
    unsafe fn park(&self) {
        let native: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(self.base + 0xe68b30 - 0x400000);
        native(self.body());
    }
    unsafe fn prepare_exit(&self, modified: bool) -> unsafe extern "thiscall" fn(usize) {
        let entry = self.base + 0x14b2390 - 0x400000;
        let mut code = if modified {
            exit_window(&self.code, entry, fall_bridge as *const () as usize).unwrap()
        } else {
            self.code.clone()
        };
        let delta = self.base.wrapping_sub(0x400000);
        // Fixture-only relocation: leaf calls go to isolated stubs in this arena.
        for (offset, target) in CALLS {
            let dst = target.wrapping_add(delta) as u32;
            code[offset + 1..offset + 5]
                .copy_from_slice(&dst.wrapping_sub((entry + offset + 5) as u32).to_le_bytes());
        }
        for va in [
            0x28387c8u32,
            0x3288bbc,
            0x339ce34,
            0x339cdec,
            0x339c0d8,
            0x339f854,
        ] {
            let mut count = 0;
            for i in 0..code.len() - 3 {
                if code[i..i + 4] == va.to_le_bytes() {
                    code[i..i + 4].copy_from_slice(&va.wrapping_add(delta as u32).to_le_bytes());
                    count += 1;
                }
            }
            assert_eq!(count, 1);
        }
        put(entry, &code);
        assert_ne!(
            FlushInstructionCache(GetCurrentProcess(), self.base as _, 0x3000000),
            0
        );
        std::mem::transmute(entry)
    }
    unsafe fn run_exit(
        &self,
        modified: bool,
        request: Option<VoluntaryExit>,
        blocked: bool,
    ) -> Option<ParkedDrone> {
        let native = self.prepare_exit(modified);
        FALLS.store(0, Ordering::SeqCst);
        CLEANUP.store(0, Ordering::SeqCst);
        PARALYZED.store(blocked, Ordering::SeqCst);
        let (_, parked) = exit_and_capture(
            request,
            self.state(),
            self.ability(),
            |_| blocked,
            || native(self.state()),
        );
        assert_eq!(
            self.state[0x138 / 4],
            0,
            "native exit must detach its own control pointer"
        );
        assert_eq!(
            CLEANUP.load(Ordering::SeqCst),
            if blocked { 59 } else { 63 },
            "native HUD/weapon/movement/base tail preserved"
        );
        assert_eq!(
            self.locks[0x28 / 4],
            0xdeadbeef,
            "native pawn lock release must run"
        );
        parked
    }
}

#[test]
#[ignore = "requires exact retail DX11 MP exit and OnStation fixtures"]
fn native_park_then_mp_exit_baseline_modified_rollback() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for (role, modified, expected) in [
            ("BASELINE", false, 3),
            ("MODIFIED", true, 1),
            ("ROLLBACK", false, 3),
        ] {
            let f = Fixture::new();
            let request = f.request().unwrap();
            let ability_before = f.ability.clone();
            f.park();
            assert_eq!(f.body[0x910 / 4], 1);
            assert_eq!(f.body[0x7c4 / 4] & 2, 2);
            f.run_exit(modified, Some(request), false);
            assert_eq!(f.body[0x910 / 4], expected);
            assert_eq!(f.ability, ability_before);
            assert_eq!(f.body[0x5dc / 4], 600);
            assert_eq!(f.body[0x9c / 4], 12f32.to_bits());
            assert_eq!(FALLS.load(Ordering::SeqCst), u32::from(!modified));
            println!("R58_EXIT {role} body_state={expected} fall_calls={} ability_body_same=true battery=0.730 hp=600 control_detached=true cleanup=63",FALLS.load(Ordering::SeqCst));
            native_fall(f.body());
            assert_eq!(
                f.body[0x910 / 4],
                3,
                "subsequent external fall must not be intercepted"
            );
        }
    }
}

#[test]
#[ignore = "requires exact retail DX11 MP exit and OnStation fixtures"]
fn native_forced_exit_and_changed_identity_never_gain_retention() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "no_request",
            "dead_owner",
            "emp_disabled",
            "reset",
            "empty_battery",
            "dead_body",
            "already_falling",
            "different_ability_body",
            "different_owner",
            "different_controller",
            "remote_role",
            "native_paralyzed",
        ] {
            let mut f = Fixture::new();
            let mut request = f.request();
            f.park();
            match case {
                "no_request" => request = None,
                "dead_owner" => f.pawn[0x60 / 4] |= 0x800,
                "emp_disabled" => f.ability[0x40 / 4] |= 4,
                "reset" => f.ability[0x40 / 4] |= 2,
                "empty_battery" => f.ability[0x7c / 4] = 0,
                "dead_body" => f.body[0x5dc / 4] = 0,
                "already_falling" => f.body[0x910 / 4] = 3,
                "different_ability_body" => f.ability[0xa0 / 4] = 0,
                "different_owner" => f.body[0x9fc / 4] = 0,
                "different_controller" => f.state[0x64 / 4] = 0,
                "remote_role" => f.pawn[0x4c / 4] = 2 << 8,
                "native_paralyzed" => (),
                _ => unreachable!(),
            }
            f.run_exit(true, request, case == "native_paralyzed");
            assert_eq!(f.body[0x910 / 4], 3);
            assert_eq!(FALLS.load(Ordering::SeqCst), 1);
            println!(
                "R58_EXIT_NEGATIVE case={case} native_fall=1 body_state=3 cleanup_preserved=true"
            );
        }
    }
}

#[test]
#[ignore = "requires exact retail DX11 MP exit and OnStation fixtures"]
fn exit_scope_is_one_shot_nested_thread_local_and_unwind_safe() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let f = Fixture::new();
        let request = f.request();
        f.park();
        exit_with_request(
            request,
            f.state(),
            f.ability(),
            |_| false,
            || {
                exit_with_request(
                    None,
                    f.state(),
                    f.ability(),
                    |_| false,
                    || {
                        assert!(
                            !forced_fall_site(f.body(), || {}),
                            "nested forced exit masks outer permission"
                        );
                    },
                );
                let body = f.body();
                assert!(!std::thread::spawn(move || forced_fall_site(body, || {}))
                    .join()
                    .unwrap());
                assert!(!forced_fall_site(body + 4, || {}));
                assert!(forced_fall_site(body, || panic!("permitted one site")));
                assert!(
                    !forced_fall_site(body, || {}),
                    "permission is consumed after one site"
                );
            },
        );
        assert!(!forced_fall_site(f.body(), || {}));
        let g = Fixture::new();
        let request = g.request();
        g.park();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exit_with_request(
                request,
                g.state(),
                g.ability(),
                |_| false,
                || panic!("fixture unwind"),
            );
        }));
        assert!(!forced_fall_site(g.body(), || {}));
    }
}

#[test]
#[ignore = "requires exact retail DX11 MP exit fixture"]
fn exit_clone_rejects_unknown_bytes_and_changes_only_one_native_call_site() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let f = Fixture::new();
        let out = exit_window(&f.code, EXIT, 0x12345678).unwrap();
        for i in 0..out.len() {
            if !(0xed..0xf3).contains(&i) {
                assert_eq!(out[i], f.code[i], "unexpected byte {i}");
            }
        }
        for i in 0..f.code.len() {
            let mut bad = f.code.clone();
            bad[i] ^= 1;
            assert!(exit_window(&bad, EXIT, 0x12345678).is_none());
        }
        assert!(exit_window(&f.code, EXIT, 0).is_none());
        for (action, detonate) in [(0, 0), (0, 11), (12, 11)] {
            assert!(VoluntaryExit::request(
                f.state(),
                f.ability(),
                action,
                detonate,
                f.layout,
                |_| false
            )
            .is_none());
        }
        assert!(
            VoluntaryExit::request(f.state(), f.ability(), 11, 11, f.layout, |_| true).is_none()
        );
    }
}

#[test]
#[ignore = "requires exact retail DX11 MP exit and OnStation fixtures"]
fn native_restriction_arriving_inside_exit_still_forces_fall() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let f = Fixture::new();
        let request = f.request();
        f.park();
        let blocked = Cell::new(false);
        exit_with_request(
            request,
            f.state(),
            f.ability(),
            |_| blocked.get(),
            || {
                blocked.set(true); // e.g. native ParalyzedState established by an exit callback
                assert!(!forced_fall_site(f.body(), || native_fall(f.body())));
            },
        );
        assert_eq!(f.body[0x910 / 4], 3);
    }
}

// R58 native existing-body reentry fixtures. No copied activation/spawn function.
static ENTRY_BODY: AtomicU32 = AtomicU32::new(0);
static ENTRY_SETUP: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn existing_body(_: usize, slot: u32) -> u32 {
    assert_eq!(slot, 0x28);
    ENTRY_BODY.load(Ordering::SeqCst)
}
unsafe extern "thiscall" fn entry_setup(state: usize) {
    assert_eq!(
        *((state + 0x138) as *const u32),
        ENTRY_BODY.load(Ordering::SeqCst)
    );
    ENTRY_SETUP.fetch_add(1, Ordering::SeqCst);
}
unsafe fn native_fixture(f: &Fixture, name: &str, va: usize, sha: &str) -> usize {
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_REENTRY_FIXTURE").unwrap());
    let code = std::fs::read(root.join(format!("{name}.bin"))).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&code)), sha);
    let p = f.base + va - 0x400000;
    put(p, &code);
    p
}
#[test]
#[ignore = "requires exact retail DX11 existing-body control fixture"]
fn native_parked_body_reentry_does_not_spawn_reset_battery_or_move_body() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        f.body[0x8c / 4] = f.pawn.as_ptr() as u32;
        f.pawn[0x5c / 4] = 0x800000;
        f.body[0x4c / 4] = 1 << 8;
        let addr = |v: usize| f.base + v - 0x400000;
        let entry = native_fixture(
            &f,
            "body_control",
            0xebd910,
            "63b18350cdd8e59b5868e919daa2c6ce11b8aa455c7cf5e097ec2d1a12e24ba1",
        );
        for va in [0xad6c20, 0xaa8f00] {
            put(addr(va), &[0xc2, 4, 0]);
        }
        put(f.base + 0x120, &[0x31, 0xc0, 0xc3]);
        *((f.layout.owner as usize + 0x4f4) as *mut u32) = (f.base + 0x120) as u32;
        put(f.base + 0x140, &[0xc2, 20, 0]);
        *((f.layout.body as usize + 0x368) as *mut u32) = (f.base + 0x140) as u32;
        put(f.base + 0x160, &[0xc2, 4, 0]);
        *((f.layout.body as usize + 0x314) as *mut u32) = (f.base + 0x160) as u32;
        // Spawn/initial positioning functions deliberately remain nonexecutable
        // zero bytes. Entering one fails the process, rather than a mock passing.
        let native: unsafe extern "thiscall" fn(usize, u32) -> u32 = std::mem::transmute(entry);
        let ability_before = f.ability.clone();
        for n in 0..3 {
            f.park();
            assert_eq!(native(f.body(), 0), 1); // native stop control keeps OnStation alive
            assert_eq!(f.body[0x910 / 4], 1);
            assert_eq!(native(f.body(), 1), 1);
            assert_eq!(f.body[0x910 / 4], 2);
            assert_eq!(f.ability, ability_before);
            assert_eq!(f.body[0x5dc / 4], 600);
            assert_eq!(f.body[0x9c / 4], 12f32.to_bits());
            assert_eq!(f.body[0xa0 / 4], 24f32.to_bits());
            assert_eq!(f.body[0x9fc / 4], f.pawn.as_ptr() as u32);
            println!("R58_REENTRY cycle={n} stop=park1 start=control2 same_body=true hp=600 battery=0.730 placement_not_called=true");
        }
    }
}
#[test]
#[ignore = "requires exact retail DX11 base-entry fixture"]
fn native_base_entry_reuses_inventory_body_and_keeps_parked_position() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for parked in [false, true] {
            let mut f = Fixture::new();
            let entry = native_fixture(
                &f,
                "base_entry",
                0x149fc60,
                "c092c0938bc30e85482e1fe941d912721d1e34659a4bf011bd5e5abc398a4995",
            );
            let delta = f.base.wrapping_sub(0x400000);
            // Only SEH metadata and stack cookie absolute addresses are reached.
            for (offset, va) in [(6usize, 0x2834948u32), (0x18, 0x3288bbc)] {
                assert_eq!(
                    std::slice::from_raw_parts((entry + offset) as *const u8, 4),
                    va.to_le_bytes()
                );
                put(entry + offset, &va.wrapping_add(delta as u32).to_le_bytes());
            }
            let addr = |v: usize| f.base + v - 0x400000;
            jump(addr(0xd5fb60), existing_body as *const () as usize);
            jump(addr(0x14f6fa0), entry_setup as *const () as usize);
            for va in [0xc94fc0, 0x8c4ed0] {
                put(addr(va), &[0x31, 0xc0, 0xc3]);
            }
            let world = vec![0u32; 0x700 / 4];
            let inventory = vec![0u32; 32];
            f.pawn[0x144 / 4] = world.as_ptr() as u32;
            f.pawn[0x1410 / 4] = inventory.as_ptr() as u32;
            f.pawn[0x94c / 4] = 1;
            f.pawn[0x9c / 4] = 999f32.to_bits();
            f.body[0x7c4 / 4] = if parked { 2 } else { 0 };
            f.body[0x910 / 4] = if parked { 1 } else { 0 };
            f.state[0x138 / 4] = 0;
            ENTRY_BODY.store(f.body() as u32, Ordering::SeqCst);
            ENTRY_SETUP.store(0, Ordering::SeqCst);
            let native: unsafe extern "thiscall" fn(usize) = std::mem::transmute(entry);
            native(f.state());
            assert_eq!(f.state[0x138 / 4], f.body() as u32);
            assert_eq!(
                f.body[0x9c / 4],
                if parked {
                    12f32.to_bits()
                } else {
                    999f32.to_bits()
                }
            );
            assert_eq!(ENTRY_SETUP.load(Ordering::SeqCst), 1);
            println!("R58_BASE_ENTRY parked={parked} inventory_slot=40 same_body=true body_x={} native_setup=1",f32::from_bits(f.body[0x9c/4]));
        }
    }
}

#[test]
#[ignore = "requires exact native MP exit fixture"]
fn confirmed_exit_uses_game_time_three_second_gate_and_never_refreshes_on_press() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        f.ability[0x80 / 4] = 10f32.to_bits();
        let request = f.request();
        f.park();
        let parked = f
            .run_exit(true, request, false)
            .expect("confirmed preserved MP exit");
        let enters = Cell::new(0);
        for (now, remaining) in [(10.0, 3.0), (11.0, 2.0), (12.999, 0.001)] {
            f.ability[0x80 / 4] = f32::to_bits(now);
            let r = parked.press(
                f.ability(),
                |_| panic!("early native entry check"),
                |_, _| enters.set(enters.get() + 1),
            );
            let Reentry::Wait(value) = r else {
                panic!("must wait: {r:?}");
            };
            assert!((value - remaining).abs() < 0.00001);
        }
        f.ability[0x80 / 4] = 13f32.to_bits();
        assert_eq!(
            parked.press(
                f.ability(),
                |_| true,
                |owner, body| {
                    assert_eq!(owner, f.pawn.as_ptr() as usize);
                    assert_eq!(body, f.body());
                    enters.set(enters.get() + 1);
                    *((body + 0x910) as *mut u8) = 2;
                }
            ),
            Reentry::Ready
        );
        assert_eq!(enters.get(), 1);
        assert_eq!(
            parked.press(f.ability(), |_| true, |_, _| panic!("double press")),
            Reentry::Invalid
        );
        assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
        println!("R58_REENTRY_GATE exit_game_time=10 wait=3,2,0.001 ready_at=13 entry_count=1 battery_unchanged=true");
    }
}
#[test]
#[ignore = "requires exact native MP exit fixture"]
fn parked_ticket_rejects_cancel_emp_death_replacement_clock_reset_and_native_restriction() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "dead",
            "empty",
            "disabled",
            "emp",
            "cancelled",
            "falling",
            "removed",
            "controller",
            "clock_reset",
            "clock_nan",
            "wrong_ability",
            "changed_current_armor",
            "changed_current_ability",
            "native_restricted",
            "eligibility_callback_destroys_body",
            "eligibility_callback_resets_clock",
        ] {
            let mut f = Fixture::new();
            f.ability[0x80 / 4] = 10f32.to_bits();
            let request = f.request();
            f.park();
            let parked = f.run_exit(true, request, false).unwrap();
            f.ability[0x80 / 4] = 13f32.to_bits();
            match case {
                "dead" => f.pawn[0x60 / 4] |= 0x800,
                "empty" => f.ability[0x7c / 4] = 0,
                "disabled" => f.ability[0x40 / 4] |= 4,
                "emp" => f.ability[0x98 / 4] = 2f32.to_bits(),
                "cancelled" => f.ability[0x40 / 4] &= !8,
                "falling" => f.body[0x910 / 4] = 3,
                "removed" => f.ability[0xa0 / 4] = 0,
                "controller" => f.pawn[0x938 / 4] = 0,
                "changed_current_armor" => f.pawn[0xcf4 / 4] = 0,
                "changed_current_ability" => f.armor[0x5c / 4] = 0,
                "clock_reset" => f.ability[0x80 / 4] = 0,
                "clock_nan" => f.ability[0x80 / 4] = f32::NAN.to_bits(),
                _ => (),
            }
            let ability = if case == "wrong_ability" {
                0
            } else {
                f.ability()
            };
            let result = parked.press(
                ability,
                |_| {
                    if case == "eligibility_callback_destroys_body" {
                        *((f.body() + 0x5dc) as *mut u32) = 0;
                    }
                    if case == "eligibility_callback_resets_clock" {
                        *((f.ability() + 0x80) as *mut u32) = 0;
                    }
                    case != "native_restricted"
                },
                |_, _| panic!("must not reenter: {case}"),
            );
            assert_eq!(result, Reentry::Invalid, "{case}");
            println!("R58_REENTRY_NEGATIVE {case} entered=false");
        }
    }
}
#[test]
#[ignore = "requires exact native MP exit fixture"]
fn native_fall_or_missing_exit_call_cannot_create_a_parked_ticket() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let f = Fixture::new();
        let request = f.request();
        f.park();
        assert!(f.run_exit(false, request, false).is_none());
        let f = Fixture::new();
        let request = f.request();
        f.park();
        let (_, parked) = exit_and_capture(
            request,
            f.state(),
            f.ability(),
            |_| false,
            || {
                *((f.state() + 0x138) as *mut u32) = 0;
            },
        );
        assert!(
            parked.is_none(),
            "an exit without the consumed native call site is not proof"
        );
    }
}

static REMOVED: AtomicU32 = AtomicU32::new(0);
static CANCELLED: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn body_dead(body: usize) -> u32 {
    u32::from(*((body + 0x5dc) as *const i32) <= 0)
}
unsafe extern "thiscall" fn remove_body(_: usize, _: usize, flag: u32) {
    assert_eq!(flag, 1);
    REMOVED.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn deactivate_fixture(ability: usize) {
    *((ability + 0x40) as *mut u32) &= !8;
}
unsafe extern "thiscall" fn cancel_fixture(ability: usize) {
    CANCELLED.fetch_add(1, Ordering::SeqCst);
    *((ability + 0xa0) as *mut u32) = 0;
    deactivate_fixture(ability);
}
#[test]
#[ignore = "requires exact retail DX11 native ability tick"]
fn native_tick_keeps_parked_ability_active_but_still_cancels_paralyzed_and_dead_body() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in ["parked", "paralyzed", "dead_body"] {
            let mut f = Fixture::new();
            f.park();
            let entry = native_fixture(
                &f,
                "ability_tick",
                0xe99c60,
                "a62e231c91c793de55231053051ed7c11725e04f4aeaa0e7f83fd168b8cc0ced",
            );
            let delta = f.base.wrapping_sub(0x400000);
            let addr = |v: usize| f.base + v - 0x400000;
            let code = std::slice::from_raw_parts_mut(entry as *mut u8, 0x14c);
            for va in [0x2aeb2bcu32, 0x339f854, 0x339fa30] {
                let indices: Vec<_> = (0..code.len() - 3)
                    .filter(|&i| code[i..i + 4] == va.to_le_bytes())
                    .collect();
                assert_eq!(indices.len(), 1);
                let i = indices[0];
                code[i..i + 4].copy_from_slice(&va.wrapping_add(delta as u32).to_le_bytes());
            }
            *((addr(0x2aeb2bc)) as *mut f32) = 1.0;
            put(addr(0x55fbc0), &[0xb8, 1, 0, 0, 0, 0xc3]);
            put(addr(0xe69c50), &[0x31, 0xc0, 0xc3]);
            put(addr(0xe7ecd0), &[0xc3]);
            jump(addr(0xdb0ee0), remove_body as *const () as usize);
            jump(addr(0xe6cd60), deactivate_fixture as *const () as usize);
            *((f.layout.body as usize + 0x330) as *mut u32) = body_dead as *const () as u32;
            *((f.layout.ability as usize + 0xb8) as *mut u32) = cancel_fixture as *const () as u32;
            REMOVED.store(0, Ordering::SeqCst);
            CANCELLED.store(0, Ordering::SeqCst);
            PARALYZED.store(case == "paralyzed", Ordering::SeqCst);
            if case == "dead_body" {
                f.body[0x5dc / 4] = 0;
            }
            let native: unsafe extern "thiscall" fn(usize, f32) = std::mem::transmute(entry);
            native(f.ability(), 0.016);
            assert_eq!(
                f.ability[0xa0 / 4],
                if case == "parked" { f.body() as u32 } else { 0 }
            );
            assert_eq!(
                f.ability[0x40 / 4] & 8,
                if case == "parked" { 8 } else { 0 }
            );
            assert_eq!(
                REMOVED.load(Ordering::SeqCst),
                u32::from(case == "dead_body")
            );
            assert_eq!(
                CANCELLED.load(Ordering::SeqCst),
                u32::from(case == "paralyzed")
            );
            println!(
                "R58_NATIVE_TICK {case} ability_active={} same_body={}",
                f.ability[0x40 / 4] & 8 != 0,
                f.ability[0xa0 / 4] == f.body() as u32
            );
        }
    }
}

static GATE_STATE: AtomicU32 = AtomicU32::new(0);
static GATE_POSE: AtomicBool = AtomicBool::new(true);
unsafe extern "thiscall" fn native_state_gate(_: usize, name: u32) -> u32 {
    if name == 1 {
        u32::from(GATE_POSE.load(Ordering::SeqCst))
    } else {
        u32::from(name != 0 && name == GATE_STATE.load(Ordering::SeqCst))
    }
}
#[test]
#[ignore = "requires exact retail native eligibility predicates"]
fn reentry_keeps_native_pose_emp_and_state_restrictions_without_reactivating_ability() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let root = std::path::PathBuf::from(std::env::var("SCBL_R58_REENTRY_FIXTURE").unwrap());
        let b = std::fs::read(root.join("entry_checks.bin")).unwrap();
        let d = std::fs::read(root.join("drone_entry_checks.bin")).unwrap();
        let mut f = Fixture::new();
        let delta = f.base.wrapping_sub(0x400000);
        let addr = |v: usize| f.base + v - 0x400000;
        let controller = vec![0u32; 0x60 / 4];
        f.pawn[0x16e0 / 4] = controller.as_ptr() as u32;
        f.ability[0x30 / 4] = 100f32.to_bits(); // new cast would require full battery
        f.ability[0x64 / 4] = u32::MAX;
        jump(addr(0xd5e8e0), native_state_gate as *const () as usize);
        put(addr(0xd11c50), &[0x31, 0xc0, 0xc3]);
        put(addr(0xe75e90), &[0x31, 0xc0, 0xc3]); // cdecl pawn argument cleaned by caller
        *((addr(0x2995478)) as *mut f64) = 100.0;
        for (va, n) in [
            (0x339f76c, 1u32),
            (0x339f9ac, 2),
            (0x339fa20, 3),
            (0x339f9bc, 4),
            (0x339f854, 5),
            (0x339f874, 6),
        ] {
            *((addr(va)) as *mut u32) = n;
        }
        for modified in [false, true] {
            let (mut base, mut drone) = if modified {
                reentry_check_windows(&b, &d, 0xe7e7a0, 0xe7eb00).unwrap()
            } else {
                (b.clone(), d.clone())
            };
            // Both code copies keep their relative layout; fixture-only absolute relocation.
            for bytes in [&mut base, &mut drone] {
                for va in [
                    0x2995478u32,
                    0x2993128,
                    0x339f76c,
                    0x339f9ac,
                    0x339fa20,
                    0x339f9bc,
                    0x339f854,
                    0x339f874,
                ] {
                    for i in 0..bytes.len() - 3 {
                        if bytes[i..i + 4] == va.to_le_bytes() {
                            bytes[i..i + 4]
                                .copy_from_slice(&va.wrapping_add(delta as u32).to_le_bytes());
                        }
                    }
                }
            }
            put(addr(0xe7e7a0), &base);
            put(addr(0xe7eb00), &drone);
            let native = |ability, arg| {
                assert_eq!(arg, 0);
                u8::from(native_entry_allowed(addr(0xe7eb00), ability))
            };
            GATE_POSE.store(true, Ordering::SeqCst);
            GATE_STATE.store(0, Ordering::SeqCst);
            assert_eq!(native(f.ability(), 0), u8::from(modified));
            if modified {
                for state in [3, 4, 5, 6] {
                    f.ability[0x64 / 4] = if state == 6 { 0 } else { u32::MAX };
                    GATE_STATE.store(state, Ordering::SeqCst);
                    assert_eq!(native(f.ability(), 0), 0, "native blocked state {state}");
                }
                f.ability[0x64 / 4] = u32::MAX;
                GATE_STATE.store(0, Ordering::SeqCst);
                GATE_POSE.store(false, Ordering::SeqCst);
                assert_eq!(native(f.ability(), 0), 0);
                GATE_POSE.store(true, Ordering::SeqCst);
                for flags in [9 | 4, 9 | 0x10, 9 | 0x20] {
                    f.ability[0x40 / 4] = flags;
                    assert_eq!(native(f.ability(), 0), 0);
                }
                f.ability[0x40 / 4] = 9;
                f.ability[0x98 / 4] = 1f32.to_bits();
                assert_eq!(native(f.ability(), 0), 0);
                f.ability[0x98 / 4] = 0;
            }
            assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
            assert_eq!(f.ability[0x40 / 4], 9);
            println!("R58_ENTRY_PREDICATES modified={modified} active_existing_body_permitted={modified} native_restrictions_retained=true fields_unchanged=true");
        }
        for i in 0..b.len() {
            let mut bad = b.clone();
            bad[i] ^= 1;
            assert!(reentry_check_windows(&bad, &d, 0xe7e7a0, 0xe7eb00).is_none());
        }
        for i in 0..d.len() {
            let mut bad = d.clone();
            bad[i] ^= 1;
            assert!(reentry_check_windows(&b, &bad, 0xe7e7a0, 0xe7eb00).is_none());
        }
    }
}

static NETWORK_DIRTY: AtomicU32 = AtomicU32::new(0);
static TRANSFORMS: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn dirty_fixture(_: usize) {
    NETWORK_DIRTY.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn transform_fixture(body: usize, pos: *const u32, rot: *const u32) {
    TRANSFORMS.fetch_add(1, Ordering::SeqCst);
    let p = std::slice::from_raw_parts(pos, 3).to_vec();
    let r = std::slice::from_raw_parts(rot, 3).to_vec();
    std::ptr::copy_nonoverlapping(p.as_ptr(), (body + 0x9c) as *mut u32, 3);
    std::ptr::copy_nonoverlapping(r.as_ptr(), (body + 0xa8) as *mut u32, 3);
}
unsafe extern "thiscall" fn transform_bridge(body: usize, pos: *const u32, rot: *const u32) {
    reentry_position_site(body, pos, rot, |p, r| transform_fixture(body, p, r));
}
#[test]
#[ignore = "requires exact retail body control and network activation fixtures"]
fn native_network_reentry_preserves_drone_transform_not_owner_spawn_transform() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for (role, modified, want) in [
            ("BASELINE", false, 999f32),
            ("MODIFIED", true, 12f32),
            ("ROLLBACK", false, 999f32),
        ] {
            let mut f = Fixture::new();
            f.body[0x8c / 4] = f.pawn.as_ptr() as u32;
            f.pawn[0x5c / 4] = 0x800000;
            f.body[0x4c / 4] = 1 << 8;
            f.pawn[0x9c / 4] = 999f32.to_bits();
            f.pawn[0xac / 4] = 777;
            f.body[0xac / 4] = 123;
            f.ability[0x80 / 4] = 10f32.to_bits();
            let request = f.request();
            f.park();
            let ticket = f.run_exit(true, request, false).unwrap();
            f.ability[0x80 / 4] = 13f32.to_bits();
            let addr = |v: usize| f.base + v - 0x400000;
            let entry = native_fixture(
                &f,
                "body_control",
                0xebd910,
                "63b18350cdd8e59b5868e919daa2c6ce11b8aa455c7cf5e097ec2d1a12e24ba1",
            );
            let root = std::path::PathBuf::from(std::env::var("SCBL_R58_REENTRY_FIXTURE").unwrap());
            let bytes = std::fs::read(root.join("replication_activate.bin")).unwrap();
            let mut net = if modified {
                replication_activate_window(
                    &bytes,
                    addr(0xfe1060),
                    transform_bridge as *const () as usize,
                )
                .unwrap()
            } else {
                bytes.clone()
            };
            for (off, target) in [(0x16, 0x7748c0usize), (0x4f, 0x7e24f0), (0x57, 0x7fc740)] {
                net[off + 1..off + 5].copy_from_slice(
                    &(addr(target) as u32)
                        .wrapping_sub((addr(0xfe1060) + off + 5) as u32)
                        .to_le_bytes(),
                );
            }
            put(addr(0xfe1060), &net);
            // Native activation tail copied exactly; node78=0 takes its immediate
            // active-flag path, while actual list/transport work stays a leaf.
            native_fixture(
                &f,
                "network_activate_tail",
                0x7fc740,
                "ff0f14a2216a400a605c47f305a9be02163fb720d94b02022f8df6bde47ff6b9",
            );
            jump(addr(0x7e24f0), dirty_fixture as *const () as usize);
            let mut node = vec![0u32; 0x400 / 4];
            node[0x14 / 4] = 1;
            node[0x88 / 4] = f.body() as u32;
            f.body[0x2c0 / 4] = node.as_ptr() as u32;
            for va in [0xad6c20, 0xaa8f00] {
                put(addr(va), &[0xc2, 4, 0]);
            }
            put(f.base + 0x120, &[0x31, 0xc0, 0xc3]);
            *((f.layout.owner as usize + 0x4f4) as *mut u32) = (f.base + 0x120) as u32;
            put(f.base + 0x140, &[0xc2, 20, 0]);
            *((f.layout.body as usize + 0x368) as *mut u32) = (f.base + 0x140) as u32;
            put(f.base + 0x160, &[0xc2, 4, 0]);
            *((f.layout.body as usize + 0x314) as *mut u32) = (f.base + 0x160) as u32;
            *((f.layout.body as usize + 0x43c) as *mut u32) = transform_fixture as *const () as u32;
            TRANSFORMS.store(0, Ordering::SeqCst);
            NETWORK_DIRTY.store(0, Ordering::SeqCst);
            let native: unsafe extern "thiscall" fn(usize, u32) -> u32 = std::mem::transmute(entry);
            with_reentry_position(if modified { Some(&ticket) } else { None }, || {
                assert_eq!(native(f.body(), 1), 1)
            });
            assert_eq!(f.body[0x9c / 4], want.to_bits());
            assert_eq!(f.body[0xac / 4], if modified { 123 } else { 777 });
            assert_eq!(NETWORK_DIRTY.load(Ordering::SeqCst), 1);
            assert_eq!(TRANSFORMS.load(Ordering::SeqCst), 1);
            assert_eq!(*((node.as_ptr() as usize + 0x32d) as *const u8), 1);
            assert_eq!(*((node.as_ptr() as usize + 0x32e) as *const u8), 1);
            assert_eq!(f.body[0x910 / 4], 2);
            assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
            println!("R58_NETWORK_ENTRY {role} transform_x={want} native_transform_calls=1 dirty=1 network_active=1 battery_unchanged=true");
            // Outside that entry, this same call site returns to native inputs.
            transform_bridge(
                f.body(),
                f.pawn.as_ptr().add(0x9c / 4),
                f.pawn.as_ptr().add(0xa8 / 4),
            );
            assert_eq!(f.body[0x9c / 4], 999f32.to_bits());
        }
    }
}

#[test]
#[ignore = "requires exact MP exit and network activation fixtures"]
fn native_reentry_position_adapter_is_exact_one_use_nested_and_identity_scoped() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let f = Fixture::new();
        let request = f.request();
        f.park();
        let ticket = f.run_exit(true, request, false).unwrap();
        let root = std::path::PathBuf::from(std::env::var("SCBL_R58_REENTRY_FIXTURE").unwrap());
        let bytes = std::fs::read(root.join("replication_activate.bin")).unwrap();
        let out = replication_activate_window(&bytes, 0xfe1060, 0x12345678).unwrap();
        for i in 0..out.len() {
            if !(0x44..0x4a).contains(&i) {
                assert_eq!(out[i], bytes[i]);
            }
        }
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 1;
            assert!(replication_activate_window(&bad, 0xfe1060, 0x12345678).is_none());
        }
        let pos = [999f32.to_bits(), 0, 0];
        let rot = [0u32; 3];
        let invoke = |body| reentry_position_site(body, pos.as_ptr(), rot.as_ptr(), |p, _| *p);
        with_reentry_position(Some(&ticket), || {
            *((f.body() + 0x910) as *mut u8) = 2;
            assert_eq!(invoke(f.body() + 4), 999f32.to_bits());
            with_reentry_position(None, || assert_eq!(invoke(f.body()), 999f32.to_bits()));
            assert_eq!(invoke(f.body()), 12f32.to_bits());
            assert_eq!(invoke(f.body()), 999f32.to_bits());
        });
        assert_eq!(invoke(f.body()), 999f32.to_bits());
        *((f.body() + 0x910) as *mut u8) = 1;
        with_reentry_position(Some(&ticket), || {
            *((f.body() + 0x910) as *mut u8) = 2;
            *((f.body() + 0x5dc) as *mut i32) = 0;
            assert_eq!(invoke(f.body()), 999f32.to_bits());
        });
    }
}

// Native input fixtures begin AFTER the game's key binding resolver. None of
// these paths knows Ctrl, Shift, a scan code, or whether the input was a mouse.
struct ActionHarness {
    fixture: Fixture,
    flow: ActionFlow,
    detonate: u32,
    station: u32,
    exit_camera: u32,
}
thread_local! {static ACTION_HARNESS:Cell<*const ActionHarness>=const{Cell::new(std::ptr::null())};}
static EXIT_EVENTS: AtomicU32 = AtomicU32::new(0);
static EXPLOSIONS: AtomicU32 = AtomicU32::new(0);
static ACTIVE_PRESSES: AtomicU32 = AtomicU32::new(0);
static NEW_CASTS: AtomicU32 = AtomicU32::new(0);
static INPUT_LOCAL: AtomicBool = AtomicBool::new(true);
unsafe extern "thiscall" fn action_base_bridge(
    state: usize,
    name: *const u32,
    payload: usize,
) -> u32 {
    ACTION_HARNESS.with(|s| {
        let h = &*s.get();
        let f = &h.fixture;
        let native: unsafe extern "thiscall" fn(usize, *const u32, usize) -> u32 =
            std::mem::transmute(f.base + 0x14cf100 - 0x400000);
        h.flow.action(
            state,
            f.ability(),
            *name,
            h.detonate,
            h.station,
            f.layout,
            |_| false,
            |a| native(state, &a, payload),
        )
    })
}
unsafe extern "thiscall" fn native_action_event(
    owner: usize,
    name: *const u32,
    a: usize,
    b: usize,
) {
    ACTION_HARNESS.with(|s| {
        let h = &*s.get();
        let f = &h.fixture;
        assert_eq!(owner, f.pawn.as_ptr() as usize);
        assert_eq!((a, b), (0, 0));
        if *name == h.exit_camera {
            EXIT_EVENTS.fetch_add(1, Ordering::SeqCst);
        } else {
            assert_eq!(*name, h.detonate);
            let call: unsafe extern "thiscall" fn(usize, *const u32, usize) -> u32 =
                std::mem::transmute(f.base + 0x14d3490 - 0x400000);
            call(f.state(), name, 0);
        }
    });
}
unsafe extern "thiscall" fn explode_fixture(body: usize, arg: u32) {
    assert_eq!(arg, 1);
    EXPLOSIONS.fetch_add(1, Ordering::SeqCst);
    *((body + 0x910) as *mut u8) = 3;
}
unsafe extern "thiscall" fn press_owner_local(_: usize) -> u32 {
    u32::from(INPUT_LOCAL.load(Ordering::SeqCst))
}
unsafe extern "thiscall" fn active_press_fixture(_: usize) {
    ACTIVE_PRESSES.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn new_cast_fixture(_: usize) {
    NEW_CASTS.fetch_add(1, Ordering::SeqCst);
}

unsafe fn action_bytes(f: &Fixture, name: &str, va: usize, sha: &str) -> Vec<u8> {
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_ACTION_FIXTURE").unwrap());
    let data = std::fs::read(root.join(format!("{name}.bin"))).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&data)), sha);
    // Only fixture globals are rebased. Their offsets are decoded from the
    // hash-locked original; direct rel32 calls remain valid in the same arena.
    let (expected_va, fixes): (usize, &[(usize, usize)]) = match name {
        "detonate_action_producer" => (0xaea1d0, &[(0x4, 0x2994eb8), (0x29, 0x339f658)]),
        "mp_action_startup_gate" => (0x14d3490, &[(0xe, 0x339f658), (0x1b, 0x2bbf4d0)]),
        "base_action" => (
            0x14cf100,
            &[
                (0x6, 0x283c7c8),
                (0x18, 0x3288bbc),
                (0x5f, 0x339f658),
                (0x84, 0x2bbf4d0),
                (0xb0, 0x339f664),
                (0xba, 0x2d75c00),
                (0xcd, 0x339f66c),
                (0xe5, 0x339f668),
                (0xef, 0x2d75c00),
                (0xff, 0x339f670),
                (0x114, 0x339f674),
                (0x12c, 0x3107ed4),
                (0x147, 0x339f678),
                (0x15f, 0x3107ed4),
                (0x17d, 0x339fb4c),
                (0x20d, 0x2bbf49c),
                (0x277, 0x339ce34),
                (0x2a9, 0x29d5f08),
                (0x2ae, 0x2bfc138),
                (0x2b3, 0x29fcbd0),
                (0x2d0, 0x2d71740),
                (0x2e5, 0x29a2300),
                (0x31f, 0x339cf30),
                (0x34a, 0x339fb50),
                (0x354, 0x2bbf4d0),
                (0x372, 0x339fb54),
                (0x3ac, 0x339f6e0),
                (0x3bc, 0x339f660),
                (0x3fe, 0x339fb98),
                (0x460, 0x339fb9c),
                (0x475, 0x339fb6c),
            ],
        ),
        _ => panic!("unregistered native action fixture"),
    };
    assert_eq!(va, expected_va);
    let mut out = data;
    for &(off, target) in fixes {
        assert_eq!(&out[off..off + 4], &(target as u32).to_le_bytes());
        out[off..off + 4].copy_from_slice(&((f.base + target - 0x400000) as u32).to_le_bytes());
    }
    out
}
impl ActionHarness {
    unsafe fn prepare(&mut self, modified: bool) {
        let f = &mut self.fixture;
        let base = f.base;
        let addr = |v: usize| base + v - 0x400000;
        f.body[0x8c / 4] = f.pawn.as_ptr() as u32;
        f.body[0x7c4 / 4] = 0x10000;
        f.ability[0x34 / 4] = 0.25f32.to_bits();
        f.ability[0x80 / 4] = 10f32.to_bits();
        for (p, v) in [
            (0x339f658, self.detonate),
            (0x339fb54, self.station),
            (0x339f6e0, self.exit_camera),
        ] {
            put(addr(p), &v.to_le_bytes());
        }
        for (name, va, sha) in [
            (
                "detonate_action_producer",
                0xaea1d0,
                "a3b705aad75ac8e1d868eee598db719e606defa8eb4043b1ff1dc404b58f69ac",
            ),
            (
                "base_action",
                0x14cf100,
                "ee270be7fe3521ab1d01a58be95f1c3d3130099e1cd281cc038dae9f2c44c0a9",
            ),
            (
                "mp_action_startup_gate",
                0x14d3490,
                "4ba74e4ab011dd78ceabcb4d397ec13b8541dddf51d18423ff9629656b030c5a",
            ),
        ] {
            let code = action_bytes(f, name, va, sha);
            put(addr(va), &code);
        }
        if modified {
            // Keep fixture-rebased absolute operands; replace exactly the same
            // native call that the production clone replaces after its guard.
            let root = std::path::PathBuf::from(std::env::var("SCBL_R58_ACTION_FIXTURE").unwrap());
            let raw = std::fs::read(root.join("mp_action_startup_gate.bin")).unwrap();
            let cloned = action_window(
                &raw,
                addr(0x14d3490),
                action_base_bridge as *const () as usize,
            )
            .unwrap();
            put(addr(0x14d3490) + 0x7d, &cloned[0x7d..0x82]);
        }
        put(addr(0xe599e0), &[0x8b, 0x41, 0x5c, 0xc3]);
        jump(addr(0xd873f0), native_action_event as *const () as usize);
        put(addr(0x14f70a0), &[0xb8, 0x56, 0x34, 0, 0, 0xc2, 8, 0]);
        put(base + 0x180, &[0xb8, 1, 0, 0, 0, 0xc3]);
        put(base + 0x190, &[0x31, 0xc0, 0xc3]);
        *((f.layout.state as usize + 0x278) as *mut u32) = (base + 0x180) as u32;
        *((f.layout.body as usize + 0x758) as *mut u32) = (base + 0x190) as u32;
        *((f.layout.body as usize + 0x750) as *mut u32) = explode_fixture as *const () as u32;
        EXIT_EVENTS.store(0, Ordering::SeqCst);
        EXPLOSIONS.store(0, Ordering::SeqCst);
    }
    unsafe fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        struct Restore(*const ActionHarness);
        impl Drop for Restore {
            fn drop(&mut self) {
                ACTION_HARNESS.with(|s| s.set(self.0));
            }
        }
        let _r = Restore(ACTION_HARNESS.with(|s| s.replace(self)));
        f()
    }
    unsafe fn emit(&self) {
        let native: unsafe extern "thiscall" fn(usize) =
            std::mem::transmute(self.fixture.base + 0xaea1d0 - 0x400000);
        self.with(|| native(self.fixture.body()));
    }
}

#[test]
#[ignore = "requires retail semantic-action and base OnStation fixtures"]
fn native_semantic_action_keeps_startup_delay_and_converts_only_explicit_detonate() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for (label, modified) in [("BASELINE", false), ("MODIFIED", true), ("ROLLBACK", false)] {
            for detonate in [11, 9123] {
                let mut h = ActionHarness {
                    fixture: Fixture::new(),
                    flow: ActionFlow::default(),
                    detonate,
                    station: detonate + 1,
                    exit_camera: detonate + 2,
                };
                h.prepare(modified);
                h.emit();
                assert_eq!(h.fixture.body[0x910 / 4], 2);
                assert_eq!(EXIT_EVENTS.load(Ordering::SeqCst), 0);
                assert_eq!(EXPLOSIONS.load(Ordering::SeqCst), 0);
                assert!(h.flow.state.lock().unwrap().abilities.is_empty());
                h.fixture.state[0x150 / 4] = 0.25f32.to_bits();
                h.emit();
                assert_eq!(h.fixture.body[0x910 / 4], if modified { 1 } else { 3 });
                assert_eq!(EXIT_EVENTS.load(Ordering::SeqCst), u32::from(modified));
                assert_eq!(EXPLOSIONS.load(Ordering::SeqCst), u32::from(!modified));
                if modified {
                    h.emit();
                    assert_eq!(EXIT_EVENTS.load(Ordering::SeqCst), 1);
                    assert_eq!(EXPLOSIONS.load(Ordering::SeqCst), 0);
                    let f = &h.fixture;
                    let native = f.prepare_exit(true);
                    h.flow
                        .exit(f.state(), f.ability(), |_| false, || native(f.state()));
                    assert!(
                        matches!(h.flow.press(f.ability(),|_|true,|_,_|panic!("early")),Press::Wait(t)if t==3.0)
                    );
                    h.fixture.ability[0x80 / 4] = 13f32.to_bits();
                    let f = &h.fixture;
                    assert_eq!(
                        h.flow.press(
                            f.ability(),
                            |_| true,
                            |owner, body| {
                                assert_eq!(owner, f.pawn.as_ptr() as usize);
                                assert_eq!(body, f.body());
                            }
                        ),
                        Press::Queued
                    );
                    assert_eq!(
                        h.flow.press(
                            f.ability(),
                            |_| panic!("duplicate"),
                            |_, _| panic!("duplicate")
                        ),
                        Press::Queued
                    );
                    h.flow.entry(f.state(), f.ability(), || {
                        *((f.body() + 0x910) as *mut u8) = 2;
                        let dummy = [999f32.to_bits(); 3];
                        assert_eq!(
                            reentry_position_site(
                                f.body(),
                                dummy.as_ptr(),
                                dummy.as_ptr(),
                                |p, _| *p
                            ),
                            12f32.to_bits()
                        );
                    });
                    assert!(h.flow.state.lock().unwrap().abilities.is_empty());
                }
                println!("R58_ACTION {label} semantic_name={detonate} startup_guard=true explicit_exit_events={} explosions={} raw_key_reads=0",EXIT_EVENTS.load(Ordering::SeqCst),EXPLOSIONS.load(Ordering::SeqCst));
            }
        }
    }
}

#[test]
#[ignore = "requires exact native press fixture"]
fn native_ability_press_keeps_local_debounce_disable_and_fresh_deploy_paths() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let base = f.base;
        let addr = |v: usize| base + v - 0x400000;
        let raw = std::fs::read(
            std::path::PathBuf::from(std::env::var("SCBL_R58_ACTION_FIXTURE").unwrap())
                .join("ability_press.bin"),
        )
        .unwrap();
        let code = ability_press_window(
            &raw,
            addr(0xe7e870),
            active_press_fixture as *const () as usize,
        )
        .unwrap();
        put(addr(0xe7e870), &code);
        for (off, target) in [(6, 0x55fbc0), (0x5d, 0xd75ca0), (0x66, 0xe6cc40)] {
            put(
                addr(0xe7e870) + off + 1,
                &(addr(target) as u32)
                    .wrapping_sub((addr(0xe7e870) + off + 5) as u32)
                    .to_le_bytes(),
            );
        }
        put(
            addr(0xe7e870) + 0x1e,
            &(addr(0x2993128) as u32).to_le_bytes(),
        );
        put(
            addr(0xe7e870) + 0x59,
            &(addr(0x339fa1c) as u32).to_le_bytes(),
        );
        jump(addr(0x55fbc0), press_owner_local as *const () as usize);
        jump(addr(0xe6cc40), new_cast_fixture as *const () as usize);
        put(base + 0x180, &[0xb8, 1, 0, 0, 0, 0xc2, 4, 0]);
        *((f.layout.ability as usize + 0x8c) as *mut u32) = (base + 0x180) as u32;
        f.ability[0x64 / 4] = u32::MAX;
        let native: unsafe extern "thiscall" fn(usize) = std::mem::transmute(addr(0xe7e870));
        for case in ["nonlocal", "debounce", "disabled", "active", "fresh"] {
            ACTIVE_PRESSES.store(0, Ordering::SeqCst);
            NEW_CASTS.store(0, Ordering::SeqCst);
            INPUT_LOCAL.store(case != "nonlocal", Ordering::SeqCst);
            f.ability[0x90 / 4] = if case == "debounce" {
                1f32.to_bits()
            } else {
                0
            };
            f.ability[0x40 / 4] = if case == "disabled" {
                12
            } else if case == "fresh" {
                0
            } else {
                8
            };
            native(f.ability());
            assert_eq!(
                ACTIVE_PRESSES.load(Ordering::SeqCst),
                u32::from(case == "active")
            );
            assert_eq!(NEW_CASTS.load(Ordering::SeqCst), u32::from(case == "fresh"));
            println!("R58_INPUT case={case} active_dispatch={} fresh_cast={} native_prefix_preserved=true",ACTIVE_PRESSES.load(Ordering::SeqCst),NEW_CASTS.load(Ordering::SeqCst));
        }
    }
}

#[test]
#[ignore = "requires native action fixtures"]
fn action_and_press_clones_reject_every_unknown_byte_and_preserve_native_action_guard() {
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_ACTION_FIXTURE").unwrap());
    for (name, make, at) in [
        (
            "mp_action_startup_gate",
            action_window as fn(&[u8], usize, usize) -> Option<Vec<u8>>,
            0x14d3490,
        ),
        ("ability_press", ability_press_window, 0xe7e870),
    ] {
        let bytes = std::fs::read(root.join(format!("{name}.bin"))).unwrap();
        let out = make(&bytes, at, 0x12345678).unwrap();
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 1;
            assert!(make(&bad, at, 0x12345678).is_none());
            let changed = if name == "ability_press" {
                (0xa5..0xe9).contains(&i)
            } else {
                (0x7e..0x82).contains(&i)
            };
            if !changed {
                assert_eq!(bytes[i], out[i]);
            }
        }
        assert!(make(&bytes, 0, 1).is_none());
        assert!(make(&bytes, at, 0).is_none());
    }
}

#[test]
#[ignore = "requires native MP exit fixture"]
fn async_action_flow_cancellation_and_reentrant_callbacks_cannot_resurrect_a_ticket() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "denied_station",
            "forced_exit",
            "cancel_during_exit",
            "cancel_during_check",
            "temporary_restriction",
            "immediate_entry",
            "clear_during_entry",
        ] {
            let mut f = Fixture::new();
            let flow = ActionFlow::default();
            flow.action(
                f.state(),
                f.ability(),
                11,
                11,
                12,
                f.layout,
                |_| false,
                |name| {
                    assert_eq!(name, 12);
                    if case != "denied_station" {
                        f.park();
                    }
                    0x3456
                },
            );
            if case == "denied_station" {
                assert!(flow.state.lock().unwrap().abilities.is_empty());
                continue;
            }
            if case == "forced_exit" {
                flow.cancel(f.ability());
            }
            let native = f.prepare_exit(true);
            flow.exit(
                f.state(),
                f.ability(),
                |_| false,
                || {
                    if case == "cancel_during_exit" {
                        flow.cancel(f.ability());
                    }
                    native(f.state());
                },
            );
            if matches!(case, "forced_exit" | "cancel_during_exit") {
                assert!(flow.state.lock().unwrap().abilities.is_empty());
                assert_eq!(
                    f.body[0x910 / 4],
                    3,
                    "cancelled request must not suppress native falling"
                );
                continue;
            }
            f.ability[0x80 / 4] = 3f32.to_bits();
            if case == "cancel_during_check" {
                assert_eq!(
                    flow.press(
                        f.ability(),
                        |_| {
                            flow.cancel(f.ability());
                            true
                        },
                        |_, _| panic!("cancelled")
                    ),
                    Press::Restricted
                );
                assert!(flow.state.lock().unwrap().abilities.is_empty());
                continue;
            }
            if case == "temporary_restriction" {
                assert_eq!(
                    flow.press(f.ability(), |_| false, |_, _| panic!("blocked pose")),
                    Press::Restricted
                );
            }
            assert_eq!(
                flow.press(
                    f.ability(),
                    |_| true,
                    |_, _| {
                        flow.entry(f.state(), f.ability(), || {
                            if case == "clear_during_entry" {
                                flow.clear();
                                *((f.body() + 0x910) as *mut u8) = 2;
                                let pos = [999f32.to_bits(); 3];
                                assert_eq!(
                                    reentry_position_site(
                                        f.body(),
                                        pos.as_ptr(),
                                        pos.as_ptr(),
                                        |p, _| *p
                                    ),
                                    999f32.to_bits()
                                );
                            } else {
                                *((f.body() + 0x910) as *mut u8) = 2;
                            }
                        });
                    }
                ),
                Press::Queued
            );
            assert!(flow.state.lock().unwrap().abilities.is_empty());
        }
    }
}

#[test]
#[ignore = "requires exact native MP exit fixture"]
fn native_action_flow_does_not_claim_other_actions_abilities_or_automatic_on_station() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut a = Fixture::new();
        let mut b = Fixture::new();
        let flow = ActionFlow::default();
        for action in [0, 12, 13, 100] {
            assert_eq!(
                flow.action(
                    a.state(),
                    a.ability(),
                    action,
                    11,
                    12,
                    a.layout,
                    |_| panic!("unrelated"),
                    |forwarded| {
                        assert_eq!(forwarded, action);
                        0x9876
                    }
                ),
                0x9876
            );
            assert!(flow.state.lock().unwrap().abilities.is_empty());
        }
        // Automatic native OnStation alone must never grant retention.
        a.park();
        let native = a.prepare_exit(true);
        flow.exit(a.state(), a.ability(), |_| false, || native(a.state()));
        assert_eq!(a.body[0x910 / 4], 3);
        assert!(flow.state.lock().unwrap().abilities.is_empty());
        a.body[0x910 / 4] = 2;
        a.state[0x138 / 4] = a.body() as u32;
        for f in [&a, &b] {
            flow.action(
                f.state(),
                f.ability(),
                11,
                11,
                12,
                f.layout,
                |_| false,
                |_| {
                    f.park();
                    0
                },
            );
            let exit = f.prepare_exit(true);
            flow.exit(f.state(), f.ability(), |_| false, || exit(f.state()));
        }
        a.ability[0x80 / 4] = 3f32.to_bits();
        b.ability[0x80 / 4] = 3f32.to_bits();
        flow.cancel(a.ability());
        assert_eq!(
            flow.press(
                a.ability(),
                |_| panic!("cancelled"),
                |_, _| panic!("cancelled")
            ),
            Press::Native
        );
        assert_eq!(
            flow.press(
                b.ability(),
                |_| true,
                |owner, body| {
                    assert_eq!(owner, b.pawn.as_ptr() as usize);
                    assert_eq!(body, b.body());
                }
            ),
            Press::Queued
        );
        assert_eq!(
            flow.press(0, |_| panic!("unrelated"), |_, _| panic!("unrelated")),
            Press::Native
        );
        flow.clear();
        assert!(flow.state.lock().unwrap().abilities.is_empty());
    }
}

#[test]
fn stale_action_completion_never_overwrites_a_new_generation() {
    let flow = ActionFlow::default();
    let old = flow.reserve(123, Phase::Busy).unwrap();
    flow.cancel(123);
    let new = flow.reserve(123, Phase::Busy).unwrap();
    assert_ne!(new, old);
    flow.finish(123, old, None);
    assert!(flow.is_current(123, new));
    flow.clear();
    flow.finish(123, new, Some(Phase::Busy));
    assert!(flow.state.lock().unwrap().abilities.is_empty());
}

// Preparation executes the whole original retail activation with world/inventory
// leaves isolated. This proves stack/ABI and no-spawn semantics, not live camera.
static PREP_SELECT: AtomicBool = AtomicBool::new(true);
static PREP_ADDS: AtomicU32 = AtomicU32::new(0);
static PREP_REMOVES: AtomicU32 = AtomicU32::new(0);
static PREP_QUEUES: AtomicU32 = AtomicU32::new(0);
thread_local! {static PREP_ABILITY:Cell<usize>=const{Cell::new(0)};}
unsafe extern "thiscall" fn prep_add(_: usize, _: usize, _: u32) {
    PREP_ADDS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn prep_remove(_: usize, _: usize, _: u32) {
    PREP_REMOVES.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn prep_current(_: usize) -> usize {
    if PREP_SELECT.load(Ordering::SeqCst) {
        PREP_ABILITY.with(|p| word(p.get(), 0xa0).unwrap() as usize)
    } else {
        0
    }
}
unsafe extern "thiscall" fn prep_camera(_: usize, out: *mut u32) -> *mut u32 {
    out.write(11);
    out.add(1).write(22);
    out.add(2).write(33);
    out
}
unsafe extern "thiscall" fn prep_queue(_: usize, _: *const u32, mode: u32) {
    assert_eq!(mode, 2);
    PREP_QUEUES.fetch_add(1, Ordering::SeqCst);
}
fn prepare_fixture_bytes(name: &str, sha: &str) -> Vec<u8> {
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_PREPARE_FIXTURE").unwrap());
    let b = std::fs::read(root.join(format!("{name}.bin"))).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&b)), sha);
    b
}
#[test]
#[ignore = "requires hash-locked native preparation fixture"]
fn same_body_preparation_keeps_native_camera_and_weapon_without_spawn_add_or_remove() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for (role, modified) in [("BASELINE", false), ("MODIFIED", true), ("ROLLBACK", false)] {
            for (local, select) in [(true, true), (true, false), (false, true)] {
                for selected in [7u8, 40] {
                    let mut f = Fixture::new();
                    f.ability[0xb4 / 4] = 39;
                    let addr = |va: usize| f.base + va - 0x400000;
                    let mut fresh = vec![0u32; 0xc40 / 4];
                    fresh[0] = f.layout.body;
                    let mut world = vec![0u32; 64];
                    world[0] = (f.base + 0x500) as u32;
                    f.pawn[0x148 / 4] = world.as_ptr() as u32;
                    f.pawn[0x1410 / 4] = f.base as u32;
                    let count = f.base + 0x600;
                    let spawn = f.base + 0x700;
                    let mut stub = vec![0xff, 0x05];
                    stub.extend((count as u32).to_le_bytes());
                    stub.push(0xb8);
                    stub.extend((fresh.as_ptr() as u32).to_le_bytes());
                    stub.extend([0xc2, 0x48, 0]);
                    put(spawn, &stub);
                    put(f.base + 0x500 + 0x98, &(spawn as u32).to_le_bytes());
                    put(addr(0x55fbc0), &[0xb8, u8::from(local), 0, 0, 0, 0xc3]);
                    put(addr(0xd15a30), &[0xb8, selected, 0, 0, 0, 0xc3]);
                    put(addr(0xd762d0), &[0xc2, 4, 0]);
                    put(addr(0xd5e8e0), &[0x31, 0xc0, 0xc2, 4, 0]);
                    for (va, fnptr) in [
                        (0xdb5600, prep_add as *const () as usize),
                        (0xdb0ee0, prep_remove as *const () as usize),
                        (0xd6ffd0, prep_current as *const () as usize),
                        (0xd1bed0, prep_camera as *const () as usize),
                        (0xd75ca0, prep_queue as *const () as usize),
                    ] {
                        jump(addr(va), fnptr);
                    }
                    let bytes = prepare_fixture_bytes(
                        "activate",
                        "9a26031908c6b43fd69e75aac7c6629090488066e5eb02e7a729188b84a87050",
                    );
                    let at = addr(0xe7eb60);
                    let mut code = if modified {
                        prepare_window(&bytes, at, addr(0xd75ca0)).unwrap()
                    } else {
                        bytes
                    };
                    for (off, target) in [
                        (0xe, 0x55fbc0),
                        (0x89, 0xbae820),
                        (0xba, 0xd15a30),
                        (0xd0, 0xdb5600),
                        (0xda, 0xd762d0),
                        (0xe8, 0xd6ffd0),
                        (0xf7, 0xdb0ee0),
                        (0x10a, 0xd5e8e0),
                        (0x119, 0xd5e120),
                        (0x13b, 0xd1bed0),
                        (0x162, 0xd75ca0),
                    ] {
                        if code[off] == 0xe8 {
                            code[off + 1..off + 5].copy_from_slice(
                                &(addr(target) as u32)
                                    .wrapping_sub((at + off + 5) as u32)
                                    .to_le_bytes(),
                            );
                        }
                    }
                    for off in [74, 83, 126, 133, 148, 261, 338] {
                        if modified && off < 0xb1 {
                            continue;
                        }
                        let va = u32::from_le_bytes(code[off..off + 4].try_into().unwrap());
                        code[off..off + 4]
                            .copy_from_slice(&(addr(va as usize) as u32).to_le_bytes());
                    }
                    put(addr(0x339e8c0), &1u32.to_le_bytes());
                    put(at, &code);
                    PREP_ABILITY.with(|p| p.set(f.ability()));
                    PREP_SELECT.store(select, Ordering::SeqCst);
                    PREP_ADDS.store(0, Ordering::SeqCst);
                    PREP_REMOVES.store(0, Ordering::SeqCst);
                    PREP_QUEUES.store(0, Ordering::SeqCst);
                    let charge = f.ability[0x7c / 4];
                    let elapsed = f.ability[0x80 / 4];
                    let call: unsafe extern "thiscall" fn(usize) = std::mem::transmute(at);
                    call(f.ability());
                    assert_eq!(*(count as *const u32), u32::from(local && !modified));
                    assert_eq!(
                        PREP_ADDS.load(Ordering::SeqCst),
                        u32::from(local && !modified)
                    );
                    assert_eq!(
                        PREP_REMOVES.load(Ordering::SeqCst),
                        u32::from(local && !modified && !select)
                    );
                    assert_eq!(
                        PREP_QUEUES.load(Ordering::SeqCst),
                        u32::from(local && select)
                    );
                    assert_eq!(
                        f.ability[0xa0 / 4],
                        if local && !modified {
                            fresh.as_ptr() as u32
                        } else {
                            f.body() as u32
                        }
                    );
                    assert_eq!(
                        (f.ability[0x7c / 4], f.ability[0x80 / 4]),
                        (charge, elapsed)
                    );
                    if local {
                        assert_eq!(
                            f.ability[0xb4 / 4] & 255,
                            if modified && selected == 40 {
                                39
                            } else {
                                selected as u32
                            }
                        );
                    }
                    if local && select {
                        assert_eq!(&f.ability[0xa4 / 4..0xb0 / 4], &[0, 22, 0]);
                    }
                    println!("R58_PREPARE {role} local={local} selected={select} spawn={} add={} remove={} queue={} battery_unchanged=true",*(count as *const u32),PREP_ADDS.load(Ordering::SeqCst),PREP_REMOVES.load(Ordering::SeqCst),PREP_QUEUES.load(Ordering::SeqCst));
                }
            }
        }
    }
}
static QUEUE_ALLOC: AtomicU32 = AtomicU32::new(0);
static QUEUE_FREE: AtomicU32 = AtomicU32::new(0);
static QUEUE_ACCEPT: AtomicBool = AtomicBool::new(false);
unsafe extern "thiscall" fn queue_alloc(_: usize, _: *const u32, _: usize, out: *mut u32) {
    out.write(QUEUE_ALLOC.load(Ordering::SeqCst));
}
unsafe extern "thiscall" fn queue_init(state: usize, owner: usize, _: usize) {
    *((state + 0x48) as *mut u32) = owner as u32;
}
unsafe extern "thiscall" fn queue_free(_: usize, _: usize) {
    QUEUE_FREE.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn queue_dispatch(
    owner: usize,
    state: usize,
    params: usize,
    priority: u32,
) {
    PREP_ABILITY.with(|p| {
        let f = &*(p.get() as *const Fixture);
        let layer = word(state, 0x34).unwrap();
        let native: unsafe extern "thiscall" fn(usize, usize, usize, u32) =
            std::mem::transmute(f.base + 0xd5e070 - 0x400000);
        native(owner, state, params, priority);
        QUEUE_ACCEPT.store(queued_state(owner, state, layer), Ordering::SeqCst);
    });
}
#[test]
#[ignore = "requires native queue/dispatch/schedule fixtures"]
fn queue_acceptance_observes_native_layer_not_incidental_void_return() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for case in [
            "accepted",
            "no_manager",
            "allocation_failed",
            "no_layer",
            "other_layer_busy",
            "disabled_layer",
            "higher_priority",
        ] {
            let mut f = Fixture::new();
            let addr = |va: usize| f.base + va - 0x400000;
            let mut layer = vec![0u32; 0x80 / 4];
            let mut other = vec![0u32; 0x80 / 4];
            f.state[0x34 / 4] = 1;
            f.pawn[0x16ec / 4] = f.base as u32;
            f.pawn[0x16e4 / 4] = layer.as_ptr() as u32;
            layer[0x3c / 4] = f.pawn.as_ptr() as u32;
            layer[0x58 / 4] = 1;
            layer[0x54 / 4] = 0;
            match case {
                "no_manager" => f.pawn[0x16ec / 4] = 0,
                "no_layer" => f.pawn[0x16e4 / 4] = 0,
                "other_layer_busy" => {
                    f.pawn[0x16e0 / 4] = other.as_ptr() as u32;
                    other[0x30 / 4] = 1;
                }
                "disabled_layer" => layer[0x58 / 4] = 0,
                "higher_priority" => layer[0x54 / 4] = 3,
                _ => (),
            }
            QUEUE_ALLOC.store(
                if case == "allocation_failed" {
                    0
                } else {
                    f.state() as u32
                },
                Ordering::SeqCst,
            );
            QUEUE_FREE.store(0, Ordering::SeqCst);
            QUEUE_ACCEPT.store(false, Ordering::SeqCst);
            for (name, va, sha) in [
                (
                    "dispatch",
                    0xd5e070,
                    "555c4324d0bd1de4e84a995634aecd0beeef301b7e932816163c4a15f73a07bc",
                ),
                (
                    "schedule",
                    0xef31b0,
                    "e2f6426df9c6d36c6259aa5d8a2e794a55890e6753cd7719c43b046242044def",
                ),
            ] {
                put(addr(va), &prepare_fixture_bytes(name, sha));
            }
            put(
                addr(0xe71fc0),
                &[
                    0x8b, 0xc1, 0x66, 0x0f, 0xef, 0xc0, 0x66, 0x0f, 0xd6, 0x00, 0x66, 0x0f, 0xd6,
                    0x40, 8, 0xc3,
                ],
            );
            put(addr(0xef30a0), &[0xc2, 4, 0]);
            put(addr(0xe87b70), &[0xc2, 4, 0]);
            jump(addr(0xef3020), queue_alloc as *const () as usize);
            jump(addr(0xe87aa0), queue_free as *const () as usize);
            put(f.base + 0x800, &[0xc3]);
            put(
                f.layout.state as usize + 0x6c,
                &((f.base + 0x800) as u32).to_le_bytes(),
            );
            put(
                f.layout.state as usize + 0x68,
                &(queue_init as *const () as u32).to_le_bytes(),
            );
            let q = prepare_fixture_bytes(
                "queue",
                "734c98c8bc4a20537cca44494c96e8b7ef65cbc3e08075ba17cd0843f568ec5f",
            );
            let at = addr(0xd75ca0);
            let mut q = queue_window(&q, at, queue_dispatch as *const () as usize).unwrap();
            for (off, target) in [(0xc, 0xe71fc0), (0x2e, 0xef3020)] {
                q[off + 1..off + 5].copy_from_slice(
                    &(addr(target) as u32)
                        .wrapping_sub((at + off + 5) as u32)
                        .to_le_bytes(),
                );
            }
            put(at, &q);
            PREP_ABILITY.with(|p| p.set(&f as *const Fixture as usize));
            let call: unsafe extern "thiscall" fn(usize, *const u32, u32) = std::mem::transmute(at);
            call(f.pawn.as_ptr() as usize, &123u32, 2);
            assert_eq!(QUEUE_ACCEPT.load(Ordering::SeqCst), case == "accepted");
            assert_eq!(
                layer[0x2c / 4],
                if case == "accepted" {
                    f.state() as u32
                } else {
                    0
                }
            );
            assert_eq!(
                QUEUE_FREE.load(Ordering::SeqCst),
                u32::from(matches!(
                    case,
                    "no_layer" | "other_layer_busy" | "disabled_layer" | "higher_priority"
                ))
            );
            println!(
                "R58_NATIVE_QUEUE case={case} accepted={} released={}",
                QUEUE_ACCEPT.load(Ordering::SeqCst),
                QUEUE_FREE.load(Ordering::SeqCst)
            );
        }
    }
}
#[test]
#[ignore = "requires native prepare/queue bytes"]
fn preparation_and_queue_signatures_fail_closed_on_every_changed_byte() {
    for (name, sha) in [
        (
            "activate",
            "9a26031908c6b43fd69e75aac7c6629090488066e5eb02e7a729188b84a87050",
        ),
        (
            "queue",
            "734c98c8bc4a20537cca44494c96e8b7ef65cbc3e08075ba17cd0843f568ec5f",
        ),
    ] {
        let b = prepare_fixture_bytes(name, sha);
        for i in 0..b.len() {
            let mut bad = b.clone();
            bad[i] ^= 1;
            let result = if name == "activate" {
                prepare_window(&bad, 0x100000, 0x200000)
            } else {
                queue_window(&bad, 0x100000, 0x200000)
            };
            assert!(result.is_none());
        }
    }
}
#[test]
#[ignore = "requires native exit fixture"]
fn confirmed_queue_failure_replacement_and_death_never_stick_in_enter() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let flow = ActionFlow::default();
        let request = f.request();
        f.park();
        let parked = f.run_exit(true, request, false).unwrap();
        flow.reserve(f.ability(), Phase::Parked(parked)).unwrap();
        f.ability[0x80 / 4] = 3f32.to_bits();
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| None),
            Press::Restricted
        );
        let mut layer = vec![0u32; 0x80 / 4];
        f.pawn[0x16e4 / 4] = layer.as_ptr() as u32;
        layer[0x2c / 4] = f.state() as u32;
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| Some((f.state(), 1))),
            Press::Queued
        );
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| panic!("duplicate")),
            Press::Queued
        );
        layer[0x2c / 4] = 0;
        flow.refresh(f.ability());
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| None),
            Press::Restricted
        );
        layer[0x2c / 4] = f.state() as u32;
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| Some((f.state(), 1))),
            Press::Queued
        );
        f.body[0x5dc / 4] = 0;
        flow.refresh(f.ability());
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| panic!("dead")),
            Press::Native
        );
    }
}

#[test]
#[ignore = "requires exact native exit fixture"]
fn confirmed_entry_requires_exact_queued_state_and_loss_signal_is_one_shot() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let flow = ActionFlow::default();
        let request = f.request();
        f.park();
        let parked = f.run_exit(true, request, false).unwrap();
        flow.reserve(f.ability(), Phase::Parked(parked)).unwrap();
        f.ability[0x80 / 4] = 3f32.to_bits();
        let mut layer = vec![0u32; 0x80 / 4];
        f.pawn[0x16e4 / 4] = layer.as_ptr() as u32;
        layer[0x2c / 4] = f.state() as u32;
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| Some((f.state(), 1))),
            Press::Queued
        );
        let other = f.state.clone();
        let mut called = false;
        flow.entry(other.as_ptr() as usize, f.ability(), || called = true);
        assert!(called);
        assert!(matches!(
            &flow.state.lock().unwrap().abilities[&f.ability()].phase,
            Phase::Enter(_)
        ));
        layer[0x2c / 4] = 0;
        assert!(flow.refresh(f.ability()));
        assert!(!flow.refresh(f.ability()));
        assert_eq!(
            flow.press_confirmed(
                f.ability(),
                |_| true,
                |_, _| {
                    flow.cancel(f.ability());
                    None
                }
            ),
            Press::Queued
        );
        assert_eq!(
            flow.press_confirmed(f.ability(), |_| true, |_, _| panic!("cancelled")),
            Press::Native
        );
    }
}
#[test]
fn unresolved_or_aliased_native_action_names_never_authorize_parking() {
    let flow = ActionFlow::default();
    unsafe {
        for (action, detonate, station) in [(0, 0, 2), (1, 1, 0), (9, 9, 9), (2, 1, 3)] {
            assert_eq!(
                flow.action(
                    0,
                    0,
                    action,
                    detonate,
                    station,
                    Layout::RETAIL,
                    |_| panic!("unknown"),
                    |n| n + 100
                ),
                action + 100
            );
        }
    }
}

#[test]
#[ignore = "requires native exit fixture"]
fn parked_hud_wait_uses_completed_exit_clock_and_preserves_charge() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let flow = ActionFlow::default();
        assert_eq!(flow.hud_wait(f.ability()), None);
        let request = f.request();
        f.park();
        let parked = f.run_exit(true, request, false).unwrap();
        flow.reserve(f.ability(), Phase::Parked(parked)).unwrap();
        let charge = f.ability[0x7c / 4];
        let smooth = f.ability[0x88 / 4];
        for (time, expected) in [
            (0.0f32, 3),
            (0.1, 3),
            (1.0, 2),
            (2.1, 1),
            (3.0, 0),
            (10.0, 0),
        ] {
            f.ability[0x80 / 4] = time.to_bits();
            assert_eq!(flow.hud_wait(f.ability()), Some(expected));
            assert_eq!(f.ability[0x7c / 4], charge);
            assert_eq!(f.ability[0x88 / 4], smooth);
        }
        f.ability[0x80 / 4] = f32::NAN.to_bits();
        assert_eq!(flow.hud_wait(f.ability()), None);
        f.ability[0x80 / 4] = (-1f32).to_bits();
        assert_eq!(flow.hud_wait(f.ability()), None);
        f.ability[0x80 / 4] = 1f32.to_bits();
        f.ability[0x98 / 4] = 1;
        assert_eq!(flow.hud_wait(f.ability()), None);
        f.ability[0x98 / 4] = 0;
        f.body[0x5dc / 4] = 0;
        assert_eq!(flow.hud_wait(f.ability()), None);
        println!("R58_HUD_WAIT native_clock=0,1,2.1,3 countdown=3,2,1,0 charge_and_segments=unchanged invalid_state=hidden");
    }
}

unsafe extern "thiscall" fn c9_existing_slot(_: usize, slot: u32) -> u32 {
    u32::from(matches!(slot, 39 | 41)) // native lookup leaf; no phantom slot creation
}
#[test]
#[ignore = "requires exact native exit and slot-select bytes"]
fn c9_retained_exit_restores_saved_weapon_before_reentry_and_never_overwrites_new_selection() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for phase in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let mut f = Fixture::new();
            let flow = ActionFlow::default();
            let mut inventory = vec![0u32; 0x80 / 4];
            inventory[0x30 / 4] = 0x28;
            f.pawn[0x1410 / 4] = inventory.as_ptr() as u32;
            f.ability[0xb4 / 4] = 39;
            let request = f.request();
            f.park();
            let parked = f.run_exit(true, request, false).unwrap();
            flow.reserve(f.ability(), Phase::Parked(parked)).unwrap();
            f.ability[0x40 / 4] = 0x49; // exact successful-exit flags observed in trial
            f.ability[0x80 / 4] = 3f32.to_bits();
            let select = native_fixture(
                &f,
                "c9_select",
                0xd762d0,
                "b3db4e060d6730b9881c61d3b1b13ee5afc2dce5a4df08c03a06baf0d4b0d12f",
            );
            jump(
                f.base + 0xd5fb10 - 0x400000,
                c9_existing_slot as *const () as usize,
            );
            let select: unsafe extern "thiscall" fn(usize, u32) = std::mem::transmute(select);
            if phase == "MODIFIED" {
                let (owner, slot) = flow.parked_weapon_restore(f.ability()).unwrap();
                select(owner, slot);
            }
            let mut layer = vec![0u32; 0x80 / 4];
            f.pawn[0x16e4 / 4] = layer.as_ptr() as u32;
            layer[0x2c / 4] = f.state() as u32;
            let result = flow.press_confirmed(
                f.ability(),
                |_| true,
                |_, _| {
                    if inventory[0x30 / 4] & 255 == 0x28 {
                        None
                    } else {
                        Some((f.state(), 1))
                    }
                },
            );
            assert_eq!(
                result,
                if phase == "MODIFIED" {
                    Press::Queued
                } else {
                    Press::Restricted
                }
            );
            // Preserve the stored weapon while queuing the same body.
            assert_eq!(
                inventory[0x30 / 4] & 255,
                if phase == "MODIFIED" { 39 } else { 40 }
            );
            assert_eq!(f.ability[0xb4 / 4], 39);
            assert_eq!(f.ability[0xa0 / 4], f.body() as u32);
            assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
            println!("R58_C9_SLOT {phase} selected={} reentry_slot_gate={} body_and_battery_unchanged=true result={result:?}",inventory[0x30/4]&255,phase=="MODIFIED");
            layer[0x2c / 4] = 0;
            flow.refresh(f.ability());
            inventory[0x30 / 4] = 41;
            assert!(flow.parked_weapon_restore(f.ability()).is_none());
            inventory[0x30 / 4] = 40;
            f.ability[0xb4 / 4] = 40;
            assert!(flow.parked_weapon_restore(f.ability()).is_none());
            f.ability[0xb4 / 4] = 39;
            flow.cancel(f.ability());
            assert!(flow.parked_weapon_restore(f.ability()).is_none());
        }
    }
}
#[test]
#[ignore = "requires exact retail deactivation bytes"]
fn c9_empty_parked_body_gets_native_fall_when_disable_has_no_control_state_receiver() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for phase in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            let mut f = Fixture::new();
            let mut inventory = vec![0u32; 0x80 / 4];
            inventory[0x30 / 4] = 39;
            f.pawn[0x1410 / 4] = inventory.as_ptr() as u32;
            f.ability[0xb4 / 4] = 39;
            f.body[0x4c / 4] = 1 << 8;
            f.body[0x910 / 4] = 1;
            f.ability[0x40 / 4] = 0x41;
            f.ability[0x7c / 4] = 0;
            let native = native_fixture(
                &f,
                "c9_deactivate",
                0xe58f00,
                "a9e544bc5692a1ccc4cbae39e9b384ce026e6af8bb930404e6368b270359e98c",
            );
            // Relocate the one action-name literal. Disable dispatch still runs,
            // but outside control it has no receiver, matching the real trial.
            std::ptr::write_unaligned(
                (native + 0x3f) as *mut u32,
                (f.base + 0x339f660 - 0x400000) as u32,
            );
            put(f.base + 0x55fbc0 - 0x400000, &[0xb8, 1, 0, 0, 0, 0xc3]);
            put(f.base + 0xd873f0 - 0x400000, &[0xc2, 12, 0]);
            put(f.base + 0xd762d0 - 0x400000, &[0xc2, 4, 0]);
            let native: unsafe extern "thiscall" fn(usize) = std::mem::transmute(native);
            FALLS.store(0, Ordering::SeqCst);
            if phase == "MODIFIED" {
                assert!(terminate_parked_with(
                    f.ability(),
                    f.layout,
                    |_| false,
                    |body| {
                        native_fall(body);
                        true
                    }
                ));
            }
            native(f.ability());
            assert_eq!(f.body[0x910 / 4], if phase == "MODIFIED" { 3 } else { 1 });
            assert_eq!(f.ability[0x7c / 4], 0);
            assert_eq!(FALLS.load(Ordering::SeqCst), u32::from(phase == "MODIFIED"));
            if phase == "MODIFIED" {
                assert!(!terminate_parked_with(
                    f.ability(),
                    f.layout,
                    |_| false,
                    |_| panic!("double fall")
                ));
            }
            println!(
                "R58_C9_EMPTY {phase} charge=0 state={} falls={} no_spurious_spawn=true",
                f.body[0x910 / 4],
                FALLS.load(Ordering::SeqCst)
            );
        }
    }
}
#[test]
#[ignore = "requires native fixture identity arena"]
fn c9_terminal_guard_retains_controlled_native_route_and_rejects_foreign_or_replaced_bodies() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        f.body[0x4c / 4] = 1 << 8;
        f.body[0x910 / 4] = 1;
        assert!(!terminate_parked_with(
            f.ability(),
            f.layout,
            |_| true,
            |_| panic!("native control owns Disable")
        ));
        for state in [0, 2, 3, 4] {
            f.body[0x910 / 4] = state;
            assert!(!terminate_parked_with(
                f.ability(),
                f.layout,
                |_| false,
                |_| panic!("wrong state")
            ));
        }
        f.body[0x910 / 4] = 1;
        for role in [0, 2, 4] {
            f.pawn[0x4c / 4] = role << 8;
            assert!(parked_body(f.ability(), f.layout).is_none());
        }
        f.pawn[0x4c / 4] = 1 << 8;
        f.body[0x9fc / 4] = 0;
        assert!(parked_body(f.ability(), f.layout).is_none());
        f.body[0x9fc / 4] = f.pawn.as_ptr() as u32;
        f.armor[0x5c / 4] = 0;
        assert!(parked_body(f.ability(), f.layout).is_none());
        f.armor[0x5c / 4] = f.ability() as u32;
        f.ability[0x28 / 4] = 2;
        assert!(parked_body(f.ability(), f.layout).is_none());
        f.ability[0x28 / 4] = 3;
        assert!(!terminate_parked_with(
            f.ability(),
            f.layout,
            |_| {
                *((f.ability() + 0xa0) as *mut u32) = 0;
                false
            },
            |_| panic!("changed during callback")
        ));
        assert!(parked_body(0, f.layout).is_none());
    }
}

static OWNER_BINDS: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn placement_test_bridge(body: usize, pos: *const u32, rot: *const u32) {
    reentry_placement_site(body, pos, rot, |p, r| transform_fixture(body, p, r));
}
unsafe extern "thiscall" fn placement_owner_test(body: usize, owner: usize) {
    assert_eq!(word(body, 0x9fc), Some(owner as u32));
    OWNER_BINDS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn placement_matrix_test(_: usize, out: *mut f32) -> *mut f32 {
    for i in 0..16 {
        *out.add(i) = if i % 5 == 0 { 1.0 } else { 0.0 };
    }
    out
}
#[test]
#[ignore = "requires exact retail body control and network activation fixtures"]
fn native_c10_delayed_control_start_preserves_both_placement_and_network_transform() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for (role, modified, want) in [
            ("BASELINE", false, 999f32),
            ("MODIFIED", true, 12f32),
            ("ROLLBACK", false, 999f32),
            ("FIRST_DEPLOYMENT", true, 999f32),
            ("CANCELLED", true, 999f32),
        ] {
            let mut f = Fixture::new();
            f.body[0x8c / 4] = f.pawn.as_ptr() as u32;
            f.pawn[0x5c / 4] = 0x800000;
            f.body[0x4c / 4] = 1 << 8;
            f.pawn[0x9c / 4] = 999f32.to_bits();
            f.pawn[0xac / 4] = 777;
            f.body[0xac / 4] = 123;
            f.ability[0x80 / 4] = 10f32.to_bits();
            let request = f.request();
            f.park();
            let ticket = f.run_exit(true, request, false).unwrap();
            f.ability[0x80 / 4] = 13f32.to_bits();
            let addr = |v: usize| f.base + v - 0x400000;
            let entry = native_fixture(
                &f,
                "body_control",
                0xebd910,
                "63b18350cdd8e59b5868e919daa2c6ce11b8aa455c7cf5e097ec2d1a12e24ba1",
            );
            let root = std::path::PathBuf::from(std::env::var("SCBL_R58_REENTRY_FIXTURE").unwrap());
            let bytes = std::fs::read(root.join("replication_activate.bin")).unwrap();
            let mut net = if true {
                replication_activate_window(
                    &bytes,
                    addr(0xfe1060),
                    transform_bridge as *const () as usize,
                )
                .unwrap()
            } else {
                bytes.clone()
            };
            for (off, target) in [(0x16, 0x7748c0usize), (0x4f, 0x7e24f0), (0x57, 0x7fc740)] {
                net[off + 1..off + 5].copy_from_slice(
                    &(addr(target) as u32)
                        .wrapping_sub((addr(0xfe1060) + off + 5) as u32)
                        .to_le_bytes(),
                );
            }
            put(addr(0xfe1060), &net);
            // Native activation tail copied exactly; node78=0 takes its immediate
            // active-flag path, while actual list/transport work stays a leaf.
            native_fixture(
                &f,
                "network_activate_tail",
                0x7fc740,
                "ff0f14a2216a400a605c47f305a9be02163fb720d94b02022f8df6bde47ff6b9",
            );
            jump(addr(0x7e24f0), dirty_fixture as *const () as usize);
            let mut node = vec![0u32; 0x400 / 4];
            node[0x14 / 4] = 1;
            node[0x88 / 4] = f.body() as u32;
            f.body[0x2c0 / 4] = node.as_ptr() as u32;
            for va in [0xad6c20, 0xaa8f00] {
                put(addr(va), &[0xc2, 4, 0]);
            }
            put(f.base + 0x120, &[0x31, 0xc0, 0xc3]);
            *((f.layout.owner as usize + 0x4f4) as *mut u32) = (f.base + 0x120) as u32;
            put(f.base + 0x140, &[0xc2, 20, 0]);
            *((f.layout.body as usize + 0x368) as *mut u32) = (f.base + 0x140) as u32;
            put(f.base + 0x160, &[0xc2, 4, 0]);
            *((f.layout.body as usize + 0x314) as *mut u32) = (f.base + 0x160) as u32;
            *((f.layout.body as usize + 0x43c) as *mut u32) = transform_fixture as *const () as u32;
            TRANSFORMS.store(0, Ordering::SeqCst);
            NETWORK_DIRTY.store(0, Ordering::SeqCst);
            // Exact MP placement and control-start byte windows. Only leaf
            // inventory/owner binding/camera/transform backends are isolated.
            let place = std::fs::read(root.join("mp_placement.bin")).unwrap();
            let mut place = mp_placement_window(
                &place,
                addr(0x1471810),
                placement_test_bridge as *const () as usize,
            )
            .unwrap();
            place[0x1a..0x1e].copy_from_slice(&(addr(0x3288bbc) as u32).to_le_bytes());
            place[0x1cb..0x1cf].copy_from_slice(
                &(addr(0x24c245a) as u32)
                    .wrapping_sub((addr(0x1471810) + 0x1cf) as u32)
                    .to_le_bytes(),
            );
            put(addr(0x1471810), &place);
            let start = std::fs::read(root.join("control_start.bin")).unwrap();
            let mut start = control_start_window(&start, addr(0x14749e0), addr(0x1471810)).unwrap();
            for (off, target) in [(0x18, 0xd5fb60), (0x68, 0x8b5e60), (0x83, 0x8bcd10)] {
                start[off + 1..off + 5].copy_from_slice(
                    &(addr(target) as u32)
                        .wrapping_sub((addr(0x14749e0) + off + 5) as u32)
                        .to_le_bytes(),
                );
            }
            put(addr(0x14749e0), &start);
            put(addr(0x24c245a), &[0xc3]); // isolated stack-cookie verifier
            put(addr(0x8bcd10), &[0xc2, 4, 0]); // camera leaf, native one-float ABI
            jump(addr(0xd5fb60), existing_body as *const () as usize);
            ENTRY_BODY.store(f.body() as u32, Ordering::SeqCst);
            f.pawn[0x1410 / 4] = f.ability() as u32; // inventory lookup is isolated
            let mut world = vec![0u32; 0x1100 / 4];
            let subsystem = vec![0u32; 0x400 / 4];
            world[0x10e0 / 4] = subsystem.as_ptr() as u32;
            f.body[0x144 / 4] = world.as_ptr() as u32;
            *((f.layout.body as usize + 0x198) as *mut u32) =
                placement_owner_test as *const () as u32;
            *((f.layout.owner as usize + 0xe8) as *mut u32) =
                placement_matrix_test as *const () as u32;
            *((f.layout.body as usize + 0x748) as *mut u32) = entry as u32;
            *((f.layout.state as usize + 0x27c) as *mut u32) = entry_setup as *const () as u32;
            f.body[0xb48 / 4] = 10f32.to_bits(); // retain native deployment offset calculation
            ENTRY_SETUP.store(0, Ordering::SeqCst);
            OWNER_BINDS.store(0, Ordering::SeqCst);
            let flow = ActionFlow::default();
            flow.reserve(f.ability(), Phase::Parked(ticket)).unwrap();
            let mut layer = vec![0u32; 0x80 / 4];
            f.pawn[0x16e4 / 4] = layer.as_ptr() as u32;
            layer[0x2c / 4] = f.state() as u32;
            assert_eq!(
                flow.press_confirmed(f.ability(), |_| true, |_, _| Some((f.state(), 1))),
                Press::Queued
            );
            layer[0x28 / 4] = f.state() as u32;
            layer[0x2c / 4] = 0;
            if !modified {
                // C9 consumed the scope here, before the delayed animation callback.
                flow.entry(f.state(), f.ability(), || {});
            }
            for _ in 0..8 {
                flow.refresh(f.ability());
            }
            if role == "FIRST_DEPLOYMENT" || role == "CANCELLED" {
                flow.cancel(f.ability());
            }
            let native: unsafe extern "thiscall" fn(usize) = std::mem::transmute(addr(0x14749e0));
            flow.entry(f.state(), f.ability(), || native(f.state()));
            assert_eq!(f.body[0x9c / 4], want.to_bits());
            assert_eq!(f.body[0xac / 4], if want == 12f32 { 123 } else { 777 });
            assert_eq!(NETWORK_DIRTY.load(Ordering::SeqCst), 1);
            assert_eq!(TRANSFORMS.load(Ordering::SeqCst), 2);
            assert_eq!(OWNER_BINDS.load(Ordering::SeqCst), 1);
            assert_eq!(ENTRY_SETUP.load(Ordering::SeqCst), 1);
            assert_eq!(*((node.as_ptr() as usize + 0x32d) as *const u8), 1);
            assert_eq!(*((node.as_ptr() as usize + 0x32e) as *const u8), 1);
            assert_eq!(f.body[0x910 / 4], 2);
            assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
            println!("R58_C10_POSITION {role} transform_x={want} native_transform_calls=2 owner_bind=1 camera_notify=1 dirty=1 network_active=1 battery_unchanged=true");
            // Outside that entry, this same call site returns to native inputs.
            transform_bridge(
                f.body(),
                f.pawn.as_ptr().add(0x9c / 4),
                f.pawn.as_ptr().add(0xa8 / 4),
            );
            assert_eq!(f.body[0x9c / 4], 999f32.to_bits());
        }
    }
}

#[test]
#[ignore = "requires exact retail placement and control-start fixtures"]
fn native_c10_control_start_and_placement_reject_all_byte_mutations() {
    let root = std::path::PathBuf::from(std::env::var("SCBL_R58_REENTRY_FIXTURE").unwrap());
    for name in ["control_start", "mp_placement"] {
        let bytes = std::fs::read(root.join(format!("{name}.bin"))).unwrap();
        let clone = |bytes: &[u8], at, bridge| {
            if name == "control_start" {
                control_start_window(bytes, at, bridge)
            } else {
                mp_placement_window(bytes, at, bridge)
            }
        };
        assert!(clone(&bytes, 0x100000, 0x200000).is_some());
        assert!(clone(&bytes, 0, 0x200000).is_none());
        assert!(clone(&bytes, 0x100000, 0).is_none());
        assert!(clone(&bytes, u32::MAX as usize - 3, 0x200000).is_none());
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 1;
            assert!(clone(&bad, 0x100000, 0x200000).is_none());
        }
    }
}
#[test]
#[ignore = "requires exact retail exit fixture"]
fn native_c10_placement_scope_uses_current_pose_and_two_independent_one_shots() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let request = f.request();
        f.park();
        let ticket = f.run_exit(true, request, false).unwrap();
        // Physics can move a parked body after exit; capture at actual activation.
        f.body[0x9c / 4] = 25f32.to_bits();
        let pos = [999f32.to_bits(), 0, 0];
        let rot = [777u32; 3];
        let placement = || reentry_placement_site(f.body(), pos.as_ptr(), rot.as_ptr(), |p, _| *p);
        let network = || reentry_position_site(f.body(), pos.as_ptr(), rot.as_ptr(), |p, _| *p);
        with_reentry_position(Some(&ticket), || {
            assert_eq!(placement(), 25f32.to_bits());
            assert_eq!(placement(), 999f32.to_bits());
            *((f.body() + 0x9c) as *mut u32) = 123f32.to_bits();
            *((f.body() + 0x910) as *mut u8) = 2;
            assert_eq!(network(), 25f32.to_bits());
            assert_eq!(network(), 999f32.to_bits());
        });
        assert_eq!(placement(), 999f32.to_bits());
        *((f.body() + 0x910) as *mut u8) = 1;
        let current = Cell::new(true);
        with_current_reentry_position(
            Some(&ticket),
            || current.get(),
            || {
                current.set(false);
                assert_eq!(placement(), 999f32.to_bits());
            },
        );
    }
}

#[test]
#[ignore = "requires native exit fixture"]
fn c12_standard_hud_event_stays_in_actual_exit_and_countdown_starts_after_return() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let flow = ActionFlow::default();
        let r = f.request().unwrap();
        flow.reserve(f.ability(), Phase::Exit(r)).unwrap();
        f.park();
        CLEANUP.store(0, Ordering::SeqCst);
        assert_eq!(flow.hud_wait(f.ability()), None);
        assert_eq!(CLEANUP.load(Ordering::SeqCst), 0);
        let native = f.prepare_exit(true);
        flow.exit(f.state(), f.ability(), |_| false, || native(f.state()));
        assert_eq!(CLEANUP.load(Ordering::SeqCst), 63);
        assert_eq!(flow.hud_wait(f.ability()), Some(3));
        println!("R58_C12_CLOSE early_standard_event=0 native_exit_cleanup=63 wait_after_exit=3 native_hud_event_not_bypassed=true");
    }
}
#[test]
#[ignore = "requires exact native preparation fixture"]
fn c11_selected_drone_keeps_previous_gadget_and_non_drone_selection_stays_native() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        for stage in ["BASELINE", "MODIFIED", "ROLLBACK"] {
            for selected in [7u32, 40] {
                let mut f = Fixture::new();
                let at = f.base + 0x3000;
                let raw = prepare_fixture_bytes(
                    "activate",
                    "9a26031908c6b43fd69e75aac7c6629090488066e5eb02e7a729188b84a87050",
                );
                let mut code = if stage == "MODIFIED" {
                    prepare_window(&raw, at, at + 0x500).unwrap()
                } else {
                    raw
                };
                // Execute the exact native slot-save branch, not the unrelated
                // world spawn. It preserves every byte above the slot byte.
                code[0xd0..0xd4].copy_from_slice(&[0x5e, 0xc2, 4, 0]);
                let mut start = vec![0x56, 0x8b, 0xf1, 0x8b, 0x44, 0x24, 8, 0xe9];
                start.extend_from_slice(&(0xcau32 - 12).to_le_bytes());
                code[..start.len()].copy_from_slice(&start);
                put(at, &code);
                f.ability[0xb4 / 4] = 0xface0027;
                let call: unsafe extern "thiscall" fn(usize, u32) = std::mem::transmute(at);
                call(f.ability(), selected);
                let expected = if selected == 40 && stage == "MODIFIED" {
                    39
                } else {
                    selected
                };
                assert_eq!(f.ability[0xb4 / 4], 0xface0000 | expected);
                assert_eq!(f.ability[0xa0 / 4], f.body() as u32);
                assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
                println!("R58_C11_SLOT {stage} selected={selected} remembered={expected} body_and_battery_unchanged=true");
            }
        }
    }
}

thread_local! { static C12_FLOW: Cell<(*const ActionFlow,usize,usize)> = const {Cell::new((std::ptr::null(),0,0))}; }
static C12_ANIM_CANCELS: AtomicU32 = AtomicU32::new(0);
static C12_SIGNAL: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn c12_cancel_animation(_: usize, id: u32) {
    assert_eq!(id, u32::MAX);
    C12_ANIM_CANCELS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "thiscall" fn c12_state_exit(state: usize) {
    C12_FLOW.with(|slot| {
        let (flow, a, entry) = slot.get();
        assert!(!flow.is_null());
        let native: unsafe extern "thiscall" fn(usize) = std::mem::transmute(entry);
        (*flow).exit(state, a, |_| false, || native(state));
    });
}
unsafe extern "thiscall" fn c12_signal(_: usize, _: usize, a: u32, b: u32) {
    assert_eq!((a, b), (0, 0));
    C12_SIGNAL.fetch_add(1, Ordering::SeqCst);
}
#[test]
#[ignore = "requires native exit and TransOut fixtures"]
fn c12_native_immediate_transout_completes_regular_exit_once_and_preserves_body() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let flow = ActionFlow::default();
        let request = f.request().unwrap();
        flow.reserve(f.ability(), Phase::Exit(request)).unwrap();
        f.park();
        let root = std::path::PathBuf::from(std::env::var("SCBL_R58_C12_FIXTURE").unwrap());
        let map = |va: usize| f.base + va - 0x400000;
        for (name, va, n, sha) in [
            (
                "transout",
                0x14c1ab0,
                311,
                "7585aff5c7b4ec3fc79cab2f82db31b2715073d1151c853622107bca947d3bb0",
            ),
            (
                "finish",
                0x14640b0,
                56,
                "727b0c72b741a64d1c43874f51f81c47a72e76c118652a0e0414005c6c3e1597",
            ),
            (
                "fast_exit",
                0x14f72e0,
                94,
                "d3a763d27f4a97b1d0244a0362a9ba35c3532f894dc52c373b425e16e6036e80",
            ),
            (
                "manager",
                0x14640f0,
                10,
                "93c1eaef3a936557e7570734d80c6de63d9c3a121f336f868236657a2423f9b3",
            ),
        ] {
            let bytes = std::fs::read(root.join(format!("{name}.bin"))).unwrap();
            assert_eq!(bytes.len(), n);
            assert_eq!(format!("{:x}", Sha256::digest(&bytes)), sha);
            // Every direct relative callee keeps the same relocation delta.
            // Fast path never reads TransOut's delayed-branch absolute operands.
            put(map(va), &bytes);
        }
        let mut manager = vec![0u32; 0x100 / 4];
        let mut vt = vec![0u32; 0x100 / 4];
        vt[0x90 / 4] = c12_cancel_animation as *const () as u32;
        manager[0] = vt.as_mut_ptr() as u32;
        f.pawn[0x13fc / 4] = manager.as_mut_ptr() as u32;
        *((f.layout.state as usize + 0xa4) as *mut u32) = map(0x14640b0) as u32;
        *((f.layout.state as usize + 0x98) as *mut u32) = c12_state_exit as *const () as u32;
        jump(map(0xd873f0), c12_signal as *const () as usize);
        let native = f.prepare_exit(true);
        C12_FLOW.with(|x| x.set((&flow, f.ability(), native as *const () as usize)));
        C12_ANIM_CANCELS.store(0, Ordering::SeqCst);
        C12_SIGNAL.store(0, Ordering::SeqCst);
        CLEANUP.store(0, Ordering::SeqCst);
        FALLS.store(0, Ordering::SeqCst);
        let transout: unsafe extern "thiscall" fn(usize, u32) = std::mem::transmute(map(0x14c1ab0));
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 1);
        transout(f.state(), 1);
        assert_eq!(C12_ANIM_CANCELS.load(Ordering::SeqCst), 1);
        assert_eq!(C12_SIGNAL.load(Ordering::SeqCst), 1);
        assert_eq!(CLEANUP.load(Ordering::SeqCst), 63);
        assert_eq!(FALLS.load(Ordering::SeqCst), 0);
        assert_eq!(f.state[0x30 / 4] & 1, 1);
        assert_eq!(f.state[0x138 / 4], 0);
        assert_eq!(f.body[0x910 / 4], 1);
        assert_eq!(f.body[0x9c / 4], 12f32.to_bits());
        assert_eq!(f.ability[0x7c / 4], 0.73f32.to_bits());
        assert_eq!(flow.hud_wait(f.ability()), Some(3));
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0);
        C12_FLOW.with(|x| x.set((std::ptr::null(), 0, 0)));
        println!("R58_C12_TRANSOUT native_immediate_branch=true cleanup=63 body_fall=0 camera_leaf=isolated wait=3 native_finish_flag=1 charge_pose_unchanged=true");
    }
}
#[test]
#[ignore = "requires native exit fixture"]
fn c12_fast_exit_selection_is_only_for_accepted_live_voluntary_park() {
    let _lock = LOCK.lock().unwrap();
    unsafe {
        let mut f = Fixture::new();
        let flow = ActionFlow::default();
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0);
        let r = f.request().unwrap();
        flow.reserve(f.ability(), Phase::Exit(r)).unwrap();
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0); // not parked yet
        f.park();
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 1);
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 7), 7);
        assert_eq!(flow.transout_mode(0, f.ability(), 0), 0);
        for (off, value) in [(0x60, 0x800), (0x4c, 2 << 8)] {
            let old = f.pawn[off / 4];
            f.pawn[off / 4] = value;
            assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0);
            f.pawn[off / 4] = old;
        }
        f.ability[0x7c / 4] = 0;
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0);
        f.ability[0x7c / 4] = 0.7f32.to_bits();
        f.body[0x5dc / 4] = 0;
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0);
        f.body[0x5dc / 4] = 600;
        flow.cancel(f.ability());
        assert_eq!(flow.transout_mode(f.state(), f.ability(), 0), 0);
    }
}

#[path = "drone_control_c18_tests.rs"]
mod c18;

#[path = "drone_control_hp0_tests.rs"]
mod c6_hp0;

#[path = "drone_disabled_tests.rs"]
mod c7_disabled;
