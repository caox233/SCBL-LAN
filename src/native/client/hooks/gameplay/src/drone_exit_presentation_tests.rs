use super::*;
use std::cell::Cell;
unsafe extern "thiscall" fn mark(hud:usize,value:u32) {
    *((hud+0x2e4) as *mut u32)=value;
}
struct Scene { state:Vec<u32>, owner:Vec<u32>, controller:Vec<u32>, hud:Vec<u32> }
impl Scene {
    fn new()->Self {
        let mut s=Self {state:vec![0;0x160/4],owner:vec![0;0x1400/4],controller:vec![0;0x320/4],hud:vec![0;0x300/4]};
        s.state[0x148/4]=1;s.state[0]=STATE;s.owner[0]=PAWN;s.hud[0]=HUD;
        let owner=s.owner.as_ptr() as u32;let controller=s.controller.as_ptr() as u32;
        s.state[0x48/4]=owner;s.state[0x64/4]=controller;
        s.owner[0x938/4]=controller;s.controller[0x310/4]=owner;s.hud[0x25c/4]=controller;
        s
    }
    fn state(&self)->usize {self.state.as_ptr() as usize}
    unsafe fn binding(&self)->Binding {Binding::capture(self.hud.as_ptr() as usize,HUD).unwrap()}
}
#[test]
fn accepted_voluntary_exit_pairs_close_after_exact_native_transition() {
    let order=Cell::new(0);
    let result=transition(0,||true,||{assert_eq!(order.get(),1);order.set(2);},
        |mode|{assert_eq!(mode,0);assert_eq!(order.get(),0);order.set(1);42});
    assert_eq!(result,42);assert_eq!(order.get(),2);
}
#[test]
fn forced_exit_never_queries_or_closes_early() {
    for native in [1,2,99] {
        assert_eq!(transition(native,||panic!("forced eligibility"),||panic!("forced movie"),|v|v),native);
    }
}
#[test]
fn rejected_voluntary_exit_forwards_native_once_without_movie() {
    let n=Cell::new(0);
    transition(0,||false,||panic!("not accepted"),|v|{assert_eq!(v,0);n.set(n.get()+1);});
    assert_eq!(n.get(),1);
}
#[test]
fn same_hud_owner_controller_required_and_native_phase_deduplicates() {
    unsafe {
        let s=Scene::new();let binding=s.binding();
        assert!(binding.begin(s.state(),HUD,STATE,PAWN,|_|mark(s.hud.as_ptr() as usize,1)));
        assert!(!binding.begin(s.state(),HUD,STATE,PAWN,|_|mark(s.hud.as_ptr() as usize,1)));
        assert_eq!(s.hud[0x2e4/4],1);
    }
}
#[test]
fn stale_null_foreign_and_rebound_objects_fail_closed() {
    unsafe {
        for fault in 0..10 {
            let mut s=Scene::new();let b=s.binding();
            match fault {
                0=>s.state[0]=0,1=>s.state[0x48/4]=0,2=>s.state[0x64/4]=0,
                3=>s.owner[0]=0,4=>s.owner[0x938/4]=0,5=>s.controller[0x310/4]=0,
                6=>s.hud[0]=0,7=>s.hud[0x25c/4]=0,8=>s.hud[0x2e4/4]=1,_=>s.state[0x148/4]=0
            }
            assert!(!b.begin(s.state(),HUD,STATE,PAWN,|_|mark(s.hud.as_ptr() as usize,1)),"fault={fault}");
        }
        assert!(Binding::capture(0,HUD).is_none());
        let s=Scene::new();assert!(!s.binding().begin(0,HUD,STATE,PAWN,|_|mark(s.hud.as_ptr() as usize,1)));
    }
}
#[test]
fn foreign_thread_does_not_touch_cached_native_objects() {
    unsafe {
        let s=Scene::new();let b=s.binding();let state=s.state();let hud=s.hud.as_ptr() as usize;
        assert!(!std::thread::spawn(move||b.begin(state,HUD,STATE,PAWN,|_|mark(hud,1))).join().unwrap());
        assert_eq!(s.hud[0x2e4/4],0);
    }
}
#[test]
fn destructor_or_shutdown_forgets_binding_but_foreign_hud_does_not() {
    unsafe {
        let s=Scene::new();let b=s.binding();*CURRENT.lock().unwrap()=Some(b);
        clear(Some(b.hud+4));assert!(CURRENT.lock().unwrap().is_some());
        clear(Some(b.hud));assert!(CURRENT.lock().unwrap().is_none());
        *CURRENT.lock().unwrap()=Some(b);clear(None);assert!(CURRENT.lock().unwrap().is_none());
    }
}
#[test]
fn unavailable_adapter_does_not_change_native_gameplay() {
    assert!(!READY.load(Ordering::Acquire));
    assert!(!unsafe{begin(0)});
    let forwarded=Cell::new(0);
    transition(0,||true,||{assert!(!unsafe{begin(0)});},|mode|{assert_eq!(mode,0);forwarded.set(1);});
    assert_eq!(forwarded.get(),1);
}

mod native_bytes { use super::*;

use std::sync::atomic::{AtomicU32, Ordering};
use std::ffi::c_void;
static C25_ENDS: AtomicU32 = AtomicU32::new(0);
static C25_STARTS: AtomicU32 = AtomicU32::new(0);
#[link(name="kernel32")]
extern "system" {
    fn VirtualAlloc(p:*mut c_void,n:usize,t:u32,k:u32)->*mut c_void;
    fn VirtualProtect(p:*mut c_void,n:usize,k:u32,old:*mut u32)->i32;
    fn VirtualFree(p:*mut c_void,n:usize,t:u32)->i32;
    fn FlushInstructionCache(h:*mut c_void,p:*const c_void,n:usize)->i32;
    fn GetCurrentProcess()->*mut c_void;
}
unsafe extern "thiscall" fn c25_play(_:usize,label:*const u8) {
    match std::ffi::CStr::from_ptr(label as _).to_bytes() {
        b"DroneEnd"=>{C25_ENDS.fetch_add(1,Ordering::SeqCst);}
        b"DroneStart"=>{C25_STARTS.fetch_add(1,Ordering::SeqCst);}
        _=>panic!("unexpected native label")
    }
}
#[test]
fn c25_same_input_native_movie_order() {
    unsafe {
        use sha2::{Digest,Sha256};
        let root=std::path::PathBuf::from(std::env::var("SCBL_R58_C25_FIXTURE").unwrap());
        let mut code=std::fs::read(root.join("hud_end.bin")).unwrap();
        assert_eq!(format!("{:x}",Sha256::digest(&code)),"3ce7e613563aecda3b50c26f7e6a73878557ba956b9793227a2cd7eecc74dd0d");
        let bank=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;
        assert_ne!(bank,0);
        for (off,label) in [(0x2c,b"DroneStart\0".as_slice()),(0x43,b"DroneEnd\0".as_slice())] {
            std::ptr::copy_nonoverlapping(label.as_ptr(),(bank+0x500+off) as *mut u8,label.len());
            code[off..off+4].copy_from_slice(&((bank+0x500+off) as u32).to_le_bytes());
        }
        std::ptr::copy_nonoverlapping(code.as_ptr(),bank as *mut u8,code.len());
        let mut old=0;assert_ne!(VirtualProtect(bank as _,4096,0x20,&mut old),0);
        FlushInstructionCache(GetCurrentProcess(),bank as _,4096);
        let setter:unsafe extern "thiscall" fn(usize,u32)=std::mem::transmute(bank);
        let mut vt=vec![0u32;0x200/4];vt[0x12c/4]=c25_play as *const () as u32;
        let mut hud=vec![0u32;0x300/4];hud[0]=vt.as_ptr() as u32;
        let p=hud.as_mut_ptr() as usize;
        // Event-order harness, not an actual animation clock or rendered GFx.
        C25_ENDS.store(0,Ordering::SeqCst);
        let at_wait;
        at_wait=transition(0,||true,||setter(p,1),|mode|{assert_eq!(mode,0);C25_ENDS.load(Ordering::SeqCst)});
        let before=C25_ENDS.load(Ordering::SeqCst);
        assert_eq!(at_wait,0);assert_eq!(before,1);
        setter(p,1);setter(p,1); // later native DroneEnd requests must deduplicate
        assert_eq!(C25_ENDS.load(Ordering::SeqCst),1);
        assert_eq!(hud[0x2e4/4],1);
        setter(p,0); // re-entry still plays native DroneStart
        assert_eq!(C25_STARTS.load(Ordering::SeqCst),1);
        assert_eq!(hud[0x2e4/4],0);
        println!("C48_SAME_MOVIE end_before_native_wait={} forwarded_mode=0 native_calls=1 end_total=1 reentry_start=1 synthetic_clock=false",at_wait);
        VirtualFree(bank as _,0,0x8000);
    }
}

static SIGNALS: AtomicU32 = AtomicU32::new(0);
static EXITS: AtomicU32 = AtomicU32::new(0);
static ENTRIES: AtomicU32 = AtomicU32::new(0);
unsafe extern "thiscall" fn signal(_:usize,_:u32,_:u32) { SIGNALS.fetch_add(1,Ordering::SeqCst); }
unsafe extern "thiscall" fn noop_dt(_:usize,_:f32) {}
unsafe extern "thiscall" fn noop(_:usize) {}
unsafe extern "thiscall" fn exit_count(_:usize) { EXITS.fetch_add(1,Ordering::SeqCst); }
unsafe extern "thiscall" fn entry_count(_:usize) { ENTRIES.fetch_add(1,Ordering::SeqCst); }
#[test]
fn native_close_signal_starts_real_timer_and_native_tick_completes_once() {
 unsafe {
  use sha2::{Digest,Sha256};
  let root=std::path::PathBuf::from(std::env::var("SCBL_R58_C48_FIXTURE").unwrap());
  let mut close=std::fs::read(root.join("native-close-signal.bin")).unwrap();
  let mut tick=std::fs::read(root.join("native-exit-tick.bin")).unwrap();
  assert_eq!(format!("{:x}",Sha256::digest(&close)),"65a250c0e6057880da7129b6eb1e63d1754497bb018f0829fc9425a56e72f034");
  assert_eq!(format!("{:x}",Sha256::digest(&tick)),"0bbdb919d0b4b277f39182547e9a0895ad20806efd4bdc601a5ed13b4a675cf7");
  let bank=VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;assert_ne!(bank,0);
  *((bank+0x500) as *mut f32)=0.2;
  close[4..8].copy_from_slice(&((bank+0x500) as u32).to_le_bytes());
  assert_eq!(close[0x19],0xe8);
  close[0x1a..0x1e].copy_from_slice(&((signal as *const () as usize).wrapping_sub(bank+0x1e) as u32).to_le_bytes());
  for (off,fun) in [(0x11,noop_dt as *const () as usize),(0x3b,noop as *const () as usize),(0x7b,exit_count as *const () as usize),(0x85,entry_count as *const () as usize)] {
   assert_eq!(tick[off],0xe8);
   tick[off+1..off+5].copy_from_slice(&(fun.wrapping_sub(bank+0x100+off+5) as u32).to_le_bytes());
  }
  std::ptr::copy_nonoverlapping(close.as_ptr(),bank as *mut u8,close.len());
  std::ptr::copy_nonoverlapping(tick.as_ptr(),(bank+0x100) as *mut u8,tick.len());
  let mut old=0;assert_ne!(VirtualProtect(bank as _,4096,0x20,&mut old),0);
  FlushInstructionCache(GetCurrentProcess(),bank as _,4096);
  let close:unsafe extern "thiscall" fn(usize,u32)->u8=std::mem::transmute(bank);
  let tick:unsafe extern "thiscall" fn(usize,f32)=std::mem::transmute(bank+0x100);
  let s=Scene::new();let b=s.binding();
  SIGNALS.store(0,Ordering::SeqCst);EXITS.store(0,Ordering::SeqCst);ENTRIES.store(0,Ordering::SeqCst);
  transition(0,||true,||{assert!(b.begin(s.state(),HUD,STATE,PAWN,|state|{assert_eq!(close(state,0),1);}));},|mode|assert_eq!(mode,0));
  assert_eq!(f32::from_bits(s.state[0x14c/4]),0.2);
  assert_eq!(SIGNALS.load(Ordering::SeqCst),1);
  tick(s.state(),0.1);assert_eq!(EXITS.load(Ordering::SeqCst),0);
  tick(s.state(),0.101);assert_eq!(EXITS.load(Ordering::SeqCst),1);
  tick(s.state(),0.1);assert_eq!(EXITS.load(Ordering::SeqCst),1);
  assert_eq!(ENTRIES.load(Ordering::SeqCst),0);
  println!("C48_NATIVE_CLOSE signal=1 native_timer=0.2 exits_before=0 exits_after=1 duplicate_exit=0 rendered_movie=false");
  VirtualFree(bank as _,0,0x8000);
 }
}

}
