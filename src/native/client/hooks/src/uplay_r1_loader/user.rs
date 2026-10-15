use std::ffi::c_void;
use std::ffi::CString;
use std::slice;

use hooks_proc::forwardable_export;
use sha2::Digest;
use sha2::Sha256;
use tracing::error;
use tracing::info;
use tracing::warn;

use super::List;
use super::UplayList;
use super::UplayOverlapped;

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_ClearGameSession() -> bool {
    crate::api::clear_game_session();
    info!("UplayGameSessionCaptureCleared");
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_GetAccountId(buffer: *mut u8) -> bool {
    if buffer.is_null() {
        return false;
    }
    *buffer = 0;
    let account_id = match CString::new(cfg.user.account_id.clone()) {
        Ok(account_id) => account_id,
        Err(e) => {
            error!("Couldn't convert account_id: {}!", e);
            return false;
        }
    };
    let account_id = account_id.as_bytes_with_nul();
    buffer.copy_from_nonoverlapping(account_id.as_ptr(), account_id.len());
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_GetCdKeys(cd_keys_list: *mut *mut List, overlapped: *mut UplayOverlapped) -> bool {
    if cd_keys_list.is_null() {
        return false;
    }
    *cd_keys_list = std::ptr::null_mut();
    // Reject the entire invalid output before transferring any C allocations.
    // Keys are opaque; never truncate or substitute their contents.
    if cfg.user.cd_keys.iter().any(|key| key.contains('\0')) {
        warn!("Native CD key list contains invalid text");
        return false;
    }
    let list = UplayList::CdKeys(cfg.user.cd_keys.clone());
    *cd_keys_list = Box::into_raw(Box::new(list.into()));

    if !overlapped.is_null() {
        (*overlapped).unk = 0;
        (*overlapped).is_completed = 1;
        (*overlapped).result = 0;
    }
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_GetEmail(out_email: *mut i8) -> bool {
    false
}

// Both supported retail callers reserve 64 bytes at EBP-0x50, with
// their security cookie at EBP-0x10. This export has no capacity argument.
const NATIVE_PASSWORD_BYTES: usize = 64;

fn native_password(value: &str) -> Option<CString> {
    if value.is_empty() || value.len() >= NATIVE_PASSWORD_BYTES { return None; }
    CString::new(value).ok()
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_GetPassword(buffer: *mut u8) -> bool {
    if buffer.is_null() { return false; }
    // A failed read must not leave a prior credential in the game's buffer.
    *buffer=0;
    let Some(password) = native_password(&cfg.user.password) else {
        error!("Native credential exceeds the retail buffer or has invalid encoding");
        return false;
    };
    if let Err(error)=crate::api::ensure_native_login() {
        warn!("NativeCredentialDeferred reason={error}");
        return false;
    }
    let password=password.as_bytes_with_nul();
    buffer.copy_from_nonoverlapping(password.as_ptr(),password.len());
    true
}

#[cfg(test)]
mod native_password_tests {
    use super::*;
    #[test]
    fn fixed_native_buffer_is_checked_without_changing_credentials() {
        for value in ["", "has\0nul", &"a".repeat(64), &"a".repeat(4096)] {
            assert!(native_password(value).is_none());
        }
        for value in ["a", &"a".repeat(63), "unchanged-fixture", "測試"] {
            assert_eq!(native_password(value).unwrap().as_bytes(), value.as_bytes());
        }
    }
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_GetUsername(buffer: *mut u8) -> bool {
    if buffer.is_null() {
        return false;
    }
    *buffer = 0;
    let username = match CString::new(cfg.user.username.clone()) {
        Ok(username) => username,
        Err(e) => {
            error!("Couldn't convert username: {}!", e);
            return false;
        }
    };
    let username = username.as_bytes_with_nul();
    buffer.copy_from_nonoverlapping(username.as_ptr(), username.len());
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_USER_IsConnected() -> bool {
    true
}

const MAX_CAPTURED_GAME_SESSION_DATA: usize = 16 * 1024;
const SESSION_ACCOUNT_ID_BYTES: usize = 0x80;
const SESSION_PAYLOAD_BYTES: usize = 0x164;

#[repr(C)]
#[derive(Clone, Copy)]
struct SessionData {
    unknown1: u32,
    checksum: u32,
    account_id: [u8; SESSION_ACCOUNT_ID_BYTES],
    some_data: [u8; SESSION_PAYLOAD_BYTES],
    some_data_size: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SessionDataWrapper {
    data: *const SessionData,
    size: u32,
}

struct CapturedSessionData {
    bytes: Vec<u8>,
    unknown1: u32,
    checksum: u32,
    payload_size: usize,
    blob_sha256: String,
    payload_sha256: String,
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

unsafe fn copy_game_session_wrapper(wrapper: *const SessionDataWrapper) -> Option<CapturedSessionData> {
    let wrapper = wrapper.as_ref()?;
    let size = usize::try_from(wrapper.size).ok()?;
    if size != std::mem::size_of::<SessionData>() || size > MAX_CAPTURED_GAME_SESSION_DATA {
        return None;
    }
    if wrapper.data.is_null() || !wrapper.data.is_aligned() {
        return None;
    }

    let session = std::ptr::read_unaligned(wrapper.data);
    let payload_size = usize::try_from(session.some_data_size).ok()?;
    if payload_size > session.some_data.len() {
        return None;
    }

    let bytes = slice::from_raw_parts(wrapper.data.cast::<u8>(), size).to_vec();
    Some(CapturedSessionData {
        blob_sha256: hex_digest(&bytes),
        payload_sha256: hex_digest(&session.some_data[..payload_size]),
        bytes,
        unknown1: session.unknown1,
        checksum: session.checksum,
        payload_size,
    })
}

#[forwardable_export(always_call)]
unsafe extern "cdecl" fn UPLAY_USER_SetGameSession(game_session_identifier: *mut c_void, flags: usize, session_data: *const SessionDataWrapper, invite_only: bool) -> bool {
    if crate::hooks::is_modded() {
        crate::show_msgbox("Fatal error. Game modified", "MOD");
        std::process::exit(1);
    }

    let identifier = game_session_identifier as usize as u64;
    let Some(captured) = copy_game_session_wrapper(session_data) else {
        warn!(
            "UplayGameSessionCaptureRejectedGameAbi identifier=0x{identifier:08x} flags={flags} \
             wrapper={session_data:p} invite_only={invite_only} expected_size={}",
            std::mem::size_of::<SessionData>()
        );
        return false;
    };

    info!(
        "UplayGameSessionCapturedGameAbi identifier=0x{identifier:08x} flags={flags} \
         invite_only={invite_only} data_bytes={} unknown1={} checksum={} payload_size={} \
         blob_fp=sha256:{} payload_fp=sha256:{}",
        captured.bytes.len(),
        captured.unknown1,
        captured.checksum,
        captured.payload_size,
        super::short_sha256_from_hex(&captured.blob_sha256),
        super::short_sha256_from_hex(&captured.payload_sha256),
    );
    super::party::trace_game_session_capture(
        identifier,
        flags,
        invite_only,
        captured.bytes.len(),
        captured.payload_size,
        &captured.blob_sha256,
        &captured.payload_sha256,
    );
    super::record_uplay_set_game_session(identifier, flags, invite_only, &captured.bytes);
    crate::api::publish_game_session(identifier, captured.bytes, flags as u32, invite_only);
    true
}

#[cfg(test)]
mod game_session_capture_tests {
    use super::*;

    #[test]
    fn blacklist_session_layout_is_the_expected_496_bytes() {
        assert_eq!(std::mem::size_of::<SessionData>(), 496);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(std::mem::size_of::<SessionDataWrapper>(), 8);
    }

    #[test]
    fn copies_game_specific_wrapper_and_rejects_invalid_sizes() {
        let mut session = SessionData {
            unknown1: 7,
            checksum: 11,
            account_id: [0; SESSION_ACCOUNT_ID_BYTES],
            some_data: [0; SESSION_PAYLOAD_BYTES],
            some_data_size: 4,
        };
        session.account_id[..5].copy_from_slice(b"host\0");
        session.some_data[..4].copy_from_slice(&[1, 2, 3, 4]);

        let valid = SessionDataWrapper {
            data: &session,
            size: std::mem::size_of::<SessionData>() as u32,
        };
        let captured = unsafe { copy_game_session_wrapper(&valid) }.expect("valid wrapper");
        assert_eq!(captured.bytes.len(), 496);
        assert_eq!(captured.payload_size, 4);

        let invalid = SessionDataWrapper { data: &session, size: 8 };
        assert!(unsafe { copy_game_session_wrapper(&invalid) }.is_none());
    }
}
