//! Owned policy fixtures, not visual evidence. Root executes the complete pinned
//! E3E210 body through fixture_install_grenade_sphere separately.
use super::*;
struct F {
    grenade: Vec<u32>, component: Vec<u32>, sphere: Vec<u32>, particle: Vec<u32>,
    node: Vec<u32>, record: Vec<u32>, view: Vec<u32>, render: Vec<u32>,
}
impl F {
    fn new() -> Self {
        let mut f=Self { grenade:vec![0;0x868/4],component:vec![0;0xb0/4],
            sphere:vec![0;0x400/4],particle:vec![0;0x440/4],node:vec![0;0x240/4],
            record:vec![0;1],view:vec![0;0x344/4],render:vec![0;0x20d4/4] };
        f.grenade[0]=SMOKE;f.grenade[0x20/4]=71;f.grenade[0x144/4]=0x12340000;
        f.grenade[0x614/4]=f.component.as_ptr()as u32;
        // Deliberately NOT Smoke component: provenance comes from the actual
        // Smoke actor and exact returned sphere/component, never this VT alone.
        f.component[0]=0x02a3e3ac;f.component[0x20/4]=81;f.component[0x30/4]=0x1000;
        f.sphere[0]=SMOKE_SPHERE;f.sphere[0x20/4]=91;f.sphere[0x144/4]=0x12340000;
        f.sphere[0x320/4]=f.component.as_ptr()as u32;
        f.particle[0]=PARTICLE;f.particle[0x20/4]=101;f.particle[0x144/4]=0x12340000;
        f.particle[0x30c/4]=f.node.as_ptr()as u32;f.record[0]=f.node.as_ptr()as u32;
        f.view[0x340/4]=8;f.render[0x20d0/4]=f.view.as_ptr()as u32;f
    }
    fn gp(&self)->usize {self.grenade.as_ptr()as usize}
    fn sp(&self)->usize {self.sphere.as_ptr()as usize}
    fn pp(&self)->usize {self.particle.as_ptr()as usize}
    unsafe fn create(&mut self,enabled:bool)->usize {
        let (g,s,p)=(self.gp(),self.sp(),self.pp()as u32);
        with_source_sphere(enabled,g,||{with_volume(enabled,s,||self.sphere[0x3bc/4]=p);s})
    }
    unsafe fn draw(&self)->u32 {with_particle(true,self.render.as_ptr()as usize,self.record.as_ptr()as usize,1,||99)}
}
impl Drop for F {fn drop(&mut self){with_delete(self.pp(),||());}}
fn reset(){
    crate::overload_particles::fixture_enable(false);crate::overload_particles::clear();
    *CLOUDS.lock().unwrap()=Clouds::new();HAS_SMOKE.store(false,Ordering::Release);
    *ROOTS.lock().unwrap()=Roots::new();
}
#[test]
fn source_sphere_exact_smoke_and_advanced_accept_owned_non_smoke_component(){unsafe{
    reset();for class in [SMOKE,ADV_SMOKE] {
        let mut f=F::new();f.grenade[0]=class;
        assert_eq!(f.create(true),f.sp());assert_eq!(f.draw(),1);
        for mode in [0,1,7,9,10] {f.view[0x340/4]=mode;assert_eq!(f.draw(),99);}
    }reset();
}}
#[test]
fn source_sphere_disabled_other_source_or_missing_fields_remain_native(){unsafe{
    reset();for case in 0..9 {
        let mut f=F::new();match case {
            0=>{},1=>f.grenade[0]=SMOKE+4,2=>f.grenade[0]=SMOKE_SPHERE,
            3=>f.grenade[0x614/4]=0,4=>f.grenade[0x144/4]=0,
            5=>f.component[0x30/4]=0,6=>f.component[0x34/4]=0x400,
            7=>f.particle[0]=PARTICLE+4,_=>f.particle[0x144/4]^=4,
        }
        assert_eq!(f.create(case!=0),f.sp());assert_eq!(f.draw(),99,"case {case}");
    }reset();
}}
#[test]
fn source_sphere_source_transition_during_native_rejects(){unsafe{
    reset();for case in 0..5 {
        let mut f=F::new();let other=f.component.clone();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
        assert_eq!(with_source_sphere(true,g,||{
            with_volume(true,s,||{ f.sphere[0x3bc/4]=p;
            match case {0=>f.grenade[0x20/4]+=1,1=>f.grenade[0x144/4]^=4,
                2=>f.grenade[0]=ADV_SMOKE,3=>f.grenade[0x614/4]=other.as_ptr()as u32,
                _=>f.grenade[0]=SMOKE+4,} }); s
        }),s);assert_eq!(f.draw(),99,"source transition {case}");
    }reset();
}}
#[test]
fn source_sphere_component_transition_during_native_rejects(){unsafe{
    reset();for case in 0..5 {
        let mut f=F::new();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
        with_source_sphere(true,g,||{
            with_volume(true,s,||{ f.sphere[0x3bc/4]=p;
            match case {0=>f.component[0x20/4]+=1,1=>f.component[0x30/4]+=4,
                2=>f.component[0]=SMOKE_COMPONENT,3=>f.component[0x34/4]|=0x400,
                _=>f.component[0x30/4]=0,} }); s
        });assert_eq!(f.draw(),99,"component transition {case}");
    }reset();
}}
#[test]
fn source_sphere_wrong_returned_sphere_relation_rejects(){unsafe{
    reset();for case in 0..5 {
        let mut f=F::new();let other=f.component.clone();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
        let returned=with_source_sphere(true,g,||{
            with_volume(true,s,||{ f.sphere[0x3bc/4]=p;
            match case {0=>f.sphere[0]=SMOKE_SPHERE+4,1=>f.sphere[0x144/4]^=4,
                2=>f.sphere[0x320/4]=other.as_ptr()as u32,3=>f.sphere[0x3bc/4]=0,_=>{},} });
            if case==4 {0}else{s}
        });assert_eq!(returned,if case==4{0}else{s});assert_eq!(f.draw(),99,"sphere relation {case}");
    }reset();
}}
#[test]
fn source_sphere_native_once_without_locks_and_result_preserved(){unsafe{
    reset();for enabled in [false,true] {
        let mut f=F::new();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);let mut calls=0;
        let got=with_source_sphere(enabled,g,||{
            calls+=1;assert!(CLOUDS.try_lock().is_ok());assert!(ROOTS.try_lock().is_ok());
            with_volume(enabled,s,||f.sphere[0x3bc/4]=p);s
        });assert_eq!(got,s);assert_eq!(calls,1);assert_eq!(f.draw(),if enabled{1}else{99});
    }
    let mut calls=0;assert_eq!(with_source_sphere(true,0,||{calls+=1;0x1234}),0x1234);assert_eq!(calls,1);reset();
}}
#[test]
fn source_sphere_other_native_component_flag_updates_are_not_rejected(){unsafe{
    reset();let mut f=F::new();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
    with_source_sphere(true,g,||{with_volume(true,s,||{f.component[0x34/4]|=0x8000;f.sphere[0x3bc/4]=p;});s});
    assert_eq!(f.draw(),1);reset();
}}
#[test]
fn source_sphere_no_origin324_dependency(){unsafe{
    reset();let mut f=F::new();f.sphere[0x324/4]=0;f.create(true);assert_eq!(f.draw(),1);reset();
}}
#[test]
fn source_sphere_nested_sources_and_deinit_keep_lifetime_scoping(){unsafe{
    reset();let mut outer=F::new();let mut inner=F::new();let(g,s,p)=(outer.gp(),outer.sp(),outer.pp()as u32);
    with_source_sphere(true,g,||{inner.create(true);with_volume(true,s,||outer.sphere[0x3bc/4]=p);s});
    assert_eq!(outer.draw(),1);assert_eq!(inner.draw(),1);deinit();
    assert_eq!(outer.draw(),99);assert_eq!(inner.draw(),99);reset();
}}
#[test]
fn source_sphere_particle_reuse_and_delete_invalidate(){unsafe{
    reset();let mut f=F::new();f.create(true);assert_eq!(f.draw(),1);
    f.particle[0x20/4]+=1;assert_eq!(f.draw(),99);f.create(true);assert_eq!(f.draw(),1);
    with_delete(f.pp(),||());assert_eq!(f.draw(),99);reset();
}}
#[test]
fn source_sphere_diagnostic_categories_do_not_consume_other_source_budgets(){
    let bits=std::sync::atomic::AtomicU32::new(0);
    assert!(source_sphere_event_claim(&bits,2,0));
    for _ in 0..100 {assert!(!source_sphere_event_claim(&bits,2,0));}
    for kind in 0..3 {for outcome in 0..5 {
        if (kind,outcome)!=(2,0) {assert!(source_sphere_event_claim(&bits,kind,outcome));}
        assert!(!source_sphere_event_claim(&bits,kind,outcome));
    }}
    assert_eq!(bits.load(Ordering::Relaxed).count_ones(),15);
    assert!(!source_sphere_event_claim(&bits,3,0));assert!(!source_sphere_event_claim(&bits,0,5));
}
#[test]
fn source_sphere_310_skip_never_classifies_old_particle(){unsafe{
    reset();let mut f=F::new();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
    f.sphere[0x310/4]=1;f.sphere[0x3bc/4]=p;
    // Same shape/identity and an old 3BC are not a creation event. The complete
    // pinned E3E210 witness separately exercises this native 310 branch.
    assert_eq!(with_source_sphere(true,g,||s),s);assert_eq!(f.draw(),99);
    assert!(SOURCE_SPHERE_SCOPE.with(|slot|slot.get().is_none()));reset();
}}
#[test]
fn source_sphere_unscoped_prior_volume_cannot_donate_same_address(){unsafe{
    reset();let mut f=F::new();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
    with_volume(true,s,||f.sphere[0x3bc/4]=p);
    assert!(SOURCE_SPHERE_SCOPE.with(|slot|slot.get().is_none()));
    assert_eq!(with_source_sphere(true,g,||s),s);assert_eq!(f.draw(),99);reset();
}}
#[test]
fn source_sphere_nested_non_smoke_scope_cannot_donate_witness(){unsafe{
    reset();let mut f=F::new();let mut non_smoke=F::new();non_smoke.grenade[0]=SMOKE+4;
    let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
    with_source_sphere(true,g,||{
        with_source_sphere(true,non_smoke.gp(),||{with_volume(true,s,||f.sphere[0x3bc/4]=p);s});s
    });assert_eq!(f.draw(),99);assert!(SOURCE_SPHERE_SCOPE.with(|slot|slot.get().is_none()));reset();
}}
#[test]
fn source_sphere_producer_then_particle_or_sphere_replacement_rejects(){unsafe{
    reset();for case in 0..3 {
        let mut f=F::new();let(g,s,p)=(f.gp(),f.sp(),f.pp()as u32);
        with_source_sphere(true,g,||{
            with_volume(true,s,||f.sphere[0x3bc/4]=p);
            match case {0=>f.particle[0x20/4]+=1,1=>f.sphere[0x20/4]+=1,_=>f.sphere[0x3bc/4]=0,} s
        });assert_eq!(f.draw(),99,"replacement after producer {case}");
    }reset();
}}
#[test]
fn source_sphere_scope_drop_clears_pointer_after_unwind(){unsafe{
    reset();let f=F::new();let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
        with_source_sphere(true,f.gp(),||panic!("fixture unwind"));
    }));assert!(result.is_err());assert!(SOURCE_SPHERE_SCOPE.with(|slot|slot.get().is_none()));reset();
}}
