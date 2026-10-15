//! Native controller+55D is the last possessed pawn type. EF2425 copies
//! pawn+B32 into it; EF2551 unpossesses without clearing it. Unlike +520 this
//! survives a death camera. Values4/5 select native spy/merc music in E9EAB9.
//! Read the native field each time; never cache a pawn or adopt camera targets.
use crate::equipment_native::{readable,word};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum Viewer { Spy, Merc }

pub(super) unsafe fn viewer(world: usize) -> Option<Viewer> {
    let pc=crate::local_player::controller(world)?;
    if word(pc,0)?!=0x02ac53ec || word(pc,0x6dc)?==u32::MAX || !readable(pc.checked_add(0x55d)?,1) { return None; }
    let kind=((pc+0x55d) as *const u8).read();
    let expected=match kind {4=>(Viewer::Spy,0x02ad3234,0x40000),5=>(Viewer::Merc,0x02ad2abc,0x80000),_=>return None};
    // During possession the native cached type is updated last. Reject that
    // transient mismatch instead of borrowing the new pawn or old faction.
    let pawn=word(pc,0x520)? as usize;
    if pawn!=0 && (word(pawn,0)!=Some(expected.1)
        || word(pawn,0x374).map(|f|f&0xc0000)!=Some(expected.2)
        || word(pawn,0x144)!=Some(world as u32)
        || word(pawn,0x938)!=Some(pc as u32)) { return None; }
    Some(expected.0)
}
