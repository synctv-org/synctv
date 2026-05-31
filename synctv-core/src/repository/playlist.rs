//! Playlist repository for database operations
//!
//! Design reference: external design doc 04-database-design.md §2.4.1

use super::query_builder::escape_ilike;
use crate::{
    models::{
        normalize_provider_instance_name, provider_type_code_from_name,
        provider_type_name_from_code, Playlist, PlaylistId, PlaylistListQuery, ProviderTypeName,
        RoomId,
    },
    Result,
};
use sqlx::{postgres::PgRow, PgPool, Row};
use std::collections::BTreeMap;

#[derive(Debug, sqlx::FromRow)]
struct PlaylistRow {
    id: PlaylistId,
    room_id: RoomId,
    creator_id: Option<crate::models::UserId>,
    name: String,
    description: String,
    cover_file_reference_id: Option<i64>,
    parent_id: Option<PlaylistId>,
    position: f64,
    source_provider: Option<i16>,
    source_config: Option<serde_json::Value>,
    provider_instance_name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
}

impl From<PlaylistRow> for Playlist {
    fn from(row: PlaylistRow) -> Self {
        Self {
            id: row.id,
            room_id: row.room_id,
            creator_id: row.creator_id,
            name: row.name,
            description: row.description,
            cover_file_reference_id: row.cover_file_reference_id,
            parent_id: row.parent_id,
            position: row.position,
            source_provider: row
                .source_provider
                .map(provider_type_name_from_code)
                .transpose()
                .map_err(crate::Error::InvalidInput)
                .unwrap_or_default(),
            source_config: row.source_config,
            provider_instance_name: row.provider_instance_name,
            created_at: row.created_at,
            updated_at: row.updated_at,
            version: row.version,
        }
    }
}

fn playlist_from_pg_row(row: &PgRow) -> Result<Playlist> {
    Ok(Playlist {
        id: row.try_get("id")?,
        room_id: row.try_get("room_id")?,
        creator_id: row.try_get("creator_id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        cover_file_reference_id: row.try_get("cover_file_reference_id")?,
        parent_id: row.try_get("parent_id")?,
        position: row.try_get("position")?,
        source_provider: row
            .try_get::<Option<ProviderTypeName>, _>("source_provider")?
            .map(|provider| provider.0),
        source_config: row.try_get("source_config")?,
        provider_instance_name: row.try_get("provider_instance_name")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

#[derive(Debug, Clone)]
pub struct PlaylistListItem {
    pub playlist: Playlist,
    pub is_available: bool,
}

const PLAYLIST_ROW_COLUMNS: &str = "id,
                   room_id,
                   creator_id,
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id,
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS provider_instance_name,
                   created_at,
                   updated_at,
                   version";

/// Playlist repository
#[derive(Clone)]
pub struct PlaylistRepository {
    pool: PgPool,
}

impl PlaylistRepository {
    fn normalize_provider_instance_name_for_db(
        provider_instance_name: Option<&str>,
    ) -> Option<&str> {
        normalize_provider_instance_name(provider_instance_name)
    }

    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn build_playlist_list_order_by(query: &PlaylistListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            crate::models::PlaylistListSortBy::Name => {
                format!("p.name {direction}, p.position {direction}, p.id {direction}")
            }
            crate::models::PlaylistListSortBy::CreatedAt => {
                format!("p.created_at {direction}, p.position {direction}, p.id {direction}")
            }
            crate::models::PlaylistListSortBy::UpdatedAt => {
                format!("p.updated_at {direction}, p.position {direction}, p.id {direction}")
            }
            crate::models::PlaylistListSortBy::Position => {
                format!("p.position {direction}, p.name {direction}, p.id {direction}")
            }
        }
    }

    fn provider_type_code(provider: &str) -> Result<i16> {
        provider_type_code_from_name(provider).map_err(crate::Error::InvalidInput)
    }

    fn push_playlist_scope_filters(
        builder: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &PlaylistListQuery,
    ) -> Result<()> {
        builder.push(" FROM playlists p LEFT JOIN users u ON p.creator_id = u.id AND u.deleted_at IS NULL WHERE p.room_id = ");
        builder.push_bind(room_id.as_i64());
        match parent_id {
            Some(parent_id) => {
                builder.push(" AND p.parent_id = ");
                builder.push_bind(parent_id.as_i64());
            }
            None => {
                builder.push(" AND p.parent_id IS NULL");
            }
        }

        if let Some(search) = &query.search {
            let pattern = escape_ilike(search);
            builder.push(" AND (p.name ILIKE ");
            builder.push_bind(pattern.clone());
            builder.push(" ESCAPE '\\' OR p.description ILIKE ");
            builder.push_bind(pattern);
            builder.push(" ESCAPE '\\')");
        }
        if let Some(source_provider) = &query.source_provider {
            builder.push(" AND p.source_provider = ");
            builder.push_bind(Self::provider_type_code(source_provider)?);
        }
        if let Some(provider_instance_name) = &query.provider_instance_name {
            if let Some(trimmed) = normalize_provider_instance_name(Some(provider_instance_name)) {
                builder.push(" AND p.provider_instance_name = ");
                builder.push_bind(trimmed.to_owned());
            } else {
                builder.push(" AND NULLIF(p.provider_instance_name, '') IS NULL");
            }
        }
        if let Some(dynamic_only) = query.dynamic_only {
            if dynamic_only {
                builder.push(" AND p.source_provider IS NOT NULL");
            } else {
                builder.push(" AND p.source_provider IS NULL");
            }
        }
        match query.availability {
            Some(true) => {
                builder.push(
                    " AND (p.creator_id IS NULL OR (u.id IS NOT NULL AND NOT EXISTS (
                    SELECT 1 FROM user_bans ub
                    WHERE ub.user_id = u.id
                      AND ub.revoked_at IS NULL
                      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                )))",
                );
            }
            Some(false) => {
                builder.push(
                    " AND p.creator_id IS NOT NULL AND (u.id IS NULL OR EXISTS (
                    SELECT 1 FROM user_bans ub
                    WHERE ub.user_id = u.id
                      AND ub.revoked_at IS NULL
                      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ))",
                );
            }
            None => {}
        }
        Ok(())
    }

    pub async fn count_filtered_by_parent(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &PlaylistListQuery,
    ) -> Result<i64> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT COUNT(*)");
        Self::push_playlist_scope_filters(&mut builder, room_id, parent_id, query)?;
        builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn list_filtered_by_parent(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &PlaylistListQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PlaylistListItem>> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT p.id, p.room_id, p.creator_id, p.name, p.description,
                    p.cover_file_reference_id,
                    p.parent_id, p.position,
                    p.source_provider, p.source_config, NULLIF(p.provider_instance_name, '') AS provider_instance_name,
                    p.created_at, p.updated_at, p.version,
                    CASE
                      WHEN p.creator_id IS NULL THEN TRUE
                      WHEN u.id IS NOT NULL AND NOT EXISTS (
                          SELECT 1 FROM user_bans ub
                          WHERE ub.user_id = u.id
                            AND ub.revoked_at IS NULL
                            AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                      ) THEN TRUE
                      ELSE FALSE
                    END AS is_available",
        );
        Self::push_playlist_scope_filters(&mut builder, room_id, parent_id, query)?;
        let order_by = Self::build_playlist_list_order_by(query);
        builder.push(format!(" ORDER BY {order_by} LIMIT "));
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(PlaylistListItem {
                    playlist: playlist_from_pg_row(&row)?,
                    is_available: row.try_get("is_available")?,
                })
            })
            .collect()
    }

    /// Get playlist by ID
    pub async fn get_by_id(&self, id: &PlaylistId) -> Result<Option<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE id = $1
            "
        );
        let row = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(id.as_i64())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(Into::into))
    }

    /// Get playlist by ID, scoped to a room.
    pub async fn get_by_room_and_id(
        &self,
        room_id: &RoomId,
        id: &PlaylistId,
    ) -> Result<Option<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND id = $2
            "
        );
        let row = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(id.as_i64())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(Into::into))
    }

    pub async fn get_by_room_and_id_for_update_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        id: &PlaylistId,
        executor: E,
    ) -> Result<Option<Playlist>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "
        );
        let row = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(id.as_i64())
            .fetch_optional(executor)
            .await?;

        Ok(row.map(Into::into))
    }

    /// Get playlists by IDs using a provided executor (for transaction support)
    pub async fn get_by_ids_with_executor<'e, E>(
        &self,
        playlist_ids: &[PlaylistId],
        executor: E,
    ) -> Result<Vec<Playlist>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if playlist_ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE id = ANY($1)
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(&id_strs)
            .fetch_all(executor)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get playlists by IDs, scoped to a room.
    pub async fn get_by_room_and_ids_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        playlist_ids: &[PlaylistId],
        executor: E,
    ) -> Result<Vec<Playlist>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if playlist_ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND id = ANY($2)
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(&id_strs)
            .fetch_all(executor)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get top-level playlists in a room.
    pub async fn get_top_level(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL
            ORDER BY position ASC
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Count top-level playlists in a room.
    pub async fn count_top_level(&self, room_id: &RoomId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*) FROM playlists WHERE room_id = $1 AND parent_id IS NULL
            ",
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count.unwrap_or(0))
    }

    /// Get paginated top-level playlists in a room.
    pub async fn get_top_level_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL
            ORDER BY position ASC
            LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get children playlists of a parent
    pub async fn get_children(&self, parent_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(parent_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get count of children playlists for a parent.
    pub async fn count_children(&self, parent_id: &PlaylistId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*) FROM playlists WHERE parent_id = $1
            ",
            parent_id as &PlaylistId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count.unwrap_or(0))
    }

    /// Get count of children playlists for a parent, scoped to a room.
    pub async fn count_children_in_room(
        &self,
        room_id: &RoomId,
        parent_id: &PlaylistId,
    ) -> Result<i64> {
        let count = sqlx::query_scalar_unchecked!(
            r"
            SELECT COUNT(*) FROM playlists WHERE room_id = $1 AND parent_id = $2
            ",
            room_id.as_i64(),
            parent_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        Ok(count)
    }

    /// Get paginated children playlists for a parent.
    pub async fn get_children_paginated(
        &self,
        parent_id: &PlaylistId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(parent_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get all playlists in a room (tree structure)
    pub async fn get_by_room(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Count all playlists in a room
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*) FROM playlists WHERE room_id = $1
            ",
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count.unwrap_or(0))
    }

    /// Get paginated playlists in a room
    pub async fn get_by_room_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    const ORDER_STEP: f64 = 1024.0;
    const MIN_ORDER_GAP: f64 = 1e-9;

    fn scope_lock_key(room_id: &RoomId, parent_id: Option<&PlaylistId>) -> i64 {
        super::stable_scope_lock_key(room_id.as_i64(), parent_id.map(PlaylistId::as_i64))
    }

    async fn lock_scope_with_tx(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1)",
            Self::scope_lock_key(room_id, parent_id),
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn get_scope_previous_position_with_tx(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        exclude_playlist_id: &PlaylistId,
        anchor_position: f64,
        anchor_playlist_id: &PlaylistId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Option<f64>> {
        sqlx::query_scalar!(
            r"
            SELECT position
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
              AND id <> $3
              AND (
                    position < $4
                 OR (position = $4 AND id < $5)
              )
            ORDER BY position DESC, id DESC
            LIMIT 1
            ",
            room_id as &RoomId,
            parent_id.map(PlaylistId::as_i64),
            exclude_playlist_id as &PlaylistId,
            anchor_position,
            anchor_playlist_id as &PlaylistId,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(Into::into)
    }

    async fn get_scope_next_position_with_tx(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        exclude_playlist_id: &PlaylistId,
        anchor_position: f64,
        anchor_playlist_id: &PlaylistId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Option<f64>> {
        sqlx::query_scalar!(
            r"
            SELECT position
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
              AND id <> $3
              AND (
                    position > $4
                 OR (position = $4 AND id > $5)
              )
            ORDER BY position ASC, id ASC
            LIMIT 1
            ",
            room_id as &RoomId,
            parent_id.map(PlaylistId::as_i64),
            exclude_playlist_id as &PlaylistId,
            anchor_position,
            anchor_playlist_id as &PlaylistId,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(Into::into)
    }

    async fn rebalance_scope_with_tx(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        let rows = sqlx::query!(
            r#"
            SELECT id as "id: PlaylistId"
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
            ORDER BY position ASC, id ASC
            FOR UPDATE
            "#,
            room_id as &RoomId,
            parent_id.map(PlaylistId::as_i64),
        )
        .fetch_all(&mut **tx)
        .await?;

        let mut position = Self::ORDER_STEP;
        for row in rows {
            sqlx::query!(
                "UPDATE playlists SET position = $2, version = version + 1 WHERE id = $1",
                row.id as PlaylistId,
                position,
            )
            .execute(&mut **tx)
            .await?;
            position += Self::ORDER_STEP;
        }

        Ok(())
    }

    fn midpoint(previous: f64, next: f64) -> Option<f64> {
        let gap = next - previous;
        if !gap.is_finite() || gap <= Self::MIN_ORDER_GAP {
            return None;
        }
        let midpoint = previous + gap / 2.0;
        if !midpoint.is_finite() || midpoint <= previous || midpoint >= next {
            return None;
        }
        Some(midpoint)
    }

    /// Create a new playlist.
    pub async fn create(&self, playlist: &Playlist) -> Result<Playlist> {
        self.create_with_executor(playlist, &self.pool).await
    }

    /// Create a playlist using a provided executor (pool or transaction).
    pub async fn create_with_executor<'e, E>(
        &self,
        playlist: &Playlist,
        executor: E,
    ) -> Result<Playlist>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let source_provider_code = playlist
            .source_provider
            .as_deref()
            .map(Self::provider_type_code)
            .transpose()?;
        let parent_id = playlist.parent_id;

        let sql = format!(
            r"
            INSERT INTO playlists (room_id, creator_id, name, description,
                                   cover_file_reference_id,
                                   parent_id, position, source_provider, source_config, provider_instance_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING {PLAYLIST_ROW_COLUMNS}
            "
        );
        let row = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(playlist.room_id.as_i64())
            .bind(
                playlist
                    .creator_id
                    .as_ref()
                    .map(crate::models::UserId::as_i64),
            )
            .bind(&playlist.name)
            .bind(&playlist.description)
            .bind(playlist.cover_file_reference_id)
            .bind(parent_id.as_ref().map(PlaylistId::as_i64))
            .bind(playlist.position)
            .bind(source_provider_code)
            .bind(playlist.source_config.as_ref())
            .bind(Self::normalize_provider_instance_name_for_db(
                playlist.provider_instance_name.as_deref(),
            ))
            .fetch_one(executor)
            .await?;

        Ok(row.into())
    }

    /// Get the next append position within a scope.
    pub async fn get_next_append_position_with_tx(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<f64> {
        self.lock_scope_with_tx(room_id, parent_id, tx).await?;

        let max_pos = sqlx::query_scalar!(
            r"
            SELECT MAX(position)
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
            ",
            room_id as &RoomId,
            parent_id.map(PlaylistId::as_i64),
        )
        .fetch_one(&mut **tx)
        .await?;

        match max_pos {
            Some(position) if position.is_finite() => Ok(position + Self::ORDER_STEP),
            _ => Ok(Self::ORDER_STEP),
        }
    }

    /// Update playlist with optimistic locking.
    ///
    /// Returns `Err(Error::OptimisticLockConflict)` if the version in the database
    /// does not match `expected_version`.
    ///
    /// On success, returns the updated playlist with incremented version.
    pub async fn update_with_version(
        &self,
        playlist: &Playlist,
        expected_version: i32,
    ) -> Result<Playlist> {
        self.update_with_version_with_executor(playlist, expected_version, &self.pool)
            .await
    }

    pub async fn update_with_version_with_executor<'e, E>(
        &self,
        playlist: &Playlist,
        expected_version: i32,
        executor: E,
    ) -> Result<Playlist>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            UPDATE playlists
            SET name = $2, description = $3,
                cover_file_reference_id = $4,
                position = $5,
                version = version + 1
            WHERE id = $1 AND version = $6
            RETURNING {PLAYLIST_ROW_COLUMNS}
            "
        );
        let row = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(playlist.id.as_i64())
            .bind(&playlist.name)
            .bind(&playlist.description)
            .bind(playlist.cover_file_reference_id)
            .bind(playlist.position)
            .bind(expected_version)
            .fetch_optional(executor)
            .await?;

        match row {
            Some(row) => Ok(row.into()),
            None => Err(crate::Error::OptimisticLockConflict),
        }
    }

    pub async fn move_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        before_playlist_id: Option<&PlaylistId>,
        after_playlist_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Playlist> {
        let ((Some(anchor_id), None) | (None, Some(anchor_id))) =
            (before_playlist_id, after_playlist_id)
        else {
            return Err(crate::Error::InvalidInput(
                "Exactly one of before_playlist_id or after_playlist_id must be set".to_string(),
            ));
        };

        if playlist_id == anchor_id {
            return Err(crate::Error::InvalidInput(
                "Cannot move a playlist relative to itself".to_string(),
            ));
        }

        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "
        );
        let moved: Playlist = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(playlist_id.as_i64())
            .fetch_optional(&mut **tx)
            .await?
            .map(Into::into)
            .ok_or_else(|| crate::Error::NotFound("Playlist not found".to_string()))?;

        let sql = format!(
            r"
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM playlists
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "
        );
        let anchor: Playlist = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(anchor_id.as_i64())
            .fetch_optional(&mut **tx)
            .await?
            .map(Into::into)
            .ok_or_else(|| crate::Error::NotFound("Anchor playlist not found".to_string()))?;

        if moved.parent_id != anchor.parent_id {
            return Err(crate::Error::InvalidInput(
                "Playlist can only be moved relative to a sibling in the same parent scope"
                    .to_string(),
            ));
        }

        self.lock_scope_with_tx(&moved.room_id, moved.parent_id.as_ref(), tx)
            .await?;

        for _ in 0..2 {
            let anchor_position: f64 = sqlx::query_scalar!(
                "SELECT position FROM playlists WHERE id = $1 FOR UPDATE",
                anchor.id as PlaylistId,
            )
            .fetch_one(&mut **tx)
            .await?;

            let new_position = if before_playlist_id.is_some() {
                match self
                    .get_scope_previous_position_with_tx(
                        &moved.room_id,
                        moved.parent_id.as_ref(),
                        &moved.id,
                        anchor_position,
                        &anchor.id,
                        tx,
                    )
                    .await?
                {
                    Some(previous) => Self::midpoint(previous, anchor_position),
                    None => Some(anchor_position - Self::ORDER_STEP),
                }
            } else {
                match self
                    .get_scope_next_position_with_tx(
                        &moved.room_id,
                        moved.parent_id.as_ref(),
                        &moved.id,
                        anchor_position,
                        &anchor.id,
                        tx,
                    )
                    .await?
                {
                    Some(next) => Self::midpoint(anchor_position, next),
                    None => Some(anchor_position + Self::ORDER_STEP),
                }
            };

            if let Some(position) = new_position.filter(|position| position.is_finite()) {
                let sql = format!(
                    r"
                    UPDATE playlists
                    SET position = $2, version = version + 1
                    WHERE id = $1
                    RETURNING {PLAYLIST_ROW_COLUMNS}
                    "
                );
                let row = sqlx::query_as::<_, PlaylistRow>(&sql)
                    .bind(moved.id.as_i64())
                    .bind(position)
                    .fetch_one(&mut **tx)
                    .await?;

                return Ok(row.into());
            }

            self.rebalance_scope_with_tx(&moved.room_id, moved.parent_id.as_ref(), tx)
                .await?;
        }

        Err(crate::Error::Internal(
            "Failed to compute a stable playlist order position".to_string(),
        ))
    }

    /// Delete a playlist subtree and all media attached to that subtree.
    ///
    /// Playback-state `RESTRICT` foreign keys are intentionally preserved: if the
    /// target playlist or any nested media is still referenced by current room
    /// playback, the delete fails and the transaction rolls back.
    pub async fn delete(&self, id: &PlaylistId) -> Result<bool> {
        let mut tx = self.pool.begin().await?;

        let room_id = sqlx::query_scalar!(
            r#"SELECT room_id as "room_id: RoomId" FROM playlists WHERE id = $1"#,
            id as &PlaylistId,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(room_id) = room_id else {
            return Ok(false);
        };

        let rows = sqlx::query!(
            r#"WITH RECURSIVE playlist_tree AS (
                SELECT id, 0 AS depth
                FROM playlists
                WHERE id = $1
                UNION ALL
                SELECT p.id, pt.depth + 1
                FROM playlists p
                JOIN playlist_tree pt ON p.parent_id = pt.id
                WHERE p.room_id = $2
            )
            SELECT id AS "id!: PlaylistId", MAX(depth) AS depth
            FROM playlist_tree
            GROUP BY id
            ORDER BY MAX(depth) DESC, id"#,
            id.as_i64(),
            room_id.as_i64()
        )
        .fetch_all(&mut *tx)
        .await?;

        let mut ids_by_depth = BTreeMap::<i32, Vec<PlaylistId>>::new();
        let mut playlist_ids = Vec::with_capacity(rows.len());
        for row in rows {
            let playlist_id = row.id;
            let depth = row.depth.unwrap_or_default();
            playlist_ids.push(playlist_id);
            ids_by_depth.entry(depth).or_default().push(playlist_id);
        }

        if !playlist_ids.is_empty() {
            let playlist_ids_raw: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
            sqlx::query!(
                "DELETE FROM media WHERE room_id = $1 AND playlist_id = ANY($2)",
                room_id as RoomId,
                &playlist_ids_raw,
            )
            .execute(&mut *tx)
            .await?;
        }

        for (_depth, ids) in ids_by_depth.into_iter().rev() {
            let ids_raw: Vec<i64> = ids.iter().map(PlaylistId::as_i64).collect();
            sqlx::query!("DELETE FROM playlists WHERE id = ANY($1)", &ids_raw,)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    /// Delete a playlist subtree scoped to a room.
    pub async fn delete_in_room(&self, room_id: &RoomId, id: &PlaylistId) -> Result<bool> {
        let mut tx = self.pool.begin().await?;

        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM playlists WHERE room_id = $1 AND id = $2) AS "exists!""#,
            room_id.as_i64(),
            id.as_i64()
        )
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Ok(false);
        }

        let rows = sqlx::query!(
            r#"WITH RECURSIVE playlist_tree AS (
                SELECT id, 0 AS depth
                FROM playlists
                WHERE room_id = $1 AND id = $2
                UNION ALL
                SELECT p.id, pt.depth + 1
                FROM playlists p
                JOIN playlist_tree pt ON p.parent_id = pt.id
                WHERE p.room_id = $1
            )
            SELECT id AS "id!: PlaylistId", MAX(depth) AS depth
            FROM playlist_tree
            GROUP BY id
            ORDER BY MAX(depth) DESC, id"#,
            room_id.as_i64(),
            id.as_i64()
        )
        .fetch_all(&mut *tx)
        .await?;

        let mut ids_by_depth = BTreeMap::<i32, Vec<PlaylistId>>::new();
        let mut playlist_ids = Vec::with_capacity(rows.len());
        for row in rows {
            let playlist_id = row.id;
            let depth = row.depth.unwrap_or_default();
            playlist_ids.push(playlist_id);
            ids_by_depth.entry(depth).or_default().push(playlist_id);
        }

        if !playlist_ids.is_empty() {
            let playlist_ids_raw: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
            sqlx::query!(
                "DELETE FROM media WHERE room_id = $1 AND playlist_id = ANY($2)",
                room_id.as_i64(),
                &playlist_ids_raw
            )
            .execute(&mut *tx)
            .await?;
        }

        for (_depth, ids) in ids_by_depth.into_iter().rev() {
            let ids_raw: Vec<i64> = ids.iter().map(PlaylistId::as_i64).collect();
            sqlx::query!(
                "DELETE FROM playlists WHERE room_id = $1 AND id = ANY($2)",
                room_id.as_i64(),
                &ids_raw
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    /// Delete playlists by IDs using a provided executor (for transaction support)
    pub async fn delete_batch_with_executor<'e, E>(
        &self,
        playlist_ids: &[PlaylistId],
        executor: E,
    ) -> Result<usize>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if playlist_ids.is_empty() {
            return Ok(0);
        }

        let id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
        let result = sqlx::query!("DELETE FROM playlists WHERE id = ANY($1)", &id_strs,)
            .execute(executor)
            .await?;

        Ok(usize::try_from(result.rows_affected()).unwrap_or(usize::MAX))
    }

    /// Convert database row to Playlist
    /// Get playlist path from a given node to root using a recursive CTE (single query)
    pub async fn get_path(&self, playlist_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            WITH RECURSIVE ancestors AS (
                SELECT id, room_id, creator_id, name, description,
                       cover_file_reference_id,
                       parent_id, position,
                       source_provider, source_config, NULLIF(provider_instance_name, '') AS provider_instance_name,
                       created_at, updated_at, version, 0 AS depth
                FROM playlists
                WHERE id = $1
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.description,
                       p.cover_file_reference_id,
                       p.parent_id, p.position,
                       p.source_provider, p.source_config, NULLIF(p.provider_instance_name, '') AS provider_instance_name,
                       p.created_at, p.updated_at, p.version, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id
                WHERE a.depth < 50
            )
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM ancestors
            ORDER BY depth DESC
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(playlist_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get playlist path (breadcrumbs), scoped to a room.
    pub async fn get_path_in_room(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<Playlist>> {
        let sql = format!(
            r"
            WITH RECURSIVE ancestors AS (
                SELECT id, room_id, creator_id, name, description,
                       cover_file_reference_id,
                       parent_id, position,
                       source_provider, source_config, NULLIF(provider_instance_name, '') AS provider_instance_name,
                       created_at, updated_at, version, 0 AS depth
                FROM playlists
                WHERE room_id = $1 AND id = $2
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.description,
                       p.cover_file_reference_id,
                       p.parent_id, p.position,
                       p.source_provider, p.source_config, NULLIF(p.provider_instance_name, '') AS provider_instance_name,
                       p.created_at, p.updated_at, p.version, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id AND p.room_id = a.room_id
                WHERE a.depth < 50
            )
            SELECT {PLAYLIST_ROW_COLUMNS}
            FROM ancestors
            ORDER BY depth DESC
            "
        );
        let rows = sqlx::query_as::<_, PlaylistRow>(&sql)
            .bind(room_id.as_i64())
            .bind(playlist_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ProviderInstance;
    use crate::repository::ProviderInstanceRepository;
    use sqlx::Execute;
    use synctv_core_testing::create_test_pool;

    async fn insert_test_provider_instance(pool: &PgPool, name: &str, provider: &str) {
        let now = chrono::Utc::now();
        let instance = ProviderInstance {
            name: name.to_string(),
            endpoint: "http://localhost:50051".to_string(),
            comment: Some("test provider instance".to_string()),
            jwt_secret: None,
            custom_ca: None,
            timeout: "10s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec![provider.to_string()],
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        ProviderInstanceRepository::new(pool.clone())
            .create(&instance)
            .await
            .unwrap();
    }

    fn assert_position_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "expected position {expected}, got {actual}"
        );
    }

    /// Unit test: Repository constructor is const
    #[test]
    fn test_repository_new() {
        fn _assert_const_new(pool: PgPool) -> PlaylistRepository {
            PlaylistRepository::new(pool)
        }
        // Compilation test only - cannot create PgPool without database
    }

    #[test]
    fn test_advisory_lock_key_deterministic() {
        let room_id = RoomId::expect_positive(80_001);
        let parent_id = PlaylistId::expect_positive(80_002);
        let key1 = PlaylistRepository::scope_lock_key(&room_id, Some(&parent_id));
        let key2 = PlaylistRepository::scope_lock_key(&room_id, Some(&parent_id));
        assert_eq!(key1, key2, "Lock key should be deterministic");
    }

    #[test]
    fn test_advisory_lock_key_different() {
        let room1 = RoomId::expect_positive(80_003);
        let room2 = RoomId::expect_positive(80_004);
        let parent1 = PlaylistId::expect_positive(80_005);
        let parent2 = PlaylistId::expect_positive(80_006);
        let key_room1_parent1 = PlaylistRepository::scope_lock_key(&room1, Some(&parent1));
        let key_room1_parent2 = PlaylistRepository::scope_lock_key(&room1, Some(&parent2));
        let key_room2_parent1 = PlaylistRepository::scope_lock_key(&room2, Some(&parent1));
        let key_room2_none = PlaylistRepository::scope_lock_key(&room2, None);

        assert_ne!(key_room1_parent1, key_room2_parent1);
        assert_ne!(key_room1_parent1, key_room1_parent2);
        assert_ne!(key_room2_parent1, key_room2_none);
    }

    #[test]
    fn test_advisory_lock_key_range() {
        let test_ids = [1, 42, 80_007, i64::from(i32::MAX), i64::MAX / 2];

        for id in test_ids {
            let room_id = RoomId::expect_positive(id);
            let parent_id = PlaylistId::expect_positive(id);
            let key = PlaylistRepository::scope_lock_key(&room_id, Some(&parent_id));
            assert!(key >= 0, "Lock key should be non-negative for id: {id}");
        }
    }

    #[test]
    fn test_normalize_provider_instance_name_for_db() {
        assert_eq!(
            PlaylistRepository::normalize_provider_instance_name_for_db(None),
            None
        );
        assert_eq!(
            PlaylistRepository::normalize_provider_instance_name_for_db(Some("")),
            None
        );
        assert_eq!(
            PlaylistRepository::normalize_provider_instance_name_for_db(Some("   ")),
            None
        );
        assert_eq!(
            PlaylistRepository::normalize_provider_instance_name_for_db(Some("alist_home")),
            Some("alist_home")
        );
        assert_eq!(
            PlaylistRepository::normalize_provider_instance_name_for_db(Some("  alist_home  ")),
            Some("alist_home")
        );
    }

    #[test]
    fn test_push_playlist_scope_filters_treats_empty_provider_instance_as_default() {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT p.id FROM playlists p");
        let query = PlaylistListQuery {
            provider_instance_name: Some("   ".to_string()),
            ..PlaylistListQuery::default()
        };
        let room_id = RoomId::expect_positive(80_008);

        PlaylistRepository::push_playlist_scope_filters(&mut builder, &room_id, None, &query)
            .unwrap();

        let built = builder.build();
        assert!(built
            .sql()
            .contains("NULLIF(p.provider_instance_name, '') IS NULL"));
    }

    /// Integration test: Create and get playlist by ID
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_and_get_by_id() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("playlist_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Playlist Test Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create top-level playlist
        let playlist = PlaylistFixture::new()
            .with_room_id(room.id)
            .with_name("Top Level")
            .build();
        let created = playlist_repo.create(&playlist).await.unwrap();

        assert!(created.is_top_level());
        assert_position_eq(created.position, 0.0);

        // Get by ID
        let fetched = playlist_repo.get_by_id(&created.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, created.id);
        assert!(fetched.is_top_level());
    }

    /// Integration test: Get top-level playlists for a room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_top_level_playlists() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("top_level_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Top Level Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        let top_level = PlaylistFixture::new()
            .with_room_id(room.id)
            .with_name("Top Level")
            .build();
        let created = playlist_repo.create(&top_level).await.unwrap();

        let fetched = playlist_repo.get_top_level(&room.id).await.unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].id, created.id);
        assert!(fetched[0].is_top_level());
    }

    /// Integration test: blank provider instance name is stored as the default binding.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_normalizes_blank_provider_instance_name_to_default_binding() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("playlist_default_provider_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Default Provider Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        let mut playlist = PlaylistFixture::new()
            .with_room_id(room.id)
            .with_name("Dynamic Default Provider")
            .build();
        playlist.source_provider = Some("alist".to_string());
        playlist.source_config = Some(serde_json::json!({ "path": "/movies" }));
        playlist.provider_instance_name = Some("   ".to_string());

        let created = playlist_repo.create(&playlist).await.unwrap();
        assert!(created.provider_instance_name.is_none());

        let stored = sqlx::query_scalar_unchecked!(
            "SELECT provider_instance_name FROM playlists WHERE id = $1",
            created.id.as_i64()
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(stored.is_none());

        let fetched = playlist_repo.get_by_id(&created.id).await.unwrap().unwrap();
        assert!(fetched.provider_instance_name.is_none());
    }

    /// Integration test: empty provider-instance filter matches default dynamic playlists.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_filtered_by_parent_matches_default_provider_instance_name() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("playlist_default_provider_filter_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Default Provider Filter Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        let mut default_provider_playlist = PlaylistFixture::new()
            .with_room_id(room.id)
            .with_name("Default Provider Playlist")
            .with_creator(owner.id)
            .build();
        default_provider_playlist.source_provider = Some("alist".to_string());
        default_provider_playlist.source_config = Some(serde_json::json!({ "path": "/default" }));
        default_provider_playlist.provider_instance_name = None;
        let default_provider_playlist = playlist_repo
            .create(&default_provider_playlist)
            .await
            .unwrap();

        let mut explicit_provider_playlist = PlaylistFixture::new()
            .with_room_id(room.id)
            .with_name("Explicit Provider Playlist")
            .with_creator(owner.id)
            .build();
        explicit_provider_playlist.source_provider = Some("alist".to_string());
        explicit_provider_playlist.source_config = Some(serde_json::json!({ "path": "/explicit" }));
        explicit_provider_playlist.provider_instance_name = Some("alist_home".to_string());
        insert_test_provider_instance(&pool, "alist_home", "alist").await;
        let _explicit_provider_playlist = playlist_repo
            .create(&explicit_provider_playlist)
            .await
            .unwrap();

        let query = PlaylistListQuery {
            source_provider: Some("alist".to_string()),
            provider_instance_name: Some(String::new()),
            dynamic_only: Some(true),
            ..PlaylistListQuery::default()
        };

        let total = playlist_repo
            .count_filtered_by_parent(&room.id, None, &query)
            .await
            .unwrap();
        assert_eq!(total, 1);

        let rows = playlist_repo
            .list_filtered_by_parent(&room.id, None, &query, 50, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].playlist.id, default_provider_playlist.id);
        assert!(rows[0].playlist.provider_instance_name.is_none());
    }

    /// Integration test: Get playlists by room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_by_room() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("room_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create top-level playlist
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create child playlists
        let child1 = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Child 1")
            .build();
        let created_child1 = playlist_repo.create(&child1).await.unwrap();

        let child2 = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Child 2")
            .build();
        let created_child2 = playlist_repo.create(&child2).await.unwrap();

        // Get all playlists for room
        let playlists = playlist_repo.get_by_room(&room.id).await.unwrap();
        assert_eq!(playlists.len(), 3);

        // Verify root comes first (NULLS FIRST in ORDER BY)
        assert!(playlists[0].is_top_level());
        assert_eq!(playlists[0].id, created_root.id);

        // Children should be sorted by position
        let child_ids: Vec<_> = playlists[1..].iter().map(|p| p.id).collect();
        assert!(child_ids.contains(&created_child1.id));
        assert!(child_ids.contains(&created_child2.id));
    }

    /// Integration test: Update playlist with optimistic locking
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_with_current_version() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("update_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Update Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root and child
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Original Name")
            .build();
        let created = playlist_repo.create(&child).await.unwrap();

        // Update playlist
        let mut updated = created.clone();
        updated.name = "Updated Name".to_string();
        updated.position = 5.0;
        updated.source_provider = Some("alist".to_string());
        updated.source_config = Some(serde_json::json!({"path": "/changed"}));
        updated.provider_instance_name = Some("changed-instance".to_string());

        let result = playlist_repo
            .update_with_version(&updated, created.version)
            .await
            .unwrap();
        assert_eq!(result.name, "Updated Name");
        assert_position_eq(result.position, 5.0);
        assert_eq!(result.source_provider, created.source_provider);
        assert_eq!(result.source_config, created.source_config);
        assert_eq!(
            result.provider_instance_name,
            created.provider_instance_name
        );
        assert!(result.version > created.version); // Version should increment
    }

    /// Integration test: Update with version (optimistic locking)
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_with_version() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("version_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Version Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root and child
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Test Playlist")
            .build();
        let created = playlist_repo.create(&child).await.unwrap();
        let original_version = created.version;

        // Update with correct version
        let mut updated = created.clone();
        updated.name = "Updated".to_string();
        let result = playlist_repo
            .update_with_version(&updated, original_version)
            .await
            .unwrap();
        assert_eq!(result.name, "Updated");

        // Update with stale version should fail
        let mut stale = created.clone();
        stale.name = "Stale Update".to_string();
        let result = playlist_repo
            .update_with_version(&stale, original_version) // Old version
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::Error::OptimisticLockConflict
        ));
    }

    /// Integration test: Delete playlist
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("delete_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Delete Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root and child
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("To Delete")
            .build();
        let created = playlist_repo.create(&child).await.unwrap();

        // Delete child
        let deleted = playlist_repo.delete(&created.id).await.unwrap();
        assert!(deleted);

        // Verify deleted
        let fetched = playlist_repo.get_by_id(&created.id).await.unwrap();
        assert!(fetched.is_none());

        // Delete non-existent returns false
        let deleted_again = playlist_repo.delete(&created.id).await.unwrap();
        assert!(!deleted_again);
    }

    /// Integration test: Delete playlist subtree scoped to a room.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_in_room() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("delete_in_room_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Delete In Room Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        let other_room = RoomFixture::new()
            .with_name("Delete In Room Other Room")
            .with_owner(owner.id)
            .build();
        let other_room = room_repo.create(&other_room).await.unwrap();

        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let root = playlist_repo.create(&root).await.unwrap();
        let child = PlaylistFixture::new_child(root.id)
            .with_room_id(room.id)
            .with_name("Scoped Child")
            .build();
        let child = playlist_repo.create(&child).await.unwrap();

        let wrong_room_deleted = playlist_repo
            .delete_in_room(&other_room.id, &child.id)
            .await
            .unwrap();
        assert!(!wrong_room_deleted);

        let deleted = playlist_repo
            .delete_in_room(&room.id, &child.id)
            .await
            .unwrap();
        assert!(deleted);

        let fetched = playlist_repo.get_by_id(&child.id).await.unwrap();
        assert!(fetched.is_none());
    }

    /// Integration test: Delete removes descendant playlists too.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_cascades() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("cascade_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Cascade Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root, child, grandchild
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Child")
            .build();
        let created_child = playlist_repo.create(&child).await.unwrap();

        let grandchild = PlaylistFixture::new_child(created_child.id)
            .with_room_id(room.id)
            .with_name("Grandchild")
            .build();
        let created_grandchild = playlist_repo.create(&grandchild).await.unwrap();

        // Delete child - the whole subtree should be removed.
        let deleted = playlist_repo.delete(&created_child.id).await.unwrap();
        assert!(deleted);

        // Grandchild should also be deleted
        let fetched = playlist_repo
            .get_by_id(&created_grandchild.id)
            .await
            .unwrap();
        assert!(fetched.is_none());
    }

    /// Integration test: Append helper returns sparse floating positions.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_next_append_position_with_tx() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("position_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Position Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let next_pos = playlist_repo
            .get_next_append_position_with_tx(&room.id, Some(&created_root.id), &mut tx)
            .await
            .unwrap();
        assert_position_eq(next_pos, 1024.0);

        // Create children with explicit positions
        for i in 0..3 {
            let child = PlaylistFixture::new_child(created_root.id)
                .with_room_id(room.id)
                .with_name(&format!("Child {i}"))
                .with_position((i + 1) * 1024)
                .build();
            playlist_repo
                .create_with_executor(&child, &mut *tx)
                .await
                .unwrap();
        }

        // Next append position should continue the sparse sequence.
        let next_pos = playlist_repo
            .get_next_append_position_with_tx(&room.id, Some(&created_root.id), &mut tx)
            .await
            .unwrap();
        assert_position_eq(next_pos, 4096.0);
        tx.commit().await.unwrap();
    }

    /// Integration test: Get children
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_children() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("children_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Children Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create 3 children
        for i in 0..3 {
            let child = PlaylistFixture::new_child(created_root.id)
                .with_room_id(room.id)
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Get children
        let children = playlist_repo.get_children(&created_root.id).await.unwrap();
        assert_eq!(children.len(), 3);

        // Should be sorted by position
        let mut expected_position = 0.0;
        for child in &children {
            assert_position_eq(child.position, expected_position);
            expected_position += 1.0;
        }
    }

    /// Integration test: Get children paginated
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_children_paginated() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new().with_username("paginated_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Paginated Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create 15 children
        for i in 0..15 {
            let child = PlaylistFixture::new_child(created_root.id)
                .with_room_id(room.id)
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Page 1 (limit 10, offset 0)
        let page1 = playlist_repo
            .get_children_paginated(&created_root.id, 10, 0)
            .await
            .unwrap();
        assert_eq!(page1.len(), 10);
        assert_eq!(page1[0].name, "Child 0");

        // Page 2 (limit 10, offset 10)
        let page2 = playlist_repo
            .get_children_paginated(&created_root.id, 10, 10)
            .await
            .unwrap();
        assert_eq!(page2.len(), 5);
        assert_eq!(page2[0].name, "Child 10");
    }

    /// Integration test: Count children
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_count_children() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("count_children_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Count Children Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Initially 0 children
        let count = playlist_repo
            .count_children(&created_root.id)
            .await
            .unwrap();
        assert_eq!(count, 0);

        // Create 5 children
        for i in 0..5 {
            let child = PlaylistFixture::new_child(created_root.id)
                .with_room_id(room.id)
                .with_name(&format!("Child {i}"))
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        let count = playlist_repo
            .count_children(&created_root.id)
            .await
            .unwrap();
        assert_eq!(count, 5);
    }

    /// Integration test: Count by room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_count_by_room() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new().with_username("count_room_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Count Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Initially 1 (just root)
        let count = playlist_repo.count_by_room(&room.id).await.unwrap();
        assert_eq!(count, 1);

        // Create children
        for i in 0..3 {
            let child = PlaylistFixture::new_child(created_root.id)
                .with_room_id(room.id)
                .with_name(&format!("Child {i}"))
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        let count = playlist_repo.count_by_room(&room.id).await.unwrap();
        assert_eq!(count, 4); // root + 3 children
    }

    /// Integration test: Get path (breadcrumb)
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_path() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("path_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Path Playlist Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root -> child -> grandchild
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Child")
            .build();
        let created_child = playlist_repo.create(&child).await.unwrap();

        let grandchild = PlaylistFixture::new_child(created_child.id)
            .with_room_id(room.id)
            .with_name("Grandchild")
            .build();
        let created_grandchild = playlist_repo.create(&grandchild).await.unwrap();

        // Get path from grandchild
        let path = playlist_repo
            .get_path(&created_grandchild.id)
            .await
            .unwrap();

        assert_eq!(path.len(), 3);
        // Should be ordered from root to leaf
        assert!(path[0].is_top_level());
        assert_eq!(path[1].id, created_child.id);
        assert_eq!(path[2].id, created_grandchild.id);
    }

    /// Integration test: Get by room paginated
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_by_room_paginated() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new()
            .with_username("room_paginated_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room Paginated Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create 15 children
        for i in 0..15 {
            let child = PlaylistFixture::new_child(created_root.id)
                .with_room_id(room.id)
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Total 16 playlists (root + 15 children)
        let count = playlist_repo.count_by_room(&room.id).await.unwrap();
        assert_eq!(count, 16);

        // Page 1 (limit 10, offset 0)
        let page1 = playlist_repo
            .get_by_room_paginated(&room.id, 10, 0)
            .await
            .unwrap();
        assert_eq!(page1.len(), 10);

        // Page 2 (limit 10, offset 10)
        let page2 = playlist_repo
            .get_by_room_paginated(&room.id, 10, 10)
            .await
            .unwrap();
        assert_eq!(page2.len(), 6);
    }

    /// Integration test: Create with executor preserves explicit sparse positions.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_with_executor_preserves_position() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());

        let owner = UserFixture::new().with_username("executor_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Executor Room")
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child_explicit = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name("Explicit Child")
            .with_position(2048)
            .build();

        let result = playlist_repo
            .create_with_executor(&child_explicit, &pool)
            .await;
        let created = result.expect("create with executor should succeed");
        assert_position_eq(created.position, 2048.0);
    }
}
