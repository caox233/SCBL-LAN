//! Opt-in, bounded native goal-origin evidence. No game memory is modified.
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameA, GetModuleHandleExA,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};

const FRAME_LIMIT: usize = 8;
const SAMPLE_LIMIT: u32 = 32;
const INTERVAL_MS: u64 = 250;
#[derive(Default)]
struct TraceGate { last: [Option<u64>; 2], count: [u32; 2] }
impl TraceGate {
    fn admit(&mut self, goal: Option<&[u8]>, now: u64) -> Option<(usize, u32)> {
        let index = match goal { Some(b"eGoal_LeaveMatch") => 0, Some(b"eGoal_Run") => 1, _ => return None };
        if self.count[index] >= SAMPLE_LIMIT || self.last[index].is_some_and(|last| now.saturating_sub(last) < INTERVAL_MS) {
            return None;
        }
        self.last[index] = Some(now);
        self.count[index] += 1;
        Some((index, self.count[index]))
    }
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut *mut c_void, hash: *mut u32) -> u16;
}
fn module_leaf(path: &[u8]) -> String {
    let leaf = path.rsplit(|b| *b == b'\\' || *b == b'/').next().unwrap_or_default();
    leaf.iter().take(64).map(|b| if b.is_ascii_alphanumeric() || matches!(*b,b'.'|b'_'|b'-') { char::from(*b) } else {'_'}).collect()
}
#[inline(never)]
fn capture_frames() -> Vec<String> {
    let mut frames = [std::ptr::null_mut::<c_void>(); FRAME_LIMIT];
    let captured = unsafe { RtlCaptureStackBackTrace(1, FRAME_LIMIT as u32, frames.as_mut_ptr(), std::ptr::null_mut()) } as usize;
    frames.into_iter().take(captured.min(FRAME_LIMIT)).map(|frame| {
        let address = frame as usize;
        let mut module = HMODULE::default();
        let resolved = unsafe {
            GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                PCSTR(frame.cast()), &mut module)
        }.is_ok();
        if !resolved { return String::from("unknown"); }
        let mut path = [0u8; 512];
        let length = unsafe { GetModuleFileNameA(module, &mut path) } as usize;
        let leaf = module_leaf(&path[..length.min(path.len())]);
        let rva = address.saturating_sub(module.0 as usize);
        format!("{}+0x{rva:08x}", if leaf.is_empty() { "unknown" } else { &leaf })
    }).collect()
}
pub(super) fn trace_goal(goal: Option<&[u8]>, queue: usize) {
    static START: OnceLock<Instant> = OnceLock::new();
    static GATE: OnceLock<Mutex<TraceGate>> = OnceLock::new();
    let now = START.get_or_init(Instant::now).elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    // Diagnostic contention or poisoning must never delay or abort a game goal.
    let Some((index, sample)) = GATE.get_or_init(|| Mutex::new(TraceGate::default()))
        .try_lock().ok().and_then(|mut gate| gate.admit(goal, now)) else { return; };
    let goal = if index == 0 { "eGoal_LeaveMatch" } else { "eGoal_Run" };
    let frames = capture_frames();
    tracing::info!("LanGoalOrigin goal={goal} sample={sample} queue=0x{queue:08x} frames={} frame_count={} behavior_changed=false", frames.join("|"), frames.len());
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn unrelated_or_missing_goals_do_not_capture() {
        let mut gate=TraceGate::default();
        assert!(gate.admit(None,0).is_none());
        assert!(gate.admit(Some(b"eGoal_Login"),1).is_none());
        assert_eq!(gate.count,[0,0]);
    }
    #[test] fn targets_have_independent_limits_and_cooldowns() {
        let mut gate=TraceGate::default();
        assert_eq!(gate.admit(Some(b"eGoal_LeaveMatch"),0),Some((0,1)));
        assert!(gate.admit(Some(b"eGoal_LeaveMatch"),249).is_none());
        assert_eq!(gate.admit(Some(b"eGoal_Run"),1),Some((1,1)));
        for sample in 2..=SAMPLE_LIMIT {
            assert_eq!(gate.admit(Some(b"eGoal_LeaveMatch"),u64::from(sample)*INTERVAL_MS),Some((0,sample)));
        }
        assert!(gate.admit(Some(b"eGoal_LeaveMatch"),100_000).is_none());
        assert_eq!(gate.admit(Some(b"eGoal_Run"),100_000),Some((1,2)));
    }
    #[test] fn module_labels_never_include_user_directory_paths() {
        assert_eq!(module_leaf(br"C:\Users\SYNTHETIC\Desktop\SCBL LAN\uplay_r1_loader.dll"),"uplay_r1_loader.dll");
        assert_eq!(module_leaf(b"/synthetic/private/path/game.exe"),"game.exe");
    }
    #[test] fn windows_stack_capture_is_bounded_and_has_no_paths() {
        let frames=capture_frames();assert!(!frames.is_empty());assert!(frames.len()<=FRAME_LIMIT);
        assert!(frames.iter().all(|f| !f.contains(['\\','/',':'])));
        assert!(frames.iter().any(|f| f.contains("+0x")));
    }
}

