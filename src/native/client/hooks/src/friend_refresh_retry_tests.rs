// Actual friend-refresh control flow regression. Test-only module; no copied
// production method, network RPC, game patch, native original DLL, or account IO.
// Run through maintain check-main, which is serial (--test-threads=1).
use super::*;
use std::sync::atomic::AtomicUsize;
use std::thread::JoinHandle;

const OLD_ID: &str = "mc7-old-friend";
const WATCHDOG: Duration = Duration::from_secs(5);
static ACK_CALLS: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());
enum PausePoint { Snapshot, Due }
struct Gate { reached: mpsc::Sender<Vec<String>>, resume: mpsc::Receiver<()> }
static SNAPSHOT_GATE: OnceLock<Mutex<Option<Gate>>> = OnceLock::new();
static DUE_GATE: OnceLock<Mutex<Option<Gate>>> = OnceLock::new();
fn gate_slot(point: PausePoint) -> &'static Mutex<Option<Gate>> {
    match point {
        PausePoint::Snapshot => SNAPSHOT_GATE.get_or_init(|| Mutex::new(None)),
        PausePoint::Due => DUE_GATE.get_or_init(|| Mutex::new(None)),
    }
}
fn pause(point: PausePoint, targets: &[String]) {
    let gate = gate_slot(point).lock().unwrap().take();
    if let Some(gate) = gate {
        gate.reached.send(targets.to_vec()).expect("fixture receiver alive");
        gate.resume.recv_timeout(WATCHDOG).expect("fixture release within watchdog");
    }
}
pub(super) fn pause_after_snapshot(targets: &[String]) { pause(PausePoint::Snapshot, targets); }
pub(super) fn pause_after_due(targets: &[String]) { pause(PausePoint::Due, targets); }
struct PausedWorker { resume: Option<mpsc::Sender<()>>, handle: Option<JoinHandle<()>> }
impl PausedWorker {
    fn release_and_join(&mut self) {
        self.resume.take().unwrap().send(()).unwrap();
        self.handle.take().unwrap().join().expect("actual retry worker returned");
    }
}
impl Drop for PausedWorker {
    fn drop(&mut self) {
        if let Some(resume) = self.resume.take() { let _ = resume.send(()); }
        if let Some(handle) = self.handle.take() { let _ = handle.join(); }
    }
}
fn start_worker_at(point: PausePoint) -> (PausedWorker, mpsc::Receiver<Vec<String>>) {
    let (reached_tx, reached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let mut slot = gate_slot(point).lock().unwrap();
    assert!(slot.is_none());
    *slot = Some(Gate { reached: reached_tx, resume: resume_rx });
    drop(slot);
    let handle = std::thread::spawn(retry_pending_friend_refresh_if_due);
    (PausedWorker { resume: Some(resume_tx), handle: Some(handle) }, reached_rx)
}
fn start_paused_worker() -> (PausedWorker, mpsc::Receiver<Vec<String>>) { start_worker_at(PausePoint::Due) }
fn start_snapshot_worker() -> (PausedWorker, mpsc::Receiver<Vec<String>>) { start_worker_at(PausePoint::Snapshot) }
// Disable the optional file trace only for this serial test, restoring the
// original environment on panic as well. No account file is opened or copied.
struct TraceDisabled(Option<std::ffi::OsString>);
impl TraceDisabled {
    fn new() -> Self {
        let original = std::env::var_os("SCBL_LAN_CLIENT_DATA_DIR");
        std::env::remove_var("SCBL_LAN_CLIENT_DATA_DIR");
        Self(original)
    }
}
impl Drop for TraceDisabled {
    fn drop(&mut self) {
        match self.0.take() {
            Some(original) => std::env::set_var("SCBL_LAN_CLIENT_DATA_DIR", original),
            None => std::env::remove_var("SCBL_LAN_CLIENT_DATA_DIR"),
        }
    }
}
fn reset_owned_state() {
    let _ = event_sender();
    let receiver = EVENTS.get().unwrap().lock().unwrap();
    while receiver.try_recv().is_ok() {}
    drop(receiver);
    COMPACT_FRIEND_REFRESH_TARGETS.get_or_init(|| Mutex::new(BTreeSet::new())).lock().unwrap().clear();
    *COMPACT_FRIEND_REFRESH_RETRY.get_or_init(|| Mutex::new(FriendRefreshRetryState::default())).lock().unwrap() = FriendRefreshRetryState::default();
    *friend_refresh_coordinator().lock().unwrap() = FriendRefreshCoordinator::default();
    assert!(gate_slot(PausePoint::Snapshot).lock().unwrap().is_none());
    assert!(gate_slot(PausePoint::Due).lock().unwrap().is_none());
    ACK_CALLS.store(0, Ordering::SeqCst);
}
fn pending() -> usize { COMPACT_FRIEND_REFRESH_TARGETS.get().unwrap().lock().unwrap().len() }
fn attempts() -> u8 { COMPACT_FRIEND_REFRESH_RETRY.get().unwrap().lock().unwrap().attempts }
fn arm_due() { arm_compact_friend_refresh(vec![OLD_ID.into()]); make_current_due(); assert_eq!(pending(), 1); }
fn acknowledge_real_pending() -> bool {
    // Exact successful-getter ACK seam. GetFriendList success calls this method;
    // RPC and projection are excluded here; the external fixture tests the successful getter.
    ACK_CALLS.fetch_add(1, Ordering::SeqCst);
    take_compact_friend_refresh() == Some([OLD_ID.to_string()].into_iter().collect())
}
fn next_event() -> (bool, bool) {
    let mut event = UplayEvent { event_type: UplayEventType::UserAccountSharing, unknown: 0 };
    let returned = unsafe { UPLAY_GetNextEvent(&mut event) };
    let refresh = returned && matches!(event.event_type, UplayEventType::FriendsFriendListUpdated);
    if returned { assert_eq!(event.unknown, 0); }
    (returned, refresh)
}

fn emit(case: &str, barrier_hit: bool, ack_ok: bool, after_getter: usize,
        attempts_after_getter: u8, returned: bool, refresh: bool,
        after_event: usize, attempts_after_worker: u8, pass: bool) {
    println!("\nMC7_PERMANENT_CASE case={case} barrier_hit={barrier_hit} ack_ok={ack_ok} pending_after_getter={after_getter} attempts_after_getter={attempts_after_getter} event_returned={returned} event_is_refresh={refresh} pending_after_event={after_event} attempts_after_worker={attempts_after_worker} ack_calls={} pass={pass}", ACK_CALLS.load(Ordering::SeqCst));
}
#[test]
fn mc7_permanent_01_ack_before_worker_stays_cleared() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    let ack_ok = acknowledge_real_pending();
    let after_getter = pending(); let ack_attempts = attempts();
    retry_pending_friend_refresh_if_due();
    let worker_attempts = attempts(); let (returned, refresh) = next_event();
    let after_event = pending();
    let pass = ack_ok && after_getter == 0 && ack_attempts == 0 && !returned && !refresh
        && after_event == 0 && worker_attempts == 0 && ACK_CALLS.load(Ordering::SeqCst) == 1;
    emit("ack_before_worker", false, ack_ok, after_getter, ack_attempts, returned, refresh, after_event, worker_attempts, pass);
    assert!(pass, "ack before worker must remain cleared");
}
#[test]
fn mc7_permanent_02_due_without_ack_delivers() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    retry_pending_friend_refresh_if_due();
    let worker_attempts = attempts(); let (returned, refresh) = next_event();
    let after_event = pending();
    let ack_ok = acknowledge_real_pending(); let after_getter = pending(); let ack_attempts = attempts();
    let no_extra_event = !next_event().0;
    let pass = returned && refresh && after_event == 1 && worker_attempts == 1 && ack_ok
        && after_getter == 0 && ack_attempts == 0 && no_extra_event && ACK_CALLS.load(Ordering::SeqCst) == 1;
    emit("due_without_ack", false, ack_ok, after_getter, ack_attempts, returned, refresh, after_event, worker_attempts, pass);
    assert!(pass, "genuine due unacknowledged retry must still deliver and then acknowledge");
}
#[test]
fn mc7_permanent_03_snapshot_then_getter_ack_rejects_stale_rearm() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    let (mut worker, reached) = start_snapshot_worker();
    let actual_snapshot = reached.recv_timeout(WATCHDOG).expect("actual worker reached snapshot barrier");
    assert_eq!(actual_snapshot, vec![OLD_ID.to_string()]);
    let ack_ok = acknowledge_real_pending(); let after_getter = pending(); let ack_attempts = attempts();
    assert!(ack_ok); assert_eq!(after_getter, 0); assert_eq!(ack_attempts, 0);
    worker.release_and_join();
    let worker_attempts = attempts(); let (returned, refresh) = next_event();
    let after_event = pending();
    let pass = ack_ok && after_getter == 0 && ack_attempts == 0 && !returned && !refresh
        && after_event == 0 && ACK_CALLS.load(Ordering::SeqCst) == 1;
    emit("snapshot_then_getter_ack", true, ack_ok, after_getter, ack_attempts, returned, refresh, after_event, worker_attempts, pass);
    assert!(pass, "acknowledged snapshot must not enqueue/rearm stale work");
}

// Immutable expanded input/oracle. Actual retry, ack, queue and export body are
// outside this section and source-bound separately for each selected revision.
fn emit_extended(case: &str, barrier_hit: bool, ack_ok: bool, pending_after_ack: usize,
    attempts_after_ack: u8, stale_returned: bool, stale_refresh: bool, pending_after_stale: usize,
    fresh_returned: bool, fresh_retry_returned: bool, final_pending: usize, pass: bool) {
    println!("\nMC7_PERMANENT_CASE case={case} barrier_hit={barrier_hit} ack_ok={ack_ok} pending_after_ack={pending_after_ack} attempts_after_ack={attempts_after_ack} stale_returned={stale_returned} stale_refresh={stale_refresh} pending_after_stale={pending_after_stale} fresh_returned={fresh_returned} fresh_retry_returned={fresh_retry_returned} final_pending={final_pending} ack_calls={} pass={pass}", ACK_CALLS.load(Ordering::SeqCst));
}
fn publish_real_confirmed_change() {
    // A genuine same-account offline->online change, using the real coordinator
    // and production Event variant. No network poller or native patch is started.
    seed_friend_refresh_snapshot(&[crate::api::Friend {
        id: OLD_ID.into(), username: "mc7-synthetic-name".into(), is_online: false,
    }]);
    let online = vec![crate::api::Friend {
        id: OLD_ID.into(), username: "mc7-synthetic-name".into(), is_online: true,
    }];
    assert!(observe_friend_refresh(online.clone()).is_none());
    let update = observe_friend_refresh(online).expect("real coordinator confirms changed snapshot");
    assert_eq!(update.targets, vec![OLD_ID.to_string()]);
    let game_event_tx = event_sender();
    game_event_tx.send(Event::FriendsFriendListUpdated(update.targets)).expect("owned queue available");
}
fn make_current_due() {
    COMPACT_FRIEND_REFRESH_RETRY.get().unwrap().lock().unwrap().last_attempt =
        Some(std::time::Instant::now().checked_sub(FRIEND_REFRESH_RETRY_INTERVAL + Duration::from_secs(1)).unwrap());
}
#[test]
fn mc7_permanent_04_take_due_then_getter_ack() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    let (mut worker, reached) = start_paused_worker();
    let actual_snapshot = reached.recv_timeout(WATCHDOG).expect("actual worker passed take_due and released retry lock");
    assert_eq!(actual_snapshot, vec![OLD_ID.to_string()]);
    assert_eq!(attempts(), 1);
    let ack_ok = acknowledge_real_pending(); let ack_pending = pending(); let ack_attempts = attempts();
    assert!(ack_ok && ack_pending == 0 && ack_attempts == 0);
    worker.release_and_join();
    let (returned, refresh) = next_event(); let after_event = pending();
    let pass = ack_ok && ack_pending == 0 && ack_attempts == 0 && !returned && !refresh
        && after_event == 0 && attempts() == 0 && !next_event().0 && ACK_CALLS.load(Ordering::SeqCst) == 1;
    emit_extended("take_due_then_getter_ack", true, ack_ok, ack_pending, ack_attempts,
        returned, refresh, after_event, false, false, pending(), pass);
    assert!(pass, "issued retry acknowledged before send must not return or re-arm");
}
#[test]
fn mc7_permanent_05_enqueued_then_getter_ack() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    retry_pending_friend_refresh_if_due(); // Real send completed before ACK.
    assert_eq!(attempts(), 1);
    let ack_ok = acknowledge_real_pending(); let ack_pending = pending(); let ack_attempts = attempts();
    assert!(ack_ok && ack_pending == 0 && ack_attempts == 0);
    let (returned, refresh) = next_event(); let after_event = pending();
    let pass = ack_ok && ack_pending == 0 && ack_attempts == 0 && !returned && !refresh
        && after_event == 0 && attempts() == 0 && !next_event().0 && ACK_CALLS.load(Ordering::SeqCst) == 1;
    emit_extended("enqueued_then_getter_ack", false, ack_ok, ack_pending, ack_attempts,
        returned, refresh, after_event, false, false, pending(), pass);
    assert!(pass, "queued retry acknowledged before dequeue must not return or re-arm");
}
#[test]
fn mc7_permanent_06_new_refresh_after_ack_is_preserved() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    let ack_ok = acknowledge_real_pending(); let ack_pending = pending(); let ack_attempts = attempts();
    publish_real_confirmed_change();
    let fresh = next_event(); let after_event = pending();
    assert!(fresh.0 && fresh.1 && after_event == 1 && attempts() == 0);
    make_current_due(); retry_pending_friend_refresh_if_due();
    let fresh_retry = next_event();
    let second_ack = acknowledge_real_pending();
    let pass = ack_ok && ack_pending == 0 && ack_attempts == 0 && fresh.0 && fresh.1 && after_event == 1
        && fresh_retry.0 && fresh_retry.1 && second_ack && pending() == 0 && attempts() == 0
        && !next_event().0 && ACK_CALLS.load(Ordering::SeqCst) == 2;
    emit_extended("new_refresh_after_ack_is_preserved", false, ack_ok, ack_pending, ack_attempts,
        false, false, after_event, fresh.0 && fresh.1, fresh_retry.0 && fresh_retry.1, pending(), pass);
    assert!(pass, "ACK must not disable a later genuine refresh or its legitimate retry");
}
#[test]
fn mc7_permanent_07_old_issued_retry_cannot_impersonate_new_refresh() {
    let _serial = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _trace_disabled = TraceDisabled::new();
    reset_owned_state(); arm_due();
    let (mut worker, reached) = start_paused_worker();
    assert_eq!(reached.recv_timeout(WATCHDOG).expect("issued old retry barrier"), vec![OLD_ID.to_string()]);
    assert_eq!(attempts(), 1);
    let ack_ok = acknowledge_real_pending(); let ack_pending = pending(); let ack_attempts = attempts();
    assert!(ack_ok && ack_pending == 0 && ack_attempts == 0);
    publish_real_confirmed_change();
    let fresh = next_event();
    assert!(fresh.0 && fresh.1 && pending() == 1 && attempts() == 0);
    worker.release_and_join(); // Old issued retry is enqueued only after new arm.
    let stale = next_event(); let after_stale = pending();
    let untouched_new = after_stale == 1 && attempts() == 0;
    make_current_due(); retry_pending_friend_refresh_if_due();
    let fresh_retry = next_event();
    let second_ack = acknowledge_real_pending();
    let pass = ack_ok && !stale.0 && !stale.1 && untouched_new && fresh.0 && fresh.1
        && fresh_retry.0 && fresh_retry.1 && second_ack && pending() == 0 && attempts() == 0
        && !next_event().0 && ACK_CALLS.load(Ordering::SeqCst) == 2;
    emit_extended("old_issued_retry_cannot_impersonate_new_refresh", true, ack_ok, ack_pending, ack_attempts,
        stale.0, stale.1, after_stale, fresh.0 && fresh.1, fresh_retry.0 && fresh_retry.1, pending(), pass);
    assert!(pass, "old token must neither impersonate nor clear a newly armed refresh");
}
