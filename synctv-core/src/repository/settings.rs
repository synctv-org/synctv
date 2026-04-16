//! Settings repository for database operations

use sqlx::{PgPool, Row};
use tracing::debug;

use crate::{Error, Result};

use crate::models::settings::SettingsGroup;

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
    pub async fn get_all(&self) -> Result<Vec<SettingsGroup>> {
        let rows = sqlx::query(
            r"
            SELECT key, group_name, value, version, created_at, updated_at
            FROM settings
            ORDER BY group_name
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        let groups: Result<Vec<_>> = rows
            .into_iter()
            .map(|row| {
                Ok(SettingsGroup {
                    key: row.try_get("key")?,
                    group_name: row.try_get("group_name")?,
                    value: row.try_get("value")?,
                    version: row.try_get("version")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect();

        debug!(
            "Retrieved {} settings",
            groups.as_ref().map_or(0, std::vec::Vec::len)
        );
        groups
    }

    /// Get a single setting by key
    pub async fn get(&self, key: &str) -> Result<SettingsGroup> {
        let row = sqlx::query(
            r"
            SELECT key, group_name, value, version, created_at, updated_at
            FROM settings
            WHERE key = $1
            ",
        )
        .bind(key)
        .fetch_one(&self.pool)
        .await?;

        Ok(SettingsGroup {
            key: row.try_get("key")?,
            group_name: row.try_get("group_name")?,
            value: row.try_get("value")?,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    /// Update a setting value by key
    ///
    /// A database trigger on the `settings` table automatically sends a
    /// NOTIFY on the `settings_changed` channel so other replicas can
    /// reload the changed setting.
    pub async fn update(&self, key: &str, value: &str) -> Result<SettingsGroup> {
        let row = sqlx::query(
            r"
            UPDATE settings
            SET value = $1, version = version + 1, updated_at = NOW()
            WHERE key = $2
            RETURNING key, group_name, value, version, created_at, updated_at
            ",
        )
        .bind(value)
        .bind(key)
        .fetch_one(&self.pool)
        .await?;

        // Notification is handled by the database trigger (settings_change_trigger)
        // which fires pg_notify('settings_changed', key) on INSERT/UPDATE/DELETE.
        debug!("Updated setting '{}'", key);
        Ok(SettingsGroup {
            key: row.try_get("key")?,
            group_name: row.try_get("group_name")?,
            value: row.try_get("value")?,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    /// Insert or update a setting value by key.
    pub async fn upsert(&self, key: &str, group_name: &str, value: &str) -> Result<SettingsGroup> {
        let row = sqlx::query(
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
        )
        .bind(key)
        .bind(group_name)
        .bind(value)
        .fetch_one(&self.pool)
        .await?;

        debug!("Upserted setting '{}'", key);
        Ok(SettingsGroup {
            key: row.try_get("key")?,
            group_name: row.try_get("group_name")?,
            value: row.try_get("value")?,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
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
    /// * `Ok(SettingsGroup)` - Updated setting with new version
    /// * `Err(OptimisticLockConflict)` - Version mismatch, update rejected
    pub async fn update_with_version(
        &self,
        key: &str,
        value: &str,
        expected_version: i32,
    ) -> Result<SettingsGroup> {
        let row = sqlx::query(
            r"
            UPDATE settings
            SET value = $1, version = version + 1, updated_at = NOW()
            WHERE key = $2 AND version = $3
            RETURNING key, group_name, value, version, created_at, updated_at
            ",
        )
        .bind(value)
        .bind(key)
        .bind(expected_version)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            debug!(
                "Updated setting '{}' with optimistic lock (version {} -> {})",
                key,
                expected_version,
                row.try_get::<i32, _>("version")?
            );
            Ok(SettingsGroup {
                key: row.try_get("key")?,
                group_name: row.try_get("group_name")?,
                value: row.try_get("value")?,
                version: row.try_get("version")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        } else {
            debug!(
                "Optimistic lock conflict for setting '{}' (expected version {})",
                key, expected_version
            );
            Err(Error::OptimisticLockConflict)
        }
    }
}
