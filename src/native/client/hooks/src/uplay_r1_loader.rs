use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::c_char;
use std::ffi::c_void;
use std::ffi::CStr;
#[cfg(feature = "diagnostic-evidence")]
use std::fs;
#[cfg(feature = "diagnostic-evidence")]
use std::fs::OpenOptions;
#[cfg(feature = "diagnostic-evidence")]
use std::io::Write;
#[cfg(feature = "diagnostic-evidence")]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "diagnostic-evidence")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
#[cfg(feature = "diagnostic-evidence")]
use std::time::Instant;
#[cfg(feature = "diagnostic-evidence")]
use std::time::SystemTime;
#[cfg(feature = "diagnostic-evidence")]
use std::time::UNIX_EPOCH;

use hooks_proc::forwardable_export;
use sha2::Digest;
use sha2::Sha256;
use tracing::error;
use tracing::info;
use tracing::warn;
use windows::core::s;
use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetProcAddress;
use windows::Win32::System::LibraryLoader::LoadLibraryA;

mod ach;
mod api_updates;
mod startup;
mod avatar;
mod friends;
mod overlay;
mod party;
mod presence;
pub(crate) mod save;
mod types;
mod user;
mod win;

use types::List;
use types::UplayDataBlob;
use types::UplayFriend;
use types::UplayGameSession;
use types::UplayList;
use types::UplayOverlapped;
use types::UplaySave;

use self::types::UplayEvent;
use self::types::UplayEventType;

type Result<T> = std::result::Result<T, anyhow::Error>;

fn get_proc(name: PCSTR) -> Option<unsafe extern "system" fn() -> isize> {
    static DLL_HANDLE: OnceLock<windows::core::Result<HMODULE>> = OnceLock::new();
    // Original-library lifetime is process-wide. Failed loads are cached too;
    // callers report native failure rather than blocking in UI or unwinding.
    let handle = DLL_HANDLE
        .get_or_init(|| {
            unsafe { LoadLibraryA(s!("uplay_r1_loader.orig.dll")) }
                .inspect_err(|e| error!("Library loading error: {e:?}"))
                .inspect(|_| info!("Library loaded"))
        })
        .as_ref()
        .ok()?;
    unsafe { GetProcAddress(*handle, name) }
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_GetLastError(out_error_string: *mut *const c_char) -> bool {
    false
}

#[derive(Clone)]
pub struct PartyGameInvite {
    pub invitation_id: u32,
    pub kind: i32,
    pub sender_id: String,
    pub sender_name: String,
    pub game_session: server_api::misc::UplayGameSession,
}

const BLACKLIST_FRIEND_INVITE_DATA_BYTES: usize = 0x1f0;
const BLACKLIST_ACCOUNT_ID_BYTES: usize = 128;
const BLACKLIST_FRIEND_REMAINDER_BYTES: usize = BLACKLIST_FRIEND_INVITE_DATA_BYTES - 8 - BLACKLIST_ACCOUNT_ID_BYTES;

/// Retail Blacklist validates a 496-byte Friends invite object, requires the
/// first DWORD to be 1, and reads the account id beginning at byte offset 8.
#[repr(C)]
struct BlacklistFriendInviteData {
    version: u32,
    reserved: u32,
    account_id: [u8; BLACKLIST_ACCOUNT_ID_BYTES],
    remainder: [u8; BLACKLIST_FRIEND_REMAINDER_BYTES],
}

#[repr(C)]
struct BlacklistFriendIdentityRef {
    reserved: [usize; 2],
    identity: *const BlacklistFriendInviteData,
    data_bytes: usize,
}

#[repr(C)]
struct BlacklistFriendAccepted {
    identity: *const BlacklistFriendIdentityRef,
}

/// Both Blacklist retail executables read `game_session` from payload +4.
/// On their 32-bit ABI this requires a u32 invitation id, not u64.
#[repr(C)]
struct BlacklistPartyAccepted {
    invitation_id: u32,
    game_session: *const UplayGameSession,
}

pub(crate) const PRIVATE_INVITE_MODE_LABEL: &str = "baseline";
const PRIVATE_INVITE_MODE_ID: u8 = 0;

#[cfg(feature = "diagnostic-evidence")]
static PRIVATE_INVITE_EVIDENCE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
#[cfg(feature = "diagnostic-evidence")]
static PRIVATE_INVITE_EVIDENCE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
#[cfg(feature = "diagnostic-evidence")]
static GET_NEXT_EVENT_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "diagnostic-evidence")]
static GET_NEXT_EVENT_POLL_STATE: OnceLock<Mutex<(Instant, u64)>> = OnceLock::new();

const PRIVATE_INVITE_BLOB_BYTES: usize = 0x1f0;
const PRIVATE_INVITE_ACCOUNT_OFFSET: usize = 0x08;
const PRIVATE_INVITE_ACCOUNT_BYTES: usize = 0x80;
#[cfg(feature = "diagnostic-evidence")]
const PRIVATE_INVITE_PAYLOAD_OFFSET: usize = 0x88;
#[cfg(feature = "diagnostic-evidence")]
const PRIVATE_INVITE_PAYLOAD_CAPACITY: usize = 0x164;
#[cfg(any(feature = "diagnostic-evidence", test))]
const PRIVATE_INVITE_PAYLOAD_SIZE_OFFSET: usize = 0x1ec;

#[cfg(feature = "diagnostic-evidence")]
fn evidence_directory() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.join("scbl48-evidence");
    fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn short_sha256(bytes: &[u8]) -> String {
    sha256_hex(bytes).chars().take(12).collect()
}

fn short_sha256_from_hex(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

#[cfg(any(feature = "diagnostic-evidence", test))]
fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

#[cfg(test)]
fn blob_account_id(bytes: &[u8]) -> String {
    let Some(account) = bytes.get(PRIVATE_INVITE_ACCOUNT_OFFSET..PRIVATE_INVITE_ACCOUNT_OFFSET + PRIVATE_INVITE_ACCOUNT_BYTES) else {
        return String::from("<unavailable>");
    };
    let length = account.iter().position(|byte| *byte == 0).unwrap_or(account.len());
    String::from_utf8_lossy(&account[..length]).into_owned()
}

#[cfg(feature = "diagnostic-evidence")]
pub(crate) fn record_private_invite_blob(stage: &str, invitation_id: i64, outer_session_id: u64, expected_session_id: u64, bytes: &[u8]) {
    let sequence = PRIVATE_INVITE_EVIDENCE_SEQUENCE.fetch_add(1, Ordering::AcqRel);
    let unknown1 = read_u32_le(bytes, 0);
    let checksum = read_u32_le(bytes, 4);
    let payload_size = read_u32_le(bytes, PRIVATE_INVITE_PAYLOAD_SIZE_OFFSET).and_then(|value| usize::try_from(value).ok());
    let payload = payload_size
        .filter(|size| *size <= PRIVATE_INVITE_PAYLOAD_CAPACITY)
        .and_then(|size| bytes.get(PRIVATE_INVITE_PAYLOAD_OFFSET..PRIVATE_INVITE_PAYLOAD_OFFSET + size));
    let blob_sha256 = sha256_hex(bytes);
    let payload_sha256 = payload.map_or_else(|| String::from("invalid"), sha256_hex);
    let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis());
    let safe_stage: String = stage.chars().map(|character| if character.is_ascii_alphanumeric() { character } else { '-' }).collect();
    let filename = format!("{timestamp_ms}-{sequence:06}-{safe_stage}-invite-{invitation_id}-outer-{outer_session_id}-expected-{expected_session_id}.bin");
    let mut written = false;
    if let Some(directory) = evidence_directory() {
        let _guard = PRIVATE_INVITE_EVIDENCE_WRITE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let binary_path = directory.join(&filename);
        written = fs::write(&binary_path, bytes).is_ok();
        let metadata = format!(
            "stage={stage}
sequence={sequence}
timestamp_ms={timestamp_ms}
mode={}
mode_id={}
invitation_id={invitation_id}
outer_session_id={outer_session_id}
expected_session_id={expected_session_id}
data_bytes={}
unknown1={:?}
checksum={:?}
payload_size={:?}
blob_fp=sha256:{}
payload_fp=sha256:{}
",
            PRIVATE_INVITE_MODE_LABEL,
            PRIVATE_INVITE_MODE_ID,
            bytes.len(),
            unknown1,
            checksum,
            payload_size,
            short_sha256_from_hex(&blob_sha256),
            short_sha256_from_hex(&payload_sha256),
        );
        let _ = fs::write(binary_path.with_extension("txt"), metadata);
        if let Ok(mut index) = OpenOptions::new().create(true).append(true).open(directory.join("evidence-index.tsv")) {
            let _ = writeln!(
                index,
                "{timestamp_ms}	{sequence}	{stage}	{}	{invitation_id}	{outer_session_id}	{expected_session_id}	{}	{:?}	{:?}	{:?}	{}	{}	{filename}",
                PRIVATE_INVITE_MODE_LABEL,
                bytes.len(),
                unknown1,
                checksum,
                payload_size,
                short_sha256_from_hex(&blob_sha256),
                short_sha256_from_hex(&payload_sha256),
            );
        }
    }
    info!(
        "PrivateInviteFullEvidence48 phase=blob stage={stage} sequence={sequence} mode={} mode_id={} invitation_id={invitation_id} outer_session_id={outer_session_id} expected_session_id={expected_session_id} data_bytes={} unknown1={unknown1:?} checksum={checksum:?} payload_size={payload_size:?} blob_fp=sha256:{} payload_fp=sha256:{} file={filename} written={written}",
        PRIVATE_INVITE_MODE_LABEL,
        PRIVATE_INVITE_MODE_ID,
        bytes.len(),
        short_sha256_from_hex(&blob_sha256),
        short_sha256_from_hex(&payload_sha256),
    );
}

#[cfg(not(feature = "diagnostic-evidence"))]
pub(crate) fn record_private_invite_blob(_stage: &str, _invitation_id: i64, _outer_session_id: u64, _expected_session_id: u64, _bytes: &[u8]) {}

#[cfg(feature = "diagnostic-evidence")]
pub(crate) fn record_uplay_set_game_session(identifier: u64, flags: usize, invite_only: bool, bytes: &[u8]) {
    record_private_invite_blob("uplay-set-game-session", -1, identifier, 0, bytes);
    info!(
        "PrivateInviteFullEvidence48 phase=uplay-capture identifier={identifier} flags={flags} invite_only={invite_only} mode={} mode_id={}",
        PRIVATE_INVITE_MODE_LABEL, PRIVATE_INVITE_MODE_ID,
    );
}

#[cfg(not(feature = "diagnostic-evidence"))]
pub(crate) fn record_uplay_set_game_session(_identifier: u64, _flags: usize, _invite_only: bool, _bytes: &[u8]) {}

#[cfg(feature = "diagnostic-evidence")]
fn trace_get_next_event_poll(returned_event: bool) {
    let total = GET_NEXT_EVENT_CALLS.fetch_add(1, Ordering::AcqRel) + 1;
    let mut state = GET_NEXT_EVENT_POLL_STATE
        .get_or_init(|| Mutex::new((Instant::now(), 0)))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.1 += 1;
    if returned_event || state.0.elapsed() >= Duration::from_secs(1) {
        info!(
            "PrivateInviteFullEvidence48 phase=get-next-event-poll calls_total={total} calls_window={} returned_event={returned_event} mode={} mode_id={}",
            state.1, PRIVATE_INVITE_MODE_LABEL, PRIVATE_INVITE_MODE_ID,
        );
        state.0 = Instant::now();
        state.1 = 0;
    }
}

#[cfg(not(feature = "diagnostic-evidence"))]
fn trace_get_next_event_poll(_returned_event: bool) {}

#[allow(dead_code)]
#[derive(Clone)]
pub enum Event {
    UserAccountSharing,
    // Kept for the overlay/event ABI, but the background updates thread must
    // not synthesize this event: retail owns its NetPresence refresh state.
    FriendsFriendListUpdated(Vec<String>),
    // Local retry token only; never a new roster change or a serialized event.
    FriendsFriendListRetry(Arc<()>),
    PartyMemberListChanged,
    FriendsGameInviteAccepted(u32, String, String),
    FriendsPrivateBlobInviteAccepted(PartyGameInvite),
    PartyGameInviteAccepted(PartyGameInvite),
}

struct EventLogSummary(String);

impl EventLogSummary {
    fn as_str(&self) -> &str {
        &self.0
    }
}

fn event_log_summary(event: &Event) -> EventLogSummary {
    let summary = match event {
        Event::UserAccountSharing => String::from("event_type=UserAccountSharing"),
        Event::FriendsFriendListUpdated(targets) => format!("event_type=FriendsFriendListUpdated target_count={}", targets.len()),
        Event::FriendsFriendListRetry(_) => String::from("event_type=FriendsFriendListRetry"),
        Event::PartyMemberListChanged => String::from("event_type=PartyMemberListChanged"),
        Event::FriendsGameInviteAccepted(invitation_id, _, _) => {
            format!("event_type=FriendsGameInviteAccepted invitation_id={invitation_id}")
        }
        Event::FriendsPrivateBlobInviteAccepted(invite) => format!(
            "event_type=FriendsPrivateBlobInviteAccepted kind={} invitation_id={} target_session_id={} data_bytes={} flags={} invite_only={} payload_fp=sha256:{} mode={}",
            invite.kind,
            invite.invitation_id,
            invite.game_session.id,
            invite.game_session.data.len(),
            invite.game_session.flags,
            invite.game_session.invite_only,
            short_sha256(&invite.game_session.data),
            PRIVATE_INVITE_MODE_LABEL,
        ),
        Event::PartyGameInviteAccepted(invite) => format!(
            "event_type=PartyGameInviteAccepted kind={} invitation_id={} target_session_id={} data_bytes={} flags={} invite_only={} payload_fp=sha256:{}",
            invite.kind,
            invite.invitation_id,
            invite.game_session.id,
            invite.game_session.data.len(),
            invite.game_session.flags,
            invite.game_session.invite_only,
            short_sha256(&invite.game_session.data),
        ),
    };
    EventLogSummary(summary)
}

pub static EVENTS: OnceLock<Mutex<mpsc::Receiver<Event>>> = OnceLock::new();
static EVENT_SENDER: OnceLock<mpsc::Sender<Event>> = OnceLock::new();
static COMPACT_FRIEND_REFRESH_TARGETS: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
static COMPACT_FRIEND_REFRESH_RETRY: OnceLock<Mutex<FriendRefreshRetryState>> = OnceLock::new();
static FRIEND_ROSTER_MERGE_PATCH_ENABLED: AtomicBool = AtomicBool::new(false);
static FRIEND_REFRESH_COORDINATOR: OnceLock<Mutex<FriendRefreshCoordinator>> = OnceLock::new();

const FRIEND_ROSTER_CLEAR_CALL: usize = 0x0089_98DF;
const FRIEND_ROSTER_CLEAR_ORIGINAL: [u8; 5] = [0xE8, 0x4C, 0x3E, 0xFA, 0xFF];
const FRIEND_ROSTER_CLEAR_BYPASS: [u8; 5] = [0x90; 5];
const FRIEND_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(4);
const FRIEND_REFRESH_RETRY_LIMIT: u8 = 15;

#[derive(Default)]
struct FriendRefreshRetryState {
    epoch: Arc<()>,
    last_attempt: Option<std::time::Instant>,
    attempts: u8,
}

impl FriendRefreshRetryState {
    fn arm(&mut self, now: std::time::Instant) {
        self.epoch = Arc::new(());
        self.last_attempt = Some(now);
        self.attempts = 0;
    }

    fn acknowledge(&mut self) {
        *self = Self::default();
    }

    fn take_due_attempt(&mut self, now: std::time::Instant) -> Option<u8> {
        if self.last_attempt.is_none() || self.attempts >= FRIEND_REFRESH_RETRY_LIMIT
            || self
                .last_attempt
                .is_some_and(|last_attempt| now.saturating_duration_since(last_attempt) < FRIEND_REFRESH_RETRY_INTERVAL)
        {
            return None;
        }
        self.attempts = self.attempts.saturating_add(1);
        self.last_attempt = Some(now);
        Some(self.attempts)
    }
}

pub(crate) fn event_sender() -> mpsc::Sender<Event> {
    EVENT_SENDER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            EVENTS.set(Mutex::new(receiver)).unwrap_or_else(|_| panic!("Uplay event receiver was initialized twice"));
            sender
        })
        .clone()
}

type FriendPresenceSnapshot = BTreeMap<String, (String, bool)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FriendSnapshotChange {
    Roster,
    Presence,
}

#[derive(Debug, PartialEq, Eq)]
struct FriendRefreshUpdate {
    change: FriendSnapshotChange,
    targets: Vec<String>,
}

#[derive(Default)]
struct FriendRefreshCoordinator {
    applied: Option<FriendPresenceSnapshot>,
    candidate: Option<(FriendPresenceSnapshot, u8)>,
}

impl FriendRefreshCoordinator {
    fn snapshot(friends: impl IntoIterator<Item = crate::api::Friend>) -> FriendPresenceSnapshot {
        friends.into_iter().map(|friend| (friend.id, (friend.username, friend.is_online))).collect()
    }

    fn seed(&mut self, friends: Vec<crate::api::Friend>) {
        let snapshot = Self::snapshot(friends);
        // The native startup roster is already being consumed by the retail
        // presence controller. Only confirmed changes observed after this
        // baseline are published, and each change is published once: repeating
        // an unchanged refresh can leave NetPresence waiting in the lookup
        // state and starve a subsequent invitation.
        self.applied = Some(snapshot);
        self.candidate = None;
    }

    fn observe(&mut self, friends: Vec<crate::api::Friend>) -> Option<FriendRefreshUpdate> {
        let next = Self::snapshot(friends);

        let Some(applied) = self.applied.as_ref() else {
            self.applied = Some(next);
            return None;
        };
        if applied == &next {
            self.candidate = None;
            return None;
        }

        match self.candidate.as_mut() {
            Some((candidate, confirmations)) if candidate == &next => {
                *confirmations = confirmations.saturating_add(1);
                if *confirmations < 2 {
                    return None;
                }
            }
            _ => {
                self.candidate = Some((next, 1));
                return None;
            }
        }

        let (confirmed, _) = self.candidate.take().expect("confirmed friend snapshot must exist");
        let change = if applied.len() != confirmed.len()
            || applied
                .iter()
                .any(|(id, (username, _))| confirmed.get(id).is_none_or(|(next_username, _)| next_username != username))
        {
            FriendSnapshotChange::Roster
        } else {
            FriendSnapshotChange::Presence
        };
        let targets = confirmed
            .iter()
            .filter_map(|(id, state)| (applied.get(id) != Some(state)).then(|| id.clone()))
            .collect::<Vec<_>>();
        self.applied = Some(confirmed);
        Some(FriendRefreshUpdate { change, targets })
    }
}

fn friend_refresh_coordinator() -> &'static Mutex<FriendRefreshCoordinator> {
    FRIEND_REFRESH_COORDINATOR.get_or_init(|| Mutex::new(FriendRefreshCoordinator::default()))
}

pub(super) fn seed_friend_refresh_snapshot(friends: &[crate::api::Friend]) {
    friend_refresh_coordinator().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).seed(friends.to_vec());
}

fn observe_friend_refresh(friends: Vec<crate::api::Friend>) -> Option<FriendRefreshUpdate> {
    friend_refresh_coordinator().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).observe(friends)
}

fn spawn_friend_refresh_thread() {
    let Some(claim) = startup::claim_friend_refresh() else {
        return;
    };
    if !enable_friend_roster_merge_patch() {
        warn!("FriendPresenceRefreshUnavailable reason=roster-merge-patch-failed");
        return;
    }
    let game_event_tx = event_sender();
    if let Err(error) = std::thread::Builder::new()
        .name(String::from("friends-refresh-thread"))
        .spawn(move || {
            let _claim = claim;
            let runtime = match crate::api::runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    warn!("FriendPresenceRefreshUnavailable error={error}");
                    return;
                }
            };
            runtime.block_on(async {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    match crate::api::list_friends_async().await {
                        Ok(friends) => {
                            if let Some(update) = observe_friend_refresh(friends) {
                                info!(
                                    "FriendSnapshotChanged change={:?} refresh_scope=change-triggered-full-roster roster_behavior=merge target_count={}",
                                    update.change,
                                    update.targets.len()
                                );
                                if game_event_tx.send(Event::FriendsFriendListUpdated(update.targets)).is_err() {
                                    warn!("FriendSnapshotChanged event queue is unavailable");
                                    return;
                                }
                            }
                        }
                        Err(error) => warn!("FriendPresenceRefreshFailed error={error}"),
                    }
                    retry_pending_friend_refresh_if_due();
                }
            });
        })
    {
        warn!("FriendPresenceRefreshUnavailable thread_error={error}");
    }
}

fn enable_friend_roster_merge_patch() -> bool {
    if FRIEND_ROSTER_MERGE_PATCH_ENABLED.load(Ordering::Acquire) {
        return true;
    }
    let target = FRIEND_ROSTER_CLEAR_CALL as *mut u8;
    let current = unsafe { std::slice::from_raw_parts(target, FRIEND_ROSTER_CLEAR_ORIGINAL.len()) };
    if current == FRIEND_ROSTER_CLEAR_BYPASS {
        FRIEND_ROSTER_MERGE_PATCH_ENABLED.store(true, Ordering::Release);
        return true;
    }
    if current != FRIEND_ROSTER_CLEAR_ORIGINAL {
        warn!("FriendRosterMergePatch result=unexpected-bytes");
        return false;
    }
    unsafe { crate::writemem(target, &FRIEND_ROSTER_CLEAR_BYPASS) };
    let enabled = unsafe { std::slice::from_raw_parts(target, FRIEND_ROSTER_CLEAR_BYPASS.len()) } == FRIEND_ROSTER_CLEAR_BYPASS;
    FRIEND_ROSTER_MERGE_PATCH_ENABLED.store(enabled, Ordering::Release);
    if enabled {
        info!("FriendRosterMergePatch enabled=true behavior=preserve-existing-and-upsert-refresh");
    }
    enabled
}

fn arm_compact_friend_refresh(targets: Vec<String>) {
    let mut pending = COMPACT_FRIEND_REFRESH_TARGETS
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let was_empty = pending.is_empty();
    pending.extend(targets);
    if was_empty && !pending.is_empty() {
        COMPACT_FRIEND_REFRESH_RETRY
            .get_or_init(|| Mutex::new(FriendRefreshRetryState::default()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .arm(std::time::Instant::now());
    }
}

pub(super) fn take_compact_friend_refresh() -> Option<BTreeSet<String>> {
    let mut targets = COMPACT_FRIEND_REFRESH_TARGETS
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if targets.is_empty() {
        None
    } else {
        let targets = std::mem::take(&mut *targets);
        COMPACT_FRIEND_REFRESH_RETRY
            .get_or_init(|| Mutex::new(FriendRefreshRetryState::default()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .acknowledge();
        Some(targets)
    }
}

fn retry_pending_friend_refresh_if_due() {
    let targets = COMPACT_FRIEND_REFRESH_TARGETS
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    #[cfg(test)]
    friend_refresh_retry_tests::pause_after_snapshot(&targets);
    if targets.is_empty() {
        return;
    }

    let now = std::time::Instant::now();
    let mut retry = COMPACT_FRIEND_REFRESH_RETRY
        .get_or_init(|| Mutex::new(FriendRefreshRetryState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let attempt = retry.take_due_attempt(now);
    let Some(attempt) = attempt else {
        return;
    };
    let epoch = Arc::clone(&retry.epoch);
    drop(retry);
    #[cfg(test)]
    friend_refresh_retry_tests::pause_after_due(&targets);

    info!(
        "FriendSnapshotRefreshRetry reason=pending-acknowledgement attempt={attempt} target_count={} acknowledgement=pending",
        targets.len()
    );
    if event_sender().send(Event::FriendsFriendListRetry(epoch)).is_err() {
        warn!("FriendSnapshotRefreshRetry event queue is unavailable");
    }
}

// The queue can outlive an acknowledgement (or a later refresh). Retry tokens
// may request another read, but must never re-arm an old target snapshot.
fn current_friend_refresh_retry(epoch: &Arc<()>) -> bool {
    let retry = COMPACT_FRIEND_REFRESH_RETRY
        .get_or_init(|| Mutex::new(FriendRefreshRetryState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    retry.last_attempt.is_some() && Arc::ptr_eq(&retry.epoch, epoch)
}

fn should_reconnect_after_poll_failures(failures: u32) -> bool {
    failures > 0 && (failures == 1 || failures % 10 == 0)
}

fn observe_event_poll<T, E>(failures: &mut u32, result: &std::result::Result<T, E>) -> bool {
    if result.is_ok() {
        *failures = 0;
    } else {
        *failures = failures.saturating_add(1);
    }
    should_reconnect_after_poll_failures(*failures)
}

#[cfg(test)]
mod polling_reconnect_tests {
    use super::observe_event_poll;
    use super::should_reconnect_after_poll_failures;

    #[test]
    fn successful_poll_never_triggers_reconnect() {
        assert!(!should_reconnect_after_poll_failures(0));
    }

    #[test]
    fn reconnects_on_first_and_periodic_failures() {
        assert!(should_reconnect_after_poll_failures(1));
        assert!(!should_reconnect_after_poll_failures(2));
        assert!(should_reconnect_after_poll_failures(10));
    }

    #[test]
    fn successful_empty_poll_clears_failure_state() {
        let mut failures = 1;
        let result: Result<Option<()>, ()> = Ok(None);
        assert!(!observe_event_poll(&mut failures, &result));
        assert_eq!(failures, 0);
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn successful_invite_poll_clears_failure_state_without_losing_the_invite() {
        let mut failures = 3;
        let result: Result<Option<u32>, ()> = Ok(Some(42));
        assert!(!observe_event_poll(&mut failures, &result));
        assert_eq!(failures, 0);
        assert_eq!(result.unwrap(), Some(42));
    }

    #[test]
    fn consecutive_errors_remain_failed_and_success_rearms_first_error_reconnect() {
        let mut failures = 0;
        assert!(observe_event_poll(&mut failures, &Err::<(), _>(())));
        assert!(!observe_event_poll(&mut failures, &Err::<(), _>(())));
        assert_eq!(failures, 2);
        assert!(!observe_event_poll(&mut failures, &Ok::<_, ()>(())));
        assert_eq!(failures, 0);
        assert!(observe_event_poll(&mut failures, &Err::<(), _>(())));
    }
}

pub(crate) fn acknowledge_private_invite_match_target(expected_session_id: u64) {
    party::acknowledge_private_invite_match_target(expected_session_id);
}

pub(crate) fn arm_party_presence_proof(kind: i32, invitation_id: i64, expected_session_id: u64, sender_id: &str) {
    party::arm_party_presence_proof(kind, invitation_id, expected_session_id, sender_id);
}

pub(crate) fn take_private_invite_match_target() -> Option<u64> {
    party::take_private_invite_match_target()
}

fn copy_account_id(target: &mut [u8], account_id: &str) {
    let range = PRIVATE_INVITE_ACCOUNT_OFFSET..PRIVATE_INVITE_ACCOUNT_OFFSET + PRIVATE_INVITE_ACCOUNT_BYTES;
    target[range.clone()].fill(0);
    let account = account_id.as_bytes();
    let length = account.len().min(PRIVATE_INVITE_ACCOUNT_BYTES - 1);
    target[range.start..range.start + length].copy_from_slice(&account[..length]);
}

fn build_private_friend_blob(account_id: &str) -> [u8; PRIVATE_INVITE_BLOB_BYTES] {
    let mut output = [0u8; PRIVATE_INVITE_BLOB_BYTES];
    output[..4].copy_from_slice(&1u32.to_le_bytes());
    copy_account_id(&mut output, account_id);
    output
}

unsafe fn return_standard_friend_event(event: *mut UplayEvent, invitation_id: u32, account_id: &str, outer_session_id: u64, expected_session_id: u64) {
    #![allow(static_mut_refs)]

    static mut IDENTITY: BlacklistFriendInviteData = BlacklistFriendInviteData {
        version: 1,
        reserved: 0,
        account_id: [0u8; BLACKLIST_ACCOUNT_ID_BYTES],
        remainder: [0u8; BLACKLIST_FRIEND_REMAINDER_BYTES],
    };
    static mut IDENTITY_REF: BlacklistFriendIdentityRef = BlacklistFriendIdentityRef {
        reserved: [0usize; 2],
        identity: std::ptr::addr_of!(IDENTITY),
        data_bytes: BLACKLIST_FRIEND_INVITE_DATA_BYTES,
    };
    static mut FRIEND_ACCEPTED: BlacklistFriendAccepted = BlacklistFriendAccepted {
        identity: std::ptr::addr_of!(IDENTITY_REF),
    };

    let bytes = build_private_friend_blob(account_id);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), std::ptr::addr_of_mut!(IDENTITY).cast::<u8>(), bytes.len());
    IDENTITY_REF.identity = std::ptr::addr_of!(IDENTITY);
    IDENTITY_REF.data_bytes = BLACKLIST_FRIEND_INVITE_DATA_BYTES;
    FRIEND_ACCEPTED.identity = std::ptr::addr_of!(IDENTITY_REF);
    (*event).event_type = UplayEventType::FriendsGameInviteAccepted;
    (*event).unknown = std::ptr::addr_of!(FRIEND_ACCEPTED) as usize;
    party::trace_invite_event_payload("FriendsGameInviteAccepted", invitation_id);
    record_private_invite_blob("game-event-returned", i64::from(invitation_id), outer_session_id, expected_session_id, &bytes);
    info!(
        "PrivateInviteFullEvidence48 phase=event-returned event=FriendsGameInviteAccepted invitation_id={invitation_id} mode={} mode_id={} standard_friends_abi=true data_bytes={} account_offset=8 outer_session_id={outer_session_id} expected_session_id={expected_session_id}",
        PRIVATE_INVITE_MODE_LABEL,
        PRIVATE_INVITE_MODE_ID,
        bytes.len(),
    );
}

unsafe fn into_friend_invite_accepted(event: *mut UplayEvent, invitation_id: u32, ubi_name: String) {
    return_standard_friend_event(event, invitation_id, &ubi_name, 0, 0);
}

unsafe fn into_friend_private_blob_invite_accepted(event: *mut UplayEvent, invite: &PartyGameInvite) {
    return_standard_friend_event(event, invite.invitation_id, &invite.sender_id, invite.game_session.id, invite.game_session.id);
}

pub(crate) const MAX_PARTY_INVITE_BYTES: usize = 16 * 1024;

unsafe fn into_party_invite_accepted(event: *mut UplayEvent, invite: &PartyGameInvite) -> bool {
    #![allow(static_mut_refs)]

    static mut SESSION_BYTES: [u8; MAX_PARTY_INVITE_BYTES] = [0; MAX_PARTY_INVITE_BYTES];
    static mut GAME_SESSION: UplayGameSession = UplayGameSession {
        id: 0,
        data: UplayDataBlob {
            data: std::ptr::null(),
            num_bytes: 0,
        },
    };
    static mut PARTY_ACCEPTED: BlacklistPartyAccepted = BlacklistPartyAccepted {
        invitation_id: 0,
        game_session: std::ptr::addr_of!(GAME_SESSION),
    };

    let size = invite.game_session.data.len();
    if size == 0 || size > MAX_PARTY_INVITE_BYTES {
        warn!(
            "PartyGameInvitePayloadRejected invitation_id={} game_session_id={} data_bytes={size}",
            invite.invitation_id, invite.game_session.id
        );
        return false;
    }
    SESSION_BYTES[..size].copy_from_slice(&invite.game_session.data);
    GAME_SESSION.id = invite.game_session.id;
    GAME_SESSION.data.data = SESSION_BYTES.as_ptr().cast::<c_void>();
    GAME_SESSION.data.num_bytes = size as u32;
    PARTY_ACCEPTED.invitation_id = invite.invitation_id;
    PARTY_ACCEPTED.game_session = std::ptr::addr_of!(GAME_SESSION);

    (*event).event_type = UplayEventType::PartyGameInviteAccepted;
    (*event).unknown = std::ptr::addr_of!(PARTY_ACCEPTED) as usize;
    party::trace_invite_event_payload("PartyGameInviteAccepted", invite.invitation_id);
    info!(
        "BlacklistPartyAcceptedAbiBuilt invitation_id={} kind={} game_session_id={} data_bytes={} game_session_offset=4 flags={} invite_only={}",
        invite.invitation_id, invite.kind, invite.game_session.id, size, invite.game_session.flags, invite.game_session.invite_only,
    );
    true
}

#[cfg(test)]
mod blacklist_uplay_abi_tests {
    use super::*;

    #[test]
    fn friends_invite_data_matches_retail_blacklist() {
        assert_eq!(std::mem::size_of::<BlacklistFriendInviteData>(), 0x1f0);
        assert_eq!(std::mem::offset_of!(BlacklistFriendInviteData, version), 0);
        assert_eq!(std::mem::offset_of!(BlacklistFriendInviteData, account_id), 8);
    }

    #[test]
    fn private_invite_blob_uses_only_the_retail_baseline_layout() {
        let baseline = build_private_friend_blob("sender");
        assert_eq!(read_u32_le(&baseline, 0), Some(1));
        assert_eq!(read_u32_le(&baseline, 4), Some(0));
        assert_eq!(read_u32_le(&baseline, PRIVATE_INVITE_PAYLOAD_SIZE_OFFSET), Some(0));
        assert_eq!(blob_account_id(&baseline), "sender");
    }

    #[test]
    fn party_accepted_matches_retail_blacklist_x86() {
        #[cfg(target_pointer_width = "32")]
        {
            assert_eq!(std::mem::size_of::<BlacklistPartyAccepted>(), 8);
            assert_eq!(std::mem::offset_of!(BlacklistPartyAccepted, invitation_id), 0);
            assert_eq!(std::mem::offset_of!(BlacklistPartyAccepted, game_session), 4);
            assert_eq!(std::mem::size_of::<BlacklistFriendIdentityRef>(), 16);
            assert_eq!(std::mem::offset_of!(BlacklistFriendIdentityRef, identity), 8);
            assert_eq!(std::mem::offset_of!(BlacklistFriendIdentityRef, data_bytes), 12);
        }
    }

    #[test]
    fn event_log_summary_keeps_join_evidence_without_identity_or_raw_session_data() {
        let sender_id = "11111111-2222-3333-4444-555555555555";
        let sender_name = "SCBL_FIXED_FAKE_DISPLAY_NAME";
        let data = (0..496).map(|index| (index % 251) as u8).collect::<Vec<_>>();
        let invite = PartyGameInvite {
            invitation_id: 17,
            kind: 3,
            sender_id: String::from(sender_id),
            sender_name: String::from(sender_name),
            game_session: server_api::misc::UplayGameSession {
                id: 460,
                data,
                flags: 3,
                invite_only: false,
            },
        };

        let party = event_log_summary(&Event::PartyGameInviteAccepted(invite.clone()));
        assert_eq!(
            party.as_str(),
            "event_type=PartyGameInviteAccepted kind=3 invitation_id=17 target_session_id=460 data_bytes=496 flags=3 invite_only=false payload_fp=sha256:8cbbbb3a02f0"
        );

        let private = event_log_summary(&Event::FriendsPrivateBlobInviteAccepted(invite));
        assert!(private.as_str().contains("event_type=FriendsPrivateBlobInviteAccepted"));
        assert!(private.as_str().contains("target_session_id=460"));
        assert!(private.as_str().contains("data_bytes=496"));
        assert!(private.as_str().contains("payload_fp=sha256:8cbbbb3a02f0"));

        for output in [party.as_str(), private.as_str()] {
            assert!(!output.contains(sender_id));
            assert!(!output.contains(sender_name));
            assert!(!output.contains("data: ["));
            assert!(!output.contains("[0, 1, 2, 3"));
            assert!(!output.contains("PartyGameInvite {"));
            assert!(!output.contains("UplayGameSession {"));
        }
    }

    #[test]
    fn every_event_variant_has_a_structured_identity_free_summary() {
        let fake_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let fake_name = "SCBL_EVENT_FAKE_NAME";
        let events = [
            Event::UserAccountSharing,
            Event::FriendsFriendListUpdated(Vec::new()),
            Event::FriendsFriendListRetry(Arc::new(())),
            Event::PartyMemberListChanged,
            Event::FriendsGameInviteAccepted(19, String::from(fake_id), String::from(fake_name)),
        ];

        for event in &events {
            let summary = event_log_summary(event);
            assert!(summary.as_str().starts_with("event_type="));
            assert!(!summary.as_str().contains(fake_id));
            assert!(!summary.as_str().contains(fake_name));
        }
    }

    #[test]
    fn compact_friend_refresh_is_consumed_once() {
        let _ = take_compact_friend_refresh();
        arm_compact_friend_refresh(vec![String::from("friend-a"), String::from("friend-b")]);
        assert_eq!(
            take_compact_friend_refresh(),
            Some([String::from("friend-a"), String::from("friend-b")].into_iter().collect())
        );
        assert_eq!(take_compact_friend_refresh(), None);
    }

    #[test]
    fn friend_refresh_retry_is_bounded_and_stops_after_acknowledgement() {
        let start = std::time::Instant::now();
        let mut retry = FriendRefreshRetryState::default();
        retry.arm(start);
        assert_eq!(retry.take_due_attempt(start + FRIEND_REFRESH_RETRY_INTERVAL - Duration::from_millis(1)), None);
        for attempt in 1..=FRIEND_REFRESH_RETRY_LIMIT {
            assert_eq!(retry.take_due_attempt(start + FRIEND_REFRESH_RETRY_INTERVAL * u32::from(attempt)), Some(attempt));
        }
        assert_eq!(
            retry.take_due_attempt(start + FRIEND_REFRESH_RETRY_INTERVAL * u32::from(FRIEND_REFRESH_RETRY_LIMIT + 1)),
            None
        );
        retry.acknowledge();
        assert_eq!(retry.attempts, 0);
        assert!(retry.last_attempt.is_none());
        assert_eq!(retry.take_due_attempt(start + Duration::from_secs(120)), None);
    }

    #[test]
    fn sensitive_hook_log_patterns_do_not_reappear() {
        let sources = [
            include_str!("uplay_r1_loader.rs"),
            include_str!("uplay_r1_loader/party.rs"),
            include_str!("uplay_r1_loader/friends.rs"),
            include_str!("uplay_r1_loader/user.rs"),
            include_str!("overlay.rs"),
        ];
        let forbidden = [
            ["New event ", "{evt:?}"].concat(),
            ["format!(\"", "{evt:?}", "\")"].concat(),
            ["Returning friends: ", "{list:?}"].concat(),
            ["account_id=", "{account_id}"].concat(),
            ["sender_id=", "{sender_id}"].concat(),
            ["sender_id=", "{proof_sender_id}"].concat(),
            ["sender=", "{sender_name}"].concat(),
            ["guest_id=", "{}"].concat(),
        ];

        for source in sources {
            for pattern in &forbidden {
                assert!(!source.contains(pattern), "sensitive log pattern reappeared: {pattern}");
            }
        }
    }
}

#[cfg_attr(not(test), forwardable_export(log = false))]
unsafe extern "cdecl" fn UPLAY_GetNextEvent(event: *mut UplayEvent) -> bool {
    if event.is_null() || !event.is_aligned() {
        return false;
    }

    let evt = EVENTS
        .get()
        .map(Mutex::lock)
        .map(std::result::Result::unwrap)
        .as_deref()
        .and_then(|receiver| loop {
            let event = receiver.try_recv().ok()?;
            if let Event::FriendsFriendListRetry(epoch) = &event {
                if !current_friend_refresh_retry(epoch) {
                    continue;
                }
            }
            break Some(event);
        });

    if let Some(evt) = evt {
        let event_summary = event_log_summary(&evt);
        party::trace_event_dispatch("dequeued", &event_summary);
        // A Kind 1 private-room invite targets the match only. Do not synthesize
        // a two-member social Party: after leaving that match the game would
        // otherwise restore a Party that the backend never admitted us to.
        // Kind 2/3 Party events establish their Party state in their own arm.
        match evt {
            Event::UserAccountSharing => {
                (*event).event_type = UplayEventType::UserAccountSharing;
                (*event).unknown = 0;
            }
            Event::FriendsFriendListUpdated(targets) => {
                // Keep one pending acknowledgement per changed account. The
                // getter projects the current full roster, matching the input
                // shape the retail controller completes at startup without
                // reintroducing unchanged periodic refreshes.
                arm_compact_friend_refresh(targets);
                (*event).event_type = UplayEventType::FriendsFriendListUpdated;
                (*event).unknown = 0;
            }
            Event::FriendsFriendListRetry(_) => {
                // The pending refresh is already armed. Never resurrect it.
                (*event).event_type = UplayEventType::FriendsFriendListUpdated;
                (*event).unknown = 0;
            }
            Event::PartyMemberListChanged => {
                (*event).event_type = UplayEventType::PartyMemberListChanged;
                (*event).unknown = 0;
            }
            Event::FriendsGameInviteAccepted(invitation_id, user, _username) => {
                into_friend_invite_accepted(event, invitation_id, user);
            }
            Event::FriendsPrivateBlobInviteAccepted(invite) => {
                into_friend_private_blob_invite_accepted(event, &invite);
            }
            Event::PartyGameInviteAccepted(invite) => {
                if !into_party_invite_accepted(event, &invite) {
                    return false;
                }
                // Only a payload actually delivered to the native consumer
                // may establish invitation/Party state.
                party::observe_party_game_invite_accepted(invite.kind, invite.invitation_id, &invite.sender_id, &invite.sender_name);
            }
        }
        party::trace_event_dispatch("returned", &event_summary);
        trace_get_next_event_poll(true);
        true
    } else {
        trace_get_next_event_poll(false);
        false
    }
}

unsafe fn get_overlapped_operation_result(overlapped: *mut UplayOverlapped, result: *mut usize) -> bool {
    let overlapped = unsafe { overlapped.as_ref() };
    let result = unsafe { result.as_mut() };
    if let Some((overlapped, result)) = overlapped.filter(|o| o.is_completed != 0).zip(result) {
        *result = overlapped.result;
        true
    } else {
        false
    }
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_GetOverlappedOperationResult_(overlapped: *mut UplayOverlapped, result: *mut usize) -> bool {
    get_overlapped_operation_result(overlapped, result)
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_GetOverlappedOperationResult(overlapped: *mut UplayOverlapped, result: *mut usize) -> bool {
    get_overlapped_operation_result(overlapped, result)
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_HasOverlappedOperationCompleted(overlapped: *mut UplayOverlapped) -> bool {
    let overlapped = unsafe { overlapped.as_ref() };
    overlapped.is_some_and(|o| o.is_completed != 0)
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_Quit() -> bool {
    false
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_Release(ptr: *mut List) -> bool {
    if !ptr.is_null() {
        let list = *Box::from_raw(ptr);
        let list: UplayList = list.try_into().unwrap();
        drop(list);
    }
    true
}

#[forwardable_export(always_call)]
unsafe extern "cdecl" fn UPLAY_Startup(uplay_id: usize, game_version: usize, language_country_code_utf8: *const c_char) -> isize {
    crate::plugin_loader::ensure_builtin_audio_initialized();
    let language_code = if language_country_code_utf8.is_null() {
        ""
    } else {
        CStr::from_ptr(language_country_code_utf8).to_str().unwrap_or("")
    };
    crate::overlay::set_ui_language(language_code);
    info!(
        "Overlay language selected: game_language={:?}, chinese_ui={}",
        language_code,
        crate::overlay::is_chinese_ui()
    );
    let mut update_sink = None;
    if let Some(config) = crate::config::get() {
        if config.enable_overlay {
            if let Some(claim) = startup::claim_overlay() {
                info!("Initializing overlay");
                let (tx, rx) = crossbeam_channel::unbounded();
                update_sink = Some(tx);
                // needs to be done in a separate thread, otherwise it'll not work
                if let Err(error) = std::thread::Builder::new()
                    .name(String::from("overlay-thread"))
                    .spawn(move || {
                        let Some(engine) = crate::overlay::Engine::detect() else {
                            error!("Couldn't identify DX version");
                            return;
                        };
                        info!("Detected DX version {engine:?}");
                        // Hudhook initialization during the game's fullscreen transition can block
                        // the render thread. Keep this compatibility delay until a real render-ready
                        // signal is available; shortening it requires DX9 and DX11 fullscreen proof.
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        // Hudhook owns process-wide cells/native hooks even on partial failure.
                        // Retry only failures before entering its installation.
                        claim.keep_for_process();
                        if let Err(err) = crate::overlay::init(engine, rx) {
                            error!("Couldn't initialize overlay: {err}; restart required before retry");
                        } else {
                            info!("Overlay initialized");
                        }
                    }) {
                    warn!("Overlay thread unavailable: {error}");
                }
            }

        } else {
            info!("Overlay is disabled");
        }
    } else {
        warn!("config not loaded!");
    }
    api_updates::start(update_sink);
    spawn_friend_refresh_thread();
    0 // 0 = all good, 1 = error occured, 2 = ??? (), 3 = ??? (potentially offline mode)
}

#[forwardable_export(log = false)]
unsafe extern "cdecl" fn UPLAY_Update() -> bool {
    true
}

#[cfg(test)]
mod friend_refresh_tests {
    use super::*;

    fn friend(id: &str, username: &str, online: bool) -> crate::api::Friend {
        crate::api::Friend {
            id: id.into(),
            username: username.into(),
            is_online: online,
        }
    }

    #[test]
    fn initial_and_unchanged_snapshots_are_quiet() {
        let mut refresh = FriendRefreshCoordinator::default();
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", false)]), None);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", false)]), None);
    }

    #[test]
    fn later_online_change_is_coalesced_then_published_once() {
        let mut refresh = FriendRefreshCoordinator::default();
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", false)]), None);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), None);
        let expected = FriendRefreshUpdate {
            change: FriendSnapshotChange::Presence,
            targets: vec![String::from("existing")],
        };
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), Some(expected));
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), None);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), None);
    }

    #[test]
    fn game_delivered_roster_is_the_baseline_even_if_background_poll_starts_late() {
        let mut refresh = FriendRefreshCoordinator::default();
        refresh.seed(vec![friend("existing", "Existing", false)]);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), None);
        assert_eq!(
            refresh.observe(vec![friend("existing", "Existing", true)]),
            Some(FriendRefreshUpdate {
                change: FriendSnapshotChange::Presence,
                targets: vec![String::from("existing")],
            })
        );
    }

    #[test]
    fn native_online_roster_is_not_reasserted_during_startup() {
        let mut refresh = FriendRefreshCoordinator::default();
        refresh.seed(vec![friend("online", "Online", true), friend("offline", "Offline", false)]);
        assert_eq!(refresh.observe(vec![friend("online", "Online", true), friend("offline", "Offline", false)]), None);
        assert_eq!(refresh.observe(vec![friend("online", "Online", true), friend("offline", "Offline", false)]), None);
    }

    #[test]
    fn newly_registered_user_is_a_roster_refresh() {
        let mut refresh = FriendRefreshCoordinator::default();
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), None);
        let roster = vec![friend("existing", "Existing", true), friend("new", "New", true)];
        assert_eq!(refresh.observe(roster.clone()), None);
        assert_eq!(
            refresh.observe(roster),
            Some(FriendRefreshUpdate {
                change: FriendSnapshotChange::Roster,
                targets: vec![String::from("new")],
            })
        );
    }

    #[test]
    fn unstable_changes_are_not_published() {
        let mut refresh = FriendRefreshCoordinator::default();
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", false)]), None);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", true)]), None);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", false)]), None);
        assert_eq!(refresh.observe(vec![friend("existing", "Existing", false)]), None);
    }
}

#[cfg(test)]
#[path = "friend_refresh_retry_tests.rs"]
mod friend_refresh_retry_tests;
