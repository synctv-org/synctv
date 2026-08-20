use std::collections::HashMap;

use sqlx::PgPool;

use crate::{
    models::{RoomId, RoomSettings},
    repository::pools::RepoPools,
    Error, Result,
};

#[derive(Clone)]
pub struct RoomSettingsRepository {
    pools: RepoPools,
}

impl RoomSettingsRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pools: RepoPools::new(pool),
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
        }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        self.pools.primary()
    }

    #[must_use]
    pub fn eventually_consistent_pool(&self) -> &PgPool {
        self.pools.read()
    }

    pub async fn get(&self, room_id: &RoomId) -> Result<RoomSettings> {
        let (settings, _version) = self.get_with_version(room_id).await?;
        Ok(settings)
    }

    pub async fn get_with_version(&self, room_id: &RoomId) -> Result<(RoomSettings, i64)> {
        let row = sqlx::query!(
            r#"
            SELECT settings AS "settings!: RoomSettings", version
            FROM room_settings
            WHERE room_id = $1
            "#,
            room_id as &RoomId,
        )
        .fetch_optional(self.pools.primary())
        .await?;

        Ok(row.map_or_else(
            || (RoomSettings::default(), 0),
            |row| (row.settings, row.version),
        ))
    }

    pub async fn get_for_update<'e, E>(&self, room_id: &RoomId, executor: E) -> Result<RoomSettings>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query!(
            r#"
            SELECT settings AS "settings!: RoomSettings"
            FROM room_settings
            WHERE room_id = $1
            FOR UPDATE
            "#,
            room_id as &RoomId,
        )
        .fetch_optional(executor)
        .await?;

        Ok(row.map_or_else(RoomSettings::default, |row| row.settings))
    }

    pub async fn set_settings_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query!(
            r#"
            INSERT INTO room_settings (room_id, settings, version)
            VALUES ($1, $2, 1)
            ON CONFLICT (room_id)
            DO UPDATE SET settings = $2, version = room_settings.version + 1, updated_at = NOW()
            "#,
            room_id as &RoomId,
            settings as &RoomSettings,
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    pub async fn set_settings(&self, room_id: &RoomId, settings: &RoomSettings) -> Result<()> {
        self.set_settings_with_executor(room_id, settings, self.pools.primary())
            .await
    }

    pub async fn get_batch(&self, room_ids: &[RoomId]) -> Result<HashMap<RoomId, RoomSettings>> {
        Ok(self
            .get_batch_with_version(room_ids)
            .await?
            .into_iter()
            .map(|(room_id, (settings, _version))| (room_id, settings))
            .collect())
    }

    pub async fn get_batch_eventually_consistent(
        &self,
        room_ids: &[RoomId],
    ) -> Result<HashMap<RoomId, RoomSettings>> {
        Ok(self
            .get_batch_with_version_from_pool(room_ids, self.pools.read())
            .await?
            .into_iter()
            .map(|(room_id, (settings, _version))| (room_id, settings))
            .collect())
    }

    pub async fn get_batch_with_version(
        &self,
        room_ids: &[RoomId],
    ) -> Result<HashMap<RoomId, (RoomSettings, i64)>> {
        self.get_batch_with_version_from_pool(room_ids, self.pools.primary())
            .await
    }

    async fn get_batch_with_version_from_pool(
        &self,
        room_ids: &[RoomId],
        pool: &PgPool,
    ) -> Result<HashMap<RoomId, (RoomSettings, i64)>> {
        if room_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();

        let rows = sqlx::query!(
            r#"
            SELECT room_id AS "room_id: RoomId", settings AS "settings!: RoomSettings", version
            FROM room_settings
            WHERE room_id = ANY($1)
            "#,
            &ids,
        )
        .fetch_all(pool)
        .await?;

        let mut result = room_ids
            .iter()
            .map(|room_id| (*room_id, (RoomSettings::default(), 0)))
            .collect::<HashMap<_, _>>();
        for row in rows {
            result.insert(row.room_id, (row.settings, row.version));
        }
        Ok(result)
    }

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
            self.pools.primary(),
        )
        .await
    }

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
            self.pools.primary(),
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

        if expected_version == 0 {
            let row = sqlx::query_scalar!(
                r#"
                INSERT INTO room_settings (room_id, settings, version)
                VALUES ($1, $2, $3)
                ON CONFLICT (room_id) DO UPDATE
                SET settings = EXCLUDED.settings, version = EXCLUDED.version, updated_at = NOW()
                WHERE room_settings.version = 0
                RETURNING version
                "#,
                room_id as &RoomId,
                settings as &RoomSettings,
                new_version,
            )
            .fetch_optional(executor)
            .await?;

            row.ok_or(Error::OptimisticLockConflict)
        } else {
            let row = sqlx::query_scalar!(
                r#"
                UPDATE room_settings
                SET settings = $2, version = $4, updated_at = NOW()
                WHERE room_id = $1 AND version = $3
                RETURNING version
                "#,
                room_id as &RoomId,
                settings as &RoomSettings,
                expected_version,
                new_version,
            )
            .fetch_optional(executor)
            .await?;

            row.ok_or(Error::OptimisticLockConflict)
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
        if expected_version == 0 {
            let row = sqlx::query_scalar!(
                r#"
                INSERT INTO room_settings (room_id, settings, version)
                VALUES ($1, $2, 1)
                ON CONFLICT (room_id) DO NOTHING
                RETURNING version
                "#,
                room_id as &RoomId,
                settings as &RoomSettings,
            )
            .fetch_optional(executor)
            .await?;

            row.ok_or(Error::OptimisticLockConflict)
        } else {
            let row = sqlx::query_scalar!(
                r#"
                UPDATE room_settings
                SET settings = $2, version = version + 1, updated_at = NOW()
                WHERE room_id = $1 AND version = $3
                RETURNING version
                "#,
                room_id as &RoomId,
                settings as &RoomSettings,
                expected_version,
            )
            .fetch_optional(executor)
            .await?;

            row.ok_or(Error::OptimisticLockConflict)
        }
    }
}
