use super::{device::Device, group::Group, SecurityKey, WalletInfo};
use crate::{
    auth::TOTP_CODE_VALIDITY_PERIOD,
    db::{Wallet, WebAuthn},
    DbPool,
};
use argon2::{
    password_hash::{
        errors::Error as HashError, rand_core::OsRng, PasswordHash, PasswordHasher,
        PasswordVerifier, SaltString,
    },
    Argon2,
};
use model_derive::Model;
use otpauth::TOTP;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use sqlx::{query, query_as, query_scalar, Error as SqlxError, Type};
use std::time::SystemTime;

const RECOVERY_CODES_COUNT: usize = 8;

#[derive(Deserialize, Serialize, Type)]
#[sqlx(type_name = "mfa_method", rename_all = "snake_case")]
pub enum MFAMethod {
    None,
    OneTimePassword,
    WebAuthn,
    Web3,
}

#[derive(Model)]
pub struct User {
    pub id: Option<i64>,
    pub username: String,
    password_hash: String,
    pub last_name: String,
    pub first_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub ssh_key: Option<String>,
    pub pgp_key: Option<String>,
    pub pgp_cert_id: Option<String>,
    // secret has been verified and TOTP can be used
    pub totp_enabled: bool,
    totp_secret: Option<Vec<u8>>,
    #[model(enum)]
    pub mfa_method: MFAMethod,
    #[model(ref)]
    recovery_codes: Vec<String>,
}

impl User {
    fn hash_password(password: &str) -> Result<String, HashError> {
        let salt = SaltString::generate(&mut OsRng);
        Ok(Argon2::default()
            .hash_password(password.as_bytes(), &salt)?
            .to_string())
    }

    #[must_use]
    pub fn new(
        username: String,
        password: &str,
        last_name: String,
        first_name: String,
        email: String,
        phone: Option<String>,
    ) -> Self {
        Self {
            id: None,
            username,
            password_hash: Self::hash_password(password).unwrap(),
            last_name,
            first_name,
            email,
            phone,
            ssh_key: None,
            pgp_key: None,
            pgp_cert_id: None,
            totp_enabled: false,
            totp_secret: None,
            mfa_method: MFAMethod::None,
            recovery_codes: Vec::new(),
        }
    }

    pub fn set_password(&mut self, password: &str) {
        self.password_hash = Self::hash_password(password).unwrap();
    }

    pub fn verify_password(&self, password: &str) -> Result<(), HashError> {
        let parsed_hash = PasswordHash::new(&self.password_hash)?;
        Argon2::default().verify_password(password.as_bytes(), &parsed_hash)
    }

    /// Generate new `secret`, save it, then return it as RFC 4648 base32-encoded string.
    pub async fn new_secret(&mut self, pool: &DbPool) -> Result<String, SqlxError> {
        let secret = thread_rng().gen::<[u8; 20]>().to_vec();
        if let Some(id) = self.id {
            query!(
                "UPDATE \"user\" SET totp_secret = $1 WHERE id = $2",
                secret,
                id
            )
            .execute(pool)
            .await?;
        }
        let secret_base32 = TOTP::from_bytes(&secret).base32_secret();
        self.totp_secret = Some(secret);
        Ok(secret_base32)
    }

    /// Check if any of the multi-factor authentication methods is on.
    /// - TOTP is enabled
    /// - a [`Wallet`] flagged `use_for_mfa`
    /// - a security key for Webauthn
    pub async fn mfa_enabled(&self, pool: &DbPool) -> Result<bool, SqlxError> {
        // short-cut
        if self.totp_enabled {
            return Ok(true);
        }

        if let Some(id) = self.id {
            query_scalar!(
                "SELECT totp_enabled OR coalesce(bool_or(wallet.use_for_mfa), FALSE) \
                OR count(webauthn.id) > 0 \"bool!\" FROM \"user\" \
                LEFT JOIN wallet ON wallet.user_id = \"user\".id \
                LEFT JOIN webauthn ON webauthn.user_id = \"user\".id \
                WHERE \"user\".id = $1 GROUP BY totp_enabled;",
                id
            )
            .fetch_one(pool)
            .await
        } else {
            Ok(false)
        }
    }

    /// Enable MFA; generate new recovery codes.
    pub async fn enable_mfa(&mut self, pool: &DbPool) -> Result<Option<Vec<String>>, SqlxError> {
        if self.mfa_enabled(pool).await? {
            return Ok(None);
        }

        self.recovery_codes.clear();
        for _ in 0..RECOVERY_CODES_COUNT {
            let code = thread_rng()
                .sample_iter(Alphanumeric)
                .take(16)
                .map(char::from)
                .collect();
            self.recovery_codes.push(code);
        }
        if let Some(id) = self.id {
            query!(
                "UPDATE \"user\" SET recovery_codes = $2 WHERE id = $1",
                id,
                &self.recovery_codes
            )
            .execute(pool)
            .await?;
        }

        Ok(Some(self.recovery_codes.clone()))
    }

    /// Disable MFA; discard recovery codes, TOTP secret, and security keys.
    pub async fn disable_mfa(&mut self, pool: &DbPool) -> Result<(), SqlxError> {
        if let Some(id) = self.id {
            query!(
                "UPDATE \"user\" SET totp_secret = NULL, recovery_codes = '{}' \
                WHERE id = $1",
                id
            )
            .execute(pool)
            .await?;
            Wallet::disable_mfa_for_user(pool, id).await?;
            WebAuthn::delete_all_for_user(pool, id).await?;
        }
        self.totp_secret = None;
        self.recovery_codes.clear();
        Ok(())
    }

    /// Enable TOTP
    pub async fn enable_totp(&mut self, pool: &DbPool) -> Result<(), SqlxError> {
        if !self.totp_enabled {
            if let Some(id) = self.id {
                query!("UPDATE \"user\" SET totp_enabled = TRUE WHERE id = $1", id)
                    .execute(pool)
                    .await?;
            }
            self.totp_enabled = false;
        }
        Ok(())
    }

    /// Disable TOTP; discard the secret.
    pub async fn disable_totp(&mut self, pool: &DbPool) -> Result<(), SqlxError> {
        if self.totp_enabled {
            if let Some(id) = self.id {
                query!(
                    "UPDATE \"user\" SET totp_enabled = FALSE AND totp_secret = NULL WHERE id = $1",
                    id
                )
                .execute(pool)
                .await?;
                WebAuthn::delete_all_for_user(pool, id).await?;
            }
            self.totp_enabled = false;
            self.totp_secret = None;
        }
        Ok(())
    }

    /// Check if TOTP `code` is valid.
    pub fn verify_code(&self, code: u32) -> bool {
        if let Some(totp_secret) = &self.totp_secret {
            let totp = TOTP::from_bytes(totp_secret);
            if let Ok(timestamp) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
                return totp.verify(code, TOTP_CODE_VALIDITY_PERIOD, timestamp.as_secs());
            }
        }
        false
    }

    /// Verify recovery code. If it is valid, consume it, so it can't be used again.
    pub async fn verify_recovery_code(
        &mut self,
        pool: &DbPool,
        code: &str,
    ) -> Result<bool, SqlxError> {
        if let Some(index) = self.recovery_codes.iter().position(|c| c == code) {
            // Note: swap_remove() should be faster than remove().
            self.recovery_codes.swap_remove(index);
            if let Some(id) = self.id {
                query!(
                    "UPDATE \"user\" SET recovery_codes = $2 WHERE id = $1",
                    id,
                    &self.recovery_codes
                )
                .execute(pool)
                .await?;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn find_by_username(
        pool: &DbPool,
        username: &str,
    ) -> Result<Option<Self>, SqlxError> {
        query_as!(
            Self,
            "SELECT id \"id?\", username, password_hash, last_name, first_name, email, \
            phone, ssh_key, pgp_key, pgp_cert_id, totp_enabled, totp_secret, \
            mfa_method \"mfa_method: _\", recovery_codes \
            FROM \"user\" WHERE username = $1",
            username
        )
        .fetch_optional(pool)
        .await
    }

    pub async fn member_of(&self, pool: &DbPool) -> Result<Vec<String>, SqlxError> {
        if let Some(id) = self.id {
            query_scalar!(
                "SELECT \"group\".name FROM \"group\" JOIN group_user ON \"group\".id = group_user.group_id \
                WHERE group_user.user_id = $1",
                id
            )
            .fetch_all(pool)
            .await
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn devices(&self, pool: &DbPool) -> Result<Vec<Device>, SqlxError> {
        if let Some(id) = self.id {
            query_as!(
                Device,
                "SELECT device.id \"id?\", name, wireguard_ip, wireguard_pubkey, user_id, created \
                FROM device WHERE user_id = $1",
                id
            )
            .fetch_all(pool)
            .await
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn wallets(&self, pool: &DbPool) -> Result<Vec<WalletInfo>, SqlxError> {
        if let Some(id) = self.id {
            query_as!(
                WalletInfo,
                "SELECT address \"address!\", name, chain_id, use_for_mfa \
                FROM wallet WHERE user_id = $1 AND validation_timestamp IS NOT NULL",
                id
            )
            .fetch_all(pool)
            .await
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn security_keys(&self, pool: &DbPool) -> Result<Vec<SecurityKey>, SqlxError> {
        if let Some(id) = self.id {
            query_as!(
                SecurityKey,
                "SELECT id \"id!\", name FROM webauthn WHERE user_id = $1",
                id
            )
            .fetch_all(pool)
            .await
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn add_to_group(&self, pool: &DbPool, group: &Group) -> Result<(), SqlxError> {
        if let (Some(id), Some(group_id)) = (self.id, group.id) {
            query!(
                "INSERT INTO group_user (group_id, user_id) VALUES ($1, $2) \
                ON CONFLICT DO NOTHING",
                group_id,
                id
            )
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    pub async fn remove_from_group(&self, pool: &DbPool, group: &Group) -> Result<(), SqlxError> {
        if let (Some(id), Some(group_id)) = (self.id, group.id) {
            query!(
                "DELETE FROM group_user WHERE group_id = $1 AND user_id = $2",
                group_id,
                id
            )
            .execute(pool)
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[sqlx::test]
    async fn test_user(pool: DbPool) {
        let mut user = User::new(
            "hpotter".into(),
            "pass123",
            "Potter".into(),
            "Harry".into(),
            "h.potter@hogwart.edu.uk".into(),
            None,
        );
        user.save(&pool).await.unwrap();

        let fetched_user = User::find_by_username(&pool, "hpotter").await.unwrap();
        assert!(fetched_user.is_some());
        assert_eq!(fetched_user.unwrap().email, "h.potter@hogwart.edu.uk");

        user.email = "harry.potter@hogwart.edu.uk".into();
        user.save(&pool).await.unwrap();

        let fetched_user = User::find_by_username(&pool, "hpotter").await.unwrap();
        assert!(fetched_user.is_some());
        assert_eq!(fetched_user.unwrap().email, "harry.potter@hogwart.edu.uk");

        assert!(user.verify_password("pass123").is_ok());

        let fetched_user = User::find_by_username(&pool, "rweasley").await.unwrap();
        assert!(fetched_user.is_none());
    }

    #[sqlx::test]
    async fn test_all_users(pool: DbPool) {
        let mut harry = User::new(
            "hpotter".into(),
            "pass123",
            "Potter".into(),
            "Harry".into(),
            "h.potter@hogwart.edu.uk".into(),
            None,
        );
        harry.save(&pool).await.unwrap();

        let mut albus = User::new(
            "adumbledore".into(),
            "magic!",
            "Dumbledore".into(),
            "Albus".into(),
            "a.dumbledore@hogwart.edu.uk".into(),
            None,
        );
        albus.save(&pool).await.unwrap();

        let users = User::all(&pool).await.unwrap();
        // Including "admin" user from migrations.
        assert_eq!(users.len(), 3);

        albus.delete(&pool).await.unwrap();

        let users = User::all(&pool).await.unwrap();
        assert_eq!(users.len(), 2);
    }

    #[sqlx::test]
    async fn test_recovery_codes(pool: DbPool) {
        let mut harry = User::new(
            "hpotter".into(),
            "pass123",
            "Potter".into(),
            "Harry".into(),
            "h.potter@hogwart.edu.uk".into(),
            None,
        );
        harry.enable_mfa(&pool).await.unwrap();
        assert_eq!(harry.recovery_codes.len(), RECOVERY_CODES_COUNT);
        harry.save(&pool).await.unwrap();

        let fetched_user = User::find_by_username(&pool, "hpotter").await.unwrap();
        assert!(fetched_user.is_some());

        let mut user = fetched_user.unwrap();
        assert_eq!(user.recovery_codes.len(), RECOVERY_CODES_COUNT);
        assert!(!user
            .verify_recovery_code(&pool, "invalid code")
            .await
            .unwrap());
        let codes = user.recovery_codes.clone();
        for code in &codes {
            assert!(user.verify_recovery_code(&pool, code).await.unwrap());
        }
        assert_eq!(user.recovery_codes.len(), 0);
    }
}