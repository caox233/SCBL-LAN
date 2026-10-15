//! R58 candidate: preserve the native OnStation body at the *one* forced-fall
//! call in UMPMercUseDroneState::Exit. No global fall/destruction hook.
//!
//! Runtime wiring owns the native lifecycle and semantic input as one unit.
//! Retail-byte tests isolate external world/render/transport leaves.
//! The retail-byte fixture exercises the whole MP exit with isolated leaf calls.
#![allow(dead_code)]
#[path = "drone_action_flow.rs"]
mod action_flow;
pub(super) use action_flow::{ActionFlow, Press};

#[path = "drone_control_runtime.rs"]
mod runtime;
use crate::equipment_native::{float, word};
pub(super) use runtime::{deinit, hud_wait, init};
use sha2::{Digest, Sha256};
use std::cell::Cell;

const EXIT: usize = 0x014b2390;
const EXIT_SHA: &str = "50191f90e816b0036d326adf09ba6bce53954d40e0f17ca40535f8b4c832840b";
const CALLS: [(usize, usize); 11] = [
    (0x62, 0xb07c90),
    (0x8a, 0x42d850),
    (0xa3, 0x42d7b0),
    (0xc1, 0xb07a90),
    (0xcd, 0x52e7f0),
    (0xdc, 0xe69000),
    (0x104, 0xd5e8e0),
    (0x112, 0xdb4e90),
    (0x122, 0xdb9630),
    (0x131, 0xe5e6e0),
    (0x145, 0x149fe60),
];

/// Only the decoded direct calls are relocated. The six-byte vtable load at
/// 14B247D becomes MOV EDX, bridge; NOP. CALL EDX and all native cleanup remain.
pub(super) fn exit_window(bytes: &[u8], entry: usize, bridge: usize) -> Option<Vec<u8>> {
    if bytes.len() != 347
        || format!("{:x}", Sha256::digest(bytes)) != EXIT_SHA
        || entry > u32::MAX as usize
        || bridge == 0
        || bridge > u32::MAX as usize
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (offset, target) in CALLS {
        if out[offset] != 0xe8 {
            return None;
        }
        let original = i32::from_le_bytes(out[offset + 1..offset + 5].try_into().ok()?);
        if (EXIT + offset + 5).wrapping_add_signed(original as isize) != target {
            return None;
        }
        out[offset + 1..offset + 5].copy_from_slice(
            &(target as u32)
                .wrapping_sub((entry + offset + 5) as u32)
                .to_le_bytes(),
        );
    }
    if out[0xed..0xf3] != [0x8b, 0x90, 0x54, 0x07, 0, 0] {
        return None;
    }
    out[0xed] = 0xba;
    out[0xee..0xf2].copy_from_slice(&(bridge as u32).to_le_bytes());
    out[0xf2] = 0x90;
    Some(out)
}

#[derive(Clone, Copy)]
pub(super) struct Layout {
    state: u32,
    owner: u32,
    ability: u32,
    armor: u32,
    body: u32,
}
impl Layout {
    pub(super) const RETAIL: Self = Self {
        state: 0x02bebe74,
        owner: 0x02ad2abc,
        ability: 0x02a3f14c,
        armor: 0x02ad42ac,
        body: 0x02a88f54,
    };
    #[cfg(test)]
    pub(super) fn relocated(delta: usize) -> Self {
        let r = |p: u32| p.wrapping_add(delta as u32);
        Self {
            state: r(Self::RETAIL.state),
            owner: r(Self::RETAIL.owner),
            ability: r(Self::RETAIL.ability),
            armor: r(Self::RETAIL.armor),
            body: r(Self::RETAIL.body),
        }
    }
}

// This is a one-use value, not a Clone/Copy token or a global pointer cache.
// The action bridge must drop it on cancel and must not manufacture it on exit.
pub(super) struct VoluntaryExit {
    state: usize,
    owner: usize,
    controller: u32,
    ability: usize,
    body: usize,
    layout: Layout,
}
impl VoluntaryExit {
    pub(super) unsafe fn request(
        state: usize,
        ability: usize,
        action: u32,
        detonate: u32,
        layout: Layout,
        native_blocked: impl FnOnce(usize) -> bool,
    ) -> Option<Self> {
        if action == 0 || detonate == 0 || action != detonate {
            return None;
        }
        let owner = word(state, 0x48)? as usize;
        let body = word(state, 0x138)? as usize;
        let controller = word(state, 0x64)?;
        let token = Self {
            state,
            owner,
            controller,
            ability,
            body,
            layout,
        };
        (token.valid(2) && !native_blocked(owner)).then_some(token)
    }
    unsafe fn valid(&self, body_state: u32) -> bool {
        word(self.state, 0) == Some(self.layout.state)
            && word(self.state, 0x48) == Some(self.owner as u32)
            && self.controller != 0 && word(self.state, 0x64) == Some(self.controller)
            && word(self.state, 0x138) == Some(self.body as u32)
            && word(self.owner, 0) == Some(self.layout.owner)
            // B8B300 is the native +330 dead predicate for both MP pawn classes.
            && word(self.owner, 0x60).is_some_and(|f| f & 0x800 == 0)
            && word(self.owner, 0x4c).is_some_and(|f| matches!((f >> 8) & 255, 1 | 3))
            && word(self.owner, 0x938) == Some(self.controller)
            && word(self.owner, 0xcf4).is_some_and(|armor| {
                word(armor as usize, 0) == Some(self.layout.armor)
                    && word(armor as usize, 0x5c) == Some(self.ability as u32)
            })
            && word(self.ability, 0) == Some(self.layout.ability)
            && word(self.ability, 0x28).is_some_and(|f| f & 255 == 3)
            && word(self.ability, 0x6c) == Some(self.owner as u32)
            && word(self.ability, 0xa0) == Some(self.body as u32)
            // Native reset/disabled/charging states cannot authorize retention.
            && word(self.ability, 0x40).is_some_and(|f| f & 0x2e == 8)
            && float(self.ability, 0x98).is_some_and(|v| v == 0.0)
            && float(self.ability, 0x7c).is_some_and(|v| v.is_finite() && v > 0.0 && v <= 1.0)
            && word(self.body, 0) == Some(self.layout.body)
            && word(self.body, 0x9fc) == Some(self.owner as u32)
            && word(self.body, 0x4b0).is_some_and(|f| f & 0x80000000 == 0)
            && word(self.body, 0x5dc).is_some_and(|hp| (hp as i32) > 0)
            && word(self.body, 0x910).is_some_and(|s| s & 255 == body_state)
    }
}

struct Scope<'a> {
    token: &'a VoluntaryExit,
    native_blocked: &'a dyn Fn(usize) -> bool,
    consumed: Cell<bool>,
}
thread_local! { static EXIT_SCOPE: Cell<*const Scope<'static>> = const { Cell::new(std::ptr::null()) }; }

// A separate, one-use terminal scope. This cannot authorize voluntary parking
// or affect a global destruction entry: only the existing MPExit fall site reads it.
struct DestroyedExit { state: usize, body: usize, layout: Layout, consumed: Cell<bool>, previous: *const DestroyedExit }
thread_local! {
    static DESTROYED_EXIT: Cell<*const DestroyedExit> = const { Cell::new(std::ptr::null()) };
    static NATIVE_EXIT_STATE: Cell<usize> = const { Cell::new(0) };
}
struct RestoreDestroyed(*const DestroyedExit);
impl Drop for RestoreDestroyed {
    fn drop(&mut self) { DESTROYED_EXIT.with(|s| s.set(self.0)); }
}
unsafe fn with_destroyed_exit<R>(state: usize, body: usize, layout: Layout, native: impl FnOnce() -> R) -> R {
    let scope = DestroyedExit { state, body, layout, consumed: Cell::new(false), previous: DESTROYED_EXIT.with(Cell::get) };
    let _restore = RestoreDestroyed(DESTROYED_EXIT.with(|s| s.replace(&scope)));
    native()
}
// Native animation cancellation precedes the native finished flag. Walk only
// synchronous stack-owned TLS scopes; never hold a mutex across native callbacks.
unsafe fn destroyed_exit_in_progress(state: usize) -> bool {
    DESTROYED_EXIT.with(|slot| {
        let mut p = slot.get();
        while let Some(scope) = p.as_ref() {
            if scope.state == state { return true; }
            p = scope.previous;
        }
        false
    })
}
unsafe fn skip_repeated_destroyed_fall(body: usize) -> bool {
    DESTROYED_EXIT.with(|slot| {
        let Some(scope) = slot.get().as_ref() else { return false; };
        if NATIVE_EXIT_STATE.with(Cell::get) != scope.state || scope.consumed.get() || scope.body != body
            || word(scope.state, 0) != Some(scope.layout.state)
            || word(scope.state, 0x138) != Some(body as u32)
            || word(body, 0) != Some(scope.layout.body)
            || word(body, 0x9fc) != word(scope.state, 0x48)
        { return false; }
        scope.consumed.set(true);
        // A disabled body already falling must not restart its native effects/
        // velocity when the delayed control exit completes. Flying bodies still fall.
        word(body, 0x910).is_some_and(|v| v & 255 == 3)
    })
}

/// Run the cloned native exit once. An absent/invalid inner request masks an
/// outer request; scope cannot leak to another native thread or a later fall.
pub(super) unsafe fn exit_with_request<R>(
    request: Option<VoluntaryExit>,
    state: usize,
    ability: usize,
    native_blocked: impl Fn(usize) -> bool,
    native: impl FnOnce() -> R,
) -> R {
    exit_and_capture(request, state, ability, native_blocked, native).0
}
pub(super) unsafe fn exit_and_capture<R>(
    request: Option<VoluntaryExit>,
    state: usize,
    ability: usize,
    native_blocked: impl Fn(usize) -> bool,
    native: impl FnOnce() -> R,
) -> (R, Option<ParkedDrone>) {
    // A nested exit for another state must not consume an outer terminal scope.
    struct RestoreNativeState(usize);
    impl Drop for RestoreNativeState {
        fn drop(&mut self) { NATIVE_EXIT_STATE.with(|s| s.set(self.0)); }
    }
    let _native_state = RestoreNativeState(NATIVE_EXIT_STATE.with(|s| s.replace(state)));
    let permit = request
        .as_ref()
        .filter(|r| r.state == state && r.ability == ability && r.valid(1));
    let permit = permit.filter(|r| !native_blocked(r.owner));
    let scope = permit.map(|token| Scope {
        token,
        native_blocked: &native_blocked,
        consumed: Cell::new(false),
    });
    struct Restore(*const Scope<'static>);
    impl Drop for Restore {
        fn drop(&mut self) {
            EXIT_SCOPE.with(|s| s.set(self.0));
        }
    }
    let pointer = scope
        .as_ref()
        .map_or(std::ptr::null(), |s| s as *const Scope<'_>);
    let _restore = Restore(EXIT_SCOPE.with(|s| s.replace(pointer.cast())));
    let result = native();
    let parked = scope
        .as_ref()
        .filter(|s| s.consumed.get() && !(s.native_blocked)(s.token.owner))
        .and_then(|s| ParkedDrone::after_exit(s.token));
    (result, parked)
}

/// Called only by the cloned MP exit's original +754 call site, never installed
/// on B0F3B0 itself. Unknown, forced, cancelled and repeated calls stay native.
pub(super) unsafe fn forced_fall_site(body: usize, native_fall: impl FnOnce()) -> bool {
    if skip_repeated_destroyed_fall(body) { return true; }
    let skip = EXIT_SCOPE.with(|slot| {
        let p = slot.get();
        if p.is_null() {
            return false;
        }
        let scope = &*p;
        if scope.consumed.get()
            || scope.token.body != body
            || !scope.token.valid(1)
            || (scope.native_blocked)(scope.token.owner)
        {
            return false;
        }
        scope.consumed.set(true);
        true
    });
    if !skip {
        native_fall();
    }
    skip
}

/// Scoped *existing-body* eligibility, never substituted for normal activation.
/// Preserve every native pose, EMP, disabled, pending-charge and movement check.
/// Only a new cast's minimum starting battery and already-active veto are not
/// relevant to resuming a positively charged drone proven by ParkedDrone.
pub(super) fn reentry_check_windows(
    base: &[u8],
    drone: &[u8],
    base_at: usize,
    drone_at: usize,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if base.len() != 195
        || drone.len() != 93
        || base_at == 0
        || drone_at == 0
        || base_at.checked_add(base.len())? > u32::MAX as usize
        || drone_at.checked_add(drone.len())? > u32::MAX as usize
        || format!("{:x}", Sha256::digest(base))
            != "28c50a72ccafc40daa3bf239cb29589c153083f288f6b9d2c5b898ac7ee9ffe9"
        || format!("{:x}", Sha256::digest(drone))
            != "82d2f4452b7f38b45a2ecc3001317d4e0371599a3b3a9172c030de4a1510a06b"
    {
        return None;
    }
    let mut b = base.to_vec();
    let mut d = drone.to_vec();
    for (bytes, original, entry, calls) in [
        (
            &mut b,
            0xe7e7a0usize,
            base_at,
            vec![
                (0x63, 0xd5e8e0),
                (0x76, 0xd5e8e0),
                (0x89, 0xd5e8e0),
                (0x96, 0xe75e90),
                (0xa8, 0xd5e8e0),
            ],
        ),
        (
            &mut d,
            0xe7eb00usize,
            drone_at,
            vec![
                (0xf, 0xd5e8e0),
                (0x22, 0xd5e8e0),
                (0x44, 0xd11c50),
                (0x53, 0xe7e7a0),
            ],
        ),
    ] {
        for (off, target) in calls {
            if bytes[off] != 0xe8 {
                return None;
            }
            let rel = i32::from_le_bytes(bytes[off + 1..off + 5].try_into().ok()?);
            if (original + off + 5).wrapping_add_signed(rel as isize) != target {
                return None;
            }
            let target = if target == 0xe7e7a0 { base_at } else { target };
            bytes[off + 1..off + 5].copy_from_slice(
                &(target as u32)
                    .wrapping_sub((entry + off + 5) as u32)
                    .to_le_bytes(),
            );
        }
    }
    if b[0x1f..0x21] != [0x0f, 0x87] || b[0x30..0x32] != [0x0f, 0x84] {
        return None;
    }
    b[0x1f..0x25].fill(0x90);
    b[0x30..0x36].fill(0x90);
    Some((b, d))
}

/// Native eligibility is thiscall(this, u32) -> AL, and pops four stack bytes.
/// Share this adapter with the native-byte regression, not a different test ABI.
pub(super) unsafe fn native_entry_allowed(entry: usize, ability: usize) -> bool {
    let check: unsafe extern "thiscall" fn(usize, u32) -> u8 = std::mem::transmute(entry);
    check(ability, 0) != 0
}

/// Exact body-EMP routine, changing only its owner-feedback lookup argument
/// from owner (EAX) to body (ESI) and calling our local cdecl adapter. No global
/// HUD/event hook, no temporary owner-pointer writes, no altered damage or score.
pub(super) fn emp_feedback_window(bytes: &[u8], entry: usize, bridge: usize) -> Option<Vec<u8>> {
    const ORIGINAL: usize = 0xaea010;
    if bytes.len() != 385
        || format!("{:x}", Sha256::digest(bytes))
            != "dd99cd6345de8ed1874e8fd609c2e5f52590dec1b347ac23ccf97b2a9bd21d8c"
        || entry == 0
        || entry > u32::MAX as usize
        || bridge == 0
        || bridge > u32::MAX as usize
        || bytes[0xfe] != 0x50
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, target) in [
        (0x0a, 0xe69000usize),
        (0xff, 0xe93df0),
        (0x12c, 0xa32bc0),
        (0x147, 0xd873f0),
        (0x165, 0xe8ca30),
        (0x174, 0xef7520),
    ] {
        if out[off] != 0xe8 {
            return None;
        }
        let rel = i32::from_le_bytes(out[off + 1..off + 5].try_into().ok()?);
        if (ORIGINAL + off + 5).wrapping_add_signed(rel as isize) != target {
            return None;
        }
        let target = if off == 0xff { bridge } else { target };
        out[off + 1..off + 5].copy_from_slice(
            &(target as u32)
                .wrapping_sub((entry + off + 5) as u32)
                .to_le_bytes(),
        );
    }
    out[0xfe] = 0x56; // push ESI(body), same one-argument cdecl ABI and caller cleanup.
    Some(out)
}

/// Revalidate at the feedback instruction, after native EMP FX creation.
/// b68 may already be set: it is not a reason to leak feedback to a parked owner.
/// Returning zero uses native "no local feedback recipient" branch only.
pub(super) unsafe fn emp_feedback_owner_with(
    body: usize,
    layout: Layout,
    in_control: impl FnOnce(usize) -> bool,
    native_owner_lookup: impl FnOnce(usize) -> usize,
) -> usize {
    let owner = word(body, 0x9fc).unwrap_or(0) as usize;
    let ability = word(owner, 0xcf4)
        .and_then(|armor| word(armor as usize, 0x5c))
        .unwrap_or(0) as usize;
    if parked_body(ability, layout) == Some((owner, body))
        && word(owner, 0x60).is_some_and(|flags| flags & 0x800 == 0)
        && !in_control(owner)
        && parked_body(ability, layout) == Some((owner, body))
    {
        return 0;
    }
    native_owner_lookup(owner)
}

/// One native owner-damage callsite, not the global Cancel virtual function.
/// Full surrounding bytes fix the instruction boundary and damage path.
pub(super) const OWNER_HIT_ORIGINAL: [u8; 6] = [0x8b, 0x90, 0xb8, 0, 0, 0];
pub(super) fn owner_hit_patch(context: &[u8], bridge: usize) -> Option<[u8; 6]> {
    if context.len() != 54
        || format!("{:x}", Sha256::digest(context))
            != "2ed5b49c8a6a30fbe34afd3fad89ae1b608083afd4d94c36769287f79610d286"
        || context[30..36] != OWNER_HIT_ORIGINAL
        || bridge == 0
        || bridge > u32::MAX as usize
    {
        return None;
    }
    let mut bytes = [0xba, 0, 0, 0, 0, 0x90];
    bytes[1..5].copy_from_slice(&(bridge as u32).to_le_bytes());
    Some(bytes)
}


/// Match the retail 8DCBA0 predicates used by B2A5F0: Electric plus
/// eElectric_IsTazer(1) or eElectric_IsStickyShocker(2). Other damage stays native.
pub(super) fn drone_disabling_electric(bits: u32) -> bool {
    bits & 15 == 6 && ((bits >> 4) & 3) != 0
}

/// At DC1E27 EBX is the original immutable DamageInfo, ECX the current ability.
/// Insert that pointer as one thiscall stack argument; do not touch EBX/ESI.
pub(super) fn owner_hit_context_thunk(bridge: usize) -> Option<Vec<u8>> {
    if bridge == 0 || bridge > u32::MAX as usize { return None; }
    let mut out = vec![0x58, 0x53, 0x50, 0xb8]; // return, EBX, return, target
    out.extend_from_slice(&(bridge as u32).to_le_bytes());
    out.extend_from_slice(&[0xff, 0xe0]);
    Some(out)
}

/// At B2A77A ESI is the drone, ECX its owner. Preserve all three original
/// event arguments and add the owner while transferring ECX to the drone.
pub(super) fn body_electric_context_thunk(bridge: usize) -> Option<Vec<u8>> {
    if bridge == 0 || bridge > u32::MAX as usize { return None; }
    let mut out = vec![0x58, 0x51, 0x50, 0x89, 0xf1, 0xb8];
    out.extend_from_slice(&(bridge as u32).to_le_bytes());
    out.extend_from_slice(&[0xff, 0xe0]);
    Some(out)
}

pub(super) const BODY_ELECTRIC_CALL: usize = 0xb2a77a;
pub(super) const BODY_ELECTRIC_ORIGINAL: [u8; 5] = [0xe8, 0x71, 0xcc, 0x25, 0x00];
pub(super) fn body_electric_patch(context: &[u8], bridge: usize) -> Option<[u8; 5]> {
    if context.len() != 455
        || format!("{:x}", Sha256::digest(context))
            != "c8f002f2d592b8c5df2923338ea8418e7894e540ee4a9cc64cfb6f5077c92114"
        || context[0x18a..0x18f] != BODY_ELECTRIC_ORIGINAL
        || bridge == 0 || bridge > u32::MAX as usize
    { return None; }
    let mut out = [0xe8, 0, 0, 0, 0];
    out[1..].copy_from_slice(&(bridge as u32).wrapping_sub((BODY_ELECTRIC_CALL + 5) as u32).to_le_bytes());
    Some(out)
}

pub(super) unsafe fn retain_parked_for_damage(flow: &ActionFlow, ability: usize, info: usize) -> bool {
    // Unknown DamageInfo fails back to the native cancel, never to immunity.
    word(info, 0x40).is_some_and(|bits| !drone_disabling_electric(bits))
        && flow.keep_parked_on_owner_hit(ability)
}

/// Only entered at the native accepted Tazer/StickyShocker Disable callsite.
/// Native hit acceptance, score, source, event arguments and return are retained.
/// Deliver the absent control-state receiver only for the same parked authority.
pub(super) unsafe fn parked_electric_dispatch<R>(
    body: usize, owner: usize, layout: Layout,
    mut in_control: impl FnMut(usize) -> bool,
    native: impl FnOnce() -> R, disable: impl FnOnce(usize) -> bool,
) -> (R, bool) {
    let identity = (|| {
        if word(body, 0) != Some(layout.body) || word(body, 0x9fc) != Some(owner as u32) {
            return None;
        }
        let ability = word(owner, 0xcf4).and_then(|p| word(p as usize, 0x5c))? as usize;
        (parked_body(ability, layout) == Some((owner, body)) && !in_control(owner))
            .then_some((ability, word(body, 0x2c0)))
    })();
    let result = native();
    let completed = identity.is_some_and(|(ability, source)| {
        parked_body(ability, layout) == Some((owner, body))
            && word(body, 0x2c0) == source && !in_control(owner) && disable(body)
    });
    (result, completed)
}

/// Native body EMP always runs once, including feedback/score/Disable. Only
/// complete the missing parked receiver afterward, never force a second fall.
pub(super) unsafe fn parked_emp_dispatch<R>(
    body: usize,
    layout: Layout,
    mut in_control: impl FnMut(usize) -> bool,
    native: impl FnOnce() -> R,
    disable: impl FnOnce(usize) -> bool,
) -> (R, bool) {
    let identity = (|| {
        if word(body, 0) != Some(layout.body) || word(body, 0xb68) != Some(0) {
            return None;
        }
        let owner = word(body, 0x9fc)? as usize;
        let armor = word(owner, 0xcf4)? as usize;
        let ability = word(armor, 0x5c)? as usize;
        (parked_body(ability, layout) == Some((owner, body)) && !in_control(owner))
            .then_some((owner, ability))
    })();
    let result = native();
    let completed = identity.is_some_and(|(owner, ability)| {
        parked_body(ability, layout) == Some((owner, body)) && !in_control(owner) && disable(body)
    });
    (result, completed)
}

/// Terminal ownership is deliberately independent of battery/active flags:
/// native deactivation has already consumed those by the time it dispatches.
/// Only the current exact MP merc ability and its parked body qualify.
pub(super) unsafe fn parked_body(ability: usize, layout: Layout) -> Option<(usize, usize)> {
    if word(ability, 0) != Some(layout.ability) || word(ability, 0x28).map(|v| v & 255) != Some(3) {
        return None;
    }
    let owner = word(ability, 0x6c)? as usize;
    let body = word(ability, 0xa0)? as usize;
    let armor = word(owner, 0xcf4)? as usize;
    (word(owner, 0) == Some(layout.owner)
        && word(owner, 0x4c).is_some_and(|v| matches!((v >> 8) & 255, 1 | 3))
        && word(armor, 0) == Some(layout.armor)
        && word(armor, 0x5c) == Some(ability as u32)
        && word(body, 0) == Some(layout.body)
        && word(body, 0x9fc) == Some(owner as u32)
        && word(body, 0x4c).is_some_and(|v| matches!((v >> 8) & 255, 1 | 3))
        && word(body, 0x4b0).is_some_and(|v| v & 0x80000000 == 0)
        && word(body, 0x910).map(|v| v & 255) == Some(1))
    .then_some((owner, body))
}

/// Complete only a parked actor whose Disable receiver has already exited.
pub(super) unsafe fn terminate_parked_with(
    ability: usize,
    layout: Layout,
    control_state_active: impl FnOnce(usize) -> bool,
    disable: impl FnOnce(usize) -> bool,
) -> bool {
    let Some((owner, body)) = parked_body(ability, layout) else {
        return false;
    };
    if control_state_active(owner) || parked_body(ability, layout) != Some((owner, body)) {
        return false;
    }
    disable(body)
}

/// A live parked identity, created only after the one permitted MP-exit call.
/// The native ability clock counts game time, not OS time, key-down duration,
/// or a fresh timer at each press. No raw input/virtual-key dependency.
pub(super) struct ParkedDrone {
    owner: usize,
    controller: u32,
    ability: usize,
    body: usize,
    layout: Layout,
    exit_elapsed: f32,
}
#[derive(Debug, PartialEq)]
pub(super) enum Reentry {
    Invalid,
    Wait(f32),
    Ready,
}
impl ParkedDrone {
    // Capture before MP exit; commit only if native exit kept the same body and
    // detached the control-state pointer. Never create a ticket from a tick.
    pub(super) unsafe fn after_exit(request: &VoluntaryExit) -> Option<Self> {
        let value = Self {
            owner: request.owner,
            controller: request.controller,
            ability: request.ability,
            body: request.body,
            layout: request.layout,
            exit_elapsed: float(request.ability, 0x80)?,
        };
        (word(request.state, 0x138) == Some(0)
            && value.exit_elapsed.is_finite()
            && value.exit_elapsed >= 0.0
            && value.live())
        .then_some(value)
    }
    unsafe fn live(&self) -> bool {
        self.live_as(1)
    }
    unsafe fn live_as(&self, state: u32) -> bool {
        // Read from the current ability/pawn graph on every use. A removed body,
        // replacement pawn/controller, cancelled cast or new round invalidates it.
        word(self.owner, 0) == Some(self.layout.owner)
            && word(self.owner, 0x60).is_some_and(|v| v & 0x800 == 0)
            && word(self.owner, 0x4c).is_some_and(|v| matches!((v >> 8) & 255, 1 | 3))
            && word(self.owner, 0x938) == Some(self.controller)
            && word(self.owner, 0xcf4).is_some_and(|armor| {
                word(armor as usize, 0) == Some(self.layout.armor)
                    && word(armor as usize, 0x5c) == Some(self.ability as u32)
            })
            && word(self.ability, 0) == Some(self.layout.ability)
            && word(self.ability, 0x28).is_some_and(|v| v & 255 == 3)
            && word(self.ability, 0x6c) == Some(self.owner as u32)
            && word(self.ability, 0xa0) == Some(self.body as u32)
            && word(self.ability, 0x40).is_some_and(|v| v & 0x3e == 8)
            && float(self.ability, 0x98).is_some_and(|v| v == 0.0)
            && float(self.ability, 0x7c).is_some_and(|v| v.is_finite() && v > 0.0 && v <= 1.0)
            && word(self.body, 0) == Some(self.layout.body)
            && word(self.body, 0x9fc) == Some(self.owner as u32)
            && word(self.body, 0x4b0).is_some_and(|v| v & 0x80000000 == 0)
            && word(self.body, 0x910).is_some_and(|v| v & 255 == state)
            && word(self.body, 0x5dc).is_some_and(|v| (v as i32) > 0)
    }
    pub(super) unsafe fn check(
        &self,
        ability: usize,
        native_allowed: impl FnOnce(usize) -> bool,
    ) -> Reentry {
        if ability != self.ability || !self.live() {
            return Reentry::Invalid;
        }
        let Some(now) = float(ability, 0x80).filter(|v| v.is_finite() && *v >= self.exit_elapsed)
        else {
            return Reentry::Invalid;
        };
        let remaining = 3.0 - (now - self.exit_elapsed);
        if remaining > 0.0 {
            return Reentry::Wait(remaining);
        }
        if native_allowed(self.owner) {
            Reentry::Ready
        } else {
            Reentry::Invalid
        }
    }
    /// Both callbacks run only from the native ability-press route. `enter`
    /// must queue the existing MPMercUseDroneState; it must not activate/spawn.
    /// Recheck *after* native eligibility, which can dispatch native callbacks.
    pub(super) unsafe fn press(
        &self,
        ability: usize,
        native_allowed: impl FnOnce(usize) -> bool,
        enter: impl FnOnce(usize, usize),
    ) -> Reentry {
        let result = self.check(ability, native_allowed);
        if result == Reentry::Ready {
            if self.check(ability, |_| true) != Reentry::Ready {
                return Reentry::Invalid;
            }
            enter(self.owner, self.body);
        }
        result
    }
}

/// FE1060 also asks the actor to relocate to its owner's transform. That is a
/// first-deployment action, not a valid existing-drone reentry. Keep all native
/// network dirty/activation calls; redirect only this one transform call site.
pub(super) fn replication_activate_window(
    bytes: &[u8],
    at: usize,
    bridge: usize,
) -> Option<Vec<u8>> {
    if bytes.len() != 92
        || at == 0
        || bridge == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || bridge > u32::MAX as usize
        || format!("{:x}", Sha256::digest(bytes))
            != "20ec0d80a1739c5096a0a4d728ae04985dd2860b335cf169da5b7b19eeb7e07d"
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, opcode, target) in [
        (0x16, 0xe9, 0x7748c0usize),
        (0x4f, 0xe8, 0x7e24f0),
        (0x57, 0xe9, 0x7fc740),
    ] {
        if out[off] != opcode {
            return None;
        }
        let rel = i32::from_le_bytes(out[off + 1..off + 5].try_into().ok()?);
        if (0xfe1060usize + off + 5).wrapping_add_signed(rel as isize) != target {
            return None;
        }
        out[off + 1..off + 5].copy_from_slice(
            &(target as u32)
                .wrapping_sub((at + off + 5) as u32)
                .to_le_bytes(),
        );
    }
    if out[0x44..0x4a] != [0x8b, 0x82, 0x3c, 4, 0, 0] {
        return None;
    }
    out[0x44] = 0xb8;
    out[0x45..0x49].copy_from_slice(&(bridge as u32).to_le_bytes());
    out[0x49] = 0x90;
    Some(out)
}
struct PositionScope<'a> {
    parked: &'a ParkedDrone,
    consumed: Cell<bool>,
    placement_consumed: Cell<bool>,
    position: [u32; 3],
    rotation: [u32; 3],
    current: &'a dyn Fn() -> bool,
}
thread_local! {static POSITION_SCOPE:Cell<*const PositionScope<'static>>=const{Cell::new(std::ptr::null())};}
/// Used only while executing a confirmed existing-body native MP entry. Queuing
/// a state transition is not entry: the asynchronous bridge must consume its
/// pending token at actual control startup (after its animation), not MP Enter.
/// The scope never survives a native call or frame.
pub(super) unsafe fn with_reentry_position<R>(
    ticket: Option<&ParkedDrone>,
    native: impl FnOnce() -> R,
) -> R {
    with_current_reentry_position(ticket, || true, native)
}
unsafe fn with_current_reentry_position<R>(
    ticket: Option<&ParkedDrone>,
    current: impl Fn() -> bool,
    native: impl FnOnce() -> R,
) -> R {
    let scope = ticket.filter(|p| p.live() && current()).and_then(|parked| {
        let position = [
            word(parked.body, 0x9c)?,
            word(parked.body, 0xa0)?,
            word(parked.body, 0xa4)?,
        ];
        if !position.iter().all(|v| f32::from_bits(*v).is_finite()) {
            return None;
        }
        let rotation = [
            word(parked.body, 0xa8)?,
            word(parked.body, 0xac)?,
            word(parked.body, 0xb0)?,
        ];
        Some(PositionScope {
            parked,
            consumed: Cell::new(false),
            placement_consumed: Cell::new(false),
            position,
            rotation,
            current: &current,
        })
    });
    let pointer = scope
        .as_ref()
        .map_or(std::ptr::null(), |s| s as *const PositionScope<'_>);
    struct Restore(*const PositionScope<'static>);
    impl Drop for Restore {
        fn drop(&mut self) {
            POSITION_SCOPE.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(POSITION_SCOPE.with(|s| s.replace(pointer.cast())));
    native()
}
pub(super) unsafe fn reentry_position_site<R>(
    body: usize,
    position: *const u32,
    rotation: *const u32,
    native: impl FnOnce(*const u32, *const u32) -> R,
) -> R {
    preserved_transform_site(body, position, rotation, false, native)
}
/// MP control startup positions the body before activating replication. Preserve
/// this first transform as well, without skipping ownership or physics setup.
pub(super) unsafe fn reentry_placement_site<R>(
    body: usize,
    position: *const u32,
    rotation: *const u32,
    native: impl FnOnce(*const u32, *const u32) -> R,
) -> R {
    preserved_transform_site(body, position, rotation, true, native)
}
unsafe fn preserved_transform_site<R>(
    body: usize,
    position: *const u32,
    rotation: *const u32,
    placement: bool,
    native: impl FnOnce(*const u32, *const u32) -> R,
) -> R {
    let preserved = POSITION_SCOPE.with(|slot| {
        let p = slot.get();
        if p.is_null() {
            return None;
        }
        let scope = &*p;
        let consumed = if placement {
            &scope.placement_consumed
        } else {
            &scope.consumed
        };
        if consumed.get()
            || scope.parked.body != body
            || !scope.parked.live_as(if placement { 1 } else { 2 })
            || !(scope.current)()
        {
            return None;
        }
        consumed.set(true);
        Some((scope.position, scope.rotation))
    });
    if let Some((position, rotation)) = preserved {
        native(position.as_ptr(), rotation.as_ptr())
    } else {
        native(position, rotation)
    }
}

/// Actual control startup may run after MP Entry returns. Keep its inventory,
/// activation, camera and notification calls; route only the verified MP v280.
pub(super) fn control_start_window(bytes: &[u8], at: usize, bridge: usize) -> Option<Vec<u8>> {
    if bytes.len() != 188
        || at == 0
        || bridge == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || bridge > u32::MAX as usize
        || format!("{:x}", Sha256::digest(bytes))
            != "0c49b1731be41f2ee5bb0942a2bb04d17dcc8c3e011fc0cadb7b21c58ba4270d"
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, target) in [(0x18, 0xd5fb60), (0x68, 0x8b5e60), (0x83, 0x8bcd10)] {
        relocate_call(&mut out, off, 0x14749e0, target, at, target)?;
    }
    if out[0x2e..0x34] != [0x8b, 0x90, 0x80, 2, 0, 0] {
        return None;
    }
    out[0x2e] = 0xba;
    out[0x2f..0x33].copy_from_slice(&(bridge as u32).to_le_bytes());
    out[0x33] = 0x90;
    Some(out)
}
/// Preserve native MP placement's owner binding and calculations. Only replace
/// its transform arguments in an exact retained-body reentry scope.
pub(super) fn mp_placement_window(bytes: &[u8], at: usize, bridge: usize) -> Option<Vec<u8>> {
    if bytes.len() != 470
        || at == 0
        || bridge == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || bridge > u32::MAX as usize
        || format!("{:x}", Sha256::digest(bytes))
            != "17392dcb84c9df2e8151761112b02851871e2d6cfd3def387f4e509fb28635b5"
    {
        return None;
    }
    let mut out = bytes.to_vec();
    relocate_call(&mut out, 0x1be, 0x1471810, 0xe68a20, at, bridge)?;
    relocate_call(&mut out, 0x1ca, 0x1471810, 0x24c245a, at, 0x24c245a)?;
    Some(out)
}

/// Preserve the complete native action startup guard. Only its final base-action
/// call is routed through the semantic-action bridge; no scan code or VK exists.
pub(super) fn action_window(bytes: &[u8], at: usize, bridge: usize) -> Option<Vec<u8>> {
    if bytes.len() != 136
        || format!("{:x}", Sha256::digest(bytes))
            != "4ba74e4ab011dd78ceabcb4d397ec13b8541dddf51d18423ff9629656b030c5a"
        || at == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || bridge == 0
        || bridge > u32::MAX as usize
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, original, target) in [
        (0x34, 0xe599e0, 0xe599e0),
        (0x46, 0xe599e0, 0xe599e0),
        (0x7d, 0x14cf100, bridge),
    ] {
        relocate_call(&mut out, off, 0x14d3490, original, at, target)?;
    }
    Some(out)
}

fn relocate_call(
    bytes: &mut [u8],
    off: usize,
    original_at: usize,
    expected: usize,
    at: usize,
    target: usize,
) -> Option<()> {
    if bytes.get(off) != Some(&0xe8) {
        return None;
    }
    let rel = i32::from_le_bytes(bytes.get(off + 1..off + 5)?.try_into().ok()?);
    if (original_at + off + 5).wrapping_add_signed(rel as isize) != expected {
        return None;
    }
    bytes[off + 1..off + 5].copy_from_slice(
        &(target as u32)
            .wrapping_sub((at + off + 5) as u32)
            .to_le_bytes(),
    );
    Some(())
}

/// Keep the native input route's local-owner, debounce, disabled and new-cast
/// branches. Intercept only the already-active ability branch. Its bridge must
/// call the original native press for every ability without a parked ticket.
pub(super) fn ability_press_window(bytes: &[u8], at: usize, bridge: usize) -> Option<Vec<u8>> {
    if bytes.len() != 235
        || format!("{:x}", Sha256::digest(bytes))
            != "3c0b9c7526bd50ea71cad099f21f47512bbbe4beae875d0fd180ab9c1d3771a4"
        || at == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || bridge == 0
        || bridge > u32::MAX as usize
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, target) in [(6, 0x55fbc0), (0x5d, 0xd75ca0), (0x66, 0xe6cc40)] {
        relocate_call(&mut out, off, 0xe7e870, target, at, target)?;
    }
    // Original active branch at +A5: preserve its ESI/stack contract and join
    // the original POP ESI; RET epilogue. Inactive/failure paths stay byte-exact.
    out[0xa5..0xe9].fill(0x90);
    out[0xa5..0xa8].copy_from_slice(&[0x8b, 0xce, 0xe8]);
    out[0xa8..0xac].copy_from_slice(
        &(bridge as u32)
            .wrapping_sub((at + 0xac) as u32)
            .to_le_bytes(),
    );
    out[0xac..0xae].copy_from_slice(&[0xeb, 0x3b]);
    Some(out)
}

/// Existing-body preparation from the exact native activation. Keep local-owner,
/// weapon slot selection, camera snapshot and state construction. Never call this
/// without a valid parked ticket and a matching native inventory slot 0x28.
/// The world spawn and inventory add/remove are new-deployment operations only.
pub(super) fn prepare_window(bytes: &[u8], at: usize, queue: usize) -> Option<Vec<u8>> {
    if bytes.len() != 366
        || at == 0
        || queue == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || queue > u32::MAX as usize
        || format!("{:x}", Sha256::digest(bytes))
            != "9a26031908c6b43fd69e75aac7c6629090488066e5eb02e7a729188b84a87050"
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, target) in [
        (0xe, 0x55fbc0),
        (0x89, 0xbae820),
        (0xba, 0xd15a30),
        (0xd0, 0xdb5600),
        (0xda, 0xd762d0),
        (0xe8, 0xd6ffd0),
        (0xf7, 0xdb0ee0),
        (0x10a, 0xd5e8e0),
        (0x119, 0xd5e120),
        (0x13b, 0xd1bed0),
        (0x162, 0xd75ca0),
    ] {
        relocate_call(
            &mut out,
            off,
            0xe7eb60,
            target,
            at,
            if off == 0x162 { queue } else { target },
        )?;
    }
    out[0x1b..0xb1].fill(0x90);
    // EBX is the native zero register used by the preserved tail. Jump rather
    // than execute the dead spawn argument setup, so its stack is untouched.
    out[0x1b..0x22].copy_from_slice(&[0x33, 0xdb, 0xe9, 0x8f, 0, 0, 0]);
    for off in [0xd0, 0xf7] {
        // Both original thiscall functions consumed two pushed arguments.
        out[off..off + 5].copy_from_slice(&[0x83, 0xc4, 8, 0x90, 0x90]);
    }
    // A depleted gadget can make the native inventory select its only remaining
    // item: this same drone. That is NOT a second deployment. Do not replace
    // the remembered pre-drone gadget with 0x28 when resuming the existing body.
    if out[0xca..0xd0] != [0x88, 0x86, 0xb4, 0, 0, 0] {
        return None;
    }
    out.resize(0x180, 0x90);
    out[0xca..0xd0].copy_from_slice(&[0xe9, 0xb1, 0, 0, 0, 0x90]);
    out.extend_from_slice(&[
        0x9c, 0x3c, 0x28, 0x74, 6, 0x88, 0x86, 0xb4, 0, 0, 0, 0x9d, 0xe9,
    ]);
    out.extend_from_slice(&(0xd0u32.wrapping_sub(0x191)).to_le_bytes());
    Some(out)
}

/// Observe the result at a *scoped* state dispatch call. Do not treat D75CA0's
/// incidental EAX as a boolean, and never detour the global state dispatcher.
pub(super) fn queue_window(bytes: &[u8], at: usize, dispatch: usize) -> Option<Vec<u8>> {
    if bytes.len() != 96
        || at == 0
        || dispatch == 0
        || at.checked_add(bytes.len())? > u32::MAX as usize
        || dispatch > u32::MAX as usize
        || format!("{:x}", Sha256::digest(bytes))
            != "734c98c8bc4a20537cca44494c96e8b7ef65cbc3e08075ba17cd0843f568ec5f"
    {
        return None;
    }
    let mut out = bytes.to_vec();
    for (off, target) in [(0xc, 0xe71fc0), (0x2e, 0xef3020), (0x54, 0xd5e070)] {
        relocate_call(
            &mut out,
            off,
            0xd75ca0,
            target,
            at,
            if off == 0x54 { dispatch } else { target },
        )?;
    }
    Some(out)
}

/// EF31B0 only queues a state when its layer is enabled and priority allows it.
/// D5E070 can also reject it because another layer is busy. Both return void.
/// Read the actual layer's pending/active identity, not a guessed return value.
pub(super) unsafe fn queued_state(owner: usize, state: usize, layer: u32) -> bool {
    layer < 3
        && state != 0
        && word(owner, 0x16e0 + layer as usize * 4).is_some_and(|p| {
            word(p as usize, 0x2c) == Some(state as u32)
                || word(p as usize, 0x28) == Some(state as u32)
        })
}
