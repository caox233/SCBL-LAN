use super::*;
#[derive(Clone)]
struct Mem(Vec<u32>);
impl Mem {
    fn new(n: usize) -> Self {
        Self(vec![0; (n + 3) / 4])
    }
    fn p(&self) -> usize {
        self.0.as_ptr() as usize
    }
    fn w(&mut self, o: usize, v: u32) {
        self.0[o / 4] = v;
    }
    fn f(&mut self, o: usize, v: f32) {
        self.w(o, v.to_bits());
    }
    fn b(&mut self, o: usize, v: u8) {
        unsafe {
            *((self.0.as_mut_ptr() as usize + o) as *mut u8) = v;
        }
    }
    fn link(&mut self, o: usize, to: usize) {
        self.w(o, to as u32);
    }
    fn vec(&mut self, o: usize, to: usize, n: u32) {
        self.link(o, to);
        self.w(o + 4, n);
        self.w(o + 8, n);
    }
}
struct Player {
    pawn: Mem,
    pc: Mem,
    record: Mem,
    inv: Mem,
    bucket: Mem,
    buckets: Mem,
    items: Mem,
    gear: Mem,
    armor: Mem,
    peer: Mem,
}
impl Player {
    fn new(role: Role, team: u32, world: usize, vt: usize) -> Self {
        let mut s = Self {
            pawn: Mem::new(0x1414),
            pc: Mem::new(0x6e0),
            record: Mem::new(0x4c),
            inv: Mem::new(0x58),
            bucket: Mem::new(0x38),
            buckets: Mem::new(4),
            items: Mem::new(4),
            gear: Mem::new(0x748),
            armor: Mem::new(0x100),
            peer: Mem::new(0x78),
        };
        s.pawn.w(0, if role == Role::Spy { SPY } else { MERC });
        s.pawn
            .w(0x374, if role == Role::Spy { 0x40000 } else { 0x80000 });
        s.pawn.link(0x144, world);
        s.pawn.link(0x1410, s.inv.p());
        s.pawn.link(0xcf4, s.armor.p());
        s.pawn.link(0x2c0, s.peer.p());
        s.pc.link(0, vt);
        s.pc.link(0x520, s.pawn.p());
        s.pc.w(0x6dc, team);
        s.record.w(0x2c, team);
        s.record.link(0x48, s.pc.p());
        s.inv.vec(0x38, s.buckets.p(), 1);
        s.buckets.link(0, s.bucket.p());
        s.bucket.b(0x28, 0x29);
        s.bucket.vec(0x2c, s.items.p(), 1);
        s.items.link(0, s.gear.p());
        s.gear.w(0, ATS);
        s.gear.w(0x744, 1);
        s.armor.w(0, ARMOR);
        s.armor.f(0xfc, 1.5);
        s
    }
}
struct Fixture {
    world: Mem,
    world_info: Mem,
    info: Mem,
    vt: Mem,
    players: Vec<Player>,
    records: Mem,
    locals: Mem,
    weapon: Mem,
    props: Mem,
    logic: Mem,
    logic_props: Mem,
    attachments: Mem,
    attachment: Mem,
    attachment_props: Mem,
}
impl Fixture {
    fn new() -> Self {
        let mut s = Self {
            world: Mem::new(0x6d4),
            world_info: Mem::new(0x378),
            info: Mem::new(0x560),
            vt: Mem::new(0x434),
            players: vec![],
            records: Mem::new(12),
            locals: Mem::new(12),
            weapon: Mem::new(0x770),
            props: Mem::new(0x40),
            logic: Mem::new(0x30),
            logic_props: Mem::new(0x30),
            attachments: Mem::new(4),
            attachment: Mem::new(0x704),
            attachment_props: Mem::new(0x4c),
        };
        s.vt.w(0x430, 0xe5c960);
        s.world.link(0x6d0, s.world_info.p());
        s.world.f(0x5a0, 0.1);
        s.players = vec![
            Player::new(Role::Spy, 10, s.world.p(), s.vt.p()),
            Player::new(Role::Merc, 20, s.world.p(), s.vt.p()),
            Player::new(Role::Merc, 20, s.world.p(), s.vt.p()),
        ];
        for (i, p) in s.players.iter().enumerate() {
            s.records.link(i * 4, p.record.p());
        }
        for i in 0..3 {
            s.locals.link(i * 4, s.players[i].pc.p());
            s.players[i].pc.link(0x144, s.world.p());
            s.players[i].pc.w(0x60, 0x60000);
            s.players[i].pc.b(0x4d, if i == 1 { 1 } else { 2 });
        }
        s.world_info.vec(0x36c, s.locals.p(), 3);
        s.info.link(0x144, s.world.p());
        s.info.w(0x330, 0x10);
        s.info.vec(0x448, s.records.p(), 3);
        s.info.f(0x530, 5.);
        s.info.f(0x544, 100.);
        s.players[0].pawn.f(0x9c, 90.);
        s.players[1].gear.w(0, 0x2a96b0c);
        s.players[0].pawn.link(0x4b8, s.weapon.p());
        s.players[0].inv.link(0x50, s.weapon.p());
        s.weapon.link(0x744, s.props.p());
        s.props.b(0x3b, 1);
        s.props.b(0x38, 0x10);
        s.weapon.link(0x748, s.logic.p());
        s.logic.link(0x28, s.weapon.p());
        s.logic.link(0x2c, s.logic_props.p());
        s.weapon.vec(0x764, s.attachments.p(), 1);
        s.attachments.link(0, s.attachment.p());
        s.attachment.link(0x700, s.attachment_props.p());
        s.attachment_props.b(0x48, 4);
        s
    }
    unsafe fn facts(&self) -> Option<Facts> {
        collect(self.players[0].pawn.p(), self.info.p(), 12345)
    }
    fn sample(&self) -> Vec<u32> {
        let mut v = Vec::new();
        for m in [
            &self.world,
            &self.world_info,
            &self.info,
            &self.vt,
            &self.records,
            &self.locals,
            &self.weapon,
            &self.props,
            &self.logic,
            &self.logic_props,
            &self.attachments,
            &self.attachment,
            &self.attachment_props,
        ] {
            v.extend(&m.0);
        }
        for p in &self.players {
            for m in [
                &p.pawn, &p.pc, &p.record, &p.inv, &p.bucket, &p.buckets, &p.items, &p.gear,
                &p.armor, &p.peer,
            ] {
                v.extend(&m.0);
            }
        }
        v
    }
}
#[test]
fn detector_roster_copies_native_object_and_peer_ids_without_writes() {
    unsafe {
        let mut f = Fixture::new();
        for (i, p) in f.players.iter_mut().enumerate() {
            p.pawn.w(0x20, 100 + i as u32);
            p.peer.w(0x70, 10 + i as u32);
            p.peer.w(0x74, 2);
        }
        let current = frame_from(f.info.p()).unwrap();
        let before = f.sample();
        let result = detector_players_from(&current, &current).unwrap();
        assert_eq!(result, [(100, 0x20000000a, Role::Spy)]);
        assert_eq!(f.sample(), before);
        f.players[0].pawn.w(0x60, 0x800);
        assert_eq!(detector_players_from(&current, &current).unwrap().len(), 0);
    }
}
#[test]
fn detector_roster_rejects_recycled_duplicate_and_foreign_scene_ids() {
    unsafe {
        let mut f = Fixture::new();
        for (i, p) in f.players.iter_mut().enumerate() {
            p.pawn.w(0x20, 100 + i as u32);
            p.peer.w(0x70, 10 + i as u32);
        }
        let current = frame_from(f.info.p()).unwrap();
        f.players[2].record.w(0x2c, 10);
        f.players[2].pc.w(0x6dc, 10);
        f.players[2].peer.w(0x70, 10);
        assert!(detector_players_from(&current, &current).is_none());
        f.players[2].peer.w(0x70, 12);
        f.players[2].pawn.w(0x20, 100);
        assert!(detector_players_from(&current, &current).is_none());
        f.players[2].pawn.w(0x20, 102);
        let mut other = current;
        other.world += 4;
        assert!(detector_players_from(&current, &other).is_none());
    }
}
#[test]
fn detector_roster_excludes_same_team_even_if_role_is_spy() {
    unsafe {
        let mut f = Fixture::new();
        f.players[0].pawn.w(0x20, 42);
        f.players[0].peer.w(0x70, 100);
        f.players[0].record.w(0x2c, 20);
        f.players[0].pc.w(0x6dc, 20);
        let current = frame_from(f.info.p()).unwrap();
        assert!(detector_players_from(&current, &current)
            .unwrap()
            .is_empty());
    }
}
#[test]
fn actual_remote_ats_facts_do_not_require_local_viewer_equipment() {
    unsafe {
        let mut f = Fixture::new();
        f.players[1].pawn.f(0x9c, 10000.);
        let before = f.sample();
        let x = f.facts().unwrap();
        assert_eq!(x.listeners.len(), 1);
        assert_eq!(x.listeners[0].native_duration_ms, 7500);
        let mut m = Marks::new(x.world as u64, x.team);
        assert!(m.observe(x.shot, &x.listeners, 100));
        assert_eq!(
            m.visible(x.world as u64, 20, Role::Merc, 100)[0].expires_ms,
            7600
        );
        assert!(m.visible(x.world as u64, 10, Role::Spy, 100).is_empty());
        assert_eq!(before, f.sample());
    }
}
#[test]
fn real_operational_ats_and_current_team_world_life_are_required() {
    unsafe {
        for case in 0..8 {
            let mut f = Fixture::new();
            match case {
                0 => f.players[2].gear.w(0x744, 0),
                1 => f.players[2].gear.w(0, 0x2a96b0c),
                2 => {
                    f.players[2].record.w(0x2c, 30);
                    f.players[2].pc.w(0x6dc, 30);
                }
                3 => f.players[2].pawn.w(0x60, 1 << 11),
                4 => f.players[2].pawn.w(0x4b0, 0x80000000),
                5 => f.players[2].pawn.link(0x144, f.info.p()),
                6 => f.players[2].bucket.w(0x30, 0),
                _ => f.players[2].pc.w(0x6dc, 40),
            }
            let x = f.facts().unwrap();
            assert!(x.listeners.is_empty(), "{case}");
            assert!(!Marks::new(x.world as u64, x.team).observe(x.shot, &x.listeners, 100));
        }
    }
}
#[test]
fn weapon_suppression_matches_inherent_and_installed_native_paths() {
    unsafe {
        let mut f = Fixture::new();
        assert_eq!(suppressed(f.weapon.p(), 12345), Some(true));
        f.attachment_props.b(0x48, 0);
        assert_eq!(suppressed(f.weapon.p(), 12345), Some(false));
        f.attachment_props.w(0x2c, 12345);
        assert_eq!(suppressed(f.weapon.p(), 12345), Some(true));
        f.attachment_props.w(0x2c, 987);
        f.logic_props.w(0x2c, 0x100);
        assert_eq!(suppressed(f.weapon.p(), 12345), Some(true));
        f.logic.w(0x28, 0);
        assert_eq!(suppressed(f.weapon.p(), 12345), None);
    }
}
#[test]
fn native_slot_keeps_mac11_pistol_and_shock_exemptions() {
    unsafe {
        let mut f = Fixture::new();
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::SuppressedPrimary);
        f.props.b(0x3b, 2);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::Pistol);
        f.props.b(0x3b, 1);
        f.props.b(0x38, 0x21);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::ShockCrossbow);
        f.props.b(0x38, 0x10);
        f.attachment_props.b(0x48, 0);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::Unsuppressed);
        f.props.b(0x3b, 0);
        assert!(f.facts().is_none());
    }
}
#[test]
fn corrupt_sources_and_ambiguous_rosters_fail_closed_without_writes() {
    unsafe {
        assert!(collect(0, 0, 0).is_none());
        for case in 0..8 {
            let mut f = Fixture::new();
            match case {
                0 => f.info.w(0x44c, 33),
                1 => f.players[0].pc.b(0x4d, 1),
                2 => f.world.f(0x5a0, f32::NAN),
                3 => f.players[0].pawn.f(0x9c, f32::INFINITY),
                4 => f.info.w(0x330, 0),
                5 => f.records.link(8, f.players[1].record.p()),
                6 => f.players[0].pawn.w(0x60, 1 << 11),
                _ => f.players[0].pawn.w(0x4b8, 0),
            }
            let before = f.sample();
            assert!(f.facts().is_none(), "{case}");
            assert_eq!(before, f.sample());
        }
    }
}
#[test]
fn native_timing_and_multiple_eligible_detectors_do_not_use_guessed_five_seconds() {
    unsafe {
        let mut f = Fixture::new();
        f.players[1].gear.w(0, ATS);
        f.players[1].armor.f(0xfc, 2.);
        let x = f.facts().unwrap();
        let mut m = Marks::new(x.world as u64, x.team);
        assert!(m.observe(x.shot, &x.listeners, 100));
        assert_eq!(
            m.visible(x.world as u64, x.team, Role::Merc, 100)[0].expires_ms,
            10100
        );
        // Ineligible/disabled detector cannot extend an actual short detection.
        f.players[1].gear.w(0x744, 0);
        f.players[2].armor.f(0xfc, 0.5);
        let x = f.facts().unwrap();
        let mut m = Marks::new(x.world as u64, x.team);
        assert!(m.observe(x.shot, &x.listeners, 100));
        assert_eq!(
            m.visible(x.world as u64, x.team, Role::Merc, 100)[0].expires_ms,
            2600
        );
    }
}
#[test]
fn retained_snapshot_does_not_follow_actor_or_reuse_previous_round() {
    unsafe {
        let mut f = Fixture::new();
        let x = f.facts().unwrap();
        let mut s = State {
            world: x.world,
            info: x.info,
            local: x.local,
            team: x.team,
            last: 100,
            serial: 0,
            marks: Marks::new(x.world as u64, x.team),
        };
        assert!(s.feed(x.clone()));
        f.players[0].pawn.f(0x9c, 40.);
        assert_eq!(
            s.marks.visible(x.world as u64, 20, Role::Merc, 100)[0].position[0],
            90.
        );
        // A new actual shot, not a render tick, replaces the immutable snapshot.
        f.world.f(0x5a0, 0.2);
        assert!(s.feed(f.facts().unwrap()));
        assert_eq!(
            s.marks.visible(x.world as u64, 20, Role::Merc, 200)[0].position[0],
            40.
        );
        f.world.f(0x5a0, 0.05);
        let mut next = f.facts().unwrap();
        next.listeners.clear();
        assert!(!s.feed(next));
        assert!(s
            .marks
            .visible(x.world as u64, 20, Role::Merc, 50)
            .is_empty());
    }
}
#[test]
fn native_duration_conversion_rejects_nan_and_overflow() {
    for x in [f32::NAN, f32::INFINITY, -1., f32::MAX] {
        assert_eq!(milliseconds(x), None);
    }
    assert_eq!(milliseconds(7.5), Some(7500));
    assert_eq!(milliseconds(0.), Some(0));
}

#[test]
#[ignore = "requires checked retail bytes; run with SCBL_R58_C28_FIXTURE"]
fn bounded_getters_agree_with_executed_native_inventory_and_suppressor_code() {
    unsafe {
        use sha2::{Digest, Sha256};
        #[link(name = "kernel32")]
        extern "system" {
            fn VirtualAlloc(
                p: *mut std::ffi::c_void,
                n: usize,
                kind: u32,
                protection: u32,
            ) -> *mut std::ffi::c_void;
            fn VirtualProtect(
                p: *mut std::ffi::c_void,
                n: usize,
                protection: u32,
                old: *mut u32,
            ) -> i32;
            fn VirtualFree(p: *mut std::ffi::c_void, n: usize, kind: u32) -> i32;
            fn FlushInstructionCache(
                p: *mut std::ffi::c_void,
                address: *const std::ffi::c_void,
                n: usize,
            ) -> i32;
        }
        let dir = std::path::PathBuf::from(std::env::var("SCBL_R58_C28_FIXTURE").unwrap());
        let read = |name: &str, sha: &str| {
            let b = std::fs::read(dir.join(name)).unwrap();
            assert_eq!(format!("{:x}", Sha256::digest(&b)), sha);
            b
        };
        let inv = read(
            "getter-inventory.bin",
            "f811eb09bb06acbbb543a16d22036c4417326f660525449237ee534bd2854d9b",
        );
        let mut weapon = read(
            "getter-weapon-suppressed.bin",
            "3f27ba87598b5f3653f956d322e3be51bd81a514f2d04811bcdd2d3a97c008df",
        );
        let mut suppress = read(
            "getter-logic-suppressed.bin",
            "a1da17de7f25336c79f2151f0ac3ca2f1734fbbd832caaa1e9cfa2e2d4ecaa8c",
        );
        let mem = VirtualAlloc(std::ptr::null_mut(), 4096, 0x3000, 4) as usize;
        assert_ne!(mem, 0);
        struct Free(usize);
        impl Drop for Free {
            fn drop(&mut self) {
                unsafe {
                    VirtualFree(self.0 as _, 0, 0x8000);
                }
            }
        }
        let _free = Free(mem);
        assert_eq!(weapon[10], 0xe9);
        weapon[11..15].copy_from_slice(
            &((mem + 0x200) as u32)
                .wrapping_sub((mem + 0x100 + 15) as u32)
                .to_le_bytes(),
        );
        assert_eq!(&suppress[0x32..0x36], &0x33a039cu32.to_le_bytes());
        suppress[0x32..0x36].copy_from_slice(&((mem + 0x300) as u32).to_le_bytes());
        std::ptr::copy_nonoverlapping(inv.as_ptr(), mem as *mut u8, inv.len());
        std::ptr::copy_nonoverlapping(weapon.as_ptr(), (mem + 0x100) as *mut u8, weapon.len());
        std::ptr::copy_nonoverlapping(suppress.as_ptr(), (mem + 0x200) as *mut u8, suppress.len());
        *((mem + 0x300) as *mut u32) = 12345;
        let mut old = 0;
        assert_ne!(VirtualProtect(mem as _, 4096, 0x20, &mut old), 0);
        FlushInstructionCache(usize::MAX as _, mem as _, 4096);
        let lookup: unsafe extern "thiscall" fn(usize, u32) -> usize = std::mem::transmute(mem);
        let native_suppressed: unsafe extern "thiscall" fn(usize) -> u8 =
            std::mem::transmute(mem + 0x100);
        let mut f = Fixture::new();
        let before = f.sample();
        for _ in 0..1024 {
            assert_eq!(
                lookup(f.players[2].inv.p(), 0x29),
                ats_gear(f.players[2].pawn.p()).unwrap()
            );
            assert_eq!(
                native_suppressed(f.weapon.p()) != 0,
                suppressed(f.weapon.p(), 12345).unwrap()
            );
        }
        assert_eq!(before, f.sample());
        for (flags, inherent, name) in [(0, 0, 987), (0, 0, 12345), (4, 0, 987), (0, 0x100, 987)] {
            f.attachment_props.b(0x48, flags);
            f.logic_props.w(0x2c, inherent);
            f.attachment_props.w(0x2c, name);
            assert_eq!(
                native_suppressed(f.weapon.p()) != 0,
                suppressed(f.weapon.p(), 12345).unwrap()
            );
        }
        println!("C28_NATIVE_GETTERS inventory=80B suppressor=18B+99B native_calls=2052 read_only=true source_slot_exemptions=separate shared_render=false");
    }
}

#[test]
fn fresh_frame_uses_local_living_merc_and_never_requires_personal_ats() {
    let mut f = Fixture::new();
    f.info.f(0x534, 1.25);
    let old = f.sample();
    let a = unsafe { frame_from(f.info.p()) }.unwrap();
    assert_eq!(
        (a.local, a.world, a.team, a.now, a.distance_delay_ms),
        (f.players[1].pawn.p(), f.world.p(), 20, 100, 1250)
    );
    assert_eq!(old, f.sample());
    f.players[1].pawn.w(0x60, 1 << 11);
    assert!(unsafe { frame_from(f.info.p()) }.is_none());
    f.players[1].pawn.w(0x60, 0);
    // Switch actual local-control ownership, not array ordering.
    f.players[1].pc.b(0x4d, 2);
    f.players[0].pc.b(0x4d, 1);
    assert!(unsafe { frame_from(f.info.p()) }.is_none());
}
#[test]
fn fresh_frame_rejects_invalid_scene_role_position_clock_and_roster() {
    let mut f = Fixture::new();
    f.info.link(0x144, 0);
    assert!(unsafe { frame_from(f.info.p()) }.is_none());
    f.info.link(0x144, f.world.p());
    f.players[1].pawn.f(0x9c, f32::NAN);
    assert!(unsafe { frame_from(f.info.p()) }.is_none());
    f.players[1].pawn.f(0x9c, 0.);
    f.world.f(0x5a0, -1.);
    assert!(unsafe { frame_from(f.info.p()) }.is_none());
    f.world.f(0x5a0, 1.);
    f.info.w(0x330, 0);
    assert!(unsafe { frame_from(f.info.p()) }.is_none());
}

#[test]
fn c55_remote_current_weapon_does_not_require_owner_inventory_slots() {
    unsafe {
        let mut f = Fixture::new();
        f.players[0].inv.w(0x50, 0);
        f.players[0].inv.w(0x54, 0);
        let before = f.sample();
        let x = f.facts().unwrap();
        assert_eq!(x.shot.weapon, Weapon::SuppressedPrimary);
        let mut marks = Marks::new(x.world as u64, x.team);
        assert!(marks.observe(x.shot, &x.listeners, 100));
        f.players[0].pawn.f(0x9c, 2000.0);
        assert_eq!(
            marks.visible(x.world as u64, x.team, Role::Merc, 101)[0].position[0],
            90.0
        );
        f.players[0].pawn.f(0x9c, 90.0);
        assert_eq!(before, f.sample());
        f.props.b(0x3b, 2);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::Pistol);
        f.props.b(0x3b, 1);
        f.props.b(0x38, 0x21);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::ShockCrossbow);
    }
}
#[test]
fn c55_new_suppressed_mark_range_never_exceeds_footsteps_20m() {
    unsafe {
        let mut f = Fixture::new();
        f.info.f(0x544, 4000.0);
        for (distance, expected) in [(1999.0, true), (2000.0, true), (2000.01, false)] {
            f.players[0].pawn.f(0x9c, distance);
            let x = f.facts().unwrap();
            let mut m = Marks::new(x.world as u64, x.team);
            assert_eq!(m.observe(x.shot, &x.listeners, 100), expected);
        }
    }
}

#[test]
fn maintenance_installed_suppressor_survives_exact_null_logic_without_guessing() {
    unsafe {
        let mut f = Fixture::new();
        f.weapon.w(0x748, 0);
        let before = f.sample();
        assert_eq!(suppressed(f.weapon.p(), 12345), Some(true));
        assert_eq!(f.sample(), before);
        f.attachment_props.b(0x48, 0);
        f.attachment_props.w(0x2c, 12345);
        assert_eq!(suppressed(f.weapon.p(), 12345), Some(true));
        f.attachment_props.w(0x2c, 987);
        assert_eq!(suppressed(f.weapon.p(), 12345), None);
        f.attachment_props.b(0x48, 4);
        f.weapon.w(0x748, 1); // malformed non-null logic is not pending
        assert_eq!(suppressed(f.weapon.p(), 12345), None);
        f.weapon.link(0x748, f.logic.p());f.logic.w(0x28, 0);
        assert_eq!(suppressed(f.weapon.p(), 12345), None);
    }
}
#[test]
fn maintenance_null_logic_keeps_pistol_exemption_and_primary_fixed_position_rule() {
    unsafe {
        let mut f = Fixture::new();
        f.weapon.w(0x748, 0);
        f.props.b(0x3b, 2);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::Pistol);
        f.props.b(0x3b, 1);
        assert_eq!(f.facts().unwrap().shot.weapon, Weapon::SuppressedPrimary);
        f.attachment_props.b(0x48, 0);
        assert!(f.facts().is_none());
    }
}

#[test]
fn maintenance_native_ats_override_requires_current_opposing_live_spy() {
    unsafe {
        for kind in 0..11 {
            let mut f = Fixture::new();
            f.weapon.link(0x8c, f.players[0].pawn.p());
            f.weapon.w(0x748, 0);f.props.b(0x3b, 2);
            assert_eq!(suppression_for_ats(f.weapon.p(), f.info.p(),12345),Some(true));
            match kind {
                0 => f.info.w(0x330, 0),
                1 => f.players[0].pawn.w(0x60, 1<<11),
                2 => f.players[0].pawn.w(0x4b0, 0x80000000),
                3 => f.players[0].pawn.w(0x4b8, 0),
                4 => f.weapon.w(0x8c, 0),
                5 => {f.players[0].record.w(0x2c,20);f.players[0].pc.w(0x6dc,20);},
                6 => f.players[0].pawn.link(0x144, f.info.p()),
                7 => {f.players[1].pawn.w(0,SPY);f.players[1].pawn.w(0x374,0x40000);},
                8 => f.props.b(0x3b, 0),
                9 => f.info.w(0x44c, 33),
                _ => f.players[1].pawn.w(0x60, 1<<11),
            }
            let before=f.sample();
            assert_eq!(suppression_for_ats(f.weapon.p(),f.info.p(),12345),None,"{kind}");
            assert_eq!(f.sample(),before);
        }
        let mut f=Fixture::new();f.weapon.link(0x8c,f.players[0].pawn.p());
        assert_eq!(suppression_for_ats(f.weapon.p(),f.info.p(),0),None);
    }
}

#[test]
#[ignore = "executes checked retail ATS callback bytes; requires SCBL_R58_C28_FIXTURE"]
fn maintenance_native_ats_callsite_classifies_installed_suppressor_and_rolls_back() {
    use sha2::{Digest,Sha256};
    use std::sync::atomic::{AtomicUsize,Ordering};
    static GETTER:AtomicUsize=AtomicUsize::new(0);
    static INFO:AtomicUsize=AtomicUsize::new(0);
    static KIND:AtomicUsize=AtomicUsize::new(99);
    static EVENTS:AtomicUsize=AtomicUsize::new(0);
    static CALLS:AtomicUsize=AtomicUsize::new(0);
    unsafe extern "thiscall" fn emit(bus:usize,event:*const u32,extra:u32) {
        assert_eq!(bus,0x339b7d8);assert_eq!(extra,0);assert_eq!(*event,1);assert_eq!(*event.add(3),1);
        EVENTS.fetch_add(1,Ordering::Relaxed);KIND.store(*event.add(2) as usize,Ordering::Relaxed);
    }
    unsafe extern "thiscall" fn classified(weapon:usize)->u8 {
        CALLS.fetch_add(1,Ordering::Relaxed);
        let native:unsafe extern "thiscall" fn(usize)->u8=std::mem::transmute(GETTER.load(Ordering::Relaxed));
        crate::ats_suppressor::repair_result(weapon,native(weapon),Some(INFO.load(Ordering::Relaxed)),Some(12345))
    }
    #[link(name="kernel32")] extern "system" {
        fn VirtualAlloc(p:*mut std::ffi::c_void,n:usize,t:u32,protect:u32)->*mut std::ffi::c_void;
        fn VirtualProtect(p:*mut std::ffi::c_void,n:usize,protect:u32,old:*mut u32)->i32;
        fn VirtualFree(p:*mut std::ffi::c_void,n:usize,t:u32)->i32;
        fn FlushInstructionCache(process:*mut std::ffi::c_void,p:*const std::ffi::c_void,n:usize)->i32;
    }
    unsafe {
        let dir=std::path::PathBuf::from(std::env::var("SCBL_R58_C28_FIXTURE").unwrap());
        let read=|name:&str,n:usize,hash:&str| {
            let data=std::fs::read(dir.join(name)).unwrap()[..n].to_vec();
            assert_eq!(format!("{:x}",Sha256::digest(&data)),hash);data
        };
        let mut getter=read("getter-weapon-suppressed.bin",18,"3f27ba87598b5f3653f956d322e3be51bd81a514f2d04811bcdd2d3a97c008df");
        let mut logic=read("getter-logic-suppressed.bin",99,"a1da17de7f25336c79f2151f0ac3ca2f1734fbbd832caaa1e9cfa2e2d4ecaa8c");
        let mut bats=read("native-00b105a0.bin",110,"d49bd282485b2feffe6a76c7932f22d0d8c5e2721ce2c81d953c15a0fc9fc81a");
        let family=read("native-00dd51d0.bin",11,"d5d7dd5cb4c18ee6fa10149baf6af2862300e0a12ae5789706f2d67989feac80");
        let mem=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;assert_ne!(mem,0);
        struct Free(usize);impl Drop for Free {fn drop(&mut self){unsafe {VirtualFree(self.0 as _,0,0x8000);}}}
        let _free=Free(mem);
        getter[11..15].copy_from_slice(&((mem+0x200) as u32).wrapping_sub((mem+0x100+15) as u32).to_le_bytes());
        logic[0x32..0x36].copy_from_slice(&((mem+0x300) as u32).to_le_bytes());
        for (off,target) in [(0x22,mem+0x100),(0x31,mem+0x500),(0x61,emit as usize)] {
            assert_eq!(bats[off],0xe8);
            bats[off+1..off+5].copy_from_slice(&(target as u32).wrapping_sub((mem+0x400+off+5) as u32).to_le_bytes());
        }
        for (off,data) in [(0x100,getter.as_slice()),(0x200,logic.as_slice()),(0x400,bats.as_slice()),(0x500,family.as_slice())] {
            std::ptr::copy_nonoverlapping(data.as_ptr(),(mem+off) as *mut u8,data.len());
        }
        *((mem+0x300) as *mut u32)=12345;
        let mut protect=0;assert_ne!(VirtualProtect(mem as _,4096,0x20,&mut protect),0);
        assert_ne!(FlushInstructionCache(usize::MAX as _,mem as _,4096),0);
        GETTER.store(mem+0x100,Ordering::Relaxed);
        let call:unsafe extern "thiscall" fn(usize,usize)=std::mem::transmute(mem+0x400);
        let mut f=Fixture::new();f.weapon.link(0x8c,f.players[0].pawn.p());f.props.b(0x3b,2);f.weapon.w(0x748,0);
        INFO.store(f.info.p(),Ordering::Relaxed);
        let fire=|f:&Fixture| {
            EVENTS.store(0,Ordering::Relaxed);KIND.store(99,Ordering::Relaxed);
            call(f.players[1].gear.p(),f.players[0].pawn.p());
            assert_eq!(EVENTS.load(Ordering::Relaxed),1);KIND.load(Ordering::Relaxed)
        };
        assert_eq!(fire(&f),0);
        println!("ATS_NATIVE_BASELINE pistol=true installed_suppressor=true logic_missing=true event=0 (unsuppressed)");
        let at=mem+0x422;let old=bats[0x22..0x27].to_vec();let mut new=old.clone();
        new[1..].copy_from_slice(&(classified as usize as u32).wrapping_sub((at+5) as u32).to_le_bytes());
        assert!(crate::equipment_native::patch(at,&old,&new));
        for (label,flags,name) in [("flag",4,987),("fname",0,12345)] {
            f.attachment_props.b(0x48,flags);f.attachment_props.w(0x2c,name);
            let before=f.sample();CALLS.store(0,Ordering::Relaxed);
            for _ in 0..1024 {assert_eq!(fire(&f),1);}
            assert_eq!(CALLS.load(Ordering::Relaxed),1024);assert_eq!(f.sample(),before);
            println!("ATS_NATIVE_MODIFIED suppressor={} event=1 (suppressed) calls=1024 events_per_call=1 actor_writes=false",label);
        }
        f.attachment_props.w(0x2c,987);assert_eq!(fire(&f),0); // unconfirmed stays native
        f.attachment_props.b(0x48,4);f.info.w(0x330,0);assert_eq!(fire(&f),0);
        f.info.w(0x330,0x10);f.weapon.w(0x8c,0);assert_eq!(fire(&f),0);
        f.weapon.link(0x8c,f.players[0].pawn.p());assert_eq!(fire(&f),1);
        assert!(crate::equipment_native::patch(at,&new,&old));assert_eq!(fire(&f),0);
        assert_eq!(std::slice::from_raw_parts((mem+0x400) as *const u8,bats.len()),bats.as_slice());
        println!("ATS_NATIVE_ROLLBACK event=0 exact_callback_bytes=true stack_balanced=true live_game=false");
    }
}
