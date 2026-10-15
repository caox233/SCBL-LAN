//! Mode binding is independent of display names, account UI and mutable host choice.
use super::{run, Result, Storage};
use eyre::eyre;
use server_api::release_contract::ReleaseIdentity;
use std::net::IpAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeRun {
    pub id: i64,
    pub user_id: u32,
    pub mode: i64,
}

impl Storage {
    /// Caller holds `mode_gate.write()`. Credential verification happens before this.
    /// Same-run renewals preserve ticket/room identity; retired instances cannot
    /// become current again when a delayed old process retries Login.
    pub(crate) async fn begin_game_run_with_release(
        &self,
        user: u32,
        instance: &str,
        mode: i64,
        source: IpAddr,
        release: &ReleaseIdentity,
    ) -> Result<()> {
        if release != &self.approved_release {
            return Err(eyre!("PACKAGE_CATALOG_MISMATCH: update the complete package"));
        }
        let mut tx = self.begin_write().await?;
        let previous:Option<(i64,String,i64,String,Option<String>)>=sqlx::query_as("SELECT id,instance,mode,source_ip,release_identity FROM game_runs WHERE user_id=? AND retired_at IS NULL").bind(user).fetch_optional(&mut *tx).await?;
        if let Some((_, old_instance, old_mode, old_source, old_release)) = &previous {
            if old_instance == instance {
                if *old_mode != mode {
                    return Err(eyre!("Game instance cannot change gameplay mode"));
                }
                if !release.matches_stored_run(old_release.as_deref()) {
                    return Err(eyre!("Game instance cannot change package identity; restart this game"));
                }
                if old_source != &source.to_string() {
                    sqlx::query("DELETE FROM user_sessions WHERE user_id=?")
                        .bind(user)
                        .execute(&mut *tx)
                        .await?;
                }
                sqlx::query("UPDATE game_runs SET source_ip=?,seen_at=CURRENT_TIMESTAMP WHERE user_id=? AND retired_at IS NULL").bind(source.to_string()).bind(user).execute(&mut *tx).await?;
                tx.commit().await?;
                return Ok(());
            }
        }
        let retired: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM game_runs WHERE user_id=? AND instance=?)",
        )
        .bind(user)
        .bind(instance)
        .fetch_one(&mut *tx)
        .await?;
        if retired {
            return Err(eyre!(
                "Game instance has been superseded; restart this game"
            ));
        }
        if previous.is_some() {
            // Uses existing native migration cleanup, not an invented host election.
            Self::cleanup_user_game_state_async(&mut tx, user).await?;
            sqlx::query("UPDATE game_runs SET retired_at=CURRENT_TIMESTAMP WHERE user_id=? AND retired_at IS NULL").bind(user).execute(&mut *tx).await?;
        }
        // Old tokens cannot last longer than seven days. Keep their nonce tombstones
        // for eight days so a retired token/login cannot resurrect the previous run.
        sqlx::query(
            "DELETE FROM game_runs WHERE user_id=? AND retired_at<datetime('now','-8 days')",
        )
        .bind(user)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip,release_identity) VALUES(?,?,?,?,?)")
            .bind(user)
            .bind(instance)
            .bind(mode)
            .bind(source.to_string())
            .bind(release.storage_key())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub(crate) async fn touch_game_run_with_release(
        &self,
        user: u32,
        instance: &str,
        mode: i64,
        source: IpAddr,
        release: &ReleaseIdentity,
    ) -> Result<bool> {
        if release != &self.approved_release { return Ok(false); }
        Ok(sqlx::query("UPDATE game_runs SET seen_at=CURRENT_TIMESTAMP WHERE user_id=? AND instance=? AND mode=? AND source_ip=? AND release_identity=? AND retired_at IS NULL")
            .bind(user).bind(instance).bind(mode).bind(source.to_string()).bind(release.storage_key()).execute(&self.pool).await?.rows_affected()==1)
    }
    /// Revalidate the interceptor identity under the generation lock without
    /// taking a `SQLite` write lock ahead of the publication/acceptance lock order.
    /// The authenticated interceptor already refreshes the lease.
    pub(crate) async fn game_run_is_current(
        &self,
        user: u32,
        instance: &str,
        mode: i64,
        source: IpAddr,
        release: &ReleaseIdentity,
    ) -> Result<bool> {
        if release != &self.approved_release { return Ok(false); }
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM game_runs WHERE user_id=? AND instance=? AND mode=? AND source_ip=? AND release_identity=? AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds'))")
            .bind(user).bind(instance).bind(mode).bind(source.to_string()).bind(release.storage_key()).fetch_one(&self.pool).await?)
    }
    pub(crate) async fn current_game_mode(&self, user: u32) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar("SELECT mode FROM game_runs WHERE user_id=? AND release_identity=? AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds')").bind(user).bind(self.approved_release.storage_key()).fetch_optional(&self.pool).await?)
    }
    /// Pin each issued Kerberos key to the authenticated run, never the latest
    /// username at the later moment when the secure connection happens to arrive.
    pub(crate) fn create_bound_user_session(
        &self,
        user: u32,
        key: &[u8],
        source: IpAddr,
    ) -> Result<()> {
        let mut key_hex = sodiumoxide::hex::encode(key);
        key_hex.make_ascii_uppercase();
        let hash = quazal::kerberos::ticket_fingerprint(key);
        let hash_hex = sodiumoxide::hex::encode(hash);
        run(async {
            let mut tx = self.begin_write().await?;
            let run_id:i64=sqlx::query_scalar("SELECT id FROM game_runs WHERE user_id=? AND source_ip=? AND release_identity=? AND retired_at IS NULL AND seen_at>=datetime('now','-90 seconds')").bind(user).bind(source.to_string()).bind(self.approved_release.storage_key()).fetch_optional(&mut *tx).await?.ok_or_else(|| eyre!("PACKAGE_IDENTITY_REQUIRED: approved game run required"))?;
            sqlx::query("INSERT INTO user_sessions(id,user_id,game_run_id,ticket_hash,native_connected) VALUES(?,?,?,?,0)").bind(key_hex).bind(user).bind(run_id).bind(hash_hex).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok::<(), eyre::Error>(())
        })??;
        Ok(())
    }
    pub(crate) fn mark_secure_ticket(
        &self,
        user: u32,
        fingerprint: Option<[u8; 32]>,
    ) -> Result<bool> {
        let Some(hash) = fingerprint else {
            return Ok(false);
        };
        let hash = sodiumoxide::hex::encode(hash);
        run(async {
            let mut tx = self.begin_write().await?;
            let count = sqlx::query(
                "UPDATE user_sessions SET native_connected=1 WHERE user_id=? AND ticket_hash=? AND EXISTS(SELECT 1 FROM game_runs r WHERE r.id=user_sessions.game_run_id AND r.user_id=user_sessions.user_id AND r.release_identity=? AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds'))",
            )
            .bind(user)
            .bind(hash)
            .bind(self.approved_release.storage_key())
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if count == 1 {
                sqlx::query("UPDATE users SET is_online=1 WHERE id=?")
                    .bind(user)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            Ok(count == 1)
        })?
    }
    pub(crate) fn native_game_run(
        &self,
        user: u32,
        fingerprint: Option<[u8; 32]>,
        source: IpAddr,
    ) -> Result<Option<NativeRun>> {
        let Some(hash) = fingerprint else {
            return Ok(None);
        };
        let hash = sodiumoxide::hex::encode(hash);
        let value:Option<(i64,i64)>=run(sqlx::query_as("SELECT r.id,r.mode FROM user_sessions t JOIN game_runs r ON r.id=t.game_run_id WHERE t.user_id=? AND t.ticket_hash=? AND t.native_connected=1 AND r.user_id=? AND r.retired_at IS NULL AND r.source_ip=? AND r.release_identity=? AND r.seen_at>=datetime('now','-90 seconds')").bind(user).bind(hash).bind(user).bind(source.to_string()).bind(self.approved_release.storage_key()).fetch_optional(&self.pool))??;
        Ok(value.map(|(id, mode)| NativeRun {
            id,
            user_id: user,
            mode,
        }))
    }
    pub(crate) async fn room_mode(&self, type_id: u32, session_id: u32) -> Result<Option<i64>> {
        // Destroyed rows are retained for the existing SplitSession restoration path.
        Ok(sqlx::query_scalar("SELECT gameplay_mode FROM game_sessions WHERE type_id=? AND id=? AND gameplay_mode IN(1,2) AND release_identity=?").bind(type_id).bind(session_id).bind(self.approved_release.storage_key()).fetch_optional(&self.pool).await?)
    }
    pub(crate) fn room_matches_mode(
        &self,
        type_id: u32,
        session_id: u32,
        mode: i64,
    ) -> Result<bool> {
        Ok(run(self.room_mode(type_id, session_id))?? == Some(mode))
    }
    pub(crate) async fn invite_matches_mode(
        &self,
        receiver: u32,
        invite_id: i64,
        mode: i64,
    ) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM invites i JOIN game_sessions g ON g.id=i.session_id AND g.type_id=i.session_type WHERE i.id=? AND i.receiver=? AND g.gameplay_mode=? AND g.destroyed_at IS NULL AND g.release_identity=? AND EXISTS(SELECT 1 FROM game_runs r WHERE r.user_id=i.receiver AND r.mode=g.gameplay_mode AND r.release_identity=g.release_identity AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds')) AND EXISTS(SELECT 1 FROM game_runs r WHERE r.user_id=i.sender AND r.mode=g.gameplay_mode AND r.release_identity=g.release_identity AND r.retired_at IS NULL AND r.seen_at>=datetime('now','-90 seconds')))").bind(invite_id).bind(receiver).bind(mode).bind(self.approved_release.storage_key()).fetch_one(&self.pool).await?)
    }
}

#[cfg(test)]
impl Storage {
    pub(crate) async fn begin_game_run(&self,user:u32,instance:&str,mode:i64,source:IpAddr)->Result<()> {
        self.begin_game_run_with_release(user,instance,mode,source,&crate::api::test_release_identity()).await
    }
    pub(crate) async fn touch_game_run(&self,user:u32,instance:&str,mode:i64,source:IpAddr)->Result<bool> {
        self.touch_game_run_with_release(user,instance,mode,source,&crate::api::test_release_identity()).await
    }
}
