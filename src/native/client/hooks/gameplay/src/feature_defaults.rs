//! Canonical stable gameplay policy, compiled into scbl_gameplay.dll.
//! The optional development child reuses this source; it is not a release dependency.
use super::feature_contract::*;
use super::rule_contract::{AtsInput, NameplateInput};
use super::rule_defaults;

pub(crate) const STAMINA_OPACITY: f32 = 0.58;
pub(crate) const STAMINA_HEIGHT: f32 = 0.004;

pub unsafe extern "system" fn prepare(
    old_revision: u32,
    old: *const FeatureState,
    output: *mut FeatureState,
) -> i32 {
    let (Some(old), Some(output)) = (old.as_ref(), output.as_mut()) else {
        return -1;
    };
    // Never reset an incompatible live state silently.
    if old_revision != 0 && !(old.valid_for(1) || old.valid_for(2)) {
        return -1;
    }
    let mut next = if old_revision == 0 {
        FeatureState::default()
    } else {
        *old
    };
    next.schema = 2;
    next.used = 4;
    *output = next;
    0
}

/// Canonical policies without transferable gameplay state. Stamina deliberately
/// stays in dispatch(): its visibility/fade state must survive a development reload.
pub(crate) fn stateless(input: &FeaturePacket) -> Option<FeaturePacket> {
    if input.size != std::mem::size_of::<FeaturePacket>() as u32 {
        return None;
    }
    let mut result = FeaturePacket::new(input.event);
    result.words[0] = match input.event {
        EVENT_NAMEPLATE => rule_defaults::nameplate(NameplateInput {
            viewer_role: input.words[0],
            target_role: input.words[1],
            bms_equipped: input.words[2],
            intel_marked: input.words[3],
        }),
        EVENT_ATS_RANGE => rule_defaults::ats_range(AtsInput {
            native_range: f32::from_bits(input.words[0]),
            silent_shoes: input.words[1],
        })
        .to_bits(),
        EVENT_MELEE_RANGE => super::equipment_rules::melee_range(
            f32::from_bits(input.words[0]),
            f32::from_bits(input.words[1]),
            input.words[2] == 1,
        )
        .to_bits(),
        EVENT_MELEE_SPEED => super::combat_rules::melee_speed(input.words[0] == 1).to_bits(),
        EVENT_HEAD_EMP => super::head_emp_rules::protected_phase(
            f32::from_bits(input.words[0]),
            f32::from_bits(input.words[1]),
            f32::from_bits(input.words[2]),
            f32::from_bits(input.words[3]),
        )
        .to_bits(),
        EVENT_DRONE_EMP => super::combat_rules::drone_emp_damage(input.words[0], input.words[1]),
        EVENT_RFD => u32::from(super::equipment_rules::rfd_allowed(
            input.words[0] == 1,
            input.words[1] == 1,
            input.words[2],
            input.words[3],
        )),
        _ => return None,
    };
    Some(result)
}

pub unsafe extern "system" fn dispatch(
    input: *const FeaturePacket,
    state: *mut FeatureState,
    output: *mut FeaturePacket,
) -> i32 {
    let (Some(input), Some(state), Some(output)) =
        (input.as_ref(), state.as_mut(), output.as_mut())
    else {
        return -1;
    };
    if input.size != std::mem::size_of::<FeaturePacket>() as u32 || !state.valid_for(2) {
        return -1;
    }
    let mut result = FeaturePacket::new(input.event);
    result.words[0] = match input.event {
        EVENT_STAMINA => {
            if state.words[1] != input.words[1] as u64 {
                state.words[1] = input.words[1] as u64;
                state.words[2] = 0;
                state.words[3] = 0;
            }
            let mut visible = state.words[2] != 0;
            let mut fade = f32::from_bits(state.words[3] as u32);
            let (ratio, alpha) = super::equipment_rules::stamina(
                input.words[0] == 1,
                f32::from_bits(input.words[2]),
                f32::from_bits(input.words[3]),
                f32::from_bits(input.words[4]),
                &mut visible,
                &mut fade,
            );
            state.words[2] = u64::from(visible);
            state.words[3] = fade.to_bits() as u64;
            result.words[1] = (alpha * STAMINA_OPACITY).to_bits();
            result.words[2] = 0.5f32.to_bits();
            result.words[3] = 0.775f32.to_bits();
            result.words[4] = 0.145f32.to_bits();
            result.words[5] = STAMINA_HEIGHT.to_bits();
            ratio.to_bits()
        }
        _ => {
            let Some(value) = stateless(input) else { return STATUS_UNSUPPORTED };
            value.words[0]
        }
    };
    // A transferable implementation-owned counter, not gameplay/input state.
    state.words[0] = state.words[0].wrapping_add(1);
    *output = result;
    0
}

pub fn api(revision: u32) -> FeatureApi {
    FeatureApi {
        abi: FEATURE_ABI,
        size: std::mem::size_of::<FeatureApi>() as u32,
        revision,
        events: SUPPORTED_EVENTS,
        state_schema: 2,
        prepare: Some(prepare),
        dispatch: Some(dispatch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_migration_preserves_data_and_rejects_unknown_schema() {
        let mut state = FeatureState::default();
        assert_eq!(
            unsafe { prepare(0, &FeatureState::default(), &mut state) },
            0
        );
        state.words[0] = 99;
        let mut next = FeatureState::default();
        assert_eq!(unsafe { prepare(25, &state, &mut next) }, 0);
        assert_eq!(next, state);
        state.schema = 9;
        let before = next;
        assert_eq!(unsafe { prepare(25, &state, &mut next) }, -1);
        assert_eq!(next, before);
    }
    #[test]
    fn unknown_event_has_no_output_or_state_side_effect() {
        let mut state = FeatureState::default();
        unsafe { prepare(0, &FeatureState::default(), &mut state) };
        let before = state;
        let mut output = FeaturePacket::new(999);
        let old = output;
        assert_eq!(
            unsafe { dispatch(&old, &mut state, &mut output) },
            STATUS_UNSUPPORTED
        );
        assert_eq!(state, before);
        assert_eq!(output, old);
    }
    #[test]
    fn previous_schema_migrates_without_resetting_counter() {
        let mut old = FeatureState::default();
        old.schema = 1;
        old.used = 1;
        old.words[0] = 77;
        let mut next = FeatureState::default();
        assert_eq!(unsafe { prepare(25, &old, &mut next) }, 0);
        assert_eq!(next.schema, 2);
        assert_eq!(next.words[..4], [77, 0, 0, 0]);
    }
    #[test]
    fn stamina_style_is_translucent_and_does_not_change_visibility_threshold() {
        let mut state = FeatureState::default();
        assert_eq!(
            unsafe { prepare(0, &FeatureState::default(), &mut state) },
            0
        );
        let mut p = FeaturePacket::new(EVENT_STAMINA);
        let mut out = p;
        p.words[..5].copy_from_slice(&[
            1,
            1,
            6.0f32.to_bits(),
            10.0f32.to_bits(),
            0.016f32.to_bits(),
        ]);
        assert_eq!(unsafe { dispatch(&p, &mut state, &mut out) }, 0);
        assert_eq!(f32::from_bits(out.words[1]), 0.0);
        p.words[2] = 4.0f32.to_bits();
        assert_eq!(unsafe { dispatch(&p, &mut state, &mut out) }, 0);
        assert_eq!(f32::from_bits(out.words[0]), 0.4);
        assert_eq!(f32::from_bits(out.words[1]), STAMINA_OPACITY);
        assert_eq!(f32::from_bits(out.words[5]), STAMINA_HEIGHT);
        p.words[2] = 8.0f32.to_bits();
        assert_eq!(unsafe { dispatch(&p, &mut state, &mut out) }, 0);
        assert_eq!(f32::from_bits(out.words[1]), STAMINA_OPACITY);
    }
}

#[cfg(test)]
mod stateless_tests {
    use super::*;
    #[test]
    fn all_stateless_outputs_match_the_transactional_dispatch() {
        let mut inputs = Vec::new();
        for viewer in 0..4 { for target in 0..4 { for bms in 0..3 { for marked in 0..3 {
            let mut p=FeaturePacket::new(EVENT_NAMEPLATE);
            p.words[..4].copy_from_slice(&[viewer,target,bms,marked]);inputs.push(p);
        }}}}
        for native in [0.0f32,1.0,20.0,1000.0,-1.0,f32::NAN,f32::INFINITY] {
            for shoes in 0..3 { let mut p=FeaturePacket::new(EVENT_ATS_RANGE);p.words[..2].copy_from_slice(&[native.to_bits(),shoes]);inputs.push(p); }
        }
        for native in [120.0f32,180.0,189.0,225.0,f32::NAN] { for glove in 0..3 {
            let mut p=FeaturePacket::new(EVENT_MELEE_RANGE);p.words[..3].copy_from_slice(&[native.to_bits(),1.05f32.to_bits(),glove]);inputs.push(p);
        }}
        for glove in 0..3 { let mut p=FeaturePacket::new(EVENT_MELEE_SPEED);p.words[0]=glove;inputs.push(p); }
        for max in [0,1,2,99,100,u32::MAX] { for hp in [0,1,49,50,51,99,100,u32::MAX] {
            let mut p=FeaturePacket::new(EVENT_DRONE_EMP);p.words[..2].copy_from_slice(&[hp,max]);inputs.push(p);
        }}
        for status in [0.0f32,0.3,1.0,6.0,f32::NAN] {
            let mut p=FeaturePacket::new(EVENT_HEAD_EMP);p.words[..4].copy_from_slice(&[status.to_bits(),6f32.to_bits(),1f32.to_bits(),3f32.to_bits()]);inputs.push(p);
        }
        for native in 0..3 { for pants in 0..3 { for kind in 0..5 {
            let mut p=FeaturePacket::new(EVENT_RFD);p.words[..4].copy_from_slice(&[native,pants,kind,8]);inputs.push(p);
        }}}
        for input in inputs {
            let direct=stateless(&input).unwrap();
            let mut state=FeatureState::default();unsafe { assert_eq!(prepare(0,&FeatureState::default(),&mut state),0); }
            state.words[0]=77;state.words[1]=123;state.words[2]=1;state.words[3]=0.25f32.to_bits() as u64;
            let old=state;let mut output=FeaturePacket::new(input.event);
            assert_eq!(unsafe {dispatch(&input,&mut state,&mut output)},0);
            assert_eq!(direct,output);assert_eq!(state.words[0],78);assert_eq!(state.words[1..],old.words[1..]);
        }
    }
    #[test]
    fn stateless_path_rejects_unknown_stamina_and_malformed_headers() {
        for event in [0,EVENT_STAMINA,3,999] { assert!(stateless(&FeaturePacket::new(event)).is_none()); }
        let mut input=FeaturePacket::new(EVENT_DRONE_EMP); input.size=0;assert!(stateless(&input).is_none());
    }
}
