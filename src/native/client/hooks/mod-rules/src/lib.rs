//! Hot implementation: host-owned POD state, no native hooks/threads/pointers.
#[path = "../../gameplay/src/combat_rules.rs"]
mod combat_rules;
#[path = "../../gameplay/src/equipment_rules.rs"]
mod equipment_rules;
#[path = "../../gameplay/src/feature_contract.rs"]
mod feature_contract;
#[path = "../../gameplay/src/feature_defaults.rs"]
mod feature_defaults;
#[path = "../../gameplay/src/head_emp_rules.rs"]
mod head_emp_rules;
#[path = "../../gameplay/src/rule_contract.rs"]
mod rule_contract;
#[path = "../../gameplay/src/rule_defaults.rs"]
mod rule_defaults;
use rule_contract::*;

unsafe extern "system" fn nameplate(input: *const NameplateInput) -> u32 {
    input
        .as_ref()
        .map_or(0, |input| rule_defaults::nameplate(*input))
}
unsafe extern "system" fn ats_range(input: *const AtsInput) -> f32 {
    input
        .as_ref()
        .map_or(0.0, |input| rule_defaults::ats_range(*input))
}

/// Returns a copy of the immutable C function table. Does not retain any input.
/// # Safety
/// `output` must refer to a writable RuleApi when size/ABI are valid.
#[no_mangle]
pub unsafe extern "system" fn scbl_mod_rules_get_api(
    abi: u32,
    output: *mut RuleApi,
    size: u32,
) -> i32 {
    if abi != RULE_ABI || size != std::mem::size_of::<RuleApi>() as u32 || output.is_null() {
        return -1;
    }
    output.write(RuleApi {
        abi: RULE_ABI,
        size,
        capabilities: CAP_NAMEPLATES | CAP_ATS_RANGE,
        revision: 22,
        nameplate: Some(nameplate),
        ats_range: Some(ats_range),
    });
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abi_mismatch_does_not_write_output() {
        let mut api = RuleApi::default();
        assert_eq!(
            unsafe { scbl_mod_rules_get_api(0, &mut api, std::mem::size_of::<RuleApi>() as u32) },
            -1
        );
        assert_eq!(api.abi, 0);
        assert_eq!(
            unsafe {
                scbl_mod_rules_get_api(
                    RULE_ABI,
                    std::ptr::null_mut(),
                    std::mem::size_of::<RuleApi>() as u32,
                )
            },
            -1
        );
        assert_eq!(unsafe { scbl_mod_rules_get_api(RULE_ABI, &mut api, 0) }, -1);
    }
    #[test]
    fn abi_table_and_null_input_are_bounded() {
        let mut api = RuleApi::default();
        assert_eq!(
            unsafe {
                scbl_mod_rules_get_api(RULE_ABI, &mut api, std::mem::size_of::<RuleApi>() as u32)
            },
            0
        );
        assert_eq!(api.revision, 22);
        assert_eq!(unsafe { api.nameplate.unwrap()(std::ptr::null()) }, 0);
        assert_eq!(unsafe { api.ats_range.unwrap()(std::ptr::null()) }, 0.0);
    }
}

// Retain the ABI1 exports unchanged: an old stable host can safely keep using
// the legacy RuleApi only. R32 feature mask needs its matching host; deploy the\n// host and child together while the game is closed. No native hooks here.
#[no_mangle]
pub unsafe extern "system" fn scbl_mod_features_get_api(
    abi: u32,
    out: *mut feature_contract::FeatureApi,
    size: u32,
) -> i32 {
    if abi != feature_contract::FEATURE_ABI
        || out.is_null()
        || size != std::mem::size_of::<feature_contract::FeatureApi>() as u32
    {
        return -1;
    }
    out.write(feature_defaults::api(32));
    0
}
