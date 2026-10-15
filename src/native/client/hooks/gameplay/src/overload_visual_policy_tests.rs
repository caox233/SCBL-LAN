use super::*;
struct F {world:Vec<u32>,info:Vec<u32>,pc:Vec<u32>,pawn:Vec<u32>,_array:Vec<u32>}
impl F {
 fn new()->Self {
  let mut f=Self{world:vec![0;0x700/4],info:vec![0;0x400/4],pc:vec![0;0x1000/4],pawn:vec![0;0x2000/4],_array:vec![0;1]};
  let w=f.world.as_ptr()as u32;f.world[0x6d0/4]=f.info.as_ptr()as u32;
  f.info[0x330/4]=0x10;f.info[0x144/4]=w;f.info[0x36c/4]=f._array.as_ptr()as u32;f.info[0x370/4]=1;f.info[0x374/4]=1;f._array[0]=f.pc.as_ptr()as u32;
  f.pc[0]=0x02ac53ec;f.pc[0x60/4]=0x40000;f.pc[0x144/4]=w;f.pc[0x6dc/4]=1;f.pc[0x520/4]=f.pawn.as_ptr()as u32;
  unsafe{(f.pc.as_mut_ptr()as *mut u8).add(0x55d).write(4);}
  f.pawn[0]=0x02ad3234;f.pawn[0x374/4]=0x40000;f.pawn[0x144/4]=w;f.pawn[0x938/4]=f.pc.as_ptr()as u32;f.pawn[0x1410/4]=0x12341000;f.pawn[0x20/4]=1;f
 }
 fn w(&self)->usize{self.world.as_ptr()as usize}
}
#[test]
fn gate_transitions_refresh_once_and_restore_native_on_reopen(){unsafe{
 clear();let f=F::new();let calls=std::cell::Cell::new(0);
 for on in [false,true,true,false,false,true] {
  fixture_update(f.info.as_ptr()as usize,|_|on,||calls.set(calls.get()+1));assert_eq!(hidden(f.w()),!on);
 }
 assert_eq!(calls.get(),3);assert!(hidden(0));clear();assert!(hidden(f.w()));
}}
#[test]
fn cameras_dead_pawns_merchants_stale_identity_and_unknown_are_hidden(){unsafe{
 for reason in 0..9 {clear();let mut f=F::new();fixture_update(f.info.as_ptr()as usize,|_|true,||{});assert!(!hidden(f.w()));
  match reason {
   0=>f.pc[0x520/4]=0,
   1=>f.pawn[0x60/4]|=0x800,
   2=>f.pawn[0x4b0/4]|=0x80000000,
   3=>f.pawn[0xc50/4]=1,
   4=>f.pawn[0x20/4]+=1,
   5=>{(f.pc.as_mut_ptr()as *mut u8).add(0x55d).write(5);f.pawn[0]=0x02ad2abc;f.pawn[0x374/4]=0x80000;},
   6=>f.pc[0x6dc/4]=u32::MAX,
   7=>f.pawn[0x938/4]=0,
   _=>f.pawn[0x1410/4]=0,
  }
  assert!(hidden(f.w()),"reason {reason}");
 }
 clear();
}}
#[test]
fn invalid_non_mp_or_missing_inventory_never_calls_native_goggles(){unsafe{
 clear();let mut f=F::new();f.info[0x330/4]=0;
 fixture_update(f.info.as_ptr()as usize,|_|panic!("non MP getter"),||{});assert!(hidden(f.w()));
 f.info[0x330/4]=0x10;f.pawn[0x1410/4]=0;
 fixture_update(f.info.as_ptr()as usize,|_|panic!("missing inventory getter"),||{});assert!(hidden(f.w()));clear();
}}