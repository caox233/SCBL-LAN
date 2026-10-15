use std::collections::HashMap;
use std::time::Instant;

use super::project_private_invite_session_class_only;
use super::run;
use super::GameSession;
use super::Invite;
use super::InvitedGameSession;
use super::Result;
use super::Storage;
use super::INVITE_KIND_LOBBY_PARTY;
use super::INVITE_KIND_LOBBY_RESTORE;
use super::INVITE_KIND_PARTY_FOLLOW;
use super::INVITE_KIND_PRIVATE_ROOM;
use super::MANUAL_INVITATION_SEARCH_TTL;

impl Storage {
    const MANUAL_INVITE_EXPIRY_SQL: &'static str = "+30 seconds";

    fn prune_manual_invitation_searches(searches: &mut HashMap<(u32, u32), Instant>) {
        searches.retain(|_, created| created.elapsed() <= MANUAL_INVITATION_SEARCH_TTL);
    }

    pub fn note_manual_invitation_search(&self, receiver_id: u32, session_type: u32) {
        if let Ok(mut searches) = self.manual_invitation_searches.lock() {
            Self::prune_manual_invitation_searches(&mut searches);
            searches.insert((receiver_id, session_type), Instant::now());
        }
    }

    pub fn clear_manual_invitation_search(&self, receiver_id: u32, session_type: u32) {
        if let Ok(mut searches) = self.manual_invitation_searches.lock() {
            searches.remove(&(receiver_id, session_type));
        }
    }

    #[cfg(test)]
    pub fn manual_invitation_search_is_active(&self, receiver_id: u32, session_type: u32) -> bool {
        let Ok(mut searches) = self.manual_invitation_searches.lock() else {
            return false;
        };
        Self::prune_manual_invitation_searches(&mut searches);
        searches.contains_key(&(receiver_id, session_type))
    }

    /// Returns true when another invitation to the selected target would be
    /// redundant. Lobby invitations use persistent Party membership (with the
    /// lobby participant list as a compatibility fallback); room invitations
    /// use the exact room participant list. Party identity follows the current
    /// session host so host migration does not create a second logical party.
    pub async fn invite_target_already_contains_async(&self, receiver_id: u32, kind: i32, session_id: u32) -> Result<bool> {
        let (already_joined,): (i64,) = if kind == INVITE_KIND_LOBBY_PARTY {
            sqlx::query_as(
                r"
                SELECT EXISTS(
                SELECT 1
                    FROM party_members AS host_member
                    INNER JOIN party_members AS member ON member.party_id = host_member.party_id
                    WHERE host_member.user_id = (
                        SELECT creator_id FROM game_sessions WHERE id = ?
                    )
                      AND member.user_id = ?
                    UNION ALL
                    SELECT 1
                    FROM participants
                    WHERE game_id = ? AND user_id = ?
                )
                ",
            )
            .bind(session_id)
            .bind(receiver_id)
            .bind(session_id)
            .bind(receiver_id)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query_as("SELECT EXISTS(SELECT 1 FROM participants WHERE game_id = ? AND user_id = ?)")
                .bind(session_id)
                .bind(receiver_id)
                .fetch_one(&self.pool)
                .await?
        };
        Ok(already_joined != 0)
    }

    /// Creates one pending invitation bound to the inviter's exact current
    /// session. `kind=1` is a private-room direct invite and `kind=2` is a
    /// lobby-party invite. Multiple receivers may point at the same session.
    pub async fn add_invite_async(&self, sender_id: u32, receiver_id: u32, kind: i32, session_type: u32, session_id: u32) -> Result<Option<i64>> {
        if !matches!(kind, INVITE_KIND_PRIVATE_ROOM | INVITE_KIND_LOBBY_PARTY) {
            return Ok(None);
        }
        let published_lobby_session_id = self.published_game_session(sender_id);
        let mut transaction = self.begin_write().await?;
        let session_creator: Option<(u32, String)> = sqlx::query_as("SELECT creator_id, attributes FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL")
            .bind(session_id)
            .bind(session_type)
            .fetch_optional(&mut *transaction)
            .await?;
        let Some((session_creator_id, session_attributes)) = session_creator else {
            transaction.rollback().await?;
            return Ok(None);
        };
        sqlx::query("DELETE FROM invites WHERE consumed_at IS NOT NULL OR expires_at IS NULL OR expires_at <= CURRENT_TIMESTAMP")
            .execute(&mut *transaction)
            .await?;

        let valid_sender: (i64,) = sqlx::query_as(
            r"
            SELECT EXISTS (
                SELECT 1
                FROM game_sessions AS g
                INNER JOIN participants AS host
                    ON host.game_id = g.id AND host.user_id = g.creator_id
                WHERE g.id = ?
                  AND g.type_id = ?
                  AND g.destroyed_at IS NULL
                  AND EXISTS (SELECT 1 FROM station_urls AS endpoint WHERE endpoint.user_id = g.creator_id)
                  AND (
                      EXISTS (
                          SELECT 1 FROM participants AS sender_participant
                          WHERE sender_participant.game_id = g.id
                            AND sender_participant.user_id = ?
                      )
                      OR EXISTS (
                          SELECT 1
                          FROM party_members AS sender_party_member
                          INNER JOIN party_members AS host_party_member
                              ON host_party_member.party_id = sender_party_member.party_id
                          WHERE sender_party_member.user_id = ?
                            AND host_party_member.user_id = g.creator_id
                      )
                  )
            )
            ",
        )
        .bind(session_id)
        .bind(session_type)
        .bind(sender_id)
        .bind(sender_id)
        .fetch_one(&mut *transaction)
        .await?;
        if valid_sender.0 == 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        let party_id = if matches!(kind, INVITE_KIND_PRIVATE_ROOM | INVITE_KIND_LOBBY_PARTY) {
            // A direct invitation into an already running room is still a
            // social Party admission. Bind it to the inviter's exact Party so
            // a member invitation and a host invitation converge to the same
            // generation. Public matchmaking can expose a newer native lobby
            // than the persistent Party rows after repeated leave/split flows;
            // reconcile only from the inviter's explicit published lobby so
            // unrelated Parties sharing the same match are never merged.
            // Lobby invitations remain bound to the lobby host.
            let party_owner_id = if kind == INVITE_KIND_PRIVATE_ROOM { sender_id } else { session_creator_id };
            let published_party =
                if kind == INVITE_KIND_PRIVATE_ROOM && super::is_game_room_attributes(&session_attributes) && !super::is_private_room_attributes(&session_attributes) {
                    if let Some(published_lobby_session_id) = published_lobby_session_id {
                        Self::synchronize_party_from_published_lobby_async(&mut transaction, sender_id, published_lobby_session_id).await?
                    } else {
                        None
                    }
                } else {
                    None
                };
            if let Some(party_id) = published_party {
                Some(party_id)
            } else {
                let existing_party = sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = ?")
                    .bind(party_owner_id)
                    .fetch_optional(&mut *transaction)
                    .await?;
                Some(if let Some(party_id) = existing_party {
                    party_id
                } else {
                    Self::ensure_party_for_leader_async(&mut transaction, party_owner_id).await?
                })
            }
        } else {
            None
        };
        // Every member of one room points the receiver at the same session.
        // Treat those clicks as one logical invitation regardless of which
        // member clicked: emitting two accepted events for one target can make
        // retail start a second session transition while the first is active.
        // Keep the first server ID, sender and expiry stable, including while
        // that generation has already been accepted but not yet joined.
        let existing: Option<(i64,)> = sqlx::query_as(
            r"
            SELECT id
            FROM invites
            WHERE receiver = ?
              AND kind = ?
              AND session_type = ?
              AND session_id = ?
              AND consumed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            ORDER BY created DESC, id DESC
            LIMIT 1
            ",
        )
        .bind(receiver_id)
        .bind(kind)
        .bind(session_type)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some((invite_id,)) = existing {
            transaction.commit().await?;
            info!(
                self.logger,
                "InviteDeduplicated invite_id={invite_id} sender_id={sender_id} receiver_id={receiver_id} kind={kind} session_type={session_type} session_id={session_id} expiry_extended=false"
            );
            return Ok(Some(invite_id));
        }

        // A different target is a new invitation generation. Retire any old
        // pending generation for this sender/receiver/lane before inserting it.
        sqlx::query(
            r"
            UPDATE invites
            SET consumed_at = CURRENT_TIMESTAMP
            WHERE sender = ?
              AND receiver = ?
              AND kind = ?
              AND accepted_at IS NULL
              AND consumed_at IS NULL
            ",
        )
        .bind(sender_id)
        .bind(receiver_id)
        .bind(kind)
        .execute(&mut *transaction)
        .await?;

        let result = sqlx::query(
            r"
            INSERT INTO invites (
                sender, receiver, kind, session_type, session_id, party_id, expires_at
            )
            VALUES (?, ?, ?, ?, ?, ?, datetime('now', ?))
            ",
        )
        .bind(sender_id)
        .bind(receiver_id)
        .bind(kind)
        .bind(session_type)
        .bind(session_id)
        .bind(party_id)
        .bind(Self::MANUAL_INVITE_EXPIRY_SQL)
        .execute(&mut *transaction)
        .await?;
        let invite_id = result.last_insert_rowid();
        transaction.commit().await?;
        info!(
            self.logger,
            "InviteCreated invite_id={invite_id} sender_id={sender_id} receiver_id={receiver_id} kind={kind} session_type={session_type} session_id={session_id}"
        );
        Ok(Some(invite_id))
    }

    pub(super) async fn consume_all_invites_for_receiver_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, receiver_id: u32) -> sqlx::Result<u64> {
        Ok(sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE receiver = ? AND consumed_at IS NULL")
            .bind(receiver_id)
            .execute(&mut **transaction)
            .await?
            .rows_affected())
    }

    /// Retires one exact invite generation after the protocol layer proves
    /// that its in-memory Uplay publication is no longer current. Binding the
    /// receiver prevents one client from consuming another user's invite.
    pub async fn consume_invite_async(&self, receiver_id: u32, invite_id: i64) -> Result<bool> {
        Ok(
            sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE id = ? AND receiver = ? AND consumed_at IS NULL")
                .bind(invite_id)
                .bind(receiver_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                == 1,
        )
    }

    async fn cleanup_invalid_invites_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> sqlx::Result<()> {
        sqlx::query(
            r"
            DELETE FROM invites
            WHERE consumed_at IS NOT NULL
               OR expires_at IS NULL
               OR expires_at <= CURRENT_TIMESTAMP
               OR NOT EXISTS (
                   SELECT 1
                   FROM game_sessions AS g
                   INNER JOIN participants AS p
                       ON p.game_id = g.id AND p.user_id = g.creator_id
                   WHERE g.id = invites.session_id
                     AND g.type_id = invites.session_type
                     AND invites.kind IN (1, 2, 3, 4, 5)
                     AND (
                         EXISTS (
                             SELECT 1 FROM participants AS sender_participant
                             WHERE sender_participant.game_id = g.id
                               AND sender_participant.user_id = invites.sender
                         )
                         OR EXISTS (
                             SELECT 1
                             FROM party_members AS sender_party_member
                             INNER JOIN party_members AS host_party_member
                                 ON host_party_member.party_id = sender_party_member.party_id
                             WHERE sender_party_member.user_id = invites.sender
                               AND host_party_member.user_id = g.creator_id
                         )
                     )
                     AND g.destroyed_at IS NULL
                     AND EXISTS (SELECT 1 FROM station_urls AS s WHERE s.user_id = g.creator_id)
               )
            ",
        )
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    /// Delivers an unaccepted invitation repeatedly at a controlled interval.
    /// Delivery is not treated as acknowledgement; only `AcceptInvite` marks the
    /// invitation accepted, and only `JoinSession` consumes it.
    pub async fn take_invite_async(&self, user_id: u32) -> Result<Option<Invite>> {
        let mut transaction = self.begin_write().await?;
        Self::cleanup_invalid_invites_async(&mut transaction).await?;
        let redundant_party_follows = sqlx::query(
            r"
            UPDATE invites
            SET consumed_at = CURRENT_TIMESTAMP
            WHERE receiver = ?
              AND kind = ?
              AND accepted_at IS NULL
              AND consumed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
              AND EXISTS (
                  SELECT 1
                  FROM participants AS receiver_participant
                  WHERE receiver_participant.game_id = invites.session_id
                    AND receiver_participant.user_id = invites.receiver
              )
            ",
        )
        .bind(user_id)
        .bind(INVITE_KIND_PARTY_FOLLOW)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        let mut invite: Option<Invite> = sqlx::query_as(
            r"
            SELECT
                id,
                sender,
                receiver,
                kind,
                session_type,
                session_id,
                COALESCE(delivery_count, 0) AS delivery_count
            FROM invites
            WHERE receiver = ?
              AND accepted_at IS NULL
              AND consumed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
              AND (delivered_at IS NULL OR delivered_at <= datetime('now', '-3 seconds'))
              AND (kind != ? OR created <= datetime('now', '-2 seconds'))
              AND (
                  kind != ?
                  OR (
                      NOT EXISTS (
                          SELECT 1
                          FROM participants AS old_room_member
                          INNER JOIN game_sessions AS old_room
                              ON old_room.id = old_room_member.game_id
                          WHERE old_room_member.user_id = invites.receiver
                            AND old_room.destroyed_at IS NULL
                            AND (';' || replace(old_room.attributes, ' ', '') || ';') LIKE '%;113=>0;%'
                      )
                      AND NOT EXISTS (
                          SELECT 1
                          FROM participants AS leader_room_member
                          INNER JOIN game_sessions AS leader_room
                              ON leader_room.id = leader_room_member.game_id
                          WHERE leader_room_member.user_id = invites.sender
                            AND leader_room.destroyed_at IS NULL
                            AND (';' || replace(leader_room.attributes, ' ', '') || ';') LIKE '%;113=>0;%'
                      )
                  )
              )
            ORDER BY created DESC, id DESC
            LIMIT 1
            ",
        )
        .bind(user_id)
        .bind(INVITE_KIND_PARTY_FOLLOW)
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(invite_ref) = invite.as_ref().filter(|invite| invite.kind == INVITE_KIND_LOBBY_RESTORE) {
            if let Some(gate) = self.quick_match_former_host_restore_gate(invite_ref.session_type, user_id) {
                let invite_party_id = sqlx::query_scalar::<_, i64>("SELECT party_id FROM invites WHERE id = ?")
                    .bind(invite_ref.id)
                    .fetch_optional(&mut *transaction)
                    .await?;
                if invite_party_id != Some(gate.party_id) {
                    self.clear_quick_match_former_host_restore_gate(invite_ref.session_type, user_id);
                } else {
                    let newer_personal_lobby_exists = sqlx::query_scalar::<_, i64>(
                        r"
                    SELECT EXISTS (
                        SELECT 1
                        FROM game_sessions AS lobby
                        INNER JOIN participants AS owner
                            ON owner.game_id = lobby.id AND owner.user_id = ?
                        WHERE lobby.type_id = ?
                          AND lobby.id > ?
                          AND lobby.creator_id = ?
                          AND lobby.destroyed_at IS NULL
                          AND (';' || replace(lobby.attributes, ' ', '') || ';') LIKE '%;113=>1;%'
                    )
                    ",
                    )
                    .bind(user_id)
                    .bind(invite_ref.session_type)
                    .bind(gate.seed_lobby_session_id)
                    .bind(user_id)
                    .fetch_one(&mut *transaction)
                    .await?
                        != 0;
                    if newer_personal_lobby_exists {
                        self.clear_quick_match_former_host_restore_gate(invite_ref.session_type, user_id);
                    } else {
                        invite = None;
                    }
                }
            }
        }
        if let Some(invite_mut) = invite.as_mut() {
            sqlx::query("UPDATE invites SET delivered_at = CURRENT_TIMESTAMP, delivery_count = COALESCE(delivery_count, 0) + 1 WHERE id = ?")
                .bind(invite_mut.id)
                .execute(&mut *transaction)
                .await?;
            invite_mut.delivery_count += 1;
        }
        transaction.commit().await?;
        if redundant_party_follows > 0 {
            info!(
                self.logger,
                "PartyFollowInviteSuppressedAlreadyJoined receiver_id={user_id} count={redundant_party_follows} fallback_delay_seconds=2"
            );
        }
        Ok(invite)
    }

    /// Acknowledges a specific invitation for the authenticated receiver. The
    /// operation is idempotent so a retrying overlay cannot corrupt state.
    // Test convenience only; runtime acceptance uses the checked readiness callback.
    #[cfg(test)]
    pub async fn accept_invite_async(&self, user_id: u32, invite_id: i64) -> Result<Option<Invite>> {
        self.accept_invite_checked_async(user_id, invite_id, |_, _| Ok(())).await
    }

    /// Check transient protocol readiness under the same write transaction as
    /// authorization, before accepting or retiring competing invitations.
    /// Callers coordinating the publication registry must acquire it first.
    pub async fn accept_invite_checked_async<F>(&self, user_id: u32, invite_id: i64, check: F) -> Result<Option<Invite>>
    where
        F: FnOnce(&Invite, Option<(u32, String)>) -> Result<()> + Send,
    {
        let mut transaction = self.begin_write().await?;
        Self::cleanup_invalid_invites_async(&mut transaction).await?;
        let invite: Option<Invite> = sqlx::query_as(
            r"
            SELECT
                id,
                sender,
                receiver,
                kind,
                session_type,
                session_id,
                COALESCE(delivery_count, 0) AS delivery_count
            FROM invites
            WHERE id = ?
              AND receiver = ?
              AND consumed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            ",
        )
        .bind(invite_id)
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await?;

        let authorized_target = if let Some(invite) = invite
            .as_ref()
            .filter(|invite| matches!(invite.kind, INVITE_KIND_PRIVATE_ROOM | INVITE_KIND_LOBBY_PARTY | INVITE_KIND_PARTY_FOLLOW))
        {
            let (valid_target,): (i64,) = sqlx::query_as(
                r"
                SELECT EXISTS(
                    SELECT 1
                    FROM game_sessions AS g
                    INNER JOIN participants AS host
                        ON host.game_id = g.id AND host.user_id = g.creator_id
                    WHERE g.id = ?
                      AND g.type_id = ?
                      AND g.destroyed_at IS NULL
                      AND EXISTS (
                          SELECT 1 FROM station_urls AS endpoint
                          WHERE endpoint.user_id = g.creator_id
                      )
                       AND (
                           EXISTS (
                               SELECT 1 FROM participants AS sender_participant
                               WHERE sender_participant.game_id = g.id
                                 AND sender_participant.user_id = ?
                           )
                           OR EXISTS (
                               SELECT 1
                               FROM party_members AS sender_party_member
                               INNER JOIN party_members AS host_party_member
                                   ON host_party_member.party_id = sender_party_member.party_id
                               WHERE sender_party_member.user_id = ?
                                 AND host_party_member.user_id = g.creator_id
                           )
                       )
                )
                ",
            )
            .bind(invite.session_id)
            .bind(invite.session_type)
            .bind(invite.sender)
            .bind(invite.sender)
            .fetch_one(&mut *transaction)
            .await?;
            if valid_target == 0 {
                transaction.rollback().await?;
                return Ok(None);
            }

            // Accepting a private-room or party-follow invitation authorizes exact
            // discovery only. Persisting either lane before the authenticated receiver
            // completes its own JoinSession can emit PlayerJoin too early and leave the
            // retail client waiting forever in RDV StateJoin.
            Some((invite.kind, invite.session_type, invite.session_id))
        } else {
            None
        };

        if let Some(invite) = invite.as_ref() {
            let target: Option<(u32, String)> = sqlx::query_as(
                "SELECT creator_id, attributes FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL",
            )
            .bind(invite.session_id)
            .bind(invite.session_type)
            .fetch_optional(&mut *transaction)
            .await?;
            if let Err(error) = check(invite, target) {
                transaction.rollback().await?;
                return Err(error);
            }
        }
        let accepted_rows = sqlx::query(
            r"
            UPDATE invites
            SET accepted_at = COALESCE(accepted_at, CURRENT_TIMESTAMP)
            WHERE id = ?
              AND receiver = ?
              AND consumed_at IS NULL
              AND expires_at > CURRENT_TIMESTAMP
            ",
        )
        .bind(invite_id)
        .bind(user_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if accepted_rows != 1 {
            // The invitation may expire between reading it and executing the
            // guarded update. Never retire competitors or report success then.
            transaction.rollback().await?;
            return Ok(None);
        }

        // Acceptance is the start of one native session transition. Retire all
        // other invitations for this receiver in the same transaction, not
        // only equivalent same-room rows: a rapid second F5 targeting another
        // room must not enqueue a competing native join event. Keep the chosen
        // row until JoinSession consumes it so exact-session routing and retry
        // remain available.
        let superseded_competing_invites = if authorized_target.is_some() {
            sqlx::query(
                r"
                UPDATE invites
                SET consumed_at = CURRENT_TIMESTAMP
                WHERE receiver = ?
                  AND id != ?
                  AND consumed_at IS NULL
                  AND expires_at > CURRENT_TIMESTAMP
                ",
            )
            .bind(user_id)
            .bind(invite_id)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
        } else {
            0
        };

        transaction.commit().await?;
        if let Some((kind, session_type, session_id)) = authorized_target {
            info!(
                self.logger,
                "InviteParticipantAuthorized receiver_id={user_id} kind={kind} session_type={session_type} session_id={session_id} participant_persisted=false"
            );
            if superseded_competing_invites > 0 {
                info!(
                    self.logger,
                    "CompetingInvitesSuperseded receiver_id={user_id} session_type={session_type} session_id={session_id} accepted_invite_id={invite_id} count={superseded_competing_invites}"
                );
            }
        }
        Ok(invite)
    }

    /// Resolves the exact session associated with an accepted invitation. This
    /// deliberately ignores normal matchmaking attributes and query IDs: the
    /// authenticated receiver explicitly accepted this exact target.
    pub fn find_accepted_invited_session(&self, receiver_id: u32, session_type: u32) -> Result<Option<InvitedGameSession>> {
        run(self.find_accepted_invited_session_async(receiver_id, session_type))?
    }

    pub fn find_accepted_manual_invited_session(&self, receiver_id: u32, session_type: u32) -> Result<Option<InvitedGameSession>> {
        run(self.find_accepted_invited_session_filtered_async(receiver_id, session_type, true))?
    }

    pub async fn find_accepted_invited_session_async(&self, receiver_id: u32, session_type: u32) -> Result<Option<InvitedGameSession>> {
        self.find_accepted_invited_session_filtered_async(receiver_id, session_type, false).await
    }

    async fn find_accepted_invited_session_filtered_async(&self, receiver_id: u32, session_type: u32, manual_only: bool) -> Result<Option<InvitedGameSession>> {
        let mut transaction = self.pool.begin().await?;
        let row: Option<(i64, i32, u32, u32, u32, String)> = sqlx::query_as(
            r"
            SELECT
                i.id AS invite_id,
                i.kind,
                g.type_id AS session_type,
                g.id AS session_id,
                g.creator_id,
                g.attributes
            FROM invites AS i
            INNER JOIN game_sessions AS g
                ON g.id = i.session_id AND g.type_id = i.session_type
            INNER JOIN participants AS p
                ON p.game_id = g.id AND p.user_id = g.creator_id
            WHERE i.receiver = ?
              AND i.session_type = ?
              AND (? = 0 OR i.kind IN (1, 2, 4))
              AND i.accepted_at IS NOT NULL
              AND i.consumed_at IS NULL
              AND i.expires_at > CURRENT_TIMESTAMP
              AND i.kind IN (1, 2, 3, 4, 5)
              AND (
                  EXISTS (
                      SELECT 1 FROM participants AS sender_participant
                      WHERE sender_participant.game_id = g.id
                        AND sender_participant.user_id = i.sender
                  )
                  OR EXISTS (
                      SELECT 1
                      FROM party_members AS sender_party_member
                      INNER JOIN party_members AS host_party_member
                          ON host_party_member.party_id = sender_party_member.party_id
                      WHERE sender_party_member.user_id = i.sender
                        AND host_party_member.user_id = g.creator_id
                  )
              )
              AND g.destroyed_at IS NULL
              AND EXISTS (SELECT 1 FROM station_urls AS s WHERE s.user_id = g.creator_id)
            ORDER BY i.created DESC, i.id DESC
            LIMIT 1
            ",
        )
        .bind(receiver_id)
        .bind(session_type)
        .bind(if manual_only { 1_i64 } else { 0_i64 })
        .fetch_optional(&mut *transaction)
        .await?;

        #[cfg(test)]
        self.discovery_checkpoint();

        let Some((invite_id, kind, session_type, session_id, creator_id, attributes)) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let mut session = GameSession {
            session_type,
            session_id,
            creator_id,
            attributes,
            participants: sqlx::query_as(
                r"
                SELECT user_id, username AS name
                FROM participants AS p, users AS u
                WHERE u.id = user_id AND game_id = ?
                ORDER BY user_id
                ",
            )
            .bind(session_id)
            .fetch_all(&mut *transaction)
            .await?,
        };
        if kind == INVITE_KIND_PRIVATE_ROOM {
            let stored_attributes = session.attributes.clone();
            session.attributes = project_private_invite_session_class_only(&stored_attributes);
            info!(
                self.logger,
                "PrivateInviteClassOnlyCleanup receiver_id={receiver_id} session_type={session_type} session_id={session_id} public_slot_projection=false uplay_target_projection=false response_only=true"
            );
            info!(
                self.logger,
                "PrivateInviteClassProbe receiver_id={receiver_id} session_type={session_type} session_id={session_id} stored_113=0 projected_113=1 stored_attributes={stored_attributes} projected_attributes={} public_slot_unchanged=true uplay_id_unchanged=true response_only=true",
                session.attributes
            );
        }

        for participant in &mut session.participants {
            participant.station_urls = sqlx::query_as("SELECT url FROM station_urls WHERE user_id = ? ORDER BY url")
                .bind(participant.user_id)
                .fetch_all(&mut *transaction)
                .await?
                .into_iter()
                .map(|row: (String,)| row.0)
                .collect();
        }
        transaction.commit().await?;
        Ok(Some(InvitedGameSession { invite_id, kind, session }))
    }
}

#[cfg(test)]
impl Storage {
    pub(crate) fn lan_readiness_fixture(attributes: &str, members:u32) -> (std::sync::Arc<Self>, u32, u32) {
        let unique=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path=std::env::temp_dir().join(format!("scbl-lan-readiness-{}-{unique}.db",std::process::id()));
        let storage=Self::init_with_database_url(slog::Logger::root(slog::Discard,slog::o!()),&format!("sqlite://{}?mode=rwc",path.display())).unwrap();
        run(async {
            for id in 1..=3 {
                sqlx::query("INSERT INTO users (id, username, password_hash, ubi_id) VALUES (?, ?, ?, ?)")
                    .bind(id).bind(format!("synthetic-user-{id}")).bind("synthetic-not-a-password").bind(format!("synthetic-ubi-{id}"))
                    .execute(&storage.pool).await.unwrap();
            }
        }).unwrap();
        // Native follow tests use an explicitly authenticated Official run for
        // each participant, before the first immutable room identity is stamped.
        run(async {
            let _generation=storage.mode_gate.write().await;
            for id in 1..=3 {
                storage.begin_game_run(id,&format!("{id:032x}"),1,"127.0.0.1".parse().unwrap()).await.unwrap();
            }
        }).unwrap();
        for id in 1..=3 { storage.register_urls(id,vec![format!("prudp:/address=10.77.0.{id};port=13000;sid=14;type=2")]).unwrap(); }
        let lobby=storage.create_game_session(1,1,"113 => 1;3 => 8;4 => 0".into()).unwrap();
        for member in 2..=members {
            let invite=run(storage.add_invite_async(1,member,INVITE_KIND_LOBBY_PARTY,1,lobby)).unwrap().unwrap().unwrap();
            assert!(run(storage.accept_invite_async(member,invite)).unwrap().unwrap().is_some());
            assert!(storage.join_game_session_unbounded(member,1,lobby).unwrap());
        }
        let room=storage.create_game_session(1,1,attributes.into()).unwrap();
        run(sqlx::query("UPDATE invites SET created=datetime('now','-3 seconds') WHERE kind=3").execute(&storage.pool)).unwrap().unwrap();
        (std::sync::Arc::new(storage),lobby,room)
    }
    pub(crate) async fn lan_readiness_hold_write(&self) -> sqlx::Transaction<'static, sqlx::Sqlite> {
        self.begin_write().await.unwrap()
    }
    pub(crate) fn lan_readiness_state(&self,id:i64)->Option<(i32,bool,bool)> {
        run(sqlx::query_as("SELECT kind, accepted_at IS NOT NULL, consumed_at IS NOT NULL FROM invites WHERE id=?")
            .bind(id).fetch_optional(&self.pool)).unwrap().unwrap()
    }
    pub(crate) fn lan_readiness_id(&self)->i64 {
        run(sqlx::query_scalar("SELECT id FROM invites WHERE receiver=2 AND kind=3 AND consumed_at IS NULL ORDER BY id DESC LIMIT 1")
            .fetch_one(&self.pool)).unwrap().unwrap()
    }
    pub(crate) fn lan_readiness_redelivery(&self) {
        run(sqlx::query("UPDATE invites SET delivered_at=NULL").execute(&self.pool)).unwrap().unwrap();
    }
    pub(crate) fn lan_readiness_kind(&self,id:i64,kind:i32) {
        run(sqlx::query("UPDATE invites SET kind=? WHERE id=?").bind(kind).bind(id).execute(&self.pool)).unwrap().unwrap();
    }
    pub(crate) fn lan_readiness_competitor(&self,id:i64)->i64 {
        run(sqlx::query("INSERT INTO invites(sender,receiver,kind,session_type,session_id,party_id,expires_at) SELECT sender,receiver,1,session_type,session_id,party_id,expires_at FROM invites WHERE id=?")
            .bind(id).execute(&self.pool)).unwrap().unwrap().last_insert_rowid()
    }
    pub(crate) fn lan_readiness_invalidate(&self,room:u32,expire:bool) {
        if expire {
            run(sqlx::query("UPDATE invites SET expires_at=datetime('now','-1 second')").execute(&self.pool)).unwrap().unwrap();
        } else {
            run(sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?").bind(room).execute(&self.pool)).unwrap().unwrap();
        }
    }
    pub(crate) fn lan_readiness_expire_soon(&self,id:i64) {
        run(sqlx::query("UPDATE invites SET expires_at=datetime('now','+1 second') WHERE id=?").bind(id).execute(&self.pool)).unwrap().unwrap();
    }
    pub(crate) fn lan_readiness_published(&self,id:u32)->Option<u32> { self.published_game_session(id) }

}

