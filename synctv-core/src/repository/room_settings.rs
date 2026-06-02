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

use sqlx::PgPool;

use crate::{
    models::{RoomId, RoomSettings},
    Error, Result,
};

fn parse_room_settings_json(room_id: RoomId, value: &str) -> Result<RoomSettings> {
    serde_json::from_str(value).map_err(|e| {
        Error::Internal(format!(
            "Failed to deserialize room settings for room {room_id}: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_room_settings_json_error_includes_room_id() {
        let room_id = RoomId::expect_positive(42);
        let error = parse_room_settings_json(room_id, "not json")
            .expect_err("invalid settings JSON should fail");

        assert!(
            error
                .to_string()
                .contains("Failed to deserialize room settings"),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains(&room_id.to_string()),
            "error should identify corrupted room: {error}"
        );
    }
}

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
        let row = sqlx::query!(
            r#"
            SELECT value, version
            FROM room_settings
            WHERE room_id = $1 AND key = '_settings'
            "#,
            room_id as &RoomId,
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            let settings = parse_room_settings_json(*room_id, &row.value)?;
            Ok((settings, row.version))
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
        let row = sqlx::query!(
            r#"
            SELECT value, version
            FROM room_settings
            WHERE room_id = $1 AND key = '_settings'
            FOR UPDATE
            "#,
            room_id as &RoomId,
        )
        .fetch_optional(executor)
        .await?;

        if let Some(row) = row {
            let settings = parse_room_settings_json(*room_id, &row.value)?;
            Ok(settings)
        } else {
            Ok(RoomSettings::default())
        }
    }

    /// Set a specific setting for a room
    pub async fn set(&self, room_id: &RoomId, key: &str, value: &str) -> Result<()> {
        sqlx::query!(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, $2, $3, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $3, version = room_settings.version + 1, updated_at = NOW()
            ",
            room_id as &RoomId,
            key,
            value,
        )
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
        sqlx::query!(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, $2, $3, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $3, version = room_settings.version + 1, updated_at = NOW()
            ",
            room_id as &RoomId,
            key,
            value,
        )
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
        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        sqlx::query!(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, '_settings', $2, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $2, version = room_settings.version + 1, updated_at = NOW()
            ",
            room_id as &RoomId,
            json_value,
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Get settings for multiple rooms in a single query
    pub async fn get_batch(
        &self,
        room_ids: &[RoomId],
    ) -> Result<std::collections::HashMap<RoomId, RoomSettings>> {
        Ok(self
            .get_batch_with_version(room_ids)
            .await?
            .into_iter()
            .map(|(room_id, (settings, _version))| (room_id, settings))
            .collect())
    }

    /// Get settings and optimistic-lock versions for multiple rooms in a single query.
    pub async fn get_batch_with_version(
        &self,
        room_ids: &[RoomId],
    ) -> Result<std::collections::HashMap<RoomId, (RoomSettings, i64)>> {
        if room_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let ids: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();

        let rows = sqlx::query!(
            r#"
            SELECT room_id as "room_id: RoomId", value, version
            FROM room_settings
            WHERE room_id = ANY($1) AND key = '_settings'
            "#,
            &ids,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut result = std::collections::HashMap::new();
        for row in rows {
            let settings = parse_room_settings_json(row.room_id, &row.value)?;
            result.insert(row.room_id, (settings, row.version));
        }
        Ok(result)
    }

    /// Get a specific setting value for a room
    pub async fn get_value(&self, room_id: &RoomId, key: &str) -> Result<Option<String>> {
        let result = sqlx::query_scalar!(
            r"
            SELECT value
            FROM room_settings
            WHERE room_id = $1 AND key = $2
            ",
            room_id as &RoomId,
            key,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(result)
    }

    /// Set multiple settings at once
    ///
    /// This uses **automatic serde serialization** - the entire struct becomes JSON!
    /// All settings are stored as a single JSON value under key "_settings".
    pub async fn set_settings(&self, room_id: &RoomId, settings: &RoomSettings) -> Result<()> {
        // Serialize entire settings struct to JSON - one line!
        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        // Upsert settings as single JSON value (ON CONFLICT handles existing rows)
        sqlx::query!(
            r"
            INSERT INTO room_settings (room_id, key, value, version)
            VALUES ($1, '_settings', $2, 1)
            ON CONFLICT (room_id, key)
            DO UPDATE SET value = $2, version = room_settings.version + 1, updated_at = NOW()
            ",
            room_id as &RoomId,
            json_value,
        )
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
        self.set_settings_with_version_with_executor(
            room_id,
            settings,
            expected_version,
            &self.pool,
        )
        .await
    }

    /// Set settings with optimistic locking and an externally allocated version.
    ///
    /// This is used by strong-cache write paths where Redis is the version
    /// fence allocator. The database stores the exact allocated version instead
    /// of deriving a new version with `version + 1`.
    pub async fn set_settings_with_exact_version(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        expected_version: i64,
        new_version: i64,
    ) -> Result<i64> {
        self.set_settings_with_exact_version_with_executor(
            room_id,
            settings,
            expected_version,
            new_version,
            &self.pool,
        )
        .await
    }

    pub async fn set_settings_with_exact_version_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        expected_version: i64,
        new_version: i64,
        executor: E,
    ) -> Result<i64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if new_version <= expected_version {
            return Err(Error::InvalidInput(format!(
                "new settings version {new_version} must be greater than expected version {expected_version}"
            )));
        }

        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        if expected_version == 0 {
            let row = sqlx::query_scalar!(
                r"
                INSERT INTO room_settings (room_id, key, value, version)
                VALUES ($1, '_settings', $2, $3)
                ON CONFLICT (room_id, key) DO UPDATE
                SET value = EXCLUDED.value, version = EXCLUDED.version, updated_at = NOW()
                WHERE room_settings.version = 0
                RETURNING version
                ",
                room_id as &RoomId,
                json_value,
                new_version,
            )
            .fetch_optional(executor)
            .await?;

            match row {
                Some(version) => Ok(version),
                None => Err(Error::OptimisticLockConflict),
            }
        } else {
            let row = sqlx::query_scalar!(
                r"
                UPDATE room_settings
                SET value = $2, version = $4, updated_at = NOW()
                WHERE room_id = $1 AND key = '_settings' AND version = $3
                RETURNING version
                ",
                room_id as &RoomId,
                json_value,
                expected_version,
                new_version,
            )
            .fetch_optional(executor)
            .await?;

            match row {
                Some(version) => Ok(version),
                None => Err(Error::OptimisticLockConflict),
            }
        }
    }

    pub async fn set_settings_with_version_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        expected_version: i64,
        executor: E,
    ) -> Result<i64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let json_value = serde_json::to_string(settings)
            .map_err(|e| Error::Internal(format!("Failed to serialize room settings: {e}")))?;

        if expected_version == 0 {
            // No existing row -- INSERT with conflict check on version
            let row = sqlx::query!(
                r"
                INSERT INTO room_settings (room_id, key, value, version)
                VALUES ($1, '_settings', $2, 1)
                ON CONFLICT (room_id, key) DO NOTHING
                RETURNING version
                ",
                room_id as &RoomId,
                json_value,
            )
            .fetch_optional(executor)
            .await?;

            match row {
                Some(row) => Ok(row.version),
                // Row already exists (someone inserted concurrently)
                None => Err(Error::OptimisticLockConflict),
            }
        } else {
            // Existing row -- UPDATE with version check
            let row = sqlx::query!(
                r"
                UPDATE room_settings
                SET value = $2, version = version + 1, updated_at = NOW()
                WHERE room_id = $1 AND key = '_settings' AND version = $3
                RETURNING version
                ",
                room_id as &RoomId,
                json_value,
                expected_version,
            )
            .fetch_optional(executor)
            .await?;

            match row {
                Some(row) => Ok(row.version),
                None => Err(Error::OptimisticLockConflict),
            }
        }
    }

    /// Delete a specific setting for a room (revert to default)
    pub async fn delete(&self, room_id: &RoomId, key: &str) -> Result<()> {
        sqlx::query!(
            "DELETE FROM room_settings WHERE room_id = $1 AND key = $2",
            room_id as &RoomId,
            key,
        )
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
        sqlx::query!(
            "DELETE FROM room_settings WHERE room_id = $1 AND key = $2",
            room_id as &RoomId,
            key,
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Delete auxiliary setting rows while keeping the versioned `_settings` snapshot.
    pub async fn delete_auxiliary_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query!(
            "DELETE FROM room_settings WHERE room_id = $1 AND key <> '_settings'",
            room_id as &RoomId,
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Delete all settings for a room
    pub async fn delete_all(&self, room_id: &RoomId) -> Result<()> {
        sqlx::query!(
            "DELETE FROM room_settings WHERE room_id = $1",
            room_id as &RoomId,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get all settings for a room as raw `HashMap` (for type-safe settings system)
    pub async fn get_all_raw(
        &self,
        room_id: &RoomId,
    ) -> Result<std::collections::HashMap<String, String>> {
        let rows = sqlx::query!(
            r"
            SELECT key, value
            FROM room_settings
            WHERE room_id = $1
            ",
            room_id as &RoomId,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut settings = std::collections::HashMap::new();
        for row in rows {
            settings.insert(row.key, row.value);
        }

        Ok(settings)
    }
}
