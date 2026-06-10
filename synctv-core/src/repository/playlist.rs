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
    Error, Result,
};
use sqlx::PgPool;
use std::collections::BTreeMap;

fn count_value(value: Option<i64>, query_description: &str) -> Result<i64> {
    value.ok_or_else(|| {
        crate::Error::Internal(format!(
            "{query_description} COUNT query returned no scalar value"
        ))
    })
}

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

impl TryFrom<PlaylistRow> for Playlist {
    type Error = crate::Error;

    fn try_from(row: PlaylistRow) -> Result<Self> {
        Ok(Self {
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
                .map_err(crate::Error::InvalidInput)?,
            source_config: row.source_config,
            provider_instance_name: row.provider_instance_name,
            created_at: row.created_at,
            updated_at: row.updated_at,
            version: row.version,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PlaylistListRow {
    id: PlaylistId,
    room_id: RoomId,
    creator_id: Option<crate::models::UserId>,
    name: String,
    description: String,
    cover_file_reference_id: Option<i64>,
    parent_id: Option<PlaylistId>,
    position: f64,
    source_provider: Option<ProviderTypeName>,
    source_config: Option<serde_json::Value>,
    provider_instance_name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
    is_available: bool,
}

impl From<PlaylistListRow> for PlaylistListItem {
    fn from(row: PlaylistListRow) -> Self {
        Self {
            playlist: Playlist {
                id: row.id,
                room_id: row.room_id,
                creator_id: row.creator_id,
                name: row.name,
                description: row.description,
                cover_file_reference_id: row.cover_file_reference_id,
                parent_id: row.parent_id,
                position: row.position,
                source_provider: row.source_provider.map(|provider| provider.0),
                source_config: row.source_config,
                provider_instance_name: row.provider_instance_name,
                created_at: row.created_at,
                updated_at: row.updated_at,
                version: row.version,
            },
            is_available: row.is_available,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlaylistListItem {
    pub playlist: Playlist,
    pub is_available: bool,
}

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

    fn push_playlist_list_order_by(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        query: &PlaylistListQuery,
    ) {
        use crate::models::{PlaylistListSortBy, SortDirection};

        let order_by = match (query.sort_by, query.sort_direction) {
            (PlaylistListSortBy::Name, SortDirection::Asc) => {
                " ORDER BY p.name ASC, p.position ASC, p.id ASC"
            }
            (PlaylistListSortBy::Name, SortDirection::Desc) => {
                " ORDER BY p.name DESC, p.position DESC, p.id DESC"
            }
            (PlaylistListSortBy::CreatedAt, SortDirection::Asc) => {
                " ORDER BY p.created_at ASC, p.position ASC, p.id ASC"
            }
            (PlaylistListSortBy::CreatedAt, SortDirection::Desc) => {
                " ORDER BY p.created_at DESC, p.position DESC, p.id DESC"
            }
            (PlaylistListSortBy::UpdatedAt, SortDirection::Asc) => {
                " ORDER BY p.updated_at ASC, p.position ASC, p.id ASC"
            }
            (PlaylistListSortBy::UpdatedAt, SortDirection::Desc) => {
                " ORDER BY p.updated_at DESC, p.position DESC, p.id DESC"
            }
            (PlaylistListSortBy::Position, SortDirection::Asc) => {
                " ORDER BY p.position ASC, p.name ASC, p.id ASC"
            }
            (PlaylistListSortBy::Position, SortDirection::Desc) => {
                " ORDER BY p.position DESC, p.name DESC, p.id DESC"
            }
        };
        builder.push(order_by);
    }

    fn provider_type_code(provider: &str) -> Result<i16> {
        provider_type_code_from_name(provider).map_err(crate::Error::InvalidInput)
    }

    fn push_playlist_scope_filters(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
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
        Self::push_playlist_list_order_by(&mut builder, query);
        builder.push(" LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder
            .build_query_as::<PlaylistListRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(PlaylistListItem::from).collect())
    }

    /// Get playlist by ID
    pub async fn get_by_id(&self, id: &PlaylistId) -> Result<Option<Playlist>> {
        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists
            WHERE id = $1
            "#,
            id as &PlaylistId,
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    /// Get playlist by ID, scoped to a room.
    pub async fn get_by_room_and_id(
        &self,
        room_id: &RoomId,
        id: &PlaylistId,
    ) -> Result<Option<Playlist>> {
        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists
            WHERE room_id = $1 AND id = $2
            "#,
            room_id as &RoomId,
            id as &PlaylistId,
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
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
        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "#,
            room_id as &RoomId,
            id as &PlaylistId,
        )
        .fetch_optional(executor)
        .await?;

        row.map(TryInto::try_into).transpose()
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
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists
            WHERE id = ANY($1)
            "#,
            &id_strs,
        )
        .fetch_all(executor)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
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
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1 AND id = ANY($2)
            "#,
            room_id as &RoomId,
            &id_strs,
        )
        .fetch_all(executor)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Get top-level playlists in a room.
    pub async fn get_top_level(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL
            ORDER BY position ASC
            "#,
            room_id as &RoomId,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
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

        count_value(count, "top-level playlist")
    }

    /// Get paginated top-level playlists in a room.
    pub async fn get_top_level_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL
            ORDER BY position ASC
            LIMIT $2 OFFSET $3
            "#,
            room_id as &RoomId,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Get children playlists of a parent
    pub async fn get_children(&self, parent_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            "#,
            parent_id as &PlaylistId,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
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

        count_value(count, "child playlist")
    }

    /// Get count of children playlists for a parent, scoped to a room.
    pub async fn count_children_in_room(
        &self,
        room_id: &RoomId,
        parent_id: &PlaylistId,
    ) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!" FROM playlists WHERE room_id = $1 AND parent_id = $2
            "#,
            room_id.as_i64(),
            parent_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Get paginated children playlists for a parent.
    pub async fn get_children_paginated(
        &self,
        parent_id: &PlaylistId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            LIMIT $2 OFFSET $3
            "#,
            parent_id as &PlaylistId,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Get all playlists in a room (tree structure)
    pub async fn get_by_room(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            "#,
            room_id as &RoomId,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
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

        count_value(count, "room playlist")
    }

    /// Get paginated playlists in a room
    pub async fn get_by_room_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            LIMIT $2 OFFSET $3
            "#,
            room_id as &RoomId,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
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

        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            INSERT INTO playlists (room_id, creator_id, name, description,
                                   cover_file_reference_id,
                                   parent_id, position, source_provider, source_config, provider_instance_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING id as "id: PlaylistId",
                      room_id as "room_id: RoomId",
                      creator_id as "creator_id: crate::models::UserId",
                      name,
                      description,
                      cover_file_reference_id,
                      parent_id as "parent_id: PlaylistId",
                      position,
                      source_provider,
                      source_config,
                      NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                      created_at,
                      updated_at,
                      version
            "#,
            playlist.room_id as RoomId,
            playlist
                .creator_id
                .as_ref()
                .map(crate::models::UserId::as_i64),
            playlist.name,
            playlist.description,
            playlist.cover_file_reference_id,
            parent_id.as_ref().map(PlaylistId::as_i64),
            playlist.position,
            source_provider_code,
            playlist.source_config.as_ref(),
            Self::normalize_provider_instance_name_for_db(
                playlist.provider_instance_name.as_deref(),
            ),
        )
        .fetch_one(executor)
        .await?;

        row.try_into()
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
        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            UPDATE playlists
            SET name = $2, description = $3,
                cover_file_reference_id = $4,
                position = $5,
                version = version + 1
            WHERE id = $1 AND version = $6
            RETURNING id as "id: PlaylistId",
                      room_id as "room_id: RoomId",
                      creator_id as "creator_id: crate::models::UserId",
                      name,
                      description,
                      cover_file_reference_id,
                      parent_id as "parent_id: PlaylistId",
                      position,
                      source_provider,
                      source_config,
                      NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                      created_at,
                      updated_at,
                      version
            "#,
            playlist.id as PlaylistId,
            playlist.name,
            playlist.description,
            playlist.cover_file_reference_id,
            playlist.position,
            expected_version,
        )
        .fetch_optional(executor)
        .await?;

        match row {
            Some(row) => row.try_into(),
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

        let moved: Playlist = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "#,
            room_id as &RoomId,
            playlist_id as &PlaylistId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .map(TryInto::try_into)
        .transpose()?
        .ok_or_else(|| crate::Error::NotFound("Playlist not found".to_string()))?;

        let anchor: Playlist = sqlx::query_as!(
            PlaylistRow,
            r#"
            SELECT id as "id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name,
                   description,
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "#,
            room_id as &RoomId,
            anchor_id as &PlaylistId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .map(TryInto::try_into)
        .transpose()?
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
                let row = sqlx::query_as!(
                    PlaylistRow,
                    r#"
                    UPDATE playlists
                    SET position = $2, version = version + 1
                    WHERE id = $1
                    RETURNING id as "id: PlaylistId",
                              room_id as "room_id: RoomId",
                              creator_id as "creator_id: crate::models::UserId",
                              name,
                              description,
                              cover_file_reference_id,
                              parent_id as "parent_id: PlaylistId",
                              position,
                              source_provider,
                              source_config,
                              NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                              created_at,
                              updated_at,
                              version
                    "#,
                    moved.id as PlaylistId,
                    position,
                )
                .fetch_one(&mut **tx)
                .await?;

                return row.try_into();
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
            let depth = row.depth.ok_or_else(|| {
                Error::Internal("playlist tree query did not return depth".into())
            })?;
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
            let depth = row.depth.ok_or_else(|| {
                Error::Internal("playlist tree query did not return depth".into())
            })?;
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

        usize::try_from(result.rows_affected())
            .map_err(|_| Error::Internal("deleted playlist count exceeds usize::MAX".to_string()))
    }

    /// Get playlist path from a given node to root using a recursive CTE (single query)
    pub async fn get_path(&self, playlist_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
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
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM ancestors
            ORDER BY depth DESC
            "#,
            playlist_id as &PlaylistId,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Get playlist path (breadcrumbs), scoped to a room.
    pub async fn get_path_in_room(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
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
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM ancestors
            ORDER BY depth DESC
            "#,
            room_id as &RoomId,
            playlist_id as &PlaylistId,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }
}

#[cfg(test)]
#[path = "playlist_tests.rs"]
mod tests;
