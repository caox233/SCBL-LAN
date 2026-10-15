use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;

use slog::Logger;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

mod gameplay;
mod accounts;
mod invites;
mod parties;
mod presence;
mod sessions;
mod station_urls;

type Result<T> = eyre::Result<T>;

pub(crate) const INVITE_KIND_PRIVATE_ROOM: i32 = 1;
pub(crate) const INVITE_KIND_LOBBY_PARTY: i32 = 2;
pub(crate) const INVITE_KIND_PARTY_FOLLOW: i32 = 3;
pub(crate) const INVITE_KIND_LOBBY_RESTORE: i32 = 4;
const MANUAL_INVITATION_SEARCH_TTL: Duration = Duration::from_secs(15);
const GAME_ROOM_DEPARTURE_CORRELATION_TTL: Duration = Duration::from_millis(750);
const LOBBY_RESTORE_SPLIT_TTL: Duration = Duration::from_secs(2);
const NATIVE_HOST_MIGRATION_TTL: Duration = Duration::from_secs(15);
const QUICK_MATCH_FORMER_HOST_RESTORE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteGameSessionOutcome {
    NotFound,
    Destroyed,
    MigrationPending { remaining_participants: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingAcceptedNormalInvite {
    invite_id: i64,
    target_session_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcceptedNormalInviteTransition {
    Pending(PendingAcceptedNormalInvite),
    Joined,
}

#[derive(Debug, Clone, Copy)]
struct PendingQuickMatchMemberLeave {
    generation: u64,
    recorded_at: Instant,
    accepted_invite: Option<PendingAcceptedNormalInvite>,
}

#[derive(Debug, Clone)]
struct QuickMatchPartyDeparture {
    captured_at: Instant,
    party_id: i64,
    leader_id: u32,
    room_host_id: u32,
    lobby_session_id: u32,
    members: Vec<u32>,
    departed_members: HashSet<u32>,
}

#[derive(Debug, Clone, Copy)]
struct QuickMatchFormerHostRestoreGate {
    started_at: Instant,
    seed_lobby_session_id: u32,
    party_id: i64,
}

#[derive(Debug, Clone, Copy)]
struct LobbyRestoreSplitTransition {
    generation: u64,
    started_at: Instant,
    removal_deferred: bool,
}

#[derive(Debug, Clone)]
struct NativeMigrationCleanup {
    creator_id: u32,
    remaining_participants: Vec<u32>,
    outcome: DeleteGameSessionOutcome,
}

#[derive(Debug, Clone)]
struct PendingNativeMigration {
    generation: u64,
    started_at: Instant,
    cleanup: Option<NativeMigrationCleanup>,
}

// A successful worker may acknowledge only the exact operation it observed.
// Cancellation deletes the entry; replacement has a new generation. Neither is
// undone by a delayed acknowledgement or a failed worker.
fn cleanup_is_current<K: Eq + std::hash::Hash, V>(map: &Mutex<HashMap<K, V>>, key: &K, generation: u64, version: impl Fn(&V) -> u64) -> bool {
    map.lock().is_ok_and(|items| items.get(key).is_some_and(|item| version(item) == generation))
}
fn complete_cleanup<K: Eq + std::hash::Hash, V>(map: &Mutex<HashMap<K, V>>, key: &K, generation: u64, version: impl Fn(&V) -> u64) {
    if let Ok(mut items) = map.lock() {
        if items.get(key).is_some_and(|item| version(item) == generation) {
            items.remove(key);
        }
    }
}

fn session_attribute_value(attributes: &str, attribute_id: u32) -> Option<u32> {
    attributes.split(';').find_map(|entry| {
        let (raw_id, raw_value) = entry.split_once("=>")?;
        let id = raw_id.trim().parse::<u32>().ok()?;
        if id != attribute_id {
            return None;
        }
        raw_value.trim().parse::<u32>().ok()
    })
}

fn is_game_room_attributes(attributes: &str) -> bool {
    session_attribute_value(attributes, 113) == Some(0)
}

fn is_private_room_attributes(attributes: &str) -> bool {
    is_game_room_attributes(attributes) && session_attribute_value(attributes, 3) == Some(0) && session_attribute_value(attributes, 4).is_some_and(|slots| slots > 0)
}

/// Response-only compatibility projection for an accepted direct private invite.
/// Persistent attributes and every field except Session class `113` remain
/// byte-for-byte equivalent at the semantic attribute level.
fn project_private_invite_session_class_only(attributes: &str) -> String {
    let mut projected = Vec::new();
    let mut replaced = false;

    for entry in attributes.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((raw_id, _)) = entry.split_once("=>") {
            if raw_id.trim() == "113" {
                projected.push(String::from("113 => 1"));
                replaced = true;
                continue;
            }
        }
        projected.push(entry.to_string());
    }

    if !replaced {
        projected.push(String::from("113 => 1"));
    }
    projected.join(";")
}

fn run<F>(future: F) -> Result<F::Output>
where
    F: std::future::Future,
{
    Ok(tokio::runtime::Builder::new_current_thread().enable_time().build()?.block_on(future))
}

pub(crate) fn mode_current_sync(storage:&Storage,user:u32)->Result<Option<i64>> {run(storage.current_game_mode(user))?}

/// A deferred close still belongs to its exact transport ticket owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecureCloseAttempt {
    Committed { preserved: bool },
    Deferred,
}

pub struct Storage {
    approved_release: server_api::release_contract::ReleaseIdentity,
    pub(crate) mode_gate:tokio::sync::RwLock<()>,
    cleanup_worker: Mutex<()>,
    cleanup_generation: AtomicU64,
    logger: Logger,
    pool: SqlitePool,
    // Dedicated retry lane: identical DB identity, never changes main-pool waits.
    secure_close_pool: SqlitePool,
    #[cfg(test)]
    discovery_checkpoint: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    login_checkpoint: Mutex<Option<(&'static str, Box<dyn FnOnce() + Send>)>>,
    manual_invitation_searches: Mutex<HashMap<(u32, u32), Instant>>,
    recent_game_room_departures: Mutex<HashMap<(u32, u32, u32), Instant>>,
    native_host_migrations: Mutex<HashMap<(u32, u32), PendingNativeMigration>>,
    lobby_restore_split_transitions: Mutex<HashMap<(u32, u32, u32), LobbyRestoreSplitTransition>>,
    quick_match_party_departures: Mutex<HashMap<(u32, u32, i64), QuickMatchPartyDeparture>>,
    quick_match_former_host_restore_gates: Mutex<HashMap<(u32, u32), QuickMatchFormerHostRestoreGate>>,
    pending_quick_match_member_leaves: Mutex<HashMap<(u32, u32, u32), PendingQuickMatchMemberLeave>>,
    published_game_sessions: Mutex<HashMap<u32, u32>>,
}

pub enum LoginError {
    NotFound,
    InvalidPassword,
}

impl Storage {
    pub fn init(logger: Logger, approved_release: server_api::release_contract::ReleaseIdentity) -> Result<Self> {
        Self::init_with_database_url_and_release(logger, "sqlite://5th-echelon.db?mode=rwc", approved_release)
    }

    #[cfg(test)]
    fn init_with_database_url(logger: Logger, database_url: &str) -> Result<Self> {
        Self::init_with_database_url_and_release(logger, database_url, crate::api::test_release_identity())
    }

    fn init_with_database_url_and_release(logger: Logger, database_url: &str, approved_release: server_api::release_contract::ReleaseIdentity) -> Result<Self> {
        let (pool, secure_close_pool) = run(async {
            let connect_options = database_url
                .parse::<SqliteConnectOptions>()?
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(5))
                .journal_mode(SqliteJournalMode::Wal);
            // Clone the parsed identity (also preserves SQLx's generated shared
            // in-memory DB name), rather than parsing a second different DB.
            let close_options = connect_options.clone()
                .busy_timeout(Duration::ZERO)
                .optimize_on_close(false, None);
            let pool = SqlitePoolOptions::new().connect_with(connect_options).await?;
            sqlx::migrate!("src/storage/migrations").run(&pool).await?;
            let secure_close_pool = SqlitePoolOptions::new()
                .max_connections(1)
                .min_connections(0)
                .idle_timeout(None)
                .max_lifetime(None)
                .acquire_timeout(Duration::from_millis(25))
                .connect_lazy_with(close_options);
            Ok::<_, eyre::Error>((pool, secure_close_pool))
        })??;
        Ok(Self {
            approved_release,
            mode_gate:tokio::sync::RwLock::new(()),
            cleanup_worker: Mutex::new(()),
            cleanup_generation: AtomicU64::new(1),
            logger,
            pool,
            secure_close_pool,
            #[cfg(test)]
            discovery_checkpoint: Mutex::new(None),
            #[cfg(test)]
            login_checkpoint: Mutex::new(None),
            manual_invitation_searches: Mutex::new(HashMap::new()),
            recent_game_room_departures: Mutex::new(HashMap::new()),
            native_host_migrations: Mutex::new(HashMap::new()),
            lobby_restore_split_transitions: Mutex::new(HashMap::new()),
            quick_match_party_departures: Mutex::new(HashMap::new()),
            quick_match_former_host_restore_gates: Mutex::new(HashMap::new()),
            pending_quick_match_member_leaves: Mutex::new(HashMap::new()),
            published_game_sessions: Mutex::new(HashMap::new()),
        })
    }

    fn next_cleanup_generation(&self) -> u64 {
        self.cleanup_generation.fetch_add(1, Ordering::Relaxed)
    }

    // Event polls can overlap. Never wait for another cleanup worker while
    // holding an outer mode lock, and never hold a map lock across DB work.
    fn begin_cleanup(&self) -> Result<Option<std::sync::MutexGuard<'_, ()>>> {
        match self.cleanup_worker.try_lock() {
            Ok(worker) => Ok(Some(worker)),
            Err(std::sync::TryLockError::WouldBlock) => Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => Err(eyre::eyre!("Cleanup worker panicked")),
        }
    }

    fn note_native_host_migration(&self, type_id: u32, session_id: u32) {
        if let Ok(mut migrations) = self.native_host_migrations.lock() {
            migrations.insert((type_id, session_id), PendingNativeMigration {generation: self.next_cleanup_generation(), started_at: Instant::now(), cleanup: None});
        }
    }

    fn native_host_migration_is_active(&self, type_id: u32, session_id: u32) -> bool {
        let Ok(migrations) = self.native_host_migrations.lock() else {
            return false;
        };
        migrations.get(&(type_id, session_id)).is_some_and(|started| started.started_at.elapsed() <= NATIVE_HOST_MIGRATION_TTL)
    }

    fn clear_native_host_migration(&self, type_id: u32, session_id: u32) {
        if let Ok(mut migrations) = self.native_host_migrations.lock() {
            migrations.remove(&(type_id, session_id));
        }
    }

    pub(crate) fn native_host_migration_cleanup_due(&self) -> bool {
        self.native_host_migrations
            .lock()
            .is_ok_and(|migrations| migrations.values().any(|started| started.started_at.elapsed() > NATIVE_HOST_MIGRATION_TTL))
    }

    fn pending_expired_native_host_migrations(&self) -> Vec<(u32, u32, PendingNativeMigration)> {
        let Ok(migrations) = self.native_host_migrations.lock() else { return Vec::new(); };
        migrations.iter().filter_map(|(key, pending)|
            (pending.started_at.elapsed() > NATIVE_HOST_MIGRATION_TTL).then(|| (key.0, key.1, pending.clone()))).collect()
    }

    pub fn record_published_game_session(&self, user_id: u32, session_id: u64) {
        if let Ok(mut sessions) = self.published_game_sessions.lock() {
            if let Ok(session_id) = u32::try_from(session_id) {
                sessions.insert(user_id, session_id);
            } else {
                sessions.remove(&user_id);
            }
        }
    }

    pub fn clear_published_game_session(&self, user_id: u32) {
        if let Ok(mut sessions) = self.published_game_sessions.lock() {
            sessions.remove(&user_id);
        }
    }

    fn published_game_session(&self, user_id: u32) -> Option<u32> {
        self.published_game_sessions.lock().ok()?.get(&user_id).copied()
    }

    fn record_game_room_departure(&self, type_id: u32, session_id: u32, user_id: u32) {
        if let Ok(mut departures) = self.recent_game_room_departures.lock() {
            let now = Instant::now();
            departures.retain(|_, recorded_at| now.duration_since(*recorded_at) <= GAME_ROOM_DEPARTURE_CORRELATION_TTL);
            departures.insert((type_id, session_id, user_id), now);
        }
    }

    fn recent_game_room_departures(&self, type_id: u32, session_id: u32) -> Vec<u32> {
        let Ok(mut departures) = self.recent_game_room_departures.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        departures.retain(|_, recorded_at| now.duration_since(*recorded_at) <= GAME_ROOM_DEPARTURE_CORRELATION_TTL);
        departures
            .iter()
            .filter_map(|((departure_type, departure_session, user_id), _)| (*departure_type == type_id && *departure_session == session_id).then_some(*user_id))
            .collect()
    }

    #[cfg(test)]
    fn record_pending_quick_match_member_leave(&self, type_id: u32, session_id: u32, user_id: u32) {
        self.record_pending_quick_match_member_leave_for_invite(type_id, session_id, user_id, None);
    }

    #[cfg(test)]
    fn record_pending_quick_match_member_leave_for_invite(&self, type_id: u32, session_id: u32, user_id: u32, accepted_invite: Option<PendingAcceptedNormalInvite>) {
        if let Ok(mut departures) = self.pending_quick_match_member_leaves.lock() {
            departures.insert(
                (type_id, session_id, user_id),
                PendingQuickMatchMemberLeave {
                    generation: self.next_cleanup_generation(),
                    recorded_at: Instant::now(),
                    accepted_invite,
                },
            );
        }
    }

    fn pending_quick_match_member_leaves(&self, type_id: u32, session_id: u32) -> Vec<u32> {
        let Ok(departures) = self.pending_quick_match_member_leaves.lock() else {
            return Vec::new();
        };
        departures
            .keys()
            .filter_map(|(departure_type, departure_session, user_id)| (*departure_type == type_id && *departure_session == session_id).then_some(*user_id))
            .collect()
    }

    fn clear_pending_quick_match_member_leaves(&self, type_id: u32, session_id: u32) {
        if let Ok(mut departures) = self.pending_quick_match_member_leaves.lock() {
            departures.retain(|(departure_type, departure_session, _), _| *departure_type != type_id || *departure_session != session_id);
        }
    }

    pub(crate) fn pending_quick_match_member_leave_cleanup_due(&self) -> bool {
        self.pending_quick_match_member_leaves
            .lock()
            .is_ok_and(|departures| departures.values().any(|pending| pending.recorded_at.elapsed() > GAME_ROOM_DEPARTURE_CORRELATION_TTL))
    }

    fn pending_expired_quick_match_member_leaves(&self) -> Vec<(u32, u32, u32, PendingQuickMatchMemberLeave)> {
        let Ok(departures) = self.pending_quick_match_member_leaves.lock() else { return Vec::new(); };
        departures.iter().filter_map(|(key, pending)|
            (pending.recorded_at.elapsed() > GAME_ROOM_DEPARTURE_CORRELATION_TTL).then_some((key.0, key.1, key.2, *pending))).collect()
    }

    fn clear_game_room_departures(&self, type_id: u32, session_id: u32) {
        if let Ok(mut departures) = self.recent_game_room_departures.lock() {
            departures.retain(|(departure_type, departure_session, _), _| *departure_type != type_id || *departure_session != session_id);
        }
    }

    fn clear_game_room_departure_for_rejoin(&self, type_id: u32, session_id: u32, user_id: u32) {
        if let Ok(mut departures) = self.recent_game_room_departures.lock() {
            departures.remove(&(type_id, session_id, user_id));
        }
        if let Ok(mut departures) = self.pending_quick_match_member_leaves.lock() {
            departures.remove(&(type_id, session_id, user_id));
        }
        if let Ok(mut snapshots) = self.quick_match_party_departures.lock() {
            snapshots.retain(|(snapshot_type, snapshot_session, _), _| *snapshot_type != type_id || *snapshot_session != session_id);
        }
    }

    fn remember_quick_match_party_departure(&self, type_id: u32, session_id: u32, snapshot: QuickMatchPartyDeparture) {
        if let Ok(mut snapshots) = self.quick_match_party_departures.lock() {
            let now = Instant::now();
            snapshots.retain(|_, pending| now.duration_since(pending.captured_at) <= GAME_ROOM_DEPARTURE_CORRELATION_TTL);
            let key = (type_id, session_id, snapshot.party_id);
            if let Some(pending) = snapshots.get_mut(&key) {
                pending.captured_at = snapshot.captured_at;
                pending.members.extend(snapshot.members);
                pending.members.sort_unstable();
                pending.members.dedup();
                pending.departed_members.extend(snapshot.departed_members);
            } else {
                snapshots.insert(key, snapshot);
            }
        }
    }

    fn note_quick_match_party_member_departed(&self, type_id: u32, session_id: u32, user_id: u32) -> Option<QuickMatchPartyDeparture> {
        let mut snapshots = self.quick_match_party_departures.lock().ok()?;
        let now = Instant::now();
        snapshots.retain(|_, pending| now.duration_since(pending.captured_at) <= GAME_ROOM_DEPARTURE_CORRELATION_TTL);
        let key = snapshots.iter_mut().find_map(|(key, pending)| {
            if key.0 != type_id || key.1 != session_id || !pending.members.contains(&user_id) {
                return None;
            }
            pending.departed_members.insert(user_id);
            (pending.departed_members.len() >= 2).then_some(*key)
        })?;
        snapshots.remove(&key)
    }

    fn clear_quick_match_party_departure(&self, type_id: u32, session_id: u32, party_id: i64) {
        if let Ok(mut snapshots) = self.quick_match_party_departures.lock() {
            snapshots.remove(&(type_id, session_id, party_id));
        }
    }

    fn note_quick_match_former_host_restore_gate(&self, type_id: u32, user_id: u32, seed_lobby_session_id: u32, party_id: i64) {
        if let Ok(mut gates) = self.quick_match_former_host_restore_gates.lock() {
            gates.insert(
                (type_id, user_id),
                QuickMatchFormerHostRestoreGate {
                    started_at: Instant::now(),
                    seed_lobby_session_id,
                    party_id,
                },
            );
        }
    }

    fn quick_match_former_host_restore_gate(&self, type_id: u32, user_id: u32) -> Option<QuickMatchFormerHostRestoreGate> {
        let mut gates = self.quick_match_former_host_restore_gates.lock().ok()?;
        let now = Instant::now();
        gates.retain(|_, gate| now.duration_since(gate.started_at) <= QUICK_MATCH_FORMER_HOST_RESTORE_TTL);
        gates.get(&(type_id, user_id)).copied()
    }

    fn clear_quick_match_former_host_restore_gate(&self, type_id: u32, user_id: u32) {
        if let Ok(mut gates) = self.quick_match_former_host_restore_gates.lock() {
            gates.remove(&(type_id, user_id));
        }
    }

    /// Acquires `SQLite`'s single writer slot before a multi-step mutation reads
    /// any state. A deferred transaction can otherwise read successfully and
    /// then fail immediately with `SQLITE_BUSY` while upgrading to a writer,
    /// even though every pooled connection has a busy timeout.
    async fn begin_write(&self) -> sqlx::Result<sqlx::Transaction<'static, sqlx::Sqlite>> {
        self.pool.begin_with("BEGIN IMMEDIATE").await
    }

    /// Handles a secure PRUDP connection closing or expiring. When the
    /// in-game overlay has checked in recently, only the obsolete secure ticket
    /// is removed; the lobby/room membership and station URL stay available.
    /// The preserved state is cleaned after the API heartbeat becomes stale.
    /// Returns true when game state was preserved.
    #[cfg(test)]
    pub fn close_secure_session(&self,user_id:u32)->Result<bool> {self.close_selected_secure_session(user_id,None)}

    #[cfg(test)]
    pub fn close_secure_ticket(&self,user_id:u32,fingerprint:Option<[u8;32]>)->Result<bool> {
        let Some(hash)=fingerprint else {return Ok(true)};
        let _generation=self.mode_gate.blocking_read();
        let hash=sodiumoxide::hex::encode(hash);
        self.close_selected_secure_session(user_id,Some(hash))
    }

    /// Attempt an exact-ticket close without queueing behind a mode writer or
    /// SQLite's five-second busy timeout. No close SQL is started until acquire
    /// succeeds; after that, every SQL/commit/rollback/close is awaited in full.
    pub(crate) fn try_close_secure_ticket(&self, user_id: u32, fingerprint: Option<[u8; 32]>) -> Result<SecureCloseAttempt> {
        use sqlx::Connection;

        let Some(hash) = fingerprint else {
            return Ok(SecureCloseAttempt::Committed { preserved: true });
        };
        let Ok(_generation) = self.mode_gate.try_read() else {
            return Ok(SecureCloseAttempt::Deferred);
        };
        let hash = sodiumoxide::hex::encode(hash);
        run(async {
            let mut connection = match self.secure_close_pool.acquire().await {
                Ok(connection) => connection,
                Err(sqlx::Error::PoolTimedOut) => return Ok(SecureCloseAttempt::Deferred),
                Err(error) => return Err(eyre::Report::from(error)),
            };
            // The transaction borrows this connection. It cannot escape this
            // runtime or silently return an unacknowledged write to the pool.
            let outcome: sqlx::Result<bool> = async {
                let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
                match Self::close_selected_secure_session_in_transaction(&mut transaction, user_id, Some(&hash)).await {
                    Ok((preserved, _handoffs)) => {
                        transaction.commit().await?;
                        Ok(preserved)
                    }
                    Err(error) => {
                        // Explicit rollback of body failures, not async Drop.
                        if let Err(rollback_error) = transaction.rollback().await {
                            error!(self.logger, "Secure close rollback failed; closing connection"; "cause" => %error, "rollback" => %rollback_error);
                            return Err(rollback_error);
                        }
                        Err(error)
                    }
                }
            }.await;
            match outcome {
                Ok(preserved) => {
                    // SQLx 0.8.6 PoolConnection::Drop otherwise spawns a return
                    // task on our about-to-end temporary current-thread runtime.
                    connection.return_to_pool().await;
                    Ok(SecureCloseAttempt::Committed { preserved })
                }
                Err(error) => {
                    // SQLx commit errors enqueue rollback in Transaction::Drop.
                    // Graceful close is FIFO after that rollback and waits for
                    // SQLite worker Shutdown. Never recycle a doubtful connection,
                    // and never time out this close / leave an orphan business write.
                    connection.close().await.map_err(|close_error| {
                        eyre::eyre!("secure close failed: {error}; connection shutdown failed: {close_error}")
                    })?;
                    let code = error.as_database_error().and_then(|error| error.code())
                        .and_then(|code| code.parse::<i32>().ok());
                    if code.is_some_and(|code| matches!(code & 0xff, 5 | 6)) {
                        // SQLite primary BUSY / LOCKED, including extended codes.
                        Ok(SecureCloseAttempt::Deferred)
                    } else {
                        Err(error.into())
                    }
                }
            }
        })?
    }

    #[cfg(test)]
    fn close_selected_secure_session(&self, user_id:u32, fingerprint:Option<String>) -> Result<bool> {
        let (preserved, _handoffs) = run(async {
            let mut transaction = self.begin_write().await?;
            let outcome = Self::close_selected_secure_session_in_transaction(&mut transaction, user_id, fingerprint.as_deref()).await?;
            transaction.commit().await?;
            Ok::<_, sqlx::Error>(outcome)
        })??;
        Ok(preserved)
    }

    // Shared business body: ticket precision, active-peer/API preservation and
    // existing host cleanup policy do not vary with the writer acquisition lane.
    async fn close_selected_secure_session_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_id: u32,
        fingerprint: Option<&str>,
    ) -> sqlx::Result<(bool, Vec<(u32, u32, u32)>)> {
        let (recent_api,): (i64,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM users WHERE id = ? AND last_api_seen_at >= datetime('now', '-15 seconds'))")
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?;

        if let Some(hash)=fingerprint {
            let removed=sqlx::query("DELETE FROM user_sessions WHERE user_id=? AND ticket_hash=?").bind(user_id).bind(hash).execute(&mut **transaction).await?.rows_affected();
            let remaining:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_sessions WHERE user_id=? AND native_connected=1)").bind(user_id).fetch_one(&mut **transaction).await?;
            if removed==0 || remaining {
                return Ok((true,Vec::new()));
            }
        } else {
            sqlx::query("DELETE FROM user_sessions WHERE user_id = ?").bind(user_id).execute(&mut **transaction).await?;
        }
        sqlx::query("UPDATE users SET is_online = 0 WHERE id = ?").bind(user_id).execute(&mut **transaction).await?;

        if recent_api != 0 {
            return Ok::<(bool, Vec<(u32, u32, u32)>), sqlx::Error>((true, Vec::new()));
        }

        // The selected ticket was already deleted above. A newer ticket may
        // still be between CONNECT and Register (native_connected=0); keep it.
        let handoffs = Self::cleanup_user_game_state_preserving_tickets_async(transaction, user_id).await?;
        Ok::<(bool, Vec<(u32, u32, u32)>), sqlx::Error>((false, handoffs))
    }

    // Whole-user/run cleanup retains its original all-ticket deletion contract.
    async fn cleanup_user_game_state_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, user_id: u32) -> sqlx::Result<Vec<(u32, u32, u32)>> {
        let handoffs = Self::cleanup_user_game_state_preserving_tickets_async(transaction, user_id).await?;
        sqlx::query("DELETE FROM user_sessions WHERE user_id = ?").bind(user_id).execute(&mut **transaction).await?;
        Ok(handoffs)
    }

    // Business-state cleanup does not own unrelated pending/native tickets.
    async fn cleanup_user_game_state_preserving_tickets_async(transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>, user_id: u32) -> sqlx::Result<Vec<(u32, u32, u32)>> {
        let affected_sessions = sqlx::query_as::<_, (u32, u32, u32)>(
            r"
            SELECT DISTINCT g.type_id, g.id, g.creator_id
            FROM game_sessions AS g
            LEFT JOIN participants AS p ON p.game_id = g.id
            WHERE g.destroyed_at IS NULL
              AND (g.creator_id = ? OR p.user_id = ?)
            ",
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&mut **transaction)
        .await?;

        let mut pending_migrations = Vec::new();
        for (type_id, session_id, creator_id) in &affected_sessions {
            if *creator_id != user_id {
                continue;
            }
            let (remaining_participants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM participants WHERE game_id = ? AND user_id != ?")
                .bind(session_id)
                .bind(user_id)
                .fetch_one(&mut **transaction)
                .await?;
            if remaining_participants > 0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                pending_migrations.push((*type_id, *session_id, remaining_participants as u32));
            }
        }

        // A departing host remains the recorded owner/member only until the
        // game-selected successor submits SplitSession.  Preselecting the
        // lowest participant here makes two clients believe they are host and
        // splits the N-1 Party.  Non-host memberships and solo host state can
        // still be removed immediately.
        let has_pending_migration = !pending_migrations.is_empty();
        if !has_pending_migration {
            sqlx::query("DELETE FROM parties WHERE leader_id = ?").bind(user_id).execute(&mut **transaction).await?;
            sqlx::query("DELETE FROM party_members WHERE user_id = ?").bind(user_id).execute(&mut **transaction).await?;
        }
        sqlx::query(
            r"
            DELETE FROM participants
            WHERE user_id = ?
              AND NOT EXISTS (
                  SELECT 1
                  FROM game_sessions AS session
                  INNER JOIN participants AS survivor
                      ON survivor.game_id = session.id AND survivor.user_id != ?
                  WHERE session.id = participants.game_id
                    AND session.creator_id = ?
                    AND session.destroyed_at IS NULL
              )
            ",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .execute(&mut **transaction)
        .await?;
        sqlx::query("DELETE FROM station_urls WHERE user_id = ?").bind(user_id).execute(&mut **transaction).await?;
        sqlx::query("UPDATE users SET is_online = 0 WHERE id = ?").bind(user_id).execute(&mut **transaction).await?;

        for (type_id, session_id, _) in affected_sessions {
            Self::reconcile_game_session_async(transaction, type_id, session_id).await?;
        }
        Ok(pending_migrations)
    }

    /// Removes preserved lobby/room state after both the secure session and
    /// overlay heartbeat are gone. Active secure users have `is_online=1`
    /// and are never selected by this sweep.
    pub async fn cleanup_stale_api_sessions_async(&self) -> Result<()> {
        let mut transaction = self.begin_write().await?;
        let stale_users: Vec<(u32,)> = sqlx::query_as(
            r"
            SELECT id
            FROM users
            WHERE is_online = 0
              AND (last_api_seen_at IS NULL OR last_api_seen_at < datetime('now', '-15 seconds'))
              AND (
                  EXISTS (SELECT 1 FROM station_urls WHERE station_urls.user_id = users.id)
                  OR EXISTS (SELECT 1 FROM participants WHERE participants.user_id = users.id)
              )
            ",
        )
        .fetch_all(&mut *transaction)
        .await?;

        let mut _handoffs = Vec::new();
        for (user_id,) in stale_users {
            _handoffs.extend(
                Self::cleanup_user_game_state_preserving_tickets_async(&mut transaction, user_id)
                    .await?
                    .into_iter()
                    .map(|(type_id, session_id, remaining_participants)| (type_id, session_id, user_id, remaining_participants)),
            );
            // Keep only unregistered tickets that Register can still mark. This
            // is the same approved/current/90-second run predicate as
            // mark_secure_ticket; room/Party/URL cleanup above still runs.
            sqlx::query(
                r"
                DELETE FROM user_sessions
                WHERE user_id = ? AND NOT (
                    native_connected = 0 AND ticket_hash IS NOT NULL
                    AND EXISTS (
                        SELECT 1 FROM game_runs r
                        WHERE r.id = user_sessions.game_run_id
                          AND r.user_id = user_sessions.user_id
                          AND r.release_identity = ?
                          AND r.retired_at IS NULL
                          AND r.seen_at >= datetime('now', '-90 seconds')
                    )
                )
                ",
            )
            .bind(user_id)
            .bind(self.approved_release.storage_key())
            .execute(&mut *transaction)
            .await?;
        }
        // A preserved pending ticket may outlive the last room/URL row (for
        // example after host migration). Prune invalid pending tickets even
        // without those rows, but never broaden the business cleanup selector
        // or touch online users, fresh API leases, or registered tickets here.
        sqlx::query(
            r"
            DELETE FROM user_sessions
            WHERE native_connected = 0
              AND EXISTS (
                  SELECT 1 FROM users u WHERE u.id = user_sessions.user_id
                    AND u.is_online = 0
                    AND (u.last_api_seen_at IS NULL OR u.last_api_seen_at < datetime('now', '-15 seconds'))
              )
              AND (ticket_hash IS NULL OR NOT EXISTS (
                  SELECT 1 FROM game_runs r
                  WHERE r.id = user_sessions.game_run_id
                    AND r.user_id = user_sessions.user_id
                    AND r.release_identity = ?
                    AND r.retired_at IS NULL
                    AND r.seen_at >= datetime('now', '-90 seconds')
              ))
            ",
        )
        .bind(self.approved_release.storage_key())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub fn invalidate_sessions(&self) -> Result<()> {
        run(async {
            // Restart rotates the signing key and invalidates every native ticket.
            // Clear nonce tombstones too so the still-running same DLL can relogin.
            sqlx::query("DELETE FROM game_runs").execute(&self.pool).await?;
            sqlx::query("DELETE FROM parties").execute(&self.pool).await?;
            sqlx::query("DELETE FROM party_members").execute(&self.pool).await?;
            sqlx::query("DELETE FROM station_urls").execute(&self.pool).await?;
            sqlx::query("DELETE FROM user_sessions").execute(&self.pool).await?;
            sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE destroyed_at IS NULL")
                .execute(&self.pool)
                .await?;
            sqlx::query("UPDATE users SET is_online=0").execute(&self.pool).await
        })??;

        Ok(())
    }

    pub fn create_game_session(&self, user_id: u32, type_id: u32, attributes: String) -> Result<u32> {
        if run(self.current_game_mode(user_id))??.is_none() { return Err(eyre::eyre!("PACKAGE_IDENTITY_REQUIRED: approved game run required")); }
        let game_room = is_game_room_attributes(&attributes);
        let personal_lobby = session_attribute_value(&attributes, 113) == Some(1);
        let (id, follow_count, retired_solo_lobbies, regroup_followers, personal_party_ensured) = run(async {
            let mut transaction = self.begin_write().await?;
            let id = sqlx::query("INSERT INTO game_sessions (type_id, creator_id, attributes, release_identity) VALUES (?, ?, ?, (SELECT release_identity FROM game_runs WHERE user_id=? AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds')))")
                .bind(type_id)
                .bind(user_id)
                .bind(&attributes)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?
                .last_insert_rowid();
            sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                .bind(id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?;
            let retired_solo_lobbies = if personal_lobby {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Self::retire_superseded_solo_lobbies_async(&mut transaction, user_id, type_id, id as u32).await?
            } else {
                0
            };
            let regroup_followers = if personal_lobby {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Self::complete_private_room_party_leader_restore_async(&mut transaction, user_id, type_id, id as u32).await?
            } else {
                0
            };
            let personal_party_ensured = if personal_lobby {
                Self::ensure_personal_party_if_unassigned_async(&mut transaction, user_id).await?
            } else {
                false
            };
            let follow_count = if game_room {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Self::queue_party_follow_invites_async(&mut transaction, user_id, type_id, id as u32).await?
            } else {
                0
            };
            transaction.commit().await?;
            Ok::<(i64, u64, u64, u64, bool), sqlx::Error>((id, follow_count, retired_solo_lobbies, regroup_followers, personal_party_ensured))
        })??;

        #[allow(clippy::cast_possible_truncation)]
        #[allow(clippy::cast_sign_loss)]
        let session_id = id as u32;
        if follow_count > 0 {
            info!(
                self.logger,
                "PartySessionBound leader_id={user_id} session_type={type_id} session_id={session_id} follow_invites={follow_count}"
            );
        }
        if retired_solo_lobbies > 0 {
            info!(
                self.logger,
                "SupersededSoloLobbyRetired user_id={user_id} type_id={type_id} replacement_session_id={session_id} retired_count={retired_solo_lobbies}"
            );
        }
        if regroup_followers > 0 {
            info!(
                self.logger,
                "PrivateRoomPartyLeaderRestored leader_id={user_id} session_type={type_id} lobby_session_id={session_id} follower_invites={regroup_followers}"
            );
        }
        if personal_party_ensured {
            info!(
                self.logger,
                "PersonalLobbyPartyEnsured user_id={user_id} type_id={type_id} session_id={session_id} member_count=1"
            );
        }
        Ok(session_id)
    }

    /// Records a validated `JoinSession` caller as a participant without applying
    /// any capacity gate. The game may repeat `JoinSession` and `AddParticipants`,
    /// so INSERT OR IGNORE keeps the operation idempotent.
    ///
    /// Once a participant is newly persisted, all of that receiver's outstanding
    /// invitations are consumed in the same transaction. Repeated `JoinSession`
    /// calls remain idempotent and cannot erase invitations received later.
    ///
    /// Returns false only when the referenced session is missing or destroyed.
    pub fn join_game_session_unbounded(&self, user_id: u32, type_id: u32, session_id: u32) -> Result<bool> {
        let (joined, follow_count, regroup_followers) = run(async {
            let mut transaction = self.begin_write().await?;
            let session = sqlx::query_as::<_, (u32, String)>("SELECT creator_id, attributes FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL AND release_identity=?")
                .bind(session_id)
                .bind(type_id)
                .bind(self.approved_release.storage_key())
                .fetch_optional(&mut *transaction)
                .await?;
            let Some((creator_id, attributes)) = session else {
                transaction.rollback().await?;
                return Ok::<(bool, u64, u64), sqlx::Error>((false, 0, 0));
            };

            let inserted = sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                .bind(session_id)
                .bind(user_id)
                .execute(&mut *transaction)
                .await?
                .rows_affected()
                > 0;

            let accepted_party_invite = self.attach_accepted_invite_party_async(&mut transaction, user_id, type_id, session_id).await?;

            let regroup_followers = if creator_id == user_id && session_attribute_value(&attributes, 113) == Some(1) {
                Self::complete_private_room_party_leader_restore_async(&mut transaction, user_id, type_id, session_id).await?
            } else {
                0
            };

            if inserted || accepted_party_invite.is_some() {
                Self::consume_all_invites_for_receiver_async(&mut transaction, user_id).await?;
            }

            if accepted_party_invite == Some(INVITE_KIND_LOBBY_RESTORE) {
                // The native client can ask the current lobby owner to remove
                // this just-restored member immediately before that member's
                // SplitSession elects the final transport host. Keep a short,
                // explicit transition after the invite row is consumed so the
                // two calls cannot be misread as an explicit Leave Party.
                self.note_lobby_restore_split_transition(type_id, session_id, user_id);
            }

            let private_room = is_private_room_attributes(&attributes);
            let follow_count = if is_game_room_attributes(&attributes) {
                let follow_leader_id = if private_room { creator_id } else { user_id };
                Self::queue_party_follow_invites_async(&mut transaction, follow_leader_id, type_id, session_id).await?
            } else {
                0
            };
            transaction.commit().await?;
            Ok::<(bool, u64, u64), sqlx::Error>((true, follow_count, regroup_followers))
        })??;
        if follow_count > 0 {
            info!(
                self.logger,
                "PartySessionBound session_type={type_id} session_id={session_id} follow_invites={follow_count}"
            );
        }
        if regroup_followers > 0 {
            info!(
                self.logger,
                "PrivateRoomPartyLeaderRestored leader_id={user_id} session_type={type_id} lobby_session_id={session_id} follower_invites={regroup_followers}"
            );
        }
        if joined {
            // A successful rejoin starts a new room-membership generation.
            // Old departure correlation for this same user/session must not be
            // combined with a later coordinated leave during rapid testing.
            self.clear_game_room_departure_for_rejoin(type_id, session_id, user_id);
        }
        Ok(joined)
    }

    pub fn add_participants(&self, caller_id: u32, type_id: u32, session_id: u32, private_participants: Vec<u32>, public_participants: Vec<u32>) -> Result<()> {
        let private_participants = private_participants.into_iter().collect::<HashSet<_>>();
        let public_participants = public_participants.into_iter().collect::<HashSet<_>>();
        if private_participants.is_empty() && public_participants.is_empty() {
            warn!(self.logger, "Empty participant list");
            return Ok(());
        }
        let (mut deferred_private, mut host_admitted_private, mut persisted_private, mut newly_admitted) = run(async {
            let mut transaction = self.begin_write().await?;
            let session = sqlx::query_as::<_, (u32, String)>("SELECT creator_id, attributes FROM game_sessions WHERE id = ? AND type_id = ? AND destroyed_at IS NULL AND release_identity=?")
                .bind(session_id)
                .bind(type_id)
                .bind(self.approved_release.storage_key())
                .fetch_optional(&mut *transaction)
                .await?;
            let Some((creator_id, _attributes)) = session else {
                transaction.rollback().await?;
                return Ok::<(Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>), sqlx::Error>((Vec::new(), Vec::new(), Vec::new(), Vec::new()));
            };

            // Only the current owner confirms P2P admission. Guest reports are
            // acknowledged without mutation; JoinSession remains the guest path.
            // Check after acquiring the writer so migration cannot race admission.
            if caller_id != creator_id {
                transaction.rollback().await?;
                info!(self.logger, "ParticipantsDeferredUntilOwner caller_id={caller_id} creator_id={creator_id} type_id={type_id} session_id={session_id}");
                return Ok((Vec::new(), Vec::new(), Vec::new(), Vec::new()));
            }

            let mut newly_admitted = Vec::new();
            for user_id in public_participants {
                let (already_joined,): (i64,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM participants WHERE game_id = ? AND user_id = ?)")
                    .bind(session_id)
                    .bind(user_id)
                    .fetch_one(&mut *transaction)
                    .await?;
                sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                    .bind(session_id)
                    .bind(user_id)
                    .execute(&mut *transaction)
                    .await?;
                if already_joined == 0 {
                    newly_admitted.push(user_id);
                }
            }

            let mut deferred_private = Vec::new();
            let mut host_admitted_private = Vec::new();
            let mut persisted_private = Vec::new();
            for user_id in private_participants {
                let pending_invite_sender = sqlx::query_as::<_, (u32,)>(
                    r"
                    SELECT sender FROM invites
                    WHERE receiver = ? AND kind IN (?, ?) AND session_type = ? AND session_id = ?
                      AND accepted_at IS NOT NULL AND consumed_at IS NULL AND expires_at > CURRENT_TIMESTAMP
                    ORDER BY id DESC LIMIT 1
                    ",
                )
                .bind(user_id)
                .bind(INVITE_KIND_PRIVATE_ROOM)
                .bind(INVITE_KIND_PARTY_FOLLOW)
                .bind(type_id)
                .bind(session_id)
                .fetch_optional(&mut *transaction)
                .await?;
                let (already_joined,): (i64,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM participants WHERE game_id = ? AND user_id = ?)")
                    .bind(session_id)
                    .bind(user_id)
                    .fetch_one(&mut *transaction)
                    .await?;

                if let Some((_invite_sender,)) = pending_invite_sender {
                    if already_joined == 0 {
                        // The room owner is the authoritative P2P admission
                        // source. The accepted invitation may have been sent by
                        // any current room/Party member, so requiring that the
                        // invitation sender is also the owner strands legitimate
                        // non-host invitees outside formal membership. They then
                        // miss lobby restore and host-migration notifications.
                        if caller_id == creator_id {
                            sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                                .bind(session_id)
                                .bind(user_id)
                                .execute(&mut *transaction)
                                .await?;
                            host_admitted_private.push(user_id);
                            newly_admitted.push(user_id);
                        } else {
                            deferred_private.push(user_id);
                        }
                        continue;
                    }
                }
                sqlx::query("INSERT OR IGNORE INTO participants (game_id, user_id) VALUES (?, ?)")
                    .bind(session_id)
                    .bind(user_id)
                    .execute(&mut *transaction)
                    .await?;
                persisted_private.push(user_id);
                if already_joined == 0 {
                    newly_admitted.push(user_id);
                }
            }
            Self::reconcile_game_session_async(&mut transaction, type_id, session_id).await?;
            for user_id in &newly_admitted {
                let invite_kind = self.attach_accepted_invite_party_async(&mut transaction, *user_id, type_id, session_id).await?;
                Self::consume_all_invites_for_receiver_async(&mut transaction, *user_id).await?;
                if invite_kind == Some(INVITE_KIND_LOBBY_RESTORE) {
                    self.note_lobby_restore_split_transition(type_id, session_id, *user_id);
                }
            }
            transaction.commit().await?;
            Ok::<(Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>), sqlx::Error>((deferred_private, host_admitted_private, persisted_private, newly_admitted))
        })??;

        deferred_private.sort_unstable();
        host_admitted_private.sort_unstable();
        persisted_private.sort_unstable();
        newly_admitted.sort_unstable();
        if !deferred_private.is_empty() {
            info!(
                self.logger,
                "PrivateParticipantsDeferredUntilJoin type_id={type_id} session_id={session_id} participant_ids={deferred_private:?} invitation_kinds=[1,3] formal_membership=JoinSession"
            );
        }
        if !host_admitted_private.is_empty() {
            info!(
                self.logger,
                "PrivateInviteOwnerAdmissionPersisted caller_id={caller_id} session_creator_id={caller_id} type_id={type_id} session_id={session_id} participant_ids={host_admitted_private:?} membership_source=owner-AddParticipants-after-P2P invitation_kinds=[1,3]"
            );
        }
        if !persisted_private.is_empty() {
            info!(
                self.logger,
                "PrivateParticipantsPersisted type_id={type_id} session_id={session_id} participant_ids={persisted_private:?} accepted_invite_pending=false"
            );
        }
        if !newly_admitted.is_empty() {
            info!(
                self.logger,
                "InviteLifecycleClearedOnAdmission type_id={type_id} session_id={session_id} participant_ids={newly_admitted:?} scope=all-receiver-invites"
            );
        }
        Ok(())
    }

    /// Handles the owner leaving a session. A populated session must remain
    /// alive so the retail client can run its native host-migration sequence;
    /// destroying it here makes every follower enter `StateError` before
    /// `SplitSession` can elect the real replacement host.
    pub fn delete_game_session(&self, creator_id: u32, type_id: u32, session_id: u32) -> Result<DeleteGameSessionOutcome> {
        self.delete_game_session_checked(creator_id, type_id, session_id, || true)
    }

    fn delete_game_session_checked(&self, creator_id: u32, type_id: u32, session_id: u32, may_proceed: impl Fn() -> bool) -> Result<DeleteGameSessionOutcome> {
        if !may_proceed() { return Ok(DeleteGameSessionOutcome::NotFound); }
        let recent_departures = self.recent_game_room_departures(type_id, session_id);
        let published_session_id = self.published_game_session(creator_id);
        let (outcome, party_handoff, party_return) = run(async {
            let mut transaction = self.begin_write().await?;
            // Recheck after writer admission, not before a potentially long wait.
            if !may_proceed() {
                transaction.rollback().await?;
                return Ok((DeleteGameSessionOutcome::NotFound, None, None));
            }
            let owned = sqlx::query_as::<_, (String,)>("SELECT attributes FROM game_sessions WHERE creator_id = ? AND type_id = ? AND id = ? AND destroyed_at IS NULL")
                .bind(creator_id)
                .bind(type_id)
                .bind(session_id)
                .fetch_optional(&mut *transaction)
                .await?;
            if owned.is_none() {
                transaction.rollback().await?;
                return Ok::<(DeleteGameSessionOutcome, Option<(u32, u32, u64, u64)>, Option<(u32, u64, u64, u64)>), sqlx::Error>((DeleteGameSessionOutcome::NotFound, None, None));
            }
            let attributes = owned.expect("checked above").0;

            let remaining_participants = sqlx::query_as::<_, (u32,)>("SELECT user_id FROM participants WHERE game_id = ? AND user_id != ? ORDER BY user_id")
                .bind(session_id)
                .bind(creator_id)
                .fetch_all(&mut *transaction)
                .await?;
            let mut remaining_participant_ids = remaining_participants.iter().map(|(user_id,)| *user_id).collect::<Vec<_>>();
            if is_private_room_attributes(&attributes) && !recent_departures.is_empty() {
                // A follower removes its Party row as soon as it leaves an
                // otherwise live private match. The in-memory marker is
                // therefore the authoritative proof for the narrow native
                // unwind in which followers leave immediately before the
                // owner. Older voluntary departures have already expired.
                remaining_participant_ids.extend(recent_departures.iter().copied());
                remaining_participant_ids.sort_unstable();
                remaining_participant_ids.dedup();
            }
            let party_return = if is_private_room_attributes(&attributes) {
                if let Some(published_lobby_session_id) = published_session_id {
                    Self::restore_published_lobby_party_async(&mut transaction, creator_id, session_id, published_lobby_session_id)
                        .await?
                        .map(|(member_count, restore_invites, cancelled_personal_restores)| {
                            (published_lobby_session_id, member_count, restore_invites, cancelled_personal_restores)
                        })
                } else {
                    None
                }
            } else {
                None
            };
            let party_handoff = if is_private_room_attributes(&attributes) && party_return.is_none() {
                Self::handoff_private_room_party_async(&mut transaction, creator_id, type_id, session_id, &remaining_participant_ids).await?
            } else {
                None
            };

            if remaining_participants.is_empty() {
                sqlx::query(
                    "UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP, host_leave_pending=0 WHERE creator_id = ? AND type_id = ? AND id = ? AND destroyed_at IS NULL",
                )
                .bind(creator_id)
                .bind(type_id)
                .bind(session_id)
                .execute(&mut *transaction)
                .await?;
                transaction.commit().await?;
                return Ok((DeleteGameSessionOutcome::Destroyed, party_handoff, party_return));
            }

            // Keep the exact membership and owner stable until the native
            // election names its successor through SplitSession.  Removing or
            // transferring here races the game's election state machine.
            sqlx::query("UPDATE game_sessions SET host_leave_pending = 1 WHERE id = ? AND type_id = ? AND creator_id = ? AND destroyed_at IS NULL")
                .bind(session_id)
                .bind(type_id)
                .bind(creator_id)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            #[allow(clippy::cast_possible_truncation)]
            Ok((
                DeleteGameSessionOutcome::MigrationPending {
                    remaining_participants: remaining_participants.len() as u32,
                },
                party_handoff,
                party_return,
            ))
        })??;
        // A failed ownership/liveness lookup is an idempotent no-op, including
        // the short-lived markers used by the real owner's Party restoration.
        if outcome != DeleteGameSessionOutcome::NotFound {
            self.clear_game_room_departures(type_id, session_id);
        }
        if let Some((new_leader_id, lobby_session_id, member_count, restore_invites)) = party_handoff {
            info!(
                self.logger,
                "PrivateRoomPartyHandedOff old_leader_id={creator_id} new_leader_id={new_leader_id} type_id={type_id} room_session_id={session_id} lobby_session_id={lobby_session_id} member_count={member_count} restore_invites={restore_invites} delivery_gate=left-old-room"
            );
        }
        if let Some((lobby_session_id, member_count, restore_invites, cancelled_personal_restores)) = party_return {
            info!(
                self.logger,
                "PrivateRoomPartyReturnedWithHost host_id={creator_id} type_id={type_id} room_session_id={session_id} lobby_session_id={lobby_session_id} member_count={member_count} late_member_restore_invites={restore_invites} cancelled_personal_restores={cancelled_personal_restores} source=published-multiplayer-lobby"
            );
        }
        Ok(outcome)
    }

    /// Returns active sessions the user may invite another player into. Direct
    /// Quazal membership is authoritative. Persistent Party
    /// membership is also accepted because a room transition can briefly
    /// publish the host's new private session before every follower has been
    /// recorded in `participants`. Party membership is matched to the current
    /// session creator, rather than the original party leader, so a legitimate
    /// host migration does not strand the remaining members.
    pub async fn find_invitable_session_candidates_async(&self, user_id: u32) -> Result<Vec<InvitableGameSession>> {
        let mut candidates = sqlx::query_as::<_, (u32, u32, u32, String, i64, i64)>(
            r"
            SELECT DISTINCT
                g.type_id AS session_type,
                g.id AS session_id,
                g.creator_id,
                g.attributes,
                (SELECT COUNT(*) FROM participants AS member WHERE member.game_id = g.id) AS participant_count,
                EXISTS (
                    SELECT 1 FROM participants AS direct_member
                    WHERE direct_member.game_id = g.id
                      AND direct_member.user_id = ?
                ) AS direct_member
            FROM game_sessions AS g
            INNER JOIN participants AS host
                ON host.game_id = g.id AND host.user_id = g.creator_id
            WHERE g.destroyed_at IS NULL
              AND EXISTS (SELECT 1 FROM station_urls AS s WHERE s.user_id = g.creator_id)
              AND (
                  EXISTS (
                      SELECT 1 FROM participants AS direct_member
                      WHERE direct_member.game_id = g.id
                        AND direct_member.user_id = ?
                  )
                  OR EXISTS (
                      SELECT 1
                      FROM party_members AS party_member
                      INNER JOIN party_members AS host_party_member
                          ON host_party_member.party_id = party_member.party_id
                      WHERE party_member.user_id = ?
                        AND host_party_member.user_id = g.creator_id
                  )
              )
            ORDER BY g.id DESC
            ",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        // Private rooms remain first, including the transition window before
        // every Party follower is recorded there. Among lobbies, a shared
        // migrated Party outranks a newer preserved solo fallback. The solo
        // lobby remains available for native recovery, but it must not replace
        // the N-1 Party as the target of a later invitation.
        candidates.sort_by_key(|(_, session_id, _, attributes, participant_count, _)| {
            let class = if is_game_room_attributes(attributes) {
                0
            } else if *participant_count > 1 {
                1
            } else {
                2
            };
            (class, std::cmp::Reverse(*session_id))
        });

        Ok(candidates
            .into_iter()
            .map(|(session_type, session_id, creator_id, attributes, _, direct_member)| InvitableGameSession {
                session: GameSession {
                    session_type,
                    session_id,
                    creator_id,
                    attributes,
                    participants: Vec::new(),
                },
                direct_member: direct_member != 0,
            })
            .collect())
    }

    #[cfg(test)]
    pub async fn find_invitable_sessions_async(&self, user_id: u32) -> Result<Vec<GameSession>> {
        Ok(self
            .find_invitable_session_candidates_async(user_id)
            .await?
            .into_iter()
            .map(|candidate| candidate.session)
            .collect())
    }

    #[cfg(test)]
    pub async fn find_active_session_creator_async(&self, session_type: u32, session_id: u32) -> Result<Option<u32>> {
        Ok(self
            .find_active_session_invite_target_async(session_type, session_id)
            .await?
            .map(|(creator_id, _)| creator_id))
    }

    pub async fn find_active_session_invite_target_async(&self, session_type: u32, session_id: u32) -> Result<Option<(u32, String)>> {
        Ok(sqlx::query_as(
            r"
            SELECT g.creator_id, g.attributes
            FROM game_sessions AS g
            INNER JOIN participants AS host
                ON host.game_id = g.id AND host.user_id = g.creator_id
            WHERE g.type_id = ?
              AND g.id = ?
              AND g.destroyed_at IS NULL
              AND EXISTS (SELECT 1 FROM station_urls AS endpoint WHERE endpoint.user_id = g.creator_id)
            ",
        )
        .bind(session_type)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?)
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct User {
    pub id: u32,
    pub username: String,
    pub ubi_id: String,
    pub is_online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AuthIdentity {
    pub user_id: u32,
    pub username: String,
    pub account_id: String,
    pub auth_version: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct GameSession {
    pub session_type: u32,
    pub session_id: u32,
    pub creator_id: u32,
    pub attributes: String,
    #[sqlx(skip)]
    pub participants: Vec<Participant>,
}

#[derive(Debug)]
pub struct InvitableGameSession {
    pub session: GameSession,
    pub direct_member: bool,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Participant {
    pub user_id: u32,
    pub name: String,
    #[sqlx(skip)]
    pub station_urls: Vec<String>,
}

#[derive(Debug)]
pub struct InvitedGameSession {
    pub invite_id: i64,
    pub kind: i32,
    pub session: GameSession,
}

#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub struct Invite {
    pub id: i64,
    pub sender: u32,
    pub receiver: u32,
    pub kind: i32,
    pub session_type: u32,
    pub session_id: u32,
    pub delivery_count: i64,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod release_test_support;

#[cfg(test)]
mod native_release_tests;

#[cfg(test)]
mod native_release_migration_review_tests;

#[cfg(test)]
mod native_release_edge_review_tests;

#[cfg(test)]
impl Storage {
    pub(crate) fn set_login_checkpoint(&self, stage: &'static str, callback: Box<dyn FnOnce() + Send>) {
        let mut slot = self.login_checkpoint.lock().unwrap();
        assert!(slot.is_none(), "only one owned login checkpoint at a time");
        *slot = Some((stage, callback));
    }
    pub(crate) fn run_login_checkpoint(&self, stage: &'static str) {
        let callback = {
            let mut slot = self.login_checkpoint.lock().unwrap();
            if slot.as_ref().map(|entry| entry.0) == Some(stage) {
                slot.take().map(|entry| entry.1)
            } else { None }
        };
        if let Some(callback) = callback { callback(); }
    }
}
