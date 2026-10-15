//! Synchronous, read-only mapping. Actor object identity is NOT native owning peer ID.
//! Native From is accepted only against the current session's bounded peer list.
use crate::equipment_native::{float, readable, word};
use crate::goggle_feedback::{Context, Person};
#[derive(Clone)]
pub(crate) struct Frame {
    pub context: Context,
    pub manager: usize,
    pub net: usize,
    pub world: usize,
    pub pawn: usize,
}
unsafe fn ptr(p: usize, o: usize) -> Option<usize> {
    word(p, o).filter(|x| *x >= 0x10000).map(|v| v as usize)
}
unsafe fn key(p: usize) -> Option<u64> {
    Some(u64::from(word(p, 0x70)?) | (u64::from(word(p, 0x74)?) << 32)).filter(|v| *v != 0)
}
unsafe fn peers(session: usize) -> Option<Vec<u32>> {
    let sentinel = session.checked_add(0x584)?;
    let mut node = ptr(session, 0x588)?;
    let mut nodes = Vec::new();
    let mut ids = Vec::new();
    while node != sentinel {
        if nodes.len() >= 32 || nodes.contains(&node) {
            return None;
        }
        nodes.push(node);
        let peer = ptr(node, 8)?;
        let id = word(peer, 0)?;
        if id == u32::MAX || ids.contains(&id) {
            return None;
        }
        ids.push(id);
        node = ptr(node, 4)?;
    }
    Some(ids)
}

use std::sync::atomic::{AtomicU32, Ordering};

/// Native-context gate only, never an assertion that detection occurred.
/// Labels carry no names, identifiers, addresses, packets or player data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameRejection {
    Information, World, WorldInfo, Manager, Net, NetIdentity, Session,
    PeerList, MatchKey, LocalController, Roster, ActorPointer, ActorBinding,
    ActorPeer, ActorIdentity, LocalIdentity, Time, Context,
}
impl FrameRejection {
    fn label(self) -> &'static str {
        match self {
            Self::Information => "missing_information",
            Self::World => "missing_world",
            Self::WorldInfo => "missing_world_info",
            Self::Manager => "missing_manager",
            Self::Net => "missing_network_object",
            Self::NetIdentity => "network_class_or_backref",
            Self::Session => "missing_session",
            Self::PeerList => "invalid_session_peers",
            Self::MatchKey => "invalid_match_identity",
            Self::LocalController => "invalid_local_controller",
            Self::Roster => "invalid_native_roster",
            Self::ActorPointer => "invalid_actor_pointer",
            Self::ActorBinding => "actor_class_or_binding",
            Self::ActorPeer => "actor_peer_not_in_session",
            Self::ActorIdentity => "invalid_actor_identity",
            Self::LocalIdentity => "missing_local_identity",
            Self::Time => "invalid_world_time",
            Self::Context => "invalid_context_contract",
        }
    }
}
static FRAME_LOG_BITS: AtomicU32 = AtomicU32::new(0);
fn log_rejection(bits: &AtomicU32, reason: FrameRejection) {
    let bit = 1u32 << reason as u32;
    // At most 18 lines per DLL lifetime, including ordinary transition failures.
    if bits.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        tracing::info!(reason=reason.label(), "SCBL_MOD_EXPOSURE_FRAME rejected_native_context");
    }
}
unsafe fn inspect_pointer(info: Option<usize>) -> Result<Frame, FrameRejection> {
    inspect(info.ok_or(FrameRejection::Information)?)
}
unsafe fn from_pointer(info: Option<usize>) -> Option<Frame> {
    match inspect_pointer(info) {
        Ok(frame) => Some(frame),
        Err(reason) => { log_rejection(&FRAME_LOG_BITS, reason); None }
    }
}
#[cfg(test)]
pub(crate) unsafe fn from_info(info: usize) -> Option<Frame> {
    from_pointer(Some(info))
}

unsafe fn inspect(info: usize) -> Result<Frame, FrameRejection> {
    let world = ptr(info, 0x144).ok_or(FrameRejection::World)?;
    let wi = ptr(world, 0x6d0).ok_or(FrameRejection::WorldInfo)?;
    let manager = ptr(wi, 0x378).ok_or(FrameRejection::Manager)?;
    let net = ptr(manager, 0x2c0).ok_or(FrameRejection::Net)?;
    if word(net, 0) != Some(0x2af70fc) || ptr(net, 0x88).ok_or(FrameRejection::NetIdentity)? != manager {
        return Err(FrameRejection::NetIdentity);
    }
    let session = ptr(net, 0x50).ok_or(FrameRejection::Session)?;
    let ids = peers(session).ok_or(FrameRejection::PeerList)?;
    let match_key = key(net).ok_or(FrameRejection::MatchKey)?;
    let local_pc = crate::local_player::controller(world).ok_or(FrameRejection::LocalController)?;
    let raw = crate::ats_native_source::feedback_people(info).ok_or(FrameRejection::Roster)?;
    let mut people = Vec::new();
    let mut local = None;
    for (pc, pawn, team, role) in raw {
        // Remote pawns can precede their replicated actor proxy. Omit only
        // this exact-null remote entry, as the roster already does for an
        // unspawned pawn; never invent its peer or reuse a previous identity.
        // Missing local identity and every malformed non-null mapping still
        // invalidate the frame below.
        let actor = match word(pawn, 0x2c0).ok_or(FrameRejection::ActorPointer)? as usize {
            0 if pc != local_pc => continue,
            value if value >= 0x10000 => value,
            _ => return Err(FrameRejection::ActorPointer),
        };
        if word(actor, 0) != Some(0x2b1cd1c)
            || ptr(actor, 0x88).ok_or(FrameRejection::ActorBinding)? != pawn
            || ptr(actor, 0x50).ok_or(FrameRejection::ActorBinding)? != session
        {
            return Err(FrameRejection::ActorBinding);
        }
        let peer = word(actor, 0x18).ok_or(FrameRejection::ActorPeer)?;
        if !ids.contains(&peer) {
            return Err(FrameRejection::ActorPeer);
        }
        let actor = key(actor).ok_or(FrameRejection::ActorIdentity)?;
        if pc == local_pc {
            local = Some((actor, pawn, u64::from(word(pawn, 0x20).ok_or(FrameRejection::LocalIdentity)?)));
        }
        people.push(Person {
            actor,
            peer,
            team,
            role,
        });
    }
    let (local, pawn, generation) = local.ok_or(FrameRejection::LocalIdentity)?;
    let now = float(world, 0x5a0).ok_or(FrameRejection::Time)? as f64 * 1000.;
    if !now.is_finite() || now < 0. || now >= u64::MAX as f64 {
        return Err(FrameRejection::Time);
    }
    let context = Context {
        scope: [world as u64, net as u64, local, generation],
        match_key,
        now: now.round() as u64,
        local,
        people,
        session_peers: ids,
    };
    context.valid().then_some(Frame {
        context,
        manager,
        net,
        world,
        pawn,
    }).ok_or(FrameRejection::Context)
}
pub(crate) unsafe fn current() -> Option<Frame> {
    from_pointer(ptr(0x339e5a0, 0))
}
/// Direct copy avoids native FString's mutating terminator/allocator getter.
pub(crate) unsafe fn text(message: usize) -> Option<Vec<u16>> {
    let b = ptr(message, 0x40)?;
    let n = word(b, 4)? as usize;
    let cap = word(b, 8)? as usize;
    if n == 0 || n > crate::goggle_feedback_wire::NATIVE_LIMIT || cap < n || cap > 4096 {
        return None;
    }
    let start = b.checked_add(0xc)?;
    if !readable(start, n * 2) {
        return None;
    }
    let text: Vec<_> = (0..n)
        .map(|i| ((start + i * 2) as *const u16).read_unaligned())
        .collect();
    (!text.contains(&0)).then_some(text)
}
#[cfg(test)]
#[path = "goggle_peer_frame_tests.rs"]
mod tests;
