use super::*;
use std::cell::Cell;
struct Scene {body:Vec<u32>,world:Vec<u32>,info:Vec<u32>,list:Vec<u32>,pc:Vec<u32>,pawn:Vec<u32>}
impl Scene {
 fn new()->Self {
  let mut s=Self{body:vec![0;0xc00/4],world:vec![0;0x700/4],info:vec![0;0x380/4],list:vec![0;1],pc:vec![0;0x530/4],pawn:vec![0;0x950/4]};
  s.body[0]=DRONE;s.body[0x144/4]=s.world.as_ptr() as u32;s.body[0x5dc/4]=600;
  s.body[0x910/4]=2;s.body[0x4c/4]=2<<8;
  s.world[0x6d0/4]=s.info.as_ptr() as u32;s.info[0x36c/4]=s.list.as_ptr() as u32;s.info[0x370/4]=1;s.info[0x374/4]=1;
  s.pc[0x144/4]=s.world.as_ptr() as u32;s.pc[0x60/4]=0x60000;s.pc[0x4c/4]=1<<8;
  s.list[0]=s.pc.as_ptr() as u32;s.pc[0]=CONTROLLER;s.pc[0x310/4]=s.pawn.as_ptr() as u32;s.pc[0x520/4]=s.pawn.as_ptr() as u32;
  s.pawn[0]=SPY;s.pawn[0x938/4]=s.pc.as_ptr() as u32;s.pawn[0x144/4]=s.world.as_ptr() as u32;s.pawn[0x374/4]=1<<18;s
 }
 fn body(&self)->usize {self.body.as_ptr() as usize}
}
#[test]
fn local_spy_from_native_local_array_accepts_remote_drone_not_owner_only() {
 let s=Scene::new();
 assert!(unsafe{body_alive(s.body())&&spy_listener(s.body())});
 assert_eq!(s.body[0x4c/4],2<<8);
}
#[test]
fn mercenary_campaign_spectator_foreign_world_ambiguous_local_players_retain_native_audio() {
 unsafe {for fault in 0..10 {
  let mut s=Scene::new();
  match fault {
   0=>s.pawn[0]=0x2ad2abc,1=>s.pawn[0x374/4]=1<<19,2=>s.pawn[0x374/4]=0,
   3=>s.pc[0x310/4]=0,4=>s.pawn[0x938/4]=0,5=>s.pawn[0x144/4]=0,
   6=>s.info[0x370/4]=2,7=>s.info[0x374/4]=0,8=>s.pc[0]=0,_=>s.info[0x36c/4]=0
  }
  assert!(!spy_listener(s.body()),"fault={fault}");
 }}
}
#[test]
fn dead_destroying_foreign_drone_fails_closed() {
 unsafe {for fault in 0..4 {
  let mut s=Scene::new();match fault {0=>s.body[0]=0,1=>s.body[0x5dc/4]=0,2=>s.body[0x910/4]=3,_=>s.body[0x4b0/4]=0x80000000}
  assert!(!body_alive(s.body()));
 }}
}
#[test]
fn request_scope_is_stack_bound_and_unrelated_nested_audio_does_not_inherit() {
 assert_eq!(SCOPE.with(|s|s.get()),None);
 scoped(Some(0x10000),||{
  assert_eq!(SCOPE.with(|s|s.get()),Some(0x10000));
  scoped(None,||assert_eq!(SCOPE.with(|s|s.get()),None));
  scoped(Some(0x20000),||assert_eq!(SCOPE.with(|s|s.get()),Some(0x20000)));
  assert_eq!(SCOPE.with(|s|s.get()),Some(0x10000));
 });
 assert_eq!(SCOPE.with(|s|s.get()),None);
}
#[test]
fn panic_does_not_leave_stale_audio_scope() {
 let result=std::panic::catch_unwind(||scoped(Some(0x10000),||panic!("fixture")));
 assert!(result.is_err());assert_eq!(SCOPE.with(|s|s.get()),None);
}
#[test]
fn only_matching_actor_and_spatial_subkey_receive_gain() {
 unsafe {
  let mut params=[0u32;12];params[0]=100;params[1]=0x10000;params[2]=2;params[3]=0x15;
  assert!(request_matches(0x10000,params.as_ptr()));
  assert!(!request_matches(0x20000,params.as_ptr()));
  params[2]=1;assert!(!request_matches(0x10000,params.as_ptr()));
  params[2]=2;params[3]=5;assert!(!request_matches(0x10000,params.as_ptr()));
  assert!(!request_matches(0x10000,std::ptr::null()));
 }
}
#[test]
fn gain_is_submitted_once_only_for_successful_eligible_requests_and_preserves_return() {
 let calls=Cell::new(0);
 for result in [0,123,0xffff_fffe] {
  assert_eq!(finish_request(result,true,|id,db,fade,curve|{
   assert_eq!(id,result);assert_eq!(db,GAIN_DB);assert_eq!((fade,curve),(0.,0));
   calls.set(calls.get()+1);
  }),result);
 }
 assert_eq!(calls.get(),3);
 assert_eq!(finish_request(u32::MAX,true,|_,_,_,_|panic!("failed")),u32::MAX);
 assert_eq!(finish_request(11,false,|_,_,_,_|panic!("foreign")),11);
}
#[test]
fn disabled_adapter_passes_player_electronics_and_all_arguments_without_native_probe() {
 let count=Cell::new(0);
 let r=unsafe{with_request(0,0,7,||{count.set(count.get()+1);123})};
 assert_eq!(r,123);assert_eq!(count.get(),1);assert_eq!(SCOPE.with(|s|s.get()),None);
}
#[test]
fn prepare_rejects_unknown_code_and_keeps_original_immutable() {
 let root=std::path::PathBuf::from(std::env::var("SCBL_R58_C26_FIXTURE").unwrap());
 let code=std::fs::read(root.join("gain_command.bin")).unwrap();let old=code.clone();
 let output=prepare(&code,0x10000000).unwrap();assert_eq!(code,old);
 assert_eq!(&output[0x25..0x2c],&[0xdd,0xd8,0xe9,0x27,0,0,0]);
 let mut bad=code;bad[0]^=1;assert!(prepare(&bad,0x10000000).is_none());assert!(prepare(&[],0x10000000).is_none());
}

mod native_queue {use super::*;

use std::ffi::c_void;
static CAPTURE: std::sync::Mutex<Vec<(u32,f32,f32,u32)>>=std::sync::Mutex::new(Vec::new());
#[link(name="kernel32")]
extern "system" {
 fn VirtualAlloc(p:*mut c_void,n:usize,t:u32,k:u32)->*mut c_void;
 fn VirtualProtect(p:*mut c_void,n:usize,k:u32,old:*mut u32)->i32;
 fn VirtualFree(p:*mut c_void,n:usize,t:u32)->i32;
 fn FlushInstructionCache(h:*mut c_void,p:*const c_void,n:usize)->i32;
 fn GetCurrentProcess()->*mut c_void;
}
unsafe extern "thiscall" fn context(p:usize)->usize {p}
unsafe extern "thiscall" fn get_context(_:usize)->u32 {2}
unsafe extern "thiscall" fn queue(_:usize,cmd:usize)->u32 {
 let get=|n:usize|std::ptr::read_unaligned((cmd+n) as *const u32);
 CAPTURE.lock().unwrap().push((get(0x10),f32::from_bits(get(0x24)),f32::from_bits(get(0x1c)),get(0x18)));
 0
}
unsafe extern "fastcall" fn cookie(value:u32,_:u32) {assert_eq!(value,0);}
unsafe fn bank(native:bool)->usize {
 let path=std::path::PathBuf::from(std::env::var("SCBL_R58_C26_FIXTURE").unwrap()).join("gain_command.bin");
 let original=std::fs::read(path).unwrap();
 let at=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;assert_ne!(at,0);
 let mut code=original.clone();
 if !native {code=prepare(&original,at).unwrap();}
 // Preserve native frame, request/gain/fade/curve writes, and queue sequence.
 // Only dependencies are mocked; this does not start the game's sound mixer.
 for (off,to) in [(0x18,context as *const () as usize),(0x6f,context as *const () as usize),(0x7c,context as *const () as usize),(0xbe,context as *const () as usize),(0xcb,cookie as *const () as usize)] {
  assert_eq!(code[off],0xe8);
  code[off+1..off+5].copy_from_slice(&(to as u32).wrapping_sub((at+off+5) as u32).to_le_bytes());
 }
 // Stable fixture globals: zero cookie, -96dB limit, a fake queue object.
 *((at+0x500) as *mut f32)=-96.;
 *((at+0x510) as *mut u32)=0;
 *((at+0x520) as *mut u32)=(at+0x530) as u32;
 *((at+0x530) as *mut u32)=(at+0x550) as u32;
 *((at+0x550+0x2c) as *mut u32)=get_context as *const () as u32;
 *((at+0x550+0x1c) as *mut u32)=queue as *const () as u32;
 for (old,new) in [(0x3288bbcu32,at+0x510),(0x29fe230,at+0x500),(0x34b1d28,at+0x520)] {
  for i in 0..code.len()-3 {
   if code[i..i+4]==old.to_le_bytes(){code[i..i+4].copy_from_slice(&(new as u32).to_le_bytes());}
  }
 }
 std::ptr::copy_nonoverlapping(code.as_ptr(),at as *mut u8,code.len());
 let mut old=0;assert_ne!(VirtualProtect(at as _,4096,0x20,&mut old),0);
 FlushInstructionCache(GetCurrentProcess(),at as _,code.len());at
}
#[test]
fn c26_same_input_native_request_gain() {
 unsafe {
  CAPTURE.lock().unwrap().clear();
  let native=bank(true);let variant=bank(false);
  let f:unsafe extern "C" fn(u32,f32,f32,u32)=std::mem::transmute(native);
  f(100,3.521825,0.25,2);
  assert_eq!(CAPTURE.lock().unwrap()[0],(100,0.,0.25,2));
  let f:unsafe extern "C" fn(u32,f32,f32,u32)=std::mem::transmute(variant);
  f(101,3.521825,0.25,2);
  let v=CAPTURE.lock().unwrap()[1];
  assert_eq!((v.0,v.2,v.3),(101,0.25,2));
  let amplitude=10.0f32.powf(v.1/20.);
  println!("C26_SAME_INPUT queued_gain_db={:.6} amplitude_factor={:.3} request=101 fade=0.25 curve=2 engine_mixer_live=false",v.1,amplitude);
  assert!((amplitude-1.5f32).abs()<0.00001);
  VirtualFree(native as _,0,0x8000);VirtualFree(variant as _,0,0x8000);
 }
}

}

#[test]
fn real_two_player_scene_only_local_spy_receives_gain() {
 let mut s=Scene::new();let mut remote=vec![0u32;0x200/4];
 remote[0x60/4]=0x60000;remote[0x4c/4]=2<<8;remote[0x144/4]=s.world.as_ptr() as u32;
 s.list=vec![remote.as_ptr() as u32,s.pc.as_ptr() as u32];
 s.info[0x36c/4]=s.list.as_ptr() as u32;s.info[0x370/4]=2;s.info[0x374/4]=2;
 assert!(unsafe {spy_listener(s.body())});
 s.list.swap(0,1);assert!(unsafe {spy_listener(s.body())});
 s.pawn[0]=0x2ad2abc;assert!(!unsafe {spy_listener(s.body())});
}
