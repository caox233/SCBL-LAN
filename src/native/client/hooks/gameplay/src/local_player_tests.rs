use super::*;
use sha2::{Digest, Sha256};
struct Scene { world:Vec<u32>, wi:Vec<u32>, list:Vec<u32>, pcs:Vec<Vec<u32>> }
impl Scene {
 fn new()->Self {
  let mut s=Self{world:vec![0;0x700/4],wi:vec![0;0x400/4],list:vec![0;2],pcs:vec![vec![0;0x200/4];2]};
  s.world[0x6d0/4]=s.wi.as_ptr() as u32;
  s.wi[0x36c/4]=s.list.as_ptr() as u32;s.wi[0x370/4]=2;s.wi[0x374/4]=2;
  for i in 0..2 {
   s.list[i]=s.pcs[i].as_ptr() as u32;s.pcs[i][0x144/4]=s.world.as_ptr() as u32;
   s.pcs[i][0x60/4]=0x60000;s.pcs[i][0x4c/4]=if i==1 {1<<8}else{2<<8};
  } s
 }
 fn get(&self)->Option<usize>{unsafe{controller(self.world.as_ptr() as usize)}}
}
#[test]
fn live_two_player_layout_chooses_flagged_local_not_first_entry_or_single_player() {
 let mut s=Scene::new();let before=s.pcs.clone();
 assert_eq!(s.get(),Some(s.pcs[1].as_ptr() as usize));
 s.list.swap(0,1);assert_eq!(s.get(),Some(s.pcs[1].as_ptr() as usize));
 assert_eq!(s.pcs,before);
}
#[test]
fn missing_or_ambiguous_local_invalid_extent_foreign_world_fail_closed() {
 for fault in 0..8 {
  let mut s=Scene::new();match fault {
   0=>s.pcs[1][0x60/4]=0,1=>s.pcs[1][0x4c/4]=2<<8,
   2=>s.pcs[0][0x4c/4]=1<<8,3=>s.pcs[1][0x144/4]=0,
   4=>s.wi[0x370/4]=33,5=>s.wi[0x374/4]=1,
   6=>s.wi[0x36c/4]=0,_=>s.list[0]=s.list[1]
  } assert!(s.get().is_none(),"fault={fault}");
 }
}
#[test]
fn retail_predicate_executes_same_local_flag_and_replica_byte_rule() {
 use windows::Win32::System::Memory::{VirtualAlloc,VirtualFree,MEM_COMMIT,MEM_RESERVE,MEM_RELEASE,PAGE_EXECUTE_READWRITE};
 let file=std::path::PathBuf::from(std::env::var("SCBL_R58_C40_FIXTURE").unwrap()).join("native-008c4ed0.bin");
 let bytes=std::fs::read(file).unwrap();let raw=&bytes[..24];
 assert_eq!(raw,&[0xf7,0x41,0x60,0,0,4,0,0x74,0xc,0x80,0x79,0x4d,2,0x74,6,0xb8,1,0,0,0,0xc3,0x33,0xc0,0xc3]);
 assert_eq!(format!("{:x}",Sha256::digest(raw)),"851eb9ba1cb656e934456a7b63edf3d6bea93c0622a8af170b6f42dc23d415a2");
 unsafe {
  let mem=VirtualAlloc(None,4096,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);assert!(!mem.is_null());
  std::ptr::copy_nonoverlapping(raw.as_ptr(),mem as *mut u8,raw.len());
  let native:unsafe extern "thiscall" fn(usize)->u32=std::mem::transmute(mem);
  for flags in [0u32,0x20000,0x40000,0x60000] {for role in [0u32,1,2,3] {
   let mut pc=vec![0u32;0x200/4];pc[0x60/4]=flags;pc[0x4c/4]=role<<8;
   let before=pc.clone();assert_eq!(native(pc.as_ptr() as usize)!=0,flags&0x40000!=0&&role!=2);assert_eq!(pc,before);
  }}
  VirtualFree(mem,0,MEM_RELEASE).unwrap();
 }
}
