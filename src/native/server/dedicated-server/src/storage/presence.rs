#[cfg(test)]
use super::run;
use super::Result;
use super::Storage;
use super::User;

impl Storage {
    /// Refreshes the API-side presence heartbeat. The overlay polls the event
    /// service every second while the game is running, so this heartbeat keeps
    /// a player visible even when the secure PRUDP connection is temporarily idle.
    pub async fn touch_api_presence_async(&self, user_id: u32) -> Result<()> {
        sqlx::query("UPDATE users SET last_api_seen_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Unbound legacy sessions are fixtures only; runtime creates source-bound sessions.
    #[cfg(test)]
    pub fn create_user_session(&self, user_id: u32, key: &[u8]) -> Result<()> {
        use std::fmt::Write;
        let mut s = String::new();
        for c in key {
            write!(&mut s, "{c:02X}")?;
        }
        run(async {
            sqlx::query("INSERT INTO user_sessions (id, user_id) VALUES (?, ?)")
                .bind(s)
                .bind(user_id)
                .execute(&self.pool)
                .await?;

            sqlx::query("UPDATE users SET is_online=1 WHERE id=?").bind(user_id).execute(&self.pool).await
        })??;

        Ok(())
    }

    pub async fn list_users_async(&self) -> Result<Vec<User>> {
        self.cleanup_stale_api_sessions_async().await?;
        Ok(sqlx::query_as(
            r"
            SELECT
                id,
                username,
                ubi_id,
                CASE
                    WHEN is_online = 1
                      OR last_api_seen_at >= datetime('now', '-15 seconds')
                    THEN 1
                    ELSE 0
                END AS is_online
            FROM users
            WHERE ubi_id IS NOT NULL
            ",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Lists users for the in-game friend roster. Authentication alone is not
    /// gameplay presence: the Launcher uses the same login endpoint before the
    /// game has a secure session or a routable station URL.
    pub async fn list_game_ready_users_async(&self) -> Result<Vec<User>> {
        self.cleanup_stale_api_sessions_async().await?;
        Ok(sqlx::query_as(
            r"
            SELECT
                users.id,
                users.username,
                users.ubi_id,
                CASE
                    WHEN EXISTS (
                        SELECT 1
                        FROM user_sessions AS secure
                        WHERE secure.user_id = users.id AND secure.native_connected=1
                    )
                    OR (
                        users.last_api_seen_at >= datetime('now', '-15 seconds')
                        AND EXISTS (
                            SELECT 1
                            FROM station_urls AS station
                            WHERE station.user_id = users.id
                        )
                    )
                    THEN 1
                    ELSE 0
                END AS is_online
            FROM users
            WHERE users.ubi_id IS NOT NULL
            ",
        )
        .fetch_all(&self.pool)
        .await?)
    }
}
