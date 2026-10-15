use super::*;
static SERIAL: Mutex<()> = Mutex::new(());
struct Fixture {
    source: Vec<u32>, definition: Vec<u32>, particle: Vec<u32>, node: Vec<u32>,
    render: Vec<u32>, view: Vec<u32>, record: Vec<u32>,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self { source: vec![0; 0x880/4], definition: vec![0; 0x100/4],
            particle: vec![0; 0x500/4], node: vec![0; 0x240/4], render: vec![0; 0x2400/4],
            view: vec![0; 0x400/4], record: vec![0; 0x40/4] };
        f.source[0] = SMOKE; f.source[0x144/4] = 0x56780000;
        f.source[0x610/4] = f.definition.as_ptr() as u32;
        f.particle[0] = PARTICLE; f.particle[0x20/4] = 14;
        f.particle[0x144/4] = f.source[0x144/4];
        f.particle[0x30c/4] = f.node.as_ptr() as u32;
        f.node[0x20/4] = f.particle.as_ptr() as u32;
        f.render[0x20d0/4] = f.view.as_ptr() as u32; f.view[0x340/4] = 8;
        f.record[0] = f.node.as_ptr() as u32;
        f
    }
    unsafe fn spawn(&self) -> usize {
        with_spawn(true, self.source.as_ptr() as usize, self.definition.as_ptr() as usize,
                   || self.particle.as_ptr() as usize)
    }
    unsafe fn draw(&self, enabled: bool) -> u8 {
        with_particle(enabled, self.render.as_ptr() as usize, self.record.as_ptr() as usize, 1, || 99)
    }
}
// Match the production deletion boundary before the owned actor/node allocations die.
impl Drop for Fixture {
    fn drop(&mut self) { with_delete(self.particle.as_ptr() as usize, || ()); }
}
fn reset() {
    *CLOUDS.lock().unwrap() = Clouds::new(); HAS_SMOKE.store(false, Ordering::Release);
    *ROOTS.lock().unwrap() = Roots::new();
}
#[test]
fn exact_smoke_source_only_no_trail_parent_or_global_status_dependency() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset();
    for kind in [SMOKE, ADV_SMOKE] {
        let mut f = Fixture::new(); f.source[0] = kind;
        let before = (f.source.clone(), f.particle.clone(), f.node.clone());
        assert_eq!(f.spawn(), f.particle.as_ptr() as usize); assert_eq!(f.draw(true), 1);
        assert_eq!(before, (f.source.clone(), f.particle.clone(), f.node.clone()));
        forget(f.particle.as_ptr() as usize);
    }
    for kind in [0, 0x02ad2abc, 0x02a76b1c, 0x02a88f54] {
        let mut f = Fixture::new(); f.source[0] = kind; f.spawn(); assert_eq!(f.draw(true), 99);
    }
    let mut f = Fixture::new(); f.source[0x610/4] = 0; f.spawn(); assert_eq!(f.draw(true), 99);
    reset();
} }
#[test]
fn thermal_only_and_native_return_values_preserved() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset(); let mut f = Fixture::new(); f.spawn();
    for mode in [0, 7, 9, 15, u32::MAX, 8, 0, 8] {
        f.view[0x340/4] = mode; assert_eq!(f.draw(true), if mode == 8 { 1 } else { 99 });
        assert_eq!(f.draw(false), 99);
    }
    reset();
} }
#[test]
fn identity_proxy_world_unknown_and_deleted_particle_fail_open() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset(); let mut f = Fixture::new(); f.spawn();
    assert_eq!(f.draw(true), 1);
    for field in [0, 0x20/4, 0x144/4, 0x30c/4] {
        let old = f.particle[field]; f.particle[field] ^= 4;
        assert_eq!(f.draw(true), 99); f.particle[field] = old;
    }
    forget(f.particle.as_ptr() as usize); assert_eq!(f.draw(true), 99);
    f.particle[0x144/4] ^= 4; f.spawn(); assert_eq!(f.draw(true), 99);
    assert!(!HAS_SMOKE.load(Ordering::Acquire)); reset();
} }
#[test]
fn bounded_registry_no_eviction_and_address_replacement() {
    let mut c = Clouds::new();
    for address in 1..=CAPACITY { assert!(c.insert(Identity { address, object: 1, world: 7 })); }
    assert!(!c.insert(Identity { address: 999, object: 1, world: 7 }));
    assert!(c.insert(Identity { address: 1, object: 2, world: 8 }));
    assert_eq!(c.get(1).unwrap().object, 2); c.remove(1); assert_eq!(c.get(1), None);
    assert!(c.insert(Identity { address: 999, object: 1, world: 7 }));
}
#[test]
fn null_factory_and_cross_thread_render_preserve_native_lifetime() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset(); let f = Fixture::new();
    assert_eq!(with_spawn(true, f.source.as_ptr() as usize, f.definition.as_ptr() as usize, || 0), 0);
    assert_eq!(f.draw(true), 99); f.spawn();
    let (render, record) = (f.render.as_ptr() as usize, f.record.as_ptr() as usize);
    assert_eq!(std::thread::spawn(move || with_particle(true, render, record, 1, || 99)).join().unwrap(), 1);
    forget(f.particle.as_ptr() as usize); assert_eq!(f.draw(true), 99); reset();
} }
#[test]
fn new_non_smoke_effect_at_reused_address_never_inherits_cloud() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset(); let mut f = Fixture::new();
    f.spawn(); assert_eq!(f.draw(true), 1);
    f.source[0] = 0x02ad2abc; f.spawn(); assert_eq!(f.draw(true), 99);
    assert!(!HAS_SMOKE.load(Ordering::Acquire)); reset();
} }
#[test]
fn native_callbacks_do_not_hold_registry_lock_and_unknown_input_is_native() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset();
    assert_eq!(with_particle(true, 0, 0, 1, || { assert!(CLOUDS.try_lock().is_ok()); 99 }), 99);
    let f = Fixture::new(); f.spawn();
    assert_eq!(with_particle(true, 0, 0, 1, || { assert!(CLOUDS.try_lock().is_ok()); 99 }), 99);
    reset();
} }
#[test]
fn bounded_draw_cost_observation_not_a_game_fps_claim() { unsafe {
    let _guard = SERIAL.lock().unwrap(); reset(); let f = Fixture::new();
    for phase in ["empty", "tracked", "untracked"] {
        if phase == "tracked" { f.spawn(); }
        let other = Fixture::new(); let input = if phase == "untracked" { &other } else { &f };
        let start = std::time::Instant::now(); let mut count = 0u32;
        for _ in 0..10000 { count += u32::from(std::hint::black_box(input.draw(true))); }
        println!("SMOKE_COST phase={phase} draws=10000 elapsed_us={} checksum={count} debug_fixture_not_game_fps=true", start.elapsed().as_micros());
    }
    reset();
} }
#[test]
fn continuous_cloud_rejects_other_effects_source_reuse_and_unknown_identity() { unsafe {
    let _guard=SERIAL.lock().unwrap();reset();
    for bad in 0..9 {
      let mut f=Fixture::new();f.source[0]=ADV_SMOKE;f.source[0x20/4]=7;f.source[0x864/4]=9;f.source[0x860/4]=f.particle.as_ptr()as u32;
      let s=f.source.as_mut_ptr();let p=f.particle.as_mut_ptr();
      match bad {0=>f.source[0]=SMOKE,1=>f.source[0]=0x02aa237c,2=>f.source[0x864/4]=0,3=>f.source[0x144/4]=0,4=>f.particle[0]=0,5=>f.particle[0x144/4]^=4,6=>f.source[0x860/4]=0,_=>()}
      let mut calls=0;assert_eq!(with_continuous(true,s as usize,||{calls+=1;assert!(CLOUDS.try_lock().is_ok());if bad==7{s.add(0x20/4).write(8);}if bad==8{s.add(0x864/4).write(10);}77}),77);
      assert_eq!(calls,1);assert_eq!(f.draw(true),99,"bad={bad}");forget(p as usize);
    }
    assert_eq!(with_continuous(true,0,||77),77);
    let mut f=Fixture::new();f.source[0]=ADV_SMOKE;f.source[0x864/4]=9;f.source[0x860/4]=f.particle.as_ptr()as u32;
    assert_eq!(with_continuous(false,f.source.as_ptr()as usize,||77),77);assert_eq!(f.draw(true),99);reset();
} }
#[test]
fn continuous_cloud_existing_address_does_not_duplicate_entries() { unsafe {
 let _guard=SERIAL.lock().unwrap();reset();let mut f=Fixture::new();f.source[0]=ADV_SMOKE;f.source[0x864/4]=9;f.source[0x860/4]=f.particle.as_ptr()as u32;
 for _ in 0..300 {with_continuous(true,f.source.as_ptr()as usize,||());}
 assert_eq!(CLOUDS.lock().unwrap().entries.iter().flatten().count(),1);assert_eq!(f.draw(true),1);
 with_delete(f.particle.as_ptr()as usize,||());assert_eq!(f.draw(true),99);reset();
} }

// Deterministic owned-memory lifetime regressions. The node allocation stays
// alive throughout; only its binding changes. No allocator reuse is required,
// no freed actor is dereferenced, and no synthetic Root is inserted.
#[test]
fn fixture_lifetime_reset_retires_previous_world_roots() { unsafe {
    let guard = SERIAL.lock().unwrap();
    reset(); *ROOTS.lock().unwrap() = Roots::new(); // isolate either baseline or fixed reset
    let old = Fixture::new(); let mut next = Fixture::new();
    assert_ne!(old.particle.as_ptr(), next.particle.as_ptr());
    assert_eq!(old.spawn(), old.particle.as_ptr() as usize);
    assert_eq!(old.draw(true), 1);
    let node = old.node.as_ptr() as usize; // allocation remains owned by old
    reset(); // measured boundary: a new test world must retire both registries
    let roots_after_reset = ROOTS.lock().unwrap().entries.iter().flatten().count();
    next.particle[0x30c/4] = node as u32; next.record[0] = node as u32;
    assert_eq!(next.spawn(), next.particle.as_ptr() as usize);
    let roots_for_node = ROOTS.lock().unwrap().entries.iter().flatten().filter(|r| r.node == node).count();
    let draw = next.draw(true);
    println!("SMOKE_FIXTURE_LIFETIME case=reset roots_after_reset={roots_after_reset} roots_for_node={roots_for_node} draw={draw}");
    // Cleanup even on the known bad baseline, before asserting the observations.
    // This does not change reset or the measured output and avoids a poison cascade.
    with_delete(old.particle.as_ptr() as usize, || ());
    with_delete(next.particle.as_ptr() as usize, || ());
    drop(next); drop(old); reset(); *ROOTS.lock().unwrap() = Roots::new(); drop(guard);
    assert_eq!((roots_after_reset, roots_for_node, draw), (0, 1, 1));
} }
#[test]
fn fixture_lifetime_drop_retires_actor_before_owned_node_reuse() { unsafe {
    let guard = SERIAL.lock().unwrap();
    reset(); *ROOTS.lock().unwrap() = Roots::new();
    // Allocate both actors before either is freed: their addresses cannot alias.
    let mut old = Fixture::new(); let mut next = Fixture::new();
    let old_actor = old.particle.as_ptr() as usize;
    let next_actor = next.particle.as_ptr() as usize;
    assert_ne!(old_actor, next_actor);
    assert_eq!(old.spawn(), old_actor); assert_eq!(old.draw(true), 1);
    let mut held_node = std::mem::take(&mut old.node);
    let node = held_node.as_ptr() as usize;
    next.particle[0x30c/4] = node as u32; next.record[0] = node as u32;
    assert_eq!(next.spawn(), next_actor);
    let roots_before_drop = ROOTS.lock().unwrap().entries.iter().flatten().filter(|r| r.node == node).count();
    let ambiguous_draw = next.draw(true); // two live bindings must still fail native
    drop(old); // measured boundary; fixed Fixture::drop calls real with_delete
    held_node[0x20/4] = next_actor as u32; // not used as an owner shortcut
    let stale_roots = ROOTS.lock().unwrap().entries.iter().flatten().filter(|r| r.id.address == old_actor).count();
    let stale_clouds = CLOUDS.lock().unwrap().entries.iter().flatten().filter(|id| id.address == old_actor).count();
    let roots_for_node = ROOTS.lock().unwrap().entries.iter().flatten().filter(|r| r.node == node).count();
    // In the baseline two matching Roots stop by_node BEFORE any old-actor read.
    let draw = next.draw(true);
    println!("SMOKE_FIXTURE_LIFETIME case=drop roots_before_drop={roots_before_drop} ambiguous_draw={ambiguous_draw} stale_roots={stale_roots} stale_clouds={stale_clouds} roots_for_node={roots_for_node} draw={draw}");
    with_delete(old_actor, || ()); // registry removal only; never dereferences old_actor
    with_delete(next_actor, || ());
    drop(next); drop(held_node); reset(); *ROOTS.lock().unwrap() = Roots::new(); drop(guard);
    assert_eq!((roots_before_drop, ambiguous_draw, stale_roots, stale_clouds, roots_for_node, draw), (2, 99, 0, 0, 1, 1));
} }
