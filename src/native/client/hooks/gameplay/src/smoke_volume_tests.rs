//! Owned policy/lifecycle fixtures. Complete native creator is tested separately.
use super::*;
struct F { sphere: Vec<u32>, component: Vec<u32>, actor: Vec<u32>, node: Vec<u32>,
    record: Vec<u32>, view: Vec<u32>, render: Vec<u32> }
impl F {
    fn new() -> Self {
        let mut f=Self {sphere:vec![0;0x400/4],component:vec![0;0x90/4],actor:vec![0;0x440/4],
            node:vec![0;0x240/4],record:vec![0;1],view:vec![0;0x344/4],render:vec![0;0x20d4/4]};
        f.sphere[0]=SMOKE_SPHERE;f.sphere[0x20/4]=71;f.sphere[0x144/4]=0x12340000;
        f.sphere[0x320/4]=f.component.as_ptr()as u32;
        f.component[0]=SMOKE_COMPONENT;f.component[0x20/4]=81;f.component[0x30/4]=1;
        f.actor[0]=PARTICLE;f.actor[0x20/4]=91;f.actor[0x144/4]=0x12340000;
        f.actor[0x30c/4]=f.node.as_ptr()as u32;f.record[0]=f.node.as_ptr()as u32;
        f.view[0x340/4]=8;f.render[0x20d0/4]=f.view.as_ptr()as u32;f
    }
    fn ap(&self)->usize {self.actor.as_ptr()as usize}
    fn sp(&self)->usize {self.sphere.as_ptr()as usize}
    unsafe fn create(&mut self,enabled:bool)->u32 {
        let ap=self.ap()as u32;with_volume(enabled,self.sp(),||{self.sphere[0x3bc/4]=ap;77})
    }
    unsafe fn draw(&self)->u32 {with_particle(true,self.render.as_ptr()as usize,self.record.as_ptr()as usize,1,||99)}
}
impl Drop for F {fn drop(&mut self){with_delete(self.ap(),||());}}
fn reset(){
    crate::overload_particles::fixture_enable(false);crate::overload_particles::clear();
    *CLOUDS.lock().unwrap()=Clouds::new();HAS_SMOKE.store(false,Ordering::Release);
    *ROOTS.lock().unwrap()=Roots::new();
}
#[test]
fn volume_exact_source_resource_world_and_mode(){unsafe{
    reset();let mut f=F::new();assert_eq!(f.create(true),77);assert_eq!(f.draw(),1);
    for mode in [0,1,7,9,10] {f.view[0x340/4]=mode;assert_eq!(f.draw(),99);}
    f.view[0x340/4]=8;assert_eq!(f.draw(),1);reset();
}}
#[test]
fn volume_unknown_classes_or_null_resource_keep_native(){unsafe{
    reset();for case in 0..9 {
        let mut f=F::new();match case {
            0=>f.sphere[0]=SMOKE,1=>f.sphere[0]=ADV_SMOKE,
            2=>f.component[0]=0x02a3e6dc,3=>f.component[0]=SMOKE_COMPONENT+4,
            4=>f.component[0x30/4]=0,5=>f.sphere[0x144/4]=0,
            6=>f.actor[0x144/4]^=4,7=>f.actor[0]=PARTICLE+4,
            _=>f.sphere[0x320/4]=0,
        }
        assert_eq!(f.create(true),77);assert_eq!(f.draw(),99,"negative case {case}");
    }reset();
}}
#[test]
fn volume_source_or_component_replacement_during_native_rejects(){unsafe{
    reset();for case in 0..7 {
        let mut f=F::new();let ap=f.ap()as u32;
        let mut other=f.component.clone();other[0x20/4]=82;
        let result=with_volume(true,f.sp(),||{
            f.sphere[0x3bc/4]=ap;
            match case {0=>f.sphere[0x20/4]+=1,1=>f.sphere[0x144/4]^=4,
                2=>f.sphere[0]=SMOKE,3=>f.component[0x20/4]+=1,
                4=>f.component[0x30/4]+=1,5=>f.component[0]=0x02a3e6dc,
                _=>f.sphere[0x320/4]=other.as_ptr()as u32,} 42
        });assert_eq!(result,42);assert_eq!(f.draw(),99,"transition case {case}");
    }reset();
}}
#[test]
fn volume_reused_actor_non_smoke_does_not_inherit(){unsafe{
    reset();let mut f=F::new();f.create(true);assert_eq!(f.draw(),1);
    // Same actor serial deliberately: factory reuse must clear independently.
    f.component[0]=0x02a3e6dc;f.create(true);assert_eq!(f.draw(),99);
    f.component[0]=SMOKE_COMPONENT;f.create(true);assert_eq!(f.draw(),1);
    f.actor[0x20/4]+=1;assert_eq!(f.draw(),99);f.create(true);assert_eq!(f.draw(),1);
    with_delete(f.ap(),||());assert_eq!(f.draw(),99);reset();
}}
#[test]
fn volume_native_once_without_locks_and_return_preserved(){unsafe{
    reset();let mut f=F::new();let ap=f.ap()as u32;let mut calls=0;
    assert_eq!(with_volume(true,f.sp(),||{
        calls+=1;assert!(CLOUDS.try_lock().is_ok());assert!(ROOTS.try_lock().is_ok());
        f.sphere[0x3bc/4]=ap;0x12345678u32
    }),0x12345678);assert_eq!(calls,1);assert_eq!(f.draw(),1);reset();
}}
#[test]
fn volume_disabled_null_and_failed_spawn_are_native(){unsafe{
    reset();let mut f=F::new();assert_eq!(f.create(false),77);assert_eq!(f.draw(),99);
    let mut calls=0;assert_eq!(with_volume(true,0,||{calls+=1;37}),37);assert_eq!(calls,1);
    f.sphere[0x3bc/4]=0;assert_eq!(with_volume(true,f.sp(),||81),81);assert_eq!(f.draw(),99);reset();
}}
#[test]
fn volume_pending_root_then_native_creation_refresh(){unsafe{
    reset();let mut f=F::new();f.actor[0x30c/4]=0;f.create(true);assert_eq!(f.draw(),99);
    let ap=f.ap();with_node_creation(true,ap,||f.actor[0x30c/4]=f.node.as_ptr()as u32);
    assert_eq!(f.draw(),1);assert_eq!(f.node[0x20/4],0);reset();
}}
#[test]
fn volume_nested_callbacks_and_deinit_are_bounded(){unsafe{
    reset();let mut outer=F::new();let mut inner=F::new();let ap=outer.ap()as u32;
    with_volume(true,outer.sp(),||{inner.create(true);outer.sphere[0x3bc/4]=ap;});
    assert_eq!(outer.draw(),1);assert_eq!(inner.draw(),1);deinit();
    assert_eq!(outer.draw(),99);assert_eq!(inner.draw(),99);reset();
}}
#[test]
fn volume_capacity_rejects_unknown_without_eviction(){unsafe{
    reset();let mut items:Vec<F>=(0..CAPACITY+1).map(|_|F::new()).collect();
    for (i,f) in items.iter_mut().enumerate(){f.create(true);assert_eq!(f.draw(),if i<CAPACITY {1}else{99});}
    assert_eq!(items[0].draw(),1);reset();
}}
#[test]
fn volume_temporary_diagnostic_budget_saturates(){unsafe{
    reset();VOLUME_DIAGNOSTICS.store(0,Ordering::Relaxed);let mut f=F::new();
    for _ in 0..20 {f.create(true);}assert_eq!(VOLUME_DIAGNOSTICS.load(Ordering::Relaxed),8);reset();
}}
