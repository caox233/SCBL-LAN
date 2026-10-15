//! Plain C ABI between the permanent gameplay host and hot-reloaded pure rules.
//! No game pointers, callbacks, hooks, owned allocations or retained input state.
pub const RULE_ABI: u32 = 1;
#[allow(dead_code)] // Host-side lookup; provider shares the same contract.
pub const RULE_EXPORT: &[u8] = b"scbl_mod_rules_get_api\0";
pub const CAP_NAMEPLATES: u32 = 1;
pub const CAP_ATS_RANGE: u32 = 2;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NameplateInput {
    pub viewer_role: u32,
    pub target_role: u32,
    pub bms_equipped: u32,
    // ABI1 field name retained; now the authoritative native active-mark flag.
    // Covers Intel Suit, drone, intel device and native removal/expiry.
    pub intel_marked: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AtsInput {
    pub native_range: f32,
    pub silent_shoes: u32,
}
pub type NameplateRule = unsafe extern "system" fn(*const NameplateInput) -> u32;
pub type AtsRule = unsafe extern "system" fn(*const AtsInput) -> f32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RuleApi {
    pub abi: u32,
    pub size: u32,
    pub capabilities: u32,
    pub revision: u32,
    pub nameplate: Option<NameplateRule>,
    pub ats_range: Option<AtsRule>,
}
#[allow(dead_code)] // Host-side loader signature.
pub type GetApi = unsafe extern "system" fn(u32, *mut RuleApi, u32) -> i32;
