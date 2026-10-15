//! Stateful feature implementation ABI. The permanent host owns state and native hooks.
//! All pointers are borrowed POD buffers, valid only during the call. No game
//! pointers, allocations, threads, callbacks or retained pointers cross this ABI.
//! prepare/dispatch must be bounded, non-reentrant and side-effect free outside
//! output buffers. Changing native adapters/ABI still requires a host upgrade.
pub const FEATURE_ABI: u32 = 1;
pub const FEATURE_EXPORT: &[u8] = b"scbl_mod_features_get_api\0";
pub const EVENT_NAMEPLATE: u32 = 1;
pub const EVENT_ATS_RANGE: u32 = 2;
pub const EVENT_MELEE_RANGE: u32 = 4;
pub const EVENT_RFD: u32 = 8;
pub const EVENT_STAMINA: u32 = 16;
pub const EVENT_MELEE_SPEED: u32 = 32;
pub const EVENT_DRONE_EMP: u32 = 64;
pub const EVENT_HEAD_EMP: u32 = 128;
pub const SUPPORTED_EVENTS: u32 = 255;
pub const STATUS_UNSUPPORTED: i32 = -2;
pub const STATE_WORDS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureState {
    pub schema: u32,
    pub used: u32,
    pub words: [u64; STATE_WORDS],
}
impl Default for FeatureState {
    fn default() -> Self {
        Self {
            schema: 0,
            used: 0,
            words: [0; STATE_WORDS],
        }
    }
}
impl FeatureState {
    pub fn valid_for(&self, schema: u32) -> bool {
        self.schema == schema
            && schema != 0
            && self.used as usize <= STATE_WORDS
            && self.words[self.used as usize..].iter().all(|v| *v == 0)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeaturePacket {
    pub size: u32,
    pub event: u32,
    pub words: [u32; 16],
}
impl FeaturePacket {
    pub fn new(event: u32) -> Self {
        Self {
            size: std::mem::size_of::<Self>() as u32,
            event,
            words: [0; 16],
        }
    }
}
pub type Prepare = unsafe extern "system" fn(u32, *const FeatureState, *mut FeatureState) -> i32;
pub type Dispatch =
    unsafe extern "system" fn(*const FeaturePacket, *mut FeatureState, *mut FeaturePacket) -> i32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FeatureApi {
    pub abi: u32,
    pub size: u32,
    pub revision: u32,
    pub events: u32,
    pub state_schema: u32,
    pub prepare: Option<Prepare>,
    pub dispatch: Option<Dispatch>,
}
pub type GetFeatureApi = unsafe extern "system" fn(u32, *mut FeatureApi, u32) -> i32;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_state_schema_and_unused_bytes_are_checked() {
        let mut s = FeatureState::default();
        assert!(!s.valid_for(1));
        s.schema = 1;
        assert!(s.valid_for(1));
        s.used = 65;
        assert!(!s.valid_for(1));
        s.used = 0;
        s.words[63] = 1;
        assert!(!s.valid_for(1));
        s.used = 64;
        assert!(s.valid_for(1));
        assert!(!s.valid_for(2));
    }
    #[test]
    fn pod_layout_is_fixed_for_x86() {
        assert_eq!(std::mem::size_of::<FeaturePacket>(), 72);
        assert_eq!(std::mem::size_of::<FeatureState>(), 520);
        assert_eq!(std::mem::size_of::<FeatureApi>(), 28);
    }
}
