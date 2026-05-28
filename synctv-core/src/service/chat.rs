//! Chat service for managing room chat messages
//!
//! Handles sending, receiving, and deleting chat messages with rate limiting
//! and content filtering.

use chrono::Utc;
use std::sync::Arc;
use synctv_common::ExecutionControl;
use tracing::{debug, error, info};

use crate::{
    models::{ChatMessage, RoomId, SendDanmakuRequest, UserId},
    repository::ChatRepository,
    service::{
        notification::NotificationService, ContentFilter, PermissionService, RateLimitConfig,
        RequestRateLimiterService, RoomSettingsService, UserService,
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
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    rate_limit_config: RateLimitConfig,
    content_filter: ContentFilter,
    permission_service: PermissionService,
    room_settings_service: RoomSettingsService,
    user_service: Arc<UserService>,
    /// Local room event bus for chat/domain notifications
    notification_service: NotificationService,
}

#[derive(Clone)]
pub struct ChatRuntime {
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
}

#[derive(Clone)]
pub struct ChatDependencies {
    pub permission_service: PermissionService,
    pub room_settings_service: RoomSettingsService,
    pub user_service: Arc<UserService>,
    pub notification_service: NotificationService,
}

impl std::fmt::Debug for ChatService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatService").finish()
    }
}

impl ChatService {
    /// Create a new chat service
    #[must_use]
    pub fn new(
        chat_repository: Arc<ChatRepository>,
        runtime: ChatRuntime,
        dependencies: ChatDependencies,
    ) -> Self {
        let ChatRuntime {
            rate_limiter,
            rate_limit_config,
            content_filter,
        } = runtime;
        let ChatDependencies {
            permission_service,
            room_settings_service,
            user_service,
            notification_service,
        } = dependencies;

        Self {
            chat_repository,
            rate_limiter,
            rate_limit_config,
            content_filter,
            permission_service,
            room_settings_service,
            user_service,
            notification_service,
        }
    }

    #[must_use]
    pub const fn room_settings_service(&self) -> &RoomSettingsService {
        &self.room_settings_service
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
        self.send_message_with_control(room_id, user_id, content, None)
            .await
    }

    /// Send a chat message with cooperative execution control.
    pub async fn send_message_with_control(
        &self,
        room_id: RoomId,
        user_id: UserId,
        content: String,
        control: Option<&ExecutionControl>,
    ) -> Result<ChatMessage> {
        // Check CHAT permission
        self.permission_service
            .check_permission(&room_id, &user_id, crate::models::RoomPermission::CHAT)
            .await?;

        // Check if chat is enabled for this room
        let room_settings = self.room_settings_service.get(&room_id).await?;
        if !room_settings.chat_enabled.0 {
            return Err(Error::Authorization(
                "Chat is disabled in this room".to_string(),
            ));
        }

        // Rate limiting: use configured chat_per_second from RateLimitConfig
        let rate_key = format!("chat:rate:{room_id}:{user_id}");
        if let Err(e) = self
            .rate_limiter
            .check_rate_limit_with_control(
                &rate_key,
                self.rate_limit_config.chat_per_second,
                self.rate_limit_config.window_seconds,
                control,
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
        let username = self.username_for_user(&user_id).await?;

        // Filter content
        let filtered_content = self
            .content_filter
            .filter_chat(&content)
            .map_err(|e| Error::InvalidInput(format!("Content filter error: {e}")))?;

        // Create message
        let message = ChatMessage::new(room_id, user_id, filtered_content.clone());

        // Persist to database
        let created_message = self.chat_repository.create(&message).await?;

        info!(
            room_id = %room_id,
            user_id = %user_id,
            message_id = %created_message.id,
            "Chat message sent"
        );

        // Broadcast chat message to room members
        if let Err(e) = self.notification_service.notify_chat_message(
            &room_id,
            &created_message.id.to_string(),
            &user_id,
            &username,
            &filtered_content,
        ) {
            error!(
                room_id = %room_id,
                user_id = %user_id,
                message_id = %created_message.id,
                error = %e,
                "Failed to publish chat message room event"
            );
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
        cursor: Option<(chrono::DateTime<Utc>, i64)>,
        limit: i32,
    ) -> Result<(Vec<ChatMessage>, Option<(chrono::DateTime<Utc>, i64)>)> {
        self.chat_repository
            .list_by_room_cursor(room_id, cursor, limit)
            .await
    }

    /// Delete a chat message
    ///
    /// # Arguments
    /// * `room_id` - Room that owns the message
    /// * `message_id` - Message ID to delete
    /// * `user_id` - User ID requesting deletion (must be sender or have `DELETE_CHAT` permission)
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn delete_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        user_id: &UserId,
    ) -> Result<bool> {
        // Get the message to check ownership
        let message = self
            .chat_repository
            .get_by_room_and_id(room_id, message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;

        // Check if user is the sender or has DELETE_CHAT permission.
        // If the original author has been deleted (user_id is None), treat as
        // non-owner and require DELETE_CHAT permission.
        let is_sender = message.user_id.as_ref() == Some(user_id);
        if !is_sender {
            // Not the sender, check DELETE_CHAT permission via PermissionService
            self.permission_service
                .check_permission(room_id, user_id, crate::models::RoomPermission::DELETE_CHAT)
                .await?;
        }

        self.chat_repository
            .delete_in_room(room_id, message_id, message.created_at)
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
        self.send_danmaku_with_control(room_id, user_id, request, None)
            .await
    }

    /// Send a danmaku message with cooperative execution control.
    pub async fn send_danmaku_with_control(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SendDanmakuRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<crate::models::DanmakuMessage> {
        use crate::models::DanmakuMessage;

        // Check CHAT permission (danmaku is a form of chat)
        self.permission_service
            .check_permission(&room_id, &user_id, crate::models::RoomPermission::CHAT)
            .await?;

        // Check if danmaku is enabled for this room
        let room_settings = self.room_settings_service.get(&room_id).await?;
        if !room_settings.danmaku_enabled.0 {
            return Err(Error::Authorization(
                "Danmaku is disabled in this room".to_string(),
            ));
        }

        // Rate limiting: use configured danmaku_per_second from RateLimitConfig
        let rate_key = format!("danmaku:rate:{room_id}:{user_id}");
        if let Err(e) = self
            .rate_limiter
            .check_rate_limit_with_control(
                &rate_key,
                self.rate_limit_config.danmaku_per_second,
                self.rate_limit_config.window_seconds,
                control,
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

        // Validate color format (hex color: #RRGGBB with hex digits only).
        // We use `chars().count()` instead of `.len()` to correctly reject
        // multi-byte UTF-8 strings that happen to have a byte-length of 7.
        // For a valid ASCII `#RRGGBB` string, chars().count() == len() == 7,
        // so the behavior is identical for valid input.
        if !request.color.starts_with('#') || request.color.chars().count() != 7 {
            return Err(Error::InvalidInput("Invalid color format".to_string()));
        }
        if !request.color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::InvalidInput(
                "Invalid color format: must be hex digits".to_string(),
            ));
        }

        // Get username for broadcast
        let username = self.username_for_user(&user_id).await?;

        // Filter content
        let filtered_content = self
            .content_filter
            .filter_danmaku(&request.content)
            .map_err(|e| Error::InvalidInput(format!("Content filter error: {e}")))?;

        // Log before moving values
        info!(
            room_id = %room_id,
            user_id = %user_id,
            "Danmaku sent"
        );

        // Capture position string before moving request.position
        let position_str = request.position.as_str();

        // Create danmaku message
        let danmaku = DanmakuMessage::new(
            room_id,
            user_id,
            filtered_content.clone(),
            request.color,
            request.position,
        );

        // Broadcast danmaku message to room members
        if let Err(e) = self.notification_service.notify_danmaku(
            &room_id,
            &user_id,
            &username,
            &filtered_content,
            position_str,
        ) {
            error!(
                room_id = %room_id,
                user_id = %user_id,
                error = %e,
                "Failed to publish danmaku room event"
            );
        }

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
                room_id = %room_id,
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

    async fn username_for_user(&self, user_id: &UserId) -> Result<String> {
        let username = self
            .user_service
            .get_username(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))?;
        Ok(username)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        cache::{KeyBuilder, UsernameCache},
        config::PasswordComplexityConfig,
        models::{SignupMethod, User},
        repository::{
            RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository,
        },
        service::{
            auth::JwtService, BruteForceProtection, InMemoryTokenBlacklistStore, RateLimiter,
        },
    };

    fn test_user_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> Arc<UserService> {
        Arc::new(UserService::new(
            pool,
            JwtService::new("test-secret-key-for-chat-service-tests-32-chars").expect("jwt"),
            username_cache,
            PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        ))
    }

    fn test_chat_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> ChatService {
        let permission_service = PermissionService::new(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            None,
            PermissionService::DEFAULT_CACHE_SIZE,
            PermissionService::DEFAULT_CACHE_TTL_SECS,
        );
        let room_settings_service = RoomSettingsService::new(
            RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(NotificationService::default()),
            None,
            None,
        );

        ChatService::new(
            Arc::new(ChatRepository::new(pool.clone())),
            ChatRuntime {
                rate_limiter: Arc::new(RateLimiter::local_only("test:chat:".to_string())),
                rate_limit_config: RateLimitConfig::default(),
                content_filter: ContentFilter::new(),
            },
            ChatDependencies {
                permission_service,
                room_settings_service,
                user_service: test_user_service(pool, username_cache),
                notification_service: NotificationService::default(),
            },
        )
    }

    #[tokio::test]
    async fn username_lookup_falls_back_to_database_and_populates_cache() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_cache_miss_user".to_string(),
                None,
                "hash".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        let username_cache = UsernameCache::local_only("test:chat:username:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());

        assert_eq!(
            username_cache
                .get(&user.id)
                .await
                .expect("cache read should succeed"),
            None
        );

        let username = service
            .username_for_user(&user.id)
            .await
            .expect("database fallback should resolve username");

        assert_eq!(username, user.username);
        assert_eq!(
            username_cache
                .get(&user.id)
                .await
                .expect("cache read should succeed"),
            Some(user.username)
        );
    }

    /// Test color validation logic extracted from `send_danmaku`.
    ///
    /// Uses `chars().count()` instead of `.len()` to correctly reject
    /// multi-byte UTF-8 strings that happen to have a byte-length of 7.
    fn validate_color(color: &str) -> std::result::Result<(), &'static str> {
        if !color.starts_with('#') || color.chars().count() != 7 {
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

    /// Test that multi-byte UTF-8 strings with a byte-length of 7 are rejected.
    ///
    /// Before the fix, `color.len() != 7` used byte-length, so a string like
    /// "#" + two 3-byte CJK chars (7 bytes total, 3 chars) would pass the length
    /// check. With `chars().count() != 7`, we correctly reject this.
    #[test]
    fn test_color_validation_multibyte_utf8_rejected() {
        // "#" (1 byte) + 2 CJK characters (3 bytes each) = 7 bytes, but 3 chars
        let tricky = "#\u{4E16}\u{754C}";
        assert_eq!(tricky.len(), 7, "Should be 7 bytes");
        assert_eq!(tricky.chars().count(), 3, "Should be 3 chars");
        assert!(
            validate_color(tricky).is_err(),
            "Multi-byte string with 7 bytes but 3 chars should be rejected"
        );
    }
}
