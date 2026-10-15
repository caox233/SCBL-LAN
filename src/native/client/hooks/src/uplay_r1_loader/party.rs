use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::c_char;
use std::ffi::c_void;
use std::ffi::CStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use hooks_proc::forwardable_export;
use tracing::info;

use super::types::List;
use super::types::UplayDataBlob;
use super::UplayOverlapped;

static PARTY_INITIALIZED: AtomicBool = AtomicBool::new(false);
static SOCIAL_PARTY_ACTIVE: AtomicBool = AtomicBool::new(false);
static GAME_INVITE_ACTIVE: AtomicBool = AtomicBool::new(false);
static GAME_INVITE_ACCEPTED: AtomicBool = AtomicBool::new(false);
static TRACE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const MEMBER_FLAG_LEADER: u32 = 1;
const MEMBER_FLAG_LOCAL: u32 = 2;
const MAX_TEXT: usize = 128;
const MAX_PARTY_MEMBERS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PartyMemberState {
    account_id: String,
    username: String,
    flags: u32,
}

static PARTY_MEMBERS: OnceLock<Mutex<Vec<PartyMemberState>>> = OnceLock::new();

// PartyPresenceProof47: diagnostic context only. It never participates in a
// return value or invite-state decision.
#[derive(Clone, Debug, Default)]
struct PartyPresenceProofContext {
    kind: i32,
    invitation_id: i64,
    expected_session_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PartyPresenceProofSignature {
    initialized: bool,
    social_party_active: bool,
    game_invite_active: bool,
    game_invite_accepted: bool,
    members: usize,
    kind: i32,
    invitation_id: i64,
    expected_session_id: u64,
    result: i64,
    returned_count: usize,
    native_frames: [usize; 4],
}

#[derive(Clone, Copy, Debug, Default)]
struct PartyPresenceProofLast {
    calls: u64,
    last: Option<PartyPresenceProofSignature>,
}

static PARTY_PRESENCE_PROOF_CONTEXT: OnceLock<Mutex<PartyPresenceProofContext>> = OnceLock::new();
static PARTY_PRESENCE_PROOF_LAST: OnceLock<Mutex<HashMap<&'static str, PartyPresenceProofLast>>> = OnceLock::new();
static PARTY_PRESENCE_PROOF_SEQUENCE: AtomicU64 = AtomicU64::new(0);
// One-shot handoff to StateAcceptInvite::Enter.  This is separate from the
// persistent .47 diagnostic context so later proof logs keep their evidence.
static PRIVATE_INVITE_MATCH_TARGET: AtomicU64 = AtomicU64::new(0);

fn invite_kind_uses_match_controller_bridge(kind: i32) -> bool {
    kind == 1
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(frames_to_skip: u32, frames_to_capture: u32, back_trace: *mut *mut c_void, back_trace_hash: *mut u32) -> u16;
}

pub(super) fn arm_party_presence_proof(kind: i32, invitation_id: i64, expected_session_id: u64, _sender_id: &str) {
    PRIVATE_INVITE_MATCH_TARGET.store(if invite_kind_uses_match_controller_bridge(kind) { expected_session_id } else { 0 }, Ordering::Release);
    *PARTY_PRESENCE_PROOF_CONTEXT
        .get_or_init(|| Mutex::new(PartyPresenceProofContext::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = PartyPresenceProofContext {
        kind,
        invitation_id,
        expected_session_id,
    };
    trace_party(&format!(
        "PartyPresenceProof47 phase=armed kind={kind} invitation_id={invitation_id} expected_session_id={expected_session_id} behavior_changed=false"
    ));
}

fn party_presence_proof_context() -> PartyPresenceProofContext {
    PARTY_PRESENCE_PROOF_CONTEXT
        .get_or_init(|| Mutex::new(PartyPresenceProofContext::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub(super) fn take_private_invite_match_target() -> Option<u64> {
    let expected_session_id = PRIVATE_INVITE_MATCH_TARGET.load(Ordering::Acquire);
    (expected_session_id != 0).then_some(expected_session_id)
}

pub(super) fn acknowledge_private_invite_match_target(expected_session_id: u64) {
    let _ = PRIVATE_INVITE_MATCH_TARGET.compare_exchange(expected_session_id, 0, Ordering::AcqRel, Ordering::Acquire);
}

#[inline(never)]
fn trace_party_presence_proof(api: &'static str, result: i64, account_id: &str, returned_count: usize) {
    let initialized = PARTY_INITIALIZED.load(Ordering::Acquire);
    let social_party_active = SOCIAL_PARTY_ACTIVE.load(Ordering::Acquire);
    let game_invite_active = GAME_INVITE_ACTIVE.load(Ordering::Acquire);
    let game_invite_accepted = GAME_INVITE_ACCEPTED.load(Ordering::Acquire);
    let members = party_members().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len();
    let context = party_presence_proof_context();
    let mut raw_frames = [std::ptr::null_mut::<c_void>(); 4];
    let captured = unsafe {
        // Skip RtlCaptureStackBackTrace and this non-inlined proof helper.
        // frames[0] should be the exported UPLAY function and frames[1+] its
        // native game-side callers.
        RtlCaptureStackBackTrace(2, 4, raw_frames.as_mut_ptr(), std::ptr::null_mut())
    } as usize;
    let mut native_frames = [0usize; 4];
    for (index, frame) in raw_frames.into_iter().take(captured.min(4)).enumerate() {
        native_frames[index] = frame as usize;
    }

    let signature = PartyPresenceProofSignature {
        initialized,
        social_party_active,
        game_invite_active,
        game_invite_accepted,
        members,
        kind: context.kind,
        invitation_id: context.invitation_id,
        expected_session_id: context.expected_session_id,
        result,
        returned_count,
        native_frames,
    };
    let sequence = PARTY_PRESENCE_PROOF_SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1;
    let (api_call, changed, should_log) = {
        let mut map = PARTY_PRESENCE_PROOF_LAST
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = map.entry(api).or_default();
        state.calls += 1;
        let changed = state.last != Some(signature);
        state.last = Some(signature);
        (state.calls, changed, state.calls <= 12 || changed || state.calls % 120 == 0)
    };
    if should_log {
        let invitation_only_party_presence = initialized && !social_party_active && game_invite_active && result != 0;
        let identity_present = !matches!(account_id, "<none>" | "<null>");
        trace_party(&format!(
            "PartyPresenceProof47 phase=query seq={sequence} api={api} api_call={api_call} changed={changed} result={result} identity_present={identity_present} returned_count={returned_count} initialized={initialized} social_party_active={social_party_active} game_invite_active={game_invite_active} game_invite_accepted={game_invite_accepted} members={members} invite_kind={} invitation_id={} expected_session_id={} invitation_only_party_presence={invitation_only_party_presence} export_frame=0x{:08x} caller1=0x{:08x} caller2=0x{:08x} caller3=0x{:08x} behavior_changed=false",
            context.kind,
            context.invitation_id,
            context.expected_session_id,
            native_frames[0],
            native_frames[1],
            native_frames[2],
            native_frames[3],
        ));
    }
}

fn trace_party_presence_transition(phase: &str, event: &str, invitation_id: u32) {
    let context = party_presence_proof_context();
    let initialized = PARTY_INITIALIZED.load(Ordering::Acquire);
    let social_party_active = SOCIAL_PARTY_ACTIVE.load(Ordering::Acquire);
    let game_invite_active = GAME_INVITE_ACTIVE.load(Ordering::Acquire);
    let game_invite_accepted = GAME_INVITE_ACCEPTED.load(Ordering::Acquire);
    let members = party_members().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len();
    trace_party(&format!(
        "PartyPresenceProof47 phase={phase} event={event} event_invitation_id={invitation_id} initialized={initialized} social_party_active={social_party_active} game_invite_active={game_invite_active} game_invite_accepted={game_invite_accepted} members={members} invite_kind={} invitation_id={} expected_session_id={} behavior_changed=false",
        context.kind,
        context.invitation_id,
        context.expected_session_id,
    ));
}

fn party_members() -> &'static Mutex<Vec<PartyMemberState>> {
    PARTY_MEMBERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn replace_party_members(members: Vec<PartyMemberState>) -> bool {
    let mut current = party_members().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *current == members {
        false
    } else {
        *current = members;
        true
    }
}

fn notify_party_member_list_changed(changed: bool) {
    if !changed {
        return;
    }
    if super::event_sender().send(super::Event::PartyMemberListChanged).is_err() {
        trace_party("authoritative_roster event=PartyMemberListChanged result=queue-unavailable");
    } else {
        trace_party("authoritative_roster event=PartyMemberListChanged result=queued");
    }
}

fn local_identity() -> Option<(String, String)> {
    crate::config::get().map(|config| (config.user.account_id.clone(), config.user.username.clone()))
}

fn set_receiver_party(sender_id: &str, sender_name: &str) {
    let mut members = vec![PartyMemberState {
        account_id: sender_id.to_string(),
        username: sender_name.to_string(),
        flags: MEMBER_FLAG_LEADER,
    }];
    if let Some((account_id, username)) = local_identity() {
        members.push(PartyMemberState {
            account_id,
            username,
            flags: MEMBER_FLAG_LOCAL,
        });
    }
    *party_members().lock().unwrap() = members;
}

fn set_local_leader_party(invitee_id: &str) {
    let Some((local_account_id, local_username)) = local_identity() else {
        return;
    };
    let invitee_name = crate::api::list_friends()
        .unwrap_or_default()
        .into_iter()
        .find(|friend| friend.id == invitee_id)
        .map_or_else(|| invitee_id.to_string(), |friend| friend.username);
    let mut members = party_members().lock().unwrap();
    for member in members.iter_mut() {
        member.flags &= !MEMBER_FLAG_LEADER;
    }
    if let Some(local) = members.iter_mut().find(|member| member.account_id == local_account_id) {
        local.username = local_username;
        local.flags = MEMBER_FLAG_LEADER | MEMBER_FLAG_LOCAL;
    } else {
        members.insert(
            0,
            PartyMemberState {
                account_id: local_account_id,
                username: local_username,
                flags: MEMBER_FLAG_LEADER | MEMBER_FLAG_LOCAL,
            },
        );
    }
    if let Some(invitee) = members.iter_mut().find(|member| member.account_id == invitee_id) {
        invitee.username = invitee_name;
        invitee.flags &= !(MEMBER_FLAG_LEADER | MEMBER_FLAG_LOCAL);
    } else if members.len() < MAX_PARTY_MEMBERS {
        members.push(PartyMemberState {
            account_id: invitee_id.to_string(),
            username: invitee_name,
            flags: 0,
        });
    }
}

fn project_authoritative_roster(roster: &server_api::misc::PartyRoster, local_account_id: &str) -> Option<Vec<PartyMemberState>> {
    let leader_id = roster.leader.as_ref()?.id.as_str();
    if leader_id.is_empty() || roster.members.is_empty() || roster.members.len() > MAX_PARTY_MEMBERS {
        return None;
    }
    let mut seen = HashSet::with_capacity(roster.members.len());
    let mut leader_present = false;
    let mut local_present = false;
    let mut projected = Vec::with_capacity(roster.members.len());
    for member in &roster.members {
        if member.id.is_empty() || !seen.insert(member.id.as_str()) {
            return None;
        }
        let mut flags = 0;
        if member.id == leader_id {
            flags |= MEMBER_FLAG_LEADER;
            leader_present = true;
        }
        if member.id == local_account_id {
            flags |= MEMBER_FLAG_LOCAL;
            local_present = true;
        }
        projected.push(PartyMemberState {
            account_id: member.id.clone(),
            username: member.username.clone(),
            flags,
        });
    }
    (leader_present && local_present).then_some(projected)
}

fn project_local_singleton(account_id: String, username: String) -> Vec<PartyMemberState> {
    vec![PartyMemberState {
        account_id,
        username,
        flags: MEMBER_FLAG_LEADER | MEMBER_FLAG_LOCAL,
    }]
}

pub(super) fn observe_authoritative_party_roster(roster: Option<&server_api::misc::PartyRoster>) {
    let Some(roster) = roster else {
        // No backend Party means the local player is back in a singleton
        // lobby. Clear a previously projected multi-member roster once no
        // invitation transition is active; otherwise the old host remains
        // locally "in the same party" forever after a successful migration.
        if !GAME_INVITE_ACTIVE.load(Ordering::Acquire) {
            let Some((account_id, username)) = local_identity() else {
                trace_party("authoritative_roster result=rejected reason=local_identity_unavailable");
                return;
            };
            PARTY_INITIALIZED.store(true, Ordering::Release);
            SOCIAL_PARTY_ACTIVE.store(false, Ordering::Release);
            let changed = replace_party_members(project_local_singleton(account_id, username));
            trace_party("authoritative_roster result=applied members=1 source=backend-absence");
            notify_party_member_list_changed(changed);
        }
        return;
    };
    let Some((local_account_id, _)) = local_identity() else {
        trace_party("authoritative_roster result=rejected reason=local_identity_unavailable");
        return;
    };
    let Some(members) = project_authoritative_roster(roster, &local_account_id) else {
        trace_party("authoritative_roster result=rejected reason=invalid_or_incomplete");
        return;
    };
    let member_count = members.len();
    PARTY_INITIALIZED.store(true, Ordering::Release);
    SOCIAL_PARTY_ACTIVE.store(member_count > 1, Ordering::Release);
    let changed = replace_party_members(members);
    trace_party(&format!("authoritative_roster result=applied members={member_count}"));
    notify_party_member_list_changed(changed);
}

fn party_presence(initialized: bool, social_party_active: bool, game_invite_active: bool) -> bool {
    initialized && (social_party_active || game_invite_active)
}

fn trace_path() -> Option<PathBuf> {
    std::env::var_os("SCBL_LAN_CLIENT_DATA_DIR")
        .map(PathBuf::from)
        .map(|path| path.join("logs").join("game").join("hooks-party-trace.log"))
}

fn trace_party(message: &str) {
    let sequence = TRACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_millis());
    let thread_id = format!("{:?}", std::thread::current().id());
    let line = format!("SCBL_DEBUG24 seq={sequence} unix_ms={unix_ms} thread={thread_id} SCBL_PARTY_API {message}");
    info!("{line}");
    let Some(path) = trace_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

pub(super) fn trace_invite_event_payload(event_type: &str, invitation_id: u32) {
    trace_party(&format!("payload event={event_type} invitation_id={invitation_id}"));
}

pub(super) fn trace_event_dispatch(stage: &str, event: &super::EventLogSummary) {
    trace_party(&format!("dispatch stage={stage} {}", event.as_str()));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trace_game_session_capture(identifier: u64, flags: usize, invite_only: bool, data_bytes: usize, payload_size: usize, blob_sha256: &str, payload_sha256: &str) {
    trace_party(&format!(
        "game_session_capture identifier=0x{identifier:08x} flags={flags} invite_only={invite_only} data_bytes={data_bytes} payload_size={payload_size} blob_fp=sha256:{} payload_fp=sha256:{}",
        super::short_sha256_from_hex(blob_sha256),
        super::short_sha256_from_hex(payload_sha256),
    ));
}

unsafe fn account_id_text(account_id: *const c_char) -> String {
    if account_id.is_null() {
        String::from("<null>")
    } else {
        CStr::from_ptr(account_id).to_string_lossy().into_owned()
    }
}

unsafe fn complete_success(overlapped: *mut UplayOverlapped) {
    if let Some(overlapped) = overlapped.as_mut() {
        overlapped.set_success();
    }
}

pub(super) fn observe_party_game_invite_accepted(kind: i32, invitation_id: u32, account_id: &str, username: &str) {
    trace_party_presence_transition("event-before", "PartyGameInviteAccepted", invitation_id);
    PARTY_INITIALIZED.store(true, Ordering::Release);
    GAME_INVITE_ACTIVE.store(true, Ordering::Release);
    GAME_INVITE_ACCEPTED.store(true, Ordering::Release);
    if party_invite_activates_social_party(kind) {
        SOCIAL_PARTY_ACTIVE.store(true, Ordering::Release);
        set_receiver_party(account_id, username);
    }
    trace_party_presence_transition("event-after", "PartyGameInviteAccepted", invitation_id);
}

fn party_invite_activates_social_party(kind: i32) -> bool {
    matches!(kind, 2 | 3)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PartyMemberRaw {
    account_id_utf8: *const c_char,
    nick_utf8: *const c_char,
    avatar_id: u32,
    data: UplayDataBlob,
    flags: u32,
    presence: *const c_void,
    party_host_if_guest: *const c_void,
}

const EMPTY_PARTY_MEMBER_RAW: PartyMemberRaw = PartyMemberRaw {
    account_id_utf8: std::ptr::null(),
    nick_utf8: std::ptr::null(),
    avatar_id: 0,
    data: UplayDataBlob {
        data: std::ptr::null(),
        num_bytes: 0,
    },
    flags: 0,
    presence: std::ptr::null(),
    party_host_if_guest: std::ptr::null(),
};

unsafe fn copy_text(target: &mut [u8; MAX_TEXT], value: &str) {
    target.fill(0);
    let bytes = value.as_bytes();
    let len = bytes.len().min(MAX_TEXT - 1);
    target[..len].copy_from_slice(&bytes[..len]);
}

unsafe fn write_member_list(out_member_list: *mut List) -> bool {
    #![allow(static_mut_refs)]
    if out_member_list.is_null() || !out_member_list.is_aligned() {
        return false;
    }
    let members = party_members().lock().unwrap().clone();
    if members.is_empty() {
        (*out_member_list).count = 0;
        (*out_member_list).list = std::ptr::null_mut();
        return true;
    }

    static mut IDS: [[u8; MAX_TEXT]; MAX_PARTY_MEMBERS] = [[0; MAX_TEXT]; MAX_PARTY_MEMBERS];
    static mut NAMES: [[u8; MAX_TEXT]; MAX_PARTY_MEMBERS] = [[0; MAX_TEXT]; MAX_PARTY_MEMBERS];
    static mut RAW: [PartyMemberRaw; MAX_PARTY_MEMBERS] = [EMPTY_PARTY_MEMBER_RAW; MAX_PARTY_MEMBERS];
    static mut PTRS: [*mut c_void; MAX_PARTY_MEMBERS] = [std::ptr::null_mut(); MAX_PARTY_MEMBERS];

    let count = members.len().min(MAX_PARTY_MEMBERS);
    for index in 0..count {
        copy_text(&mut IDS[index], &members[index].account_id);
        copy_text(&mut NAMES[index], &members[index].username);
        RAW[index].account_id_utf8 = IDS[index].as_ptr().cast::<c_char>();
        RAW[index].nick_utf8 = NAMES[index].as_ptr().cast::<c_char>();
        RAW[index].flags = members[index].flags;
        PTRS[index] = std::ptr::addr_of_mut!(RAW[index]).cast::<c_void>();
    }
    (*out_member_list).count = count;
    (*out_member_list).list = PTRS.as_mut_ptr();
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_DisablePartyMemberMenuItem(id: u32) -> bool {
    trace_party(&format!("call=DisablePartyMemberMenuItem id={id} result=true"));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_EnablePartyMemberMenuItem(id: u32, menu_item_mode: u32, filter: *const c_void) -> bool {
    trace_party(&format!("call=EnablePartyMemberMenuItem id={id} mode={menu_item_mode} filter={filter:p} result=true"));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_GetFullMemberList(out_member_list: *mut List) -> bool {
    let result = write_member_list(out_member_list);
    let returned_count = if result && !out_member_list.is_null() { (*out_member_list).count } else { 0 };
    trace_party_presence_proof("GetFullMemberList", i64::from(result), "<none>", returned_count);
    trace_party(&format!("call=GetFullMemberList out={out_member_list:p} result={result}"));
    result
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_GetId() -> i32 {
    let result = i32::from(SOCIAL_PARTY_ACTIVE.load(Ordering::Acquire));
    trace_party_presence_proof("GetId", i64::from(result), "<none>", 0);
    trace_party(&format!("call=GetId result={result}"));
    result
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_GetInGameMemberList(out_member_list: *mut List) -> bool {
    let result = write_member_list(out_member_list);
    let returned_count = if result && !out_member_list.is_null() { (*out_member_list).count } else { 0 };
    trace_party_presence_proof("GetInGameMemberList", i64::from(result), "<none>", returned_count);
    trace_party(&format!("call=GetInGameMemberList out={out_member_list:p} result={result}"));
    result
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_Init(flags: u32) -> bool {
    PARTY_INITIALIZED.store(true, Ordering::Release);
    trace_party(&format!("call=Init flags={flags} result=true"));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_InvitePartyToGame(overlapped: *mut UplayOverlapped) -> bool {
    PARTY_INITIALIZED.store(true, Ordering::Release);
    SOCIAL_PARTY_ACTIVE.store(true, Ordering::Release);
    complete_success(overlapped);
    trace_party(&format!(
        "call=InvitePartyToGame overlapped_null={} result=true completed_success=true",
        overlapped.is_null()
    ));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_InviteToParty(account_id: *const c_char, overlapped: *mut UplayOverlapped) -> bool {
    let account_id = account_id_text(account_id);
    if account_id == "<null>" {
        trace_party("call=InviteToParty identity_present=false result=false");
        return false;
    }
    PARTY_INITIALIZED.store(true, Ordering::Release);
    SOCIAL_PARTY_ACTIVE.store(true, Ordering::Release);
    set_local_leader_party(&account_id);
    complete_success(overlapped);
    trace_party(&format!(
        "call=InviteToParty identity_present=true overlapped_null={} result=true completed_success=true",
        overlapped.is_null()
    ));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_IsInParty(account_id: *const c_char) -> bool {
    let initialized = PARTY_INITIALIZED.load(Ordering::Acquire);
    let social_party_active = SOCIAL_PARTY_ACTIVE.load(Ordering::Acquire);
    let game_invite_active = GAME_INVITE_ACTIVE.load(Ordering::Acquire);
    let result = party_presence(initialized, social_party_active, game_invite_active);
    let account_id = account_id_text(account_id);
    trace_party_presence_proof("IsInParty", i64::from(result), &account_id, 0);
    trace_party(&format!(
        "call=IsInParty identity_present={} initialized={initialized} social_party_active={social_party_active} game_invite_active={game_invite_active} result={result}",
        account_id != "<null>",
    ));
    result
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_IsPartyLeader(account_id: *const c_char) -> bool {
    let account_id = account_id_text(account_id);
    let result = party_members()
        .lock()
        .unwrap()
        .iter()
        .any(|member| member.account_id == account_id && member.flags & MEMBER_FLAG_LEADER != 0);
    trace_party_presence_proof("IsPartyLeader", i64::from(result), &account_id, 0);
    trace_party(&format!("call=IsPartyLeader identity_present={} result={result}", account_id != "<null>"));
    result
}


// A rejected promotion is not a roster update. Resolve the target while the
// caller holds the roster lock, before clearing the current leader flag.
fn promote_party_member(members: &mut [PartyMemberState], account_id: &str) -> bool {
    if !members.iter().any(|member| member.account_id == account_id) {
        return false;
    }
    for member in members {
        if member.account_id == account_id {
            member.flags |= MEMBER_FLAG_LEADER;
        } else {
            member.flags &= !MEMBER_FLAG_LEADER;
        }
    }
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_PromoteToLeader(account_id: *const c_char, overlapped: *mut UplayOverlapped) -> bool {
    let account_id = account_id_text(account_id);
    let mut members = party_members().lock().unwrap();
    let found = promote_party_member(&mut members, &account_id);
    if found {
        complete_success(overlapped);
    }
    trace_party(&format!(
        "call=PromoteToLeader identity_present={} overlapped_null={} result={found}",
        account_id != "<null>",
        overlapped.is_null()
    ));
    found
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_RespondToGameInvite(invitation_id: u32, accept: bool) -> bool {
    trace_party_presence_transition("api-before", "RespondToGameInvite", invitation_id);
    PARTY_INITIALIZED.store(true, Ordering::Release);
    GAME_INVITE_ACTIVE.store(accept, Ordering::Release);
    GAME_INVITE_ACCEPTED.store(accept, Ordering::Release);
    trace_party_presence_transition("api-after", "RespondToGameInvite", invitation_id);
    trace_party(&format!("call=RespondToGameInvite invitation_id={invitation_id} accept={accept} result=true"));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_SetGuest(guest_id: *const c_char, overlapped: *mut UplayOverlapped) -> bool {
    complete_success(overlapped);
    trace_party(&format!(
        "call=SetGuest identity_present={} overlapped_null={} result=true",
        !guest_id.is_null(),
        overlapped.is_null()
    ));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_SetUserData(data_blob: *const UplayDataBlob) -> bool {
    trace_party(&format!("call=SetUserData data_blob={data_blob:p} result=true"));
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_PARTY_ShowGameInviteOverlayUI(invitation_id: u32) -> bool {
    trace_party(&format!("call=ShowGameInviteOverlayUI invitation_id={invitation_id} result=true"));
    true
}

#[cfg(test)]
mod tests {
    use super::invite_kind_uses_match_controller_bridge;
    use super::party_invite_activates_social_party;
    use super::party_presence;
    use super::project_authoritative_roster;
    use super::project_local_singleton;
    use super::replace_party_members;
    use super::UplayOverlapped;
    use super::MAX_PARTY_MEMBERS;
    use super::MEMBER_FLAG_LEADER;
    use super::MEMBER_FLAG_LOCAL;

    fn user(id: &str) -> server_api::users::User {
        server_api::users::User {
            id: id.into(),
            username: format!("user-{id}"),
            ips: Vec::new(),
        }
    }

    #[test]
    fn only_private_blob_invites_use_the_match_controller_bridge() {
        assert!(invite_kind_uses_match_controller_bridge(1));
        assert!(!invite_kind_uses_match_controller_bridge(2));
        assert!(!invite_kind_uses_match_controller_bridge(3));
        assert!(!invite_kind_uses_match_controller_bridge(4));
    }

    #[test]
    fn lobby_and_follow_party_events_activate_social_party_state() {
        assert!(!party_invite_activates_social_party(1));
        assert!(party_invite_activates_social_party(2));
        assert!(party_invite_activates_social_party(3));
        assert!(!party_invite_activates_social_party(4));
    }

    #[test]
    fn party_presence_requires_initialization_and_one_active_lane() {
        assert!(!party_presence(false, true, true));
        assert!(!party_presence(true, false, false));
        assert!(party_presence(true, true, false));
        assert!(party_presence(true, false, true));
    }

    #[test]
    fn authoritative_roster_projects_all_members_and_flags() {
        let roster = server_api::misc::PartyRoster {
            leader: Some(user("two")),
            members: vec![user("two"), user("three"), user("four")],
        };
        let projected = project_authoritative_roster(&roster, "three").unwrap();
        assert_eq!(projected.len(), 3);
        assert_eq!(projected[0].flags, MEMBER_FLAG_LEADER);
        assert_eq!(projected[1].flags, MEMBER_FLAG_LOCAL);
        assert_eq!(projected[2].flags, 0);
    }

    #[test]
    fn authoritative_roster_rejects_incomplete_duplicate_or_oversized_data() {
        let missing_local = server_api::misc::PartyRoster {
            leader: Some(user("one")),
            members: vec![user("one"), user("two")],
        };
        assert!(project_authoritative_roster(&missing_local, "three").is_none());

        let duplicate = server_api::misc::PartyRoster {
            leader: Some(user("one")),
            members: vec![user("one"), user("one")],
        };
        assert!(project_authoritative_roster(&duplicate, "one").is_none());

        let oversized = server_api::misc::PartyRoster {
            leader: Some(user("0")),
            members: (0..=MAX_PARTY_MEMBERS).map(|index| user(&index.to_string())).collect(),
        };
        assert!(project_authoritative_roster(&oversized, "0").is_none());
    }

    #[test]
    fn backend_party_absence_projects_a_local_leader_singleton() {
        let projected = project_local_singleton("one".into(), "user-one".into());
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].account_id, "one");
        assert_eq!(projected[0].username, "user-one");
        assert_eq!(projected[0].flags, MEMBER_FLAG_LEADER | MEMBER_FLAG_LOCAL);
    }

    #[test]
    fn party_projection_change_is_reported_once_per_distinct_roster() {
        let first = project_local_singleton("one".into(), "user-one".into());
        assert!(replace_party_members(first.clone()));
        assert!(!replace_party_members(first));

        let second = project_local_singleton("two".into(), "user-two".into());
        assert!(replace_party_members(second));
    }

    #[test]
    fn overlapped_success_is_completed_with_zero_result() {
        let mut overlapped = UplayOverlapped {
            unk: 0,
            is_completed: 0,
            result: usize::MAX,
        };
        unsafe { super::complete_success(&mut overlapped) };
        assert_eq!(overlapped.is_completed, 1);
        assert_eq!(overlapped.result, 0);
    }
}

#[cfg(test)]
mod party_promotion_tests {
    use super::*;

    fn roster() -> Vec<PartyMemberState> {
        vec![
            PartyMemberState { account_id: "a".into(), username: "A".into(), flags: MEMBER_FLAG_LEADER | MEMBER_FLAG_LOCAL },
            PartyMemberState { account_id: "b".into(), username: "B".into(), flags: 8 },
        ]
    }

    #[test]
    fn rejected_target_preserves_every_roster_field() {
        for target in ["", "absent", "<null>", "A"] {
            let mut members = roster();
            let before = members.clone();
            assert!(!promote_party_member(&mut members, target));
            assert_eq!(members, before);
        }
    }

    #[test]
    fn current_leader_promotion_is_idempotent() {
        let mut members = roster();
        let before = members.clone();
        assert!(promote_party_member(&mut members, "a"));
        assert!(promote_party_member(&mut members, "a"));
        assert_eq!(members, before);
    }

    #[test]
    fn valid_promotion_only_transfers_leader_flag() {
        let mut members = roster();
        let mut expected = members.clone();
        expected[0].flags = MEMBER_FLAG_LOCAL;
        expected[1].flags = 8 | MEMBER_FLAG_LEADER;
        assert!(promote_party_member(&mut members, "b"));
        assert_eq!(members, expected);
    }

    #[test]
    fn empty_roster_rejects_without_inventing_members() {
        let mut members = Vec::new();
        assert!(!promote_party_member(&mut members, "b"));
        assert!(members.is_empty());
    }
}
