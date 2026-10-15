use std::ffi::c_char;
use std::ffi::c_void;

use hooks_proc::forwardable_export;
use tracing::info;
use tracing::warn;

use super::UplayOverlapped;
use crate::uplay_r1_loader;

fn select_friend_snapshot(friends: Vec<crate::api::Friend>, local_account_id: Option<&str>) -> Vec<crate::api::Friend> {
    friends.into_iter().filter(|friend| local_account_id != Some(friend.id.as_str())).collect()
}

fn failed_precondition_message(status: &tonic::Status, chinese: bool) -> String {
    let reason = status.message().to_ascii_lowercase();
    if reason.contains("gameplay mode mismatch") {
        return String::from(if chinese {
            "官方模式与模组模式不能互相组队或进入同一房间"
        } else {
            "Official and MOD players cannot join the same party or room"
        });
    }
    if reason.contains("mod files verification failed") {
        return String::from(if chinese {
            "游戏模式文件校验失败"
        } else {
            "Game mode files failed verification"
        });
    }
    if reason.contains("player mode state unavailable") {
        return String::from(if chinese {
            "暂时无法确认好友的游戏模式"
        } else {
            "The friend's gameplay mode is temporarily unavailable"
        });
    }
    String::from(if chinese {
        "当前没有可加入的大厅或房间"
    } else {
        "No joinable lobby or room"
    })
}

fn invite_error_message(error: &crate::api::Error, chinese: bool) -> String {
    match error {
        crate::api::Error::GRPCStatus(status) if status.code() == tonic::Code::FailedPrecondition => failed_precondition_message(status, chinese),
        crate::api::Error::GRPCStatus(status) if status.code() == tonic::Code::NotFound => String::from(if chinese { "好友不存在" } else { "Friend not found" }),
        crate::api::Error::GRPCStatus(status) if status.code() == tonic::Code::InvalidArgument => {
            String::from(if chinese { "不能邀请自己" } else { "You cannot invite yourself" })
        }
        crate::api::Error::GRPCStatus(status) if status.code() == tonic::Code::AlreadyExists => {
            let same_room = status.message().to_ascii_lowercase().contains("room");
            if same_room {
                String::from(if chinese { "已在同一房间" } else { "Already in the same room" })
            } else {
                String::from(if chinese { "已在同一队伍" } else { "Already in the same party" })
            }
        }
        _ => crate::overlay::format_api_error(error, chinese),
    }
}

fn invite_error_is_transient(error: &crate::api::Error) -> bool {
    matches!(
        error,
        crate::api::Error::GRPCStatus(status)
            if matches!(
                status.code(),
                tonic::Code::FailedPrecondition
                    | tonic::Code::NotFound
                    | tonic::Code::InvalidArgument
                    | tonic::Code::AlreadyExists
            )
    )
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_AddToBlackList() -> isize {
    0
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_DisableFriendMenuItem() -> isize {
    0
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_EnableFriendMenuItem() -> isize {
    0
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_GetFriendList(friend_list_filter: *mut c_void, out_friend_list: *mut uplay_r1_loader::List) -> bool {
    if out_friend_list.is_null() || !out_friend_list.is_aligned() {
        return false;
    }
    let local_account_id = crate::config::get().map(|config| config.user.account_id.as_str());
    // A failed snapshot is not an empty roster and must not acknowledge a
    // pending native refresh. Keep caller output and retry state unchanged.
    let Ok(all_friends) = crate::api::list_friends() else {
        warn!("FriendsListSnapshot unavailable; pending refresh retained");
        return false;
    };
    let total_count = all_friends.len();
    let changed_targets = super::take_compact_friend_refresh();
    if changed_targets.is_none() {
        super::seed_friend_refresh_snapshot(&all_friends);
    }
    // The retail controller reliably completes the full-roster shape it uses
    // at startup. A one-account offline delta can remain in
    // StateLookupUbiUserByName and block every later refresh and invitation.
    // Changes are still coalesced and event-driven; only their payload shape
    // follows the native full-list path.
    let friends = select_friend_snapshot(all_friends, local_account_id)
        .into_iter()
        .map(|f| uplay_r1_loader::UplayFriend {
            id: f.id,
            username: f.username,
            is_online: f.is_online,
        })
        .collect::<Vec<_>>();
    let friend_count = friends.len();
    let online_count = friends.iter().filter(|friend| friend.is_online).count();
    let online_candidate_fps = friends
        .iter()
        .filter(|friend| friend.is_online)
        .map(|friend| super::short_sha256(friend.id.as_bytes()))
        .collect::<Vec<_>>();
    let refresh_scope = if changed_targets.is_some() {
        "change-triggered-full-roster"
    } else {
        "startup-full-roster"
    };
    let roster_behavior = if changed_targets.is_some() { "merge" } else { "replace" };
    info!(
        "FriendsListSnapshot refresh_scope={refresh_scope} roster_behavior={roster_behavior} total_count={total_count} friend_count={friend_count} online_count={online_count} online_candidate_fps={online_candidate_fps:?}"
    );
    let list = uplay_r1_loader::UplayList::Friends(friends);
    let list: uplay_r1_loader::List = list.into();
    (*out_friend_list).count = list.count;
    (*out_friend_list).list = list.list;
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_Init(flags: usize) -> bool {
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_InviteToGame(account_id_utf8: *const c_char, overlapped: *mut UplayOverlapped) -> bool {
    const UPLAY_RESULT_FAILED: usize = 6;
    let chinese = crate::overlay::is_chinese_ui();
    let result = if account_id_utf8.is_null() {
        Err((String::from(if chinese { "好友信息无效" } else { "Invalid friend information" }), true))
    } else {
        std::ffi::CStr::from_ptr(account_id_utf8)
            .to_str()
            .map_err(|_| (String::from(if chinese { "好友信息无效" } else { "Invalid friend information" }), true))
            .and_then(|account_id| {
                info!("FriendInviteDispatch target_fp=sha256:{}", super::short_sha256(account_id.as_bytes()));
                crate::api::invite_friend(account_id).map_err(|error| {
                    let transient = invite_error_is_transient(&error);
                    (invite_error_message(&error, chinese), transient)
                })
            })
    };

    let success = result.is_ok();
    if !overlapped.is_null() && overlapped.is_aligned() {
        if success {
            (*overlapped).set_success();
        } else {
            (*overlapped).set_result(UPLAY_RESULT_FAILED);
        }
    }
    match result {
        Ok(()) => {
            info!("Friend game invitation sent successfully");
            crate::overlay::notify(if chinese { "邀请已发送" } else { "Invitation sent" });
        }
        Err((message, transient)) => {
            warn!("Friend game invitation failed: {message}");
            if transient {
                crate::overlay::notify(message);
            } else {
                crate::overlay::notify_error(message);
            }
        }
    }
    success
}

#[cfg(test)]
mod invite_error_tests {
    use super::*;

    #[test]
    fn duplicate_room_and_party_are_explained_in_both_languages() {
        let room = crate::api::Error::GRPCStatus(tonic::Status::already_exists("Already in the same room"));
        let party = crate::api::Error::GRPCStatus(tonic::Status::already_exists("Already in the same party"));
        assert_eq!(invite_error_message(&room, true), "已在同一房间");
        assert_eq!(invite_error_message(&party, true), "已在同一队伍");
        assert_eq!(invite_error_message(&room, false), "Already in the same room");
        assert_eq!(invite_error_message(&party, false), "Already in the same party");
        assert!(invite_error_is_transient(&room));
        assert!(invite_error_is_transient(&party));
        assert!(invite_error_is_transient(&crate::api::Error::GRPCStatus(tonic::Status::invalid_argument(
            "Cannot invite yourself",
        ))));
        assert!(!invite_error_is_transient(&crate::api::Error::GRPCStatus(
            tonic::Status::unavailable("Server unavailable",)
        )));
    }

    #[test]
    fn gameplay_mode_failures_are_actionable_and_transient() {
        let mismatch = crate::api::Error::GRPCStatus(tonic::Status::failed_precondition("gameplay mode mismatch"));
        let integrity = crate::api::Error::GRPCStatus(tonic::Status::failed_precondition("mod files verification failed"));
        assert_eq!(invite_error_message(&mismatch, true), "官方模式与模组模式不能互相组队或进入同一房间");
        assert_eq!(invite_error_message(&mismatch, false), "Official and MOD players cannot join the same party or room");
        assert_eq!(invite_error_message(&integrity, true), "游戏模式文件校验失败");
        assert_eq!(invite_error_message(&integrity, false), "Game mode files failed verification");
        assert!(invite_error_is_transient(&mismatch));
        assert!(invite_error_is_transient(&integrity));
    }
}

#[cfg(test)]
mod friend_snapshot_tests {
    use super::*;

    fn friend(id: &str, online: bool) -> crate::api::Friend {
        crate::api::Friend {
            id: id.into(),
            username: id.into(),
            is_online: online,
        }
    }

    #[test]
    fn native_snapshot_keeps_the_full_roster_and_excludes_self() {
        let selected = select_friend_snapshot(vec![friend("self", true), friend("online", true), friend("offline", false)], Some("self"));
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|friend| friend.id == "online"));
        assert!(selected.iter().any(|friend| friend.id == "offline"));
    }

    #[test]
    fn change_triggered_snapshot_keeps_the_native_full_roster_shape() {
        let selected = select_friend_snapshot(
            vec![
                friend("self", true),
                friend("online", true),
                friend("offline", false),
                friend("unchanged-online", true),
                friend("unchanged-offline", false),
            ],
            Some("self"),
        );
        assert_eq!(
            selected,
            vec![
                friend("online", true),
                friend("offline", false),
                friend("unchanged-online", true),
                friend("unchanged-offline", false)
            ]
        );
    }
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_IsBlackListed(account_id_utf8: *const c_char) -> bool {
    false
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_IsFriend(account_id_utf8: *const c_char) -> bool {
    false
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_RequestFriendship() -> isize {
    0
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_FRIENDS_ShowFriendSelectionUI() -> isize {
    0
}
