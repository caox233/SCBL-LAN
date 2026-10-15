use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::Argon2;
use argon2::PasswordHash;
use argon2::PasswordHasher;
use argon2::PasswordVerifier;
use eyre::eyre;

use super::run;
use super::AuthIdentity;
use super::LoginError;
use super::Result;
use super::Storage;
use super::User;

// Retail Quazal reserves low principal IDs for infrastructure identities
// (for example authentication PID 2 and Tracking PID 105).  The historical
// 2.x database seeded player identities at 1000+, so keep fresh 3.x accounts
// in that proven namespace even after the demo rows have been removed.
const FIRST_PLAYER_PRINCIPAL_ID: u32 = 1000;
const SECURE_SERVER_PRINCIPAL_ID: u32 = 0x1000;

impl Storage {
    /// Identity and password material are read in the same SQLite snapshot.
    /// Successful password verification never endorses a later auth_version.
    pub(crate) async fn authenticate_user_async(&self, username: &str, password: &str) -> Result<std::result::Result<AuthIdentity, LoginError>> {
        let Some((id, stored_username, account_id, auth_version, db_password, password_hash)) = sqlx::query_as::<_, (u32, String, String, i64, Option<String>, Option<String>)>("SELECT id, username, COALESCE(ubi_id, CAST(id AS TEXT)), auth_version, password, password_hash FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?
        else {
            warn!(self.logger, "Authentication account not found");
            return Ok(Err(LoginError::NotFound));
        };

        #[cfg(test)]
        self.run_login_checkpoint("snapshot");
        let maybe_id = match (db_password, password_hash) {
            (None, None) => Err(eyre!("account credential state has no password material")),
            (Some(_), Some(_)) => Err(eyre!("account credential state has both password forms")),
            (Some(db_password), None) => {
                info!(self.logger, "Verifying legacy password");
                if db_password == password {
                    Ok(Ok(id))
                } else {
                    Ok(Err(LoginError::InvalidPassword))
                }
            }
            (None, Some(password_hash)) => {
                info!(self.logger, "Verifying password hash");
                let parsed_hash = PasswordHash::new(&password_hash).map_err(|_| eyre!("password hash parsing failed"))?;
                Ok(Argon2::default()
                    .verify_password(password.as_bytes(), &parsed_hash)
                    .map_err(|_| LoginError::InvalidPassword)
                    .and(Ok(id)))
            }
        }?;

        if let Ok(user_id) = maybe_id {
            #[cfg(test)]
            self.run_login_checkpoint("verified");
            // Authentication is shared by the Launcher and the game. It must
            // not publish gameplay presence before the game has established
            // its secure session and station endpoint.
            let accepted = sqlx::query("UPDATE users SET last_login = CURRENT_TIMESTAMP WHERE id = ? AND auth_version = ?")
                .bind(user_id)
                .bind(auth_version)
                .execute(&self.pool)
                .await?.rows_affected() == 1;
            if !accepted { return Ok(Err(LoginError::InvalidPassword)); }
        }

        Ok(maybe_id.map(|user_id| AuthIdentity { user_id, username: stored_username, account_id, auth_version }))
    }

    #[cfg(test)]
    pub async fn login_user_async(&self, username: &str, password: &str) -> Result<std::result::Result<u32, LoginError>> {
        Ok(self.authenticate_user_async(username, password).await?.map(|identity| identity.user_id))
    }

    pub(crate) fn authenticate_user(&self, username: &str, password: &str) -> Result<std::result::Result<AuthIdentity, LoginError>> {
        run(self.authenticate_user_async(username, password))?
    }

    /// Caller holds mode_gate across this check and run/ticket publication.
    pub(crate) async fn auth_identity_is_current_async(&self, identity: &AuthIdentity) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=? AND username=? AND auth_version=?)")
            .bind(identity.user_id).bind(&identity.username).bind(identity.auth_version)
            .fetch_one(&self.pool).await?)
    }

    pub(crate) fn auth_identity_is_current(&self, identity: &AuthIdentity) -> Result<bool> {
        run(self.auth_identity_is_current_async(identity))?
    }

    pub async fn register_user_with_recovery_async(&self, username: &str, password: &str, ubi_id: Option<&str>, recovery_code_hash: Option<&str>) -> Result<()> {
        let password_hash = Self::hash_password(password)?;
        Ok(self.register_user_unsafe_async(username, &password_hash, ubi_id, recovery_code_hash).await?)
    }

    fn hash_password(password: &str) -> Result<String> {
        let salt = SaltString::try_from_rng(&mut OsRng).map_err(|_| eyre!("password salt generation failed"))?;
        Argon2::default()
            .hash_password(password.as_bytes(), salt.as_salt())
            .map(|hash| hash.to_string())
            .map_err(|_| eyre!("password hashing failed"))
    }

    async fn register_user_unsafe_async(&self, username: &str, password: &str, ubi_id: Option<&str>, recovery_code_hash: Option<&str>) -> sqlx::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO users (id, username, password_hash, ubi_id, recovery_code_hash)
            SELECT
                CASE
                    WHEN COALESCE(MAX(id), 0) < ? THEN ?
                    WHEN MAX(id) + 1 = ? THEN ? + 1
                    ELSE MAX(id) + 1
                END,
                ?, ?, ?, ?
            FROM users
            "#,
        )
        .bind(FIRST_PLAYER_PRINCIPAL_ID)
        .bind(FIRST_PLAYER_PRINCIPAL_ID)
        .bind(SECURE_SERVER_PRINCIPAL_ID)
        .bind(SECURE_SERVER_PRINCIPAL_ID)
        .bind(username)
        .bind(password)
        .bind(ubi_id)
        .bind(recovery_code_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the password only when the supplied high-entropy recovery hash
    /// matches. The recovery code is rotated in the same database update.
    pub async fn reset_password_with_recovery_async(&self, username: &str, recovery_code_hash: &str, new_password: &str, replacement_recovery_code_hash: &str) -> Result<bool> {
        let stored = sqlx::query_as::<_, (Option<String>,)>("SELECT recovery_code_hash FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|row| row.0);

        if stored.as_deref() != Some(recovery_code_hash) {
            return Ok(false);
        }

        let password_hash = Self::hash_password(new_password)?;
        // Serialize version rotation with authenticated token/ticket publication.
        // Preserve the existing reset contract: do not clear native rooms or Party.
        let _generation = self.mode_gate.write().await;
        let result = sqlx::query(
            "UPDATE users SET password = NULL, password_hash = ?, recovery_code_hash = ?, auth_version = auth_version + 1, updated_at = CURRENT_TIMESTAMP WHERE username = ? AND recovery_code_hash = ?",
        )
        .bind(password_hash)
        .bind(replacement_recovery_code_hash)
        .bind(username)
        .bind(recovery_code_hash)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub fn find_password_for_user(&self, user_id: u32) -> Result<Option<String>> {
        let password = run(sqlx::query_as::<_, (Option<String>,)>("SELECT password FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool))??
        .and_then(|row| row.0);
        Ok(password)
    }

    pub async fn find_user_by_ubi_id_async(&self, ubi_id: &str) -> Result<Option<User>> {
        Ok(sqlx::query_as("SELECT id, username, ubi_id, is_online FROM users WHERE ubi_id = ?")
            .bind(ubi_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn find_user_by_id_async(&self, id: u32) -> Result<Option<User>> {
        Ok(sqlx::query_as("SELECT id, username, ubi_id, is_online FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn find_auth_identity_by_user_id_async(&self, user_id: u32) -> Result<Option<AuthIdentity>> {
        Ok(
            sqlx::query_as("SELECT id AS user_id, username, COALESCE(ubi_id, CAST(id AS TEXT)) AS account_id, auth_version FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub fn find_user_id_by_name(&self, username: &str) -> Result<Option<u32>> {
        let uid = run(sqlx::query_as::<_, (u32,)>("SELECT id FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool))??
        .map(|row| row.0);
        Ok(uid)
    }

    pub fn find_ubi_id_by_user_id(&self, user_id: u32) -> Result<Option<String>> {
        run(self.find_ubi_id_by_user_id_async(user_id))?
    }

    pub async fn find_ubi_id_by_user_id_async(&self, user_id: u32) -> Result<Option<String>> {
        let ubi_id = sqlx::query_as::<_, (Option<String>,)>("SELECT ubi_id FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|row| row.0);
        Ok(ubi_id)
    }

    pub fn find_user_id_by_ubi_id(&self, ubi_id: &str) -> Result<Option<u32>> {
        run(self.find_user_id_by_ubi_id_async(ubi_id))?
    }

    pub async fn find_user_id_by_ubi_id_async(&self, ubi_id: &str) -> Result<Option<u32>> {
        let uid = sqlx::query_as::<_, (u32,)>("SELECT id FROM users WHERE ubi_id = ?")
            .bind(ubi_id)
            .fetch_optional(&self.pool)
            .await?
            .map(|row| row.0);
        Ok(uid)
    }

    pub async fn delete_user_async(&self, user_id: u32) -> Result<()> {
        sqlx::query("DELETE FROM users WHERE id = ?").bind(user_id).execute(&self.pool).await?;
        Ok(())
    }
}
