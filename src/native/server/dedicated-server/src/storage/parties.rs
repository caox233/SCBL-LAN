use super::Storage;
use super::INVITE_KIND_LOBBY_RESTORE;
use super::INVITE_KIND_PARTY_FOLLOW;

impl Storage {
    /// Returns the caller's authoritative social-party roster. The leader is
    /// ordered first and the wire-facing roster is bounded by the game's SCBL
    /// party capacity.
    pub async fn find_party_roster_async(&self, user_id: u32) -> super::Result<Option<(u32, Vec<super::User>)>> {
        let Some((party_id, leader_id)) = sqlx::query_as::<_, (i64, u32)>(
            r"
            SELECT membership.party_id, party.leader_id
            FROM party_members AS membership
            INNER JOIN parties AS party ON party.id = membership.party_id
            WHERE membership.user_id = ?
            ",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };

        let members = sqlx::query_as::<_, super::User>(
            r"
            SELECT
                user.id,
                user.username,
                COALESCE(user.ubi_id, CAST(user.id AS TEXT)) AS ubi_id,
                user.is_online
            FROM party_members AS membership
            INNER JOIN users AS user ON user.id = membership.user_id
            WHERE membership.party_id = ?
            ORDER BY CASE WHEN user.id = ? THEN 0 ELSE 1 END, membership.joined_at, user.id
            LIMIT 12
            ",
        )
        .bind(party_id)
        .bind(leader_id)
        .fetch_all(&self.pool)
        .await?;
        Ok((!members.is_empty()).then_some((leader_id, members)))
    }

    /// Promotes the game-selected successor in the Party that currently
    /// contains that successor. A private-room leave may already have chosen a
    /// provisional Party leader before the native `SplitSession` result arrives;
    /// the authoritative game result must be able to correct that provisional
    /// choice without requiring the old host to remain a Party member.
    pub(super) async fn transfer_party_leader_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, old_leader_id: u32, new_leader_id: u32) -> sqlx::Result<bool> {
        let party_id = sqlx::query_scalar::<_, i64>(
            r"
            SELECT party.id
            FROM parties AS party
            INNER JOIN party_members AS new_member
                ON new_member.party_id = party.id AND new_member.user_id = ?
            ",
        )
        .bind(new_leader_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(party_id) = party_id else {
            return Ok(false);
        };

        // A user normally has only one Party. Remove an orphaned leader row
        // defensively before assigning the UNIQUE leader_id to the N-1 Party.
        sqlx::query("DELETE FROM parties WHERE leader_id = ? AND id != ?")
            .bind(new_leader_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        let changed = sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(new_leader_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?
            .rows_affected()
            == 1;
        if changed {
            sqlx::query("DELETE FROM party_members WHERE party_id = ? AND user_id = ?")
                .bind(party_id)
                .bind(old_leader_id)
                .execute(&mut **transaction)
                .await?;
        }
        Ok(changed)
    }

    /// Detaches a departing private-game host from the social Party before the
    /// retail clients can follow that host into a newly-created personal lobby.
    /// This is deliberately Party-only: the game Session keeps its original
    /// owner and complete roster until the native `SplitSession` election names
    /// the authoritative replacement host.
    pub(super) async fn prepare_native_private_host_migration_party_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        old_host_id: u32,
        remaining_participants: &[u32],
    ) -> sqlx::Result<Option<(u32, u64)>> {
        let Some(&fallback_leader_id) = remaining_participants.first() else {
            return Ok(None);
        };

        let old_party_id = sqlx::query_scalar::<_, i64>("SELECT id FROM parties WHERE leader_id = ?")
            .bind(old_host_id)
            .fetch_optional(&mut **transaction)
            .await?;

        // Preserve the Party's stable join order when it already contains a
        // survivor. Direct private invitations can bypass the pre-room lobby,
        // so fall back to the deterministic participant order when necessary.
        let mut new_leader_id = fallback_leader_id;
        if let Some(party_id) = old_party_id {
            let ordered_members = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM party_members WHERE party_id = ? ORDER BY joined_at, user_id")
                .bind(party_id)
                .fetch_all(&mut **transaction)
                .await?;
            if let Some((party_survivor_id,)) = ordered_members.into_iter().find(|(member_id,)| remaining_participants.contains(member_id)) {
                new_leader_id = party_survivor_id;
            }
        }

        if let Some(previous_new_leader_party) = sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = ?")
            .bind(new_leader_id)
            .fetch_optional(&mut **transaction)
            .await?
        {
            if Some(previous_new_leader_party) != old_party_id {
                sqlx::query("DELETE FROM parties WHERE id = ?")
                    .bind(previous_new_leader_party)
                    .execute(&mut **transaction)
                    .await?;
            }
        }
        sqlx::query("DELETE FROM parties WHERE leader_id = ? AND (? IS NULL OR id != ?)")
            .bind(new_leader_id)
            .bind(old_party_id)
            .bind(old_party_id)
            .execute(&mut **transaction)
            .await?;

        let party_id = if let Some(party_id) = old_party_id {
            sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
                .bind(new_leader_id)
                .bind(party_id)
                .execute(&mut **transaction)
                .await?;
            party_id
        } else {
            Self::ensure_party_for_leader_async(transaction, new_leader_id).await?
        };

        // The private Session roster is authoritative for this transition.
        // Rebuilding this one Party also removes the old host and prevents
        // late/direct invitees from being omitted. No restore/follow invite is
        // emitted: native game migration remains the only active transition.
        sqlx::query("DELETE FROM party_members WHERE party_id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        for participant_id in remaining_participants {
            sqlx::query(
                "INSERT INTO party_members (user_id, party_id) VALUES (?, ?) \
                 ON CONFLICT(user_id) DO UPDATE SET party_id = excluded.party_id, joined_at = CURRENT_TIMESTAMP",
            )
            .bind(participant_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        }
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;

        #[allow(clippy::cast_possible_truncation)]
        Ok(Some((new_leader_id, remaining_participants.len() as u64)))
    }

    /// Detaches a player who explicitly left an active room while preserving
    /// the remaining social Party. The departing player immediately receives
    /// a one-member Party so the client never projects an impossible empty
    /// lobby while it restores or creates the corresponding personal Session.
    pub(super) async fn detach_party_member_to_solo_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, user_id: u32, active_session_id: u32) -> sqlx::Result<()> {
        let membership = sqlx::query_as::<_, (i64, u32)>(
            r"
            SELECT membership.party_id, party.leader_id
            FROM party_members AS membership
            INNER JOIN parties AS party ON party.id = membership.party_id
            WHERE membership.user_id = ?
            ",
        )
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await?;

        if let Some((party_id, leader_id)) = membership {
            if leader_id == user_id {
                let replacement_id = sqlx::query_scalar::<_, u32>(
                    r"
                    SELECT member.user_id
                    FROM party_members AS member
                    INNER JOIN participants AS participant
                        ON participant.user_id = member.user_id
                       AND participant.game_id = ?
                    WHERE member.party_id = ?
                      AND member.user_id != ?
                    ORDER BY member.joined_at, member.user_id
                    LIMIT 1
                    ",
                )
                .bind(active_session_id)
                .bind(party_id)
                .bind(user_id)
                .fetch_optional(&mut **transaction)
                .await?;

                // Prefer a survivor in the room that is currently unwinding,
                // but never destroy unrelated members of the same social
                // Party merely because they are not participants in this one
                // game Session.
                let replacement_id = if replacement_id.is_some() {
                    replacement_id
                } else {
                    sqlx::query_scalar::<_, u32>("SELECT user_id FROM party_members WHERE party_id = ? AND user_id != ? ORDER BY joined_at, user_id LIMIT 1")
                        .bind(party_id)
                        .bind(user_id)
                        .fetch_optional(&mut **transaction)
                        .await?
                };

                if let Some(replacement_id) = replacement_id {
                    sqlx::query("DELETE FROM parties WHERE leader_id = ? AND id != ?")
                        .bind(replacement_id)
                        .bind(party_id)
                        .execute(&mut **transaction)
                        .await?;
                    sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
                        .bind(replacement_id)
                        .bind(party_id)
                        .execute(&mut **transaction)
                        .await?;
                } else {
                    sqlx::query("DELETE FROM parties WHERE id = ?").bind(party_id).execute(&mut **transaction).await?;
                }
            }

            sqlx::query("DELETE FROM party_members WHERE user_id = ?").bind(user_id).execute(&mut **transaction).await?;
        }

        Self::ensure_party_for_leader_async(transaction, user_id).await?;
        Ok(())
    }

    /// Queues the same native lobby-restore transition used by host migration
    /// after a non-host explicitly leaves an otherwise live room. Party state
    /// alone is not enough for the retail UI: without this event it can remain
    /// on a zero-member projection until the player manually leaves Party.
    pub(super) async fn queue_departed_member_lobby_restore_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_id: u32,
        session_type: u32,
        departed_session_id: u32,
    ) -> sqlx::Result<Option<(u32, bool, u64)>> {
        // An accepted normal invite is already driving one native
        // AcceptInvite/JoinSession transition. Creating a kind-4 personal
        // lobby restore before that transition finishes makes retail execute
        // a second AcceptInvite and can tear down the newly joined Session.
        // Keep the accepted row authoritative until JoinSession consumes it
        // or its target becomes invalid/expired.
        if Self::accepted_normal_invite_transition_async(transaction, user_id, session_type, departed_session_id, None)
            .await?
            .is_some()
        {
            return Ok(None);
        }

        let party = sqlx::query_as::<_, (i64,)>(
            r"
            SELECT party.id
            FROM parties AS party
            INNER JOIN party_members AS membership ON membership.party_id = party.id
            WHERE party.leader_id = ?
              AND membership.user_id = ?
              AND (SELECT COUNT(*) FROM party_members AS member WHERE member.party_id = party.id) = 1
            ",
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some((party_id,)) = party else {
            return Ok(None);
        };

        let existing_lobby = sqlx::query_as::<_, (u32,)>(
            r"
            SELECT lobby.id
            FROM game_sessions AS lobby
            INNER JOIN participants AS owner
                ON owner.game_id = lobby.id AND owner.user_id = ?
            WHERE lobby.type_id = ?
              AND lobby.creator_id = ?
              AND lobby.id != ?
              AND lobby.destroyed_at IS NULL
              AND NOT EXISTS (
                  SELECT 1
                  FROM participants AS other
                  WHERE other.game_id = lobby.id
                    AND other.user_id != ?
              )
              AND (';' || replace(lobby.attributes, ' ', '') || ';') LIKE '%;113=>1;%'
            ORDER BY lobby.id DESC
            LIMIT 1
            ",
        )
        .bind(user_id)
        .bind(session_type)
        .bind(user_id)
        .bind(departed_session_id)
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await?;

        let (lobby_session_id, seeded) = if let Some((lobby_session_id,)) = existing_lobby {
            (lobby_session_id, false)
        } else {
            let attributes = sqlx::query_as::<_, (String,)>(
                r"
                SELECT attributes
                FROM game_sessions
                WHERE type_id = ?
                  AND creator_id = ?
                  AND (';' || replace(attributes, ' ', '') || ';') LIKE '%;113=>1;%'
                ORDER BY id DESC
                LIMIT 1
                ",
            )
            .bind(session_type)
            .bind(user_id)
            .fetch_optional(&mut **transaction)
            .await?
            .map_or_else(|| "113 => 1;3 => 8;4 => 0".to_string(), |(attributes,)| attributes);
            let lobby_session_id = sqlx::query("INSERT INTO game_sessions (type_id, creator_id, attributes, release_identity) VALUES (?, ?, ?, (SELECT release_identity FROM game_runs WHERE user_id=? AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds')))")
                .bind(session_type)
                .bind(user_id)
                .bind(attributes)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?
                .last_insert_rowid();
            sqlx::query("INSERT INTO participants (game_id, user_id) VALUES (?, ?)")
                .bind(lobby_session_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            (lobby_session_id as u32, true)
        };

        sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE receiver = ? AND kind = ? AND consumed_at IS NULL")
            .bind(user_id)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .execute(&mut **transaction)
            .await?;
        let created = sqlx::query(
            r"
            INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at)
            VALUES (?, ?, ?, ?, ?, ?, datetime('now', '+5 minutes'))
            ",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .bind(session_type)
        .bind(lobby_session_id)
        .bind(party_id)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        Ok(Some((lobby_session_id, seeded, created)))
    }

    pub(super) async fn accepted_normal_invite_transition_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_id: u32,
        session_type: u32,
        departed_session_id: u32,
        expected: Option<super::PendingAcceptedNormalInvite>,
    ) -> sqlx::Result<Option<super::AcceptedNormalInviteTransition>> {
        // Completed admission survives deletion/expiry of its transient invite.
        // Only a validated witness and current exact-target membership qualify.
        if let Some(expected) = expected {
            let joined: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM game_sessions AS target
                 INNER JOIN participants AS participant ON participant.game_id = target.id
                 WHERE target.id = ? AND target.type_id = ? AND target.id != ?
                 AND target.destroyed_at IS NULL AND participant.user_id = ?)",
            )
            .bind(expected.target_session_id)
            .bind(session_type)
            .bind(departed_session_id)
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?;
            if joined != 0 {
                return Ok(Some(super::AcceptedNormalInviteTransition::Joined));
            }
        }
        let state = sqlx::query_as::<_, (i64, u32, i64, i64)>(
            r"
            SELECT
                invite.id,
                invite.session_id,
                invite.consumed_at IS NULL AS pending,
                EXISTS (
                    SELECT 1
                    FROM participants AS participant
                    WHERE participant.game_id = target.id
                      AND participant.user_id = invite.receiver
                ) AS joined
            FROM invites AS invite
            INNER JOIN game_sessions AS target
                ON target.id = invite.session_id
               AND target.type_id = invite.session_type
            WHERE invite.receiver = ?
              AND invite.kind IN (?, ?, ?)
              AND invite.session_type = ?
              AND invite.session_id != ?
              AND invite.accepted_at IS NOT NULL
              AND invite.expires_at > CURRENT_TIMESTAMP
              AND target.destroyed_at IS NULL
              AND (? IS NULL OR invite.id = ?)
              AND (? IS NULL OR invite.session_id = ?)
              AND (
                  (? IS NULL AND invite.consumed_at IS NULL)
                  OR (
                      ? IS NOT NULL
                      AND (
                          invite.consumed_at IS NULL
                          OR EXISTS (
                              SELECT 1
                              FROM participants AS participant
                              WHERE participant.game_id = target.id
                                AND participant.user_id = invite.receiver
                          )
                      )
                  )
              )
            ORDER BY invite.accepted_at DESC, invite.id DESC
            LIMIT 1
            ",
        )
        .bind(user_id)
        .bind(super::INVITE_KIND_PRIVATE_ROOM)
        .bind(super::INVITE_KIND_LOBBY_PARTY)
        .bind(super::INVITE_KIND_PARTY_FOLLOW)
        .bind(session_type)
        .bind(departed_session_id)
        .bind(expected.map(|transition| transition.invite_id))
        .bind(expected.map(|transition| transition.invite_id))
        .bind(expected.map(|transition| transition.target_session_id))
        .bind(expected.map(|transition| transition.target_session_id))
        .bind(expected.map(|transition| transition.invite_id))
        .bind(expected.map(|transition| transition.invite_id))
        .fetch_optional(&mut **transaction)
        .await?;
        Ok(state.and_then(|(invite_id, target_session_id, pending, joined)| {
            if joined != 0 {
                Some(super::AcceptedNormalInviteTransition::Joined)
            } else if pending != 0 {
                Some(super::AcceptedNormalInviteTransition::Pending(super::PendingAcceptedNormalInvite {
                    invite_id,
                    target_session_id,
                }))
            } else {
                None
            }
        }))
    }

    /// Applies the Party generation that authorized an accepted invitation
    /// only after the receiver is formally admitted to the target Session.
    /// A player admitted through a private-room invitation is a current Party
    /// member as well: this lets the native "leave with Party" action carry
    /// every room member back through one normal lobby transition.
    pub(super) async fn attach_accepted_invite_party_async(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_id: u32,
        session_type: u32,
        session_id: u32,
    ) -> sqlx::Result<Option<i32>> {
        let accepted_party = sqlx::query_as::<_, (i64, i64, u32, i32)>(
            r"
            SELECT invite.id, invite.party_id, party.leader_id, invite.kind
            FROM invites AS invite
            INNER JOIN parties AS party ON party.id = invite.party_id
            WHERE invite.receiver = ?
              AND invite.kind IN (?, ?, ?, ?)
              AND invite.session_type = ?
              AND invite.session_id = ?
              AND invite.accepted_at IS NOT NULL
              AND invite.consumed_at IS NULL
              AND invite.expires_at > CURRENT_TIMESTAMP
            ORDER BY invite.created DESC, invite.id DESC
            LIMIT 1
            ",
        )
        .bind(user_id)
        .bind(super::INVITE_KIND_PRIVATE_ROOM)
        .bind(super::INVITE_KIND_LOBBY_PARTY)
        .bind(super::INVITE_KIND_PARTY_FOLLOW)
        .bind(super::INVITE_KIND_LOBBY_RESTORE)
        .bind(session_type)
        .bind(session_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some((invite_id, party_id, leader_id, invite_kind)) = accepted_party else {
            return Ok(None);
        };

        // Capture the exact, SQL-validated normal invitation before admission
        // consumes it. A first expiry after Join must see the same witness as
        // expiry-before-Join, not mistake the consumed invite for a solo leave.
        // This is not a success flag: expiry checks committed target membership
        // first, then the still-pending invite under the DB writer. Rollback
        // keeps pending/expiry behavior; no map lock crosses await or renews TTL.
        if matches!(invite_kind, super::INVITE_KIND_PRIVATE_ROOM | super::INVITE_KIND_LOBBY_PARTY | super::INVITE_KIND_PARTY_FOLLOW) {
            let transition = super::PendingAcceptedNormalInvite { invite_id, target_session_id: session_id };
            if let Ok(mut departures) = self.pending_quick_match_member_leaves.lock() {
                for ((departure_type, departure_session, receiver), pending) in departures.iter_mut() {
                    if *departure_type == session_type && *departure_session != session_id && *receiver == user_id
                        && pending.accepted_invite != Some(transition)
                    {
                        // An expiry snapshot taken before this writer must not
                        // act with its old None/other-invite witness afterward.
                        pending.generation = self.next_cleanup_generation();
                        pending.accepted_invite = Some(transition);
                    }
                }
            }
        }

        let previous = sqlx::query_as::<_, (i64, u32)>(
            r"
            SELECT membership.party_id, party.leader_id
            FROM party_members AS membership
            INNER JOIN parties AS party ON party.id = membership.party_id
            WHERE membership.user_id = ?
            ",
        )
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await?;
        if let Some((previous_party_id, previous_leader_id)) = previous {
            if previous_party_id == party_id {
                return Ok(Some(invite_kind));
            }
            if previous_leader_id == user_id {
                let replacement_id = sqlx::query_scalar::<_, u32>("SELECT user_id FROM party_members WHERE party_id = ? AND user_id != ? ORDER BY joined_at, user_id LIMIT 1")
                    .bind(previous_party_id)
                    .bind(user_id)
                    .fetch_optional(&mut **transaction)
                    .await?;
                if let Some(replacement_id) = replacement_id {
                    sqlx::query("DELETE FROM parties WHERE leader_id = ? AND id != ?")
                        .bind(replacement_id)
                        .bind(previous_party_id)
                        .execute(&mut **transaction)
                        .await?;
                    sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
                        .bind(replacement_id)
                        .bind(previous_party_id)
                        .execute(&mut **transaction)
                        .await?;
                } else {
                    sqlx::query("DELETE FROM parties WHERE id = ?").bind(previous_party_id).execute(&mut **transaction).await?;
                }
            }
        }

        sqlx::query("DELETE FROM parties WHERE leader_id = ? AND id != ?")
            .bind(user_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        sqlx::query(
            "INSERT INTO party_members (user_id, party_id) VALUES (?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET party_id = excluded.party_id, joined_at = CURRENT_TIMESTAMP",
        )
        .bind(user_id)
        .bind(party_id)
        .execute(&mut **transaction)
        .await?;
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ? AND leader_id = ?")
            .bind(party_id)
            .bind(leader_id)
            .execute(&mut **transaction)
            .await?;
        Ok(Some(invite_kind))
    }

    /// A personal lobby is also the wire-visible one-player Party boundary.
    /// Preserve an existing Party during normal transitions, but repair a
    /// missing membership after the native client has left a shared lobby.
    pub(super) async fn ensure_personal_party_if_unassigned_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, user_id: u32) -> sqlx::Result<bool> {
        let assigned = sqlx::query_scalar::<_, i64>("SELECT EXISTS(SELECT 1 FROM party_members WHERE user_id = ?)")
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?
            != 0;
        if assigned {
            return Ok(false);
        }
        Self::ensure_party_for_leader_async(transaction, user_id).await?;
        Ok(true)
    }

    pub(super) async fn ensure_party_for_leader_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, leader_id: u32) -> sqlx::Result<i64> {
        let existing = sqlx::query_as::<_, (i64,)>("SELECT id FROM parties WHERE leader_id = ?")
            .bind(leader_id)
            .fetch_optional(&mut **transaction)
            .await?;
        let party_id = if let Some((party_id,)) = existing {
            party_id
        } else {
            sqlx::query("INSERT INTO parties (leader_id) VALUES (?)")
                .bind(leader_id)
                .execute(&mut **transaction)
                .await?
                .last_insert_rowid()
        };
        sqlx::query(
            "INSERT INTO party_members (user_id, party_id) VALUES (?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET party_id = excluded.party_id, joined_at = CURRENT_TIMESTAMP",
        )
        .bind(leader_id)
        .bind(party_id)
        .execute(&mut **transaction)
        .await?;
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        Ok(party_id)
    }

    /// Reconciles the persistent Party generation with the lobby that the
    /// inviter is currently publishing to Uplay.  Public matchmaking keeps
    /// several independent Parties in one game Session; the inviter's own
    /// published `113 => 1` lobby is therefore the only safe compatibility
    /// source when an earlier native leave/split sequence has made the
    /// persistent Party rows stale.
    pub(super) async fn synchronize_party_from_published_lobby_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        inviter_id: u32,
        published_lobby_session_id: u32,
    ) -> sqlx::Result<Option<i64>> {
        let lobby = sqlx::query_as::<_, (u32, String)>(
            r"
            SELECT lobby.creator_id, lobby.attributes
            FROM game_sessions AS lobby
            INNER JOIN participants AS leader
                ON leader.game_id = lobby.id AND leader.user_id = lobby.creator_id
            INNER JOIN participants AS inviter
                ON inviter.game_id = lobby.id AND inviter.user_id = ?
            WHERE lobby.id = ?
              AND lobby.destroyed_at IS NULL
            ",
        )
        .bind(inviter_id)
        .bind(published_lobby_session_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some((leader_id, attributes)) = lobby else {
            return Ok(None);
        };
        if super::session_attribute_value(&attributes, 113) != Some(1) {
            return Ok(None);
        }

        let lobby_members = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? ORDER BY user_id")
            .bind(published_lobby_session_id)
            .fetch_all(&mut **transaction)
            .await?;
        if lobby_members.is_empty() {
            return Ok(None);
        }

        let mut previous_party_ids = Vec::new();
        for (member_id,) in &lobby_members {
            if let Some(previous_party_id) = sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = ?")
                .bind(member_id)
                .fetch_optional(&mut **transaction)
                .await?
            {
                previous_party_ids.push(previous_party_id);
            }
        }
        previous_party_ids.sort_unstable();
        previous_party_ids.dedup();

        // Prefer an inviter's already-established multi-member Party. The
        // published lobby can migrate independently after a public-match host
        // leaves, so its current creator is not sufficient evidence to replace
        // an elected social Party leader. A one-member Party can be stale after
        // an older split path; only that compatibility case falls back to the
        // published lobby creator.
        let inviter_party = sqlx::query_as::<_, (i64, u32, i64)>(
            r"
            SELECT membership.party_id, party.leader_id,
                   (SELECT COUNT(*) FROM party_members AS member WHERE member.party_id = membership.party_id)
            FROM party_members AS membership
            INNER JOIN parties AS party ON party.id = membership.party_id
            WHERE membership.user_id = ?
            ",
        )
        .bind(inviter_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let party_id = if let Some((party_id, _, _)) = inviter_party.filter(|(_, _, member_count)| *member_count > 1) {
            party_id
        } else {
            Self::ensure_party_for_leader_async(transaction, leader_id).await?
        };
        for (member_id,) in &lobby_members {
            sqlx::query(
                "INSERT INTO party_members (user_id, party_id) VALUES (?, ?) \
                 ON CONFLICT(user_id) DO UPDATE SET party_id = excluded.party_id, joined_at = CURRENT_TIMESTAMP",
            )
            .bind(member_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        }

        // Moving a former leader can leave its previous Party without its
        // declared leader.  Keep any remaining cohort intact by promoting its
        // first stable member; delete only truly empty Party rows.
        for previous_party_id in previous_party_ids.into_iter().filter(|previous_party_id| *previous_party_id != party_id) {
            let remaining_members = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM party_members WHERE party_id = ? ORDER BY joined_at, user_id")
                .bind(previous_party_id)
                .fetch_all(&mut **transaction)
                .await?;
            let Some((replacement_leader_id,)) = remaining_members.first().copied() else {
                sqlx::query("DELETE FROM parties WHERE id = ?").bind(previous_party_id).execute(&mut **transaction).await?;
                continue;
            };
            let current_leader_id = sqlx::query_scalar::<_, u32>("SELECT leader_id FROM parties WHERE id = ?")
                .bind(previous_party_id)
                .fetch_optional(&mut **transaction)
                .await?;
            if current_leader_id.is_some_and(|current_leader_id| !remaining_members.iter().any(|(member_id,)| *member_id == current_leader_id)) {
                sqlx::query("DELETE FROM parties WHERE leader_id = ? AND id != ?")
                    .bind(replacement_leader_id)
                    .bind(previous_party_id)
                    .execute(&mut **transaction)
                    .await?;
                sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
                    .bind(replacement_leader_id)
                    .bind(previous_party_id)
                    .execute(&mut **transaction)
                    .await?;
            }
        }
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        Ok(Some(party_id))
    }

    /// Restores the social Party already published by the room host when the
    /// native client has chosen "leave with Party". Every player still in the
    /// private room is part of that action, including players invited after the
    /// original lobby was formed. Late room members receive an explicit lobby
    /// restore event after the old room has finished unwinding.
    pub(super) async fn restore_published_lobby_party_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        room_host_id: u32,
        room_session_id: u32,
        published_lobby_session_id: u32,
    ) -> sqlx::Result<Option<(u64, u64, u64)>> {
        let lobby = sqlx::query_as::<_, (String,)>(
            r"
            SELECT lobby.attributes
            FROM game_sessions AS lobby
            INNER JOIN participants AS host
                ON host.game_id = lobby.id AND host.user_id = ?
            WHERE lobby.id = ?
              AND lobby.creator_id = ?
              AND lobby.id != ?
              AND lobby.destroyed_at IS NULL
            ",
        )
        .bind(room_host_id)
        .bind(published_lobby_session_id)
        .bind(room_host_id)
        .bind(room_session_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some((attributes,)) = lobby else {
            return Ok(None);
        };
        if super::session_attribute_value(&attributes, 113) != Some(1) {
            return Ok(None);
        }

        let lobby_members = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? ORDER BY user_id")
            .bind(published_lobby_session_id)
            .fetch_all(&mut **transaction)
            .await?;
        if lobby_members.len() < 2 {
            return Ok(None);
        }

        let room_members = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? ORDER BY user_id")
            .bind(room_session_id)
            .fetch_all(&mut **transaction)
            .await?;
        let mut returning_members = lobby_members.iter().map(|(user_id,)| *user_id).collect::<Vec<_>>();
        returning_members.extend(room_members.iter().map(|(user_id,)| *user_id));
        returning_members.sort_unstable();
        returning_members.dedup();

        // Retail can send a follower LeaveSession immediately before the room
        // host chooses "leave with Party". The follower leave path has already
        // queued a personal-lobby restore by the time the host transaction
        // arrives. Keeping both restores active makes the follower split the
        // authoritative Party lobby, destroy that generation, and later report
        // "session does not exist". Once the host's published multi-member
        // lobby has been validated above, that group return is authoritative:
        // consume every other pending restore for the same returning cohort
        // and retire only its one-member fallback target.
        let mut cancelled_personal_restores = 0_u64;
        for user_id in &returning_members {
            let conflicting_restores = sqlx::query_as::<_, (i64, u32)>(
                r"
                SELECT restore.id, restore.session_id
                FROM invites AS restore
                INNER JOIN game_sessions AS fallback
                    ON fallback.id = restore.session_id
                   AND fallback.type_id = (SELECT type_id FROM game_sessions WHERE id = ?)
                   AND fallback.creator_id = restore.receiver
                   AND fallback.destroyed_at IS NULL
                WHERE restore.receiver = ?
                  AND restore.kind = ?
                  AND restore.session_id != ?
                  AND restore.consumed_at IS NULL
                  AND restore.expires_at > CURRENT_TIMESTAMP
                  AND (';' || replace(fallback.attributes, ' ', '') || ';') LIKE '%;113=>1;%'
                  AND NOT EXISTS (
                      SELECT 1
                      FROM participants AS other
                      WHERE other.game_id = fallback.id
                        AND other.user_id != restore.receiver
                  )
                ",
            )
            .bind(published_lobby_session_id)
            .bind(user_id)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(published_lobby_session_id)
            .fetch_all(&mut **transaction)
            .await?;
            for (restore_id, fallback_session_id) in conflicting_restores {
                cancelled_personal_restores += sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE id = ? AND consumed_at IS NULL")
                    .bind(restore_id)
                    .execute(&mut **transaction)
                    .await?
                    .rows_affected();
                sqlx::query("UPDATE game_sessions SET destroyed_at = CURRENT_TIMESTAMP WHERE id = ? AND creator_id = ? AND destroyed_at IS NULL")
                    .bind(fallback_session_id)
                    .bind(user_id)
                    .execute(&mut **transaction)
                    .await?;
            }
        }

        let party_id = Self::ensure_party_for_leader_async(transaction, room_host_id).await?;
        sqlx::query("DELETE FROM party_members WHERE party_id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        for user_id in &returning_members {
            sqlx::query(
                "INSERT INTO party_members (user_id, party_id) VALUES (?, ?) \
                 ON CONFLICT(user_id) DO UPDATE SET party_id = excluded.party_id, joined_at = CURRENT_TIMESTAMP",
            )
            .bind(user_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        }

        let mut restore_invites = 0_u64;
        for (user_id,) in room_members {
            if lobby_members.iter().any(|(lobby_user_id,)| *lobby_user_id == user_id) {
                continue;
            }
            restore_invites += sqlx::query(
                r"
                INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at)
                VALUES (?, ?, ?, (SELECT type_id FROM game_sessions WHERE id = ?), ?, ?, datetime('now', '+5 minutes'))
                ",
            )
            .bind(room_host_id)
            .bind(user_id)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(published_lobby_session_id)
            .bind(published_lobby_session_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?
            .rows_affected();
        }
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(Some((returning_members.len() as u64, restore_invites, cancelled_personal_restores)))
    }

    /// Returns one social Party from a public matchmaking Session without
    /// treating every player in that Session as a member of the same Party.
    /// The snapshot was captured before native host migration began, so it is
    /// authoritative even when the game removes the Party members in a later,
    /// different RMC call order.
    pub(super) async fn restore_quick_match_party_snapshot_async(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        snapshot: &super::QuickMatchPartyDeparture,
        session_type: u32,
        match_session_id: u32,
    ) -> sqlx::Result<Option<(u32, u64, u64)>> {
        let lobby = sqlx::query_as::<_, (String,)>("SELECT attributes FROM game_sessions WHERE id = ? AND type_id = ? AND id != ? AND destroyed_at IS NULL")
            .bind(snapshot.lobby_session_id)
            .bind(session_type)
            .bind(match_session_id)
            .fetch_optional(&mut **transaction)
            .await?;
        let Some((attributes,)) = lobby else {
            return Ok(None);
        };
        if super::session_attribute_value(&attributes, 113) != Some(1) || snapshot.members.len() < 2 {
            return Ok(None);
        }

        // The published lobby predates matchmaking and is not a safe return
        // target after the native SplitSession/AbandonSession burst. Its
        // creator can already differ from the current Party leader, making a
        // joining client interpret that stale generation as host migration.
        // Seed one fresh leader-owned restore target and preserve the complete
        // server-side Party snapshot, but expose the restore transition only
        // to the leader. Retail treats every member that receives the restore
        // before the leader has published as a host and calls CreateSession
        // independently. The leader must restore first; its CreateSession then
        // queues the existing phase-two follower restores.
        let restored_lobby_session_id = sqlx::query("INSERT INTO game_sessions (type_id, creator_id, attributes, release_identity) VALUES (?, ?, ?, (SELECT release_identity FROM game_runs WHERE user_id=? AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds')))")
            .bind(session_type)
            .bind(snapshot.leader_id)
            .bind(&attributes)
            .bind(snapshot.leader_id)
            .execute(&mut **transaction)
            .await?
            .last_insert_rowid();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let restored_lobby_session_id = restored_lobby_session_id as u32;

        for user_id in &snapshot.members {
            sqlx::query("DELETE FROM participants WHERE game_id = ? AND user_id = ?")
                .bind(match_session_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("DELETE FROM participants WHERE game_id = ? AND user_id = ?")
                .bind(snapshot.lobby_session_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
        }
        Self::reconcile_game_session_async(transaction, session_type, match_session_id).await?;
        Self::reconcile_game_session_async(transaction, session_type, snapshot.lobby_session_id).await?;

        // Remove the short-lived personal/old-Party projections created while
        // retail unwound the match, then restore exactly the captured Party.
        for user_id in &snapshot.members {
            sqlx::query("DELETE FROM party_members WHERE user_id = ?").bind(user_id).execute(&mut **transaction).await?;
        }
        for user_id in &snapshot.members {
            sqlx::query("DELETE FROM parties WHERE id != ? AND leader_id = ?")
                .bind(snapshot.party_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
        }
        let target_party_exists = sqlx::query_scalar::<_, i64>("SELECT EXISTS(SELECT 1 FROM parties WHERE id = ?)")
            .bind(snapshot.party_id)
            .fetch_one(&mut **transaction)
            .await?
            != 0;
        let party_id = if target_party_exists {
            sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
                .bind(snapshot.leader_id)
                .bind(snapshot.party_id)
                .execute(&mut **transaction)
                .await?;
            snapshot.party_id
        } else {
            sqlx::query("INSERT INTO parties (leader_id) VALUES (?)")
                .bind(snapshot.leader_id)
                .execute(&mut **transaction)
                .await?
                .last_insert_rowid()
        };

        for user_id in &snapshot.members {
            sqlx::query("INSERT INTO party_members (user_id, party_id) VALUES (?, ?)")
                .bind(user_id)
                .bind(party_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                .bind(restored_lobby_session_id)
                .bind(user_id)
                .execute(&mut **transaction)
                .await?;
            sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE receiver = ? AND kind = ? AND consumed_at IS NULL")
                .bind(user_id)
                .bind(super::INVITE_KIND_LOBBY_RESTORE)
                .execute(&mut **transaction)
                .await?;
        }

        let restore_invites = sqlx::query(
            r"
            INSERT INTO invites (
                sender, receiver, kind, session_type, session_id, party_id,
                delivered_at, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, datetime('now', '+5 minutes'))
            ",
        )
        .bind(snapshot.leader_id)
        .bind(snapshot.leader_id)
        .bind(super::INVITE_KIND_LOBBY_RESTORE)
        .bind(session_type)
        .bind(restored_lobby_session_id)
        .bind(party_id)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;

        // A quick-match transport host can differ from the social Party
        // leader. The leader must restore first, but the former transport host
        // is still completing the old match's native teardown. If its
        // phase-two restore is delivered before it has created a newer
        // personal lobby, retail accepts the join, later creates and splits a
        // solo lobby, then joins the Party target. Gate only that exact former
        // host; ordinary followers and all private-room restores stay
        // immediate.
        if snapshot.room_host_id != snapshot.leader_id {
            self.note_quick_match_former_host_restore_gate(session_type, snapshot.room_host_id, restored_lobby_session_id, party_id);
        }

        #[allow(clippy::cast_possible_truncation)]
        Ok(Some((restored_lobby_session_id, snapshot.members.len() as u64, restore_invites)))
    }

    /// Keeps the exact N-1 private-room cohort together after the room owner
    /// leaves. The successor must restore its own lobby first; only after that
    /// lobby is active do the other members receive their regroup events. A
    /// Party roster alone is not enough to update the retail lobby UI.
    pub(super) async fn handoff_private_room_party_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        old_host_id: u32,
        session_type: u32,
        old_room_session_id: u32,
        remaining_participants: &[u32],
    ) -> sqlx::Result<Option<(u32, u32, u64, u64)>> {
        if remaining_participants.is_empty() {
            if let Some((party_id,)) = sqlx::query_as::<_, (i64,)>("SELECT id FROM parties WHERE leader_id = ?")
                .bind(old_host_id)
                .fetch_optional(&mut **transaction)
                .await?
            {
                sqlx::query("DELETE FROM parties WHERE id = ?").bind(party_id).execute(&mut **transaction).await?;
            }
            return Ok(None);
        }

        // The retail clients retain their pre-room lobbies while entering a
        // directly invited private room. Repeated native migration can leave
        // that N-1 lobby owned by a different member than the numerically first
        // participant. Select the authoritative owner of the lobby that already
        // contains the largest part of the surviving cohort. Choosing a user id
        // first and looking for that user's lobby afterwards can mutate the
        // Party without producing any restore event, leaving a zero-member UI.
        let candidate_lobbies = sqlx::query_as::<_, (u32, u32, String)>(
            r"
            SELECT lobby.id, lobby.creator_id, lobby.attributes
            FROM game_sessions AS lobby
            INNER JOIN participants AS leader
                ON leader.game_id = lobby.id AND leader.user_id = lobby.creator_id
            WHERE lobby.type_id = ?
              AND lobby.id != ?
              AND lobby.destroyed_at IS NULL
            ORDER BY lobby.id DESC
            ",
        )
        .bind(session_type)
        .bind(old_room_session_id)
        .fetch_all(&mut **transaction)
        .await?;
        let mut successor_lobby = None;
        for candidate_leader_id in remaining_participants {
            let mut candidate_best = None;
            for (lobby_session_id, lobby_creator_id, attributes) in &candidate_lobbies {
                if lobby_creator_id != candidate_leader_id || super::session_attribute_value(attributes, 113) != Some(1) {
                    continue;
                }
                let lobby_members = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ?")
                    .bind(lobby_session_id)
                    .fetch_all(&mut **transaction)
                    .await?;
                let cohort_overlap = lobby_members.iter().filter(|(user_id,)| remaining_participants.contains(user_id)).count();
                let exact_cohort = cohort_overlap == remaining_participants.len() && lobby_members.len() == remaining_participants.len();
                let departed_member_restore_pending = sqlx::query_scalar::<_, i64>(
                    r"
                    SELECT EXISTS (
                        SELECT 1
                        FROM invites
                        WHERE sender = ?
                          AND receiver = ?
                          AND kind = ?
                          AND session_id = ?
                          AND consumed_at IS NULL
                          AND expires_at > CURRENT_TIMESTAMP
                    )
                    ",
                )
                .bind(candidate_leader_id)
                .bind(candidate_leader_id)
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .bind(lobby_session_id)
                .fetch_one(&mut **transaction)
                .await?
                    != 0;
                let score = (!departed_member_restore_pending, exact_cohort, cohort_overlap, *lobby_session_id);
                if candidate_best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
                    candidate_best = Some((score, (*lobby_session_id, exact_cohort)));
                }
            }
            let Some(((candidate_stable, candidate_exact, candidate_overlap, _), (lobby_session_id, exact_cohort))) = candidate_best else {
                continue;
            };
            // Preserve the existing deterministic member order when all
            // candidates own equivalent solo lobbies. Only a strictly larger
            // stability/overlap score may replace the earlier candidate. A
            // lobby just seeded for an individual departure is deliberately
            // less stable than a survivor's pre-existing lobby.
            let candidate_score = (candidate_stable, candidate_exact, candidate_overlap);
            if successor_lobby.as_ref().is_none_or(|(best_score, _, _, _)| candidate_score > *best_score) {
                successor_lobby = Some((candidate_score, *candidate_leader_id, lobby_session_id, exact_cohort));
            }
        }
        let Some((_, new_leader_id, lobby_session_id, exact_cohort)) = successor_lobby else {
            return Ok(None);
        };

        let old_party_id = sqlx::query_scalar::<_, i64>("SELECT id FROM parties WHERE leader_id = ?")
            .bind(old_host_id)
            .fetch_optional(&mut **transaction)
            .await?;
        if let Some(previous_new_leader_party) = sqlx::query_scalar::<_, i64>("SELECT party_id FROM party_members WHERE user_id = ?")
            .bind(new_leader_id)
            .fetch_optional(&mut **transaction)
            .await?
        {
            if Some(previous_new_leader_party) != old_party_id {
                sqlx::query("DELETE FROM parties WHERE id = ?")
                    .bind(previous_new_leader_party)
                    .execute(&mut **transaction)
                    .await?;
            }
        }
        sqlx::query("DELETE FROM parties WHERE leader_id = ? AND (? IS NULL OR id != ?)")
            .bind(new_leader_id)
            .bind(old_party_id)
            .bind(old_party_id)
            .execute(&mut **transaction)
            .await?;

        let party_id = if let Some(party_id) = old_party_id {
            sqlx::query("UPDATE parties SET leader_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
                .bind(new_leader_id)
                .bind(party_id)
                .execute(&mut **transaction)
                .await?;
            party_id
        } else {
            Self::ensure_party_for_leader_async(transaction, new_leader_id).await?
        };

        // The private-room participant list is the authoritative cohort. A
        // player who left before the host must not be pulled back into N-1.
        sqlx::query("DELETE FROM party_members WHERE party_id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        for user_id in remaining_participants {
            sqlx::query(
                "INSERT INTO party_members (user_id, party_id) VALUES (?, ?) \
                 ON CONFLICT(user_id) DO UPDATE SET party_id = excluded.party_id, joined_at = CURRENT_TIMESTAMP",
            )
            .bind(user_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
            sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE receiver = ? AND kind = ? AND consumed_at IS NULL")
                .bind(user_id)
                .bind(INVITE_KIND_LOBBY_RESTORE)
                .execute(&mut **transaction)
                .await?;
        }

        sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE party_id = ? AND consumed_at IS NULL")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;

        // Native Party migration can finish before the private-game timeout.
        // In that case the successor already owns an exact N-1 lobby and a
        // self-restore invite would make it leave that valid lobby, race a
        // second SplitSession, and finally create a solo Party. Only seed the
        // two-phase restore path when the selected lobby is not yet complete.
        let created = if exact_cohort {
            0
        } else {
            sqlx::query(
                r"
                INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at)
                VALUES (?, ?, ?, ?, ?, ?, datetime('now', '+5 minutes'))
                ",
            )
            .bind(new_leader_id)
            .bind(new_leader_id)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(session_type)
            .bind(lobby_session_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?
            .rows_affected()
        };
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(Some((new_leader_id, lobby_session_id, remaining_participants.len() as u64, created)))
    }

    /// Completes phase one of a private-room regroup. The successor has now
    /// restored or created the active lobby, so every other exact Party member
    /// can receive one phase-two restore event bound to that lobby.
    pub(super) async fn complete_private_room_party_leader_restore_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        leader_id: u32,
        session_type: u32,
        lobby_session_id: u32,
    ) -> sqlx::Result<u64> {
        let leader_restore = sqlx::query_as::<_, (i64, i64, u32)>(
            r"
            SELECT restore.id, restore.party_id, restore.session_id
            FROM invites AS restore
            INNER JOIN parties AS party
                ON party.id = restore.party_id AND party.leader_id = ?
            INNER JOIN game_sessions AS lobby
                ON lobby.id = ?
               AND lobby.type_id = ?
               AND lobby.creator_id = ?
               AND lobby.destroyed_at IS NULL
            INNER JOIN participants AS leader
                ON leader.game_id = lobby.id AND leader.user_id = ?
            WHERE restore.sender = ?
              AND restore.receiver = ?
              AND restore.kind = ?
              AND restore.accepted_at IS NOT NULL
              AND restore.consumed_at IS NULL
              AND restore.expires_at > CURRENT_TIMESTAMP
              AND (';' || replace(lobby.attributes, ' ', '') || ';') LIKE '%;113=>1;%'
            ORDER BY restore.created DESC, restore.id DESC
            LIMIT 1
            ",
        )
        .bind(leader_id)
        .bind(lobby_session_id)
        .bind(session_type)
        .bind(leader_id)
        .bind(leader_id)
        .bind(leader_id)
        .bind(leader_id)
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some((leader_restore_id, party_id, seed_lobby_session_id)) = leader_restore else {
            return Ok(0);
        };

        sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(leader_restore_id)
            .execute(&mut **transaction)
            .await?;
        sqlx::query("UPDATE invites SET consumed_at = CURRENT_TIMESTAMP WHERE party_id = ? AND kind = ? AND consumed_at IS NULL")
            .bind(party_id)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .execute(&mut **transaction)
            .await?;
        if seed_lobby_session_id != lobby_session_id {
            sqlx::query("UPDATE game_sessions SET destroyed_at = CURRENT_TIMESTAMP WHERE id = ? AND type_id = ? AND creator_id = ? AND destroyed_at IS NULL")
                .bind(seed_lobby_session_id)
                .bind(session_type)
                .bind(leader_id)
                .execute(&mut **transaction)
                .await?;
        }

        let followers = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM party_members WHERE party_id = ? AND user_id != ? ORDER BY joined_at, user_id")
            .bind(party_id)
            .bind(leader_id)
            .fetch_all(&mut **transaction)
            .await?;
        let mut created = 0;
        for (receiver_id,) in followers {
            created += sqlx::query(
                r"
                INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at)
                VALUES (?, ?, ?, ?, ?, ?, datetime('now', '+5 minutes'))
                ",
            )
            .bind(leader_id)
            .bind(receiver_id)
            .bind(INVITE_KIND_LOBBY_RESTORE)
            .bind(session_type)
            .bind(lobby_session_id)
            .bind(party_id)
            .execute(&mut **transaction)
            .await?
            .rows_affected();
        }
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        Ok(created)
    }

    pub(super) async fn cancel_private_room_lobby_restore_for_member_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, party_member_id: u32) -> sqlx::Result<u64> {
        Ok(sqlx::query(
            r"
            UPDATE invites
            SET consumed_at = CURRENT_TIMESTAMP
            WHERE kind = ?
              AND consumed_at IS NULL
              AND receiver = ?
              AND party_id = (SELECT party_id FROM party_members WHERE user_id = ?)
            ",
        )
        .bind(INVITE_KIND_LOBBY_RESTORE)
        .bind(party_member_id)
        .bind(party_member_id)
        .execute(&mut **transaction)
        .await?
        .rows_affected())
    }

    pub(super) async fn queue_party_follow_invites_async(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        leader_id: u32,
        session_type: u32,
        session_id: u32,
    ) -> sqlx::Result<u64> {
        let Some((party_id,)) = sqlx::query_as::<_, (i64,)>("SELECT id FROM parties WHERE leader_id = ?")
            .bind(leader_id)
            .fetch_optional(&mut **transaction)
            .await?
        else {
            return Ok(0);
        };
        let (leader_joined,): (i64,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM participants WHERE game_id = ? AND user_id = ?)")
            .bind(session_id)
            .bind(leader_id)
            .fetch_one(&mut **transaction)
            .await?;
        if leader_joined == 0 {
            return Ok(0);
        }

        let members = sqlx::query_as::<_, (u32,)>(
            r"
            SELECT member.user_id
            FROM party_members AS member
            WHERE member.party_id = ?
              AND member.user_id != ?
              AND NOT EXISTS (
                  SELECT 1 FROM participants AS participant
                  WHERE participant.game_id = ?
                    AND participant.user_id = member.user_id
              )
            ORDER BY member.user_id
            ",
        )
        .bind(party_id)
        .bind(leader_id)
        .bind(session_id)
        .fetch_all(&mut **transaction)
        .await?;
        let mut created = 0;
        for (receiver_id,) in members {
            sqlx::query(
                r"
                DELETE FROM invites
                WHERE receiver = ?
                  AND kind = ?
                  AND consumed_at IS NULL
                  AND NOT (
                      party_id = ?
                      AND session_type = ?
                      AND session_id = ?
                      AND expires_at > CURRENT_TIMESTAMP
                  )
                ",
            )
            .bind(receiver_id)
            .bind(INVITE_KIND_PARTY_FOLLOW)
            .bind(party_id)
            .bind(session_type)
            .bind(session_id)
            .execute(&mut **transaction)
            .await?;
            let result = sqlx::query(
                r"
                INSERT INTO invites (sender, receiver, kind, session_type, session_id, party_id, expires_at)
                SELECT ?, ?, ?, ?, ?, ?, datetime('now', '+5 minutes')
                WHERE NOT EXISTS (
                    SELECT 1 FROM invites
                    WHERE receiver = ?
                      AND kind = ?
                      AND party_id = ?
                      AND session_type = ?
                      AND session_id = ?
                      AND consumed_at IS NULL
                      AND expires_at > CURRENT_TIMESTAMP
                )
                ",
            )
            .bind(leader_id)
            .bind(receiver_id)
            .bind(INVITE_KIND_PARTY_FOLLOW)
            .bind(session_type)
            .bind(session_id)
            .bind(party_id)
            .bind(receiver_id)
            .bind(INVITE_KIND_PARTY_FOLLOW)
            .bind(party_id)
            .bind(session_type)
            .bind(session_id)
            .execute(&mut **transaction)
            .await?;
            created += result.rows_affected();
        }
        sqlx::query("UPDATE parties SET updated_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(party_id)
            .execute(&mut **transaction)
            .await?;
        Ok(created)
    }
}
