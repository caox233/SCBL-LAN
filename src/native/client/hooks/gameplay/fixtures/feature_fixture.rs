//! Native x86 test DLL only; not shipped. Compile with fixture_b / fixture_bad_* cfg.
#![allow(dead_code)]
#[path = "../src/equipment_rules.rs"]
mod equipment_rules;
#[path = "../src/feature_contract.rs"]
mod feature_contract;
#[path = "../src/feature_defaults.rs"]
mod feature_defaults;
#[path = "../src/rule_contract.rs"]
mod rule_contract;
#[path = "../src/rule_defaults.rs"]
mod rule_defaults;
use feature_contract::*;
use rule_contract::*;
use std::sync::atomic::{AtomicBool, Ordering};
static STARTED: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn name(i: *const NameplateInput) -> u32 {
    i.as_ref().map_or(0, |i| rule_defaults::nameplate(*i))
}
unsafe extern "system" fn ats(i: *const AtsInput) -> f32 {
    i.as_ref().map_or(0.0, |i| rule_defaults::ats_range(*i))
}
#[no_mangle]
pub unsafe extern "system" fn scbl_mod_rules_get_api(
    abi: u32,
    out: *mut RuleApi,
    size: u32,
) -> i32 {
    if abi != RULE_ABI || out.is_null() || size != std::mem::size_of::<RuleApi>() as u32 {
        return -1;
    }
    out.write(RuleApi {
        abi,
        size,
        revision: 100,
        capabilities: 3,
        nameplate: Some(name),
        ats_range: Some(ats),
    });
    0
}
unsafe extern "system" fn prepare(
    revision: u32,
    old: *const FeatureState,
    out: *mut FeatureState,
) -> i32 {
    if cfg!(fixture_bad_prepare) && revision != 0 {
        (*out).schema = 777;
        (*out).words[0] = 9999;
        return -1;
    }
    feature_defaults::prepare(revision, old, out)
}
unsafe extern "system" fn dispatch(
    input: *const FeaturePacket,
    state: *mut FeatureState,
    out: *mut FeaturePacket,
) -> i32 {
    if (*input).words[15] == 1 {
        STARTED.store(true, Ordering::SeqCst);
        while !RELEASE.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
    }
    let code = feature_defaults::dispatch(input, state, out);
    if code == 0 {
        if cfg!(fixture_bad_output) {
            (*out).words[0] = f32::NAN.to_bits();
        }
        if cfg!(fixture_b) && (*input).event == EVENT_ATS_RANGE && (*input).words[1] == 1 {
            (*out).words[0] = (f32::from_bits((*input).words[0]) * 0.25).to_bits();
        }
    }
    code
}
#[no_mangle]
pub unsafe extern "system" fn scbl_mod_features_get_api(
    abi: u32,
    out: *mut FeatureApi,
    size: u32,
) -> i32 {
    if abi != FEATURE_ABI || out.is_null() || size != std::mem::size_of::<FeatureApi>() as u32 {
        return -1;
    }
    let mut api = feature_defaults::api(if cfg!(fixture_b) { 2502 } else { 2501 });
    api.prepare = Some(prepare);
    api.dispatch = Some(dispatch);
    if cfg!(fixture_bad_abi) {
        api.abi = 999;
    }
    out.write(api);
    0
}
#[no_mangle]
pub extern "system" fn fixture_started() -> u32 {
    STARTED.load(Ordering::SeqCst) as u32
}
#[no_mangle]
pub extern "system" fn fixture_release() {
    RELEASE.store(true, Ordering::SeqCst);
}
