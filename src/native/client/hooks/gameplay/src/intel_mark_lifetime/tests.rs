use super::*;

#[test]
fn c50_rf_requires_actual_surviving_cast_targets_not_equipment_or_clock() {
    let mut a = Fixture::new();
    let mut b = Fixture::new();
    b.merc_peer[0x70 / 4] = 51;
    let mut records = vec![];
    let pawn = a.spy.as_ptr() as usize;
    unsafe {
        assert!(!has_targets(&records, pawn)); // Empty cast, even native skill active.
        a.observe(&mut records);
        assert!(has_targets(&records, pawn));
        let s = a.source();
        b.marks[3] = s.controller.address as u32;
        record_add(
            &mut records,
            Some(s),
            b.target(),
            11,
            12,
            s.controller.address,
        );
        assert_eq!(records.len(), 2);
        a.merc[0x1a94 / 4] = 0;
        assert!(has_targets(&records, pawn));
        b.merc[0x1a94 / 4] = 0;
        assert!(!has_targets(&records, pawn));
        // Same source's unrelated gadget mark must not count as Intel.
        a.merc[0x1a94 / 4] = 2;
        a.observe(&mut records);
        record_add(&mut records, None, a.target(), 11, 12, s.controller.address);
        assert!(!has_targets(&records, pawn));
        a.observe(&mut records);
        a.merc[0x60 / 4] |= 0x800;
        assert!(!has_targets(&records, pawn));
        a.merc[0x60 / 4] = 0;
        a.spy[0x60 / 4] |= 0x800;
        assert!(!has_targets(&records, pawn));
        a.spy[0x60 / 4] = 0;
        assert!(has_targets(&records, pawn));
        a.ability[0x40 / 4] = 0;
        assert!(
            has_targets(&records, pawn),
            "Disruptor cannot cancel surviving mark RF"
        );
        a.merc_peer[0x70 / 4] += 1;
        assert!(!has_targets(&records, pawn));
    }
    println!("C50_INTEL empty=off one=on two=on last_expired=off unrelated=off death=off disrupt_with_mark=on");
}

struct Fixture {
    spy: Vec<u32>,
    merc: Vec<u32>,
    controller: Vec<u32>,
    ability: Vec<u32>,
    spy_peer: Vec<u32>,
    merc_peer: Vec<u32>,
    controller_peer: Vec<u32>,
    marks: Vec<u32>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            spy: vec![0; 0x1b00 / 4],
            merc: vec![0; 0x1b00 / 4],
            controller: vec![0; 0x540 / 4],
            ability: vec![0; 0xc0 / 4],
            spy_peer: vec![0; 40],
            merc_peer: vec![0; 40],
            controller_peer: vec![0; 40],
            marks: vec![20.0f32.to_bits(), 11, 12, 0, 30.0f32.to_bits(), 21, 22, 99],
        };
        f.spy[0] = SPY;
        f.merc[0] = MERC;
        f.spy[0x5c / 4] = 0x800000;
        f.controller[0x60 / 4] = 0x40000;
        f.spy_peer[0x70 / 4] = 11;
        f.spy_peer[0x74 / 4] = 12;
        f.merc_peer[0x70 / 4] = 31;
        f.merc_peer[0x74 / 4] = 32;
        f.controller_peer[0x70 / 4] = 41;
        f.controller_peer[0x74 / 4] = 42;
        f.controller_peer[0x78 / 4] = 1;
        f.spy[0x2c0 / 4] = f.spy_peer.as_mut_ptr() as u32;
        f.merc[0x2c0 / 4] = f.merc_peer.as_mut_ptr() as u32;
        f.controller[0x2c0 / 4] = f.controller_peer.as_mut_ptr() as u32;
        f.spy[0x358 / 4] = f.controller.as_mut_ptr() as u32;
        f.ability[0x6c / 4] = f.spy.as_mut_ptr() as u32;
        f.marks[3] = f.controller.as_mut_ptr() as u32;
        f.merc[0x1a90 / 4] = f.marks.as_mut_ptr() as u32;
        f.merc[0x1a94 / 4] = 2;
        f.merc[0x1a98 / 4] = 2;
        f.merc[0x1a08 / 4] = 0x800;
        f
    }
    fn source(&self) -> Source {
        unsafe { source(self.ability.as_ptr() as usize).unwrap() }
    }
    fn target(&self) -> usize {
        self.merc.as_ptr() as usize
    }
    fn observe(&self, records: &mut Vec<Record>) {
        let s = self.source();
        unsafe {
            record_add(
                records,
                Some(s),
                self.target(),
                11,
                12,
                s.controller.address,
            );
        }
    }
}

#[test]
fn death_selects_only_cast_owned_key_once_without_revival_restore() {
    let mut f = Fixture::new();
    let mut records = vec![];
    f.observe(&mut records);
    unsafe {
        assert!(take_dead(&mut records, f.target()).is_empty());
    }
    f.spy[0x60 / 4] |= 0x800;
    let pending = unsafe { take_dead(&mut records, f.target()) };
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (pending[0].source.pawn.low, pending[0].source.pawn.high),
        (11, 12)
    );
    assert_eq!(f.marks[4..], [30.0f32.to_bits(), 21, 22, 99]);
    assert!(records.is_empty());
    f.spy[0x60 / 4] &= !0x800;
    unsafe {
        assert!(take_dead(&mut records, f.target()).is_empty());
    }
}
#[test]
fn another_spy_and_same_low_different_high_stay_separate() {
    let mut a = Fixture::new();
    let mut b = Fixture::new();
    b.spy_peer[0x70 / 4] = 11;
    b.spy_peer[0x74 / 4] = 13;
    a.marks[5] = 11;
    a.marks[6] = 13;
    a.marks[7] = b.controller.as_mut_ptr() as u32;
    let mut records = vec![];
    a.observe(&mut records);
    unsafe {
        record_add(
            &mut records,
            Some(b.source()),
            a.target(),
            11,
            13,
            b.source().controller.address,
        );
    }
    a.spy[0x60 / 4] |= 0x800;
    let pending = unsafe { take_dead(&mut records, a.target()) };
    assert_eq!(pending.len(), 1);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source, b.source());
    unsafe {
        record_add(
            &mut records,
            Some(b.source()),
            a.target(),
            11,
            14,
            b.source().controller.address,
        );
    }
    assert_eq!(records.len(), 1);
}
#[test]
fn later_weapon_or_other_gadget_add_relinquishes_same_key() {
    let mut f = Fixture::new();
    let mut records = vec![];
    f.observe(&mut records);
    unsafe {
        record_add(
            &mut records,
            None,
            f.target(),
            11,
            12,
            f.source().controller.address,
        );
    }
    assert!(records.is_empty());
    f.spy[0x60 / 4] |= 0x800;
    unsafe {
        assert!(take_dead(&mut records, f.target()).is_empty());
    }
}
#[test]
fn exact_cast_context_is_nested_and_does_not_leak_to_other_threads() {
    let f = Fixture::new();
    let s = f.source();
    assert!(CAST.with(Cell::get).is_none());
    {
        let _a = CastScope::enter(Some(s));
        {
            let _b = CastScope::enter(None);
            assert!(CAST.with(Cell::get).is_none());
        }
        assert_eq!(CAST.with(Cell::get), Some(s));
        std::thread::spawn(|| assert!(CAST.with(Cell::get).is_none()))
            .join()
            .unwrap();
    }
    assert!(CAST.with(Cell::get).is_none());
}
#[test]
fn generation_reuse_expiry_or_foreign_controller_never_deletes_new_mark() {
    for scenario in 0..4 {
        let mut f = Fixture::new();
        let mut records = vec![];
        f.observe(&mut records);
        f.spy[0x60 / 4] |= 0x800;
        match scenario {
            0 => f.spy_peer[0x74 / 4] += 1,
            1 => f.merc_peer[0x74 / 4] += 1,
            2 => f.merc[0x1a94 / 4] = 0,
            _ => f.marks[3] += 4,
        }
        unsafe {
            assert!(take_dead(&mut records, f.target()).is_empty());
        }
        assert!(records.is_empty());
    }
}
#[test]
fn unknown_layout_corrupt_array_or_inactive_transport_preserves_native() {
    for scenario in 0..6 {
        let mut f = Fixture::new();
        let mut records = vec![];
        f.observe(&mut records);
        f.spy[0x60 / 4] |= 0x800;
        match scenario {
            0 => f.spy[0] += 4,
            1 => f.merc[0x1a98 / 4] = 0,
            2 => f.controller_peer[0x78 / 4] = 0,
            3 => f.spy[0x4c / 4] = 2 << 8,
            4 => f.controller_peer[0x74 / 4] += 1,
            _ => f.spy[0x5c / 4] = 0,
        }
        unsafe {
            assert!(take_dead(&mut records, f.target()).is_empty());
        }
        assert_eq!(f.merc[0x1a94 / 4], 2);
    }
    unsafe {
        assert!(source(0).is_none());
        assert!(dead(0).is_none());
    }
}
#[test]
fn no_equipment_guess_and_no_records_for_failed_native_apply() {
    let mut f = Fixture::new();
    let mut records = vec![];
    let s = f.source();
    f.merc[0x1a94 / 4] = 0;
    unsafe {
        record_add(
            &mut records,
            Some(s),
            f.target(),
            11,
            12,
            s.controller.address,
        );
    }
    assert!(records.is_empty());
    f.merc[0x1a94 / 4] = 2;
    unsafe {
        record_add(&mut records, None, f.target(), 11, 12, s.controller.address);
    }
    assert!(records.is_empty());
}
