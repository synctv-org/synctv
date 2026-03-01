//! Room settings repository
//!
//! Manages loading and saving room settings from/to the `room_settings` table.
//!
//! # Architecture
//!
//! This repository uses a **key-value based storage** approach:
//! - Each room setting is stored as a separate row (`room_id`, key, value)
//! - Settings are loaded and merged with defaults
//! - Uses serde for automatic serialization/deserialization

use sqlx::{PgPool, Row};

use crate::{
    models::{RoomId, RoomSettings},
    Error, Result,
};

/// Room settings repository
#[derive(Clone)]
pub struct RoomSettingsRepository {
    pool: PgPool,
}

impl RoomSettingsRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get a reference to the database pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get all settings for a room as `RoomSettings` struct
    ///
    /// This uses **automatic serde deserialization** - no manual mapping needed!
    /// The entire settings struct is stored as a single JSON value under key "_settings".
    pub async fn get(&self, room_id: &RoomId) -> Result<RoomSettings> {
        let (settings, _version) = self.get_with_version(room_id).await?;
        Ok(settings)
    }

    /// Get all settings for a room along with the current version for optimistic locking.
    ///
    /// Returns `(settings, version)` where version is 0 if no settings row exists yet.
    pub async fn get_with_version(&self, room_id: &RoomId) -> Result<(RoomSettings, i64)> {
        let room_id_str = room_id.as_str();

        let row = sqlx::query(
            r"
            SELECT value, version
            FROM room_settings
            WHERE room_id = $1 AND key = '_settings'
        ",
        )
        .bind(room_id_str)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            let value: String = row.try_get("value")?;
            let version: i64 = row.try_get("version")?;
            let settings: RoomSettings = serde_json::from_str(&value).map_err(|e| {
                Error::Internal(format!("Failed to deserialize room settings: {e}"))
            })?;
            Ok((settings, version))
        } else {
            // No settings stored, return defaults with version 0
            Ok((RoomSettings::default(), 0))
        }
    }

    /// Get settings with row-level lock (FOR UPDATE) using a provided executor.
    ///
    /// Must be called within a transaction. Locks the settings row to prevent
    /// concurrent read-modify-write races.
    pub async fn get_for_update<'e, E>(&self, room_id: &RoomId, executor: E) -> Result<RoomSettings>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let room_id_str = room_id.as_str();

        let row = sqlx::query(
            r"
            SELECT value, version
            FROM room_settings
            WHERE room_id = $1 AND key = '_settings'
            FOR UPDATE
        ",
        )
        .bind(room_id_str)
        .fetch_optional(executor)
        .await?;

        if let Some(row) = row {
            let value: String = row.try_get("value")?;
            let settings: RoomSettings = serde_json::from_str(&value).map_err(|e| {
                Error::Internal(format!("Failed to deserialize room settings: {e}"))
            })?;
            Ok(settings)
        } else {
            Ok(RoomSettings::default())
        }
    }

    /// Set a specific setting for a room
    pub async fn set(&self, room_id: &RoomId, key: &str, value: &str) -> Result<()> {
        let room_id_str = room_id.as_str();

        sqlx::query(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, $2, $3, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $3, version = room_settings.version + 1, updated_at = NOW()
        ",
        )
        .bind(room_id_str)
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Set a specific setting using a provided executor (pool or transaction)
    pub async fn set_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        key: &str,
        value: &str,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let room_id_str = room_id.as_str();

        sqlx::query(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, $2, $3, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $3, version = room_settings.version + 1, updated_at = NOW()
        ",
        )
        .bind(room_id_str)
        .bind(key)
        .bind(value)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Set multiple settings at once using a provided executor (pool or transaction)
    pub async fn set_settings_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let room_id_str = room_id.as_str();

        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        sqlx::query(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, '_settings', $2, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $2, version = room_settings.version + 1, updated_at = NOW()
        ",
        )
        .bind(room_id_str)
        .bind(&json_value)
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Get settings for multiple rooms in a single query
    pub async fn get_batch(
        &self,
        room_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, RoomSettings>> {
        if room_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let rows = sqlx::query(
            r"
            SELECT room_id, value
            FROM room_settings
            WHERE room_id = ANY($1) AND key = '_settings'
            ",
        )
        .bind(room_ids)
        .fetch_all(&self.pool)
        .await?;

        let mut result = std::collections::HashMap::new();
        for row in rows {
            let rid: String = row.try_get("room_id")?;
            let value: String = row.try_get("value")?;
            if let Ok(settings) = serde_json::from_str::<RoomSettings>(&value) {
                result.insert(rid, settings);
            }
        }
        Ok(result)
    }

    /// Get a specific setting value for a room
    pub async fn get_value(&self, room_id: &RoomId, key: &str) -> Result<Option<String>> {
        let room_id_str = room_id.as_str();

        let result = sqlx::query(
            r"
            SELECT value
            FROM room_settings
            WHERE room_id = $1 AND key = $2
        ",
        )
        .bind(room_id_str)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(result.map(|row| row.try_get("value")).transpose()?)
    }

    /// Get password hash for a room
    pub async fn get_password_hash(&self, room_id: &RoomId) -> Result<Option<String>> {
        self.get_value(room_id, "password").await
    }

    /// Set multiple settings at once
    ///
    /// This uses **automatic serde serialization** - the entire struct becomes JSON!
    /// All settings are stored as a single JSON value under key "_settings".
    pub async fn set_settings(&self, room_id: &RoomId, settings: &RoomSettings) -> Result<()> {
        let room_id_str = room_id.as_str();

        // Serialize entire settings struct to JSON - one line!
        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        // Upsert settings as single JSON value (ON CONFLICT handles existing rows)
        sqlx::query(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, '_settings', $2, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $2, version = room_settings.version + 1, updated_at = NOW()
        ",
        )
        .bind(room_id_str)
        .bind(&json_value)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Set settings with optimistic locking (CAS - Compare And Swap).
    ///
    /// Updates settings only if the current version matches `expected_version`.
    /// Returns `Err(Error::OptimisticLockConflict)` if the version has changed
    /// (concurrent modification detected).
    pub async fn set_settings_with_version(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        expected_version: i64,
    ) -> Result<i64> {
        let room_id_str = room_id.as_str();

        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        if expected_version == 0 {
            // No existing row -- INSERT with conflict check on version
            let row = sqlx::query(
                r"
                INSERT INTO room_settings (room_id, key, value, version)
                VALUES ($1, '_settings', $2, 1)
                ON CONFLICT (room_id, key) DO NOTHING
                RETURNING version
                ",
            )
            .bind(room_id_str)
            .bind(&json_value)
            .fetch_optional(&self.pool)
            .await?;

            match row {
                Some(row) => Ok(row.try_get("version")?),
                // Row already exists (someone inserted concurrently)
                None => Err(Error::OptimisticLockConflict),
            }
        } else {
            // Existing row -- UPDATE with version check
            let row = sqlx::query(
                r"
                UPDATE room_settings
                SET value = $2, version = version + 1, updated_at = NOW()
                WHERE room_id = $1 AND key = '_settings' AND version = $3
                RETURNING version
                ",
            )
            .bind(room_id_str)
            .bind(&json_value)
            .bind(expected_version)
            .fetch_optional(&self.pool)
            .await?;

            match row {
                Some(row) => Ok(row.try_get("version")?),
                None => Err(Error::OptimisticLockConflict),
            }
        }
    }

    /// Delete a specific setting for a room (revert to default)
    pub async fn delete(&self, room_id: &RoomId, key: &str) -> Result<()> {
        let room_id_str = room_id.as_str();

        sqlx::query("DELETE FROM room_settings WHERE room_id = $1 AND key = $2")
            .bind(room_id_str)
            .bind(key)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Delete a specific setting using a provided executor (pool or transaction)
    pub async fn delete_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        key: &str,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let room_id_str = room_id.as_str();

        sqlx::query("DELETE FROM room_settings WHERE room_id = $1 AND key = $2")
            .bind(room_id_str)
            .bind(key)
            .execute(executor)
            .await?;

        Ok(())
    }

    /// Delete all settings for a room
    pub async fn delete_all(&self, room_id: &RoomId) -> Result<()> {
        let room_id_str = room_id.as_str();

        sqlx::query("DELETE FROM room_settings WHERE room_id = $1")
            .bind(room_id_str)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get all settings for a room as raw `HashMap` (for type-safe settings system)
    pub async fn get_all_raw(
        &self,
        room_id: &RoomId,
    ) -> Result<std::collections::HashMap<String, String>> {
        let room_id_str = room_id.as_str();

        let rows = sqlx::query(
            r"
            SELECT key, value
            FROM room_settings
            WHERE room_id = $1
        ",
        )
        .bind(room_id_str)
        .fetch_all(&self.pool)
        .await?;

        let mut settings = std::collections::HashMap::new();
        for row in rows {
            let key: String = row.try_get("key")?;
            let value: String = row.try_get("value")?;
            settings.insert(key, value);
        }

        Ok(settings)
    }
}
