//! Data cleanup service for periodic maintenance tasks
//!
//! Coordinates cleanup of:
//! - Soft-deleted records (users, rooms, media, playlists, chat messages) past retention period
//! - Expired email auth and registration tokens
//! - Expired media provider credentials
//! - Old notifications
//! - Old chat messages (per-room cap)
//! - Expired room resource events
//! - Stale playback progress rows
//! - Expired and excess playback history rows
//! - Delivered realtime outbox rows
//!
//! Runs as a background task with configurable intervals and retention periods.
//!
//! # Dynamic Settings
//!
//! The `chat_max_messages_per_room` setting can be dynamically configured via
//! `RuntimeSettingsStore`. When a registry is provided, this value is read at
//! runtime on each cleanup cycle, allowing admins to
//! change settings without restarting the service.

use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{cleanup_ops, FileStorageService, LeaderCheck, RuntimeSettingsStore};
use crate::models::DeletionSource;
use crate::service::partitioning::u32_to_i32;
use crate::{InternalExt, Result};

const DEFAULT_ROOM_RESOURCE_EVENT_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;
const DEFAULT_CHAT_MESSAGE_EVENT_RETENTION_SECONDS: u64 = 90 * 24 * 60 * 60;
// Cover the full message count-pruning window so rooms inactive for up to 90 days
// are still trimmed to keep_count.  Must match CHAT_MESSAGE_COUNT_PRUNING_DAYS.
const CHAT_CAP_ACTIVITY_WINDOW_MINUTES: i32 =
    cleanup_ops::CHAT_MESSAGE_COUNT_PRUNING_DAYS * 24 * 60;

/// Configuration for data cleanup retention periods
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Days to retain soft-deleted users before permanent deletion (0 = never purge)
    pub soft_delete_retention_days: u32,
    /// Days to retain soft-deleted rooms before permanent deletion (0 = never purge)
    pub room_soft_delete_retention_days: u32,
    /// Days to retain independently soft-deleted resources before permanent deletion (0 = never purge)
    pub resource_soft_delete_retention_days: u32,
    /// Days to retain expired email auth and registration tokens before deletion (0 = never purge)
    pub expired_token_retention_days: u32,
    /// Hours buffer for expired credential cleanup (prevents race conditions)
    pub expired_credential_buffer_hours: u32,
    /// Days to retain read notifications before deletion (0 = never purge)
    pub notification_retention_days: u32,
    /// Days to retain any notification (read or unread) before deletion (0 = never purge)
    pub notification_max_retention_days: u32,
    /// Maximum chat messages to keep per room (0 = unlimited)
    pub chat_max_messages_per_room: i64,
    /// Seconds to retain room resource events for watch resume and audit diagnostics (0 = disabled)
    pub room_resource_event_retention_seconds: u64,
    /// Seconds to retain chat message events for reconnect replay and diagnostics (0 = disabled)
    pub chat_message_event_retention_seconds: u64,
    /// Days to retain playback progress rows not referenced by current playback (0 = disabled)
    pub playback_progress_retention_days: u32,
    /// Seconds to keep uploaded file objects that have no active product reference (0 = disabled)
    pub unreferenced_file_retention_seconds: u64,
    /// Days to retain successfully dispatched realtime outbox rows (0 = disabled)
    pub realtime_outbox_sent_retention_days: u32,
    /// Days to retain dead-lettered realtime outbox rows (0 = disabled)
    pub realtime_outbox_dead_retention_days: u32,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            soft_delete_retention_days: 90,
            room_soft_delete_retention_days: 90,
            resource_soft_delete_retention_days: 90,
            expired_token_retention_days: 7,
            expired_credential_buffer_hours: 1,
            notification_retention_days: 30,
            notification_max_retention_days: 90,
            chat_max_messages_per_room: 0, // unlimited by default
            room_resource_event_retention_seconds: DEFAULT_ROOM_RESOURCE_EVENT_RETENTION_SECONDS,
            chat_message_event_retention_seconds: DEFAULT_CHAT_MESSAGE_EVENT_RETENTION_SECONDS,
            playback_progress_retention_days: 15,
            unreferenced_file_retention_seconds: 86_400,
            realtime_outbox_sent_retention_days: 7,
            realtime_outbox_dead_retention_days: 30,
        }
    }
}

/// Result of a cleanup run
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    /// Number of soft-deleted users permanently deleted
    pub users_purged: u64,
    /// Number of soft-deleted rooms permanently deleted
    pub rooms_purged: u64,
    /// Number of independently soft-deleted media rows permanently deleted
    pub media_purged: u64,
    /// Number of independently soft-deleted playlists permanently deleted
    pub playlists_purged: u64,
    /// Number of independently soft-deleted chat messages permanently deleted
    pub chat_messages_purged: u64,
    /// Number of explicitly unbound email identities permanently deleted
    pub email_identities_purged: u64,
    /// Number of explicitly unbound OAuth2 identities permanently deleted
    pub oauth2_identities_purged: u64,
    /// Number of expired email auth and registration tokens deleted
    pub tokens_deleted: u64,
    /// Number of expired credentials deleted
    pub credentials_deleted: u64,
    /// Number of old notifications deleted
    pub notifications_deleted: u64,
    /// Number of old chat messages deleted
    pub chat_messages_deleted: u64,
    /// Number of expired room resource events deleted
    pub room_resource_events_deleted: u64,
    /// Number of expired chat message events deleted
    pub chat_message_events_deleted: u64,
    /// Number of stale playback progress rows deleted
    pub playback_progress_deleted: u64,
    /// Number of expired or excess playback history rows deleted
    pub playback_history_deleted: u64,
    /// Number of expired token blacklist entries deleted
    pub token_blacklist_deleted: u64,
    /// Number of unreferenced file objects cleaned
    pub unreferenced_files_deleted: u64,
    /// Number of expired file upload sessions cleaned
    pub expired_file_upload_sessions_deleted: u64,
    /// Number of delivered realtime outbox rows deleted
    pub realtime_outbox_deleted: u64,
}

#[derive(Clone, Default)]
pub struct CleanupServiceOptions {
    pub runtime_settings_store: Option<Arc<RuntimeSettingsStore>>,
    pub file_storage_service: Option<Arc<dyn FileStorageService>>,
}

/// Data cleanup service
pub struct CleanupService {
    pool: PgPool,
    config: CleanupConfig,
    leader_check: Arc<dyn LeaderCheck>,
    /// Optional runtime settings store for dynamic `chat_max_messages_per_room`
    runtime_settings_store: Option<Arc<RuntimeSettingsStore>>,
    resource_tasks: CleanupResourceTasks,
}

#[derive(Clone)]
struct CleanupResourceTasks {
    pool: PgPool,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

impl CleanupResourceTasks {
    fn new(pool: PgPool, file_storage_service: Option<Arc<dyn FileStorageService>>) -> Self {
        Self {
            pool,
            file_storage_service,
        }
    }

    async fn cleanup_expired_file_references(&self) -> Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        cleanup_ops::cleanup_expired_file_references(&self.pool, storage)
            .await
            .internal_with_err("Failed to cleanup expired file references")
    }

    async fn cleanup_unreferenced_files(&self, retention_seconds: u64) -> Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        cleanup_ops::cleanup_unreferenced_file_objects(&self.pool, storage, retention_seconds)
            .await
            .internal_with_err("Failed to cleanup unreferenced file objects")
    }

    async fn cleanup_expired_file_upload_sessions(&self) -> Result<u64> {
        let Some(storage) = &self.file_storage_service else {
            return Ok(0);
        };
        cleanup_ops::cleanup_expired_file_upload_sessions(&self.pool, storage)
            .await
            .internal_with_err("Failed to cleanup expired file upload sessions")
    }

    async fn cleanup_chat_messages(
        &self,
        keep_count: i64,
        activity_window_minutes: i32,
    ) -> Result<u64> {
        cleanup_ops::cleanup_chat_messages_with_files(
            &self.pool,
            self.file_storage_service.as_ref(),
            cleanup_ops::ChatMessageCleanupScope::ActiveRoomsCap {
                keep_count,
                activity_window_minutes,
            },
            super::FileStorageCleanupOrigin::ReferenceCapExceeded,
            "per-room cap purge",
        )
        .await
    }

    async fn purge_soft_deleted_chat_messages(&self, retention_days: i64) -> Result<u64> {
        cleanup_ops::cleanup_chat_messages_with_files(
            &self.pool,
            self.file_storage_service.as_ref(),
            cleanup_ops::ChatMessageCleanupScope::SoftDeletedRetention { retention_days },
            super::FileStorageCleanupOrigin::RetentionExpired,
            "soft-delete retention purge",
        )
        .await
    }
}

impl CleanupService {
    fn chat_max_messages_per_room_from_config(config: &CleanupConfig) -> i64 {
        config.chat_max_messages_per_room
    }

    /// Create a new cleanup service with a leader check.
    ///
    /// Cleanup only runs when this node is the cluster leader (or in
    /// single-node mode where `AlwaysLeader` is used).
    #[must_use]
    pub fn new(pool: PgPool, config: CleanupConfig, leader_check: Arc<dyn LeaderCheck>) -> Self {
        Self::new_with_options(pool, config, leader_check, CleanupServiceOptions::default())
    }

    /// Create a new cleanup service with explicit runtime dependencies.
    #[must_use]
    pub fn new_with_options(
        pool: PgPool,
        config: CleanupConfig,
        leader_check: Arc<dyn LeaderCheck>,
        options: CleanupServiceOptions,
    ) -> Self {
        Self {
            resource_tasks: CleanupResourceTasks::new(
                pool.clone(),
                options.file_storage_service.clone(),
            ),
            pool,
            config,
            leader_check,
            runtime_settings_store: options.runtime_settings_store,
        }
    }

    /// Get the effective `chat_max_messages_per_room` value.
    ///
    /// Reads from `RuntimeSettingsStore` if available, otherwise falls back to config.
    fn chat_max_messages_per_room(&self) -> Result<i64> {
        match self.runtime_settings_store.as_ref() {
            Some(registry) => registry.chat.max_messages_per_room.get().and_then(|value| {
                i64::try_from(value).map_err(|_| {
                    crate::Error::Internal(
                        "chat_max_messages_per_room exceeds i64::MAX".to_string(),
                    )
                })
            }),
            None => Ok(Self::chat_max_messages_per_room_from_config(&self.config)),
        }
    }

    fn chat_message_event_retention_seconds(&self) -> Result<u64> {
        let message_retention_days = match self.runtime_settings_store.as_ref() {
            Some(registry) => registry.chat.message_retention_days.get()?,
            None => 90,
        };
        cleanup_ops::effective_chat_message_event_retention_seconds(
            self.config.chat_message_event_retention_seconds,
            message_retention_days,
        )
    }

    fn playback_history_retention(&self) -> Result<(u32, i64)> {
        match self.runtime_settings_store.as_ref() {
            Some(settings) => Ok((
                settings.playback_history.retention_days.get()?,
                settings.playback_history.max_entries_per_room.get()?,
            )),
            None => Ok((90, 1_000)),
        }
    }

    /// Run all cleanup tasks once
    ///
    /// Each task runs independently; if one fails, others still execute.
    /// Returns a summary of what was cleaned up.
    pub async fn run_all(&self) -> CleanupResult {
        // Short-circuit if we are not the leader to avoid duplicate work
        // across cluster replicas. This guards direct `run_all()` callers;
        // the periodic loop in `start_periodic` has its own check but calling
        // `run_all()` directly (e.g. from an admin endpoint) should also be safe.
        if !self.leader_check.is_leader() {
            return CleanupResult::default();
        }

        let mut result = CleanupResult::default();

        // Read dynamic settings at runtime
        let chat_max_messages = match self.chat_max_messages_per_room() {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    error = %error,
                    "Skipping chat message cleanup because max_chat_messages_per_room could not be loaded"
                );
                0
            }
        };

        // 1. Purge soft-deleted rooms first.
        // Soft-deleted users can still be referenced by their owned room rows
        // (`rooms.created_by` is `ON DELETE RESTRICT`). Purging rooms before
        // users lets a single cleanup run fully retire the common "user deleted
        // together with their rooms" path instead of deferring user purge by a
        // whole cleanup interval.
        if self.config.room_soft_delete_retention_days > 0 {
            match self.purge_soft_deleted_rooms().await {
                Ok(count) => {
                    result.rooms_purged = count;
                    if count > 0 {
                        info!(count, "Purged soft-deleted rooms");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted rooms"),
            }
        }

        // 2. Purge soft-deleted users
        if self.config.soft_delete_retention_days > 0 {
            match self.purge_soft_deleted_users().await {
                Ok(count) => {
                    result.users_purged = count;
                    if count > 0 {
                        info!(count, "Purged soft-deleted users");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted users"),
            }
        }

        // 2b. Purge only independently deleted resources. Account- and
        // room-propagated rows remain governed by their aggregate recovery window.
        if self.config.resource_soft_delete_retention_days > 0 {
            match self.purge_soft_deleted_resources().await {
                Ok(cleanup) => {
                    result.media_purged = cleanup.media_purged;
                    result.playlists_purged = cleanup.playlists_purged;
                    result.email_identities_purged = cleanup.email_identities_purged;
                    result.oauth2_identities_purged = cleanup.oauth2_identities_purged;
                    if cleanup.media_purged > 0
                        || cleanup.playlists_purged > 0
                        || cleanup.email_identities_purged > 0
                        || cleanup.oauth2_identities_purged > 0
                    {
                        info!(
                            media = cleanup.media_purged,
                            playlists = cleanup.playlists_purged,
                            email_identities = cleanup.email_identities_purged,
                            oauth2_identities = cleanup.oauth2_identities_purged,
                            "Purged independently soft-deleted resources"
                        );
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted room resources"),
            }

            match self.purge_soft_deleted_chat_messages().await {
                Ok(count) => {
                    result.chat_messages_purged = count;
                    if count > 0 {
                        info!(count, "Purged independently soft-deleted chat messages");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to purge soft-deleted chat messages"),
            }
        }

        // 3. Delete expired email auth and registration tokens
        if self.config.expired_token_retention_days > 0 {
            match self.delete_expired_tokens().await {
                Ok(count) => {
                    result.tokens_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted expired email auth and registration tokens");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to delete expired email auth and registration tokens");
                }
            }
        }

        // 4b. Delete expired media provider credentials
        if self.config.expired_credential_buffer_hours > 0 {
            match self.delete_expired_credentials().await {
                Ok(count) => {
                    result.credentials_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted expired media provider credentials");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete expired credentials"),
            }
        }

        // 5. Delete old read notifications
        if self.config.notification_retention_days > 0 {
            match self.delete_old_notifications().await {
                Ok(count) => {
                    result.notifications_deleted += count;
                    if count > 0 {
                        info!(count, "Deleted old read notifications");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete old read notifications"),
            }
        }

        // 5b. Delete all notifications (including unread) past max retention
        if self.config.notification_max_retention_days > 0 {
            match self.delete_expired_notifications().await {
                Ok(count) => {
                    result.notifications_deleted += count;
                    if count > 0 {
                        info!(count, "Deleted expired notifications (past max retention)");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to delete expired notifications"),
            }
        }

        // 6. Cleanup chat messages (per-room cap)
        if chat_max_messages > 0 {
            match self.cleanup_chat_messages(chat_max_messages).await {
                Ok(count) => {
                    result.chat_messages_deleted = count;
                    if count > 0 {
                        info!(count, "Cleaned up old chat messages");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup chat messages"),
            }
        }

        // 7. Cleanup expired room resource events (prevents unbounded durable watch/audit log growth)
        let chat_message_event_retention_seconds = match self.chat_message_event_retention_seconds()
        {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    error = %error,
                    "Skipping chat message event cleanup because retention could not be loaded"
                );
                0
            }
        };
        if chat_message_event_retention_seconds > 0 {
            match self
                .cleanup_chat_message_events(chat_message_event_retention_seconds)
                .await
            {
                Ok(count) => {
                    result.chat_message_events_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted expired chat message events");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup chat message events"),
            }
        }

        if self.config.room_resource_event_retention_seconds > 0 {
            match self.cleanup_room_resource_events().await {
                Ok(count) => {
                    result.room_resource_events_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted expired room resource events");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup room resource events"),
            }
        }

        // 8. Cleanup stale playback progress rows.
        if self.config.playback_progress_retention_days > 0 {
            match self.cleanup_stale_playback_progress().await {
                Ok(count) => {
                    result.playback_progress_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted stale playback progress rows");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup playback progress"),
            }
        }

        match self.cleanup_playback_history().await {
            Ok(count) => {
                result.playback_history_deleted = count;
                if count > 0 {
                    info!(count, "Deleted expired or excess playback history rows");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup playback history"),
        }

        // 9. Cleanup expired token blacklist entries (prevents unbounded table growth)
        match self.cleanup_token_blacklist().await {
            Ok(count) => {
                result.token_blacklist_deleted = count;
                if count > 0 {
                    info!(count, "Deleted expired token blacklist entries");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup token blacklist"),
        }

        // 10. Cleanup uploaded file objects that were never attached to a product row.
        match self.cleanup_expired_file_references().await {
            Ok(count) => {
                if count > 0 {
                    info!(count, "Released expired file references");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup expired file references"),
        }

        match self.cleanup_expired_file_upload_sessions().await {
            Ok(count) => {
                result.expired_file_upload_sessions_deleted = count;
                if count > 0 {
                    info!(count, "Deleted expired file upload sessions");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup expired file upload sessions"),
        }

        if self.config.unreferenced_file_retention_seconds > 0 {
            match self.cleanup_unreferenced_files().await {
                Ok(count) => {
                    result.unreferenced_files_deleted = count;
                    if count > 0 {
                        info!(count, "Deleted unreferenced file objects");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to cleanup unreferenced file objects"),
            }
        }

        match self.cleanup_realtime_outbox().await {
            Ok(count) => {
                result.realtime_outbox_deleted = count;
                if count > 0 {
                    info!(count, "Deleted delivered realtime outbox rows");
                }
            }
            Err(e) => warn!(error = %e, "Failed to cleanup realtime outbox"),
        }

        result
    }

    /// Permanently delete users that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_users(&self) -> Result<u64> {
        let days = u32_to_i32(
            self.config.soft_delete_retention_days,
            "soft_delete_retention_days",
        )?;
        let user_ids = sqlx::query_scalar!(
            r#"
            SELECT id as "id: crate::models::UserId"
            FROM users
            WHERE deleted_at IS NOT NULL
              AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ORDER BY deleted_at ASC, id ASC
            "#,
            days,
        )
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to list soft-deleted users for purge")?;

        let mut purged = 0u64;
        for user_id in user_ids {
            let mut tx = self.pool.begin().await?;

            // Serialize recovery against the purge and re-check the retention
            // predicate while holding the user row lock. Recovery locks this
            // row before restoring identities and owned rooms, so a restored
            // account cannot lose its aggregate after the candidate scan.
            let purgeable_user = sqlx::query_scalar!(
                r#"
                SELECT id AS "id: crate::models::UserId"
                FROM users
                WHERE id = $1
                  AND deleted_at IS NOT NULL
                  AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $2)
                FOR UPDATE
                "#,
                user_id as crate::models::UserId,
                days,
            )
            .fetch_optional(&mut *tx)
            .await
            .internal_with_err("Failed to lock soft-deleted user before purge")?;
            if purgeable_user.is_none() {
                tx.rollback()
                    .await
                    .internal_with_err("Failed to rollback skipped user purge")?;
                continue;
            }

            // Account-owned rooms belong to the account recovery aggregate.
            // Purge them in the same transaction when the account window ends.
            let owned_room_ids = sqlx::query_scalar!(
                r#"SELECT id AS "id: crate::models::RoomId"
                   FROM rooms
                   WHERE created_by = $1
                     AND deleted_at IS NOT NULL
                   ORDER BY id
                   FOR UPDATE"#,
                user_id.as_i64(),
            )
            .fetch_all(&mut *tx)
            .await
            .internal_with_err("Failed to list retained account rooms during user purge")?;
            for room_id in owned_room_ids {
                crate::repository::room_cleanup::hard_delete_room_and_cleanup_in_tx(
                    &mut tx, &room_id,
                )
                .await
                .internal_with_err("Failed to purge retained account room")?;
            }

            // User soft-delete keeps historical memberships by marking them as
            // `left`. Those rows still carry `ON DELETE RESTRICT` FKs, so they
            // must be removed before the hard delete can succeed.
            sqlx::query!(
                "DELETE FROM room_members WHERE user_id = $1",
                user_id as crate::models::UserId,
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to delete room memberships during user purge")?;

            // Account deletion keeps recoverable identity rows during the
            // retention window. Hard purge removes every retained credential
            // and account-owned resource before deleting the user row.
            sqlx::query!(
                "UPDATE file_references fr SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP FROM users u WHERE u.id = $1 AND fr.id = u.avatar_file_reference_id AND fr.released_at IS NULL",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to expire retained user avatar reference")?;
            sqlx::query!(
                r#"
                WITH RECURSIVE retained_playlists(id) AS (
                    SELECT id
                    FROM playlists
                    WHERE creator_id = $1 OR deleted_owner_id = $1
                    UNION
                    SELECT child.id
                    FROM playlists child
                    JOIN retained_playlists parent ON child.parent_id = parent.id
                )
                UPDATE playlists
                SET deleted_owner_id = $1
                WHERE id IN (SELECT id FROM retained_playlists)
                  AND deleted_owner_id IS DISTINCT FROM $1
                "#,
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to claim retained playlist subtree during user purge")?;
            sqlx::query!(
                r#"
                UPDATE media
                SET deleted_owner_id = $1
                WHERE deleted_owner_id IS DISTINCT FROM $1
                  AND (
                      creator_id = $1
                      OR playlist_id IN (
                          SELECT id FROM playlists WHERE deleted_owner_id = $1
                      )
                  )
                "#,
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to claim retained media subtree during user purge")?;
            sqlx::query!(
                "UPDATE file_references fr SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP WHERE fr.released_at IS NULL AND (EXISTS (SELECT 1 FROM media m WHERE (m.creator_id = $1 OR m.deleted_owner_id = $1) AND (fr.id = m.cover_file_reference_id OR fr.id = m.thumbnail_file_reference_id)) OR EXISTS (SELECT 1 FROM playlists p WHERE (p.creator_id = $1 OR p.deleted_owner_id = $1) AND fr.id = p.cover_file_reference_id))",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to expire retained user file references")?;
            sqlx::query!(
                "DELETE FROM provider_playback_sessions WHERE credential_owner_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to remove provider playback sessions during user purge")?;
            sqlx::query!(
                "DELETE FROM room_creation_requests WHERE requested_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to remove room creation requests during user purge")?;
            sqlx::query!(
                "UPDATE room_creation_requests SET reviewed_by = NULL WHERE reviewed_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear room request reviewer during user purge")?;
            sqlx::query!(
                "DELETE FROM room_join_requests WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to remove room join requests during user purge")?;
            sqlx::query!(
                "UPDATE room_join_requests SET reviewed_by = NULL WHERE reviewed_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear join request reviewer during user purge")?;
            sqlx::query!(
                "UPDATE user_registration_requests SET reviewed_by = NULL WHERE reviewed_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear registration reviewer during user purge")?;
            sqlx::query!(
                "DELETE FROM content_reports WHERE reporter_user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to remove content reports during user purge")?;
            sqlx::query!(
                "UPDATE user_bans SET banned_by = NULL WHERE banned_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear ban actor during user purge")?;
            sqlx::query!(
                "UPDATE user_bans SET revoked_by = NULL WHERE revoked_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear ban revoker during user purge")?;
            sqlx::query!(
                "UPDATE room_bans SET banned_by = NULL WHERE banned_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear room ban actor during user purge")?;
            sqlx::query!(
                "UPDATE room_bans SET revoked_by = NULL WHERE revoked_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear room ban revoker during user purge")?;
            sqlx::query!(
                "DELETE FROM auth_oauth2_identities WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained OAuth2 identities")?;
            sqlx::query!(
                "DELETE FROM auth_email_identities WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained email identity")?;
            sqlx::query!(
                "DELETE FROM auth_email_tokens WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained email tokens")?;
            sqlx::query!(
                "DELETE FROM auth_email_bind_requests WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained email bind requests")?;

            // Durable room events can outlive the rows they describe. Remove
            // events authored by the account and events whose chat/resource
            // aggregate belongs to it before the user FK is deleted.
            sqlx::query!(
                r#"
                DELETE FROM chat_message_events e
                WHERE e.actor_user_id = $1
                   OR EXISTS (
                       SELECT 1
                       FROM chat_messages m
                       WHERE m.room_id = e.room_id
                         AND m.id = e.message_id
                         AND m.created_at = e.message_created_at
                         AND m.user_id = $1
                   )
                   OR EXISTS (
                       SELECT 1
                       FROM chat_message_mentions mention
                       WHERE mention.room_id = e.room_id
                         AND mention.message_id = e.message_id
                         AND mention.message_created_at = e.message_created_at
                         AND mention.mentioned_user_id = $1
                   )
                "#,
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained chat events")?;
            sqlx::query!(
                r#"
                DELETE FROM room_resource_events e
                WHERE e.actor_user_id = $1
                   OR (
                       e.resource_type = 'chat_pins'
                       AND e.aggregate_type = 'chat_message'
                       AND EXISTS (
                           SELECT 1
                           FROM chat_messages m
                           WHERE m.room_id = e.room_id
                             AND e.aggregate_id = m.id::TEXT
                             AND m.user_id = $1
                       )
                   )
                   OR (
                       e.resource_type = 'media'
                       AND EXISTS (
                           SELECT 1
                           FROM media m
                           WHERE e.resource_id = m.id::TEXT
                             AND (m.creator_id = $1 OR m.deleted_owner_id = $1)
                       )
                   )
                   OR (
                       e.resource_type = 'playlist'
                       AND EXISTS (
                           SELECT 1
                           FROM playlists p
                           WHERE e.resource_id = p.id::TEXT
                             AND (p.creator_id = $1 OR p.deleted_owner_id = $1)
                       )
                   )
                "#,
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained room resource events")?;
            // Account-owned chat rows are removed by the user FK cascade. Mark
            // their attachment references expired first so object storage can
            // release the objects after the database transaction commits.
            sqlx::query!(
                r#"
                UPDATE file_references fr
                SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP),
                    updated_at = CURRENT_TIMESTAMP
                FROM chat_message_attachments a
                JOIN chat_messages m
                  ON m.room_id = a.room_id
                 AND m.id = a.message_id
                 AND m.created_at = a.message_created_at
                WHERE (m.user_id = $1 OR m.deleted_owner_id = $1)
                  AND fr.storage_backend = a.storage_backend
                  AND fr.object_key = a.object_key
                  AND fr.reference_kind = 'chat_message_attachment'
                  AND fr.reference_id = format(
                      '%s:%s:%s:%s',
                      a.room_id,
                      a.message_id,
                      round(extract(epoch FROM a.message_created_at) * 1000000)::BIGINT,
                      a.id
                  )
                  AND fr.released_at IS NULL
                "#,
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to expire retained chat attachment references")?;
            sqlx::query!(
                "DELETE FROM chat_messages WHERE user_id = $1 OR deleted_owner_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained chat messages")?;
            sqlx::query!(
                "DELETE FROM auth_password_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained password credential")?;
            sqlx::query!(
                "DELETE FROM auth_webauthn_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained WebAuthn credentials")?;
            sqlx::query!(
                "DELETE FROM auth_totp_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained TOTP credential")?;
            sqlx::query!(
                "DELETE FROM user_media_provider_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge provider credentials")?;
            sqlx::query!(
                "DELETE FROM notifications WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge notifications")?;
            sqlx::query!(
                "DELETE FROM room_favorites WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge room favorites")?;
            sqlx::query!("DELETE FROM user_bans WHERE user_id = $1", user_id.as_i64(),)
                .execute(&mut *tx)
                .await
                .internal_with_err("Failed to purge account ban history")?;
            sqlx::query!(
                r#"
                UPDATE room_playback_state
                SET playing_media_id = NULL,
                    playing_playlist_id = NULL,
                    target = NULL,
                    current_progress_id = NULL,
                    speed = 1.0,
                    is_playing = FALSE,
                    playback_generation = playback_generation + 1,
                    version = version + 1,
                    updated_at = CURRENT_TIMESTAMP
                WHERE playing_media_id IN (
                          SELECT id
                          FROM media
                          WHERE creator_id = $1 OR deleted_owner_id = $1
                      )
                   OR playing_playlist_id IN (
                          SELECT id
                          FROM playlists
                          WHERE creator_id = $1 OR deleted_owner_id = $1
                      )
                "#,
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to clear retained resource playback state")?;
            sqlx::query!(
                "DELETE FROM media WHERE creator_id = $1 OR deleted_owner_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut *tx)
            .await
            .internal_with_err("Failed to purge retained media")?;
            loop {
                let deleted = sqlx::query!(
                    r#"
                    DELETE FROM playlists parent
                    WHERE (parent.creator_id = $1 OR parent.deleted_owner_id = $1)
                      AND NOT EXISTS (
                          SELECT 1
                          FROM playlists child
                          WHERE child.parent_id = parent.id
                      )
                    "#,
                    user_id.as_i64(),
                )
                .execute(&mut *tx)
                .await
                .internal_with_err("Failed to purge retained playlist leaves")?;
                if deleted.rows_affected() == 0 {
                    break;
                }
            }
            let retained_playlist_count = sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "count!"
                   FROM playlists
                   WHERE creator_id = $1 OR deleted_owner_id = $1"#,
                user_id.as_i64(),
            )
            .fetch_one(&mut *tx)
            .await
            .internal_with_err("Failed to verify retained playlist purge")?;
            if retained_playlist_count > 0 {
                return Err(crate::Error::Internal(format!(
                    "Cannot purge user {user_id}: retained playlist graph contains a cycle"
                )));
            }

            match sqlx::query!(
                r"
                DELETE FROM users
                WHERE id = $1
                  AND deleted_at IS NOT NULL
                  AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $2)
                ",
                user_id as crate::models::UserId,
                days,
            )
            .execute(&mut *tx)
            .await
            {
                Ok(result) => {
                    tx.commit()
                        .await
                        .internal_with_err("Failed to commit user purge transaction")?;
                    purged += result.rows_affected();
                }
                Err(error) => {
                    warn!(
                        user_id = %user_id,
                        error = %error,
                        "Failed to purge soft-deleted user; leaving row for a future retry"
                    );
                }
            }
        }

        Ok(purged)
    }

    /// Permanently delete rooms that were soft-deleted beyond the retention period
    async fn purge_soft_deleted_rooms(&self) -> Result<u64> {
        let days = u32_to_i32(
            self.config.room_soft_delete_retention_days,
            "room_soft_delete_retention_days",
        )?;
        let user_retention_days = if self.config.soft_delete_retention_days == 0 {
            None
        } else {
            Some(u32_to_i32(
                self.config.soft_delete_retention_days,
                "soft_delete_retention_days",
            )?)
        };
        let room_ids = sqlx::query_scalar!(
            r#"
            SELECT id as "id: crate::models::RoomId"
            FROM rooms r
            WHERE r.deleted_at IS NOT NULL
              AND r.deleted_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ORDER BY r.deleted_at ASC, r.id ASC
            "#,
            days,
        )
        .fetch_all(&self.pool)
        .await
        .internal_with_err("Failed to list soft-deleted rooms for purge")?;

        let mut purged = 0u64;
        for room_id in room_ids {
            let mut tx = self.pool.begin().await?;

            // Account-owned rooms are part of the user recovery aggregate.
            // Lock the owner before the room so restore and purge have one
            // ordering and cannot split the aggregate during a race.
            let candidate = sqlx::query!(
                r#"
                SELECT deletion_source AS "deletion_source?: DeletionSource",
                       deleted_owner_id AS "deleted_owner_id?: crate::models::UserId"
                FROM rooms
                WHERE id = $1
                "#,
                room_id.as_i64(),
            )
            .fetch_optional(&mut *tx)
            .await
            .internal_with_err("Failed to inspect soft-deleted room before purge")?;
            let Some(candidate) = candidate else {
                tx.rollback()
                    .await
                    .internal_with_err("Failed to rollback missing room purge")?;
                continue;
            };

            if candidate.deletion_source == Some(DeletionSource::Account) {
                let Some(owner_id) = candidate.deleted_owner_id else {
                    // Legacy rows without an owner cannot participate in
                    // account recovery and are safe to process as rooms.
                    let room_locked = sqlx::query_scalar!(
                        "SELECT id AS \"id: crate::models::RoomId\" FROM rooms WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE",
                        room_id.as_i64(),
                    )
                    .fetch_optional(&mut *tx)
                    .await
                    .internal_with_err("Failed to lock legacy account room before purge")?;
                    if room_locked.is_none() {
                        tx.rollback()
                            .await
                            .internal_with_err("Failed to rollback restored legacy room purge")?;
                        continue;
                    }
                    let deleted =
                        crate::repository::room_cleanup::hard_delete_room_and_cleanup_in_tx(
                            &mut tx, &room_id,
                        )
                        .await
                        .internal_with_err("Failed to clean up legacy account room")?;
                    tx.commit()
                        .await
                        .internal_with_err("Failed to commit legacy account room purge")?;
                    if deleted {
                        purged += 1;
                    }
                    continue;
                };

                let Some(user_retention_days) = user_retention_days else {
                    tx.rollback()
                        .await
                        .internal_with_err("Failed to rollback permanently retained room purge")?;
                    continue;
                };
                let purgeable_owner = sqlx::query_scalar!(
                    r#"
                    SELECT id AS "id: crate::models::UserId"
                    FROM users
                    WHERE id = $1
                      AND deleted_at IS NOT NULL
                      AND deleted_at < CURRENT_TIMESTAMP - make_interval(days => $2)
                    FOR UPDATE
                    "#,
                    owner_id as crate::models::UserId,
                    user_retention_days,
                )
                .fetch_optional(&mut *tx)
                .await
                .internal_with_err("Failed to lock account owner before room purge")?;
                if purgeable_owner.is_none() {
                    tx.rollback()
                        .await
                        .internal_with_err("Failed to rollback protected account room purge")?;
                    continue;
                }
                let room_locked = sqlx::query_scalar!(
                    r#"
                    SELECT id AS "id: crate::models::RoomId"
                    FROM rooms
                    WHERE id = $1
                      AND deleted_at IS NOT NULL
                      AND deletion_source = $3
                      AND deleted_owner_id = $2
                    FOR UPDATE
                    "#,
                    room_id.as_i64(),
                    owner_id as crate::models::UserId,
                    DeletionSource::Account as DeletionSource,
                )
                .fetch_optional(&mut *tx)
                .await
                .internal_with_err("Failed to re-check account room before purge")?;
                if room_locked.is_none() {
                    tx.rollback()
                        .await
                        .internal_with_err("Failed to rollback restored account room purge")?;
                    continue;
                }
            } else {
                let room_locked = sqlx::query_scalar!(
                    "SELECT id AS \"id: crate::models::RoomId\" FROM rooms WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE",
                    room_id.as_i64(),
                )
                .fetch_optional(&mut *tx)
                .await
                .internal_with_err("Failed to lock soft-deleted room before purge")?;
                if room_locked.is_none() {
                    tx.rollback()
                        .await
                        .internal_with_err("Failed to rollback restored room purge")?;
                    continue;
                }
            }

            let deleted = crate::repository::room_cleanup::hard_delete_room_and_cleanup_in_tx(
                &mut tx, &room_id,
            )
            .await
            .internal_with_err("Failed to clean up room dependencies during purge")?;
            tx.commit()
                .await
                .internal_with_err("Failed to commit room purge transaction")?;
            if deleted {
                purged += 1;
            }
        }

        Ok(purged)
    }

    async fn purge_soft_deleted_resources(
        &self,
    ) -> Result<cleanup_ops::SoftDeletedResourceCleanupResult> {
        cleanup_ops::cleanup_soft_deleted_media_and_playlists(
            &self.pool,
            self.config.resource_soft_delete_retention_days,
        )
        .await
    }

    async fn purge_soft_deleted_chat_messages(&self) -> Result<u64> {
        let days = i64::from(self.config.resource_soft_delete_retention_days);
        self.resource_tasks
            .purge_soft_deleted_chat_messages(days)
            .await
    }

    /// Delete email auth and registration tokens that expired beyond the retention period.
    async fn delete_expired_tokens(&self) -> Result<u64> {
        let days = u32_to_i32(
            self.config.expired_token_retention_days,
            "expired_token_retention_days",
        )?;
        let auth_tokens = sqlx::query!(
            r"
            DELETE FROM auth_email_tokens
            WHERE expires_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            days
        )
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired tokens")?;

        let registration_tokens = sqlx::query!(
            r"
            DELETE FROM auth_email_registration_tokens
            WHERE expires_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            days
        )
        .execute(&self.pool)
        .await
        .internal_with_err("Failed to delete expired registration tokens")?;

        Ok(auth_tokens.rows_affected() + registration_tokens.rows_affected())
    }

    /// Delete expired media provider credentials with buffer to prevent race conditions.
    async fn delete_expired_credentials(&self) -> Result<u64> {
        cleanup_ops::delete_expired_credentials(
            &self.pool,
            self.config.expired_credential_buffer_hours,
        )
        .await
    }

    /// Delete read notifications older than the retention period
    async fn delete_old_notifications(&self) -> Result<u64> {
        cleanup_ops::delete_old_read_notifications(
            &self.pool,
            self.config.notification_retention_days,
        )
        .await
    }

    /// Delete all notifications (including unread) older than the max retention period
    ///
    /// This prevents unbounded growth from unread notifications that are never
    /// acknowledged by users.
    async fn delete_expired_notifications(&self) -> Result<u64> {
        cleanup_ops::delete_expired_notifications(
            &self.pool,
            self.config.notification_max_retention_days,
        )
        .await
    }

    /// Remove expired entries from the token blacklist table.
    ///
    /// Deletes expired token blacklist rows directly in PostgreSQL.
    async fn cleanup_token_blacklist(&self) -> Result<u64> {
        let deleted_count = sqlx::query_scalar!(
            r#"
            WITH deleted AS (
                DELETE FROM auth_token_blacklist
                WHERE expires_at < CURRENT_TIMESTAMP
                RETURNING 1
            )
            SELECT COUNT(*)::BIGINT AS "deleted_count!"
            "#
        )
        .fetch_one(&self.pool)
        .await
        .internal_with_err("Failed to cleanup token blacklist")?;
        Ok(deleted_count.max(0).cast_unsigned())
    }

    async fn cleanup_room_resource_events(&self) -> Result<u64> {
        cleanup_ops::delete_old_room_resource_events(
            &self.pool,
            self.config.room_resource_event_retention_seconds,
        )
        .await
    }

    async fn cleanup_chat_message_events(&self, retention_seconds: u64) -> Result<u64> {
        cleanup_ops::delete_old_chat_message_events(&self.pool, retention_seconds).await
    }

    async fn cleanup_realtime_outbox(&self) -> Result<u64> {
        cleanup_ops::delete_delivered_realtime_outbox(
            &self.pool,
            self.config.realtime_outbox_sent_retention_days,
            self.config.realtime_outbox_dead_retention_days,
        )
        .await
    }

    async fn cleanup_stale_playback_progress(&self) -> Result<u64> {
        cleanup_ops::delete_stale_playback_progress(
            &self.pool,
            self.config.playback_progress_retention_days,
        )
        .await
    }

    async fn cleanup_playback_history(&self) -> Result<u64> {
        let (retention_days, max_entries_per_room) = self.playback_history_retention()?;
        crate::repository::PlaybackHistoryRepository::new(self.pool.clone())
            .cleanup(retention_days, max_entries_per_room)
            .await
    }

    async fn cleanup_expired_file_references(&self) -> Result<u64> {
        self.resource_tasks.cleanup_expired_file_references().await
    }

    async fn cleanup_unreferenced_files(&self) -> Result<u64> {
        self.resource_tasks
            .cleanup_unreferenced_files(self.config.unreferenced_file_retention_seconds)
            .await
    }

    async fn cleanup_expired_file_upload_sessions(&self) -> Result<u64> {
        self.resource_tasks
            .cleanup_expired_file_upload_sessions()
            .await
    }

    /// Cleanup chat messages exceeding per-room cap
    ///
    /// Processes rooms with messages within the last 90 days in bounded batches.
    async fn cleanup_chat_messages(&self, keep_count: i64) -> Result<u64> {
        self.resource_tasks
            .cleanup_chat_messages(keep_count, CHAT_CAP_ACTIVITY_WINDOW_MINUTES)
            .await
    }

    /// Start the periodic cleanup background task
    ///
    /// Runs cleanup at the specified interval. Shuts down gracefully when
    /// the `CancellationToken` is cancelled.
    ///
    /// # Arguments
    /// * `interval_hours` - Hours between cleanup runs (e.g., 24 for daily)
    /// * `cancel` - Cancellation token for graceful shutdown
    #[must_use]
    pub fn start_periodic(
        &self,
        interval_hours: u64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let service = Self {
            pool: self.pool.clone(),
            config: self.config.clone(),
            leader_check: self.leader_check.clone(),
            runtime_settings_store: self.runtime_settings_store.clone(),
            resource_tasks: self.resource_tasks.clone(),
        };

        crate::spawn::spawn_monitored("data_cleanup", async move {
            let interval_duration = tokio::time::Duration::from_secs(interval_hours * 3600);
            let mut interval = tokio::time::interval(interval_duration);

            // Skip the first immediate tick -- let the app fully start up first
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = cancel.cancelled() => {
                        info!("Data cleanup task cancelled, shutting down");
                        return;
                    }
                }

                // Only run cleanup on the leader node to avoid duplicate work
                if !service.leader_check.is_leader() {
                    info!("Skipping data cleanup (not leader)");
                    continue;
                }

                // Read dynamic settings at runtime for logging
                let chat_max_messages = match service.chat_max_messages_per_room() {
                    Ok(value) => value,
                    Err(error) => {
                        warn!(
                            error = %error,
                            "Skipping periodic data cleanup because max_chat_messages_per_room could not be loaded"
                        );
                        continue;
                    }
                };

                info!(
                    chat_max_messages,
                    room_retention_days = service.config.room_soft_delete_retention_days,
                    resource_retention_days = service.config.resource_soft_delete_retention_days,
                    user_retention_days = service.config.soft_delete_retention_days,
                    "Starting periodic data cleanup"
                );
                let result = service.run_all().await;

                let total = result.users_purged
                    + result.rooms_purged
                    + result.media_purged
                    + result.playlists_purged
                    + result.chat_messages_purged
                    + result.tokens_deleted
                    + result.credentials_deleted
                    + result.notifications_deleted
                    + result.chat_messages_deleted
                    + result.chat_message_events_deleted
                    + result.room_resource_events_deleted
                    + result.playback_progress_deleted
                    + result.playback_history_deleted
                    + result.realtime_outbox_deleted
                    + result.token_blacklist_deleted;

                if total > 0 {
                    info!(
                        users = result.users_purged,
                        rooms_purged = result.rooms_purged,
                        media_purged = result.media_purged,
                        playlists_purged = result.playlists_purged,
                        chat_messages_purged = result.chat_messages_purged,
                        tokens = result.tokens_deleted,
                        credentials = result.credentials_deleted,
                        notifications = result.notifications_deleted,
                        chat_messages = result.chat_messages_deleted,
                        chat_message_events = result.chat_message_events_deleted,
                        room_resource_events = result.room_resource_events_deleted,
                        playback_progress = result.playback_progress_deleted,
                        playback_history = result.playback_history_deleted,
                        realtime_outbox = result.realtime_outbox_deleted,
                        total,
                        "Data cleanup completed"
                    );
                } else {
                    info!("Data cleanup completed, nothing to clean");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_chat_max_messages_fallback_to_config() {
        let config = CleanupConfig {
            chat_max_messages_per_room: 500,
            ..CleanupConfig::default()
        };

        assert_eq!(
            CleanupService::chat_max_messages_per_room_from_config(&config),
            500
        );
    }

    #[tokio::test]
    async fn test_chat_message_event_retention_fallback_to_config() {
        let service = CleanupService::new(
            PgPool::connect_lazy("postgresql://unused").expect("test pool url should parse"),
            CleanupConfig {
                chat_message_event_retention_seconds: 120 * 24 * 60 * 60,
                ..CleanupConfig::default()
            },
            Arc::new(crate::service::AlwaysLeader),
        );

        assert_eq!(
            service
                .chat_message_event_retention_seconds()
                .expect("retention should resolve"),
            // config floor (120 days) > message retention default (90 days) → 120 days
            120 * 24 * 60 * 60
        );
    }

    #[tokio::test]
    async fn test_default_chat_message_event_retention_matches_message_retention() {
        let service = CleanupService::new(
            PgPool::connect_lazy("postgresql://unused").expect("test pool url should parse"),
            CleanupConfig::default(),
            Arc::new(crate::service::AlwaysLeader),
        );

        assert_eq!(
            service
                .chat_message_event_retention_seconds()
                .expect("default retention should resolve"),
            90 * 24 * 60 * 60
        );
    }
}
