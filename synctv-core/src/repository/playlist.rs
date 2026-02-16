//! Playlist repository for database operations
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.1

use sqlx::{PgPool, FromRow};
use crate::{
    models::{Playlist, PlaylistId, RoomId},
    Result,
};

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

    /// Get playlist by ID
    pub async fn get_by_id(&self, id: &PlaylistId) -> Result<Option<Playlist>> {
        let row = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at
            FROM playlists
            WHERE id = $1
            "
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Playlist::from_row(&row)?)),
            None => Ok(None),
        }
    }

    /// Get root playlist for a room
    pub async fn get_root_playlist(&self, room_id: &RoomId) -> Result<Playlist> {
        let row = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL AND name = ''
            "
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Get children playlists of a parent
    pub async fn get_children(&self, parent_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            "
        )
        .bind(parent_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Get all playlists in a room (tree structure)
    pub async fn get_by_room(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS LAST, position ASC
            "
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Create a new playlist
    pub async fn create(&self, playlist: &Playlist) -> Result<Playlist> {
        let source_provider_str = playlist.source_provider.as_deref();
        let row = sqlx::query(
            r"
            INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                   source_provider, source_config, provider_instance_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at
            "
        )
        .bind(playlist.id.as_str())
        .bind(playlist.room_id.as_str())
        .bind(playlist.creator_id.as_ref().map(|id| id.as_str()))
        .bind(&playlist.name)
        .bind(playlist.parent_id.as_ref().map(super::super::models::id::PlaylistId::as_str))
        .bind(playlist.position)
        .bind(source_provider_str)
        .bind(&playlist.source_config)
        .bind(&playlist.provider_instance_name)
        .fetch_one(&self.pool)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Create a playlist using a provided executor (pool or transaction)
    pub async fn create_with_executor<'e, E>(&self, playlist: &Playlist, executor: E) -> Result<Playlist>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let source_provider_str = playlist.source_provider.as_deref();
        let row = sqlx::query(
            r"
            INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                   source_provider, source_config, provider_instance_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at
            "
        )
        .bind(playlist.id.as_str())
        .bind(playlist.room_id.as_str())
        .bind(playlist.creator_id.as_ref().map(|id| id.as_str()))
        .bind(&playlist.name)
        .bind(playlist.parent_id.as_ref().map(super::super::models::id::PlaylistId::as_str))
        .bind(playlist.position)
        .bind(source_provider_str)
        .bind(&playlist.source_config)
        .bind(&playlist.provider_instance_name)
        .fetch_one(executor)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Get next available position in a parent
    pub async fn get_next_position(&self, room_id: &RoomId, parent_id: Option<&PlaylistId>) -> Result<i32> {
        let max_pos: Option<i32> = sqlx::query_scalar(
            r"
            SELECT MAX(position)
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
            "
        )
        .bind(room_id.as_str())
        .bind(parent_id.map(super::super::models::id::PlaylistId::as_str))
        .fetch_one(&self.pool)
        .await?;

        Ok(max_pos.unwrap_or(-1) + 1)
    }

    /// Update playlist
    pub async fn update(&self, playlist: &Playlist) -> Result<Playlist> {
        let source_provider_str = playlist.source_provider.as_deref();
        let row = sqlx::query(
            r"
            UPDATE playlists
            SET name = $2, position = $3, source_provider = $4, source_config = $5,
                provider_instance_name = $6
            WHERE id = $1
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at
            "
        )
        .bind(playlist.id.as_str())
        .bind(&playlist.name)
        .bind(playlist.position)
        .bind(source_provider_str)
        .bind(&playlist.source_config)
        .bind(&playlist.provider_instance_name)
        .fetch_one(&self.pool)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Delete playlist (cascade to children and media)
    pub async fn delete(&self, id: &PlaylistId) -> Result<bool> {
        let result = sqlx::query("DELETE FROM playlists WHERE id = $1")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Convert database row to Playlist
    /// Get playlist path from a given node to root using a recursive CTE (single query)
    pub async fn get_path(&self, playlist_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            WITH RECURSIVE ancestors AS (
                SELECT id, room_id, creator_id, name, parent_id, position,
                       source_provider, source_config, provider_instance_name,
                       created_at, updated_at, 0 AS depth
                FROM playlists
                WHERE id = $1
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.parent_id, p.position,
                       p.source_provider, p.source_config, p.provider_instance_name,
                       p.created_at, p.updated_at, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id
                WHERE a.depth < 50
            )
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at
            FROM ancestors
            ORDER BY depth DESC
            "
        )
        .bind(playlist_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|row| Ok(Playlist::from_row(&row)?)).collect()
    }

}
