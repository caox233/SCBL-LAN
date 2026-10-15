//! Owns voluntary-exit generations across native callbacks.
//! Native byte adapters and one-use tickets remain in drone_control.
//! No mutex guard may cross a native callback; clear never resets generation.
use super::{
    exit_and_capture, float, queued_state, with_current_reentry_position, word, Layout,
    ParkedDrone, Reentry, VoluntaryExit,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Mutex;

enum Phase {
    Exit(VoluntaryExit),
    Parked(ParkedDrone),
    Enter(ParkedDrone),
    Busy,
}
struct Pending {
    generation: u64,
    phase: Phase,
    queued: Option<(usize, u32)>,
}
#[derive(Default)]
struct FlowState {
    generation: u64,
    abilities: BTreeMap<usize, Pending>,
}
/// Holds identities between native animation callbacks, not between key reads.
/// Never hold this lock while calling game code: a callback may synchronously
/// enter/exit/cancel. A generation prevents cancellation from resurrecting state.
#[derive(Default)]
pub(crate) struct ActionFlow {
    state: Mutex<FlowState>,
}
#[derive(Debug, PartialEq)]
pub(crate) enum Press {
    Native,
    Wait(f32),
    Queued,
    Restricted,
}
impl ActionFlow {
    /// Only a committed voluntary-exit ticket may survive ordinary owner damage.
    /// Native pawn+0x60 bit 0x800 is the exact B8B300 dead predicate.
    /// No lock is held over the caller's native virtual dispatch.
    pub(super) unsafe fn keep_parked_on_owner_hit(&self, ability: usize) -> bool {
        let Ok(guard) = self.state.try_lock() else {
            return false;
        };
        let Some(pending) = guard.abilities.get(&ability) else {
            return false;
        };
        let Phase::Parked(ticket) = &pending.phase else {
            return false;
        };
        ticket.live() && word(ticket.body, 0xb68) == Some(0)
    }
    // Read-only view of the same native game-time wait used by input. Never
    // invent a countdown for automatic/forced exit or a replaced drone.
    pub(super) unsafe fn hud_wait(&self, ability: usize) -> Option<u8> {
        let guard = self.state.try_lock().ok()?;
        let Phase::Parked(ticket) = &guard.abilities.get(&ability)?.phase else {
            return None;
        };
        if !ticket.live() {
            return None;
        }
        let now = float(ability, 0x80).filter(|v| v.is_finite() && *v >= ticket.exit_elapsed)?;
        Some((3.0 - (now - ticket.exit_elapsed)).clamp(0.0, 3.0).ceil() as u8)
    }
    pub(super) unsafe fn parked_weapon_restore(&self, ability: usize) -> Option<(usize, u32)> {
        let guard = self.state.lock().ok()?;
        let Phase::Parked(ticket) = &guard.abilities.get(&ability)?.phase else {
            return None;
        };
        if !ticket.live() {
            return None;
        }
        let inventory = word(ticket.owner, 0x1410)? as usize;
        let previous = word(ability, 0xb4)? & 255;
        (previous != 0x28 && word(inventory, 0x30).map(|v| v & 255) == Some(0x28))
            .then_some((ticket.owner, previous))
    }
    pub(super) fn cancel(&self, ability: usize) {
        if let Ok(mut s) = self.state.lock() {
            s.abilities.remove(&ability);
        }
    }
    pub(super) fn clear(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.abilities.clear();
        }
    }
    fn reserve(&self, ability: usize, phase: Phase) -> Option<u64> {
        let mut s = self.state.lock().ok()?;
        if s.abilities.contains_key(&ability) {
            return None;
        }
        s.generation = s.generation.checked_add(1)?;
        let generation = s.generation;
        s.abilities.insert(
            ability,
            Pending {
                generation,
                phase,
                queued: None,
            },
        );
        Some(generation)
    }
    fn finish(&self, ability: usize, generation: u64, phase: Option<Phase>) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        if s.abilities
            .get(&ability)
            .is_none_or(|p| p.generation != generation)
        {
            return;
        }
        if let Some(phase) = phase {
            let pending = s.abilities.get_mut(&ability).unwrap();
            pending.phase = phase;
            pending.queued = None;
        } else {
            s.abilities.remove(&ability);
        }
    }
    fn take(&self, ability: usize, accept: impl FnOnce(&Phase) -> bool) -> Option<(u64, Phase)> {
        let mut s = self.state.lock().ok()?;
        let p = s.abilities.get_mut(&ability)?;
        if !accept(&p.phase) {
            return None;
        }
        Some((p.generation, std::mem::replace(&mut p.phase, Phase::Busy)))
    }
    /// Must be invoked at action_window's base-call site, AFTER the native
    /// startup delay. The bound action names come from the current game's names.
    pub(super) unsafe fn action(
        &self,
        state: usize,
        ability: usize,
        action: u32,
        detonate: u32,
        on_station: u32,
        layout: Layout,
        blocked: impl FnOnce(usize) -> bool,
        native: impl FnOnce(u32) -> u32,
    ) -> u32 {
        if on_station == 0 || detonate == 0 || on_station == detonate || action != detonate {
            return native(action);
        }
        // A second press during the native exit animation must not explode the
        // now-parked body, overwrite its request, or restart the wait interval.
        if self.state.lock().ok().is_some_and(|s| {
            s.abilities.get(&ability).is_some_and(|p| match &p.phase {
                Phase::Exit(r) => r.state == state && (r.valid(2) || r.valid(1)),
                _ => false,
            })
        }) {
            return 1;
        }
        let Some(request) =
            VoluntaryExit::request(state, ability, action, detonate, layout, blocked)
        else {
            return native(action);
        };
        let Some(generation) = self.reserve(ability, Phase::Exit(request)) else {
            return native(action);
        };
        let result = native(on_station);
        // Base-action return value is not a handled flag. Confirm the parked
        // body instead. Synchronous exit may already have committed Parked.
        if let Ok(mut s) = self.state.lock() {
            let rejected = s.abilities.get(&ability).is_some_and(|p| {
                p.generation == generation && matches!(&p.phase,Phase::Exit(r) if !r.valid(1))
            });
            if rejected {
                s.abilities.remove(&ability);
            }
        }
        result
    }
    /// Native TransOut(1) cancels this state's animation wait and calls the
    /// ordinary virtual Exit via 14F72E0 -> 14640B0 -> v98. It does not replace
    /// cleanup or emit a second HUD event. Only an accepted living parked body
    /// can opt in; death, depletion, foreign states and forced exits stay native.
    pub(super) unsafe fn transout_mode(&self, state: usize, ability: usize, native: u32) -> u32 {
        if native != 0 {
            return native;
        }
        let eligible = self.state.try_lock().ok().is_some_and(|s| {
            s.abilities.get(&ability).is_some_and(|p| match &p.phase {
                Phase::Exit(r) => r.state == state && r.valid(1),
                _ => false,
            })
        });
        u32::from(eligible)
    }
    pub(super) unsafe fn exit<R>(
        &self,
        state: usize,
        ability: usize,
        blocked: impl Fn(usize) -> bool,
        native: impl FnOnce() -> R,
    ) -> R {
        let request = self.take(ability, |p| matches!(p,Phase::Exit(r) if r.state==state));
        let (generation, request) = match request {
            Some((g, Phase::Exit(r))) => (Some(g), Some(r)),
            _ => (None, None),
        };
        let (result, parked) = exit_and_capture(
            request,
            state,
            ability,
            |owner| blocked(owner) || generation.is_none_or(|g| !self.is_current(ability, g)),
            native,
        );
        if let Some(g) = generation {
            self.finish(ability, g, parked.map(Phase::Parked));
        }
        result
    }
    pub(super) unsafe fn press(
        &self,
        ability: usize,
        allowed: impl FnOnce(usize) -> bool,
        enter: impl FnOnce(usize, usize),
    ) -> Press {
        let Some((generation, Phase::Parked(ticket))) =
            self.take(ability, |p| matches!(p, Phase::Parked(_)))
        else {
            return if self
                .state
                .lock()
                .ok()
                .is_some_and(|s| s.abilities.contains_key(&ability))
            {
                Press::Queued
            } else {
                Press::Native
            };
        };
        let result = ticket.check(ability, allowed);
        match result {
            Reentry::Ready if ticket.check(ability, |_| true) == Reentry::Ready => {
                let (owner, body) = (ticket.owner, ticket.body);
                // Publish pending entry before the callback; it may enter now
                // or in a later frame. Do NOT keep PositionScope across frames.
                if !self.is_current(ability, generation) {
                    return Press::Restricted;
                }
                self.finish(ability, generation, Some(Phase::Enter(ticket)));
                enter(owner, body);
                Press::Queued
            }
            other => {
                let valid = ticket.live()
                    && float(ability, 0x80)
                        .is_some_and(|v| v.is_finite() && v >= ticket.exit_elapsed);
                self.finish(ability, generation, valid.then_some(Phase::Parked(ticket)));
                match other {
                    Reentry::Wait(t) => Press::Wait(t),
                    _ => Press::Restricted,
                }
            }
        }
    }
    fn is_current(&self, ability: usize, generation: u64) -> bool {
        self.state.lock().ok().is_some_and(|s| {
            s.abilities
                .get(&ability)
                .is_some_and(|p| p.generation == generation)
        })
    }
    /// Production entry uses a verified pending native state. Failed allocation,
    /// disabled layers and priority rejection restore Parked immediately. No
    /// guessed timeout: subsequent ticks compare the native queue identity.
    pub(super) unsafe fn press_confirmed(
        &self,
        ability: usize,
        allowed: impl FnOnce(usize) -> bool,
        enter: impl FnOnce(usize, usize) -> Option<(usize, u32)>,
    ) -> Press {
        self.refresh(ability);
        let rejected = Cell::new(false);
        let result = self.press(ability, allowed, |owner, body| {
            let generation = self
                .state
                .lock()
                .ok()
                .and_then(|s| s.abilities.get(&ability).map(|p| p.generation));
            let queued = enter(owner, body);
            let Ok(mut s) = self.state.lock() else {
                rejected.set(true);
                return;
            };
            let Some(p) = s.abilities.get_mut(&ability) else {
                return;
            }; // synchronous entry/cancel consumed it
            if Some(p.generation) != generation || !matches!(p.phase, Phase::Enter(_)) {
                return;
            }
            if let Some((state, layer)) =
                queued.filter(|&(state, layer)| queued_state(owner, state, layer))
            {
                p.queued = Some((state, layer));
            } else {
                let Phase::Enter(ticket) = std::mem::replace(&mut p.phase, Phase::Busy) else {
                    unreachable!()
                };
                if ticket.live() {
                    p.phase = Phase::Parked(ticket);
                } else {
                    s.abilities.remove(&ability);
                }
                rejected.set(true);
            }
        });
        if rejected.get() {
            Press::Restricted
        } else {
            result
        }
    }
    pub(super) unsafe fn refresh(&self, ability: usize) -> bool {
        let Ok(mut s) = self.state.lock() else {
            return false;
        };
        let Some(p) = s.abilities.get_mut(&ability) else {
            return false;
        };
        let valid = match &p.phase {
            Phase::Exit(r) => r.valid(1) || r.valid(2),
            Phase::Parked(t) | Phase::Enter(t) => t.live(),
            Phase::Busy => return false,
        };
        if !valid {
            s.abilities.remove(&ability);
            return false;
        }
        let lost = matches!(&p.phase,Phase::Enter(t) if p.queued.is_some_and(|(state,layer)|!queued_state(t.owner,state,layer)));
        if lost {
            let Phase::Enter(t) = std::mem::replace(&mut p.phase, Phase::Busy) else {
                unreachable!()
            };
            p.phase = Phase::Parked(t);
            p.queued = None;
        }
        lost
    }
    pub(super) unsafe fn entry<R>(
        &self,
        state: usize,
        ability: usize,
        native: impl FnOnce() -> R,
    ) -> R {
        let matches_queue = self.state.lock().ok().is_some_and(|s| {
            s.abilities
                .get(&ability)
                .is_some_and(|p| p.queued.is_none_or(|(expected, _)| expected == state))
        });
        let ticket=self.take(ability,|phase|matches_queue && matches!(phase,Phase::Enter(p) if word(state,0)==Some(p.layout.state) && word(state,0x48)==Some(p.owner as u32) && word(state,0x64)==Some(p.controller)));
        let (generation, ticket) = match ticket {
            Some((g, Phase::Enter(p))) => (Some(g), Some(p)),
            _ => (None, None),
        };
        let result = with_current_reentry_position(
            ticket.as_ref(),
            || generation.is_some_and(|g| self.is_current(ability, g)),
            native,
        );
        if let (Some(g), Some(p)) = (generation, ticket) {
            let retry = p.live();
            self.finish(ability, g, retry.then_some(Phase::Parked(p)));
        }
        result
    }
}

#[cfg(test)]
#[path = "drone_control_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "drone_flow_lifecycle_tests.rs"]
mod lifecycle_tests;
