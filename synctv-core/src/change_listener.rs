//! `PostgreSQL` LISTEN/NOTIFY-based change listener for cluster-wide resource cleanup.
//!
//! # Status: Dead code -- retained for potential future use
//!
//! This module is **not currently wired into the application bootstrap**. All cache
//! invalidation is handled by [`CacheInvalidationService`](crate::cache::invalidation::CacheInvalidationService)
//! via Redis Streams, which covers all deletion paths (both soft and hard deletes)
//! because they flow through the service/repository layer.
//!
//! The `PostgreSQL` LISTEN/NOTIFY triggers still exist in the database migrations
//! (`notify_user_deleted`, `notify_room_deleted`, etc.) but no code subscribes to
//! their channels. The triggers fire harmlessly with no listeners.
//!
//! ## When to activate this module
//!
//! If a future requirement introduces hard deletes that bypass the service layer
//! (e.g., administrative SQL scripts, external tools), this listener would provide
//! a safety net to catch those deletions and invalidate caches. To activate:
//!
//! 1. Implement [`ChangeHandler`] with a struct that delegates to
//!    [`CacheManager`](crate::cache::manager::CacheManager) and the connection manager.
//! 2. Instantiate [`PostgresChangeListener`] in the bootstrap and call `start()`.
//!
//! ## Architecture
//!
//! When critical resources (users, rooms, playlists, media) are deleted from the database,
//! `PostgreSQL` triggers send notifications via LISTEN/NOTIFY channels. All cluster nodes
//! listen to these channels and perform local cleanup actions:
//!
//! - **`UserDeleted`**: Clear user caches, disconnect user connections, invalidate permissions
//! - **`RoomDeleted`**: Clear room caches, disconnect room connections, invalidate room state
//! - **`PlaylistDeleted`**: Clear playlist caches
//! - **`MediaDeleted`**: Clear media caches

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgListener};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Change event types sent via `PostgreSQL` LISTEN/NOTIFY
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChangeEvent {
    /// User was deleted from the database
    UserDeleted {
        user_id: String,
        timestamp: DateTime<Utc>,
    },
    /// Room was deleted from the database
    RoomDeleted {
        room_id: String,
        timestamp: DateTime<Utc>,
    },
    /// Playlist was deleted from the database
    PlaylistDeleted {
        playlist_id: String,
        timestamp: DateTime<Utc>,
    },
    /// Media was deleted from the database
    MediaDeleted {
        media_id: String,
        timestamp: DateTime<Utc>,
    },
}

impl ChangeEvent {
    /// Get the event type as a string for logging
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::UserDeleted { .. } => "user_deleted",
            Self::RoomDeleted { .. } => "room_deleted",
            Self::PlaylistDeleted { .. } => "playlist_deleted",
            Self::MediaDeleted { .. } => "media_deleted",
        }
    }

    /// Get the resource ID from the event
    #[must_use]
    pub fn resource_id(&self) -> &str {
        match self {
            Self::UserDeleted { user_id, .. } => user_id,
            Self::RoomDeleted { room_id, .. } => room_id,
            Self::PlaylistDeleted { playlist_id, .. } => playlist_id,
            Self::MediaDeleted { media_id, .. } => media_id,
        }
    }
}

/// Handler trait for processing change events
///
/// Implementations perform local cleanup actions when resources are deleted.
///
/// TODO: No production implementation exists yet. If activating this module,
/// implement a struct that delegates to `CacheManager` and the connection manager.
#[async_trait::async_trait]
pub trait ChangeHandler: Send + Sync {
    /// Handle a user deletion event
    async fn handle_user_deleted(&self, user_id: &str) -> Result<()>;

    /// Handle a room deletion event
    async fn handle_room_deleted(&self, room_id: &str) -> Result<()>;

    /// Handle a playlist deletion event
    async fn handle_playlist_deleted(&self, playlist_id: &str) -> Result<()>;

    /// Handle a media deletion event
    async fn handle_media_deleted(&self, media_id: &str) -> Result<()>;
}

/// `PostgreSQL` LISTEN/NOTIFY change listener
///
/// Listens to database notifications for resource deletions and invokes
/// the registered handler to perform local cleanup actions.
///
/// **Not currently started** -- see module-level docs for activation instructions.
pub struct PostgresChangeListener {
    db_pool: PgPool,
    handler: Arc<dyn ChangeHandler>,
    cancel_token: CancellationToken,
}

impl PostgresChangeListener {
    /// `PostgreSQL` notification channels (must match trigger definitions)
    const CHANNEL_USER_DELETED: &'static str = "synctv_user_deleted";
    const CHANNEL_ROOM_DELETED: &'static str = "synctv_room_deleted";
    const CHANNEL_PLAYLIST_DELETED: &'static str = "synctv_playlist_deleted";
    const CHANNEL_MEDIA_DELETED: &'static str = "synctv_media_deleted";

    /// Create a new change listener
    ///
    /// # Arguments
    ///
    /// * `db_pool` - `PostgreSQL` connection pool
    /// * `handler` - Handler for processing change events
    pub fn new(db_pool: PgPool, handler: Arc<dyn ChangeHandler>) -> Self {
        Self {
            db_pool,
            handler,
            cancel_token: CancellationToken::new(),
        }
    }

    /// Get the cancellation token for external shutdown signaling
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Start listening for change notifications
    ///
    /// This spawns a background task that listens to `PostgreSQL` NOTIFY channels
    /// and processes events through the registered handler.
    ///
    /// The task automatically reconnects if the database connection is lost.
    pub async fn start(self: Arc<Self>) -> Result<()> {
        info!("Starting PostgreSQL change listener");

        let mut listener = PgListener::connect_with(&self.db_pool)
            .await
            .context("Failed to create PostgreSQL listener")?;

        // Subscribe to all change notification channels
        listener
            .listen(Self::CHANNEL_USER_DELETED)
            .await
            .context("Failed to subscribe to user_deleted channel")?;
        listener
            .listen(Self::CHANNEL_ROOM_DELETED)
            .await
            .context("Failed to subscribe to room_deleted channel")?;
        listener
            .listen(Self::CHANNEL_PLAYLIST_DELETED)
            .await
            .context("Failed to subscribe to playlist_deleted channel")?;
        listener
            .listen(Self::CHANNEL_MEDIA_DELETED)
            .await
            .context("Failed to subscribe to media_deleted channel")?;

        info!(
            "Subscribed to PostgreSQL notification channels: {}, {}, {}, {}",
            Self::CHANNEL_USER_DELETED,
            Self::CHANNEL_ROOM_DELETED,
            Self::CHANNEL_PLAYLIST_DELETED,
            Self::CHANNEL_MEDIA_DELETED
        );

        // Spawn background task to process notifications
        let cancel_token = self.cancel_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("PostgreSQL change listener cancelled");
                        return;
                    }
                    result = listener.recv() => {
                        match result {
                            Ok(notification) => {
                                if let Err(e) = self.handle_notification(notification.channel(), notification.payload()).await {
                                    error!(
                                        channel = %notification.channel(),
                                        payload = %notification.payload(),
                                        error = %e,
                                        "Failed to handle change notification"
                                    );
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "PostgreSQL listener error, will retry");
                                // Connection lost - wait and let sqlx reconnect automatically
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Handle a notification from `PostgreSQL`
    async fn handle_notification(&self, channel: &str, payload: &str) -> Result<()> {
        debug!(channel = %channel, payload = %payload, "Received PostgreSQL notification");

        // Parse the JSON payload into a ChangeEvent
        let event: ChangeEvent = match channel {
            Self::CHANNEL_USER_DELETED => {
                let data: serde_json::Value = serde_json::from_str(payload)
                    .context("Failed to parse user_deleted payload")?;
                ChangeEvent::UserDeleted {
                    user_id: data["user_id"]
                        .as_str()
                        .context("Missing user_id in payload")?
                        .to_string(),
                    timestamp: data["timestamp"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map_or_else(Utc::now, |dt| dt.with_timezone(&Utc)),
                }
            }
            Self::CHANNEL_ROOM_DELETED => {
                let data: serde_json::Value = serde_json::from_str(payload)
                    .context("Failed to parse room_deleted payload")?;
                ChangeEvent::RoomDeleted {
                    room_id: data["room_id"]
                        .as_str()
                        .context("Missing room_id in payload")?
                        .to_string(),
                    timestamp: data["timestamp"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map_or_else(Utc::now, |dt| dt.with_timezone(&Utc)),
                }
            }
            Self::CHANNEL_PLAYLIST_DELETED => {
                let data: serde_json::Value = serde_json::from_str(payload)
                    .context("Failed to parse playlist_deleted payload")?;
                ChangeEvent::PlaylistDeleted {
                    playlist_id: data["playlist_id"]
                        .as_str()
                        .context("Missing playlist_id in payload")?
                        .to_string(),
                    timestamp: data["timestamp"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map_or_else(Utc::now, |dt| dt.with_timezone(&Utc)),
                }
            }
            Self::CHANNEL_MEDIA_DELETED => {
                let data: serde_json::Value = serde_json::from_str(payload)
                    .context("Failed to parse media_deleted payload")?;
                ChangeEvent::MediaDeleted {
                    media_id: data["media_id"]
                        .as_str()
                        .context("Missing media_id in payload")?
                        .to_string(),
                    timestamp: data["timestamp"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map_or_else(Utc::now, |dt| dt.with_timezone(&Utc)),
                }
            }
            _ => {
                warn!(channel = %channel, "Unknown notification channel, ignoring");
                return Ok(());
            }
        };

        // Dispatch to the appropriate handler method
        let result = match &event {
            ChangeEvent::UserDeleted { user_id, .. } => {
                self.handler.handle_user_deleted(user_id).await
            }
            ChangeEvent::RoomDeleted { room_id, .. } => {
                self.handler.handle_room_deleted(room_id).await
            }
            ChangeEvent::PlaylistDeleted { playlist_id, .. } => {
                self.handler.handle_playlist_deleted(playlist_id).await
            }
            ChangeEvent::MediaDeleted { media_id, .. } => {
                self.handler.handle_media_deleted(media_id).await
            }
        };

        if let Err(e) = result {
            error!(
                event_type = %event.event_type(),
                resource_id = %event.resource_id(),
                error = %e,
                "Failed to handle change event"
            );
        } else {
            info!(
                event_type = %event.event_type(),
                resource_id = %event.resource_id(),
                "Successfully handled change event"
            );
        }

        Ok(())
    }

    /// Gracefully shut down the change listener
    pub fn shutdown(&self) {
        info!("Shutting down PostgreSQL change listener");
        self.cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHandler;

    #[async_trait::async_trait]
    impl ChangeHandler for MockHandler {
        async fn handle_user_deleted(&self, user_id: &str) -> Result<()> {
            println!("Mock: User deleted: {}", user_id);
            Ok(())
        }

        async fn handle_room_deleted(&self, room_id: &str) -> Result<()> {
            println!("Mock: Room deleted: {}", room_id);
            Ok(())
        }

        async fn handle_playlist_deleted(&self, playlist_id: &str) -> Result<()> {
            println!("Mock: Playlist deleted: {}", playlist_id);
            Ok(())
        }

        async fn handle_media_deleted(&self, media_id: &str) -> Result<()> {
            println!("Mock: Media deleted: {}", media_id);
            Ok(())
        }
    }

    #[test]
    fn test_change_event_serialization() {
        let event = ChangeEvent::UserDeleted {
            user_id: "user123".to_string(),
            timestamp: Utc::now(),
        };

        assert_eq!(event.event_type(), "user_deleted");
        assert_eq!(event.resource_id(), "user123");
    }
}
