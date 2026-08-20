//! Playlist repository for database operations
//!
//! Design reference: external design doc 04-database-design.md §2.4.1

use super::{query_builder::ilike_contains_pattern, required_count, sqlx_types::ProviderTypeName};
use crate::{
    models::{
        normalize_provider_instance_name, Playlist, PlaylistBrowseAccessMode, PlaylistId,
        PlaylistListQuery, PlaylistSourceConfig, RoomId, SourceProvider,
    },
    Result,
};
use sqlx::PgPool;

#[derive(Debug, sqlx::FromRow)]
struct PlaylistRow {
    id: PlaylistId,
    room_id: RoomId,
    creator_id: Option<crate::models::UserId>,
    browse_access_mode: i16,
    name: String,
    description: String,
    cover_file_reference_id: Option<i64>,
    parent_id: Option<PlaylistId>,
    position: f64,
    source_provider: Option<i16>,
    source_config: Option<PlaylistSourceConfig>,
    provider_instance_name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
}

impl TryFrom<PlaylistRow> for Playlist {
    type Error = crate::Error;

    fn try_from(row: PlaylistRow) -> Result<Self> {
        let source_provider = row
            .source_provider
            .map(SourceProvider::try_from)
            .transpose()
            .map_err(crate::Error::InvalidInput)?;
        let source_config = match (source_provider, row.source_config) {
            (Some(provider), Some(config)) => {
                Some(config.ensure_provider(provider).map_err(|error| {
                    crate::Error::InvalidInput(format!(
                        "Invalid persisted playlist source_config for {}: {error}",
                        row.id
                    ))
                })?)
            }
            (None | Some(_), None) => None,
            (None, Some(_)) => {
                return Err(crate::Error::InvalidInput(format!(
                    "Playlist {} has source_config without source_provider",
                    row.id
                )));
            }
        };
        Ok(Self {
            id: row.id,
            room_id: row.room_id,
            creator_id: row.creator_id,
            browse_access_mode: PlaylistBrowseAccessMode::try_from(row.browse_access_mode)
                .map_err(crate::Error::InvalidInput)?,
            name: row.name,
            description: row.description,
            cover_file_reference_id: row.cover_file_reference_id,
            parent_id: row.parent_id,
            position: row.position,
            source_provider,
            source_config,
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
    browse_access_mode: i16,
    name: String,
    description: String,
    cover_file_reference_id: Option<i64>,
    parent_id: Option<PlaylistId>,
    position: f64,
    source_provider: Option<ProviderTypeName>,
    source_config: Option<PlaylistSourceConfig>,
    provider_instance_name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
    is_available: bool,
}

impl TryFrom<PlaylistListRow> for PlaylistListItem {
    type Error = crate::Error;

    fn try_from(row: PlaylistListRow) -> Result<Self> {
        let source_provider = row.source_provider.map(|provider| provider.0);
        let source_config = match (source_provider, row.source_config) {
            (Some(provider), Some(config)) => {
                Some(config.ensure_provider(provider).map_err(|error| {
                    crate::Error::InvalidInput(format!(
                        "Invalid persisted playlist source_config for {}: {error}",
                        row.id
                    ))
                })?)
            }
            (None | Some(_), None) => None,
            (None, Some(_)) => {
                return Err(crate::Error::InvalidInput(format!(
                    "Playlist {} has source_config without source_provider",
                    row.id
                )));
            }
        };
        Ok(Self {
            playlist: Playlist {
                id: row.id,
                room_id: row.room_id,
                creator_id: row.creator_id,
                browse_access_mode: PlaylistBrowseAccessMode::try_from(row.browse_access_mode)
                    .map_err(crate::Error::InvalidInput)?,
                name: row.name,
                description: row.description,
                cover_file_reference_id: row.cover_file_reference_id,
                parent_id: row.parent_id,
                position: row.position,
                source_provider,
                source_config,
                provider_instance_name: row.provider_instance_name,
                created_at: row.created_at,
                updated_at: row.updated_at,
                version: row.version,
            },
            is_available: row.is_available,
        })
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

    fn push_playlist_scope_filters(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        query: &PlaylistListQuery,
    ) -> Result<()> {
        builder.push(" FROM playlists p LEFT JOIN users u ON p.creator_id = u.id AND u.deleted_at IS NULL WHERE p.room_id = ");
        builder.push_bind(room_id.as_i64());
        builder.push(" AND p.deleted_at IS NULL");
        builder.push(
            " AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)",
        );
        builder.push(" AND (p.parent_id IS NULL OR EXISTS (SELECT 1 FROM playlists parent WHERE parent.id = p.parent_id AND parent.deleted_at IS NULL))");
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
            if let Some(pattern) = ilike_contains_pattern(search) {
                builder.push(" AND (p.name ILIKE ");
                builder.push_bind(pattern.clone());
                builder.push(" ESCAPE '\\' OR p.description ILIKE ");
                builder.push_bind(pattern);
                builder.push(" ESCAPE '\\')");
            }
        }
        if let Some(source_provider) = &query.source_provider {
            builder.push(" AND p.source_provider = ");
            builder.push_bind(source_provider.as_i16());
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
                    " AND (p.source_provider IS NULL OR p.creator_id IS NULL OR (u.id IS NOT NULL AND NOT EXISTS (
                    SELECT 1 FROM user_bans ub
                    WHERE ub.user_id = u.id
                      AND ub.revoked_at IS NULL
                      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ) AND EXISTS (
                    SELECT 1 FROM room_members rm
                    WHERE rm.room_id = p.room_id AND rm.user_id = p.creator_id
                ) AND NOT EXISTS (
                    SELECT 1 FROM room_member_kick_cooldowns cooldown
                    WHERE cooldown.room_id = p.room_id
                      AND cooldown.user_id = p.creator_id
                      AND cooldown.ends_at > CURRENT_TIMESTAMP
                )))",
                );
            }
            Some(false) => {
                builder.push(
                    " AND p.source_provider IS NOT NULL AND p.creator_id IS NOT NULL AND (u.id IS NULL OR EXISTS (
                    SELECT 1 FROM user_bans ub
                    WHERE ub.user_id = u.id
                      AND ub.revoked_at IS NULL
                      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ) OR NOT EXISTS (
                    SELECT 1 FROM room_members rm
                    WHERE rm.room_id = p.room_id AND rm.user_id = p.creator_id
                ) OR EXISTS (
                    SELECT 1 FROM room_member_kick_cooldowns cooldown
                    WHERE cooldown.room_id = p.room_id
                      AND cooldown.user_id = p.creator_id
                      AND cooldown.ends_at > CURRENT_TIMESTAMP
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
                    p.parent_id, p.position, p.browse_access_mode,
                    p.source_provider, p.source_config, NULLIF(p.provider_instance_name, '') AS provider_instance_name,
                    p.created_at, p.updated_at, p.version,
                    CASE
                      WHEN p.source_provider IS NULL OR p.creator_id IS NULL THEN TRUE
                      WHEN u.id IS NOT NULL AND NOT EXISTS (
                          SELECT 1 FROM user_bans ub
                          WHERE ub.user_id = u.id
                            AND ub.revoked_at IS NULL
                            AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                      ) AND EXISTS (
                          SELECT 1 FROM room_members rm
                          WHERE rm.room_id = p.room_id
                            AND rm.user_id = p.creator_id
                      ) AND NOT EXISTS (
                          SELECT 1 FROM room_member_kick_cooldowns cooldown
                          WHERE cooldown.room_id = p.room_id
                            AND cooldown.user_id = p.creator_id
                            AND cooldown.ends_at > CURRENT_TIMESTAMP
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
        rows.into_iter().map(PlaylistListItem::try_from).collect()
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
                   browse_access_mode as "browse_access_mode!",
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists
            WHERE id = $1
              AND deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = playlists.room_id AND r.deleted_at IS NULL)
              AND (creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users WHERE users.id = playlists.creator_id AND users.deleted_at IS NULL
              ))
              AND (parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = playlists.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                   browse_access_mode as "browse_access_mode!",
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists
            WHERE room_id = $1
              AND id = $2
              AND deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = playlists.room_id AND r.deleted_at IS NULL)
              AND (creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users WHERE users.id = playlists.creator_id AND users.deleted_at IS NULL
              ))
              AND (parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = playlists.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                   browse_access_mode as "browse_access_mode!",
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists p
            WHERE p.room_id = $1
              AND p.id = $2
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                   browse_access_mode as "browse_access_mode!",
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at as "created_at!",
                   updated_at as "updated_at!",
                   version as "version!"
            FROM playlists p
            WHERE p.id = ANY($1)
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.room_id = $1
              AND p.id = ANY($2)
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.room_id = $1
              AND p.parent_id IS NULL
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            ORDER BY position ASC
            "#,
            room_id as &RoomId,
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.parent_id = $1
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              )
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
            SELECT COUNT(*)
            FROM playlists p
            WHERE p.parent_id = $1
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              )
            ",
            parent_id as &PlaylistId,
        )
        .fetch_one(&self.pool)
        .await?;

        required_count(count, "child playlist")
    }

    /// Get count of children playlists for a parent, scoped to a room.
    pub async fn count_children_in_room(
        &self,
        room_id: &RoomId,
        parent_id: &PlaylistId,
    ) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM playlists p
            WHERE p.room_id = $1
              AND p.parent_id = $2
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              )
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.parent_id = $1
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              )
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.room_id = $1
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
            SELECT COUNT(*)
            FROM playlists p
            WHERE p.room_id = $1
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            ",
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        required_count(count, "room playlist")
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.room_id = $1
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
              AND deleted_at IS NULL
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
              AND deleted_at IS NULL
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
              AND deleted_at IS NULL
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
                "UPDATE playlists SET position = $2, version = version + 1 WHERE id = $1 AND deleted_at IS NULL",
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
        let source_provider_code = playlist.source_provider.map(SourceProvider::as_i16);
        let source_config = match (playlist.source_provider, playlist.source_config.as_ref()) {
            (Some(provider), Some(config)) => {
                if config.provider() != provider {
                    return Err(crate::Error::InvalidInput(format!(
                        "playlist source_config provider '{}' does not match source_provider '{}'",
                        config.provider(),
                        provider
                    )));
                }
                Some(config)
            }
            (None, None) => None,
            (Some(provider), None) => {
                return Err(crate::Error::InvalidInput(format!(
                    "source_config is required for {provider} playlist"
                )));
            }
            (None, Some(_)) => {
                return Err(crate::Error::InvalidInput(
                    "source_provider is required when source_config is present".to_string(),
                ));
            }
        };
        let source_config = source_config.map(sqlx::types::Json);
        let parent_id = playlist.parent_id;

        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            INSERT INTO playlists (room_id, creator_id, name, description,
                                   cover_file_reference_id,
                                   parent_id, position, browse_access_mode, source_provider, source_config, provider_instance_name)
            SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11
            WHERE $2::BIGINT IS NULL OR EXISTS (
                SELECT 1
                FROM users
                WHERE users.id = $2::BIGINT AND users.deleted_at IS NULL
                FOR KEY SHARE
            )
            RETURNING id as "id: PlaylistId",
                      room_id as "room_id: RoomId",
                      creator_id as "creator_id: crate::models::UserId",
                      name,
                      description,
                      cover_file_reference_id,
                      parent_id as "parent_id: PlaylistId",
                      position,
                      browse_access_mode,
                      source_provider,
                      source_config as "source_config: crate::models::PlaylistSourceConfig",
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
            i16::from(playlist.browse_access_mode),
            source_provider_code,
            source_config as _,
            normalize_provider_instance_name(playlist.provider_instance_name.as_deref()),
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
              AND deleted_at IS NULL
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
        let source_provider_code = playlist.source_provider.map(SourceProvider::as_i16);
        let source_config = match (playlist.source_provider, playlist.source_config.as_ref()) {
            (Some(provider), Some(config)) => {
                if config.provider() != provider {
                    return Err(crate::Error::InvalidInput(format!(
                        "playlist source_config provider '{}' does not match source_provider '{}'",
                        config.provider(),
                        provider
                    )));
                }
                Some(sqlx::types::Json(config))
            }
            (None, None) => None,
            (Some(provider), None) => {
                return Err(crate::Error::InvalidInput(format!(
                    "source_config is required for {provider} playlist"
                )));
            }
            (None, Some(_)) => {
                return Err(crate::Error::InvalidInput(
                    "source_provider is required when source_config is present".to_string(),
                ));
            }
        };
        let row = sqlx::query_as!(
            PlaylistRow,
            r#"
            UPDATE playlists
            SET name = $2, description = $3,
                cover_file_reference_id = $4,
                position = $5,
                source_provider = $6,
                source_config = $7,
                provider_instance_name = $8,
                browse_access_mode = $9,
                version = version + 1
            WHERE id = $1
              AND deleted_at IS NULL
              AND version = $10
              AND (creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = playlists.creator_id AND u.deleted_at IS NULL
              ))
              AND (parent_id IS NULL OR EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = playlists.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            RETURNING id as "id: PlaylistId",
                      room_id as "room_id: RoomId",
                      creator_id as "creator_id: crate::models::UserId",
                      name,
                      description,
                      cover_file_reference_id,
                      parent_id as "parent_id: PlaylistId",
                      position,
                      browse_access_mode,
                      source_provider,
                      source_config as "source_config: crate::models::PlaylistSourceConfig",
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
            source_provider_code,
            source_config as _,
            normalize_provider_instance_name(playlist.provider_instance_name.as_deref()),
            i16::from(playlist.browse_access_mode),
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.room_id = $1
              AND p.id = $2
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                   browse_access_mode,
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   created_at,
                   updated_at,
                   version
            FROM playlists p
            WHERE p.room_id = $1
              AND p.id = $2
              AND p.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
              AND (p.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
              ))
              AND (p.parent_id IS NULL OR EXISTS (
                  SELECT 1 FROM playlists parent
                  WHERE parent.id = p.parent_id
                    AND parent.deleted_at IS NULL
                    AND (parent.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = parent.creator_id AND u.deleted_at IS NULL
                    ))
              ))
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
                "SELECT position FROM playlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
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
                    WHERE id = $1 AND deleted_at IS NULL
                    RETURNING id as "id: PlaylistId",
                              room_id as "room_id: RoomId",
                              creator_id as "creator_id: crate::models::UserId",
                              name,
                              description,
                              cover_file_reference_id,
                              parent_id as "parent_id: PlaylistId",
                              position,
                              browse_access_mode,
                              source_provider,
                              source_config as "source_config: crate::models::PlaylistSourceConfig",
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

    /// Get playlist path from a given node to root using a recursive CTE (single query)
    pub async fn get_path(&self, playlist_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query_as!(
            PlaylistRow,
            r#"
            WITH RECURSIVE ancestors AS (
                SELECT id, room_id, creator_id, name, description,
                       cover_file_reference_id,
                       parent_id, position, browse_access_mode,
                       source_provider, source_config, NULLIF(provider_instance_name, '') AS provider_instance_name,
                       created_at, updated_at, version, 0 AS depth
                FROM playlists p0
                WHERE p0.id = $1
                  AND p0.deleted_at IS NULL
                  AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p0.room_id AND r.deleted_at IS NULL)
                  AND (p0.creator_id IS NULL OR EXISTS (
                      SELECT 1 FROM users u WHERE u.id = p0.creator_id AND u.deleted_at IS NULL
                  ))
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.description,
                       p.cover_file_reference_id,
                       p.parent_id, p.position, p.browse_access_mode,
                       p.source_provider, p.source_config, NULLIF(p.provider_instance_name, '') AS provider_instance_name,
                       p.created_at, p.updated_at, p.version, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id
                WHERE p.deleted_at IS NULL
                  AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
                  AND (p.creator_id IS NULL OR EXISTS (
                      SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                  ))
                  AND a.depth < 50
            )
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   browse_access_mode as "browse_access_mode!",
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
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
                       parent_id, position, browse_access_mode,
                       source_provider, source_config, NULLIF(provider_instance_name, '') AS provider_instance_name,
                       created_at, updated_at, version, 0 AS depth
                FROM playlists p0
                WHERE p0.room_id = $1
                  AND p0.id = $2
                  AND p0.deleted_at IS NULL
                  AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p0.room_id AND r.deleted_at IS NULL)
                  AND (p0.creator_id IS NULL OR EXISTS (
                      SELECT 1 FROM users u WHERE u.id = p0.creator_id AND u.deleted_at IS NULL
                  ))
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.description,
                       p.cover_file_reference_id,
                       p.parent_id, p.position, p.browse_access_mode,
                       p.source_provider, p.source_config, NULLIF(p.provider_instance_name, '') AS provider_instance_name,
                       p.created_at, p.updated_at, p.version, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id AND p.room_id = a.room_id
                WHERE p.deleted_at IS NULL
                  AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = p.room_id AND r.deleted_at IS NULL)
                  AND (p.creator_id IS NULL OR EXISTS (
                      SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                  ))
                  AND a.depth < 50
            )
            SELECT id as "id!: PlaylistId",
                   room_id as "room_id!: RoomId",
                   creator_id as "creator_id: crate::models::UserId",
                   name as "name!",
                   description as "description!",
                   cover_file_reference_id,
                   parent_id as "parent_id: PlaylistId",
                   position as "position!",
                   browse_access_mode as "browse_access_mode!",
                   source_provider,
                   source_config as "source_config: crate::models::PlaylistSourceConfig",
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
