use std::collections::HashSet;
use std::time::Instant;

use sqlx::Execute;

use super::is_game_room_attributes;
use super::{cleanup_is_current, complete_cleanup};
use super::is_private_room_attributes;
use super::run;
use super::session_attribute_value;
use super::GameSession;
use super::LobbyRestoreSplitTransition;
use super::Result;
use super::Storage;
use super::INVITE_KIND_PRIVATE_ROOM;
use super::LOBBY_RESTORE_SPLIT_TTL;

impl Storage {
    pub(super) fn note_lobby_restore_split_transition(&self, type_id: u32, session_id: u32, user_id: u32) {
        if let Ok(mut transitions) = self.lobby_restore_split_transitions.lock() {
            transitions.insert(
                (type_id, session_id, user_id),
                LobbyRestoreSplitTransition {
                    generation: self.next_cleanup_generation(),
                    started_at: Instant::now(),
                    removal_deferred: false,
                },
            );
        }
    }

    fn defer_lobby_restore_split_removal(&self, type_id: u32, session_id: u32, user_id: u32) -> bool {
        let Ok(mut transitions) = self.lobby_restore_split_transitions.lock() else {
            return false;
        };
        // Expiry is not completion: another room may still have a deferred
        // removal to apply. Only the expiry worker consumes those records.
        let Some(transition) = transitions.get_mut(&(type_id, session_id, user_id)) else {
            return false;
        };
        if transition.started_at.elapsed() > LOBBY_RESTORE_SPLIT_TTL {
            return false;
        }
        transition.removal_deferred = true;
        true
    }

    fn lobby_restore_split_transition_is_active(&self, type_id: u32, session_id: u32, user_id: u32) -> bool {
        let Ok(transitions) = self.lobby_restore_split_transitions.lock() else {
            return false;
        };
        transitions.get(&(type_id, session_id, user_id))
            .is_some_and(|transition| transition.started_at.elapsed() <= LOBBY_RESTORE_SPLIT_TTL)
    }

    fn clear_lobby_restore_split_transition(&self, type_id: u32, session_id: u32, user_id: u32) {
        if let Ok(mut transitions) = self.lobby_restore_split_transitions.lock() {
            transitions.remove(&(type_id, session_id, user_id));
        }
    }

    pub(crate) fn lobby_restore_split_cleanup_due(&self) -> bool {
        self.lobby_restore_split_transitions
            .lock()
            .is_ok_and(|transitions| transitions.values().any(|transition| transition.started_at.elapsed() > LOBBY_RESTORE_SPLIT_TTL))
    }

    fn pending_expired_lobby_restore_split_transitions(&self) -> Vec<(u32, u32, u32, LobbyRestoreSplitTransition)> {
        let Ok(transitions) = self.lobby_restore_split_transitions.lock() else { return Vec::new(); };
        transitions.iter().filter_map(|(key, transition)|
            (transition.started_at.elapsed() > LOBBY_RESTORE_SPLIT_TTL).then_some((key.0, key.1, key.2, *transition))).collect()
    }

    async fn private_room_fallback_reasons_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_id: u32,
        type_id: u32,
        lobby_session_id: u32,
    ) -> sqlx::Result<(bool, bool)> {
        let accepted_targets = sqlx::query_as::<_, (String,)>(
            r"
            SELECT target.attributes
            FROM invites AS invite
            INNER JOIN game_sessions AS target
                ON target.id = invite.session_id
               AND target.type_id = invite.session_type
               AND target.destroyed_at IS NULL
            WHERE invite.receiver = ?
              AND invite.kind = ?
              AND invite.session_type = ?
              AND invite.session_id != ?
              AND invite.accepted_at IS NOT NULL
              AND invite.consumed_at IS NULL
              AND invite.expires_at > CURRENT_TIMESTAMP
            ",
        )
        .bind(user_id)
        .bind(INVITE_KIND_PRIVATE_ROOM)
        .bind(type_id)
        .bind(lobby_session_id)
        .fetch_all(&mut **transaction)
        .await?;
        let accepted_private_invite = accepted_targets.iter().any(|(attributes,)| is_game_room_attributes(attributes));

        let active_sessions = sqlx::query_as::<_, (String,)>(
            r"
            SELECT session.attributes
            FROM game_sessions AS session
            INNER JOIN participants AS participant ON participant.game_id = session.id
            WHERE participant.user_id = ?
              AND session.type_id = ?
              AND session.id != ?
              AND session.destroyed_at IS NULL
            ",
        )
        .bind(user_id)
        .bind(type_id)
        .bind(lobby_session_id)
        .fetch_all(&mut **transaction)
        .await?;
        let active_private_room = active_sessions.iter().any(|(attributes,)| is_game_room_attributes(attributes));

        Ok((accepted_private_invite, active_private_room))
    }

    pub(super) async fn retire_superseded_solo_lobbies_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_id: u32,
        type_id: u32,
        replacement_session_id: u32,
    ) -> sqlx::Result<u64> {
        let candidates = sqlx::query_as::<_, (u32, String)>(
            r"
            SELECT session.id, session.attributes
            FROM game_sessions AS session
            WHERE session.creator_id = ?
              AND session.type_id = ?
              AND session.id != ?
              AND session.destroyed_at IS NULL
              AND NOT EXISTS (
                  SELECT 1
                  FROM participants AS other
                  WHERE other.game_id = session.id
                    AND other.user_id != ?
              )
            ",
        )
        .bind(user_id)
        .bind(type_id)
        .bind(replacement_session_id)
        .bind(user_id)
        .fetch_all(&mut **transaction)
        .await?;

        let mut retired = 0;
        for (session_id, attributes) in candidates {
            if session_attribute_value(&attributes, 113) != Some(1) {
                continue;
            }
            sqlx::query("DELETE FROM participants WHERE game_id = ?")
                .bind(session_id)
                .execute(&mut **transaction)
                .await?;
            retired += sqlx::query("UPDATE game_sessions SET destroyed_at = CURRENT_TIMESTAMP WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
                .bind(session_id)
                .bind(type_id)
                .execute(&mut **transaction)
                .await?
                .rows_affected();
        }
        Ok(retired)
    }

    pub fn game_session_is_active(&self, type_id: u32, session_id: u32) -> Result<bool> {
        let (active,): (i64,) = run(
            sqlx::query_as("SELECT EXISTS(SELECT 1 FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL)")
                .bind(session_id)
                .bind(type_id)
                .fetch_one(&self.pool),
        )??;
        Ok(active == 1)
    }

    /// Returns a read-only snapshot used only for SCBL capacity diagnostics.
    /// This does not enforce a limit or change the database schema.
    pub fn game_session_diagnostics(&self, type_id: u32, session_id: u32) -> Result<Option<(u32, String, Vec<u32>)>> {
        let snapshot = run(async {
            let Some((creator_id, attributes)) =
                sqlx::query_as::<_, (u32, String)>("SELECT creator_id, attributes FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
                    .bind(session_id)
                    .bind(type_id)
                    .fetch_optional(&self.pool)
                    .await?
            else {
                return Ok::<Option<(u32, String, Vec<u32>)>, sqlx::Error>(None);
            };

            let participant_ids = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? ORDER BY user_id")
                .bind(session_id)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(|row| row.0)
                .collect();

            Ok::<Option<(u32, String, Vec<u32>)>, sqlx::Error>(Some((creator_id, attributes, participant_ids)))
        })??;
        Ok(snapshot)
    }

    pub(super) async fn reconcile_game_session_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, type_id: u32, session_id: u32) -> sqlx::Result<Option<u32>> {
        let creator = sqlx::query_as::<_, (u32, i64)>("SELECT creator_id, host_leave_pending FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
            .bind(session_id)
            .bind(type_id)
            .fetch_optional(&mut **transaction)
            .await?;
        let Some((creator_id, host_leave_pending)) = creator else {
            return Ok(None);
        };

        let participant_ids = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? ORDER BY user_id")
            .bind(session_id)
            .fetch_all(&mut **transaction)
            .await?;

        if participant_ids.is_empty() {
            sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP, host_leave_pending=0 WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
                .bind(session_id)
                .bind(type_id)
                .execute(&mut **transaction)
                .await?;
            return Ok(None);
        }

        // DeleteSession deliberately preserves the old owner row while the
        // remaining clients unwind or elect a native successor. Once every
        // non-owner has left, that row is no longer a live private room. Retire
        // it immediately so a later N-1 regroup is not blocked by stale state.
        if host_leave_pending != 0 && participant_ids.iter().all(|(participant_id,)| *participant_id == creator_id) {
            sqlx::query("DELETE FROM participants WHERE game_id = ?")
                .bind(session_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP, host_leave_pending=0 WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
                .bind(session_id)
                .bind(type_id)
                .execute(&mut **transaction)
                .await?;
            return Ok(None);
        }

        if participant_ids.iter().any(|(participant_id,)| *participant_id == creator_id) {
            return Ok(Some(creator_id));
        }

        let new_creator_id = participant_ids[0].0;
        sqlx::query("UPDATE game_sessions SET creator_id = ? WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
            .bind(new_creator_id)
            .bind(session_id)
            .bind(type_id)
            .execute(&mut **transaction)
            .await?;
        Ok(Some(new_creator_id))
    }

    /// Removes a user from an active game session without synthesizing a lobby
    /// transition. Party/lobby ownership belongs to the game client's state
    /// machine; in particular a non-host leaving a match must not be attached
    /// to the former host's Party.
    pub fn leave_game_session(&self, user_id: u32, type_id: u32, session_id: u32) -> Result<()> {
        self.leave_game_session_impl(user_id, type_id, session_id, false)
    }

    pub fn abandon_game_session(&self, user_id: u32, type_id: u32, session_id: u32) -> Result<()> {
        self.leave_game_session_impl(user_id, type_id, session_id, true)
    }

    fn capture_quick_match_party_departure(&self, user_id: u32, type_id: u32, session_id: u32, recent_departures: &[u32]) -> Result<Option<super::QuickMatchPartyDeparture>> {
        let Some(lobby_session_id) = self.published_game_session(user_id) else {
            return Ok(None);
        };
        Ok(run(async {
            let session = sqlx::query_as::<_, (String, u32, i64, u32)>(
                r"
                SELECT room.attributes, room.creator_id, membership.party_id, party.leader_id
                FROM game_sessions AS room
                INNER JOIN participants AS room_member
                    ON room_member.game_id = room.id AND room_member.user_id = ?
                INNER JOIN party_members AS membership ON membership.user_id = ?
                INNER JOIN parties AS party ON party.id = membership.party_id
                WHERE room.id = ?
                  AND room.type_id = ?
                  AND (room.creator_id = ? OR party.leader_id = ?)
                  AND room.destroyed_at IS NULL
                ",
            )
            .bind(user_id)
            .bind(user_id)
            .bind(session_id)
            .bind(type_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
            let Some((attributes, room_host_id, party_id, leader_id)) = session else {
                return Ok::<Option<super::QuickMatchPartyDeparture>, sqlx::Error>(None);
            };
            if !is_game_room_attributes(&attributes) || is_private_room_attributes(&attributes) {
                return Ok(None);
            }

            let members = sqlx::query_as::<_, (u32,)>(
                r"
                SELECT membership.user_id
                FROM party_members AS membership
                WHERE membership.party_id = ?
                ORDER BY membership.joined_at, membership.user_id
                ",
            )
            .bind(party_id)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|(member_id,)| member_id)
            .collect::<Vec<_>>();
            if members.len() < 2 || !members.contains(&user_id) {
                return Ok(None);
            }

            let departed_members = recent_departures.iter().copied().filter(|member_id| members.contains(member_id)).collect::<HashSet<_>>();
            Ok(Some(super::QuickMatchPartyDeparture {
                captured_at: Instant::now(),
                party_id,
                leader_id,
                room_host_id,
                lobby_session_id,
                members,
                departed_members,
            }))
        })??)
    }

    fn party_leader_for_user(&self, user_id: u32) -> Result<Option<u32>> {
        Ok(run(sqlx::query_scalar::<_, u32>(
            r"
                SELECT party.leader_id
                FROM party_members AS membership
                INNER JOIN parties AS party ON party.id = membership.party_id
                WHERE membership.user_id = ?
                ",
        )
        .bind(user_id)
        .fetch_optional(&self.pool))??)
    }

    fn leave_game_session_impl(&self, user_id: u32, type_id: u32, session_id: u32, native_abandon: bool) -> Result<()> {
        self.leave_game_session_checked(user_id, type_id, session_id, native_abandon, || true)
    }

    pub(super) fn leave_game_session_checked(&self, user_id: u32, type_id: u32, session_id: u32, native_abandon: bool, may_proceed: impl Fn() -> bool) -> Result<()> {
        if !may_proceed() { return Ok(()); }
        // Retail normally asks the owner to RemoveParticipants before the
        // departing client sends LeaveSession. Preserve that first-step proof:
        // by the time this call arrives the participant row is already gone,
        // so querying only the live Session would miss the real departure.
        let owner_removed_game_room_member = self.recent_game_room_departures(type_id, session_id).contains(&user_id);
        let mut recent_game_room_departures = self.recent_game_room_departures(type_id, session_id);
        recent_game_room_departures.extend(self.pending_quick_match_member_leaves(type_id, session_id));
        recent_game_room_departures.sort_unstable();
        recent_game_room_departures.dedup();
        let native_migration_active = self.native_host_migration_is_active(type_id, session_id);
        let correlated_split_party_departure = if native_abandon {
            self.note_quick_match_party_member_departed(type_id, session_id, user_id)
        } else {
            None
        };
        let captured_party_departure = if correlated_split_party_departure.is_none() && native_abandon {
            recent_game_room_departures.push(user_id);
            self.capture_quick_match_party_departure(user_id, type_id, session_id, &recent_game_room_departures)?
        } else if !native_abandon && owner_removed_game_room_member {
            // The room owner removes Party followers before its own
            // "leave with Party" Abandon. Freeze the complete cohort before
            // this member is detached to a personal Party. The snapshot alone
            // performs no transition; it expires unless a same-Party Split and
            // leader Abandon confirm the coordinated exit.
            if let Some(party_leader_id) = self.party_leader_for_user(user_id)? {
                self.capture_quick_match_party_departure(party_leader_id, type_id, session_id, &recent_game_room_departures)?
            } else {
                None
            }
        } else {
            None
        };
        if let Some(snapshot) = captured_party_departure.clone() {
            self.remember_quick_match_party_departure(type_id, session_id, snapshot);
        }
        let quick_match_party_departure = correlated_split_party_departure.or(captured_party_departure);
        let (preserved, accepted_private_invite, active_private_room, migration_pending, member_restore, native_party_handoff, party_return) = run(async {
            let mut transaction = self.begin_write().await?;
            if !may_proceed() {
                transaction.rollback().await?;
                return Ok((false, false, false, false, None, None, None));
            }

            let current_lobby = sqlx::query_as::<_, (String, u32, i64, i64)>(
                r"
                SELECT
                    session.attributes,
                    session.creator_id,
                    (SELECT COUNT(*) FROM participants AS member WHERE member.game_id = session.id),
                    session.host_leave_pending
                FROM game_sessions AS session
                INNER JOIN participants AS participant ON participant.game_id = session.id
                WHERE session.id = ?
                  AND session.type_id = ?
                  AND session.destroyed_at IS NULL
                  AND participant.user_id = ?
                ",
            )
            .bind(session_id)
            .bind(type_id)
            .bind(user_id)
            .fetch_optional(&mut *transaction)
            .await?;
            let owner_removed_public_quick_match_member = if !native_abandon && current_lobby.is_none() && owner_removed_game_room_member {
                sqlx::query_scalar::<_, String>("SELECT attributes FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
                    .bind(session_id)
                    .bind(type_id)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .is_some_and(|attributes| is_game_room_attributes(&attributes) && !is_private_room_attributes(&attributes))
            } else {
                false
            };

            // In the retail "leave with Party" ordering, a remaining Party
            // member can submit SplitSession first. That authoritative split
            // removes the old leader's participant row, so their immediately
            // following AbandonSession no longer has a live-session join from
            // which to reconstruct the cohort. Only a previously correlated
            // Party snapshot can complete this path; a normal host migration
            // sends the old host's Abandon before Split and never enters it.
            if native_abandon && current_lobby.is_none() {
                if let Some(snapshot) = quick_match_party_departure
                    .as_ref()
                    .filter(|snapshot| snapshot.leader_id == user_id && snapshot.departed_members.len() >= 2)
                {
                    let party_return = self.restore_quick_match_party_snapshot_async(&mut transaction, snapshot, type_id, session_id).await?;
                    if party_return.is_some() {
                        transaction.commit().await?;
                        return Ok::<(bool, bool, bool, bool, Option<(u32, bool, u64)>, Option<(u32, u64)>, Option<(u32, u64, u64)>), sqlx::Error>((
                            false,
                            false,
                            false,
                            false,
                            None,
                            None,
                            party_return,
                        ));
                    }
                }
            }

            let is_callers_solo_lobby = current_lobby.as_ref().is_some_and(|(attributes, creator_id, participant_count, _)| {
                *creator_id == user_id && *participant_count == 1 && session_attribute_value(attributes, 113) == Some(1)
            });
            if is_callers_solo_lobby {
                let (accepted_private_invite, active_private_room) = Self::private_room_fallback_reasons_async(&mut transaction, user_id, type_id, session_id).await?;
                if accepted_private_invite || active_private_room {
                    transaction.rollback().await?;
                    return Ok::<(bool, bool, bool, bool, Option<(u32, bool, u64)>, Option<(u32, u64)>, Option<(u32, u64, u64)>), sqlx::Error>((
                        true,
                        accepted_private_invite,
                        active_private_room,
                        false,
                        None,
                        None,
                        None,
                    ));
                }
            }

            // A public matchmaking Session can contain several independent
            // Parties. A non-room-host Party leader uses AbandonSession for
            // "leave with Party" as well; restore only that leader's captured
            // Party and leave every unrelated Party in the match untouched.
            if native_abandon
                && current_lobby
                    .as_ref()
                    .is_some_and(|(attributes, creator_id, _, _)| *creator_id != user_id && is_game_room_attributes(attributes) && !is_private_room_attributes(attributes))
            {
                if let Some(snapshot) = quick_match_party_departure.as_ref().filter(|snapshot| snapshot.departed_members.len() >= 2) {
                    let party_return = self.restore_quick_match_party_snapshot_async(&mut transaction, snapshot, type_id, session_id).await?;
                    if party_return.is_some() {
                        transaction.commit().await?;
                        return Ok::<(bool, bool, bool, bool, Option<(u32, bool, u64)>, Option<(u32, u64)>, Option<(u32, u64, u64)>), sqlx::Error>((
                            false,
                            false,
                            false,
                            false,
                            None,
                            None,
                            party_return,
                        ));
                    }
                }
            }

            if current_lobby
                .as_ref()
                .is_some_and(|(_, creator_id, participant_count, _)| *creator_id == user_id && *participant_count > 1)
            {
                // Native host migration is authoritative. Keep the old owner
                // and complete roster intact until the selected client calls
                // SplitSession; choosing a provisional owner here races the
                // election and splits the remaining Party.
                if native_abandon || native_migration_active {
                    sqlx::query("UPDATE game_sessions SET host_leave_pending = 1 WHERE id = ? AND type_id = ? AND creator_id = ? AND destroyed_at IS NULL")
                        .bind(session_id)
                        .bind(type_id)
                        .bind(user_id)
                        .execute(&mut *transaction)
                        .await?;
                }
                // "Leave with Party" removes one or more current Party peers
                // from the active public match immediately before the owner
                // abandons it. The persistent Party already contains a late
                // direct invitee, but retail may not have admitted that player
                // to its older native Party Session yet. Restore the captured
                // Party through one fresh lobby generation instead of
                // detaching the owner to a solo lobby and stranding the late
                // invitee in matchmaking.
                let party_return = if native_abandon {
                    if let Some(snapshot) = quick_match_party_departure.as_ref().filter(|snapshot| snapshot.departed_members.len() >= 2) {
                        self.restore_quick_match_party_snapshot_async(&mut transaction, snapshot, type_id, session_id).await?
                    } else {
                        None
                    }
                } else {
                    None
                };
                let native_party_handoff =
                    if party_return.is_none() && native_abandon && current_lobby.as_ref().is_some_and(|(attributes, _, _, _)| is_private_room_attributes(attributes)) {
                        let remaining_participants = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? AND user_id != ? ORDER BY user_id")
                            .bind(session_id)
                            .bind(user_id)
                            .fetch_all(&mut *transaction)
                            .await?
                            .into_iter()
                            .map(|(participant_id,)| participant_id)
                            .collect::<Vec<_>>();
                        Self::prepare_native_private_host_migration_party_async(&mut transaction, user_id, &remaining_participants).await?
                    } else {
                        None
                    };
                // Preserve the shared game Session for native SplitSession,
                // but restore the departing owner through an independent
                // single-member Party/lobby lane. Without this, the backend
                // correctly reports a local singleton while retail has no
                // native personal Session to display and remains in a
                // zero-member lobby. AbandonSession is the one-shot owner
                // departure signal; the following LeaveSession cleanup must
                // not enqueue a second restore.
                let owner_restore = if native_abandon && party_return.is_none() {
                    Self::detach_party_member_to_solo_async(&mut transaction, user_id, session_id).await?;
                    Self::queue_departed_member_lobby_restore_async(&mut transaction, user_id, type_id, session_id).await?
                } else {
                    None
                };
                transaction.commit().await?;
                if native_abandon && party_return.is_none() {
                    self.note_native_host_migration(type_id, session_id);
                }
                return Ok::<(bool, bool, bool, bool, Option<(u32, bool, u64)>, Option<(u32, u64)>, Option<(u32, u64, u64)>), sqlx::Error>((
                    party_return.is_none(),
                    false,
                    false,
                    true,
                    owner_restore,
                    native_party_handoff,
                    party_return,
                ));
            }

            if current_lobby
                .as_ref()
                .is_some_and(|(_, creator_id, _, _)| *creator_id != user_id && native_migration_active)
            {
                // During native migration retail unwinds the old connection
                // by sending LeaveSession from every surviving client before
                // the elected successor submits SplitSession. Those calls are
                // transport cleanup, not player departures. Removing the rows
                // here empties the Session before the elected host can claim
                // it, leaving every client stuck in host migration.
                transaction.commit().await?;
                return Ok::<(bool, bool, bool, bool, Option<(u32, bool, u64)>, Option<(u32, u64)>, Option<(u32, u64, u64)>), sqlx::Error>((
                    true, false, false, true, None, None, None,
                ));
            }

            let record_private_room_departure = current_lobby
                .as_ref()
                .is_some_and(|(attributes, creator_id, participant_count, _)| *creator_id != user_id && *participant_count > 1 && is_private_room_attributes(attributes));
            let defer_public_quick_match_member_leave = !native_abandon
                && (owner_removed_public_quick_match_member
                    || current_lobby.as_ref().is_some_and(|(attributes, creator_id, participant_count, host_leave_pending)| {
                        *host_leave_pending == 0
                            && *creator_id != user_id
                            && *participant_count > 1
                            && is_game_room_attributes(attributes)
                            && !is_private_room_attributes(attributes)
                    }));
            let detach_to_solo_party = !defer_public_quick_match_member_leave
                && (current_lobby.as_ref().is_some_and(|(attributes, creator_id, participant_count, host_leave_pending)| {
                    *host_leave_pending == 0 && is_game_room_attributes(attributes) && (*creator_id != user_id || *participant_count == 1)
                }) || (current_lobby.is_none() && owner_removed_game_room_member));
            let restore_solo_party_after_shared_lobby_leave = if current_lobby
                .as_ref()
                .is_some_and(|(attributes, creator_id, _, _)| *creator_id != user_id && session_attribute_value(attributes, 113) == Some(1))
            {
                sqlx::query_scalar::<_, i64>(
                    r"
                    SELECT EXISTS (
                        SELECT 1
                        FROM parties AS party
                        INNER JOIN party_members AS membership ON membership.party_id = party.id
                        WHERE party.leader_id = ?
                          AND membership.user_id = ?
                          AND (SELECT COUNT(*) FROM party_members AS member WHERE member.party_id = party.id) = 1
                    )
                    ",
                )
                .bind(user_id)
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await?
                    != 0
            } else {
                false
            };

            sqlx::query(
                r"
                DELETE FROM participants
                WHERE game_id = ?
                  AND user_id = ?
                  AND EXISTS (
                      SELECT 1 FROM game_sessions
                      WHERE id = ? AND type_id = ? AND destroyed_at IS NULL
                  )
                ",
            )
            .bind(session_id)
            .bind(user_id)
            .bind(session_id)
            .bind(type_id)
            .execute(&mut *transaction)
            .await?;
            Self::reconcile_game_session_async(&mut transaction, type_id, session_id).await?;
            // The lobby-leave trigger correctly retires the old shared-Party
            // membership. If an earlier room leave already established this
            // player's solo Party, recreate it in the same transaction so no
            // Event poll can observe an empty roster between lobby teardown
            // and the client's next CreateSession.
            if restore_solo_party_after_shared_lobby_leave {
                Self::ensure_party_for_leader_async(&mut transaction, user_id).await?;
            }
            if detach_to_solo_party {
                Self::detach_party_member_to_solo_async(&mut transaction, user_id, session_id).await?;
            }
            // Party state alone cannot recreate the retail client's native
            // personal-lobby Session. Queue the internal lobby-restore event
            // for both LeaveSession orderings; Hooks keeps this transition
            // silent and lets the native failed-search fallback create the
            // fresh Session before the old seeded target is retired.
            let member_restore = if detach_to_solo_party {
                Self::queue_departed_member_lobby_restore_async(&mut transaction, user_id, type_id, session_id).await?
            } else {
                None
            };
            if record_private_room_departure {
                // A member who leaves an otherwise live private match is no
                // longer part of that social Party. Keeping this row makes the
                // returning client project an impossible zero-member lobby and
                // also makes later invitations look as if the player never
                // left. If the owner exits in the same native unwind, the
                // short-lived departure correlation below still reconstructs
                // the exact N-1 Party.
                // Publish the short-lived correlation marker before releasing
                // SQLite's writer slot. A concurrent owner DeleteSession can
                // acquire that slot immediately after commit.
                self.record_game_room_departure(type_id, session_id, user_id);
            }
            // Publish while this transaction still owns SQLite's writer slot.
            // A Join admitted immediately after COMMIT must see this departure
            // and attach its exact accepted-invite witness before consuming it.
            // Never hold a map guard across COMMIT or other database work.
            let pending_key = (type_id, session_id, user_id);
            let pending_publication = if defer_public_quick_match_member_leave {
                self.pending_quick_match_member_leaves.lock().ok().map(|mut departures| {
                    let pending = super::PendingQuickMatchMemberLeave {
                        generation: self.next_cleanup_generation(),
                        recorded_at: Instant::now(),
                        accepted_invite: None,
                    };
                    let previous = departures.insert(pending_key, pending);
                    (pending.generation, previous)
                })
            } else {
                None
            };
            let commit_result = transaction.commit().await;
            #[cfg(test)]
            if defer_public_quick_match_member_leave {
                super::tests::coverage2_join_leave_commit_checkpoint(self, commit_result.is_ok(), user_id, type_id, session_id);
            }
            if let Err(error) = commit_result {
                // Undo only this failed publication. A concurrent Join/Leave
                // may already have replaced its generation after rollback.
                if let Some((generation, previous)) = pending_publication {
                    if let Ok(mut departures) = self.pending_quick_match_member_leaves.lock() {
                        if departures.get(&pending_key).is_some_and(|pending| pending.generation == generation) {
                            if let Some(previous) = previous {
                                departures.insert(pending_key, previous);
                            } else {
                                departures.remove(&pending_key);
                            }
                        }
                    }
                }
                return Err(error);
            }
            Ok::<(bool, bool, bool, bool, Option<(u32, bool, u64)>, Option<(u32, u64)>, Option<(u32, u64, u64)>), sqlx::Error>((
                false,
                false,
                false,
                false,
                member_restore,
                None,
                None,
            ))
        })??;
        if migration_pending {
            info!(self.logger, "HostLeaveAwaitingNativeMigration user_id={user_id} type_id={type_id} session_id={session_id}");
        } else if preserved {
            info!(
                self.logger,
                "SoloLobbyPreservedForRoomFallback user_id={user_id} type_id={type_id} session_id={session_id} accepted_private_invite={accepted_private_invite} active_private_room={active_private_room}"
            );
        }
        if let Some((lobby_session_id, seeded, restore_invites)) = member_restore {
            info!(
                self.logger,
                "DepartedMemberLobbyRestoreQueued user_id={user_id} type_id={type_id} departed_session_id={session_id} lobby_session_id={lobby_session_id} seeded={seeded} restore_invites={restore_invites}"
            );
        }
        if let Some((new_leader_id, member_count)) = native_party_handoff {
            info!(
                self.logger,
                "NativePrivatePartyLeadershipTransferred old_leader_id={user_id} new_leader_id={new_leader_id} type_id={type_id} session_id={session_id} member_count={member_count} transition=native-split-session no_restore_invites=true"
            );
        }
        if let Some((lobby_session_id, member_count, restore_invites)) = party_return {
            self.clear_game_room_departures(type_id, session_id);
            self.clear_pending_quick_match_member_leaves(type_id, session_id);
            if let Some(snapshot) = quick_match_party_departure.as_ref() {
                self.clear_quick_match_party_departure(type_id, session_id, snapshot.party_id);
            }
            info!(
                self.logger,
                "QuickMatchPartyReturnedWithHost host_id={user_id} type_id={type_id} match_session_id={session_id} lobby_session_id={lobby_session_id} member_count={member_count} late_member_restore_invites={restore_invites} source=coordinated-leave-with-party"
            );
        }
        Ok(())
    }

    /// Finalizes an explicit public-match member leave only after the short
    /// native command burst has closed. Retail emits Party-member `LeaveSession`
    /// before the room host's `AbandonSession` for "leave with Party"; mutating
    /// Party state on that first call loses the cohort before the owner signal
    /// arrives. A true solo leave has no matching owner/leader transaction and
    /// is restored to a one-member lobby after the correlation window.
    pub fn expire_pending_quick_match_member_leaves(&self) -> Result<usize> {
        let Some(_worker) = self.begin_cleanup()? else { return Ok(0); };
        let expired = self.pending_expired_quick_match_member_leaves();
        let mut resolved = 0usize;
        for (type_id, session_id, user_id, pending) in expired {
            let key = (type_id, session_id, user_id);
            let generation = pending.generation;
            let accepted_invite = pending.accepted_invite;
            if !cleanup_is_current(&self.pending_quick_match_member_leaves, &key, generation, |item| item.generation) { continue; }
            if self.native_host_migration_is_active(type_id, session_id) {
                info!(
                    self.logger,
                    "QuickMatchMemberLeaveAbsorbedByHostMigration user_id={user_id} type_id={type_id} session_id={session_id}"
                );
                complete_cleanup(&self.pending_quick_match_member_leaves, &key, generation, |item| item.generation);
                continue;
            }

            let (restore, invite_transition_pending) = run(async {
                let mut transaction = self.begin_write().await?;
                if !cleanup_is_current(&self.pending_quick_match_member_leaves, &key, generation, |item| item.generation) {
                    transaction.rollback().await?;
                    return Ok((None, None));
                }
                let still_in_departed_session = sqlx::query_scalar::<_, i64>("SELECT EXISTS(SELECT 1 FROM participants WHERE game_id = ? AND user_id = ?)")
                    .bind(session_id)
                    .bind(user_id)
                    .fetch_one(&mut *transaction)
                    .await?
                    != 0;
                let active_elsewhere = sqlx::query_scalar::<_, i64>(
                    r"
                    SELECT EXISTS (
                        SELECT 1
                        FROM participants AS participant
                        INNER JOIN game_sessions AS session ON session.id = participant.game_id
                        WHERE participant.user_id = ?
                          AND session.id != ?
                          AND session.type_id = ?
                          AND session.destroyed_at IS NULL
                          AND (';' || replace(session.attributes, ' ', '') || ';') LIKE '%;113=>0;%'
                    )
                    ",
                )
                .bind(user_id)
                .bind(session_id)
                .bind(type_id)
                .fetch_one(&mut *transaction)
                .await?
                    != 0;
                if still_in_departed_session || active_elsewhere {
                    transaction.rollback().await?;
                    return Ok::<(Option<(u32, bool, u64)>, Option<super::PendingAcceptedNormalInvite>), sqlx::Error>((None, None));
                }

                match Self::accepted_normal_invite_transition_async(&mut transaction, user_id, type_id, session_id, accepted_invite).await? {
                    Some(super::AcceptedNormalInviteTransition::Pending(transition)) => {
                        transaction.rollback().await?;
                        return Ok::<(Option<(u32, bool, u64)>, Option<super::PendingAcceptedNormalInvite>), sqlx::Error>((None, Some(transition)));
                    }
                    Some(super::AcceptedNormalInviteTransition::Joined) => {
                        transaction.rollback().await?;
                        return Ok::<(Option<(u32, bool, u64)>, Option<super::PendingAcceptedNormalInvite>), sqlx::Error>((None, None));
                    }
                    None => {}
                }

                Self::detach_party_member_to_solo_async(&mut transaction, user_id, session_id).await?;
                let restore = Self::queue_departed_member_lobby_restore_async(&mut transaction, user_id, type_id, session_id).await?;
                transaction.commit().await?;
                Ok::<(Option<(u32, bool, u64)>, Option<super::PendingAcceptedNormalInvite>), sqlx::Error>((restore, None))
            })??;
            if let Some(transition) = invite_transition_pending {
                if let Ok(mut departures) = self.pending_quick_match_member_leaves.lock() {
                    if let Some(current) = departures.get_mut(&key).filter(|item| item.generation == generation) {
                        current.generation = self.next_cleanup_generation();
                        current.recorded_at = Instant::now();
                        current.accepted_invite = Some(transition);
                    }
                }
                info!(
                    self.logger,
                    "QuickMatchLobbyRestoreDeferredForAcceptedInvite user_id={user_id} type_id={type_id} departed_session_id={session_id} transition=accepted-normal-invite"
                );
                continue;
            }
            complete_cleanup(&self.pending_quick_match_member_leaves, &key, generation, |item| item.generation);
            if let Some((lobby_session_id, seeded, restore_invites)) = restore {
                info!(
                    self.logger,
                    "QuickMatchSoloMemberLobbyRestoreQueued user_id={user_id} type_id={type_id} departed_session_id={session_id} lobby_session_id={lobby_session_id} seeded={seeded} restore_invites={restore_invites} source=expired-coordination-window"
                );
                resolved = resolved.saturating_add(1);
            }
        }
        Ok(resolved)
    }

    pub fn expire_native_host_migrations(&self) -> Result<usize> {
        let Some(_worker) = self.begin_cleanup()? else { return Ok(0); };
        let expired = self.pending_expired_native_host_migrations();
        let mut resolved = 0usize;
        for (type_id, session_id, pending) in expired {
            let key = (type_id, session_id);
            let current = || cleanup_is_current(&self.native_host_migrations, &key, pending.generation, |item| item.generation);
            if !current() { continue; }
            let cleanup = if let Some(cleanup) = pending.cleanup {
                cleanup
            } else {
                let Some((creator_id, _, participant_ids)) = self.game_session_diagnostics(type_id, session_id)? else {
                    complete_cleanup(&self.native_host_migrations, &key, pending.generation, |item| item.generation);
                    continue;
                };
                let outcome = self.delete_game_session_checked(creator_id, type_id, session_id, current)?;
                let cleanup = super::NativeMigrationCleanup {
                    creator_id,
                    remaining_participants: participant_ids.into_iter().filter(|id| *id != creator_id).collect(),
                    outcome,
                };
                // The host/Party transaction has committed. Retrying it against
                // a smaller roster would elect a different N-1 Party leader.
                if let Ok(mut migrations) = self.native_host_migrations.lock() {
                    if let Some(item) = migrations.get_mut(&key).filter(|item| item.generation == pending.generation) {
                        item.cleanup = Some(cleanup.clone());
                    }
                }
                cleanup
            };
            for participant_id in cleanup.remaining_participants {
                if !current() { break; }
                self.leave_game_session_checked(participant_id, type_id, session_id, false, current)?;
                if let Ok(mut migrations) = self.native_host_migrations.lock() {
                    if let Some(item) = migrations.get_mut(&key).filter(|item| item.generation == pending.generation) {
                        if let Some(progress) = item.cleanup.as_mut() {
                            progress.remaining_participants.retain(|id| *id != participant_id);
                        }
                    }
                }
            }
            if !current() { continue; }
            complete_cleanup(&self.native_host_migrations, &key, pending.generation, |item| item.generation);
            info!(self.logger, "NativeHostMigrationExpired type_id={type_id} session_id={session_id} old_host_id={} outcome={:?} fallback=n-minus-one-lobby", cleanup.creator_id, cleanup.outcome);
            resolved = resolved.saturating_add(1);
        }
        Ok(resolved)
    }

    /// Resolves a restore-member removal that was briefly deferred while the
    /// retail client had an opportunity to submit its immediately-following
    /// `SplitSession`. If no split arrived, apply the original owner removal
    /// through the normal single path so explicit Party cleanup is preserved.
    pub fn expire_lobby_restore_split_transitions(&self) -> Result<usize> {
        let Some(_worker) = self.begin_cleanup()? else { return Ok(0); };
        let expired = self.pending_expired_lobby_restore_split_transitions();
        let mut resolved = 0usize;
        for (type_id, session_id, user_id, pending) in expired {
            let key = (type_id, session_id, user_id);
            if !cleanup_is_current(&self.lobby_restore_split_transitions, &key, pending.generation, |item| item.generation) { continue; }
            if !pending.removal_deferred {
                complete_cleanup(&self.lobby_restore_split_transitions, &key, pending.generation, |item| item.generation);
                continue;
            }
            let Some((creator_id, _, participant_ids)) = self.game_session_diagnostics(type_id, session_id)? else {
                complete_cleanup(&self.lobby_restore_split_transitions, &key, pending.generation, |item| item.generation);
                continue;
            };
            if creator_id == user_id || !participant_ids.contains(&user_id) {
                complete_cleanup(&self.lobby_restore_split_transitions, &key, pending.generation, |item| item.generation);
                continue;
            }
            if run(self.remove_participants_checked_async(creator_id, type_id, session_id, vec![user_id], || cleanup_is_current(&self.lobby_restore_split_transitions, &key, pending.generation, |item| item.generation)))?? {
                info!(
                    self.logger,
                    "LobbyRestoreDeferredRemovalApplied type_id={type_id} session_id={session_id} owner_id={creator_id} participant_id={user_id} outcome=no-split"
                );
                resolved = resolved.saturating_add(1);
            }
            complete_cleanup(&self.lobby_restore_split_transitions, &key, pending.generation, |item| item.generation);
        }
        Ok(resolved)
    }

    /// Transfers ownership of an active game session to a remaining
    /// participant. `SplitSession` does not carry a separate host PID, so the
    /// authenticated caller is the new host selected by the game client.
    ///
    /// Returns `false` when the session is not active or the caller is not a
    /// participant. Returning a boolean lets the protocol layer map an invalid
    /// migration request to `AccessDenied` without exposing storage details.
    pub fn migrate_game_session_host(&self, user_id: u32, type_id: u32, session_id: u32) -> Result<bool> {
        let Some(mode)=run(self.current_game_mode(user_id))?? else { return Ok(false); };
        if !self.room_matches_mode(type_id, session_id, mode)? { return Ok(false); }
        let lobby_restore_split_active = self.lobby_restore_split_transition_is_active(type_id, session_id, user_id);
        // In public matchmaking, a departing Party member can submit
        // SplitSession a few milliseconds before its native Party leader sends
        // AbandonSession. Freeze and merge the authoritative Party here, but do
        // not restore it yet: a normal host migration uses the same Split call.
        // Only the old leader's subsequent Abandon confirms "leave with Party".
        let pre_migration_party_departure = if let Some((current_creator_id, attributes, _)) = self.game_session_diagnostics(type_id, session_id)? {
            if current_creator_id != user_id && is_game_room_attributes(&attributes) && !is_private_room_attributes(&attributes) {
                let mut recent_departures = self.recent_game_room_departures(type_id, session_id);
                recent_departures.extend(self.pending_quick_match_member_leaves(type_id, session_id));
                recent_departures.push(user_id);
                recent_departures.sort_unstable();
                recent_departures.dedup();
                self.capture_quick_match_party_departure(current_creator_id, type_id, session_id, &recent_departures)?
            } else {
                None
            }
        } else {
            None
        };

        if let Some(snapshot) = pre_migration_party_departure.clone() {
            self.remember_quick_match_party_departure(type_id, session_id, snapshot);
        }

        let migrated = run(async {
            let mut transaction = self.begin_write().await?;

            let current_session = sqlx::query_as::<_, (u32, String)>(
                r"
                SELECT g.creator_id, g.attributes
                FROM game_sessions AS g
                INNER JOIN participants AS p ON p.game_id = g.id
                WHERE g.id = ?
                  AND g.type_id = ?
                  AND g.destroyed_at IS NULL
                  AND p.user_id = ?
                  AND g.release_identity = ?
                  AND EXISTS(SELECT 1 FROM game_runs r WHERE r.user_id=p.user_id AND r.mode=g.gameplay_mode AND r.release_identity=g.release_identity AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
                ",
            )
            .bind(session_id)
            .bind(type_id)
            .bind(user_id)
            .bind(self.approved_release.storage_key())
            .fetch_optional(&mut *transaction)
            .await?;

            let Some((current_creator_id, attributes)) = current_session else {
                transaction.rollback().await?;
                return Ok::<bool, sqlx::Error>(false);
            };

            if lobby_restore_split_active {
                // After a kind-4 lobby restore, retail sends SplitSession as
                // the final join handshake immediately after the current
                // owner briefly tries to remove the joining participant. It
                // is not a lobby leadership election: promoting this caller
                // makes its local join state remove every existing member.
                // Acknowledge the handshake while preserving the established
                // lobby owner, complete roster, and authoritative Party.
                transaction.commit().await?;
                return Ok::<bool, sqlx::Error>(true);
            }

            let public_quick_match = is_game_room_attributes(&attributes) && !is_private_room_attributes(&attributes);
            if current_creator_id != user_id && !public_quick_match {
                // A lobby Session can migrate after its former leader has
                // already left a public match and the social Party has elected
                // a different successor. In that case SplitSession transfers
                // only the stale lobby transport owner; it must not overwrite
                // the already-authoritative Party leader. Private game-room
                // migration still retains the existing corrective behavior.
                let lobby_transport_only = session_attribute_value(&attributes, 113) == Some(1);
                let old_creator_still_leads_party = if lobby_transport_only {
                    sqlx::query_scalar::<_, i64>(
                        r"
                        SELECT EXISTS (
                            SELECT 1
                            FROM parties AS party
                            INNER JOIN party_members AS member ON member.party_id = party.id
                            WHERE member.user_id = ? AND party.leader_id = ?
                        )
                        ",
                    )
                    .bind(user_id)
                    .bind(current_creator_id)
                    .fetch_one(&mut *transaction)
                    .await?
                        != 0
                } else {
                    true
                };
                if old_creator_still_leads_party {
                    Self::transfer_party_leader_async(&mut transaction, current_creator_id, user_id).await?;
                }
            }

            // The caller has become the native host, so only its own stale
            // restore target is obsolete. Other Party members can still need
            // their pending kind-4 event to join the migrated lobby; consuming
            // the whole Party lane here strands late private-room invitees in
            // an empty personal-lobby projection.
            Self::cancel_private_room_lobby_restore_for_member_async(&mut transaction, user_id).await?;

            let result = sqlx::query(
                r"
                UPDATE game_sessions
                SET creator_id = ?, host_leave_pending = 0
                WHERE id = ?
                  AND type_id = ?
                  AND destroyed_at IS NULL
                ",
            )
            .bind(user_id)
            .bind(session_id)
            .bind(type_id)
            .execute(&mut *transaction)
            .await?;

            if current_creator_id != user_id {
                // The Party promotion above removes the old host. It is also
                // able to correct a provisional private-room Party leader
                // chosen before this authoritative SplitSession arrives.
                sqlx::query("DELETE FROM participants WHERE game_id = ? AND user_id = ?")
                    .bind(session_id)
                    .bind(current_creator_id)
                    .execute(&mut *transaction)
                    .await?;
            }

            transaction.commit().await?;
            Ok::<bool, sqlx::Error>(result.rows_affected() == 1)
        })??;
        if migrated {
            self.clear_lobby_restore_split_transition(type_id, session_id, user_id);
            self.clear_native_host_migration(type_id, session_id);
        }
        Ok(migrated)
    }

    /// Restores the caller's own just-abandoned session for the retail
    /// `AbandonSession -> SplitSession -> AddParticipants` migration sequence.
    /// Only the original creator may revive the exact destroyed key, so an
    /// arbitrary client cannot resurrect another player's stale session.
    pub fn restore_abandoned_game_session_for_split(&self, user_id: u32, type_id: u32, session_id: u32) -> Result<bool> {
        let restored = run(async {
            let mut transaction = self.begin_write().await?;
            let result = sqlx::query(
                r"
                UPDATE game_sessions
                SET creator_id = ?, destroyed_at = NULL, host_leave_pending = 0
                WHERE id = ?
                  AND type_id = ?
                  AND creator_id = ?
                  AND destroyed_at IS NOT NULL
                  AND release_identity = ?
                  AND EXISTS(SELECT 1 FROM game_runs r WHERE r.user_id=game_sessions.creator_id AND r.mode=game_sessions.gameplay_mode AND r.release_identity=game_sessions.release_identity AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))
                ",
            )
            .bind(user_id)
            .bind(session_id)
            .bind(type_id)
            .bind(user_id)
            .bind(self.approved_release.storage_key())
            .execute(&mut *transaction)
            .await?;

            if result.rows_affected() != 1 {
                transaction.rollback().await?;
                return Ok::<bool, sqlx::Error>(false);
            }

            sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                .bind(session_id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Ok::<bool, sqlx::Error>(true)
        })??;
        Ok(restored)
    }

    pub fn update_game_session(&self, caller_id: u32, type_id: u32, game_id: u32, attributes: String) -> Result<bool> {
        // Check ownership and liveness in the write itself, including host migration.
        let updated = run(sqlx::query("UPDATE game_sessions SET attributes = ? WHERE id = ? AND type_id = ? AND creator_id = ? AND destroyed_at IS NULL")
            .bind(attributes)
            .bind(game_id)
            .bind(type_id)
            .bind(caller_id)
            .execute(&self.pool))??;

        Ok(updated.rows_affected() == 1)
    }

    pub fn search_sessions(&self, type_id: u32, exclude_user: Option<u32>) -> Result<Vec<GameSession>> {
        run(self.search_sessions_async(type_id, exclude_user))?
    }

    async fn search_sessions_async(&self, type_id: u32, exclude_user: Option<u32>) -> Result<Vec<GameSession>> {
        // One WAL read snapshot for room metadata, roster and endpoints. Discovery
        // is advisory; JoinSession must still check the current room under its writer.
        let mut transaction = self.pool.begin().await?;
        let mut sessions: Vec<GameSession> = if let Some(uid) = exclude_user {
            sqlx::query_as(
                "SELECT type_id as session_type, id as session_id, creator_id, attributes FROM game_sessions WHERE type_id = ? AND creator_id != ? AND destroyed_at IS NULL AND release_identity=?",
            )
            .bind(type_id)
            .bind(uid)
            .bind(self.approved_release.storage_key()).fetch_all(&mut *transaction).await?
        } else {
            sqlx::query_as("SELECT type_id as session_type, id as session_id, creator_id, attributes FROM game_sessions WHERE type_id = ? AND destroyed_at IS NULL AND release_identity=?")
                .bind(type_id)
                .bind(self.approved_release.storage_key()).fetch_all(&mut *transaction).await?
        };

        #[cfg(test)]
        self.discovery_checkpoint();

        for session in &mut sessions {
            session.participants = sqlx::query_as("SELECT user_id, username as name FROM participants p, users u WHERE u.id = user_id AND game_id = ?")
                .bind(session.session_id)
                .fetch_all(&mut *transaction).await?;
            for participant in &mut session.participants {
                // Preserve the native self-endpoint exclusion.
                if matches!(exclude_user, Some(pid) if pid == participant.user_id) {
                    continue;
                }
                participant.station_urls = sqlx::query_as("SELECT url FROM station_urls WHERE user_id = ?")
                    .bind(participant.user_id)
                    .fetch_all(&mut *transaction).await?
                    .into_iter()
                    .map(|r: (String,)| r.0)
                    .collect();
            }
        }
        transaction.commit().await?;

        // A joinable session must have a live host participant and at least one
        // registered endpoint. Keep the existing native handover filtering.
        sessions.retain(|session| {
            session.participants.iter().any(|participant|
                participant.user_id == session.creator_id && !participant.station_urls.is_empty())
        });
        Ok(sessions)
    }

    /// Removes participants only when the authenticated caller still owns the
    /// exact active session.  The protocol exposes participant ids supplied by
    /// an untrusted client, so checking login at the RMC boundary is not enough:
    /// without this transactional ownership check any logged-in user could
    /// evict players from another session.
    pub async fn remove_participants_async(&self, caller_id: u32, type_id: u32, session_id: u32, participants: Vec<u32>) -> Result<bool> {
        self.remove_participants_checked_async(caller_id, type_id, session_id, participants, || true).await
    }

    pub(super) async fn remove_participants_checked_async(&self, caller_id: u32, type_id: u32, session_id: u32, participants: Vec<u32>, may_proceed: impl Fn() -> bool) -> Result<bool> {
        let mut transaction = self.begin_write().await?;
        if !may_proceed() {
            transaction.rollback().await?;
            return Ok(false);
        }
        // Acquire SQLite's write lock while checking ownership so a concurrent
        // host migration cannot race between authorization and deletion.
        let authorized = sqlx::query("UPDATE game_sessions SET creator_id = creator_id WHERE id = ? AND type_id = ? AND creator_id = ? AND destroyed_at IS NULL")
            .bind(session_id)
            .bind(type_id)
            .bind(caller_id)
            .execute(&mut *transaction)
            .await?;
        if authorized.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(false);
        }

        let session_state = sqlx::query_as::<_, (String, i64)>(
            r"
            SELECT
                attributes,
                (SELECT COUNT(*) FROM participants AS member WHERE member.game_id = game_sessions.id)
            FROM game_sessions
            WHERE id = ?
              AND type_id = ?
              AND creator_id = ?
              AND destroyed_at IS NULL
            ",
        )
        .bind(session_id)
        .bind(type_id)
        .bind(caller_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let is_party_lobby = session_state.as_ref().is_some_and(|(attributes, _)| session_attribute_value(attributes, 113) == Some(1));
        let is_populated_game_room = session_state
            .as_ref()
            .is_some_and(|(attributes, participant_count)| *participant_count > 1 && is_game_room_attributes(attributes));
        let mut removed_game_room_members = Vec::new();
        let mut deferred_restore_members = Vec::new();

        for participant_id in participants.into_iter().collect::<HashSet<_>>() {
            if is_party_lobby && participant_id != caller_id && self.defer_lobby_restore_split_removal(type_id, session_id, participant_id) {
                deferred_restore_members.push(participant_id);
                continue;
            }
            let removed = sqlx::query("DELETE FROM participants WHERE game_id = ? AND user_id = ?")
                .bind(session_id)
                .bind(participant_id)
                .execute(&mut *transaction)
                .await?;
            if is_party_lobby && participant_id != caller_id && removed.rows_affected() == 1 {
                // RemoveParticipants is the explicit Party-removal decision.
                // The generic lobby-leave trigger intentionally keeps a member
                // while an active private-room row exists, which is correct for
                // lobby-to-game transitions but leaves stale membership after a
                // real "Leave Party" action. Bind the cleanup to the caller's
                // exact Party so unrelated parties and native host migration are
                // unaffected.
                let detached = sqlx::query(
                    r"
                    DELETE FROM party_members
                    WHERE user_id = ?
                      AND party_id = (
                          SELECT party_id FROM party_members WHERE user_id = ?
                      )
                    ",
                )
                .bind(participant_id)
                .bind(caller_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
                    > 0;
                if detached {
                    Self::ensure_party_for_leader_async(&mut transaction, participant_id).await?;
                }
            }
            if is_populated_game_room && participant_id != caller_id && removed.rows_affected() == 1 {
                removed_game_room_members.push(participant_id);
            }
        }
        Self::reconcile_game_session_async(&mut transaction, type_id, session_id).await?;
        transaction.commit().await?;
        for participant_id in deferred_restore_members {
            info!(
                self.logger,
                "LobbyRestoreRemovalDeferred type_id={type_id} session_id={session_id} owner_id={caller_id} participant_id={participant_id} expected=split-session"
            );
        }
        for participant_id in removed_game_room_members {
            self.record_game_room_departure(type_id, session_id, participant_id);
            if let Some(snapshot) = self.note_quick_match_party_member_departed(type_id, session_id, participant_id) {
                let restored = {
                    let mut restore_transaction = self.begin_write().await?;
                    let restored = self
                        .restore_quick_match_party_snapshot_async(&mut restore_transaction, &snapshot, type_id, session_id)
                        .await?;
                    restore_transaction.commit().await?;
                    restored
                };
                if let Some((lobby_session_id, member_count, restore_invites)) = restored {
                    self.clear_game_room_departures(type_id, session_id);
                    info!(
                        self.logger,
                        "QuickMatchPartyCohortReturned match_session_id={session_id} lobby_session_id={} party_leader_id={} member_count={member_count} restore_invites={restore_invites} source=party-snapshot",
                        lobby_session_id,
                        snapshot.leader_id
                    );
                }
            }
        }
        Ok(true)
    }

    pub fn remove_participants(&self, caller_id: u32, type_id: u32, session_id: u32, participants: Vec<u32>) -> Result<bool> {
        run(self.remove_participants_async(caller_id, type_id, session_id, participants))?
    }

    pub fn search_sessions_with_participants(&self, type_id: u32, participant_ids: &[u32]) -> Result<Vec<GameSession>> {
        run(self.search_sessions_with_participants_async(type_id, participant_ids))?
    }

    pub async fn search_sessions_with_participants_async(&self, type_id: u32, participant_ids: &[u32]) -> Result<Vec<GameSession>> {
        let mut transaction = self.pool.begin().await?;
        let mut sessions: Vec<GameSession> = if participant_ids.is_empty() {
            let query = sqlx::query_as(
                r"SELECT
                        g.type_id as session_type,
                        g.id as session_id,
                        g.creator_id,
                        g.attributes
                    FROM game_sessions AS g
                    WHERE g.type_id = ? AND g.destroyed_at IS NULL
                    AND g.release_identity=?
                    ORDER BY g.id",
            )
            .bind(type_id);
            info!(self.logger, "Searching all active sessions: {}", query.sql());
            query.bind(self.approved_release.storage_key()).fetch_all(&mut *transaction).await?
        } else {
            let placeholders = std::iter::repeat('?').take(participant_ids.len()).intersperse(',').collect::<String>();
            let sql = format!(
                r"SELECT
                        g.type_id as session_type,
                        g.id as session_id,
                        g.creator_id,
                        g.attributes
                    FROM game_sessions AS g
                    WHERE g.type_id = ? AND g.destroyed_at IS NULL AND g.id IN (
                        SELECT game_id FROM participants WHERE user_id IN ({placeholders})
                    )
                    AND g.release_identity=?
                    ORDER BY g.id"
            );
            let mut query = sqlx::query_as(&sql).bind(type_id);
            for participant_id in participant_ids {
                query = query.bind(participant_id);
            }
            info!(self.logger, "Searching sessions with participants: {}", query.sql());
            query.bind(self.approved_release.storage_key()).fetch_all(&mut *transaction).await?
        };

        #[cfg(test)]
        self.discovery_checkpoint();

        for session in &mut sessions {
            session.participants = sqlx::query_as(
                r"
                SELECT user_id, username as name
                FROM participants p, users u
                WHERE u.id = user_id AND game_id = ?
                ORDER BY user_id
                ",
            )
            .bind(session.session_id)
            .fetch_all(&mut *transaction)
            .await?;

            for participant in &mut session.participants {
                participant.station_urls = sqlx::query_as("SELECT url FROM station_urls WHERE user_id = ? ORDER BY url")
                    .bind(participant.user_id)
                    .fetch_all(&mut *transaction)
                    .await?
                    .into_iter()
                    .map(|row: (String,)| row.0)
                    .collect();
            }
        }

        sessions.retain(|session| {
            session
                .participants
                .iter()
                .any(|participant| participant.user_id == session.creator_id && !participant.station_urls.is_empty())
        });
        transaction.commit().await?;
        Ok(sessions)
    }

    pub async fn list_game_sessions_async(&self) -> Result<Vec<GameSession>> {
        let mut transaction = self.pool.begin().await?;
        let mut sessions: Vec<GameSession> = sqlx::query_as(
            r"
        SELECT
            g.type_id as session_type,
            g.id as session_id,
            g.creator_id,
            g.attributes
        FROM game_sessions AS g
        WHERE destroyed_at IS NULL
        ",
        )
        .fetch_all(&mut *transaction)
        .await?;

        #[cfg(test)]
        self.discovery_checkpoint();

        for session in &mut sessions {
            session.participants = sqlx::query_as(
                r"
                SELECT
                    user_id,
                    username as name
                FROM participants p, users u
                WHERE u.id = user_id AND game_id = ?
                ",
            )
            .bind(session.session_id)
            .fetch_all(&mut *transaction)
            .await?;

            for participant in &mut session.participants {
                participant.station_urls = sqlx::query_as(
                    r"
                    SELECT url
                    FROM station_urls
                    WHERE user_id = ?
                    ",
                )
                .bind(participant.user_id)
                .fetch_all(&mut *transaction)
                .await?
                .into_iter()
                .map(|r: (String,)| r.0)
                .collect();
            }
        }
        transaction.commit().await?;
        Ok(sessions)
    }

    pub async fn delete_game_session_by_id_async(&self, session_id: u32) -> Result<()> {
        sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
