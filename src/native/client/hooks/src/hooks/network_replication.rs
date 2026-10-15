use std::sync::Mutex;

use tracing::error;
use tracing::info;

use crate::addresses::Addresses;
const SUPPORTED_WORLD_RAYCAST: usize = 0x0058_9860;

// User-requested temporary suspension: keep native constructor operands untouched.
// Retain the reversible installer for a future separately validated re-enable.
const BOOST_TEMPORARILY_DISABLED: bool = true;

pub(crate) const fn enabled_for_mode(is_mod: bool) -> bool {
    is_mod && !BOOST_TEMPORARILY_DISABLED
}

const NATIVE_PLAYER_MOTION_HZ: f32 = 10.0;
const NATIVE_WEAPON_STATUS_HZ: f32 = 5.0;

const PLAYER_MOTION_HZ: f32 = 20.0;
const WEAPON_STATUS_HZ: f32 = 10.0;
const MOBILE_MOTION_HZ: f32 = 10.0;

static PLAYER_MOTION_INTERVAL: f32 = 1.0 / PLAYER_MOTION_HZ;
static WEAPON_STATUS_INTERVAL: f32 = 1.0 / WEAPON_STATUS_HZ;

const RETAIL_PLAYER_MOTION_INTERVAL_POINTER: [u8; 4] = 0x0299_3AD0u32.to_le_bytes();
const RETAIL_WEAPON_STATUS_INTERVAL_POINTER: [u8; 4] = 0x0299_9F24u32.to_le_bytes();
const PLAYER_MOTION_INTERVAL_LOAD: usize = 0x00FD_9B51;
const WEAPON_STATUS_INTERVAL_LOAD: usize = 0x00FD_9A4C;

// Only synchronous startup/shutdown owns these two operands. Never forget an
// attempted write until its original bytes and instruction cache are restored.
static INTERVAL_STATE: Mutex<IntervalState> = Mutex::new(IntervalState::new());

#[derive(Clone, Copy)]
struct IntervalPatch { address: usize, original: [u8; 4], modified: [u8; 4] }
struct IntervalState { pending: u8, active: bool }
impl IntervalState {
    const fn new() -> Self { Self { pending: 0, active: false } }

    fn set(
        &mut self, patches: &[IntervalPatch; 2], enabled: bool,
        matches: &mut impl FnMut(usize, &[u8; 4]) -> bool,
        write: &mut impl FnMut(usize, &[u8; 4], &[u8; 4]) -> bool,
    ) -> bool {
        if !enabled { return self.restore(patches, matches, write); }
        if self.active && patches.iter().all(|p| matches(p.address, &p.modified)) { return true; }
        if !self.restore(patches, matches, write) { return false; }
        if patches.iter().any(|p| p.original == p.modified || !matches(p.address, &p.original)) { return false; }
        for (index, p) in patches.iter().enumerate() {
            self.pending |= 1 << index;
            if !write(p.address, &p.original, &p.modified) {
                self.restore(patches, matches, write);
                return false;
            }
        }
        self.active = true;
        true
    }

    fn restore(
        &mut self, patches: &[IntervalPatch; 2],
        matches: &mut impl FnMut(usize, &[u8; 4]) -> bool,
        write: &mut impl FnMut(usize, &[u8; 4], &[u8; 4]) -> bool,
    ) -> bool {
        self.active = false;
        for (index, p) in patches.iter().enumerate().rev() {
            if self.pending & (1 << index) == 0 { continue; }
            let expected = if matches(p.address, &p.original) { &p.original }
                else if matches(p.address, &p.modified) { &p.modified }
                else { continue; }; // Unknown bytes remain pending; other sites can restore.
            if write(p.address, expected, &p.original) { self.pending &= !(1 << index); }
        }
        self.pending == 0
    }
}

unsafe fn matches_operand(address: usize, expected: &[u8; 4]) -> bool {
    let Some(end) = address.checked_add(expected.len()) else { return false; };
    if address < 0x10000 { return false; }
    let Ok(region) = region::query(address as *const u8) else { return false; };
    region.is_committed() && region.is_readable() && !region.is_guarded()
        && (region.as_ptr::<u8>() as usize).checked_add(region.len()).is_some_and(|limit| end <= limit)
        && std::slice::from_raw_parts(address as *const u8, expected.len()) == expected
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn FlushInstructionCache(process: *mut std::ffi::c_void, address: *const std::ffi::c_void, size: usize) -> i32;
}

unsafe fn write_operand(address: usize, expected: &[u8; 4], next: &[u8; 4]) -> bool {
    if !matches_operand(address, expected) { return false; }
    let pointer = address as *mut u8;
    if expected != next {
        let Ok(guard) = region::protect_with_handle(pointer, next.len(), region::Protection::READ_WRITE_EXECUTE)
        else { return false; };
        if !matches_operand(address, expected) { return false; }
        std::ptr::copy_nonoverlapping(next.as_ptr(), pointer, next.len());
        drop(guard);
    }
    // Also flush byte-equal cleanup retries after an earlier failed publication.
    FlushInstructionCache(GetCurrentProcess(), pointer.cast(), next.len()) != 0
        && matches_operand(address, next)
}

fn interval_pointer(interval: &'static f32) -> Option<[u8; 4]> {
    u32::try_from(std::ptr::from_ref(interval) as usize).ok().map(u32::to_le_bytes)
}

fn player_motion_interval_pointer() -> Option<[u8; 4]> {
    interval_pointer(&PLAYER_MOTION_INTERVAL)
}

fn weapon_status_interval_pointer() -> Option<[u8; 4]> {
    interval_pointer(&WEAPON_STATUS_INTERVAL)
}

unsafe fn set_replication_intervals(enabled: bool) -> bool {
    let Some(player_motion_pointer) = player_motion_interval_pointer() else {
        error!("SCBL_NETWORK_REPLICATION enabled=false reason=player_interval_pointer_out_of_range");
        return false;
    };
    let Some(weapon_status_pointer) = weapon_status_interval_pointer() else {
        error!("SCBL_NETWORK_REPLICATION enabled=false reason=weapon_interval_pointer_out_of_range");
        return false;
    };

    let patches = [
        IntervalPatch { address: PLAYER_MOTION_INTERVAL_LOAD + 4, original: RETAIL_PLAYER_MOTION_INTERVAL_POINTER, modified: player_motion_pointer },
        IntervalPatch { address: WEAPON_STATUS_INTERVAL_LOAD + 4, original: RETAIL_WEAPON_STATUS_INTERVAL_POINTER, modified: weapon_status_pointer },
    ];
    let mut state = INTERVAL_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.set(&patches, enabled,
        &mut |address, expected| matches_operand(address, expected),
        &mut |address, expected, next| write_operand(address, expected, next))
}

pub(crate) unsafe fn init(addresses: &Addresses, enabled: bool) {
    if !enabled_for_mode(enabled) {
        info!(
            "SCBL_NETWORK_REPLICATION enabled=false source=Native reason={} player_motion_hz={NATIVE_PLAYER_MOTION_HZ:.0} weapon_status_hz={NATIVE_WEAPON_STATUS_HZ:.0} mobile_motion_hz={MOBILE_MOTION_HZ:.0}",
            if enabled { "temporarily_disabled" } else { "official_mode" }
        );
        return;
    }
    if addresses.func_world_raycast != Some(SUPPORTED_WORLD_RAYCAST) {
        error!(
            "SCBL_NETWORK_REPLICATION enabled=false reason=unsupported_executable world_raycast={:?}",
            addresses.func_world_raycast
        );
        return;
    }
    if !set_replication_intervals(true) {
        error!("SCBL_NETWORK_REPLICATION enabled=false reason=replication_patch_failed");
        return;
    }

    info!(
        "SCBL_NETWORK_REPLICATION enabled=true source=MOD player_motion_hz={PLAYER_MOTION_HZ:.0} player_motion_interval_ms={:.1} weapon_status_hz={WEAPON_STATUS_HZ:.0} weapon_status_interval_ms={:.1} mobile_motion_hz={MOBILE_MOTION_HZ:.0} mobile_motion_interval_ms={:.1}",
        PLAYER_MOTION_INTERVAL * 1000.0,
        WEAPON_STATUS_INTERVAL * 1000.0,
        1000.0 / MOBILE_MOTION_HZ
    );
}

pub(crate) unsafe fn deinit() {
    if !set_replication_intervals(false) {
        error!("SCBL_NETWORK_REPLICATION restore=false");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn temporary_suspension_keeps_both_modes_native() {
        assert!(!super::enabled_for_mode(true));
        assert!(!super::enabled_for_mode(false));
        assert_eq!(super::NATIVE_PLAYER_MOTION_HZ, 10.0);
        assert_eq!(super::NATIVE_WEAPON_STATUS_HZ, 5.0);
    }

    use super::player_motion_interval_pointer;
    use super::weapon_status_interval_pointer;
    use super::MOBILE_MOTION_HZ;
    use super::PLAYER_MOTION_HZ;
    use super::PLAYER_MOTION_INTERVAL;
    use super::PLAYER_MOTION_INTERVAL_LOAD;
    use super::RETAIL_PLAYER_MOTION_INTERVAL_POINTER;
    use super::RETAIL_WEAPON_STATUS_INTERVAL_POINTER;
    use super::WEAPON_STATUS_HZ;
    use super::WEAPON_STATUS_INTERVAL;
    use super::WEAPON_STATUS_INTERVAL_LOAD;
    #[test]
    fn layered_intervals_match_the_selected_game_specific_targets() {
        assert_eq!(PLAYER_MOTION_HZ, 20.0);
        assert_eq!(PLAYER_MOTION_INTERVAL, 0.05);
        assert_eq!(WEAPON_STATUS_HZ, 10.0);
        assert_eq!(WEAPON_STATUS_INTERVAL, 0.1);
        assert_eq!(MOBILE_MOTION_HZ, 10.0);
    }

    #[test]
    fn only_player_motion_and_weapon_status_loads_are_patched() {
        assert_eq!(PLAYER_MOTION_INTERVAL_LOAD, 0x00FD_9B51);
        assert_eq!(WEAPON_STATUS_INTERVAL_LOAD, 0x00FD_9A4C);
        assert_ne!(player_motion_interval_pointer().unwrap(), RETAIL_PLAYER_MOTION_INTERVAL_POINTER);
        assert_ne!(weapon_status_interval_pointer().unwrap(), RETAIL_WEAPON_STATUS_INTERVAL_POINTER);
    }
    use super::{IntervalPatch, IntervalState};
    use std::cell::RefCell;
    const PLAN: [IntervalPatch; 2] = [
        IntervalPatch {address:0, original:[1;4], modified:[11;4]},
        IntervalPatch {address:1, original:[2;4], modified:[12;4]},
    ];
    struct Memory { bytes:[[u8;4];2], fail:u8, writes:Vec<usize> }
    fn memory() -> RefCell<Memory> {RefCell::new(Memory {bytes:[[1;4],[2;4]],fail:0,writes:vec![]})}
    fn set(s:&mut IntervalState,m:&RefCell<Memory>,enabled:bool)->bool {
        s.set(&PLAN,enabled,&mut |a,e|m.borrow().bytes[a]==*e,&mut |a,e,n| {
            let mut m=m.borrow_mut();m.writes.push(a);
            if m.fail & (1<<a)!=0 || m.bytes[a]!=*e {return false;}
            m.bytes[a]=*n;true
        })
    }
    #[test] fn failed_shutdown_retains_pending_and_retries_only_failed_site() {
        let mut s=IntervalState::new();let m=memory();assert!(set(&mut s,&m,true));
        m.borrow_mut().fail=2;m.borrow_mut().writes.clear();assert!(!set(&mut s,&m,false));
        assert_eq!(s.pending,2);assert!(!s.active);assert_eq!(m.borrow().bytes,[[1;4],[12;4]]);
        assert_eq!(m.borrow().writes,[1,0]);m.borrow_mut().writes.clear();m.borrow_mut().fail=0;
        assert!(set(&mut s,&m,false));assert_eq!(m.borrow().writes,[1]);assert_eq!(s.pending,0);
    }
    #[test] fn foreign_bytes_do_not_block_other_cleanup_or_get_overwritten() {
        let mut s=IntervalState::new();let m=memory();assert!(set(&mut s,&m,true));
        m.borrow_mut().bytes[1]=[99;4];assert!(!set(&mut s,&m,false));
        assert_eq!(m.borrow().bytes,[[1;4],[99;4]]);assert_eq!(s.pending,2);
        m.borrow_mut().bytes[1]=[12;4];assert!(set(&mut s,&m,false));
    }
    #[test] fn unowned_replacement_is_never_adopted() {
        let mut s=IntervalState::new();let m=memory();m.borrow_mut().bytes[0]=[11;4];
        assert!(!set(&mut s,&m,true));assert!(set(&mut s,&m,false));
        assert!(m.borrow().writes.is_empty());assert_eq!(s.pending,0);
    }
    #[test] fn duplicate_install_is_noop_but_foreign_change_is_not_success() {
        let mut s=IntervalState::new();let m=memory();assert!(set(&mut s,&m,true));
        m.borrow_mut().writes.clear();assert!(set(&mut s,&m,true));assert!(m.borrow().writes.is_empty());
        m.borrow_mut().bytes[1]=[99;4];assert!(!set(&mut s,&m,true));assert!(!s.active);
        assert_eq!(m.borrow().bytes,[[1;4],[99;4]]);
    }
    #[test] fn failed_cleanup_blocks_reinstall_until_recovered() {
        let mut s=IntervalState::new();let m=memory();assert!(set(&mut s,&m,true));
        m.borrow_mut().fail=3;assert!(!set(&mut s,&m,false));assert!(!set(&mut s,&m,true));
        assert_eq!(s.pending,3);assert!(!s.active);m.borrow_mut().fail=0;
        assert!(set(&mut s,&m,true));assert!(s.active);
    }
    #[test] fn byte_equal_cleanup_must_finish_failed_cache_sync() {
        let mut s=IntervalState {pending:1,active:false};let m=memory();m.borrow_mut().fail=1;
        assert!(!set(&mut s,&m,false));assert_eq!(s.pending,1);m.borrow_mut().fail=0;
        assert!(set(&mut s,&m,false));assert_eq!(m.borrow().writes,[0,0]);assert_eq!(s.pending,0);
    }

}
