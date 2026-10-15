//! Transactional host side of the feature ABI. Host buffers are copied before
//! calling a child; failed migrations/dispatches cannot commit partial state.
//! The caller holds the publication read lease throughout invoke(). Switching
//! needs its exclusive write lease; the worker uses try_write and defers if busy.
use super::feature_contract::*;
use std::sync::Mutex;

pub(crate) struct Runtime {
    pub api: FeatureApi,
    state: Mutex<FeatureState>,
}
pub(crate) fn valid_api(api: &FeatureApi) -> bool {
    api.abi == FEATURE_ABI
        && api.size == std::mem::size_of::<FeatureApi>() as u32
        && api.revision != 0
        && api.events & 3 == 3
        && api.events & !SUPPORTED_EVENTS == 0
        && api.state_schema != 0
        && api.prepare.is_some()
        && api.dispatch.is_some()
}

pub(crate) fn valid_output(input: &FeaturePacket, output: &FeaturePacket) -> bool {
    if output.size != std::mem::size_of::<FeaturePacket>() as u32
        || output.event != input.event
        || (input.event != EVENT_STAMINA && output.words[1..].iter().any(|v| *v != 0))
    {
        return false;
    }
    match input.event {
        EVENT_NAMEPLATE => output.words[0] <= 1 && (input.words[0] != 0 || output.words[0] == 0),
        EVENT_ATS_RANGE => {
            let native = f32::from_bits(input.words[0]);
            let result = f32::from_bits(output.words[0]);
            result.is_finite()
                && result >= 0.0
                && result <= native
                && (input.words[1] == 1 || result == native)
        }
        EVENT_MELEE_RANGE => {
            let n = f32::from_bits(input.words[0]);
            let v = f32::from_bits(output.words[0]);
            v.is_finite() && v >= n && v <= n * 2.0 && (input.words[2] == 1 || v == n)
        }
        EVENT_MELEE_SPEED => {
            let v = f32::from_bits(output.words[0]);
            v.is_finite() && (1.0..=1.5).contains(&v) && (input.words[0] == 1 || v == 1.0)
        }
        EVENT_HEAD_EMP => {
            let v = f32::from_bits(output.words[0]);
            let status = f32::from_bits(input.words[0]);
            let visual = f32::from_bits(input.words[1]);
            let first = f32::from_bits(input.words[2]);
            let threshold = f32::from_bits(input.words[3]);
            v == 0.0
                || (v.is_finite()
                    && v > 0.0
                    && v <= status
                    && v <= visual
                    && v <= first
                    && first <= threshold
                    && (0.05..=3.0).contains(&first))
        }
        EVENT_DRONE_EMP => {
            let (hp, max, damage) = (input.words[0], input.words[1], output.words[0]);
            damage == 0
                || (max >= 2
                    && max <= i32::MAX as u32
                    && hp <= max
                    && hp > max / 2
                    && damage == max.div_ceil(2)
                    && damage <= hp)
        }
        EVENT_RFD => {
            output.words[0] <= 1
                && (input.words[0] != 0 || output.words[0] == 0)
                && (input.words[1] == 1 || output.words[0] == input.words[0])
        }
        EVENT_STAMINA => {
            output.words[..6].iter().all(|v| {
                let f = f32::from_bits(*v);
                f.is_finite() && (0.0..=1.0).contains(&f)
            }) && output.words[6..].iter().all(|v| *v == 0)
                && f32::from_bits(output.words[4]) <= 0.5
                && f32::from_bits(output.words[5]) <= 0.05
        }
        _ => false,
    }
}
impl Runtime {
    pub(crate) fn new(api: FeatureApi) -> Result<Self, &'static str> {
        if !valid_api(&api) {
            return Err("features-abi");
        }
        let runtime = Self {
            api,
            state: Mutex::new(FeatureState::default()),
        };
        runtime.migrate_from(None)?;
        Ok(runtime)
    }
    // Invoked under exclusive publication lease. Both old and new modules remain
    // mapped, but no game callback can overlap this state handoff.
    pub(crate) fn migrate_from(&self, old: Option<&Self>) -> Result<(), &'static str> {
        let (revision, previous) = if let Some(old) = old {
            (
                old.api.revision,
                *old.state.lock().map_err(|_| "features-old-state-lock")?,
            )
        } else {
            (0, FeatureState::default())
        };
        let mut prepared = FeatureState::default();
        if unsafe { self.api.prepare.unwrap()(revision, &previous, &mut prepared) } != 0
            || !prepared.valid_for(self.api.state_schema)
        {
            return Err("features-state-migration");
        }
        self.probe_state(prepared)?;
        *self.state.lock().map_err(|_| "features-new-state-lock")? = prepared;
        Ok(())
    }
    pub(crate) fn invoke(&self, input: FeaturePacket) -> Option<FeaturePacket> {
        // Do not stall a native callback on another native thread. Fall back to
        // caller-owned fallback when busy; shipping stateless rules bypass this lease.
        let mut committed = self.state.try_lock().ok()?;
        let mut scratch = *committed;
        let mut output = FeaturePacket::new(input.event);
        if unsafe { self.api.dispatch.unwrap()(&input, &mut scratch, &mut output) } != 0
            || !scratch.valid_for(self.api.state_schema)
            || !valid_output(&input, &output)
        {
            return None;
        }
        *committed = scratch;
        Some(output)
    }
    fn probe_state(&self, initial: FeatureState) -> Result<(), &'static str> {
        // Isolated probe buffers: validation does not modify the committed state.
        let mut inputs = Vec::new();
        for roles in [(0, 0, 0, 0), (2, 1, 1, 0), (1, 2, 0, 1), (2, 1, 0, 0)] {
            let mut p = FeaturePacket::new(EVENT_NAMEPLATE);
            p.words[..4].copy_from_slice(&[roles.0, roles.1, roles.2, roles.3]);
            inputs.push(p);
        }
        for native in [0.0f32, 20.0, 1000.0] {
            for shoes in [0, 1] {
                let mut p = FeaturePacket::new(EVENT_ATS_RANGE);
                p.words[..2].copy_from_slice(&[native.to_bits(), shoes]);
                inputs.push(p);
            }
        }
        for (event, words) in [
            (
                EVENT_MELEE_RANGE,
                vec![225f32.to_bits(), 1.25f32.to_bits(), 1],
            ),
            (EVENT_MELEE_SPEED, vec![1]),
            (EVENT_MELEE_SPEED, vec![0]),
            (
                EVENT_HEAD_EMP,
                vec![
                    6f32.to_bits(),
                    6f32.to_bits(),
                    1f32.to_bits(),
                    3f32.to_bits(),
                ],
            ),
            (EVENT_DRONE_EMP, vec![100, 100]),
            (EVENT_DRONE_EMP, vec![50, 100]),
            (EVENT_DRONE_EMP, vec![99, 99]),
            (EVENT_DRONE_EMP, vec![0, 0]),
            (EVENT_RFD, vec![1, 1, 1, 8]),
            (
                EVENT_STAMINA,
                vec![1, 7, 40f32.to_bits(), 100f32.to_bits(), 0.016f32.to_bits()],
            ),
        ] {
            if self.api.events & event != 0 {
                let mut p = FeaturePacket::new(event);
                p.words[..words.len()].copy_from_slice(&words);
                inputs.push(p);
            }
        }
        for p in inputs {
            let mut state = initial;
            let mut out = FeaturePacket::new(p.event);
            if unsafe { self.api.dispatch.unwrap()(&p, &mut state, &mut out) } != 0
                || !state.valid_for(self.api.state_schema)
                || !valid_output(&p, &out)
            {
                return Err("features-output-contract");
            }
        }
        let p = FeaturePacket::new(u32::MAX);
        let mut state = initial;
        let mut out = p;
        if unsafe { self.api.dispatch.unwrap()(&p, &mut state, &mut out) } != STATUS_UNSUPPORTED
            || state != initial
            || out != p
        {
            return Err("features-unknown-event-contract");
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn state(&self) -> FeatureState {
        *self.state.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_defaults;

    #[test]
    fn new_combat_policies_are_optional_bounded_and_transactional() {
        let r = Runtime::new(feature_defaults::api(30)).unwrap();
        let mut p = FeaturePacket::new(EVENT_MELEE_SPEED);
        p.words[0] = 1;
        assert_eq!(f32::from_bits(r.invoke(p).unwrap().words[0]), 1.20);
        p.words[0] = 0;
        assert_eq!(f32::from_bits(r.invoke(p).unwrap().words[0]), 1.0);
        let mut out = p;
        out.words[0] = 1.5f32.to_bits();
        assert!(!valid_output(&p, &out));
        p = FeaturePacket::new(EVENT_DRONE_EMP);
        p.words[..2].copy_from_slice(&[100, 100]);
        assert_eq!(r.invoke(p).unwrap().words[0], 50);
        out = p;
        out.words[0] = 51;
        out.words[1] = 0;
        assert!(!valid_output(&p, &out));
        p.words[0] = 50;
        assert_eq!(r.invoke(p).unwrap().words[0], 0);
        out.words[0] = 50;
        assert!(!valid_output(&p, &out));
        let mut old = feature_defaults::api(28);
        old.events = 31;
        assert!(Runtime::new(old).is_ok()); // old generation remains valid
    }
    #[test]
    fn migration_keeps_last_committed_state_not_old_generation_snapshot() {
        let a = Runtime::new(feature_defaults::api(25)).unwrap();
        a.invoke(FeaturePacket::new(EVENT_NAMEPLATE)).unwrap();
        let b = Runtime::new(feature_defaults::api(26)).unwrap();
        b.migrate_from(Some(&a)).unwrap();
        assert_eq!(b.state().words[0], 1);
        b.invoke(FeaturePacket::new(EVENT_NAMEPLATE)).unwrap();
        a.migrate_from(Some(&b)).unwrap();
        assert_eq!(a.state().words[0], 2);
    }
    unsafe extern "system" fn poison(
        _: *const FeaturePacket,
        s: *mut FeatureState,
        o: *mut FeaturePacket,
    ) -> i32 {
        (*s).words[0] = 9999;
        (*o).words[0] = 99;
        0
    }
    #[test]
    fn failed_dispatch_never_commits_scratch_state() {
        let mut r = Runtime::new(feature_defaults::api(25)).unwrap();
        let before = r.state();
        r.api.dispatch = Some(poison);
        assert!(r.invoke(FeaturePacket::new(EVENT_NAMEPLATE)).is_none());
        assert_eq!(r.state(), before);
    }
    unsafe extern "system" fn refuse(_: u32, _: *const FeatureState, s: *mut FeatureState) -> i32 {
        (*s).schema = 99;
        -1
    }
    #[test]
    fn failed_prepare_keeps_both_existing_states() {
        let a = Runtime::new(feature_defaults::api(25)).unwrap();
        let mut b = Runtime::new(feature_defaults::api(26)).unwrap();
        let before = (a.state(), b.state());
        b.api.prepare = Some(refuse);
        assert!(b.migrate_from(Some(&a)).is_err());
        assert_eq!((a.state(), b.state()), before);
    }
    #[test]
    fn unknown_schema_event_or_header_is_rejected() {
        let mut a = feature_defaults::api(25);
        a.events = 0x8000;
        assert!(Runtime::new(a).is_err());
        a = feature_defaults::api(25);
        a.abi += 1;
        assert!(Runtime::new(a).is_err());
        let r = Runtime::new(feature_defaults::api(25)).unwrap();
        let old = r.state();
        assert!(r.invoke(FeaturePacket::new(999)).is_none());
        let mut p = FeaturePacket::new(EVENT_NAMEPLATE);
        p.size = 0;
        assert!(r.invoke(p).is_none());
        assert_eq!(r.state(), old);
    }
    #[test]
    fn stateful_dispatch_preserves_existing_name_and_ats_rules() {
        let r = Runtime::new(feature_defaults::api(25)).unwrap();
        for viewer in 0..=3 {
            for target in 0..=3 {
                for bms in 0..=1 {
                    for intel in 0..=1 {
                        let mut p = FeaturePacket::new(EVENT_NAMEPLATE);
                        p.words[..4].copy_from_slice(&[viewer, target, bms, intel]);
                        let expected =
                            crate::rule_defaults::nameplate(crate::rule_contract::NameplateInput {
                                viewer_role: viewer,
                                target_role: target,
                                bms_equipped: bms,
                                intel_marked: intel,
                            });
                        assert_eq!(r.invoke(p).unwrap().words[0], expected);
                    }
                }
            }
        }
        for native in [0.0f32, 1.0, 1500.0, 6000.0] {
            for shoes in [0, 1] {
                let mut p = FeaturePacket::new(EVENT_ATS_RANGE);
                p.words[..2].copy_from_slice(&[native.to_bits(), shoes]);
                let expected = crate::rule_defaults::ats_range(crate::rule_contract::AtsInput {
                    native_range: native,
                    silent_shoes: shoes,
                });
                assert_eq!(f32::from_bits(r.invoke(p).unwrap().words[0]), expected);
            }
        }
    }
    #[test]
    fn stamina_hot_handoff_preserves_latched_recovery_and_resets_new_pawn() {
        let a = Runtime::new(feature_defaults::api(26)).unwrap();
        let mut p = FeaturePacket::new(EVENT_STAMINA);
        p.words[..5].copy_from_slice(&[
            1,
            1,
            4.0f32.to_bits(),
            10.0f32.to_bits(),
            0.016f32.to_bits(),
        ]);
        assert_eq!(
            f32::from_bits(a.invoke(p).unwrap().words[1]),
            feature_defaults::STAMINA_OPACITY
        );
        let b = Runtime::new(feature_defaults::api(27)).unwrap();
        b.migrate_from(Some(&a)).unwrap();
        p.words[2] = 8.0f32.to_bits();
        assert_eq!(
            f32::from_bits(b.invoke(p).unwrap().words[1]),
            feature_defaults::STAMINA_OPACITY
        );
        a.migrate_from(Some(&b)).unwrap();
        assert_eq!(
            f32::from_bits(a.invoke(p).unwrap().words[1]),
            feature_defaults::STAMINA_OPACITY
        );
        p.words[1] = 2;
        assert_eq!(f32::from_bits(a.invoke(p).unwrap().words[1]), 0.0);
    }
}

#[cfg(test)]
impl Runtime {
    pub(crate) fn with_state_lease<T>(&self, f: impl FnOnce() -> T) -> T {
        let _lease = self.state.lock().unwrap();
        f()
    }
}
