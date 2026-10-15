//! Lifecycle/negative cases. Native userdata construction is separately exercised
//! by native_owner_zero_observer.template.rs, NOT proved by these owned fixtures.
use super::*;

struct F {
    source: Vec<u32>, actor: Vec<u32>, node: Vec<u32>,
    record: Vec<u32>, view: Vec<u32>, render: Vec<u32>,
}
impl F {
    fn new() -> Self {
        let mut f = Self { source: vec![0; 0x880/4], actor: vec![0; 0x440/4],
            node: vec![0; 0x240/4], record: vec![0; 4],
            view: vec![0; 0x344/4], render: vec![0; 0x20d4/4] };
        f.source[0] = SMOKE; f.source[0x144/4] = 0x12340000; f.source[0x610/4] = 1;
        f.actor[0x350/4] = 0x12341000;f.actor[0] = PARTICLE; f.actor[0x20/4] = 17; f.actor[0x144/4] = 0x12340000;
        f.record[0] = f.node.as_ptr() as u32;
        f.view[0x340/4] = 8; f.render[0x20d0/4] = f.view.as_ptr() as u32;
        f
    }
    fn actor(&self) -> usize { self.actor.as_ptr() as usize }
    unsafe fn register(&self) { assert_eq!(with_spawn(true, self.source.as_ptr() as usize, 1, || self.actor()), self.actor()); }
    unsafe fn create(&mut self) {
        let actor = self.actor(); let node = self.node.as_ptr() as u32;
        with_node_creation(true, actor, || self.actor[0x30c/4] = node);
    }
    unsafe fn draw(&self) -> u8 { with_particle(true, self.render.as_ptr() as usize, self.record.as_ptr() as usize, 1, || 99) }
}
impl Drop for F { fn drop(&mut self) { with_delete(self.actor(), || ()); } }
fn reset() {
    crate::overload_particles::fixture_enable(false);
    crate::overload_particles::clear();
    *CLOUDS.lock().unwrap() = Clouds::new(); HAS_SMOKE.store(false, Ordering::Release);
    *ROOTS.lock().unwrap() = Roots::new();
}
#[test]
fn roots_zero_then_create_and_create_then_register() { unsafe {
    reset();
    for before in [false, true] {
        let mut f = F::new();
        if before { f.register(); assert_eq!(f.draw(), 99); }
        f.create();
        if !before { assert_eq!(f.draw(), 99); f.register(); }
        assert_eq!(f.draw(), 1);
        assert_eq!(f.node[0x20/4], 0); // lifecycle fixture, native proof is separate
        f.view[0x340/4] = 7; assert_eq!(f.draw(), 99);
    }
    reset();
} }
#[test]
fn roots_rebuild_and_clear_never_accept_stale_node() { unsafe {
    reset(); let mut f = F::new(); f.create(); f.register(); assert_eq!(f.draw(), 1);
    let next = vec![0u32; 0x240/4]; let old = f.record[0];
    f.actor[0x30c/4] = next.as_ptr() as u32;
    assert_eq!(f.draw(), 99); // old cache must be checked against live root
    f.record[0] = next.as_ptr() as u32; assert_eq!(f.draw(), 99); // not refreshed yet
    with_node_creation(true, f.actor(), || ()); assert_eq!(f.draw(), 1);
    f.record[0] = old; assert_eq!(f.draw(), 99);
    f.record[0] = next.as_ptr() as u32; f.actor[0x30c/4] = 0; assert_eq!(f.draw(), 99);
    with_node_creation(true, f.actor(), || ()); assert_eq!(f.draw(), 99);
    reset();
} }
#[test]
fn roots_serial_world_class_reuse_requires_fresh_authorized_registration() { unsafe {
    reset();
    for field in [0, 0x20/4, 0x144/4] {
        let mut f = F::new(); f.create(); f.register(); assert_eq!(f.draw(), 1);
        let old = f.actor[field]; f.actor[field] ^= 4;
        assert_eq!(f.draw(), 99);
        with_node_creation(true, f.actor(), || ()); // remove stale identity, do not reclassify
        f.actor[field] = old; assert_eq!(f.draw(), 99);
        f.register(); assert_eq!(f.draw(), 1);
        f.source[0] = 0x02ad2abc; f.register(); // same address becomes unrelated FX
        assert_eq!(f.draw(), 99);
        with_node_creation(true, f.actor(), || ()); assert_eq!(f.draw(), 99);
    }
    reset();
} }
#[test]
fn roots_unknown_and_forged_userdata_never_inherit_owner() { unsafe {
    reset(); let mut smoke = F::new(); smoke.create(); smoke.register();
    let mut other = F::new(); other.create(); other.node[0x20/4] = smoke.actor() as u32;
    assert_eq!(smoke.draw(), 1); assert_eq!(other.draw(), 99);
    other.source[0] = 0x02ad2abc; other.register(); assert_eq!(other.draw(), 99);
    assert_eq!(particle_owner(0), None);
    let null = [0u32]; assert_eq!(particle_owner(null.as_ptr() as usize), None);
    let unknown = [1u32]; assert_eq!(particle_owner(unknown.as_ptr() as usize), None);
    reset();
} }
#[test]
fn roots_creation_native_once_no_lock_and_delete_before_reuse() { unsafe {
    reset(); let mut f = F::new(); f.register(); let ap = f.actor(); let mut calls = 0;
    assert_eq!(with_node_creation(true, ap, || {
        calls += 1; assert!(ROOTS.try_lock().is_ok()); assert!(CLOUDS.try_lock().is_ok());
        f.actor[0x30c/4] = f.node.as_ptr() as u32; 37
    }), 37); assert_eq!(calls, 1); assert_eq!(f.draw(), 1);
    with_delete(ap, || { assert_eq!(f.draw(), 99); assert!(ROOTS.try_lock().is_ok()); });
    with_node_creation(true, ap, || ()); assert_eq!(f.draw(), 99);
    reset();
} }
#[test]
fn roots_registration_inside_creation_callback_is_not_lost() { unsafe {
    reset(); let mut f = F::new(); let ap = f.actor();
    // Registration snapshots zero while creation is in progress; return refresh
    // must consult current registered identity, not only the pre-call table.
    with_node_creation(true, ap, || { f.register(); f.actor[0x30c/4] = f.node.as_ptr() as u32; });
    assert_eq!(f.draw(), 1); reset();
} }
#[test]
fn roots_disabled_creation_unknown_and_cross_thread_draw() { unsafe {
    reset(); let mut f = F::new(); f.register(); let ap = f.actor();
    with_node_creation(false, ap, || f.actor[0x30c/4] = f.node.as_ptr() as u32);
    assert_eq!(f.draw(), 99);
    with_node_creation(true, ap, || ()); assert_eq!(f.draw(), 1);
    let (render, record) = (f.render.as_ptr() as usize, f.record.as_ptr() as usize);
    assert_eq!(std::thread::spawn(move || with_particle(true, render, record, 1, || 99)).join().unwrap(), 1);
    reset();
} }
#[test]
fn roots_bounded_owned_index_no_eviction_and_ambiguous_nodes_native() {
    let mut r = Roots::new();
    for i in 1..=ROOT_CAPACITY {
        assert!(r.insert(Root { id: Identity { address: i, object: 1, world: 2 }, node: i + 1000 }));
    }
    assert!(!r.insert(Root { id: Identity { address: 999, object: 1, world: 2 }, node: 9999 }));
    assert_eq!(r.by_node(1001).unwrap().id.address, 1);
    assert_eq!(r.by_node(9999), None); assert_eq!(r.by_node(0), None);
    assert!(r.insert(Root { id: Identity { address: 2, object: 1, world: 2 }, node: 1001 }));
    assert_eq!(r.by_node(1001), None);
    r.remove(2); assert_eq!(r.by_node(1001).unwrap().id.address, 1);
}
#[test]
fn roots_overload_uses_same_index_not_node_userdata() { unsafe {
    reset(); let mut f = F::new();
    let mut world = vec![0u32; 0x700/4]; let mut info = vec![0u32; 0x400/4];
    let mut pc = vec![0u32; 0x1000/4]; let mut pawn = vec![0u32; 0xc00/4]; let players = [pc.as_ptr() as u32];
    let wp = world.as_ptr() as u32;
    world[0x6d0/4] = info.as_ptr() as u32; info[0x36c/4] = players.as_ptr() as u32;
    info[0x370/4] = 1; info[0x374/4] = 1;
    pc[0] = 0x02ac53ec; pc[0x60/4] = 0x40000; pc[0x144/4] = wp; pc[0x6dc/4] = 1; pc[0x6a4/4] = 1;
    (pc.as_mut_ptr() as *mut u8).add(0x55d).write(5);
    pawn[0] = 0x02ad2abc; pawn[0x374/4] = 0x80000; pawn[0x144/4] = wp; pawn[0x938/4] = pc.as_ptr() as u32;
    pc[0x520/4] = pawn.as_ptr() as u32;
    let mut ability = vec![0u32; 0x100/4]; let mut owner = vec![0u32; 0x500/4];
    ability[0] = 0x02a3f3a4; ability[0x6c/4] = owner.as_ptr() as u32;
    owner[0x144/4] = wp; f.actor[0x144/4] = wp;
    crate::overload_particles::fixture_enable(true);
    crate::overload_particles::record_spawn(ability.as_ptr() as usize, f.actor());
    assert_eq!(f.draw(), 99); f.create(); assert_eq!(f.draw(), 1);
    assert_eq!(f.node[0x20/4], 0);
    (pc.as_mut_ptr() as *mut u8).add(0x55d).write(4);
    pawn[0] = 0x02ad3234; pawn[0x374/4] = 0x40000;
    assert_eq!(f.draw(), 1); // no goggles: hidden under POLISH6
    (pc.as_mut_ptr() as *mut u8).add(0x55d).write(5); pc[0x520/4] = 0;
    assert_eq!(f.draw(), 1); // existing death-camera policy unchanged
    crate::overload_particles::clear(); assert_eq!(f.draw(), 99);
    reset();
} }
