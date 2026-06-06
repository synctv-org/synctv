//! Chat service for managing room chat messages
//!
//! Handles sending, receiving, and deleting chat messages with rate limiting
//! and content filtering.

use chrono::{DateTime, Utc};
use moka::future::Cache as AsyncCache;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ExecutionControl;
use tracing::{debug, error, info, warn};

use crate::{
    models::{
        AuditAction, AuditTargetType, ChatEventKind, ChatHistoryCursor, ChatHistoryPage, ChatImage,
        ChatMessage, ChatMessageContext, ChatMessageEvent, ChatMessageEventLog, ChatMessageStatus,
        ChatMessageType, ChatMessageWithImages, ChatPlaybackMessagesQuery, ChatReactionUsersCursor,
        ChatReactionUsersPage, ChatReadState, ChatReadStateWithUnread,
        CreateChatImageUploadSession, CreateFileUploadSession, DeleteChatMessage, EditChatMessage,
        FileBlob, FileUploadSession, MarkChatRead, NewChatImage, RoomId, SendChatMessage,
        SetChatReaction, UserId,
    },
    repository::{
        ChatMessageOperationIdempotency, ChatRepository, DeleteChatMessageEventRequest,
        EditChatMessageEventRequest,
    },
    service::{
        audit::{AuditEventParams, AuditService},
        notification::NotificationService,
        ContentFilter, PermissionService, RateLimitConfig, RequestRateLimiterService,
        RoomSettingsService, UserService,
    },
    Error, Result,
};

use super::file_storage::{
    FileStorageCleanupOrigin, FileStorageContext, FileStorageService, FILE_OWNERSHIP_PROOF_KEY,
    FILE_UPLOAD_TOKEN_KEY,
};
use super::file_upload_policies::chat_image_upload_policy;

pub use super::file_upload_policies::MAX_CHAT_IMAGE_SIZE_BYTES;

#[cfg(test)]
use opendal::Operator;

#[cfg(test)]
use super::file_storage::{
    file_content_object_key, file_ownership_proof_digest, file_storage_object_base_path,
    file_storage_public_url, ownership_proof_chunks_from_bytes,
    validate_create_file_upload_session, DatabaseFileStorageService,
    S3CompatibleFileStorageService, S3FileStorageConfig, FILE_UPLOAD_TOKEN_HEADER,
};

/// Maximum allowed chat message length in characters.
/// Used by both the WebSocket handler and the service layer for consistent validation.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 500;
pub const MAX_CHAT_IMAGES_PER_MESSAGE: usize = 10;
const CHAT_REACTION_DETAIL_CACHE_TTL_SECS: u64 = 5;
const CHAT_REACTION_DETAIL_CACHE_CAPACITY: u64 = 1024;

fn max_messages_to_keep_count(max_messages: u64) -> Result<i32> {
    i32::try_from(max_messages)
        .map_err(|_| Error::InvalidInput("chat max_messages exceeds i32::MAX".to_string()))
}

fn validate_chat_playback_query(
    query: ChatPlaybackMessagesQuery,
) -> Result<ChatPlaybackMessagesQuery> {
    let query = query.normalize();
    if query.media_id.is_none() && query.playlist_id.is_none() && query.target.is_none() {
        return Err(Error::InvalidInput(
            "chat playback query requires a media id, playlist id, or target".to_string(),
        ));
    }
    if !query.position_seconds.is_finite()
        || query.position_seconds < 0.0
        || !query.before_seconds.is_finite()
        || query.before_seconds < 0.0
        || !query.after_seconds.is_finite()
        || query.after_seconds < 0.0
    {
        return Err(Error::InvalidInput(
            "chat playback query time window must be non-negative finite seconds".to_string(),
        ));
    }
    if !(1..=500).contains(&query.limit) {
        return Err(Error::InvalidInput(
            "chat playback query limit must be between 1 and 500".to_string(),
        ));
    }
    Ok(query)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChatReactionDetailCacheKey {
    room_id: RoomId,
    message_id: i64,
    reaction_key: String,
    cursor: Option<ChatReactionUsersCursor>,
    limit: i32,
}

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
    file_storage_service: Arc<dyn FileStorageService>,
    audit_service: Option<Arc<AuditService>>,
    /// Local room event bus for chat/domain notifications
    notification_service: NotificationService,
    reaction_detail_cache: AsyncCache<ChatReactionDetailCacheKey, ChatReactionUsersPage>,
}

#[derive(Debug, Clone)]
pub struct ChatMessageEventOutcome {
    pub event: ChatMessageEvent,
    pub inserted: bool,
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
    pub file_storage_service: Arc<dyn FileStorageService>,
    pub audit_service: Option<Arc<AuditService>>,
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
            file_storage_service,
            audit_service,
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
            file_storage_service,
            audit_service,
            notification_service,
            reaction_detail_cache: AsyncCache::builder()
                .max_capacity(CHAT_REACTION_DETAIL_CACHE_CAPACITY)
                .time_to_live(Duration::from_secs(CHAT_REACTION_DETAIL_CACHE_TTL_SECS))
                .support_invalidation_closures()
                .build(),
        }
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Arc<dyn FileStorageService> {
        self.file_storage_service.clone()
    }

    pub async fn create_image_upload_session(
        &self,
        request: CreateChatImageUploadSession,
    ) -> Result<FileUploadSession> {
        self.permission_service
            .check_permission(
                &request.room_id,
                &request.user_id,
                crate::models::RoomPermission::CHAT,
            )
            .await?;

        let room_settings = self.room_settings_service.get(&request.room_id).await?;
        if !room_settings.chat_enabled.0 {
            return Err(Error::Authorization(
                "Chat is disabled in this room".to_string(),
            ));
        }

        self.file_storage_service
            .create_upload_session(chat_image_upload_request_to_file_request(request))
            .await
    }

    pub async fn store_image_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        self.file_storage_service
            .store_upload_object(encoded_object_key, upload_token, content_type, data)
            .await
    }

    pub async fn get_image_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<FileBlob> {
        self.file_storage_service
            .get_object(encoded_object_key, read_token)
            .await
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
        let event = self
            .send_message_event_with_control(
                SendChatMessage {
                    room_id,
                    user_id,
                    client_message_id: None,
                    content,
                    message_type: ChatMessageType::Text,
                    reply_to_message_id: None,
                    metadata: serde_json::Value::Object(Default::default()),
                    images: Vec::new(),
                },
                control,
            )
            .await?;
        Ok(event.message.message)
    }

    pub async fn send_message_event(&self, request: SendChatMessage) -> Result<ChatMessageEvent> {
        Ok(self.send_message_event_outcome(request).await?.event)
    }

    pub async fn send_message_event_outcome(
        &self,
        request: SendChatMessage,
    ) -> Result<ChatMessageEventOutcome> {
        self.send_message_event_with_control_outcome(request, None)
            .await
    }

    pub async fn send_message_event_with_control(
        &self,
        request: SendChatMessage,
        control: Option<&ExecutionControl>,
    ) -> Result<ChatMessageEvent> {
        Ok(self
            .send_message_event_with_control_outcome(request, control)
            .await?
            .event)
    }

    pub async fn send_message_event_with_control_outcome(
        &self,
        mut request: SendChatMessage,
        control: Option<&ExecutionControl>,
    ) -> Result<ChatMessageEventOutcome> {
        let room_id = request.room_id;
        let user_id = request.user_id;

        validate_client_message_id(request.client_message_id.as_deref())?;
        validate_chat_metadata(&request.metadata)?;
        validate_chat_images(&request.images)?;
        if request.content.trim().is_empty() && request.images.is_empty() {
            return Err(Error::InvalidInput(
                "empty chat message: content or image is required".to_string(),
            ));
        }
        if request.content.chars().count() > MAX_CHAT_MESSAGE_CHARS {
            return Err(Error::InvalidInput(format!(
                "Message content must be at most {MAX_CHAT_MESSAGE_CHARS} characters"
            )));
        }

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

        let request_hash = chat_send_request_hash(&request)?;
        if let Some(client_message_id) = request.client_message_id.as_deref() {
            if let Some(event) = self
                .chat_repository
                .replay_idempotent_send_event(&room_id, &user_id, client_message_id, &request_hash)
                .await?
            {
                info!(
                    room_id = %room_id,
                    user_id = %user_id,
                    message_id = %event.event.message.message.id,
                    event_id = %event.event.event_id,
                    inserted = false,
                    "Chat message send replayed from idempotency record"
                );
                return Ok(ChatMessageEventOutcome {
                    event: event.event,
                    inserted: false,
                });
            }
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

        let storage_scope = chat_file_storage_scope(room_id, user_id);
        request.images = self
            .file_storage_service
            .prepare_files(
                FileStorageContext {
                    user_id,
                    storage_scope: &storage_scope,
                    client_request_id: request.client_message_id.as_deref(),
                },
                request.images,
            )
            .await?;
        validate_chat_images(&request.images)?;
        strip_internal_chat_image_metadata(&mut request.images);

        let reply_to_message_created_at = self
            .ensure_reply_target_visible(&room_id, request.reply_to_message_id)
            .await?;

        // Get username
        let username = self.username_for_user(&user_id).await?;

        // Filter content
        let filtered_content = if request.content.trim().is_empty() {
            String::new()
        } else {
            self.content_filter
                .filter_chat(&request.content)
                .map_err(|e| Error::InvalidInput(format!("Content filter error: {e}")))?
        };
        if filtered_content.trim().is_empty() && request.images.is_empty() {
            return Err(Error::InvalidInput(
                "empty chat message: content or image is required".to_string(),
            ));
        }
        request.content = filtered_content.clone();
        if !request.images.is_empty() {
            request.message_type = ChatMessageType::Image;
        }

        // Create message
        let mut message = ChatMessage::new(room_id, user_id, filtered_content.clone());
        message.client_message_id = request.client_message_id.clone();
        message.message_type = request.message_type;
        message.reply_to_message_id = request.reply_to_message_id;
        message.reply_to_message_created_at = reply_to_message_created_at;
        message.metadata = request.metadata.clone();

        // Persist to database
        let occurred_at = Utc::now();
        let event_id = synctv_common::snanoid!(16);
        let created = self
            .chat_repository
            .insert_message_event_idempotent(
                &message,
                &request.images,
                &request_hash,
                &event_id,
                occurred_at,
            )
            .await?;
        let event = created.event.event;

        info!(
            room_id = %room_id,
            user_id = %user_id,
            message_id = %event.message.message.id,
            event_id = %event.event_id,
            inserted = created.inserted,
            "Chat message sent"
        );

        if created.inserted {
            let subscriber_count = self.notification_service.notify_chat_message(
                &room_id,
                &event.message.message.id.to_string(),
                &user_id,
                &username,
                &filtered_content,
            );
            if subscriber_count == 0 {
                debug!(
                    room_id = %room_id,
                    user_id = %user_id,
                    message_id = %event.message.message.id,
                    "Chat message room event had no local subscribers"
                );
            }
        }

        Ok(ChatMessageEventOutcome {
            event,
            inserted: created.inserted,
        })
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
        let cursor = cursor.map(|(created_at, id)| ChatHistoryCursor { created_at, id });
        let (messages, next) = self
            .chat_repository
            .list_by_room_cursor(room_id, cursor, limit, true)
            .await?;
        Ok((
            messages.into_iter().map(|m| m.message).collect(),
            next.map(|cursor| (cursor.created_at, cursor.id)),
        ))
    }

    pub async fn get_history_with_images(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
    ) -> Result<(Vec<ChatMessageWithImages>, Option<ChatHistoryCursor>)> {
        self.get_history_with_images_for_viewer(room_id, cursor, limit, include_deleted, None)
            .await
    }

    pub async fn get_history_with_images_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<(Vec<ChatMessageWithImages>, Option<ChatHistoryCursor>)> {
        self.chat_repository
            .list_by_room_cursor_for_viewer(room_id, cursor, limit, include_deleted, viewer_user_id)
            .await
    }

    pub async fn get_history_page_with_images_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatHistoryPage> {
        self.chat_repository
            .list_history_page_for_viewer(room_id, cursor, limit, include_deleted, viewer_user_id)
            .await
    }

    pub async fn get_playback_messages_with_images(
        &self,
        query: ChatPlaybackMessagesQuery,
    ) -> Result<Vec<ChatMessageWithImages>> {
        self.get_playback_messages_with_images_for_viewer(query, None)
            .await
    }

    pub async fn get_playback_messages_with_images_for_viewer(
        &self,
        query: ChatPlaybackMessagesQuery,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithImages>> {
        let query = validate_chat_playback_query(query)?;
        self.chat_repository
            .list_playback_messages_for_viewer(&query, viewer_user_id)
            .await
    }

    pub async fn get_message_with_images(
        &self,
        room_id: &RoomId,
        message_id: i64,
        include_deleted: bool,
    ) -> Result<ChatMessageWithImages> {
        self.get_message_with_images_for_viewer(room_id, message_id, include_deleted, None)
            .await
    }

    pub async fn get_message_with_images_for_viewer(
        &self,
        room_id: &RoomId,
        message_id: i64,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatMessageWithImages> {
        let message = self
            .chat_repository
            .get_with_images_by_room_and_id_for_viewer(room_id, message_id, viewer_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if message.message.status == ChatMessageStatus::Deleted && !include_deleted {
            return Err(Error::NotFound("Message not found".to_string()));
        }
        Ok(message)
    }

    pub async fn get_message_context(
        &self,
        room_id: &RoomId,
        message_id: i64,
        before_limit: i32,
        after_limit: i32,
        include_deleted: bool,
    ) -> Result<ChatMessageContext> {
        self.get_message_context_for_viewer(
            room_id,
            message_id,
            before_limit,
            after_limit,
            include_deleted,
            None,
        )
        .await
    }

    pub async fn get_message_context_for_viewer(
        &self,
        room_id: &RoomId,
        message_id: i64,
        before_limit: i32,
        after_limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatMessageContext> {
        self.chat_repository
            .list_context_around_message_for_viewer(
                room_id,
                message_id,
                before_limit,
                after_limit,
                include_deleted,
                viewer_user_id,
            )
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))
    }

    pub async fn get_events_after(
        &self,
        room_id: &RoomId,
        after_event_id: Option<&str>,
        limit: i32,
    ) -> Result<Vec<ChatMessageEventLog>> {
        match self
            .chat_repository
            .list_events_after(room_id, after_event_id, limit)
            .await
        {
            Err(Error::NotFound(message)) if message == "Chat event not found" => {
                Err(Error::InvalidInput("Invalid chat event cursor".to_string()))
            }
            result => result,
        }
    }

    pub async fn get_events_after_sequence(
        &self,
        room_id: &RoomId,
        after_sequence: i64,
        limit: i32,
    ) -> Result<Vec<ChatMessageEventLog>> {
        self.chat_repository
            .list_events_after_sequence(room_id, after_sequence, limit)
            .await
    }

    pub async fn is_event_sequence_retained_for_room(
        &self,
        room_id: &RoomId,
        after_sequence: i64,
    ) -> Result<bool> {
        let after_sequence = after_sequence.max(0);

        let Some((min_sequence, _max_sequence)) = self
            .chat_repository
            .retained_chat_event_sequence_bounds(room_id)
            .await?
        else {
            return Ok(after_sequence == 0);
        };

        Ok(min_sequence <= after_sequence.saturating_add(1))
    }

    pub async fn mark_read(&self, request: MarkChatRead) -> Result<ChatReadStateWithUnread> {
        self.permission_service
            .check_permission(
                &request.room_id,
                &request.user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;

        let message = self
            .chat_repository
            .get_by_room_and_id(&request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if message.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }

        let event = self
            .chat_repository
            .created_event_for_message(&request.room_id, message.id, message.created_at)
            .await?;
        let current = self
            .chat_repository
            .get_read_state(&request.room_id, &request.user_id)
            .await?;

        let state = match current {
            Some(state) if read_state_covers_message(Some(&state), &message, event.as_ref()) => {
                state
            }
            _ => {
                self.chat_repository
                    .upsert_read_state(
                        &request.room_id,
                        &request.user_id,
                        message.id,
                        message.created_at,
                        event.as_ref().map(|event| event.event.event_id.as_str()),
                        event.as_ref().map(|event| event.sequence),
                    )
                    .await?
            }
        };
        let unread_count = self
            .chat_repository
            .unread_count_after_read_state(&request.room_id, &request.user_id, Some(&state))
            .await?;
        Ok(ChatReadStateWithUnread {
            state,
            unread_count,
        })
    }

    pub async fn get_read_state(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<ChatReadStateWithUnread> {
        self.permission_service
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;

        let state = self
            .chat_repository
            .get_read_state(room_id, user_id)
            .await?;
        let unread_count = self
            .chat_repository
            .unread_count_after_read_state(room_id, user_id, state.as_ref())
            .await?;
        Ok(ChatReadStateWithUnread {
            state: state.unwrap_or_else(|| empty_read_state(*room_id, *user_id)),
            unread_count,
        })
    }

    pub async fn edit_message(&self, request: EditChatMessage) -> Result<ChatMessageEvent> {
        Ok(self.edit_message_outcome(request).await?.event)
    }

    pub async fn edit_message_outcome(
        &self,
        request: EditChatMessage,
    ) -> Result<ChatMessageEventOutcome> {
        validate_client_operation_id(request.client_operation_id.as_deref())?;
        validate_chat_metadata(&request.metadata)?;
        if request.content.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Message content cannot be empty".to_string(),
            ));
        }
        if request.content.chars().count() > MAX_CHAT_MESSAGE_CHARS {
            return Err(Error::InvalidInput(format!(
                "Message content must be at most {MAX_CHAT_MESSAGE_CHARS} characters"
            )));
        }

        self.permission_service
            .check_permission(
                &request.room_id,
                &request.user_id,
                crate::models::RoomPermission::CHAT,
            )
            .await?;

        let request_hash = chat_edit_request_hash(&request)?;
        if let Some(client_operation_id) = request.client_operation_id.as_deref() {
            if let Some(event) = self
                .chat_repository
                .replay_message_operation_event(
                    &request.room_id,
                    &request.user_id,
                    client_operation_id,
                    ChatEventKind::Edited,
                    &request_hash,
                )
                .await?
            {
                return Ok(ChatMessageEventOutcome {
                    event: event.event,
                    inserted: false,
                });
            }
        }

        let current = self
            .chat_repository
            .get_by_room_and_id(&request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        ensure_message_owner(&current, &request.user_id)?;
        if current.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict(
                "Message has already been deleted".to_string(),
            ));
        }

        let filtered_content = self
            .content_filter
            .filter_chat(&request.content)
            .map_err(|e| Error::InvalidInput(format!("Content filter error: {e}")))?;
        if filtered_content.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Message content cannot be empty".to_string(),
            ));
        }
        if let Some(expected_version) = request.expected_version {
            if expected_version != current.version {
                if current.version > expected_version
                    && current.content == filtered_content
                    && current.metadata == request.metadata
                {
                    let event = self
                        .existing_edit_event(&request.room_id, &current, &request.user_id)
                        .await?;
                    return Ok(ChatMessageEventOutcome {
                        event,
                        inserted: false,
                    });
                }
                return Err(Error::OptimisticLockConflict);
            }
        }
        let operation = request
            .client_operation_id
            .as_deref()
            .map(|client_operation_id| ChatMessageOperationIdempotency {
                client_operation_id,
                operation_kind: ChatEventKind::Edited,
                request_hash: &request_hash,
                message_id: request.message_id,
                message_created_at: current.created_at,
            });
        let updated = self
            .chat_repository
            .edit_with_event(EditChatMessageEventRequest {
                room_id: &request.room_id,
                message_id: request.message_id,
                message_created_at: current.created_at,
                content: &filtered_content,
                metadata: &request.metadata,
                expected_version: request.expected_version,
                event_id: &synctv_common::snanoid!(16),
                actor_user_id: &request.user_id,
                occurred_at: Utc::now(),
                operation: operation.as_ref(),
            })
            .await?;
        let Some(updated) = updated else {
            let current = self
                .chat_repository
                .get_by_room_and_id(&request.room_id, request.message_id)
                .await?
                .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
            if current.status == ChatMessageStatus::Deleted {
                return Err(Error::Conflict(
                    "Message has already been deleted".to_string(),
                ));
            }
            if let Some(expected_version) = request.expected_version {
                if current.version > expected_version
                    && current.content == filtered_content
                    && current.metadata == request.metadata
                {
                    let event = self
                        .existing_edit_event(&request.room_id, &current, &request.user_id)
                        .await?;
                    return Ok(ChatMessageEventOutcome {
                        event,
                        inserted: false,
                    });
                }
            }
            return Err(Error::OptimisticLockConflict);
        };

        Ok(ChatMessageEventOutcome {
            event: updated.event.event,
            inserted: updated.inserted,
        })
    }

    pub async fn delete_message_event(
        &self,
        request: DeleteChatMessage,
    ) -> Result<ChatMessageEvent> {
        Ok(self.delete_message_event_outcome(request).await?.event)
    }

    pub async fn delete_message_event_outcome(
        &self,
        request: DeleteChatMessage,
    ) -> Result<ChatMessageEventOutcome> {
        let current_with_images = self
            .chat_repository
            .get_with_images_by_room_and_id(&request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        let current = &current_with_images.message;

        let is_sender = current.user_id.as_ref() == Some(&request.user_id);
        if !is_sender {
            self.permission_service
                .check_permission(
                    &request.room_id,
                    &request.user_id,
                    crate::models::RoomPermission::DELETE_CHAT,
                )
                .await?;
        }
        validate_client_operation_id(request.client_operation_id.as_deref())?;
        let request_hash = chat_delete_request_hash(&request)?;
        if let Some(client_operation_id) = request.client_operation_id.as_deref() {
            if let Some(event) = self
                .chat_repository
                .replay_message_operation_event(
                    &request.room_id,
                    &request.user_id,
                    client_operation_id,
                    ChatEventKind::Deleted,
                    &request_hash,
                )
                .await?
            {
                return Ok(ChatMessageEventOutcome {
                    event: event.event,
                    inserted: false,
                });
            }
        }
        if current.status == ChatMessageStatus::Deleted {
            if let Some(event) = self.existing_delete_event(&request, current).await? {
                return Ok(ChatMessageEventOutcome {
                    event,
                    inserted: false,
                });
            }
            return Err(Error::Conflict(
                "Message has already been deleted".to_string(),
            ));
        }
        if request
            .expected_version
            .is_some_and(|version| version != current.version)
        {
            return Err(Error::OptimisticLockConflict);
        }

        let operation = request
            .client_operation_id
            .as_deref()
            .map(|client_operation_id| ChatMessageOperationIdempotency {
                client_operation_id,
                operation_kind: ChatEventKind::Deleted,
                request_hash: &request_hash,
                message_id: request.message_id,
                message_created_at: current.created_at,
            });
        let deleted = self
            .chat_repository
            .soft_delete_with_event(DeleteChatMessageEventRequest {
                room_id: &request.room_id,
                message_id: request.message_id,
                message_created_at: current.created_at,
                deleted_by: &request.user_id,
                reason: request.reason.as_deref(),
                expected_version: request.expected_version,
                event_id: &synctv_common::snanoid!(16),
                occurred_at: Utc::now(),
                operation: operation.as_ref(),
            })
            .await?;
        let Some(deleted) = deleted else {
            let current = self
                .chat_repository
                .get_by_room_and_id(&request.room_id, request.message_id)
                .await?
                .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
            if let Some(event) = self.existing_delete_event(&request, &current).await? {
                return Ok(ChatMessageEventOutcome {
                    event,
                    inserted: false,
                });
            }
            if current.status == ChatMessageStatus::Deleted {
                return Err(Error::Conflict(
                    "Message has already been deleted".to_string(),
                ));
            }
            return Err(Error::OptimisticLockConflict);
        };

        if deleted.inserted {
            if !is_sender {
                self.audit_chat_management_delete(&request, current, &deleted.event.event)
                    .await;
            }

            let image_file_references = current_with_images
                .images
                .iter()
                .map(ChatImage::file_reference_target)
                .collect::<Vec<_>>();
            if let Err(error) = self
                .file_storage_service
                .delete_files(
                    FileStorageCleanupOrigin::ReferenceReleased,
                    &image_file_references,
                )
                .await
            {
                warn!(
                    room_id = %request.room_id,
                    message_id = %request.message_id,
                    error = %error,
                    "chat image cleanup failed after message deletion"
                );
                if let Err(enqueue_error) = crate::repository::FileStorageRepository::new(
                    self.chat_repository.pool().clone(),
                )
                .enqueue_cleanup_jobs(
                    FileStorageCleanupOrigin::ReferenceReleased.as_str(),
                    &image_file_references,
                    &serde_json::Value::Object(Default::default()),
                    &error.to_string(),
                )
                .await
                {
                    warn!(
                        room_id = %request.room_id,
                        message_id = %request.message_id,
                        error = %enqueue_error,
                        "failed to enqueue chat image cleanup retry after message deletion"
                    );
                }
            }
        }

        Ok(ChatMessageEventOutcome {
            event: deleted.event.event,
            inserted: deleted.inserted,
        })
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
        self.delete_message_event(DeleteChatMessage {
            room_id: *room_id,
            message_id,
            user_id: *user_id,
            client_operation_id: None,
            reason: None,
            expected_version: None,
        })
        .await
        .map(|_| true)
    }

    pub async fn set_reaction_event_outcome(
        &self,
        request: SetChatReaction,
    ) -> Result<ChatMessageEventOutcome> {
        self.permission_service
            .check_permission(
                &request.room_id,
                &request.user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;
        validate_chat_reaction_key(&request.reaction_key)?;

        let event = self
            .chat_repository
            .set_reaction_with_event(&request, &synctv_common::snanoid!(16), Utc::now())
            .await?;
        self.invalidate_reaction_detail_cache(
            &request.room_id,
            request.message_id,
            &request.reaction_key,
        )
        .await;

        info!(
            room_id = %request.room_id,
            user_id = %request.user_id,
            message_id = %request.message_id,
            reaction_key = %request.reaction_key,
            enabled = request.enabled,
            event_id = %event.event.event_id,
            "Chat message reaction changed"
        );

        Ok(ChatMessageEventOutcome {
            event: event.event,
            inserted: true,
        })
    }

    pub async fn list_reaction_users(
        &self,
        room_id: &RoomId,
        message_id: i64,
        viewer_user_id: &UserId,
        reaction_key: &str,
        cursor: Option<ChatReactionUsersCursor>,
        limit: i32,
    ) -> Result<ChatReactionUsersPage> {
        self.permission_service
            .check_permission(
                room_id,
                viewer_user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;
        validate_chat_reaction_key(reaction_key)?;
        let limit = limit.clamp(1, 100);
        let key = ChatReactionDetailCacheKey {
            room_id: *room_id,
            message_id,
            reaction_key: reaction_key.to_string(),
            cursor,
            limit,
        };
        if let Some(page) = self.reaction_detail_cache.get(&key).await {
            return Ok(page);
        }

        let page = self
            .chat_repository
            .list_reaction_users(room_id, message_id, reaction_key, cursor, limit)
            .await?;
        self.reaction_detail_cache.insert(key, page.clone()).await;
        Ok(page)
    }

    async fn invalidate_reaction_detail_cache(
        &self,
        room_id: &RoomId,
        message_id: i64,
        reaction_key: &str,
    ) {
        let room_id = *room_id;
        let reaction_key = reaction_key.to_string();
        if let Err(error) = self
            .reaction_detail_cache
            .invalidate_entries_if(move |key, _| {
                key.room_id == room_id
                    && key.message_id == message_id
                    && key.reaction_key == reaction_key
            })
        {
            debug!(%error, "Failed to invalidate chat reaction detail cache");
        }
        self.reaction_detail_cache.run_pending_tasks().await;
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
            .cleanup_old_messages(room_id, max_messages_to_keep_count(max_messages)?)
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
                max_messages_to_keep_count(max_messages)?,
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

    async fn ensure_reply_target_visible(
        &self,
        room_id: &RoomId,
        reply_to_message_id: Option<i64>,
    ) -> Result<Option<DateTime<Utc>>> {
        let Some(reply_to_message_id) = reply_to_message_id else {
            return Ok(None);
        };
        let reply = self
            .chat_repository
            .get_by_room_and_id(room_id, reply_to_message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Reply target message not found".to_string()))?;
        if reply.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict(
                "Reply target message has been deleted".to_string(),
            ));
        }
        Ok(Some(reply.created_at))
    }

    async fn existing_edit_event(
        &self,
        room_id: &RoomId,
        current: &ChatMessage,
        user_id: &UserId,
    ) -> Result<ChatMessageEvent> {
        let existing = self
            .chat_repository
            .latest_event_for_message(room_id, current.id, current.created_at)
            .await?
            .filter(|event| {
                event.event.kind == ChatEventKind::Edited
                    && event.event.actor_user_id == *user_id
                    && event.event.message.message.version == current.version
            })
            .ok_or(Error::OptimisticLockConflict)?;
        Ok(existing.event)
    }

    async fn existing_delete_event(
        &self,
        request: &DeleteChatMessage,
        current: &ChatMessage,
    ) -> Result<Option<ChatMessageEvent>> {
        if current.status == ChatMessageStatus::Deleted
            && current.deleted_by.as_ref() == Some(&request.user_id)
            && current.delete_reason.as_deref() == request.reason.as_deref()
        {
            let existing = self
                .chat_repository
                .latest_event_for_message(&request.room_id, current.id, current.created_at)
                .await?
                .filter(|event| event.event.kind == ChatEventKind::Deleted)
                .ok_or_else(|| {
                    Error::Internal(
                        "Deleted chat message is missing its durable delete event".to_string(),
                    )
                })?;
            return Ok(Some(existing.event));
        }

        Ok(None)
    }

    async fn audit_chat_management_delete(
        &self,
        request: &DeleteChatMessage,
        original: &ChatMessage,
        event: &ChatMessageEvent,
    ) {
        let Some(audit) = &self.audit_service else {
            return;
        };

        let actor_username = match self.user_service.get_username(&request.user_id).await {
            Ok(Some(username)) => username,
            Ok(None) => {
                warn!(
                    room_id = %request.room_id,
                    message_id = %request.message_id,
                    actor_user_id = %request.user_id,
                    "skipped chat delete audit because actor user was not found"
                );
                return;
            }
            Err(error) => {
                warn!(
                    room_id = %request.room_id,
                    message_id = %request.message_id,
                    actor_user_id = %request.user_id,
                    error = %error,
                    "skipped chat delete audit because actor username lookup failed"
                );
                return;
            }
        };
        let target_id = format!("{}:{}", request.room_id, request.message_id);
        let details = json!({
            "room_id": request.room_id.to_string(),
            "message_id": request.message_id,
            "message_created_at": original.created_at,
            "original_author_id": original.user_id.map(|user_id| user_id.to_string()),
            "deleted_by": request.user_id.to_string(),
            "reason": request.reason.as_deref(),
            "event_id": event.event_id,
            "client_operation_id": request.client_operation_id.as_deref(),
        });

        if let Err(error) = audit
            .log(AuditEventParams {
                actor_id: request.user_id.to_string(),
                actor_username,
                action: AuditAction::ChatMessageDeleted,
                target_type: AuditTargetType::ChatMessage,
                target_id: Some(target_id),
                details,
                ip_address: None,
                user_agent: None,
            })
            .await
        {
            warn!(
                room_id = %request.room_id,
                message_id = %request.message_id,
                actor_user_id = %request.user_id,
                error = %error,
                "failed to write chat delete audit log"
            );
        }
    }
}

fn empty_read_state(room_id: RoomId, user_id: UserId) -> ChatReadState {
    ChatReadState {
        room_id,
        user_id,
        last_read_message_id: None,
        last_read_message_created_at: None,
        last_read_event_id: None,
        last_read_event_sequence: None,
        updated_at: Utc::now(),
    }
}

fn read_state_covers_message(
    state: Option<&ChatReadState>,
    message: &ChatMessage,
    event: Option<&ChatMessageEventLog>,
) -> bool {
    let Some(state) = state else {
        return false;
    };
    if let (Some(current_sequence), Some(target)) = (
        state.last_read_event_sequence,
        event.map(|event| event.sequence),
    ) {
        if current_sequence >= target {
            return true;
        }
    }
    if let (Some(message_id), Some(created_at)) = (
        state.last_read_message_id,
        state.last_read_message_created_at,
    ) {
        let current_cursor = (created_at, message_id);
        let target_cursor = (message.created_at, message.id);
        return current_cursor > target_cursor
            || (event.is_none() && current_cursor == target_cursor);
    }
    false
}

fn validate_client_message_id(client_message_id: Option<&str>) -> Result<()> {
    if let Some(id) = client_message_id {
        let len = id.chars().count();
        if !(1..=128).contains(&len) {
            return Err(Error::InvalidInput(
                "client_message_id must be between 1 and 128 characters".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_client_operation_id(client_operation_id: Option<&str>) -> Result<()> {
    if let Some(id) = client_operation_id {
        let len = id.chars().count();
        if !(1..=128).contains(&len) {
            return Err(Error::InvalidInput(
                "client_operation_id must be between 1 and 128 characters".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_chat_reaction_key(key: &str) -> Result<()> {
    let len = key.chars().count();
    if !(1..=64).contains(&len) {
        return Err(Error::InvalidInput(
            "reaction_key must be between 1 and 64 characters".to_string(),
        ));
    }
    if key.trim() != key || key.chars().any(char::is_control) {
        return Err(Error::InvalidInput(
            "reaction_key must not contain control characters or surrounding whitespace"
                .to_string(),
        ));
    }
    Ok(())
}

fn chat_image_upload_request_to_file_request(
    request: CreateChatImageUploadSession,
) -> CreateFileUploadSession {
    CreateFileUploadSession {
        user_id: request.user_id,
        storage_scope: chat_file_storage_scope(request.room_id, request.user_id),
        client_file_id: request.client_image_id,
        mime_type: request.mime_type,
        size_bytes: request.size_bytes,
        width: request.width,
        height: request.height,
        checksum_sha256: request.checksum_sha256,
        metadata: request.metadata,
        policy: chat_image_upload_policy(),
    }
}

fn chat_file_storage_scope(room_id: RoomId, user_id: UserId) -> String {
    format!("rooms/{}/users/{}", room_id.as_i64(), user_id.as_i64())
}

fn validate_chat_metadata(metadata: &serde_json::Value) -> Result<()> {
    if !metadata.is_object() {
        return Err(Error::InvalidInput(
            "chat metadata must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

fn validate_chat_image_mime_type(mime_type: &str) -> Result<()> {
    let policy = chat_image_upload_policy();
    let normalized = mime_type.trim().to_ascii_lowercase();
    let allowed_exact = policy
        .allowed_mime_types
        .iter()
        .any(|allowed| normalized == allowed.trim().to_ascii_lowercase());
    let allowed_prefix = policy
        .allowed_mime_prefixes
        .iter()
        .any(|prefix| normalized.starts_with(&prefix.trim().to_ascii_lowercase()));
    if allowed_exact || allowed_prefix {
        return Ok(());
    }
    Err(Error::InvalidInput(
        "chat image mime_type is not allowed".to_string(),
    ))
}

fn validate_chat_images(images: &[NewChatImage]) -> Result<()> {
    if images.len() > MAX_CHAT_IMAGES_PER_MESSAGE {
        return Err(Error::InvalidInput(format!(
            "Chat messages support at most {MAX_CHAT_IMAGES_PER_MESSAGE} images"
        )));
    }
    let mut image_ids = std::collections::HashSet::with_capacity(images.len());
    let mut object_keys = std::collections::HashSet::with_capacity(images.len());
    for image in images {
        if image.id.trim().is_empty() || image.id.chars().count() > 128 {
            return Err(Error::InvalidInput(
                "image id must be between 1 and 128 characters".to_string(),
            ));
        }
        if image.storage_backend.trim().is_empty() || image.object_key.trim().is_empty() {
            return Err(Error::InvalidInput(
                "file storage_backend and object_key are required".to_string(),
            ));
        }
        if !image_ids.insert(image.id.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate image id in one message".to_string(),
            ));
        }
        if !object_keys.insert(image.object_key.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate image object_key in one message".to_string(),
            ));
        }
        if image.size_bytes.is_some_and(|size| size <= 0)
            || image.width.is_some_and(|width| width <= 0)
            || image.height.is_some_and(|height| height <= 0)
        {
            return Err(Error::InvalidInput(
                "image size and dimensions must be positive".to_string(),
            ));
        }
        if let Some(mime_type) = &image.mime_type {
            validate_chat_image_mime_type(mime_type)?;
        }
        validate_chat_metadata(&image.metadata)?;
    }
    Ok(())
}

fn strip_internal_chat_image_metadata(images: &mut [NewChatImage]) {
    for image in images {
        if let Some(metadata) = image.metadata.as_object_mut() {
            metadata.remove(FILE_UPLOAD_TOKEN_KEY);
            metadata.remove(FILE_OWNERSHIP_PROOF_KEY);
        }
    }
}

fn chat_send_request_hash(request: &SendChatMessage) -> Result<String> {
    let payload = json!({
        "content": request.content,
        "message_type": request.message_type,
        "reply_to_message_id": request.reply_to_message_id,
        "metadata": request.metadata,
        "images": request.images,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn chat_edit_request_hash(request: &EditChatMessage) -> Result<String> {
    let payload = json!({
        "message_id": request.message_id,
        "content": request.content,
        "metadata": request.metadata,
        "expected_version": request.expected_version,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn chat_delete_request_hash(request: &DeleteChatMessage) -> Result<String> {
    let payload = json!({
        "message_id": request.message_id,
        "reason": request.reason,
        "expected_version": request.expected_version,
    });
    let bytes = serde_json::to_vec(&payload)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn ensure_message_owner(message: &ChatMessage, user_id: &UserId) -> Result<()> {
    if message.user_id.as_ref() == Some(user_id) {
        Ok(())
    } else {
        Err(Error::Authorization(
            "Only the sender can edit this message".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{
        cache::{KeyBuilder, UsernameCache},
        models::{SignupMethod, User},
        repository::{
            FileStorageRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository,
            UserRepository,
        },
        service::{
            auth::JwtService, BruteForceProtection, DisabledFileStorageService,
            InMemoryTokenBlacklistStore, RateLimiter, RoomService,
        },
    };
    use tokio::sync::Barrier;

    const TEST_FILE_STORAGE_SCOPE: &str = "rooms/1/users/1";

    #[derive(Debug)]
    struct PrefixingFileStorageService;

    #[async_trait::async_trait]
    impl FileStorageService for PrefixingFileStorageService {
        fn backend_name(&self) -> &'static str {
            "test-storage"
        }

        async fn create_upload_session(
            &self,
            mut request: CreateFileUploadSession,
        ) -> Result<FileUploadSession> {
            validate_create_file_upload_session(&request)?;
            let id = request
                .client_file_id
                .take()
                .unwrap_or_else(|| "custom-image".to_string());
            Ok(FileUploadSession {
                file: NewChatImage {
                    id: id.clone(),
                    storage_backend: "test-storage".to_string(),
                    object_key: format!("normalized/uploads/{id}"),
                    url: Some(format!("https://cdn.invalid/uploads/{id}")),
                    mime_type: Some(request.mime_type),
                    size_bytes: Some(request.size_bytes),
                    width: request.width,
                    height: request.height,
                    metadata: request.metadata,
                },
                upload_required: true,
                ownership_proof_required: false,
                ownership_proof_nonce: None,
                ownership_proof_ranges: Vec::new(),
                ownership_proof_metadata_key: None,
                upload_url: Some(format!("https://upload.invalid/{id}")),
                upload_method: Some("PUT".to_string()),
                upload_headers: Default::default(),
                expires_at: Some(Utc::now()),
                max_size_bytes: MAX_CHAT_IMAGE_SIZE_BYTES,
            })
        }

        async fn prepare_files(
            &self,
            context: FileStorageContext<'_>,
            images: Vec<NewChatImage>,
        ) -> Result<Vec<NewChatImage>> {
            assert!(context.user_id.as_i64() > 0);
            assert!(!context.storage_scope.is_empty());
            validate_chat_images(&images)?;
            Ok(images
                .into_iter()
                .map(|mut image| {
                    image.storage_backend = "test-storage".to_string();
                    image.object_key = format!("normalized/{}", image.object_key);
                    image.url = Some(format!("https://cdn.invalid/{}", image.id));
                    image
                })
                .collect())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingFileStorageService {
        deleted_object_keys: Mutex<Vec<String>>,
        deleted_origins: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl FileStorageService for RecordingFileStorageService {
        fn backend_name(&self) -> &'static str {
            "test-storage"
        }

        async fn create_upload_session(
            &self,
            mut request: CreateFileUploadSession,
        ) -> Result<FileUploadSession> {
            validate_create_file_upload_session(&request)?;
            let id = request
                .client_file_id
                .take()
                .unwrap_or_else(|| "custom-image".to_string());
            Ok(FileUploadSession {
                file: NewChatImage {
                    id: id.clone(),
                    storage_backend: "test-storage".to_string(),
                    object_key: format!("normalized/uploads/{id}"),
                    url: Some(format!("https://cdn.invalid/uploads/{id}")),
                    mime_type: Some(request.mime_type),
                    size_bytes: Some(request.size_bytes),
                    width: request.width,
                    height: request.height,
                    metadata: request.metadata,
                },
                upload_required: true,
                ownership_proof_required: false,
                ownership_proof_nonce: None,
                ownership_proof_ranges: Vec::new(),
                ownership_proof_metadata_key: None,
                upload_url: Some(format!("https://upload.invalid/{id}")),
                upload_method: Some("PUT".to_string()),
                upload_headers: Default::default(),
                expires_at: Some(Utc::now()),
                max_size_bytes: MAX_CHAT_IMAGE_SIZE_BYTES,
            })
        }

        async fn prepare_files(
            &self,
            context: FileStorageContext<'_>,
            images: Vec<NewChatImage>,
        ) -> Result<Vec<NewChatImage>> {
            assert!(context.user_id.as_i64() > 0);
            assert!(!context.storage_scope.is_empty());
            validate_chat_images(&images)?;
            Ok(images
                .into_iter()
                .map(|mut image| {
                    image.storage_backend = "test-storage".to_string();
                    image.object_key = format!("normalized/{}", image.object_key);
                    image.url = Some(format!("https://cdn.invalid/{}", image.id));
                    image
                })
                .collect())
        }

        async fn delete_files(
            &self,
            origin: FileStorageCleanupOrigin,
            files: &[crate::models::FileReferenceTarget],
        ) -> Result<()> {
            let mut deleted = self.deleted_object_keys.lock().unwrap();
            deleted.extend(files.iter().map(|file| file.object_key.clone()));
            let mut origins = self.deleted_origins.lock().unwrap();
            origins.extend(files.iter().map(|_| origin.as_str().to_string()));
            Ok(())
        }
    }

    fn test_user_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> Arc<UserService> {
        Arc::new(UserService::new_for_tests(
            pool,
            JwtService::new("test-secret-key-for-chat-service-tests-32-chars").expect("jwt"),
            username_cache,
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        ))
    }

    #[test]
    fn validate_chat_metadata_rejects_non_object_values() {
        let error = validate_chat_metadata(&serde_json::json!(["tag"]))
            .expect_err("chat metadata should be an object");

        assert!(
            matches!(error, Error::InvalidInput(message) if message == "chat metadata must be a JSON object")
        );
    }

    #[test]
    fn validate_chat_images_rejects_non_object_metadata() {
        let image = NewChatImage {
            id: "img-test".to_string(),
            storage_backend: "database".to_string(),
            object_key: "rooms/1/chat/img-test".to_string(),
            url: None,
            mime_type: Some("image/png".to_string()),
            size_bytes: Some(1024),
            width: Some(32),
            height: Some(32),
            metadata: serde_json::json!(["tag"]),
        };

        let error = validate_chat_images(&[image]).expect_err("image metadata should be object");
        assert!(
            matches!(error, Error::InvalidInput(message) if message == "chat metadata must be a JSON object")
        );
    }

    fn playback_query_with_context() -> ChatPlaybackMessagesQuery {
        ChatPlaybackMessagesQuery {
            room_id: RoomId::expect_positive(1),
            media_id: Some(crate::models::MediaId::expect_positive(2)),
            playlist_id: None,
            target: None,
            position_seconds: 10.0,
            before_seconds: 1.0,
            after_seconds: 2.0,
            limit: 100,
            include_deleted: false,
        }
    }

    #[test]
    fn chat_playback_query_normalize_treats_empty_target_as_absent() {
        let query = ChatPlaybackMessagesQuery {
            media_id: None,
            target: Some(Vec::new()),
            ..playback_query_with_context()
        }
        .normalize();

        assert!(query.target.is_none());
    }

    #[test]
    fn validate_chat_playback_query_rejects_empty_context_after_normalize() {
        let query = ChatPlaybackMessagesQuery {
            media_id: None,
            target: Some(Vec::new()),
            ..playback_query_with_context()
        };

        assert!(matches!(
            validate_chat_playback_query(query),
            Err(Error::InvalidInput(message)) if message.contains("requires")
        ));
    }

    #[test]
    fn validate_chat_playback_query_rejects_invalid_limit() {
        let zero = ChatPlaybackMessagesQuery {
            limit: 0,
            ..playback_query_with_context()
        };
        let too_large = ChatPlaybackMessagesQuery {
            limit: 501,
            ..playback_query_with_context()
        };

        assert!(matches!(
            validate_chat_playback_query(zero),
            Err(Error::InvalidInput(message)) if message.contains("limit")
        ));
        assert!(matches!(
            validate_chat_playback_query(too_large),
            Err(Error::InvalidInput(message)) if message.contains("limit")
        ));
    }

    fn test_chat_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> ChatService {
        test_chat_service_with_file_storage(
            pool,
            username_cache,
            Arc::new(DisabledFileStorageService),
        )
    }

    fn test_chat_service_with_file_storage(
        pool: &sqlx::PgPool,
        username_cache: UsernameCache,
        file_storage_service: Arc<dyn FileStorageService>,
    ) -> ChatService {
        let permission_service = PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            crate::service::permission::PermissionServiceRuntime {
                room_settings_repo: Some(RoomSettingsRepository::new(pool.clone())),
                ..Default::default()
            },
        )
        .expect("permission service should build");
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
                file_storage_service,
                audit_service: None,
                notification_service: NotificationService::default(),
            },
        )
    }

    fn test_chat_message(id: i64, created_at: chrono::DateTime<Utc>) -> ChatMessage {
        ChatMessage {
            id,
            room_id: RoomId::expect_positive(1),
            user_id: Some(UserId::expect_positive(2)),
            client_message_id: None,
            content: "hello".to_string(),
            message_type: ChatMessageType::Text,
            status: ChatMessageStatus::Active,
            version: 1,
            reply_to_message_id: None,
            reply_to_message_created_at: None,
            metadata: serde_json::Value::Object(Default::default()),
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            created_at,
        }
    }

    #[test]
    fn read_state_covers_newer_message_cursor() {
        let created_at = Utc::now();
        let message = test_chat_message(10, created_at);
        let state = ChatReadState {
            room_id: message.room_id,
            user_id: UserId::expect_positive(2),
            last_read_message_id: Some(11),
            last_read_message_created_at: Some(created_at),
            last_read_event_id: None,
            last_read_event_sequence: None,
            updated_at: Utc::now(),
        };

        assert!(read_state_covers_message(Some(&state), &message, None));
    }

    #[test]
    fn read_state_allows_forward_message_cursor() {
        let created_at = Utc::now();
        let message = test_chat_message(10, created_at);
        let state = ChatReadState {
            room_id: message.room_id,
            user_id: UserId::expect_positive(2),
            last_read_message_id: Some(9),
            last_read_message_created_at: Some(created_at),
            last_read_event_id: None,
            last_read_event_sequence: None,
            updated_at: Utc::now(),
        };

        assert!(!read_state_covers_message(Some(&state), &message, None));
    }

    #[test]
    fn read_state_allows_forward_event_on_same_message() {
        let created_at = Utc::now();
        let message = test_chat_message(10, created_at);
        let event = ChatMessageEventLog {
            sequence: 12,
            event: ChatMessageEvent {
                event_id: "event-12".to_string(),
                sequence: 12,
                room_id: message.room_id,
                actor_user_id: UserId::expect_positive(2),
                kind: crate::models::ChatEventKind::Edited,
                message: ChatMessageWithImages {
                    message: message.clone(),
                    images: Vec::new(),
                    reactions: Vec::new(),
                },
                occurred_at: Utc::now(),
            },
        };
        let state = ChatReadState {
            room_id: message.room_id,
            user_id: UserId::expect_positive(2),
            last_read_message_id: Some(message.id),
            last_read_message_created_at: Some(message.created_at),
            last_read_event_id: Some("event-11".to_string()),
            last_read_event_sequence: Some(11),
            updated_at: Utc::now(),
        };

        assert!(!read_state_covers_message(
            Some(&state),
            &message,
            Some(&event)
        ));
    }

    #[tokio::test]
    async fn username_lookup_falls_back_to_database_and_populates_cache() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_cache_miss_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        let username_cache = UsernameCache::local_only("test:chat:username:".to_string(), 100, 60);
        let service = test_chat_service_with_file_storage(
            &pool,
            username_cache.clone(),
            Arc::new(
                S3CompatibleFileStorageService::new(test_s3_file_storage_config())
                    .expect("S3 file storage config should be valid"),
            ),
        );

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

    #[tokio::test]
    async fn disabled_file_storage_rejects_images() {
        let service = DisabledFileStorageService;
        let result = service
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("client-1"),
                },
                vec![NewChatImage {
                    id: "image-1".to_string(),
                    storage_backend: "database".to_string(),
                    object_key: "image.webp".to_string(),
                    url: None,
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(-1),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            )
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn validate_chat_images_rejects_duplicates_in_one_message() {
        let image = NewChatImage {
            id: "image-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "image-1.webp".to_string(),
            url: None,
            mime_type: Some("image/webp".to_string()),
            size_bytes: Some(1024),
            width: Some(640),
            height: Some(480),
            metadata: serde_json::Value::Object(Default::default()),
        };

        let duplicate_id = validate_chat_images(&[
            image.clone(),
            NewChatImage {
                id: image.id.clone(),
                object_key: "image-2.webp".to_string(),
                ..image.clone()
            },
        ]);
        assert!(matches!(duplicate_id, Err(Error::InvalidInput(_))));

        let duplicate_key = validate_chat_images(&[
            image.clone(),
            NewChatImage {
                id: "image-2".to_string(),
                object_key: image.object_key.clone(),
                ..image
            },
        ]);
        assert!(matches!(duplicate_key, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn validate_chat_images_rejects_zero_size() {
        let result = validate_chat_images(&[NewChatImage {
            id: "image-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "image-1.webp".to_string(),
            url: None,
            mime_type: Some("image/webp".to_string()),
            size_bytes: Some(0),
            width: Some(640),
            height: Some(480),
            metadata: serde_json::Value::Object(Default::default()),
        }]);

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn disabled_file_storage_rejects_upload_session() {
        let service = DisabledFileStorageService;
        let result = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(2),
                client_file_id: Some("client-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 1024,
                width: Some(640),
                height: Some(480),
                checksum_sha256: Some("a".repeat(64)),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: chat_image_upload_policy(),
            })
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn disabled_file_storage_rejects_prepared_images() {
        let service = DisabledFileStorageService;

        let result = service
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(2),
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("client-1"),
                },
                vec![NewChatImage {
                    id: "image-1".to_string(),
                    storage_backend: "database".to_string(),
                    object_key: "rooms/1/chat/2/image-1".to_string(),
                    url: None,
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(1024),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            )
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn database_file_storage_roundtrips_uploaded_object() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_database_image_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:database-image:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (_room, _) = room_service
            .create_room(
                "Database Image Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        let service = DatabaseFileStorageService::new(
            "database",
            Arc::new(FileStorageRepository::new(pool.clone())),
            "database-image-secret",
        );
        let expected_checksum = hex::encode(Sha256::digest(b"data"));
        let session = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: user.id,
                client_file_id: Some("database-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(expected_checksum.clone()),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("database upload session should be created");
        assert_eq!(session.file.storage_backend, "database");
        assert_eq!(session.upload_method.as_deref(), Some("PUT"));
        let upload_url = session
            .upload_url
            .as_deref()
            .expect("database upload url should be returned");
        let parsed = url::Url::parse(&format!("http://localhost{upload_url}"))
            .expect("relative database object URL should parse with base");
        let encoded_object_key = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .expect("encoded object key path segment should exist");
        let upload_token = session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .expect("database upload token header should be returned");
        let stored = service
            .store_upload_object(
                encoded_object_key,
                upload_token,
                Some("image/webp"),
                b"data".to_vec(),
            )
            .await
            .expect("database image object should store");
        assert_eq!(stored.object_key, session.file.object_key);
        assert_eq!(stored.data, b"data");
        assert_eq!(stored.checksum_sha256, expected_checksum);
        let read_token = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.into_owned()))
            .expect("read token should be present");
        let loaded = service
            .get_object(encoded_object_key, &read_token)
            .await
            .expect("database image object should load");
        assert_eq!(loaded.data, b"data");
        let prepared = service
            .prepare_files(
                FileStorageContext {
                    user_id: user.id,
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("database-image-message"),
                },
                vec![session.file],
            )
            .await
            .expect("uploaded database image should prepare");
        assert_eq!(prepared.len(), 1);

        let mut reuse_session = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: user.id,
                client_file_id: Some("database-image-2".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(expected_checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("database upload session should reuse existing object");
        assert!(!reuse_session.upload_required);
        assert!(reuse_session.ownership_proof_required);
        assert_eq!(reuse_session.file.object_key, stored.object_key);
        let nonce = reuse_session
            .ownership_proof_nonce
            .as_deref()
            .expect("reuse session should return proof nonce");
        let chunks =
            ownership_proof_chunks_from_bytes(b"data", &reuse_session.ownership_proof_ranges)
                .expect("proof chunks should be readable");
        let proof = file_ownership_proof_digest(
            nonce,
            &reuse_session.ownership_proof_ranges,
            chunks.iter().map(Vec::as_slice),
        );
        reuse_session
            .file
            .metadata
            .as_object_mut()
            .expect("metadata should be object")
            .insert(
                FILE_OWNERSHIP_PROOF_KEY.to_string(),
                serde_json::Value::String(proof),
            );
        service
            .prepare_files(
                FileStorageContext {
                    user_id: user.id,
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("database-image-reuse-message"),
                },
                vec![reuse_session.file],
            )
            .await
            .expect("reused database image should prepare with ownership proof");
    }

    #[tokio::test]
    async fn database_file_storage_rejects_checksum_mismatch() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_database_image_checksum_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only(
                    "test:chat:database-image-checksum:".to_string(),
                    100,
                    60,
                ),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (_room, _) = room_service
            .create_room(
                "Database Image Checksum Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        let service = DatabaseFileStorageService::new(
            "database",
            Arc::new(FileStorageRepository::new(pool.clone())),
            "database-image-checksum-secret",
        );
        let session = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: user.id,
                client_file_id: Some("database-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(hex::encode(Sha256::digest(b"data"))),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("database upload session should be created");
        let upload_url = session
            .upload_url
            .as_deref()
            .expect("database upload url should be returned");
        let parsed = url::Url::parse(&format!("http://localhost{upload_url}"))
            .expect("relative database object URL should parse with base");
        let encoded_object_key = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .expect("encoded object key path segment should exist");
        let upload_token = session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .expect("database upload token header should be returned");

        let result = service
            .store_upload_object(
                encoded_object_key,
                upload_token,
                Some("image/webp"),
                b"fail".to_vec(),
            )
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn image_upload_session_requires_checksum() {
        let service = S3CompatibleFileStorageService::new(test_s3_file_storage_config())
            .expect("S3 file storage config should be valid");

        let result = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("client-image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: None,
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn s3_file_storage_rejects_tampered_upload_session_image() {
        let service = S3CompatibleFileStorageService::new(test_s3_file_storage_config())
            .expect("S3 file storage config should be valid");
        let session = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(2),
                client_file_id: Some("client-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 1024,
                width: Some(640),
                height: Some(480),
                checksum_sha256: Some("b".repeat(64)),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("upload session should be created");
        let mut tampered = session.file;
        tampered.object_key = "files/rooms/1/chat/2/other-image.webp".to_string();

        let result = service
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(2),
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("client-1"),
                },
                vec![tampered],
            )
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    fn test_s3_file_storage_config() -> S3FileStorageConfig {
        S3FileStorageConfig {
            endpoint: "https://s3.example.com".to_string(),
            access_key_id: "test-access-key".to_string(),
            secret_access_key: "test-secret-key".to_string(),
            bucket: "synctv-files".to_string(),
            region: "auto".to_string(),
            base_path: "files/".to_string(),
            public_base_url: Some("https://cdn.example.com/files".to_string()),
            upload_expires_seconds: 600,
            storage_backend: "s3".to_string(),
            upload_token_secret: "file-upload-token-secret".to_string(),
        }
    }

    #[tokio::test]
    async fn s3_file_storage_creates_presigned_upload_session() {
        let service = S3CompatibleFileStorageService::new(test_s3_file_storage_config())
            .expect("S3 file storage config should be valid");
        let session = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("client-image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: Some("a".repeat(64)),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("S3 upload session should be created");

        assert!(session.upload_required);
        assert_eq!(session.upload_method.as_deref(), Some("PUT"));
        assert_eq!(
            session
                .upload_headers
                .get("content-type")
                .map(String::as_str),
            Some("image/png")
        );
        assert_eq!(session.file.id, "client-image-1");
        assert_eq!(session.file.storage_backend, "s3");
        assert!(session
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .is_some());
        assert!(session
            .file
            .object_key
            .starts_with("files/chat/images/sha256/aa/aa/"));
        let expected_public_url = format!(
            "https://cdn.example.com/files/synctv-files/{}",
            session.file.object_key
        );
        assert_eq!(
            session.file.url.as_deref(),
            Some(expected_public_url.as_str())
        );
        let upload_url = session.upload_url.expect("upload URL should be returned");
        assert!(upload_url.starts_with(&format!(
            "https://s3.example.com/synctv-files/{}?",
            session.file.object_key
        )));
        assert!(upload_url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(upload_url.contains("X-Amz-Credential=test-access-key%2F"));
        assert!(upload_url.contains("X-Amz-SignedHeaders="));
        assert!(upload_url.contains("X-Amz-Signature="));
        assert!(session.expires_at.is_some());
        assert_eq!(session.max_size_bytes, MAX_CHAT_IMAGE_SIZE_BYTES);
    }

    #[tokio::test]
    async fn image_upload_sessions_reuse_content_object_key_for_reused_client_ids() {
        let service = S3CompatibleFileStorageService::new(test_s3_file_storage_config())
            .expect("S3 file storage config should be valid");
        let first = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: Some("c".repeat(64)),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("first upload session should be created");
        let second = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: Some("c".repeat(64)),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("second upload session should be created");

        assert_eq!(first.file.id, "image-1");
        assert_eq!(second.file.id, "image-1");
        assert_eq!(first.file.object_key, second.file.object_key);
    }

    #[tokio::test]
    async fn s3_file_storage_reuses_registered_object_with_ownership_proof() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let config = test_s3_file_storage_config();
        let policy = chat_image_upload_policy();
        let checksum = hex::encode(Sha256::digest(b"data"));
        let object_key = file_content_object_key(
            &file_storage_object_base_path(&config.base_path, &policy.storage_namespace),
            &checksum,
        );
        let operator = Operator::new(opendal::services::Memory::default())
            .expect("memory operator should build")
            .finish();
        operator
            .write(&object_key, b"data".to_vec())
            .await
            .expect("object should be written");
        repository
            .upsert_object(
                "s3",
                &object_key,
                "image/webp",
                4,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("object registry should be written");
        let service = S3CompatibleFileStorageService::new_with_repository(config, Some(repository))
            .expect("S3 file storage config should be valid")
            .with_operator(operator);

        let mut session = service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("registered S3 object should create reuse session");
        assert!(!session.upload_required);
        assert!(session.ownership_proof_required);
        assert_eq!(session.file.object_key, object_key);
        let nonce = session
            .ownership_proof_nonce
            .as_deref()
            .expect("reuse session should return proof nonce");
        let chunks = ownership_proof_chunks_from_bytes(b"data", &session.ownership_proof_ranges)
            .expect("proof chunks should be readable");
        let proof = file_ownership_proof_digest(
            nonce,
            &session.ownership_proof_ranges,
            chunks.iter().map(Vec::as_slice),
        );
        session
            .file
            .metadata
            .as_object_mut()
            .expect("metadata should be object")
            .insert(
                FILE_OWNERSHIP_PROOF_KEY.to_string(),
                serde_json::Value::String(proof),
            );

        service
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(9),
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("s3-reuse"),
                },
                vec![session.file],
            )
            .await
            .expect("S3 reuse should prepare with ownership proof");
    }

    #[test]
    fn s3_public_url_uses_url_path_segment_encoding() {
        let mut config = test_s3_file_storage_config();
        config.bucket = "bucket with spaces".to_string();
        let url = file_storage_public_url(&config, "chat images/with # and ?.png")
            .expect("public URL should be built");

        assert_eq!(
            url,
            "https://cdn.example.com/files/bucket%20with%20spaces/chat%20images/with%20%23%20and%20%3F.png"
        );
    }

    #[test]
    fn s3_file_storage_rejects_invalid_config() {
        let mut config = test_s3_file_storage_config();
        config.endpoint.clear();

        let result = S3CompatibleFileStorageService::new(config);

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn s3_file_storage_rejects_unexpected_backend_on_send() {
        let service = S3CompatibleFileStorageService::new(test_s3_file_storage_config())
            .expect("S3 file storage config should be valid");
        let result = service
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("client-1"),
                },
                vec![NewChatImage {
                    id: "image-1".to_string(),
                    storage_backend: "database".to_string(),
                    object_key: "image.webp".to_string(),
                    url: Some("https://cdn.example.com/image.webp".to_string()),
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(1024),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            )
            .await;

        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[tokio::test]
    async fn metadata_only_image_token_is_stripped_before_persistence() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:image-token-strip:".to_string(), 100, 60);
        let service = test_chat_service_with_file_storage(
            &pool,
            username_cache.clone(),
            Arc::new(DatabaseFileStorageService::new(
                "database",
                Arc::new(FileStorageRepository::new(pool.clone())),
                "test-file-storage-secret",
            )),
        );
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_image_token_strip_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:image-token-strip:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Image Token Strip Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let payload = vec![b'd'; 1024];
        let session = service
            .create_image_upload_session(CreateChatImageUploadSession {
                room_id: room.id,
                user_id: user.id,
                client_image_id: Some("strip-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 1024,
                width: Some(640),
                height: Some(480),
                checksum_sha256: Some(hex::encode(Sha256::digest(&payload))),
                metadata: serde_json::json!({"blurhash": "abc"}),
            })
            .await
            .expect("upload session should be created");
        assert!(session.file.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_some());
        let upload_url = session
            .upload_url
            .as_deref()
            .expect("database upload url should be returned");
        let parsed = url::Url::parse(&format!("http://localhost{upload_url}"))
            .expect("relative database object URL should parse with base");
        let encoded_object_key = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .expect("encoded object key path segment should exist");
        let upload_token = session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .expect("database upload token header should be returned");
        service
            .store_image_upload_object(
                encoded_object_key,
                upload_token,
                Some("image/webp"),
                payload,
            )
            .await
            .expect("database image object should store");

        let event = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("strip-image-message-1".to_string()),
                content: String::new(),
                message_type: ChatMessageType::Image,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: vec![session.file],
            })
            .await
            .expect("image message should be stored");

        let image = event
            .message
            .images
            .first()
            .expect("image should be present");
        assert!(image.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_none());
        assert_eq!(
            image.metadata.get("blurhash").and_then(|v| v.as_str()),
            Some("abc")
        );
    }

    #[tokio::test]
    async fn custom_file_storage_can_normalize_image_metadata() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache = UsernameCache::local_only("test:chat:image:".to_string(), 100, 60);
        let service = test_chat_service_with_file_storage(
            &pool,
            username_cache.clone(),
            Arc::new(PrefixingFileStorageService),
        );
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_file_storage_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:image:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Image Storage Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        FileStorageRepository::new(pool.clone())
            .upsert_object(
                "test-storage",
                "normalized/raw/image.webp",
                "image/webp",
                123,
                &hex::encode(Sha256::digest(b"raw/image.webp")),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("normalized image object should be registered");

        let event = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("image-storage-client-id".to_string()),
                content: String::new(),
                message_type: ChatMessageType::Image,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: vec![NewChatImage {
                    id: "image-storage-1".to_string(),
                    storage_backend: "client".to_string(),
                    object_key: "raw/image.webp".to_string(),
                    url: None,
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(123),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            })
            .await
            .expect("image message should be stored");

        let image = event
            .message
            .images
            .first()
            .expect("image should be present");
        assert_eq!(image.storage_backend, "test-storage");
        assert_eq!(image.object_key, "normalized/raw/image.webp");
        assert_eq!(
            image.url.as_deref(),
            Some("https://cdn.invalid/image-storage-1")
        );
    }

    #[tokio::test]
    async fn deleting_image_message_releases_image_objects() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:delete-image:".to_string(), 100, 60);
        let storage = Arc::new(RecordingFileStorageService::default());
        let service =
            test_chat_service_with_file_storage(&pool, username_cache.clone(), storage.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_delete_image_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:delete-image:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Delete Image Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        FileStorageRepository::new(pool.clone())
            .upsert_object(
                "test-storage",
                "normalized/raw/delete-image.webp",
                "image/webp",
                123,
                &hex::encode(Sha256::digest(b"raw/delete-image.webp")),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("normalized image object should be registered");

        let created = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("delete-image-client-id".to_string()),
                content: String::new(),
                message_type: ChatMessageType::Image,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: vec![NewChatImage {
                    id: "delete-image-1".to_string(),
                    storage_backend: "client".to_string(),
                    object_key: "raw/delete-image.webp".to_string(),
                    url: None,
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(123),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            })
            .await
            .expect("image message should be stored");

        service
            .delete_message_event(DeleteChatMessage {
                room_id: room.id,
                message_id: created.message.message.id,
                user_id: user.id,
                client_operation_id: None,
                reason: None,
                expected_version: Some(created.message.message.version),
            })
            .await
            .expect("image message should delete cleanly");

        let deleted_object_keys = storage.deleted_object_keys.lock().unwrap().clone();
        assert_eq!(
            deleted_object_keys,
            vec!["normalized/raw/delete-image.webp".to_string()]
        );
        let deleted_origins = storage.deleted_origins.lock().unwrap().clone();
        assert_eq!(deleted_origins, vec!["reference_released".to_string()]);
    }

    #[tokio::test]
    async fn concurrent_idempotent_send_returns_existing_created_event() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:idempotent-send:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_idempotent_send_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:idempotent-send:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Idempotent Send Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let request = SendChatMessage {
            room_id: room.id,
            user_id: user.id,
            client_message_id: Some("same-client-message-id".to_string()),
            content: "same payload".to_string(),
            message_type: ChatMessageType::Text,
            reply_to_message_id: None,
            metadata: serde_json::Value::Object(Default::default()),
            images: Vec::new(),
        };
        let worker_count = 6;
        let barrier = Arc::new(Barrier::new(worker_count));
        let mut handles = Vec::new();
        for _ in 0..worker_count {
            let service = service.clone();
            let request = request.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                service.send_message_event_outcome(request).await
            }));
        }

        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(
                handle
                    .await
                    .expect("send task should finish")
                    .expect("idempotent send should succeed"),
            );
        }
        let first = &outcomes.first().expect("event should be returned").event;
        for outcome in &outcomes {
            let event = &outcome.event;
            assert_eq!(event.event_id, first.event_id);
            assert_eq!(event.message.message.id, first.message.message.id);
            assert_eq!(event.kind, ChatEventKind::Created);
        }
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.inserted).count(),
            1
        );

        let message_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_messages
            WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
            "#,
            room.id.as_i64(),
            user.id.as_i64(),
            "same-client-message-id"
        )
        .fetch_one(&pool)
        .await
        .expect("message count should load");
        assert_eq!(message_count, 1);

        let event_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_message_events
            WHERE room_id = $1
              AND message_id = $2
              AND event_type = $3
            "#,
            room.id.as_i64(),
            first.message.message.id,
            "chat_message_created"
        )
        .fetch_one(&pool)
        .await
        .expect("event count should load");
        assert_eq!(event_count, 1);
    }

    #[tokio::test]
    async fn concurrent_same_edit_returns_existing_edit_event() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:concurrent-edit:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_concurrent_edit_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:concurrent-edit:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Concurrent Edit Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let created = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("concurrent-edit-created".to_string()),
                content: "before edit".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("message should be stored");
        let request = EditChatMessage {
            room_id: room.id,
            message_id: created.message.message.id,
            user_id: user.id,
            client_operation_id: None,
            content: "after edit".to_string(),
            metadata: serde_json::json!({"edited": true}),
            expected_version: Some(created.message.message.version),
        };
        let worker_count = 6;
        let barrier = Arc::new(Barrier::new(worker_count));
        let mut handles = Vec::new();
        for _ in 0..worker_count {
            let service = service.clone();
            let request = request.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                service.edit_message_outcome(request).await
            }));
        }

        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(
                handle
                    .await
                    .expect("edit task should finish")
                    .expect("same edit should converge"),
            );
        }
        let first = &outcomes.first().expect("event should be returned").event;
        for outcome in &outcomes {
            let event = &outcome.event;
            assert_eq!(event.event_id, first.event_id);
            assert_eq!(event.kind, ChatEventKind::Edited);
            assert_eq!(
                event.message.message.version,
                created.message.message.version + 1
            );
        }
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.inserted).count(),
            1
        );

        let edit_event_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_message_events
            WHERE room_id = $1
              AND message_id = $2
              AND event_type = $3
            "#,
            room.id.as_i64(),
            created.message.message.id,
            "chat_message_edited"
        )
        .fetch_one(&pool)
        .await
        .expect("edit event count should load");
        assert_eq!(edit_event_count, 1);
    }

    #[tokio::test]
    async fn concurrent_same_delete_returns_existing_delete_event() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:concurrent-delete:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_concurrent_delete_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:concurrent-delete:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Concurrent Delete Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let created = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("concurrent-delete-created".to_string()),
                content: "delete me".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("message should be stored");
        let request = DeleteChatMessage {
            room_id: room.id,
            message_id: created.message.message.id,
            user_id: user.id,
            client_operation_id: None,
            reason: Some("cleanup".to_string()),
            expected_version: Some(created.message.message.version),
        };
        let worker_count = 6;
        let barrier = Arc::new(Barrier::new(worker_count));
        let mut handles = Vec::new();
        for _ in 0..worker_count {
            let service = service.clone();
            let request = request.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                service.delete_message_event_outcome(request).await
            }));
        }

        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(
                handle
                    .await
                    .expect("delete task should finish")
                    .expect("same delete should converge"),
            );
        }
        let first = &outcomes.first().expect("event should be returned").event;
        for outcome in &outcomes {
            let event = &outcome.event;
            assert_eq!(event.event_id, first.event_id);
            assert_eq!(event.kind, ChatEventKind::Deleted);
            assert_eq!(
                event.message.message.version,
                created.message.message.version + 1
            );
        }
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.inserted).count(),
            1
        );

        let delete_event_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_message_events
            WHERE room_id = $1
              AND message_id = $2
              AND event_type = $3
            "#,
            room.id.as_i64(),
            created.message.message.id,
            "chat_message_deleted"
        )
        .fetch_one(&pool)
        .await
        .expect("delete event count should load");
        assert_eq!(delete_event_count, 1);
    }

    #[tokio::test]
    async fn chat_reactions_update_history_and_emit_reaction_events() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache = UsernameCache::local_only("test:chat:reactions:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let owner = user_repository
            .create(&User::new(
                "chat_reaction_owner".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("owner should be created");
        let member = user_repository
            .create(&User::new(
                "chat_reaction_member".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("member should be created");
        username_cache
            .set(&owner.id, &owner.username)
            .await
            .unwrap();
        username_cache
            .set(&member.id, &member.username)
            .await
            .unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:reactions:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Reaction Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        room_service
            .join_room(room.id, member.id, None)
            .await
            .expect("member should join room");
        let message = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("reaction-message".to_string()),
                content: "react to this".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("message should be stored");

        let owner_reaction = service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: owner.id,
                reaction_key: "like".to_string(),
                enabled: true,
            })
            .await
            .expect("owner reaction should be stored");
        assert_eq!(owner_reaction.event.kind, ChatEventKind::ReactionsChanged);
        assert_eq!(owner_reaction.event.message.reactions.len(), 1);
        assert_eq!(owner_reaction.event.message.reactions[0].key, "like");
        assert_eq!(owner_reaction.event.message.reactions[0].count, 1);
        assert!(owner_reaction.event.message.reactions[0].reacted_by_me);

        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: member.id,
                reaction_key: "like".to_string(),
                enabled: true,
            })
            .await
            .expect("member reaction should be stored");

        let (owner_history, _) = service
            .get_history_with_images_for_viewer(&room.id, None, 10, false, Some(&owner.id))
            .await
            .expect("owner history should load");
        let owner_summary = owner_history[0]
            .reactions
            .iter()
            .find(|reaction| reaction.key == "like")
            .expect("like summary should exist");
        assert_eq!(owner_summary.count, 2);
        assert!(owner_summary.reacted_by_me);

        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: owner.id,
                reaction_key: "like".to_string(),
                enabled: false,
            })
            .await
            .expect("owner reaction should clear");

        let (owner_history, _) = service
            .get_history_with_images_for_viewer(&room.id, None, 10, false, Some(&owner.id))
            .await
            .expect("owner history should reload");
        let owner_summary = owner_history[0]
            .reactions
            .iter()
            .find(|reaction| reaction.key == "like")
            .expect("like summary should remain");
        assert_eq!(owner_summary.count, 1);
        assert!(!owner_summary.reacted_by_me);

        let (member_history, _) = service
            .get_history_with_images_for_viewer(&room.id, None, 10, false, Some(&member.id))
            .await
            .expect("member history should load");
        let member_summary = member_history[0]
            .reactions
            .iter()
            .find(|reaction| reaction.key == "like")
            .expect("like summary should exist");
        assert_eq!(member_summary.count, 1);
        assert!(member_summary.reacted_by_me);

        let page = service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                None,
                10,
            )
            .await
            .expect("reaction users should load");
        assert_eq!(page.total, 1);
        assert_eq!(page.users.len(), 1);
        assert_eq!(page.users[0].user_id, member.id);
        assert!(page.next_cursor.is_none());

        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: owner.id,
                reaction_key: "like".to_string(),
                enabled: true,
            })
            .await
            .expect("owner reaction should be restored");
        let refreshed_page = service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                None,
                10,
            )
            .await
            .expect("reaction users should reload after cache invalidation");
        assert_eq!(refreshed_page.total, 2);
        assert_eq!(refreshed_page.users.len(), 2);
        let page = service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                None,
                1,
            )
            .await
            .expect("first reaction user page should load");
        assert_eq!(page.total, 2);
        assert_eq!(page.users.len(), 1);
        let cursor = page.next_cursor.expect("next cursor should be present");
        let next = service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                Some(cursor),
                1,
            )
            .await
            .expect("second reaction user page should load");
        assert_eq!(next.total, 2);
        assert_eq!(next.users.len(), 1);
        assert_ne!(page.users[0].user_id, next.users[0].user_id);
    }

    #[tokio::test]
    async fn read_state_tracks_unread_count_and_stays_monotonic() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:read-state:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_read_state_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let reader = user_repository
            .create(&User::new(
                "chat_read_state_reader".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("reader should be created");
        username_cache
            .set(&reader.id, &reader.username)
            .await
            .unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:read-state:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Read State Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        room_service
            .join_room(room.id, reader.id, None)
            .await
            .expect("reader should join room");

        let first = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("read-state-1".to_string()),
                content: "first".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("first message should be stored");
        let second = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("read-state-2".to_string()),
                content: "second".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("second message should be stored");
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: reader.id,
                client_message_id: Some("read-state-reader-own".to_string()),
                content: "reader own message".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("reader own message should be stored");
        service
            .edit_message(EditChatMessage {
                room_id: room.id,
                message_id: first.message.message.id,
                user_id: user.id,
                client_operation_id: None,
                content: "first edited after second".to_string(),
                metadata: serde_json::Value::Object(Default::default()),
                expected_version: Some(first.message.message.version),
            })
            .await
            .expect("older message should be editable after newer message");

        let initial = service
            .get_read_state(&room.id, &reader.id)
            .await
            .expect("read state should load");
        assert_eq!(initial.unread_count, 2);
        assert_eq!(initial.state.last_read_message_id, None);

        let after_first = service
            .mark_read(MarkChatRead {
                room_id: room.id,
                user_id: reader.id,
                message_id: first.message.message.id,
            })
            .await
            .expect("first message should be marked read");
        assert_eq!(after_first.unread_count, 1);
        assert_eq!(
            after_first.state.last_read_message_id,
            Some(first.message.message.id)
        );
        sqlx::query!(
            r"
            UPDATE chat_read_states
            SET last_read_message_id = NULL, last_read_message_created_at = NULL
            WHERE room_id = $1 AND user_id = $2
            ",
            room.id.as_i64(),
            reader.id.as_i64()
        )
        .execute(&pool)
        .await
        .expect("read state message cursor should be cleared");
        let event_sequence_fallback = service
            .get_read_state(&room.id, &reader.id)
            .await
            .expect("read state should use event sequence fallback");
        assert_eq!(event_sequence_fallback.unread_count, 1);
        assert_eq!(
            event_sequence_fallback.state.last_read_event_sequence,
            after_first.state.last_read_event_sequence
        );

        let after_second = service
            .mark_read(MarkChatRead {
                room_id: room.id,
                user_id: reader.id,
                message_id: second.message.message.id,
            })
            .await
            .expect("second message should be marked read");
        assert_eq!(after_second.unread_count, 0);
        assert_eq!(
            after_second.state.last_read_message_id,
            Some(second.message.message.id)
        );

        let stale = service
            .mark_read(MarkChatRead {
                room_id: room.id,
                user_id: reader.id,
                message_id: first.message.message.id,
            })
            .await
            .expect("stale read cursor should be ignored");
        assert_eq!(stale.unread_count, 0);
        assert_eq!(
            stale.state.last_read_message_id,
            Some(second.message.message.id)
        );
    }

    #[tokio::test]
    async fn message_context_returns_messages_around_anchor_in_chronological_order() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache = UsernameCache::local_only("test:chat:context:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_context_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:context:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Context Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let mut messages = Vec::new();
        for index in 1..=5 {
            let event = service
                .send_message_event(SendChatMessage {
                    room_id: room.id,
                    user_id: user.id,
                    client_message_id: Some(format!("context-{index}")),
                    content: format!("message {index}"),
                    message_type: ChatMessageType::Text,
                    reply_to_message_id: None,
                    metadata: serde_json::Value::Object(Default::default()),
                    images: Vec::new(),
                })
                .await
                .expect("message should be stored");
            messages.push(event.message.message);
        }

        let context = service
            .get_message_context(&room.id, messages[2].id, 2, 2, false)
            .await
            .expect("context should load");

        assert_eq!(
            context
                .before
                .iter()
                .map(|message| message.message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["message 1", "message 2"]
        );
        assert_eq!(context.anchor.message.content, "message 3");
        assert_eq!(
            context
                .after
                .iter()
                .map(|message| message.message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["message 4", "message 5"]
        );
    }

    #[tokio::test]
    async fn chat_text_validation_rejects_whitespace_send_and_edit() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let username_cache =
            UsernameCache::local_only("test:chat:text-validation:".to_string(), 100, 60);
        let service = test_chat_service(&pool, username_cache.clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let user = user_repository
            .create(&User::new(
                "chat_text_validation_user".to_string(),
                SignupMethod::Password,
            ))
            .await
            .expect("user should be created");
        username_cache.set(&user.id, &user.username).await.unwrap();
        let room_service = RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:text-validation:room:".to_string(), 100, 60),
            ))
            .clone(),
        )
        .expect("room service should build");
        let (room, _) = room_service
            .create_room(
                "Text Validation Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await
            .expect("room should be created");

        let whitespace_send = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("whitespace-send".to_string()),
                content: "   \n\t ".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect_err("whitespace-only chat should be rejected");
        assert!(matches!(whitespace_send, Error::InvalidInput(_)));

        let message = service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("valid-before-edit".to_string()),
                content: "valid".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("valid message should be stored");

        let whitespace_edit = service
            .edit_message(EditChatMessage {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: user.id,
                client_operation_id: None,
                content: " \n ".to_string(),
                metadata: serde_json::Value::Object(Default::default()),
                expected_version: Some(message.message.message.version),
            })
            .await
            .expect_err("whitespace-only edit should be rejected");
        assert!(matches!(whitespace_edit, Error::InvalidInput(_)));
    }
}
