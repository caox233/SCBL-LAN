//! One MP drone HUD label CALL only. No action/input/cache/localization policy change.
use crate::equipment_native::{readable, signature, word, writable};
use std::ffi::c_void;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
const SITE: usize = 0x01ac_0fa8;
const LABEL: usize = 0x014a_f540;
const FULL_CACHE: usize = 0x033c_1370;
const SHORT_CACHE: usize = 0x033c_1d90;
const CACHE_BYTES: usize = 216 * 12;
static READY: AtomicBool = AtomicBool::new(false);
static PATCH: Mutex<Option<Patch>> = Mutex::new(None);
type Label = unsafe extern "C" fn(i32, usize, usize) -> usize;
#[derive(Clone, Copy)]
struct Patch { site: usize, old: [u8;5], new: [u8;5], protection: u32 }
#[link(name="kernel32")]
extern "system" {
    fn VirtualProtect(p:*mut c_void,n:usize,protection:u32,old:*mut u32)->i32;
    fn GetCurrentProcess()->*mut c_void;
    fn FlushInstructionCache(h:*mut c_void,p:*const c_void,n:usize)->i32;
}
fn overlap(a:usize,n:usize,b:usize,m:usize)->bool {
    match (a.checked_add(n),b.checked_add(m)) { (Some(x),Some(y))=>a<y && b<x, _=>true }
}
unsafe fn span(header:usize)->Option<(usize,usize)> {
    if header%4!=0 || !readable(header,12) {return None;}
    let p=word(header,0)? as usize;
    let n=word(header,4)? as usize;
    let cap=word(header,8)? as usize;
    if n==0 || n>cap || cap>1024 || p%2!=0 || !readable(p,cap*2) {return None;}
    Some((p,cap*2))
}
unsafe fn rewrite_full(full:usize,short:usize,fc:usize,sc:usize)->bool {
    if overlap(full,12,short,12) || overlap(full,12,fc,CACHE_BYTES)
        || overlap(full,12,sc,CACHE_BYTES) {return false;}
    let Some((p,n))=span(full) else{return false;};
    if word(full,4)!=Some(3) || !writable(p,n) {return false;}
    let Some(s)=span(short) else{return false;};
    let Some(f)=span(fc+0xb0*12) else{return false;};
    let Some(c)=span(sc+0xb0*12) else{return false;};
    // Reject whole-capacity aliases, including interior overlaps, not just equal pointers.
    for (q,m) in [(full,12),(short,12),(fc,CACHE_BYTES),(sc,CACHE_BYTES),s,f,c] {
        if overlap(p,n,q,m) || (q!=full && overlap(full,12,q,m)) {return false;}
    }
    if std::slice::from_raw_parts(p as *const u16,3)!=[0x5f15,0x7206,0] {return false;}
    // Equal-length UTF-16. Native retains allocation, Count=3, capacity, terminator,
    // ownership and destruction. This caller's full string is NOT the cache.
    (p as *mut u16).write(0x9000);
    (p as *mut u16).add(1).write(0x51fa);
    true
}
unsafe fn label_with(action:i32,full:usize,short:usize,enabled:bool,normal:bool,
    language:usize,fc:usize,sc:usize,original:impl FnOnce(i32,usize,usize)->usize)->usize {
    let result=original(action,full,short);
    if enabled && normal && action==0xb0 {
        let mut lang=[0u8;8];
        if super::ascii(language,false,&mut lang).is_some_and(|l|l.eq_ignore_ascii_case("cht")) {
            let _=rewrite_full(full,short,fc,sc);
        }
    }
    result
}
unsafe extern "C" fn label(action:i32,full:usize,short:usize)->usize {
    let original:Label=std::mem::transmute(LABEL);
    let normal=super::normal_locale_flags(word(0x0330_c1c0,0),word(0x0330_c1f0,0),word(0x0330_c1f4,0));
    label_with(action,full,short,READY.load(Ordering::Acquire),normal,0x0337_ce38,
        FULL_CACHE,SHORT_CACHE,|a,f,s|original(a,f,s))
}
fn call(site:usize,target:usize)->[u8;5] {
    let mut b=[0xe8,0,0,0,0];b[1..].copy_from_slice(&(target as u32).wrapping_sub(site as u32+5).to_le_bytes());b
}
unsafe fn write(p:&Patch,restore:bool)->bool {
    if !readable(p.site,5) {return false;}
    let (expected,next)=if restore {(&p.new,&p.old)} else {(&p.old,&p.new)};
    let current=std::slice::from_raw_parts(p.site as *const u8,5);
    if current!=expected && !(restore && current==next) {return false;}
    let mut ignored=0;
    if VirtualProtect(p.site as _,5,0x40,&mut ignored)==0 {return false;}
    std::ptr::copy_nonoverlapping(next.as_ptr(),p.site as *mut u8,5);
    let matched=std::slice::from_raw_parts(p.site as *const u8,5)==next;
    // Preserve the first original protection even if restoration fails; always flush.
    let protected=VirtualProtect(p.site as _,5,p.protection,&mut ignored)!=0;
    let flushed=FlushInstructionCache(GetCurrentProcess(),p.site as _,5)!=0;
    matched && protected && flushed
}
unsafe fn restore(journal:&mut Option<Patch>)->bool {
    match *journal {None=>true,Some(p)=>if write(&p,true){*journal=None;true}else{false}}
}
unsafe fn install_at(site:usize,target:usize,bridge:usize,journal:&mut Option<Patch>)->bool {
    if journal.is_some() || !readable(site,5) {return false;}
    let old=call(site,target);let new=call(site,bridge);
    if std::slice::from_raw_parts(site as *const u8,5)!=old {return false;}
    let mut protection=0;
    if VirtualProtect(site as _,5,0x40,&mut protection)==0 {return false;}
    let p=Patch{site,old,new,protection};*journal=Some(p);
    if write(&p,false) {true} else {let _=restore(journal);false}
}
pub(super) unsafe fn init(world:usize) {
    if READY.load(Ordering::Acquire) {return;}
    let Ok(mut journal)=PATCH.lock() else{return;};
    if journal.is_some() && !restore(&mut journal) {return;}
    if world!=0x0058_9860 || !signature(0x01ac_0e00,903,"76f22c60ded19b1bf99ec5ff6c90a6fbc0b83e9bf1608a7628f5e0705d6f5e40")
        || !signature(LABEL,605,"783fca3f75d48ed754c3b998915e4584d015f6a186addd582afe9c4abf8af12e") {return;}
    if install_at(SITE,LABEL,label as *const () as usize,&mut journal) {
        READY.store(true,Ordering::Release);
        tracing::info!("SCBL_DRONE_EXIT_LABEL enabled=true locale=cht callsite=01AC0FA8 cache_unchanged=true");
    }
}
pub(super) unsafe fn deinit() {
    READY.store(false,Ordering::Release);
    if let Ok(mut journal)=PATCH.lock() {
        if !restore(&mut journal) {tracing::error!("SCBL_DRONE_EXIT_LABEL restore=false pending=true");}
    }
}

#[cfg(test)]
#[path = "drone_exit_label_tests.rs"]
mod tests;
