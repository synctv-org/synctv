//! Settings repository for database operations

use sqlx::{PgPool, Row};
use tracing::debug;

use crate::Result;

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
            SELECT key, group_name, value, created_at, updated_at
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
                    group: row.try_get("group_name")?,
                    value: row.try_get("value")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect();

        debug!("Retrieved {} settings", groups.as_ref().map_or(0, std::vec::Vec::len));
        groups
    }

    /// Get a single setting by key
    pub async fn get(&self, key: &str) -> Result<SettingsGroup> {
        let row = sqlx::query(
            r"
            SELECT key, group_name, value, created_at, updated_at
            FROM settings
            WHERE key = $1
            ",
        )
        .bind(key)
        .fetch_one(&self.pool)
        .await?;

        Ok(SettingsGroup {
            key: row.try_get("key")?,
            group: row.try_get("group_name")?,
            value: row.try_get("value")?,
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
            SET value = $1, updated_at = NOW()
            WHERE key = $2
            RETURNING key, group_name, value, created_at, updated_at
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
            group: row.try_get("group_name")?,
            value: row.try_get("value")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}
