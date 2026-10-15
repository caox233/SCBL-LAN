//! Saboteur IN/OUT request gain, owned by the existing native audio Send hook.
//! No playback, radius, fade, global bus, or replicated gameplay writes.
use crate::equipment_native::{readable, word};
use std::{cell::Cell, sync::{Mutex, atomic::{AtomicBool, Ordering}}};

// Pinned retail DareStudio.xml event IDs, not UObject/FName indices.
const PREPARE: u32 = 0x008359b4;
const RELEASE: u32 = 0x008359bb;
const SPY: u32 = 0x02ad3234;
const MERC: u32 = 0x02ad2abc;
const PARTICLE: u32 = 0x029e8754;
static READY: AtomicBool = AtomicBool::new(false);
static GAIN_OWNER: AtomicBool = AtomicBool::new(false);
static ANY: AtomicBool = AtomicBool::new(false);
static EFFECTS: Mutex<[Option<Effect>;128]> = Mutex::new([None;128]);

#[derive(Clone,Copy,Debug,PartialEq)]
struct Source { actor: usize, key: u32, world: u32, radius: f32, origin: [f32;3] }
#[derive(Clone,Copy,Debug,PartialEq)]
struct Effect { address: usize, key: u32, source: Source }
#[derive(Clone,Copy,Debug,PartialEq)]
enum Phase { Prepare, Release }
thread_local! {static SCOPE: Cell<Option<(Source,Phase)>> = const {Cell::new(None)};}

unsafe fn position(actor: usize) -> Option<[f32;3]> {
    let p=[f32::from_bits(word(actor,0x9c)?),f32::from_bits(word(actor,0xa0)?),f32::from_bits(word(actor,0xa4)?)];
    p.iter().all(|v|v.is_finite()).then_some(p)
}
unsafe fn source(ability: usize) -> Option<Source> {
    if word(ability,0)?!=0x02a3f3a4 {return None;}
    let actor=word(ability,0x6c)? as usize;
    if word(actor,0)?!=SPY || word(actor,0x374)?&0xc0000!=0x40000
        || word(actor,0x5c)?&1!=0 {return None;}
    let world=word(actor,0x144)?;
    let radius=f32::from_bits(word(ability,0xa0)?);
    if world==0 || !radius.is_finite() || radius<=0. {return None;}
    Some(Source {actor,key:word(actor,0x20)?,world,radius,origin:position(actor)?})
}
fn scoped<R>(scope:Option<(Source,Phase)>,f:impl FnOnce()->R)->R {
    struct Restore(Option<(Source,Phase)>);
    impl Drop for Restore {fn drop(&mut self){SCOPE.with(|s|s.set(self.0));}}
    let _restore=Restore(SCOPE.with(|s|s.replace(scope)));f()
}
unsafe fn with_phase<R>(ability:usize,phase:Phase,f:impl FnOnce()->R)->R {
    let scope=if READY.load(Ordering::Acquire) {source(ability).map(|s|(s,phase))} else {None};
    scoped(scope,f)
}
pub(super) unsafe fn prepare<R>(ability:usize,f:impl FnOnce()->R)->R {with_phase(ability,Phase::Prepare,f)}
pub(super) unsafe fn release<R>(ability:usize,f:impl FnOnce()->R)->R {with_phase(ability,Phase::Release,f)}

unsafe fn in_range(s:Source)->bool {
    let Some(pc)=crate::local_player::controller(s.world as usize) else {return false;};
    if word(pc,0)!=Some(0x02ac53ec) {return false;}
    let Some(pawn)=word(pc,0x520).filter(|p|*p!=0).map(|p|p as usize) else {return false;};
    if word(pc,0x310)!=Some(pawn as u32) || word(pawn,0)!=Some(MERC)
        || word(pawn,0x374).map(|f|f&0xc0000)!=Some(0x80000)
        || word(pawn,0x144)!=Some(s.world) || word(pawn,0x938)!=Some(pc as u32)
        || !word(pawn,0x460).is_some_and(|hp|(hp as i32)>0)
        || !word(pawn,0x5c).is_some_and(|flags|flags&1==0) {return false;}
    let Some(p)=position(pawn) else {return false;};
    // f64 avoids overflow for otherwise finite native coordinates/radius.
    let distance=p.into_iter().zip(s.origin).map(|(a,b)|(a as f64-b as f64).powi(2)).sum::<f64>();
    distance <= (s.radius as f64).powi(2)
}
unsafe fn same_source(s:Source)->bool {
    word(s.actor,0)==Some(SPY) && word(s.actor,0x20)==Some(s.key)
        && word(s.actor,0x144)==Some(s.world)
}
unsafe fn particle(address:usize,key:Option<u32>,world:u32)->bool {
    word(address,0)==Some(PARTICLE) && word(address,0x144)==Some(world)
        && key.is_none_or(|k|word(address,0x20)==Some(k))
}
pub(super) unsafe fn record(ability:usize,address:usize) {
    forget(address);
    if !READY.load(Ordering::Acquire) {return;}
    let Some(source)=source(ability) else {return;};
    if !particle(address,None,source.world) {return;}
    let Some(key)=word(address,0x20) else {return;};
    if let Ok(mut entries)=EFFECTS.lock() {
        // A late producer cannot repopulate a cleared/shut-down registry.
        if !READY.load(Ordering::Acquire) {return;}
        if let Some(slot)=entries.iter_mut().find(|e|e.is_none()) {
            *slot=Some(Effect{address,key,source});ANY.store(true,Ordering::Release);
        }
    }
}
pub(super) fn forget(address:usize) {
    if !ANY.load(Ordering::Acquire) {return;}
    if let Ok(mut entries)=EFFECTS.lock() {
        for e in entries.iter_mut(){if e.is_some_and(|v|v.address==address){*e=None;}}
        ANY.store(entries.iter().any(Option::is_some),Ordering::Release);
    }
}
pub(super) fn enabled(value:bool) {
    READY.store(value,Ordering::Release);
    if !value {clear();}
}
pub(crate) fn gain_owner(value:bool) {
    GAIN_OWNER.store(value,Ordering::Release);
    if !value {enabled(false);}
}
pub(super) fn gain_ready()->bool {GAIN_OWNER.load(Ordering::Acquire)}
pub(super) fn clear() {
    if let Ok(mut entries)=EFFECTS.lock(){*entries=[None;128];ANY.store(false,Ordering::Release);}
}
pub(crate) unsafe fn eligible(event:u32,params:*const u32)->bool {
    if !READY.load(Ordering::Acquire) || !matches!(event,PREPARE|RELEASE)
        || !readable(params as usize,48) {return false;}
    let p=params as usize;
    if !word(p,0).is_some_and(|v|v!=0) || word(p,8)!=Some(2)
        || !word(p,12).is_some_and(|f|f&0x15==0x15) {return false;}
    let Some(actor)=word(p,4).map(|x|x as usize) else {return false;};
    if let Some((s,phase))=SCOPE.with(Cell::get) {
        let phase_matches=match phase {Phase::Prepare=>event==PREPARE,Phase::Release=>event==RELEASE};
        if phase_matches && same_source(s) && (actor==s.actor ||
            (phase==Phase::Release && particle(actor,None,s.world))) {return in_range(s);}
        // Unrelated nested audio cannot inherit a registered effect fallback.
        return false;
    }
    if event!=RELEASE || !ANY.load(Ordering::Acquire) {return false;}
    let saved=EFFECTS.lock().ok().and_then(|entries|entries.iter().flatten().find(|e|e.address==actor).copied());
    saved.is_some_and(|e|particle(actor,Some(e.key),e.source.world) && in_range(e.source))
}

#[cfg(test)]
#[path="overload_audio_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::fixture_gain_cases;
