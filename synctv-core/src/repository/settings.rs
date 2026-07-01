//! Settings repository for database operations

use sqlx::PgPool;
use tracing::debug;

use crate::{Error, Result};

use crate::models::settings::RuntimeSetting;

/// Settings repository
#[derive(Clone)]
pub struct SettingsRepository {
    pool: PgPool,
}

impl SettingsRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get all settings
    pub async fn get_all(&self) -> Result<Vec<RuntimeSetting>> {
        let groups = sqlx::query_as!(
            RuntimeSetting,
            r"
            SELECT key, group_name, value, version, created_at, updated_at
            FROM settings
            ORDER BY group_name
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        debug!("Retrieved {} settings", groups.len());
        Ok(groups)
    }

    /// Get a single setting by key
    pub async fn get(&self, key: &str) -> Result<RuntimeSetting> {
        self.get_optional(key)
            .await?
            .ok_or_else(|| Error::NotFound(format!("Setting not found: {key}")))
    }

    /// Get a single setting by key, returning `None` when absent.
    pub async fn get_optional(&self, key: &str) -> Result<Option<RuntimeSetting>> {
        let row = sqlx::query_as!(
            RuntimeSetting,
            r"
            SELECT key, group_name, value, version, created_at, updated_at
            FROM settings
            WHERE key = $1
            ",
            key,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    /// Read the current optimistic-lock version for a setting.
    pub async fn current_version(&self, key: &str) -> Result<i32> {
        let version = sqlx::query_scalar!("SELECT version FROM settings WHERE key = $1", key,)
            .fetch_optional(&self.pool)
            .await?;
        Ok(version.unwrap_or(0))
    }

    /// Insert or update a setting using the exact version reserved in the
    /// runtime-setting version fence.
    pub async fn upsert_with_exact_version(
        &self,
        key: &str,
        group_name: &str,
        value: &str,
        expected_version: i32,
        new_version: i32,
    ) -> Result<RuntimeSetting> {
        if new_version <= expected_version {
            return Err(Error::InvalidInput(format!(
                "new setting version {new_version} must be greater than expected version {expected_version}"
            )));
        }

        let row = sqlx::query_as!(
            RuntimeSetting,
            r"
            INSERT INTO settings (key, group_name, value, version)
            VALUES ($1, $2, $3, $5)
            ON CONFLICT (key) DO UPDATE
            SET group_name = EXCLUDED.group_name,
                value = EXCLUDED.value,
                version = EXCLUDED.version,
                updated_at = NOW()
            WHERE settings.version = $4
            RETURNING key, group_name, value, version, created_at, updated_at
            ",
            key,
            group_name,
            value,
            expected_version,
            new_version,
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            debug!(
                "Upserted setting '{}' with exact version ({} -> {})",
                key, expected_version, row.version
            );
            Ok(row)
        } else {
            debug!(
                "Optimistic lock conflict for exact-version setting upsert '{}' (expected {})",
                key, expected_version
            );
            Err(Error::OptimisticLockConflict)
        }
    }

    /// Update a setting value by key
    ///
    /// A database trigger on the `settings` table automatically sends a
    /// NOTIFY on the `settings_changed` channel so other replicas can
    /// reload the changed setting.
    pub async fn update(&self, key: &str, value: &str) -> Result<RuntimeSetting> {
        let row = sqlx::query_as!(
            RuntimeSetting,
            r"
            UPDATE settings
            SET value = $1, version = version + 1, updated_at = NOW()
            WHERE key = $2
            RETURNING key, group_name, value, version, created_at, updated_at
            ",
            value,
            key,
        )
        .fetch_one(&self.pool)
        .await?;

        // Notification is handled by the database trigger (settings_change_trigger)
        // which fires pg_notify('settings_changed', key) on INSERT/UPDATE/DELETE.
        debug!("Updated setting '{}'", key);
        Ok(row)
    }

    /// Insert or update a setting value by key.
    pub async fn upsert(&self, key: &str, group_name: &str, value: &str) -> Result<RuntimeSetting> {
        let row = sqlx::query_as!(
            RuntimeSetting,
            r"
            INSERT INTO settings (key, group_name, value, version)
            VALUES ($1, $2, $3, 0)
            ON CONFLICT (key) DO UPDATE
            SET group_name = EXCLUDED.group_name,
                value = EXCLUDED.value,
                version = settings.version + 1,
                updated_at = NOW()
            RETURNING key, group_name, value, version, created_at, updated_at
            ",
            key,
            group_name,
            value,
        )
        .fetch_one(&self.pool)
        .await?;

        debug!("Upserted setting '{}'", key);
        Ok(row)
    }

    /// Update a setting value by key with optimistic locking.
    ///
    /// This method checks the `expected_version` before updating. If the current
    /// version in the database doesn't match, it returns `OptimisticLockConflict`.
    ///
    /// Use this method when multiple admins might update settings concurrently
    /// to prevent "lost update" problems.
    ///
    /// # Arguments
    /// * `key` - The setting key to update
    /// * `value` - The new value
    /// * `expected_version` - The version the caller expects (from previous get)
    ///
    /// # Returns
    /// * `Ok(RuntimeSetting)` - Updated setting with new version
    /// * `Err(OptimisticLockConflict)` - Version mismatch, update rejected
    pub async fn update_with_version(
        &self,
        key: &str,
        value: &str,
        expected_version: i32,
    ) -> Result<RuntimeSetting> {
        let row = sqlx::query_as!(
            RuntimeSetting,
            r"
            UPDATE settings
            SET value = $1, version = version + 1, updated_at = NOW()
            WHERE key = $2 AND version = $3
            RETURNING key, group_name, value, version, created_at, updated_at
            ",
            value,
            key,
            expected_version,
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            debug!(
                "Updated setting '{}' with optimistic lock (version {} -> {})",
                key, expected_version, row.version
            );
            Ok(row)
        } else {
            debug!(
                "Optimistic lock conflict for setting '{}' (expected version {})",
                key, expected_version
            );
            Err(Error::OptimisticLockConflict)
        }
    }
}
