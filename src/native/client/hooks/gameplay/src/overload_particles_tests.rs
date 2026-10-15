use crate::overload_particles::{clear,contains,forget,record_spawn};
struct Fixture { ability:Vec<u32>, owner:Vec<u32>, particle:Vec<u32> }
impl Fixture {
 fn new()->Self {
  let mut f=Self{ability:vec![0;64],owner:vec![0;128],particle:vec![0;256]};
  f.ability[0]=0x02a3f3a4;f.ability[0x6c/4]=f.owner.as_ptr() as u32;
  f.owner[0x144/4]=0x12340000;f.particle[0]=0x029e8754;
  f.particle[0x350/4]=0x12341000;f.particle[0x20/4]=123;f.particle[0x144/4]=f.owner[0x144/4];f
 }
 fn p(&self)->usize{self.particle.as_ptr() as usize}
 unsafe fn add(&self){record_spawn(self.ability.as_ptr() as usize,self.p());}
}
#[test]
fn rejects_unknown_source_particle_cross_world_and_null(){unsafe{
 clear();let mut f=Fixture::new();
 for case in 0..5 {f.ability[0]=0x02a3f3a4;f.particle[0]=0x029e8754;f.particle[0x144/4]=0x12340000;f.ability[0x6c/4]=f.owner.as_ptr() as u32;
  match case {0=>f.ability[0]=0,1=>f.particle[0]=0,2=>f.particle[0x144/4]=0x22220000,3=>f.ability[0x6c/4]=0,_=>f.particle[0x144/4]=0}
  f.add();assert!(!contains(f.p()));
 }
 record_spawn(0,0);assert!(!contains(0));clear();
}}
#[test]
fn reused_identity_and_new_unknown_spawn_do_not_inherit(){unsafe{
 clear();let mut f=Fixture::new();f.add();assert!(contains(f.p()));
 f.particle[0x20/4]+=1;assert!(!contains(f.p()));f.add();assert!(contains(f.p()));
 f.ability[0]=0;f.add();assert!(!contains(f.p()));clear();
}}
#[test]
fn bounded_registry_fails_native_without_evicting_live_entries(){unsafe{
 clear();let fixtures:Vec<_>=(0..129).map(|_|Fixture::new()).collect();
 for f in &fixtures {f.add();}
 assert!(fixtures[..128].iter().all(|f|contains(f.p())));assert!(!contains(fixtures[128].p()));
 forget(fixtures[0].p());fixtures[128].add();assert!(contains(fixtures[128].p()));clear();
}}
#[test]
fn simulation_to_render_thread_identity_is_visible_and_read_only(){unsafe{
 clear();let f=Fixture::new();let before=(f.ability.clone(),f.owner.clone(),f.particle.clone());f.add();
 let p=f.p();assert!(std::thread::spawn(move||contains(p)).join().unwrap());
 assert_eq!(before,(f.ability,f.owner,f.particle));clear();
}}
#[test]
fn deleting_one_effect_and_shutdown_never_retain_others_incorrectly(){unsafe{
 clear();let a=Fixture::new();let b=Fixture::new();a.add();b.add();
 forget(a.p());assert!(!contains(a.p()));assert!(contains(b.p()));forget(0);
 assert!(contains(b.p()));clear();assert!(!contains(b.p()));
}}

#[test]
fn shared_draw_viewer_and_lifetime_policy() {unsafe{
 use crate::overload_particles::{fixture_capture,fixture_enable};
 fixture_enable(false);clear();
 let mut f=Fixture::new();let mut world=vec![0u32;0x700/4];let mut info=vec![0u32;0x400/4];
 let mut pc=vec![0u32;0x1000/4];let mut pawn=vec![0u32;0x2000/4];let array=[pc.as_ptr() as u32];
 let wp=world.as_ptr() as u32;info[0x144/4]=wp;info[0x330/4]=0x10;pawn[0x1410/4]=0x12341000;world[0x6d0/4]=info.as_ptr() as u32;info[0x36c/4]=array.as_ptr() as u32;info[0x370/4]=1;info[0x374/4]=1;
 pc[0]=0x02ac53ec;pc[0x60/4]=0x40000;pc[0x144/4]=wp;pc[0x6dc/4]=1;pc[0x6a4/4]=1;
 f.owner[0x144/4]=wp;f.particle[0x144/4]=wp;
 let mut node=vec![0u32;0x240/4];node[0x20/4]=f.p() as u32;f.particle[0x30c/4]=node.as_ptr() as u32;
 let record=[node.as_ptr() as u32];let rp=record.as_ptr() as usize;
 let draw=||crate::thermal_smoke::with_particle(true,0,rp,1,||99);
 fixture_capture(f.ability.as_ptr() as usize,f.p());assert!(!contains(f.p()));
 fixture_enable(true);fixture_capture(f.ability.as_ptr() as usize,f.p());assert!(contains(f.p()));
 for (kind,class,flags) in [(5u8,0x02ad2abc,0x80000),(4,0x02ad3234,0x40000)] {
  (pc.as_mut_ptr() as *mut u8).add(0x55d).write(kind);
  pawn[0]=class;pawn[0x374/4]=flags;pawn[0x144/4]=wp;pawn[0x938/4]=pc.as_ptr() as u32;
  pc[0x520/4]=pawn.as_ptr() as u32;crate::overload_visual_policy::fixture_update(info.as_ptr() as usize,|_|true,||{});assert_eq!(draw(),if kind==5{1}else{99});
  pc[0x520/4]=0;assert_eq!(draw(),1); // all death cameras hidden
 }
 (pc.as_mut_ptr() as *mut u8).add(0x55d).write(5);assert_eq!(draw(),1);
 pc[0x6dc/4]=u32::MAX;assert_eq!(draw(),1);pc[0x6dc/4]=1;
 pc[0x60/4]=0;assert_eq!(draw(),1);pc[0x60/4]=0x40000;
 pc[0x520/4]=pawn.as_ptr() as u32;assert_eq!(draw(),1);pc[0x520/4]=0; // unknown transient state hidden
 f.particle[0x20/4]+=1;assert_eq!(draw(),99);f.particle[0x20/4]-=1;
 f.particle[0x30c/4]=0;assert_eq!(draw(),99);f.particle[0x30c/4]=node.as_ptr() as u32;
 let observed=std::thread::spawn(move||crate::thermal_smoke::with_particle(true,0,rp,1,||99)).join().unwrap();assert_eq!(observed,1);
 crate::thermal_smoke::with_delete(f.p(),||assert!(!contains(f.p())));assert_eq!(draw(),99);
 fixture_capture(f.ability.as_ptr() as usize,f.p());assert_eq!(draw(),1);
 crate::thermal_smoke::with_spawn(true,0,0,||f.p());assert_eq!(draw(),99);
 fixture_capture(f.ability.as_ptr() as usize,f.p());assert_eq!(draw(),1);
 crate::overload_particles::deinit();assert_eq!(draw(),99);assert!(!contains(f.p()));
 fixture_capture(f.ability.as_ptr() as usize,f.p());assert!(!contains(f.p()));
}}

#[test]
fn same_template_activation_kept_changed_or_missing_template_fails_native(){unsafe{
 clear();let mut f=Fixture::new();f.add();assert!(contains(f.p()));
 f.particle[0x354/4]=0x55550000;assert!(contains(f.p())); // cached definition is not an epoch
 f.particle[0x350/4]+=4;assert!(!contains(f.p()));
 f.particle[0x350/4]=0;f.add();assert!(!contains(f.p()));clear();
}}

#[test]
fn retiring_stale_overload_keeps_new_shared_smoke_root(){unsafe{
 clear();let mut f=Fixture::new();let node=vec![0u32;0x240/4];
 f.particle[0x30c/4]=node.as_ptr() as u32;f.add();
 assert!(crate::overload_particles::registered_address(f.p()));
 f.particle[0x350/4]+=4;assert!(!contains(f.p()));
 crate::thermal_smoke::track_root(f.p());
 crate::overload_particles::retire_registration(f.p());
 assert!(!crate::overload_particles::registered_address(f.p()));
 assert_eq!(crate::thermal_smoke::particle_owner_node(node.as_ptr() as usize),Some(f.p()));
 crate::thermal_smoke::forget_root(f.p());clear();
}}
