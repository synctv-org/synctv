//! Chat service for managing room chat messages
//!
//! Handles sending, receiving, and deleting chat messages with rate limiting
//! and content filtering.

use chrono::Utc;
use std::sync::Arc;
use tracing::{debug, error, info};

use crate::{
    cache::UsernameCache,
    models::{ChatMessage, PermissionBits, RoomId, SendDanmakuRequest, UserId},
    repository::ChatRepository,
    service::{
        notification::NotificationService, ContentFilter, PermissionService, RateLimitConfig,
        RateLimiter, RoomSettingsService,
    },
    Error, Result,
};

/// Maximum allowed chat message length in characters.
/// Used by both the WebSocket handler and the service layer for consistent validation.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 500;

/// Chat service for managing chat messages
#[derive(Clone)]
pub struct ChatService {
    pub(crate) chat_repository: Arc<ChatRepository>,
    rate_limiter: RateLimiter,
    rate_limit_config: RateLimitConfig,
    content_filter: ContentFilter,
    username_cache: UsernameCache,
    permission_service: PermissionService,
    room_settings_service: RoomSettingsService,
    /// Optional notification service for broadcasting chat messages to room members
    notification_service: Option<NotificationService>,
}

impl std::fmt::Debug for ChatService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatService").finish()
    }
}

impl ChatService {
    /// Create a new chat service
    #[must_use]
    pub const fn new(
        chat_repository: Arc<ChatRepository>,
        rate_limiter: RateLimiter,
        rate_limit_config: RateLimitConfig,
        content_filter: ContentFilter,
        username_cache: UsernameCache,
        permission_service: PermissionService,
        room_settings_service: RoomSettingsService,
    ) -> Self {
        Self {
            chat_repository,
            rate_limiter,
            rate_limit_config,
            content_filter,
            username_cache,
            permission_service,
            room_settings_service,
            notification_service: None,
        }
    }

    /// Set the notification service for broadcasting chat messages to room members
    pub fn set_notification_service(&mut self, service: NotificationService) {
        self.notification_service = Some(service);
    }

    /// Send a chat message
    ///
    /// # Arguments
    /// * `room_id` - Room ID
    /// * `user_id` - User ID sending the message
    /// * `content` - Message content
    ///
    /// # Returns
    /// The created chat message
    pub async fn send_message(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
    ) -> Result<ChatMessage> {
        // Check SEND_CHAT permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::SEND_CHAT)
            .await?;

        // Check if chat is enabled for this room
        let room_settings = self.room_settings_service.get(&room_id).await?;
        if !room_settings.chat_enabled.0 {
            return Err(Error::Authorization(
                "Chat is disabled in this room".to_string(),
            ));
        }

        // Rate limiting: use configured chat_per_second from RateLimitConfig
        let rate_key = format!("chat:rate:{}:{}", room_id.as_str(), user_id.as_str());
        if let Err(e) = self
            .rate_limiter
            .check_rate_limit(
                &rate_key,
                self.rate_limit_config.chat_per_second,
                self.rate_limit_config.window_seconds,
            )
            .await
        {
            return Err(Error::RateLimited(format!("Chat rate limit exceeded: {e}")));
        }

        // Validate content length
        if content.is_empty() {
            return Err(Error::InvalidInput(
                "Message content cannot be empty".to_string(),
            ));
        }

        if content.chars().count() > MAX_CHAT_MESSAGE_CHARS {
            return Err(Error::InvalidInput(format!(
                "Message content must be at most {MAX_CHAT_MESSAGE_CHARS} characters"
            )));
        }

        // Get username
        let username = self
            .username_cache
            .get(&user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))?;

        // Filter content
        let filtered_content = self
            .content_filter
            .filter_chat(&content)
            .map_err(|e| Error::InvalidInput(format!("Content filter error: {e}")))?;

        // Create message
        let message = ChatMessage::new(room_id.clone(), user_id.clone(), filtered_content.clone());

        // Persist to database
        let created_message = self.chat_repository.create(&message).await?;

        info!(
            room_id = room_id.as_str(),
            user_id = user_id.as_str(),
            message_id = %created_message.id,
            "Chat message sent"
        );

        // Broadcast chat message to room members
        if let Some(ref notification_service) = self.notification_service {
            if let Err(e) = notification_service
                .notify_chat_message(
                    &room_id,
                    &created_message.id,
                    &user_id,
                    &username,
                    &filtered_content,
                )
                .await
            {
                error!(
                    room_id = room_id.as_str(),
                    user_id = user_id.as_str(),
                    message_id = %created_message.id,
                    error = %e,
                    "Failed to broadcast chat message to room members"
                );
            }
        }

        Ok(created_message)
    }

    /// Get chat history for a room using cursor-based pagination.
    ///
    /// Uses keyset (cursor) pagination with `(created_at, id)` composite cursor
    /// for efficient pagination, avoiding O(N) OFFSET scans when multiple messages
    /// share the same timestamp.
    ///
    /// # Arguments
    /// * `room_id` - Room ID
    /// * `cursor` - Optional cursor `(created_at, id)` to get messages before this point
    /// * `limit` - Maximum number of messages to return (max 100)
    ///
    /// # Returns
    /// Tuple of (messages, `next_cursor`) where messages are in reverse chronological order
    /// (newest first), and `next_cursor` is the `(created_at, id)` of the oldest message
    /// in this page to be used in the next call, or `None` when no more messages exist.
    pub async fn get_history(
        &self,
        room_id: &RoomId,
        cursor: Option<(chrono::DateTime<Utc>, &str)>,
        limit: i32,
    ) -> Result<(Vec<ChatMessage>, Option<(chrono::DateTime<Utc>, String)>)> {
        self.chat_repository
            .list_by_room_cursor(room_id, cursor, limit)
            .await
    }

    /// Delete a chat message
    ///
    /// # Arguments
    /// * `message_id` - Message ID to delete
    /// * `user_id` - User ID requesting deletion (must be sender or have `DELETE_CHAT` permission)
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn delete_message(&self, message_id: &str, user_id: &UserId) -> Result<bool> {
        // Get the message to check ownership
        let message = self
            .chat_repository
            .get_by_id(message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;

        // Check if user is the sender or has DELETE_CHAT permission
        if message.user_id != *user_id {
            // Not the sender, check DELETE_CHAT permission via PermissionService
            self.permission_service
                .check_permission(&message.room_id, user_id, PermissionBits::DELETE_CHAT)
                .await?;
        }

        self.chat_repository
            .delete(message_id, message.created_at)
            .await
    }

    /// Send a danmaku message (not persisted, real-time only)
    ///
    /// # Arguments
    /// * `room_id` - Room ID
    /// * `user_id` - User ID sending the danmaku
    /// * `request` - Danmaku request with content, color, and position
    ///
    /// # Returns
    /// The danmaku message (not persisted)
    pub async fn send_danmaku(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SendDanmakuRequest,
    ) -> Result<crate::models::DanmakuMessage> {
        use crate::models::DanmakuMessage;

        // Check SEND_CHAT permission (danmaku is a form of chat)
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::SEND_CHAT)
            .await?;

        // Check if danmaku is enabled for this room
        let room_settings = self.room_settings_service.get(&room_id).await?;
        if !room_settings.danmaku_enabled.0 {
            return Err(Error::Authorization(
                "Danmaku is disabled in this room".to_string(),
            ));
        }

        // Rate limiting: use configured danmaku_per_second from RateLimitConfig
        let rate_key = format!("danmaku:rate:{}:{}", room_id.as_str(), user_id.as_str());
        if let Err(e) = self
            .rate_limiter
            .check_rate_limit(
                &rate_key,
                self.rate_limit_config.danmaku_per_second,
                self.rate_limit_config.window_seconds,
            )
            .await
        {
            return Err(Error::RateLimited(format!(
                "Danmaku rate limit exceeded: {e}"
            )));
        }

        // Validate content length
        if request.content.is_empty() {
            return Err(Error::InvalidInput(
                "Danmaku content cannot be empty".to_string(),
            ));
        }

        if request.content.chars().count() > 100 {
            return Err(Error::InvalidInput(
                "Danmaku content must be at most 100 characters".to_string(),
            ));
        }

        // Validate color format (hex color: #RRGGBB with hex digits only)
        if !request.color.starts_with('#') || request.color.len() != 7 {
            return Err(Error::InvalidInput("Invalid color format".to_string()));
        }
        if !request.color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::InvalidInput(
                "Invalid color format: must be hex digits".to_string(),
            ));
        }

        // Filter content
        let filtered_content = self
            .content_filter
            .filter_danmaku(&request.content)
            .map_err(|e| Error::InvalidInput(format!("Content filter error: {e}")))?;

        // Log before moving values
        info!(
            room_id = room_id.as_str(),
            user_id = user_id.as_str(),
            "Danmaku sent"
        );

        // Create danmaku message
        let danmaku = DanmakuMessage::new(
            room_id,
            user_id,
            filtered_content,
            request.color,
            request.position,
        );

        Ok(danmaku)
    }

    /// Cleanup old chat messages for a specific room based on global settings
    ///
    /// # Arguments
    /// * `room_id` - Room ID to cleanup
    /// * `max_messages` - Maximum messages to keep (0 = unlimited)
    ///
    /// # Returns
    /// Number of messages deleted
    pub async fn cleanup_room_messages(&self, room_id: &RoomId, max_messages: u64) -> Result<u64> {
        // If max_messages is 0, no cleanup needed (unlimited)
        if max_messages == 0 {
            return Ok(0);
        }

        // Cleanup old messages
        let deleted = self
            .chat_repository
            .cleanup_old_messages(room_id, max_messages.try_into().unwrap_or(i32::MAX))
            .await?;

        if deleted > 0 {
            debug!(
                room_id = room_id.as_str(),
                deleted = deleted,
                max_messages = max_messages,
                "Cleaned up old chat messages"
            );
        }

        Ok(deleted)
    }

    /// Cleanup old chat messages for all rooms using global settings
    ///
    /// This method uses a single optimized SQL query with window functions to delete
    /// old messages across all rooms, making it suitable for production environments
    /// with thousands of rooms.
    ///
    /// Only processes rooms with recent activity (messages within the last few minutes),
    /// avoiding unnecessary scans of inactive rooms.
    ///
    /// # Arguments
    /// * `max_messages` - Maximum messages to keep per room (from global settings, 0 = unlimited)
    /// * `activity_window_minutes` - Only cleanup rooms with messages in the last N minutes
    ///
    /// # Returns
    /// Total number of messages deleted across all rooms
    pub async fn cleanup_all_rooms(
        &self,
        max_messages: u64,
        activity_window_minutes: i32,
    ) -> Result<u64> {
        // If max_messages is 0, no cleanup needed (unlimited)
        if max_messages == 0 {
            return Ok(0);
        }

        // Use optimized batch cleanup (single SQL query for all rooms)
        let deleted = self
            .chat_repository
            .cleanup_all_rooms(
                max_messages.try_into().unwrap_or(i32::MAX),
                activity_window_minutes,
            )
            .await?;

        if deleted > 0 {
            debug!(
                total_deleted = deleted,
                max_messages = max_messages,
                activity_window_minutes = activity_window_minutes,
                "Cleaned up chat messages for active rooms"
            );
        }

        Ok(deleted)
    }

    /// Start a background task to periodically cleanup old messages
    ///
    /// This task runs every minute and only processes rooms with recent activity (last 3 minutes),
    /// providing near real-time message limit enforcement without scanning inactive rooms.
    ///
    /// # Arguments
    /// * `settings_registry` - Settings registry to get `max_chat_messages` setting
    /// * `interval_seconds` - Cleanup interval in seconds (default: 60 seconds)
    /// * `activity_window_minutes` - Only cleanup rooms with messages in the last N minutes (default: 3 minutes)
    ///
    /// # Returns
    /// `JoinHandle` for the background task
    #[must_use]
    pub fn start_cleanup_task(
        self,
        settings_registry: Arc<crate::service::SettingsRegistry>,
        interval_seconds: u64,
        activity_window_minutes: i32,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        crate::spawn::spawn_monitored("chat_cleanup", async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(interval_seconds));

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Chat cleanup task shutting down");
                        return;
                    }
                    _ = interval.tick() => {}
                }

                // Get current max_chat_messages_per_room setting
                let max_messages = settings_registry
                    .max_chat_messages_per_room
                    .get()
                    .unwrap_or(500);

                match self
                    .cleanup_all_rooms(max_messages, activity_window_minutes)
                    .await
                {
                    Ok(deleted) => {
                        if deleted > 0 {
                            info!(
                                deleted = deleted,
                                max_messages = max_messages,
                                activity_window_minutes = activity_window_minutes,
                                "Periodic chat cleanup completed"
                            );
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to run periodic chat cleanup");
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_validate_content() {
        // Test placeholder
        assert!("hello".len() < 500);
    }

    /// Test color validation logic extracted from `send_danmaku`
    fn validate_color(color: &str) -> Result<(), &'static str> {
        if !color.starts_with('#') || color.len() != 7 {
            return Err("Invalid color format");
        }
        if !color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("Invalid color format: must be hex digits");
        }
        Ok(())
    }

    #[test]
    fn test_color_validation_valid() {
        assert!(validate_color("#000000").is_ok());
        assert!(validate_color("#FFFFFF").is_ok());
        assert!(validate_color("#ff00ff").is_ok());
        assert!(validate_color("#aAbBcC").is_ok());
        assert!(validate_color("#123456").is_ok());
        assert!(validate_color("#7890ab").is_ok());
    }

    #[test]
    fn test_color_validation_missing_hash() {
        assert!(validate_color("000000").is_err());
        assert!(validate_color("FFFFFF").is_err());
    }

    #[test]
    fn test_color_validation_wrong_length() {
        assert!(validate_color("#FFF").is_err());
        assert!(validate_color("#FFFFFFFF").is_err());
        assert!(validate_color("#").is_err());
        assert!(validate_color("").is_err());
    }

    #[test]
    fn test_color_validation_non_hex_chars() {
        // XSS-like payloads that pass starts_with('#') && len()==7
        assert!(validate_color("#<scrip").is_err());
        assert!(validate_color("#ghijkl").is_err());
        assert!(validate_color("#ZZZZZZ").is_err());
        assert!(validate_color("#12345g").is_err());
        assert!(validate_color("#00000!").is_err());
        assert!(validate_color("# space").is_err());
    }

    #[test]
    fn test_color_validation_special_chars() {
        assert!(validate_color("#<>\"'&;").is_err());
        assert!(validate_color("#script").is_err());
    }
}
