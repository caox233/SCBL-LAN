//! Overload visuals are an opt-in of the current living local Spy's goggles.
//! Native goggles getter is called only by the existing HUD update owner. Render
//! callbacks consume a validated identity, never call an inventory virtual method.
use crate::equipment_native::word;
use std::sync::Mutex;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct View { world:usize, controller:usize, pawn:usize, key:u32 }
static VIEW:Mutex<Option<View>>=Mutex::new(None);
unsafe fn eligible(world:usize)->Option<View> {
    if crate::overload_viewer::viewer(world)!=Some(crate::overload_viewer::Viewer::Spy) {return None;}
    let controller=crate::local_player::controller(world)?;
    let pawn=word(controller,0x520)? as usize;
    if pawn==0 || word(pawn,0)!=Some(0x02ad3234)
        || word(pawn,0x374)?&0xc0000!=0x40000
        || word(pawn,0x144)?!=world as u32 || word(pawn,0x938)?!=controller as u32
        || word(pawn,0x60)?&0x800!=0 || word(pawn,0x4b0)?&0x80000000!=0
        || word(pawn,0xc50)?!=0 || word(pawn,0x1410)?==0 {return None;}
    Some(View{world,controller,pawn,key:word(pawn,0x20)?})
}
unsafe fn update(info:usize,goggles:impl FnOnce(usize)->bool,refresh:impl FnOnce()) {
    let next=if word(info,0x330).is_some_and(|f|f&0x10!=0) {
        word(info,0x144).and_then(|world|unsafe{eligible(world as usize)})
            .filter(|v|goggles(v.pawn))
    }else{None};
    let changed={let Ok(mut state)=VIEW.lock()else{return;};let changed=*state!=next;*state=next;changed};
    if changed {refresh();} // Drop lock before native queue callbacks can re-enter.
}
pub(super) unsafe fn frame(info:usize) {
    update(info,|pawn|unsafe {
        std::mem::transmute::<usize,unsafe extern "thiscall" fn(usize)->i32>(0xd75d30)(pawn)!=0
    },||unsafe{crate::overload_particles::refresh_visuals()});
}
pub(super) unsafe fn hidden(world:usize)->bool {
    let saved=VIEW.lock().ok().and_then(|v|*v);
    !saved.is_some_and(|v|v.world==world && unsafe{eligible(world)}==Some(v))
}
pub(super) fn clear(){if let Ok(mut v)=VIEW.lock(){*v=None;}}
#[cfg(test)]
pub(super) unsafe fn fixture_update(info:usize,goggles:impl FnOnce(usize)->bool,refresh:impl FnOnce()) {update(info,goggles,refresh);}
#[cfg(test)]
#[path="overload_visual_policy_tests.rs"] mod tests;