//! One native request-gain owner: Spy drone loops and scoped Merc Saboteur cues.
//! PlaySound.scalar is FADE DURATION, not volume. Never modify that argument.
//! Scope the native sound request ID synchronously; no emitter-wide/global gain.
use crate::equipment_native::{readable, signature, word};
use retour::static_detour;
use std::{cell::Cell, sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering}};
const SEND: usize = 0x1e16d90;
const COMMAND: usize = 0x1e22510;
const COMMAND_LEN: usize = 212;
// 20*log10(1.5), linear amplitude, not a claim about perceived loudness.
const GAIN_DB: f32 = 3.521825;
const DRONE: u32 = 0x02a88f54;
const SPY: u32 = 0x02ad3234;
const CONTROLLER: u32 = 0x02ac53ec;
static READY: AtomicBool = AtomicBool::new(false);
static COPY: AtomicUsize = AtomicUsize::new(0);
static LOGS: AtomicU32 = AtomicU32::new(0);
static OVERLOAD_LOGS: AtomicU32 = AtomicU32::new(0);
thread_local! {static SCOPE: Cell<Option<usize>> = const {Cell::new(None)};}
type Gain = unsafe extern "C" fn(u32, f32, f32, u32);
type SendFn = unsafe extern "C" fn(u32, *const u32) -> u32;
static_detour! {static Send: unsafe extern "C" fn(u32,*const u32)->u32;}

fn scoped<R>(body: Option<usize>, f: impl FnOnce()->R)->R {
    struct Reset(Option<usize>);
    impl Drop for Reset {fn drop(&mut self) {SCOPE.with(|s|s.set(self.0));}}
    let _reset=Reset(SCOPE.with(|s|s.replace(body)));
    f()
}
unsafe fn spy_listener(body: usize)->bool {
    // The native world array also includes remote players. Select by retail
    // local-controller predicate, never by player count or first entry.
    let Some(world)=word(body,0x144).map(|x|x as usize) else {return false;};
    let Some(controller)=crate::local_player::controller(world) else {return false;};
    if word(controller,0)!=Some(CONTROLLER) {return false;}
    let Some(pawn)=word(controller,0x520).map(|x|x as usize) else {return false;};
    word(controller,0x310)==Some(pawn as u32)
        && word(pawn,0)==Some(SPY)
        && word(pawn,0x938)==Some(controller as u32)
        && word(pawn,0x144)==Some(world as u32)
        && word(pawn,0x374).is_some_and(|f|f & ((1<<18)|(1<<19))==1<<18)
}
unsafe fn body_alive(body:usize)->bool {
    word(body,0)==Some(DRONE)
        && word(body,0x5dc).is_some_and(|hp|hp as i32>0)
        && word(body,0x910).is_some_and(|v|v & 255 <=2)
        && word(body,0x4b0).is_some_and(|f|f & 0x80000000==0)
}
unsafe fn loop_start(body:usize,event:usize)->bool {
    [(0xb94,"Play_SFX_ADV_Gadget_Drone_Loop_3D"),(0xbac,"Play_SFX_ADV_Gadget_Drone_DamagedLoop_3D")]
        .into_iter().any(|(offset,name)|word(body,offset)==Some(event as u32)&&super::named(event,name))
}
pub(super) unsafe fn with_request<R>(body:usize,event:usize,slot:u32,f:impl FnOnce()->R)->R {
    let valid=READY.load(Ordering::Acquire) && slot<4 && body_alive(body)
        && loop_start(body,event) && spy_listener(body);
    // A nested unrelated native audio call must not inherit the outer drone.
    scoped(valid.then_some(body),f)
}
unsafe fn request_matches(body:usize,params:*const u32)->bool {
    // Native A96EEC builds {system spatial type, actor, userData=2, flags|0x15}.
    readable(params as usize,48)
        && word(params as usize,0).is_some_and(|v|v!=0)
        && word(params as usize,4)==Some(body as u32)
        && word(params as usize,8)==Some(2)
        && word(params as usize,12).is_some_and(|f|f & 0x15==0x15)
}
fn finish_request(result:u32, eligible:bool, gain:impl FnOnce(u32,f32,f32,u32))->u32 {
    if eligible && result!=u32::MAX {gain(result,GAIN_DB,0.,0);}
    result
}
fn send(event:u32,params:*const u32)->u32 {
    unsafe {
        let overload=READY.load(Ordering::Acquire) && crate::overload_audio::eligible(event,params);
        let eligible=overload || (READY.load(Ordering::Acquire)
            && SCOPE.with(|s|s.get()).is_some_and(|body|request_matches(body,params)));
        let result=Send.call(event,params);
        finish_request(result,eligible,|id,db,fade,curve|{
            let address=COPY.load(Ordering::Acquire);
            if address!=0 {
                let gain:Gain=std::mem::transmute(address);
                gain(id,db,fade,curve);
                if overload {
                    if OVERLOAD_LOGS.fetch_add(1,Ordering::Relaxed)<16 {
                        tracing::info!("SCBL_OVERLOAD_AUDIO native_request=true event={:08X} gain_db={} amplitude_factor=1.5 playback_added=false actual_ability_range=true",event,db);
                    }
                } else if LOGS.fetch_add(1,Ordering::Relaxed)<16 {
                    tracing::info!("SCBL_MOD_DRONE_SPY_AUDIO native_request=true gain_db={} amplitude_factor=1.5 scope=body_3d_loop spatial_range_unchanged=true",db);
                }
            }
        })
    }
}
// Native request-gain wrapper queues a thread-safe volume command, but its
// public front end clamps positive dB to zero. A private RX copy skips ONLY
// that clamp; its sole caller supplies the finite, fixed +3.521825 dB value.
// Live game bytes and all ordinary request callers retain the native clamp.
// Queueing, request lookup, interpolation, cleanup and native fade stay intact.
pub(super) fn prepare(code:&[u8], destination:usize)->Option<Vec<u8>> {
    use sha2::{Digest,Sha256};
    if code.len()!=COMMAND_LEN
        || format!("{:x}",Sha256::digest(code))!="ded7a0ff5161091967373ddf1c4afd1f59674e21f57d54da58645c4d4a98d75c"
    {return None;}
    let mut result=code.to_vec();
    for (off,target) in [(0x18,0x1f38470usize),(0x6f,0x1dff7b0),(0x7c,0x1e0f460),(0xbe,0x1f2de80),(0xcb,0x24c245a)] {
        if result[off]!=0xe8 {return None;}
        result[off+1..off+5].copy_from_slice(&(target as u32).wrapping_sub((destination+off+5) as u32).to_le_bytes());
    }
    result[0x25..0x2c].copy_from_slice(&[0xdd,0xd8,0xe9,0x27,0,0,0]);
    Some(result)
}
pub(super) unsafe fn init()->bool {
    if READY.load(Ordering::Acquire) {return true;}
    if !signature(SEND,358,"6522956767941f7a28ea9eb1328f2f46d88726557fae622b1b996157568b4d1c")
        || !signature(COMMAND,COMMAND_LEN,"ded7a0ff5161091967373ddf1c4afd1f59674e21f57d54da58645c4d4a98d75c")
        || !signature(0x1dd4ae0,48,"b2f8fb3fbfbd26dc0328fb3cc1b741db494a3adc866d36bec9b515449462531e")
        || !signature(0x8b5ef0,7,"6fc4ab3327ca85dd0c2c8d2741ca26ba2aa850f93cf9e89e3475cb330701279e")
    {return false;}
    let memory=super::super::VirtualAlloc(std::ptr::null_mut(),4096,0x3000,4) as usize;
    if memory==0 {return false;}
    let Some(code)=prepare(std::slice::from_raw_parts(COMMAND as *const u8,COMMAND_LEN),memory) else {
        super::super::VirtualFree(memory as _,0,0x8000);return false;
    };
    std::ptr::copy_nonoverlapping(code.as_ptr(),memory as *mut u8,code.len());
    let mut old=0;
    if super::super::VirtualProtect(memory as _,4096,0x20,&mut old)==0 {
        super::super::VirtualFree(memory as _,0,0x8000);return false;
    }
    super::super::FlushInstructionCache(super::super::GetCurrentProcess(),memory as _,code.len());
    COPY.store(memory,Ordering::Release);
    if Send.initialize(std::mem::transmute::<usize,SendFn>(SEND),send).is_err() || Send.enable().is_err() {
        let _=Send.disable();return false;
    }
    READY.store(true,Ordering::Release);
    crate::overload_audio::gain_owner(true);
    tracing::info!("SCBL_MOD_DRONE_SPY_AUDIO enabled=true amplitude_factor=1.5 event=3d_motor_loops_only");
    true
}
pub(super) unsafe fn deinit() {
    crate::overload_audio::gain_owner(false);
    READY.store(false,Ordering::Release);
    let _=Send.disable();
    // An already dispatched call can still return through COPY; keep RX mapped.
    SCOPE.with(|s|s.set(None));
}
#[cfg(test)]
#[path="drone_spy_audio_tests.rs"]
mod tests;
