use std::time::Duration;
use std::time::Instant;

use server_api::misc::InviteEvent;

use crate::uplay_r1_loader::Event;

pub(super) const PARTY_FOLLOW_ACCEPT_DELAY: Duration = Duration::from_millis(750);
const MANUAL_ACCEPT_PRESENTATION_GUARD: Duration = Duration::from_millis(750);

struct Invite {
    event: InviteEvent,
    accepted: bool,
    presented_at: Instant,
}

pub(super) struct InviteUpsert {
    pub index: usize,
    pub message: Option<String>,
    pub force_join: bool,
    pub invite_id: i64,
    pub kind: i32,
    pub is_new: bool,
}

/// Owns invitation selection, de-duplication and delayed auto-follow state.
/// Network acceptance remains in the render-loop adapter so this state object
/// stays deterministic and straightforward to test.
pub(super) struct InviteController {
    active: Vec<Invite>,
    accept_key_down: bool,
    accept_key_released_after_present: bool,
    scheduled_party_follow: Option<(Instant, i64)>,
}

impl InviteController {
    pub fn new() -> Self {
        Self {
            active: Vec::new(),
            accept_key_down: false,
            accept_key_released_after_present: false,
            scheduled_party_follow: None,
        }
    }

    pub fn event(&self, index: usize) -> Option<&InviteEvent> {
        self.active.get(index).map(|invite| &invite.event)
    }

    pub fn remove_id(&mut self, id: i64) {
        if let Some(index) = self.active.iter().position(|invite| invite.event.id == id) {
            self.remove(index);
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.active.len() {
            self.active.remove(index);
        }
    }

    pub fn accept_key_pressed(&mut self, is_down: bool) -> Option<usize> {
        self.accept_key_pressed_at(is_down, Instant::now())
    }

    fn accept_key_pressed_at(&mut self, is_down: bool, now: Instant) -> Option<usize> {
        if !is_down {
            self.accept_key_down = false;
            if self.active.iter().any(|invite| !invite.accepted) {
                self.accept_key_released_after_present = true;
            }
            return None;
        }
        if self.accept_key_down {
            return None;
        }
        self.accept_key_down = true;
        let index = self.active.iter().rposition(|invite| !invite.accepted)?;
        let invite = &self.active[index];
        let visible_for = now.saturating_duration_since(invite.presented_at);
        if !self.accept_key_released_after_present || visible_for < MANUAL_ACCEPT_PRESENTATION_GUARD {
            tracing::info!(
                "InviteAcceptBlocked origin=f5 reason=presentation_guard local_invite_id={} kind={} visible_ms={} released_after_present={}",
                invite.event.id,
                invite.event.kind,
                visible_for.as_millis(),
                self.accept_key_released_after_present
            );
            return None;
        }
        tracing::info!(
            "InviteAcceptOrigin origin=f5 local_invite_id={} kind={} visible_ms={}",
            invite.event.id,
            invite.event.kind,
            visible_for.as_millis()
        );
        Some(index)
    }

    pub fn upsert(&mut self, event: InviteEvent, chinese: bool) -> InviteUpsert {
        self.upsert_at(event, chinese, Instant::now())
    }

    fn upsert_at(&mut self, event: InviteEvent, chinese: bool, now: Instant) -> InviteUpsert {
        let existing = self.active.iter().position(|invite| invite.event.id == event.id);
        let is_new = existing.is_none();
        let index = if let Some(index) = existing {
            self.active[index].event = event;
            index
        } else {
            self.active.push(Invite {
                event,
                accepted: false,
                presented_at: now,
            });
            self.accept_key_released_after_present = false;
            self.active.len() - 1
        };
        let current = &self.active[index].event;
        let update = InviteUpsert {
            index,
            message: current.sender.as_ref().and_then(|sender| invite_message(current, &sender.username, chinese)),
            force_join: current.force_join,
            invite_id: current.id,
            kind: current.kind,
            is_new,
        };
        tracing::info!(
            "InviteStateUpsert local_invite_id={} kind={} is_new={} sender_present={} message_present={} force_join={}",
            update.invite_id,
            update.kind,
            update.is_new,
            current.sender.is_some(),
            update.message.is_some(),
            update.force_join
        );
        if update.force_join {
            tracing::info!(
                "InviteAcceptOrigin origin=force_join phase=requested local_invite_id={} kind={}",
                update.invite_id,
                update.kind
            );
        }
        update
    }

    pub fn schedule_party_follow(&mut self, invite_id: i64, now: Instant) {
        self.scheduled_party_follow = Some((now + PARTY_FOLLOW_ACCEPT_DELAY, invite_id));
    }

    pub fn take_due_party_follow(&mut self, now: Instant) -> Option<(usize, i64)> {
        let (deadline, invite_id) = self.scheduled_party_follow?;
        if now < deadline {
            return None;
        }
        self.scheduled_party_follow = None;
        let result = self.active.iter().position(|invite| invite.event.id == invite_id).map(|index| (index, invite_id));
        if let Some((index, _)) = result {
            let kind = self.active[index].event.kind;
            tracing::info!("InviteAcceptOrigin origin=party_follow phase=due local_invite_id={invite_id} kind={kind}");
        }
        result
    }
}

fn invite_message(event: &InviteEvent, sender_name: &str, chinese: bool) -> Option<String> {
    // Kind 4 is an internal native-lobby restore. It must drive the retail
    // state machine but is never a player invitation and must stay invisible.
    if event.kind == 4 {
        return None;
    }
    let message = if chinese {
        if matches!(event.kind, 3 | 5) {
            format!("正在跟随 {sender_name}…")
        } else if event.kind == 2 {
            format!("{sender_name} 邀请加入队伍 · F5 接受")
        } else {
            format!("{sender_name} 邀请加入私人房间 · F5 接受")
        }
    } else if matches!(event.kind, 3 | 5) {
        format!("Following {sender_name}…")
    } else if event.kind == 2 {
        format!("{sender_name} invited you to a party · F5 to accept")
    } else {
        format!("{sender_name} invited you to a private room · F5 to accept")
    };
    Some(message)
}

fn party_game_invite(
    kind: i32,
    invitation_id: i64,
    sender_id: String,
    sender_name: String,
    game_session: Option<server_api::misc::UplayGameSession>,
) -> Result<crate::uplay_r1_loader::PartyGameInvite, &'static str> {
    let invitation_id = u32::try_from(invitation_id).map_err(|_| "Invitation ID is outside the Uplay u32 range")?;
    let game_session = game_session.ok_or("Host Uplay GameSession data is not available yet")?;
    if game_session.id == 0 {
        return Err("Host Uplay GameSession ID is not available yet");
    }
    if game_session.data.is_empty() {
        return Err("Host Uplay GameSession data is empty");
    }
    if kind == 1 && game_session.data.len() != 0x1f0 {
        return Err("Host Uplay GameSession data is not the expected 496 bytes");
    }
    if game_session.data.len() > crate::uplay_r1_loader::MAX_PARTY_INVITE_BYTES {
        return Err("Host Uplay GameSession data exceeds the native event capacity");
    }
    Ok(crate::uplay_r1_loader::PartyGameInvite {
        invitation_id,
        kind,
        sender_id,
        sender_name,
        game_session,
    })
}

pub(super) fn accepted_event_payload_ready(kind: i32, game_session: Option<&server_api::misc::UplayGameSession>) -> bool {
    match kind {
        1 => game_session.is_some_and(|session| session.id != 0 && session.data.len() == 0x1f0),
        2 => true,
        3 => game_session.is_some_and(|session| {
            session.id != 0 && !session.data.is_empty() && session.data.len() <= crate::uplay_r1_loader::MAX_PARTY_INVITE_BYTES
        }),
        5 => game_session.is_some_and(|session| session.id != 0 && !session.data.is_empty()),
        _ => true,
    }
}

pub(super) fn accepted_event_for_kind(
    kind: i32,
    invitation_id: i64,
    sender_id: String,
    sender_name: String,
    game_session: Option<server_api::misc::UplayGameSession>,
) -> Result<Event, &'static str> {
    // Retail public-lobby, lobby-restore, and host-migration invitations use
    // the friend-search -> NAT -> JoinSession transition. Real three-player
    // migration traces prove PartyGameInviteAccepted only updates Party flags
    // for a survivor that is already migrating; it does not enter JoinSession.
    if matches!(kind, 2 | 4 | 5) {
        let invitation_id = u32::try_from(invitation_id).map_err(|_| "Invitation ID is outside the Uplay u32 range")?;
        return Ok(Event::FriendsGameInviteAccepted(invitation_id, sender_id, sender_name));
    }

    let invite = party_game_invite(kind, invitation_id, sender_id, sender_name, game_session)?;
    if kind == 1 {
        if invite.game_session.data.len() != 0x1f0 {
            return Err("Host Uplay GameSession data is not the expected 496 bytes");
        }
        crate::uplay_r1_loader::record_private_invite_blob(
            "receiver-api-received",
            i64::from(invite.invitation_id),
            invite.game_session.id,
            invite.game_session.id,
            &invite.game_session.data,
        );
        Ok(Event::FriendsPrivateBlobInviteAccepted(invite))
    } else {
        Ok(Event::PartyGameInviteAccepted(invite))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite_event(id: i64, kind: i32) -> InviteEvent {
        InviteEvent {
            id,
            sender: None,
            force_join: false,
            kind,
            game_session: None,
        }
    }

    fn game_session() -> server_api::misc::UplayGameSession {
        server_api::misc::UplayGameSession {
            id: 77,
            data: vec![1, 2, 3],
            flags: 0,
            invite_only: true,
        }
    }

    #[test]
    fn f5_selects_latest_pending_invitation_once_per_key_press() {
        let mut controller = InviteController::new();
        let presented_at = Instant::now();
        controller.upsert_at(invite_event(-1, 1), true, presented_at);
        controller.upsert_at(invite_event(-3, 1), true, presented_at);
        let eligible_at = presented_at + MANUAL_ACCEPT_PRESENTATION_GUARD;
        assert_eq!(controller.accept_key_pressed_at(false, presented_at), None);
        assert_eq!(controller.accept_key_pressed_at(true, eligible_at), Some(1));
        assert_eq!(controller.accept_key_pressed_at(true, eligible_at), None);
        assert_eq!(controller.accept_key_pressed_at(false, eligible_at), None);
        assert_eq!(controller.accept_key_pressed_at(true, eligible_at), Some(1));
    }

    #[test]
    fn removing_an_invalid_latest_invite_exposes_the_previous_pending_invite() {
        let mut controller = InviteController::new();
        let presented_at = Instant::now();
        controller.upsert_at(invite_event(-1, 1), true, presented_at);
        controller.upsert_at(invite_event(-3, 1), true, presented_at);
        controller.remove(1);

        let eligible_at = presented_at + MANUAL_ACCEPT_PRESENTATION_GUARD;
        assert_eq!(controller.accept_key_pressed_at(false, eligible_at), None);
        assert_eq!(controller.accept_key_pressed_at(true, eligible_at), Some(0));
        assert_eq!(controller.event(0).map(|event| event.id), Some(-1));
    }

    #[test]
    fn f5_cannot_accept_a_new_invite_before_the_card_has_been_presented() {
        let mut controller = InviteController::new();
        let presented_at = Instant::now();
        controller.upsert_at(invite_event(-1, 1), true, presented_at);
        assert_eq!(controller.accept_key_pressed_at(false, presented_at), None);
        assert_eq!(
            controller.accept_key_pressed_at(true, presented_at + MANUAL_ACCEPT_PRESENTATION_GUARD - Duration::from_millis(1)),
            None
        );
        assert_eq!(controller.accept_key_pressed_at(false, presented_at + MANUAL_ACCEPT_PRESENTATION_GUARD), None);
        assert_eq!(controller.accept_key_pressed_at(true, presented_at + MANUAL_ACCEPT_PRESENTATION_GUARD), Some(0));
    }

    #[test]
    fn f5_must_be_released_after_a_new_invite_is_presented() {
        let mut controller = InviteController::new();
        let presented_at = Instant::now();
        controller.upsert_at(invite_event(-1, 1), true, presented_at);
        let eligible_at = presented_at + MANUAL_ACCEPT_PRESENTATION_GUARD;
        assert_eq!(controller.accept_key_pressed_at(true, eligible_at), None);
        assert_eq!(controller.accept_key_pressed_at(false, eligible_at), None);
        assert_eq!(controller.accept_key_pressed_at(true, eligible_at), Some(0));
    }

    #[test]
    fn repeated_server_delivery_updates_in_place() {
        let mut controller = InviteController::new();
        assert!(controller.upsert(invite_event(7, 1), true).is_new);
        let update = controller.upsert(invite_event(7, 3), true);
        assert!(!update.is_new);
        assert_eq!(update.index, 0);
        assert_eq!(controller.event(0).map(|event| event.kind), Some(3));
    }

    #[test]
    fn internal_lobby_restore_is_force_joined_without_player_presentation() {
        let mut event = invite_event(8, 4);
        event.force_join = true;
        event.sender = Some(server_api::users::User {
            id: String::from("self-id"),
            username: String::from("self"),
            ips: Vec::new(),
        });
        let update = InviteController::new().upsert(event, true);
        assert!(update.force_join);
        assert_eq!(update.message, None);
    }

    #[test]
    fn party_follow_is_not_due_before_delay() {
        let mut controller = InviteController::new();
        controller.upsert(invite_event(9, 3), true);
        let now = Instant::now();
        controller.schedule_party_follow(9, now);
        assert_eq!(controller.take_due_party_follow(now), None);
        assert_eq!(controller.take_due_party_follow(now + PARTY_FOLLOW_ACCEPT_DELAY), Some((0, 9)));
    }

    #[test]
    fn lobby_invite_uses_retail_friend_search_transition() {
        let mut session = game_session();
        session.id = 460;
        session.data = vec![0; 0x1f0];
        session.flags = 7;
        session.invite_only = false;
        assert!(matches!(
            accepted_event_for_kind(2, 41, String::from("leader-id"), String::from("leader"), Some(session)),
            Ok(Event::FriendsGameInviteAccepted(41, ref sender_id, ref sender_name))
                if sender_id == "leader-id" && sender_name == "leader"
        ));
    }

    #[test]
    fn lobby_invite_does_not_require_party_game_session_transport() {
        assert!(accepted_event_payload_ready(2, None));
        assert!(accepted_event_payload_ready(2, Some(&game_session())));
        assert!(matches!(
            accepted_event_for_kind(2, 41, String::from("leader-id"), String::from("leader"), None),
            Ok(Event::FriendsGameInviteAccepted(41, ref sender_id, ref sender_name))
                if sender_id == "leader-id" && sender_name == "leader"
        ));
    }

    #[test]
    fn payload_readiness_preserves_private_follow_and_restore_requirements() {
        let mut private = game_session();
        private.data = vec![0; 0x1f0];
        assert!(accepted_event_payload_ready(1, Some(&private)));
        assert!(accepted_event_payload_ready(2, None));
        assert!(accepted_event_payload_ready(3, Some(&game_session())));
        assert!(accepted_event_payload_ready(4, None));
        assert!(accepted_event_payload_ready(5, Some(&game_session())));
    }

    #[test]
    fn private_invite_baseline_keeps_context() {
        let mut session = game_session();
        session.data = vec![0; 0x1f0];
        session.data[..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            accepted_event_for_kind(
                1,
                42,
                String::from("leader-id"),
                String::from("leader"),
                Some(session),
            ),
            Ok(Event::FriendsPrivateBlobInviteAccepted(ref invite))
                if invite.invitation_id == 42
                    && invite.sender_id == "leader-id"
                    && invite.game_session.id == 77
        ));
    }

    #[test]
    fn party_follow_uses_party_transition_with_game_session() {
        assert!(matches!(
            accepted_event_for_kind(
                3,
                42,
                String::from("leader-id"),
                String::from("leader"),
                Some(game_session()),
            ),
            Ok(Event::PartyGameInviteAccepted(ref invite))
                if invite.invitation_id == 42
                    && invite.kind == 3
                    && invite.sender_id == "leader-id"
                    && invite.game_session.id == 77
        ));
    }

    #[test]
    fn party_follow_waits_for_host_game_session_data() {
        assert!(accepted_event_for_kind(3, 42, String::from("leader-id"), String::from("leader"), None,).is_err());
    }

    #[test]
    fn host_migration_follow_uses_the_retail_join_session_transition() {
        assert!(matches!(
            accepted_event_for_kind(
                5,
                44,
                String::from("new-host-id"),
                String::from("new-host"),
                Some(game_session()),
            ),
            Ok(Event::FriendsGameInviteAccepted(44, ref sender_id, ref sender_name))
                if sender_id == "new-host-id" && sender_name == "new-host"
        ));
        assert!(!accepted_event_payload_ready(5, None));
    }

    #[test]
    fn lobby_restore_keeps_the_friends_transition_without_a_session() {
        assert!(matches!(
            accepted_event_for_kind(4, 43, String::from("leader-id"), String::from("leader"), None),
            Ok(Event::FriendsGameInviteAccepted(43, ref sender_id, ref sender_name))
                if sender_id == "leader-id" && sender_name == "leader"
        ));
    }
}
