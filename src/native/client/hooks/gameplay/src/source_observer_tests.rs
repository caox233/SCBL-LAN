//! Owned middleware fixtures only; no game process/GPU. Run serially with the
//! existing module tests because production registry state is intentionally global.
use super::*;
fn reset(){
    SOURCE_PROBE_BITS.store(0,Ordering::Relaxed);
    crate::overload_particles::fixture_enable(false);crate::overload_particles::clear();
    *CLOUDS.lock().unwrap()=Clouds::new();HAS_SMOKE.store(false,Ordering::Release);
    *ROOTS.lock().unwrap()=Roots::new();
}
fn seen(bit:u32)->bool{SOURCE_PROBE_BITS.load(Ordering::Relaxed)&(1<<bit)!=0}
struct F{source:Vec<u32>,component:Vec<u32>,origin:Vec<u32>,actor:Vec<u32>,node:Vec<u32>,record:Vec<u32>,view:Vec<u32>,render:Vec<u32>}
impl F{
 fn new()->Self{
  let mut f=Self{source:vec![0;0x880/4],component:vec![0;0x90/4],origin:vec![0;0x618/4],actor:vec![0;0x440/4],node:vec![0;0x240/4],record:vec![0;1],view:vec![0;0x344/4],render:vec![0;0x20d4/4]};
  f.source[0]=SMOKE;f.source[0x20/4]=51;f.source[0x144/4]=0x12340000;
  f.source[0x610/4]=71;f.source[0x864/4]=71;
  f.source[0x320/4]=f.component.as_ptr()as u32;f.source[0x324/4]=f.origin.as_ptr()as u32;f.source[0x33c/4]=71;
  f.component[0]=SMOKE_COMPONENT;f.component[0x20/4]=61;f.component[0x30/4]=71;
  f.origin[0]=SMOKE;f.origin[0x144/4]=0x12340000;f.origin[0x610/4]=71;
  f.actor[0]=PARTICLE;f.actor[0x20/4]=81;f.actor[0x144/4]=0x12340000;
  f.actor[0x30c/4]=f.node.as_ptr()as u32;f.record[0]=f.node.as_ptr()as u32;
  f.view[0x340/4]=8;f.render[0x20d0/4]=f.view.as_ptr()as u32;f
 }
 fn sp(&self)->usize{self.source.as_ptr()as usize}
 fn ap(&self)->usize{self.actor.as_ptr()as usize}
 unsafe fn draw(&self)->u32{with_particle(true,self.render.as_ptr()as usize,self.record.as_ptr()as usize,1,||99)}
}
impl Drop for F{fn drop(&mut self){with_delete(self.ap(),||());}}
#[test]
fn observer_bit_budget_once_and_out_of_range_are_safe(){
 let bits=std::sync::atomic::AtomicU32::new(0);let mut lines=0;
 for _ in 0..20{for category in 0..SOURCE_PROBE_LIMIT{lines+=u32::from(source_probe_claim(&bits,category));}}
 assert_eq!(lines,22);assert!(!source_probe_claim(&bits,22));assert!(!source_probe_claim(&bits,u32::MAX));
 assert_eq!(bits.load(Ordering::Relaxed),(1<<22)-1);
}
#[test]
fn observer_concurrent_claim_emits_once_per_category(){
 let bits=std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));let mut jobs=Vec::new();
 for _ in 0..8{let bits=bits.clone();jobs.push(std::thread::spawn(move||{
   (0..22).filter(|c|source_probe_claim(&bits,*c)).count()
 }));}
 assert_eq!(jobs.into_iter().map(|h|h.join().unwrap()).sum::<usize>(),22);
}
#[test]
fn observer_absent_entry_is_distinct_from_disabled_or_rejected(){unsafe{
 reset();assert_eq!(SOURCE_PROBE_BITS.load(Ordering::Relaxed),0);
 let mut calls=0;assert_eq!(with_spawn(false,0,0,||{calls+=1;0}),0);
 assert_eq!(calls,1);assert!(seen(0)&&seen(1));assert!(!seen(8)&&!seen(16));
 assert_eq!(with_volume(true,0,||37u32),37);assert!(seen(8)&&seen(10));
 assert_eq!(with_continuous(true,0,||(8u32,9u32)),(8,9));assert!(seen(16)&&seen(18));reset();
}}
#[test]
fn observer_factory_all_prequalification_reasons_preserve_native(){unsafe{
 for (case,bit) in [(0,2),(1,3),(2,4),(3,5),(4,6),(5,7)]{
  reset();let mut f=F::new();let ap=f.ap();let mut calls=0;
  match case{1=>f.source[0]=0x02ad2abc,2=>f.source[0]=SMOKE_SPHERE,3=>f.source[0x610/4]=72,4=>f.source[0x144/4]=0,_=>{}}
  let sp=if case==0{0}else{f.sp()};
  assert_eq!(with_spawn(true,sp,71,||{calls+=1;ap}),ap);
  assert_eq!(calls,1);assert!(seen(0)&&seen(bit));assert_eq!(f.draw(),if case==5{1}else{99});
 }reset();
}}
#[test]
fn observer_sphere_origin_match_is_diagnostic_not_suppression(){unsafe{
 reset();let mut f=F::new();f.source[0]=SMOKE_SPHERE;f.component[0]=0x02a3e6dc;
 let probe=source_probe_factory(true,f.sp(),71,false).unwrap();
 assert_eq!(probe.source_kind,"sphere");assert_eq!(probe.component_kind,"explosion");
 assert_eq!(probe.origin_kind,"smoke");assert!(probe.definition_match&&probe.origin_resource_match);
 assert_eq!(with_spawn(true,f.sp(),71,||f.ap()),f.ap());assert_eq!(f.draw(),99);reset();
}}
#[test]
fn observer_volume_rejection_matrix_and_valid_scope_unchanged(){unsafe{
 for (case,bit) in [(0,9),(1,10),(2,11),(3,12),(4,13),(5,14),(6,15)]{
  reset();let mut f=F::new();f.source[0]=SMOKE_SPHERE;let ap=f.ap()as u32;
  match case{1=>f.source[0]=SMOKE,2=>f.source[0x320/4]=0,3=>f.component[0]=0x02a3e6dc,
   4=>f.component[0x30/4]=0,5=>f.source[0x144/4]=0,_=>{}}
  let mut calls=0;assert_eq!(with_volume(case!=0,f.sp(),||{calls+=1;f.source[0x3bc/4]=ap;123}),123);
  assert_eq!(calls,1);assert!(seen(8)&&seen(bit));assert_eq!(f.draw(),if case==6{1}else{99});
 }reset();
}}
#[test]
fn observer_continuous_rejection_matrix_keeps_return_and_scope(){unsafe{
 for (case,bit) in [(0,17),(1,18),(2,19),(3,20),(4,21)]{
  reset();let mut f=F::new();f.source[0]=ADV_SMOKE;let ap=f.ap()as u32;
  match case{1=>f.source[0]=SMOKE,2=>f.source[0x864/4]=0,3=>f.source[0x144/4]=0,_=>{}}
  let mut calls=0;assert_eq!(with_continuous(case!=0,f.sp(),||{calls+=1;f.source[0x860/4]=ap;456u32}),456);
  assert_eq!(calls,1);assert!(seen(16)&&seen(bit));assert_eq!(f.draw(),if case==4{1}else{99});
 }reset();
}}
#[test]
fn observer_repeated_qualified_paths_do_not_resnapshot(){unsafe{
 reset();let f=F::new();assert!(source_probe_factory(true,f.sp(),71,true).is_some());
 // Claim precedes snapshot reads. Once qualified category is observed, even an
 // unreadable argument is not touched by the diagnostic qualified fast path.
 assert!(source_probe_factory(true,0,71,true).is_none());
 assert!(source_probe_volume(true,f.sp(),true).is_some());assert!(source_probe_volume(true,0,true).is_none());
 assert!(source_probe_continuous(true,f.sp(),true).is_some());assert!(source_probe_continuous(true,0,true).is_none());reset();
}}
#[test]
fn observer_native_runs_without_registry_locks_and_nested_calls_work(){unsafe{
 reset();let f=F::new();let mut calls=0;
 assert_eq!(with_spawn(true,f.sp(),71,||{
  calls+=1;assert!(ROOTS.try_lock().is_ok());assert!(CLOUDS.try_lock().is_ok());
  assert_eq!(with_volume(false,0,||77),77);f.ap()
 }),f.ap());assert_eq!(calls,1);assert_eq!(f.draw(),1);assert!(seen(0)&&seen(7)&&seen(8)&&seen(9));reset();
}}
#[test]
fn observer_budget_is_per_dll_not_reset_by_deinit(){unsafe{
 reset();with_spawn(false,0,0,||0);let before=SOURCE_PROBE_BITS.load(Ordering::Relaxed);
 deinit();assert_eq!(SOURCE_PROBE_BITS.load(Ordering::Relaxed),before);assert!(!source_probe_claim(&SOURCE_PROBE_BITS,1));reset();
}}
