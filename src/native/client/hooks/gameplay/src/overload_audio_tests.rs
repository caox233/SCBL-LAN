use super::*;
struct Scene {ability:Vec<u32>,owner:Vec<u32>,world:Vec<u32>,info:Vec<u32>,list:Vec<u32>,pc:Vec<u32>,pawn:Vec<u32>,effect:Vec<u32>,params:[u32;12]}
impl Scene {
 fn new()->Self {
  let mut s=Self{ability:vec![0;0x100/4],owner:vec![0;0x950/4],world:vec![0;0x700/4],info:vec![0;0x380/4],list:vec![0;1],pc:vec![0;0x700/4],pawn:vec![0;0x950/4],effect:vec![0;0x310/4],params:[0;12]};
  s.ability[0]=0x02a3f3a4;s.ability[0x6c/4]=s.owner.as_ptr() as u32;s.ability[0xa0/4]=2000f32.to_bits();
  s.owner[0]=0x02ad3234;s.owner[0x20/4]=71;s.owner[0x374/4]=0x40000;s.owner[0x144/4]=s.world.as_ptr() as u32;
  s.world[0x6d0/4]=s.info.as_ptr() as u32;s.info[0x36c/4]=s.list.as_ptr() as u32;s.info[0x370/4]=1;s.info[0x374/4]=1;
  s.list[0]=s.pc.as_ptr() as u32;s.pc[0]=0x02ac53ec;s.pc[0x60/4]=0x60000;s.pc[0x4c/4]=1<<8;s.pc[0x144/4]=s.world.as_ptr() as u32;
  s.pc[0x310/4]=s.pawn.as_ptr() as u32;s.pc[0x520/4]=s.pawn.as_ptr() as u32;s.pc[0x55c/4]=5<<8;
  s.pawn[0]=0x02ad2abc;s.pawn[0x374/4]=0x80000;s.pawn[0x460/4]=100;s.pawn[0x144/4]=s.world.as_ptr() as u32;s.pawn[0x938/4]=s.pc.as_ptr() as u32;
  s.effect[0]=0x029e8754;s.effect[0x20/4]=93;s.effect[0x144/4]=s.world.as_ptr() as u32;
  s.params[0]=100;s.params[1]=s.owner.as_ptr() as u32;s.params[2]=2;s.params[3]=0x15;
  s
 }
 fn ability(&self)->usize{self.ability.as_ptr() as usize}
 unsafe fn check(&self,event:u32,phase:Phase)->bool{with_phase(self.ability(),phase,||eligible(event,self.params.as_ptr()))}
}
struct Enabled;
impl Enabled {fn new()->Self{enabled(false);enabled(true);Self}}
impl Drop for Enabled{fn drop(&mut self){enabled(false);assert_eq!(SCOPE.with(Cell::get),None);}}

#[test]
fn only_two_native_events_in_their_phase_receive_gain(){unsafe{
 let _guard=Enabled::new();let s=Scene::new();
 assert!(s.check(0x008359b4,Phase::Prepare));assert!(s.check(0x008359bb,Phase::Release));
 assert!(!s.check(0x008359bb,Phase::Prepare)); // cancellation OUT is untouched
 assert!(!s.check(0x008359b4,Phase::Release));assert!(!s.check(123,Phase::Prepare));
 assert!(!eligible(PREPARE,s.params.as_ptr()));assert!(!eligible(RELEASE,s.params.as_ptr()));
}}
#[test]
fn distance_uses_actual_ability_radius_and_all_three_axes(){unsafe{
 let _guard=Enabled::new();let mut s=Scene::new();
 for (d,expected) in [(0f32,true),(1999.9,true),(2000.,true),(2000.1,false)] {
  s.pawn[0x9c/4]=d.to_bits();assert_eq!(s.check(PREPARE,Phase::Prepare),expected);
 }
 s.ability[0xa0/4]=1000f32.to_bits();s.pawn[0x9c/4]=1000f32.to_bits();assert!(s.check(PREPARE,Phase::Prepare));
 s.pawn[0xa4/4]=1f32.to_bits();assert!(!s.check(PREPARE,Phase::Prepare));
 for bad in [0.,-1.,f32::NAN,f32::INFINITY]{s.ability[0xa0/4]=bad.to_bits();assert!(!s.check(PREPARE,Phase::Prepare));}
}}
#[test]
fn dead_spectator_spy_invalid_and_foreign_listeners_keep_native(){unsafe{
 let _guard=Enabled::new();
 for fault in 0..13 {
  let mut s=Scene::new();match fault {
   0=>s.pawn[0]=0x02ad3234,1=>s.pawn[0x374/4]=0x40000,2=>s.pawn[0x460/4]=0,3=>s.pawn[0x460/4]=u32::MAX,
   4=>s.pc[0x520/4]=0,5=>s.pc[0x310/4]=0,6=>s.pawn[0x938/4]=0,7=>s.pawn[0x144/4]=1,
   8=>s.pawn[0x5c/4]=1,9=>s.pc[0]=0,10=>s.pawn[0x9c/4]=f32::NAN.to_bits(),11=>s.info[0x374/4]=0,_=>s.owner[0]=0x02ad2abc,
  }
  assert!(!s.check(PREPARE,Phase::Prepare),"fault={fault}");
 }
}}
#[test]
fn request_identity_and_spatial_flags_are_not_guessed(){unsafe{
 let _guard=Enabled::new();
 for fault in 0..6 {
  let mut s=Scene::new();match fault {0=>s.params[0]=0,1=>s.params[1]=1,2=>s.params[2]=1,3=>s.params[3]=5,4=>s.owner[0x374/4]=0,_=>s.owner[0x5c/4]=1}
  assert!(!s.check(PREPARE,Phase::Prepare),"fault={fault}");
 }
 let s=Scene::new();prepare(s.ability(),||assert!(!eligible(PREPARE,std::ptr::null())));
}}
#[test]
fn local_and_remote_emitter_roles_do_not_change_gain(){unsafe{
 let _guard=Enabled::new();let mut s=Scene::new();
 for role in [1,2,3] {s.owner[0x4c/4]=role<<8;assert!(s.check(PREPARE,Phase::Prepare));}
 let mut remote=vec![0u32;0x200/4];remote[0x60/4]=0x60000;remote[0x4c/4]=2<<8;remote[0x144/4]=s.world.as_ptr() as u32;
 s.list=vec![remote.as_ptr() as u32,s.pc.as_ptr() as u32];s.info[0x36c/4]=s.list.as_ptr() as u32;s.info[0x370/4]=2;s.info[0x374/4]=2;
 assert!(s.check(PREPARE,Phase::Prepare));s.list.swap(0,1);assert!(s.check(PREPARE,Phase::Prepare));
 remote[0x4c/4]=1<<8;assert!(!s.check(PREPARE,Phase::Prepare));
}}
#[test]
fn particle_initialization_and_deferred_release_preserve_identity(){unsafe{
 let _guard=Enabled::new();let mut s=Scene::new();let effect=s.effect.as_ptr() as usize;s.params[1]=effect as u32;
 assert!(!s.check(PREPARE,Phase::Prepare));assert!(s.check(RELEASE,Phase::Release));
 record(s.ability(),effect);assert!(eligible(RELEASE,s.params.as_ptr()));
 assert!(!eligible(PREPARE,s.params.as_ptr()));s.effect[0x20/4]+=1;assert!(!eligible(RELEASE,s.params.as_ptr()));
 s.effect[0x20/4]-=1;forget(effect);assert!(!eligible(RELEASE,s.params.as_ptr()));
 record(s.ability(),effect);s.ability[0]=0;record(s.ability(),effect);assert!(!eligible(RELEASE,s.params.as_ptr()));
 s.ability[0]=0x02a3f3a4;record(s.ability(),effect);enabled(false);assert!(!eligible(RELEASE,s.params.as_ptr()));
 enabled(true);assert!(!eligible(RELEASE,s.params.as_ptr()));
}}
#[test]
fn scopes_restore_on_nesting_unwind_and_other_threads(){unsafe{
 let _guard=Enabled::new();let s=Scene::new();
 prepare(s.ability(),|| {
  assert!(eligible(PREPARE,s.params.as_ptr()));
  prepare(0,||assert!(!eligible(PREPARE,s.params.as_ptr())));
  assert!(eligible(PREPARE,s.params.as_ptr()));
  assert_eq!(std::thread::spawn(||SCOPE.with(Cell::get)).join().unwrap(),None);
  let _=std::panic::catch_unwind(||release(0,||panic!("isolated unwind")));
  assert!(eligible(PREPARE,s.params.as_ptr()));
 });
}}
// Used by the transaction harness inside the actual existing Send-hook module.
// It replaces only the external native Send and gain-queue dependencies.
pub(crate) unsafe fn fixture_gain_cases(mut call:impl FnMut(&str,u32,*const u32,bool)) {
 let _guard=Enabled::new();let mut s=Scene::new();s.pawn[0x9c/4]=1500f32.to_bits();
 prepare(s.ability(),||call("prepare",PREPARE,s.params.as_ptr(),true));
 release(s.ability(),||call("release",RELEASE,s.params.as_ptr(),true));
 prepare(s.ability(),||call("cancel",RELEASE,s.params.as_ptr(),false));
 s.pawn[0x9c/4]=2000.1f32.to_bits();prepare(s.ability(),||call("outside",PREPARE,s.params.as_ptr(),false));
 s.pawn[0x9c/4]=2000f32.to_bits();prepare(s.ability(),||call("edge",PREPARE,s.params.as_ptr(),true));
 s.pawn[0x460/4]=0;release(s.ability(),||call("dead",RELEASE,s.params.as_ptr(),false));s.pawn[0x460/4]=100;
 s.pc[0x520/4]=0;release(s.ability(),||call("death_camera",RELEASE,s.params.as_ptr(),false));s.pc[0x520/4]=s.pawn.as_ptr() as u32;
 s.params[1]=s.effect.as_ptr() as u32;record(s.ability(),s.effect.as_ptr() as usize);
 call("deferred_particle",RELEASE,s.params.as_ptr(),true);
 s.effect[0x20/4]+=1;call("reused_particle",RELEASE,s.params.as_ptr(),false);s.effect[0x20/4]-=1;
 forget(s.effect.as_ptr() as usize);call("deleted_particle",RELEASE,s.params.as_ptr(),false);
 enabled(false);prepare(s.ability(),||call("disabled",PREPARE,s.params.as_ptr(),false));
}
