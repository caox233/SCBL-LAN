use super::*;

struct Fixture { ability: Vec<u32>, owner: Vec<u32>, light: Vec<u32>, table: Vec<u32> }
impl Fixture {
    fn new() -> Self {
        let mut f=Self { ability:vec![0;0xf0/4],owner:vec![0;0x400/4],light:vec![0;0x800/4],table:vec![0;0x700/4] };
        f.ability[0]=SABOTEUR;f.ability[0x6c/4]=f.owner.as_ptr() as u32;
        f.owner[0]=0x02ad3234; // independent native AEPlayerSpy RTTI, not a production constant
        f.owner[0x20/4]=71;f.owner[0x144/4]=0x12340000;f.owner[0x374/4]=1<<18;
        f.table[0x6f0/4]=LIGHT_EMP;f.light[0]=f.table.as_ptr() as u32;f.light[0x144/4]=0x12340000;
        f
    }
    fn value(&self, original: f32) -> f32 {
        unsafe { fixture_duration(self.ability.as_ptr() as usize,self.light.as_ptr() as usize,self.owner.as_ptr() as usize,original) }
    }
}
#[test]
fn saboteur_light_duration_is_ten_seconds() {
    let f=Fixture::new();for native in [1.0,3.0,5.0,12.0] { assert_eq!(f.value(native),10.0); }
}
#[test]
fn no_shared_device_or_grenade_duration_override() {
    let mut f=Fixture::new();f.table[0x6f0/4]=0xdfb0a0;assert_eq!(f.value(3.0),3.0);
    f.table[0x6f0/4]=LIGHT_EMP;f.ability[0]=0;assert_eq!(f.value(3.0),3.0);
    assert!(!unsafe { eligible(f.light.as_ptr() as usize,f.owner.as_ptr() as usize,3.0) });
}
#[test]
fn unknown_merc_and_cross_world_objects_remain_native() {
    let mut f=Fixture::new();f.owner[0x374/4]=1<<19;assert_eq!(f.value(3.0),3.0);
    f.owner[0x374/4]=0;assert_eq!(f.value(3.0),3.0);
    f.owner[0x374/4]=1<<18;f.light[0x144/4]+=1;assert_eq!(f.value(3.0),3.0);
    f.light[0]=1;assert_eq!(f.value(3.0),3.0);
    assert_eq!(unsafe { fixture_duration(0,0,0,3.0) },3.0);
}
#[test]
fn do_not_turn_disabled_or_invalid_native_events_on() {
    let f=Fixture::new();for x in [0.0,-1.0,f32::NAN,f32::INFINITY,f32::NEG_INFINITY] { assert_eq!(f.value(x).to_bits(),x.to_bits()); }
}
#[test]
fn reused_source_identity_and_unrelated_owner_do_not_inherit_scope() {
    let mut f=Fixture::new();let s=unsafe { source(f.ability.as_ptr() as usize) };
    with_source(s,|| {
        f.owner[0x20/4]+=1;
        assert!(!unsafe { eligible(f.light.as_ptr() as usize,f.owner.as_ptr() as usize,3.0) });
        assert!(!unsafe { eligible(f.light.as_ptr() as usize,0,3.0) });
    });
}
#[test]
fn scope_is_nested_unwind_safe_and_thread_local() {
    let f=Fixture::new();let s=unsafe { source(f.ability.as_ptr() as usize) };
    assert_eq!(SOURCE.with(Cell::get),None);
    with_source(s,|| {
        assert_eq!(SOURCE.with(Cell::get),s);
        with_source(None,|| assert_eq!(SOURCE.with(Cell::get),None));
        assert_eq!(SOURCE.with(Cell::get),s);
        assert_eq!(std::thread::spawn(||SOURCE.with(Cell::get)).join().unwrap(),None);
        let _=std::panic::catch_unwind(||with_source(None,||panic!("scope unwind fixture")));
        assert_eq!(SOURCE.with(Cell::get),s);
    });
    assert_eq!(SOURCE.with(Cell::get),None);
}

#[test]
fn native_spy_class_required_even_when_role_flags_are_forged() {
    let mut f=Fixture::new();
    assert_eq!(f.owner[0],0x02ad3234);
    assert_eq!(f.value(3.0),10.0);
    f.owner[0]=0x02ad2abc; // native AEPlayerMerc
    assert_eq!(f.value(3.0),3.0); // even if spy flag was left on
    f.owner[0x374/4]=1<<19;assert_eq!(f.value(3.0),3.0);
    f.owner[0]=0x02ad3234;assert_eq!(f.value(3.0),3.0); // wrong role flag
}

// Serial execution is also used by the root native-replay runner. The lock
// protects global sidecar assertions when these regression tests run in parallel.
static QUEUE_TESTS: Mutex<()> = Mutex::new(());
struct NativeQueue { manager: Vec<u32>, rows: Vec<u32> }
impl NativeQueue {
    fn new(f: &Fixture) -> Self {
        let mut q = Self { manager: vec![0; 0x7e4 / 4], rows: vec![0; 300 * 4] };
        q.manager[0] = 1; q.manager[0x20 / 4] = 101;
        q.manager[0x144 / 4] = f.light[0x144 / 4];
        q.manager[0x7d8 / 4] = q.rows.as_mut_ptr() as u32;
        q.manager[0x7e0 / 4] = 300;
        q
    }
    fn address(&self) -> usize { self.manager.as_ptr() as usize }
    fn push(&mut self, row: Row) {
        let n = self.manager[0x7dc / 4] as usize;
        self.rows[n * 4..n * 4 + 4].copy_from_slice(&[row.target as u32, row.owner as u32, row.duration, row.flag]);
        self.manager[0x7dc / 4] += 1;
    }
    fn pop(&mut self) {
        let n = self.manager[0x7dc / 4] as usize;
        self.rows.copy_within(4..n * 4, 0);
        self.manager[0x7dc / 4] -= 1;
    }
    fn enqueue(&mut self, f: &Fixture, saboteur: bool, native: f32, flags: u32) {
        let (a, p, o) = (f.ability.as_ptr() as usize, f.light.as_ptr() as usize, f.owner.as_ptr() as usize);
        let scope = if saboteur { unsafe { source(a) } } else { None };
        let m = self.address();
        with_source(scope, || unsafe { with_queue(m, p, o, native, flags, || {
            self.push(Row { target: p, owner: o, duration: native.to_bits(), flag: flags & 1 });
        }) });
    }
    fn consume<R>(&mut self, call: impl FnOnce(Row) -> R) -> R {
        let m = self.address();
        let head = unsafe { row(snapshot(m).unwrap(), 0).unwrap() };
        unsafe { with_consume(m, || { let result = call(head); self.pop(); result }) }
    }
}
impl Drop for NativeQueue { fn drop(&mut self) { clear_manager(self.address()); } }
unsafe fn emp(row: Row) -> f32 {
    let mapped = if word(row.owner, 0x5c).unwrap() & 0x200000 != 0 { row.owner } else { 0 };
    with_emp(row.target, mapped, f32::from_bits(row.duration), row.flag, |v| v)
}
fn light_fixture() -> Fixture {
    let mut f = Fixture::new();
    f.light[0x20 / 4] = 29;
    f.light[0x740 / 4] = 0.5f32.to_bits();
    f.light[0x744 / 4] = 0.25f32.to_bits();
    f
}
#[test]
fn ordinary_a_then_saboteur_b_same_tuple_cannot_steal_authorization() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let f = light_fixture(); let mut q = NativeQueue::new(&f);
    q.enqueue(&f, false, 3.0, 0); q.enqueue(&f, true, 3.0, 0);
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 3.0);
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 12.5);
    assert_eq!(f.light[0x740 / 4], 0.5f32.to_bits());
    assert_eq!(unsafe { with_emp(f.light.as_ptr() as usize, 0, 3.0, 0, |v| v) }, 3.0);
}
#[test]
fn native_owner_zero_mapping_is_exact_and_grant_is_single_use() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let mut f = light_fixture(); let mut q = NativeQueue::new(&f);
    for bit in [0, 0x200000] {
        f.owner[0x5c / 4] = bit;
        q.enqueue(&f, true, 3.0, 0);
        q.consume(|r| unsafe {
            let mapped = if bit == 0 { 0 } else { r.owner };
            let wrong = if bit == 0 { r.owner } else { 0 };
            assert_eq!(with_emp(r.target, wrong, 3.0, 0, |v| v), 3.0);
            assert_eq!(with_emp(r.target, mapped, 4.0, 0, |v| v), 4.0);
            assert_eq!(with_emp(r.target, mapped, 3.0, 1, |v| v), 3.0);
            assert_eq!(with_emp(r.target, mapped, 3.0, 0, |v| {
                assert!(PENDING.try_lock().is_ok());
                assert_eq!(word(r.target, 0x740), Some(10f32.to_bits())); v
            }), 12.5);
            assert_eq!(with_emp(r.target, mapped, 3.0, 0, |v| v), 3.0);
        });
    }
}
#[test]
fn registration_overflow_does_not_evict_or_crosswire_live_events() {
    let _lock = QUEUE_TESTS.lock().unwrap(); clear_pending();
    let f = light_fixture(); let mut q = NativeQueue::new(&f);
    for _ in 0..LIMIT + 1 { q.enqueue(&f, true, 3.0, 0); }
    assert_eq!(PENDING.lock().unwrap().iter().flatten().count(), LIMIT);
    for _ in 0..LIMIT { assert_eq!(q.consume(|r| unsafe { emp(r) }), 12.5); }
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 3.0);
}
#[test]
fn empty_queue_clears_stale_authorization_before_identical_reuse() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let f = light_fixture(); let mut q = NativeQueue::new(&f);
    q.enqueue(&f, true, 3.0, 0);
    q.manager[0x7dc / 4] = 0;
    unsafe { with_consume(q.address(), || ()); }
    q.enqueue(&f, false, 3.0, 0);
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 3.0);
    q.enqueue(&f, true, 3.0, 0);
    q.manager[0x7dc / 4] = 0;
    q.enqueue(&f, false, 3.0, 0); // Empty append also clears, without empty consume.
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 3.0);
}
#[test]
fn changed_manager_source_target_identity_or_world_falls_back() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    for change in 0..7 {
        let mut f = light_fixture(); let mut q = NativeQueue::new(&f);
        q.enqueue(&f, true, 3.0, 0);
        match change {
            0 => q.manager[0x20 / 4] += 1,
            1 => q.manager[0x144 / 4] += 1,
            2 => q.manager[0] += 1,
            3 => f.owner[0x20 / 4] += 1,
            4 => f.owner[0x144 / 4] += 1,
            5 => f.light[0x20 / 4] += 1,
            _ => f.light[0x144 / 4] += 1,
        }
        assert_eq!(q.consume(|r| unsafe { emp(r) }), 3.0, "change={change}");
    }
}
#[test]
fn synchronous_append_is_shifted_with_native_queue_after_consumption() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let f = light_fixture(); let mut q = NativeQueue::new(&f);
    q.enqueue(&f, false, 3.0, 0);
    let m = q.address(); let head = unsafe { row(snapshot(m).unwrap(), 0).unwrap() };
    unsafe { with_consume(m, || {
        q.enqueue(&f, true, 3.0, 0);
        assert_eq!(emp(head), 3.0);
        q.pop();
    }) };
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 12.5);
}
#[test]
fn distinct_managers_and_child_targets_do_not_share_authorization() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let f = light_fixture(); let child = light_fixture();
    let mut a = NativeQueue::new(&f); let mut b = NativeQueue::new(&f);
    a.enqueue(&f, false, 3.0, 0); b.enqueue(&f, true, 3.0, 0);
    assert_eq!(a.consume(|r| unsafe { emp(r) }), 3.0);
    b.consume(|r| unsafe {
        assert_eq!(with_emp(child.light.as_ptr() as usize, 0, 3.0, 0, |v| v), 3.0);
        assert_eq!(emp(r), 12.5);
    });
}
#[test]
fn unconfirmed_append_or_modified_row_never_registers() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let f = light_fixture(); let mut q = NativeQueue::new(&f);
    let (p, o) = (f.light.as_ptr() as usize, f.owner.as_ptr() as usize);
    let s = unsafe { source(f.ability.as_ptr() as usize) };
    with_source(s, || unsafe { with_queue(q.address(), p, o, 3.0, 0, || ()) });
    q.enqueue(&f, false, 3.0, 0);
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 3.0);
    q.enqueue(&f, true, 3.0, 0);
    q.rows[2] = 4f32.to_bits();
    assert_eq!(q.consume(|r| unsafe { emp(r) }), 4.0);
}
#[test]
fn event_restores_config_on_unwind_and_preserves_other_phase() {
    let _lock = QUEUE_TESTS.lock().unwrap();
    let f = light_fixture();
    let (a, p, o) = (f.ability.as_ptr() as usize, f.light.as_ptr() as usize, f.owner.as_ptr() as usize);
    let _ = std::panic::catch_unwind(|| unsafe { fixture_event(a, p, o, 3.0, 1, |_| panic!("native fixture unwind")) });
    assert_eq!(f.light[0x740 / 4], 0.5f32.to_bits());
    assert_eq!(f.light[0x744 / 4], 0.25f32.to_bits());
    assert_eq!(ACTIVE.with(Cell::get), None);
    assert_eq!(unsafe { fixture_event(a, p, o, 3.0, 1, |v| v) }, 12.75);
}
