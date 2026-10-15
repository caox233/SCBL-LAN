//! Only the MP drone native health initialization; not a damage multiplier.
//! AA5150 calls E793E0 then copies instance +B44 to current +5DC/max +5E0.
//! Leave the asset/default B44 untouched, so repeated native initialization
//! starts from the original tuning rather than repeatedly dividing live HP.
use crate::equipment_native::{signature, word, writable};
use retour::static_detour;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
const DRONE: u32 = 0x02a88f54;
const INIT: usize = 0xaa5150;
const SHA: &str = "cfdcc982cf69e9baed358ad6ad8a2713852a0f69f604014a8c4c31b7ab33d0d6";
static READY: AtomicBool = AtomicBool::new(false);
static LOGS: AtomicU32 = AtomicU32::new(0);
static_detour! {static Initialize:unsafe extern "thiscall" fn(usize);}
unsafe fn after_initialize(body: usize) -> Option<(u32, u32)> {
    if word(body, 0) != Some(DRONE) {
        return None;
    }
    let original = word(body, 0xb44)?;
    if original < 2
        || original > i32::MAX as u32
        || word(body, 0x5dc) != Some(original)
        || word(body, 0x5e0) != Some(original)
        || !writable(body + 0x5dc, 8)
    {
        return None;
    }
    let half = original / 2;
    ((body + 0x5dc) as *mut u32).write_unaligned(half);
    ((body + 0x5e0) as *mut u32).write_unaligned(half);
    Some((original, half))
}
fn initialize(body: usize) {
    unsafe {
        Initialize.call(body);
        if READY.load(Ordering::Acquire) {
            if let Some((original, half)) = after_initialize(body) {
                if LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
                    tracing::info!("SCBL_MOD_DRONE_HEALTH revision=C55 native={} maximum={} current={} damage_and_emp=native",original,half,half);
                }
            }
        }
    }
}
pub(crate) unsafe fn init(world: usize) {
    if world != 0x589860
        || !signature(INIT, 37, SHA)
        || word(DRONE as usize, 0x178) != Some(INIT as u32)
    {
        return;
    }
    if Initialize
        .initialize(
            std::mem::transmute::<usize, unsafe extern "thiscall" fn(usize)>(INIT),
            initialize,
        )
        .and_then(|h| h.enable())
        .is_ok()
    {
        READY.store(true, Ordering::Release);
        tracing::info!(
            "SCBL_MOD_DRONE_HEALTH enabled=true revision=C55 maximum_factor=0.5 native_damage=true"
        );
    }
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    let _ = Initialize.disable();
}
#[cfg(test)]
#[path = "drone_health_tests.rs"]
mod tests;
