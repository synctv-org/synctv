//! Chat service for managing room chat messages
//!
//! Handles sending, receiving, and deleting chat messages with rate limiting
//! and content filtering.

use chrono::{DateTime, Utc};
use moka::future::Cache as AsyncCache;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ExecutionControl;
use tracing::{debug, error, info, warn};

use crate::{
    models::{
        AuditAction, AuditDetails, AuditTargetType, ChatAttachment, ChatEventKind,
        ChatHistoryCursor, ChatHistoryPage, ChatMessage, ChatMessageContext, ChatMessageEvent,
        ChatMessageEventLog, ChatMessageOperationKind, ChatMessageReadReceiptsPage,
        ChatMessageStatus, ChatMessageType, ChatMessageWithAttachments, ChatPinEvent,
        ChatPinnedMessage, ChatPlaybackMessagesQuery, ChatReactionUsersCursor,
        ChatReactionUsersPage, ChatReadStateWithUnread, ChatSearchMessagesPage,
        ChatSearchMessagesQuery, CompleteFileUploadSession, CompleteFileUploadSessionResult,
        CreateChatAttachmentUploadSession, DeleteChatMessage, EditChatMessage, FileBlob,
        FileObjectDownload, FileRangeRequest, FileUploadRange, FileUploadSessionCreateResult,
        GetFileObject, MarkChatRead, PinChatMessage, RoomId, SendChatMessage, SetChatReaction,
        StoreFileUpload, StoreFileUploadResult, SubmittedFileReference, SubmittedFileReferenceKind,
        UnpinChatMessage, UserId, CHAT_PIN_NOTE_MAX_CHARS,
    },
    repository::{
        ChatMessageOperationIdempotency, ChatRepository, DeleteChatMessageEventRequest,
        EditChatMessageEventRequest, PinChatMessageEventRequest, UnpinChatMessageEventRequest,
    },
    service::{
        audit::{AuditEventParams, AuditService},
        notification::NotificationService,
        ContentFilter, PermissionService, RateLimitConfig, RequestRateLimiterService,
        RoomSettingsService, UserService,
    },
    Error, Result,
};

use super::file_storage::{FileStorageCleanupOrigin, FileStorageContext, FileStorageService};

mod helpers;
use helpers::*;

/// Maximum allowed chat message length in characters.
/// Used by both the WebSocket handler and the service layer for consistent validation.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 500;
pub const MAX_CHAT_ATTACHMENTS_PER_MESSAGE: usize = 10;
const CHAT_REACTION_DETAIL_CACHE_TTL_SECS: u64 = 5;
const CHAT_REACTION_DETAIL_CACHE_CAPACITY: u64 = 1024;

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
    message_cleanup_service: ChatMessageCleanupService,
    audit_service: Option<Arc<AuditService>>,
    /// Local room event bus for chat/domain notifications
    notification_service: NotificationService,
    runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
    reaction_detail_cache: AsyncCache<ChatReactionDetailCacheKey, ChatReactionUsersPage>,
}

#[derive(Debug, Clone)]
pub struct ChatMessageEventOutcome {
    pub event: ChatMessageEvent,
    pub inserted: bool,
    pub pin_event: Option<ChatPinEvent>,
}

#[derive(Debug, Clone)]
pub struct ChatPinEventOutcome {
    pub event: ChatPinEvent,
    pub inserted: bool,
}

#[derive(Clone)]
struct ChatMessageCleanupService {
    pool: PgPool,
    file_storage_service: Arc<dyn FileStorageService>,
}

impl ChatMessageCleanupService {
    fn new(pool: PgPool, file_storage_service: Arc<dyn FileStorageService>) -> Self {
        Self {
            pool,
            file_storage_service,
        }
    }

    async fn cleanup_room_cap(&self, room_id: RoomId, keep_count: i64) -> Result<u64> {
        super::cleanup_ops::cleanup_chat_messages_with_files(
            &self.pool,
            Some(&self.file_storage_service),
            super::cleanup_ops::ChatMessageCleanupScope::RoomCap {
                room_id,
                keep_count,
            },
            FileStorageCleanupOrigin::ReferenceCapExceeded,
            "room cap purge",
        )
        .await
    }

    async fn cleanup_active_rooms_cap(
        &self,
        keep_count: i64,
        activity_window_minutes: i32,
    ) -> Result<u64> {
        super::cleanup_ops::cleanup_chat_messages_with_files(
            &self.pool,
            Some(&self.file_storage_service),
            super::cleanup_ops::ChatMessageCleanupScope::ActiveRoomsCap {
                keep_count,
                activity_window_minutes,
            },
            FileStorageCleanupOrigin::ReferenceCapExceeded,
            "active-room cap purge",
        )
        .await
    }
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
    pub runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
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
            runtime_settings_store,
        } = dependencies;

        Self {
            message_cleanup_service: ChatMessageCleanupService::new(
                chat_repository.pool().clone(),
                file_storage_service.clone(),
            ),
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
            runtime_settings_store,
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

    pub async fn create_attachment_upload_session(
        &self,
        request: CreateChatAttachmentUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
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
            .create_upload_session(chat_attachment_upload_request_to_file_request(request))
            .await
    }

    pub async fn store_attachment_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        range: Option<FileUploadRange>,
        data: Vec<u8>,
    ) -> Result<StoreFileUploadResult> {
        self.file_storage_service
            .store_upload(StoreFileUpload {
                encoded_object_key: encoded_object_key.to_string(),
                upload_token: upload_token.to_string(),
                content_type: content_type.map(str::to_string),
                range,
                data,
            })
            .await
    }

    pub async fn complete_attachment_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        self.file_storage_service
            .complete_upload_session(request)
            .await
    }

    pub async fn get_attachment_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<FileBlob> {
        self.get_attachment_object_range(encoded_object_key, read_token, None)
            .await
    }

    pub async fn get_attachment_object_range(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileBlob> {
        self.file_storage_service
            .get_object(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn get_attachment_object_stream(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileObjectDownload> {
        self.file_storage_service
            .get_object_stream(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    #[must_use]
    pub const fn room_settings_service(&self) -> &RoomSettingsService {
        &self.room_settings_service
    }

    fn max_pinned_chat_messages_per_room(&self) -> Result<Option<i64>> {
        let Some(runtime_settings_store) = &self.runtime_settings_store else {
            return Ok(None);
        };
        let limit = runtime_settings_store
            .chat
            .max_pinned_messages_per_room
            .get()?;
        if limit == 0 {
            return Ok(None);
        }
        i64::try_from(limit)
            .map(Some)
            .map_err(|_| Error::Internal("max pinned chat message limit exceeds i64".to_string()))
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
                    metadata: crate::models::ChatMetadata::default(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
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
        validate_submitted_chat_attachments(&request.attachments)?;
        normalize_chat_mentions(&request.content, &mut request.mentions)?;
        if request.content.trim().is_empty() && request.attachments.is_empty() {
            return Err(Error::InvalidInput(
                "empty chat message: content or attachment is required".to_string(),
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
        for mention in &request.mentions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &mention.user_id,
                    crate::models::RoomPermission::VIEW_CHAT_HISTORY,
                )
                .await?;
        }

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
                let mut event = event.event;
                self.attach_event_attachment_view_metadata(&mut event, Some(&user_id))
                    .await?;
                info!(
                    room_id = %room_id,
                    user_id = %user_id,
                    message_id = %event.message.message.id,
                    event_id = %event.event_id,
                    inserted = false,
                    "Chat message send replayed from idempotency record"
                );
                return Ok(ChatMessageEventOutcome {
                    event,
                    inserted: false,
                    pin_event: None,
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
        let upload_policy = super::chat_attachment_upload_policy();
        let mut attachments = self
            .prepare_submitted_chat_attachments(
                user_id,
                room_id,
                &storage_scope,
                upload_policy.object_kind,
                request.client_message_id.as_deref(),
                std::mem::take(&mut request.attachments),
            )
            .await?;
        validate_chat_attachments(&attachments)?;
        strip_internal_chat_attachment_metadata(&mut attachments);

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
        if filtered_content.trim().is_empty() && attachments.is_empty() {
            return Err(Error::InvalidInput(
                "empty chat message: content or attachment is required".to_string(),
            ));
        }
        request.content = filtered_content.clone();
        if !attachments.is_empty() {
            request.message_type = ChatMessageType::Attachment;
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
                &attachments,
                &request.mentions,
                &request_hash,
                &event_id,
                occurred_at,
            )
            .await?;
        let mut event = created.event.event;
        self.attach_event_attachment_view_metadata(&mut event, Some(&user_id))
            .await?;

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
            pin_event: created.pin_event.map(|event| event.event),
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

    pub async fn get_history_with_attachments(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
    ) -> Result<(Vec<ChatMessageWithAttachments>, Option<ChatHistoryCursor>)> {
        self.get_history_with_attachments_for_viewer(room_id, cursor, limit, include_deleted, None)
            .await
    }

    pub async fn get_history_with_attachments_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<(Vec<ChatMessageWithAttachments>, Option<ChatHistoryCursor>)> {
        let (mut messages, cursor) = self
            .chat_repository
            .list_by_room_cursor_for_viewer(room_id, cursor, limit, include_deleted, viewer_user_id)
            .await?;
        self.attach_attachment_view_metadata(&mut messages, viewer_user_id)
            .await?;
        Ok((messages, cursor))
    }

    pub async fn get_history_page_with_attachments_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatHistoryPage> {
        let mut page = self
            .chat_repository
            .list_history_page_for_viewer(room_id, cursor, limit, include_deleted, viewer_user_id)
            .await?;
        self.attach_attachment_view_metadata(&mut page.messages, viewer_user_id)
            .await?;
        Ok(page)
    }

    pub async fn get_playback_messages_with_attachments(
        &self,
        query: ChatPlaybackMessagesQuery,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        self.get_playback_messages_with_attachments_for_viewer(query, None)
            .await
    }

    pub async fn get_playback_messages_with_attachments_for_viewer(
        &self,
        query: ChatPlaybackMessagesQuery,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        let query = validate_chat_playback_query(query)?;
        let mut messages = self
            .chat_repository
            .list_playback_messages_for_viewer(&query, viewer_user_id)
            .await?;
        self.attach_attachment_view_metadata(&mut messages, viewer_user_id)
            .await?;
        Ok(messages)
    }

    pub async fn search_messages_with_attachments_for_viewer(
        &self,
        query: ChatSearchMessagesQuery,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatSearchMessagesPage> {
        let query = validate_chat_search_query(query)?;
        let mut page = self
            .chat_repository
            .search_messages_for_viewer(&query, viewer_user_id)
            .await?;
        self.attach_attachment_view_metadata(&mut page.messages, viewer_user_id)
            .await?;
        Ok(page)
    }

    pub async fn get_message_with_attachments(
        &self,
        room_id: &RoomId,
        message_id: i64,
        include_deleted: bool,
    ) -> Result<ChatMessageWithAttachments> {
        self.get_message_with_attachments_for_viewer(room_id, message_id, include_deleted, None)
            .await
    }

    pub async fn get_message_with_attachments_for_viewer(
        &self,
        room_id: &RoomId,
        message_id: i64,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatMessageWithAttachments> {
        let message = self
            .chat_repository
            .get_with_attachments_by_room_and_id_for_viewer(room_id, message_id, viewer_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if message.message.status == ChatMessageStatus::Deleted && !include_deleted {
            return Err(Error::NotFound("Message not found".to_string()));
        }
        let mut messages = vec![message];
        self.attach_attachment_view_metadata(&mut messages, viewer_user_id)
            .await?;
        Ok(messages.remove(0))
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
        let mut context = self
            .chat_repository
            .list_context_around_message_for_viewer(
                room_id,
                message_id,
                before_limit,
                after_limit,
                include_deleted,
                viewer_user_id,
            )
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        self.attach_attachment_view_metadata(&mut context.before, viewer_user_id)
            .await?;
        self.attach_attachment_view_metadata(
            std::slice::from_mut(&mut context.anchor),
            viewer_user_id,
        )
        .await?;
        self.attach_attachment_view_metadata(&mut context.after, viewer_user_id)
            .await?;
        Ok(context)
    }

    pub async fn get_events_after(
        &self,
        room_id: &RoomId,
        after_event_id: Option<&str>,
        limit: i32,
    ) -> Result<Vec<ChatMessageEventLog>> {
        let mut events = match self
            .chat_repository
            .list_events_after(room_id, after_event_id, limit)
            .await
        {
            Err(Error::NotFound(message)) if message == "Chat event not found" => {
                Err(Error::InvalidInput("Invalid chat event cursor".to_string()))
            }
            result => result,
        }?;
        self.attach_event_log_attachment_view_metadata(&mut events, None)
            .await?;
        Ok(events)
    }

    pub async fn get_events_after_sequence(
        &self,
        room_id: &RoomId,
        after_sequence: i64,
        limit: i32,
    ) -> Result<Vec<ChatMessageEventLog>> {
        let mut events = self
            .chat_repository
            .list_events_after_sequence(room_id, after_sequence, limit)
            .await?;
        self.attach_event_log_attachment_view_metadata(&mut events, None)
            .await?;
        Ok(events)
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
            .get_by_room_and_id_from_primary(&request.room_id, request.message_id)
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

    pub async fn get_message_read_receipts(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        message_id: i64,
        page: i32,
        page_size: i32,
    ) -> Result<ChatMessageReadReceiptsPage> {
        self.permission_service
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;

        let message = self
            .chat_repository
            .get_by_room_and_id_from_primary(room_id, message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if message.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }
        if message.user_id != Some(*user_id) {
            return Err(Error::Authorization(
                "Only the message sender can view read receipts".to_string(),
            ));
        }

        let event = self
            .chat_repository
            .created_event_for_message(room_id, message.id, message.created_at)
            .await?;
        self.chat_repository
            .message_read_receipts(room_id, &message, event.as_ref(), page, page_size)
            .await
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
                    ChatMessageOperationKind::Edit,
                    &request_hash,
                )
                .await?
            {
                return Ok(ChatMessageEventOutcome {
                    event: event.event,
                    inserted: false,
                    pin_event: None,
                });
            }
        }

        let current = self
            .chat_repository
            .get_by_room_and_id_from_primary(&request.room_id, request.message_id)
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
                        pin_event: None,
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
                operation_kind: ChatMessageOperationKind::Edit,
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
                .get_by_room_and_id_from_primary(&request.room_id, request.message_id)
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
                        pin_event: None,
                    });
                }
            }
            return Err(Error::OptimisticLockConflict);
        };

        Ok(ChatMessageEventOutcome {
            event: updated.event.event,
            inserted: updated.inserted,
            pin_event: updated.pin_event.map(|event| event.event),
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
        let current_with_attachments = self
            .chat_repository
            .get_with_attachments_by_room_and_id_from_primary(&request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        let current = &current_with_attachments.message;

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
                    ChatMessageOperationKind::Delete,
                    &request_hash,
                )
                .await?
            {
                return Ok(ChatMessageEventOutcome {
                    event: event.event,
                    inserted: false,
                    pin_event: None,
                });
            }
        }
        if current.status == ChatMessageStatus::Deleted {
            if let Some(event) = self.existing_delete_event(&request, current).await? {
                return Ok(ChatMessageEventOutcome {
                    event,
                    inserted: false,
                    pin_event: None,
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
                operation_kind: ChatMessageOperationKind::Delete,
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
                .get_by_room_and_id_from_primary(&request.room_id, request.message_id)
                .await?
                .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
            if let Some(event) = self.existing_delete_event(&request, &current).await? {
                return Ok(ChatMessageEventOutcome {
                    event,
                    inserted: false,
                    pin_event: None,
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

            let attachment_file_references = current_with_attachments
                .attachments
                .iter()
                .map(ChatAttachment::file_reference_target)
                .collect::<Vec<_>>();
            if let Err(error) = self
                .file_storage_service
                .schedule_delete_files(
                    FileStorageCleanupOrigin::ReferenceReleased,
                    &attachment_file_references,
                )
                .await
            {
                warn!(
                    room_id = %request.room_id,
                    message_id = %request.message_id,
                    error = %error,
                    "chat attachment cleanup scheduling failed after message deletion"
                );
            }
        }

        Ok(ChatMessageEventOutcome {
            event: deleted.event.event,
            inserted: deleted.inserted,
            pin_event: deleted.pin_event.map(|event| event.event),
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

    pub async fn list_pinned_messages(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        limit: i32,
    ) -> Result<Vec<ChatPinnedMessage>> {
        self.permission_service
            .check_permission(
                room_id,
                user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;

        self.chat_repository
            .list_pinned_messages_for_viewer(room_id, limit.clamp(1, 100), Some(user_id))
            .await
    }

    pub async fn list_pinned_messages_for_authorized_viewer(
        &self,
        room_id: &RoomId,
        viewer_user_id: Option<&UserId>,
        limit: i32,
    ) -> Result<Vec<ChatPinnedMessage>> {
        self.chat_repository
            .list_pinned_messages_for_viewer(room_id, limit.clamp(1, 100), viewer_user_id)
            .await
    }

    pub async fn pin_message_event_outcome(
        &self,
        request: PinChatMessage,
    ) -> Result<ChatPinEventOutcome> {
        validate_client_operation_id(request.client_operation_id.as_deref())?;
        if let Some(note) = request.note.as_deref() {
            if note.trim().is_empty() || note.chars().count() > CHAT_PIN_NOTE_MAX_CHARS {
                return Err(Error::InvalidInput(format!(
                    "chat pin note must be between 1 and {CHAT_PIN_NOTE_MAX_CHARS} characters"
                )));
            }
        }
        self.permission_service
            .check_permission(
                &request.room_id,
                &request.user_id,
                crate::models::RoomPermission::DELETE_CHAT,
            )
            .await?;

        let request_hash = chat_pin_request_hash(&request)?;
        if let Some(client_operation_id) = request.client_operation_id.as_deref() {
            if let Some(event) = self
                .chat_repository
                .replay_pin_operation_event(
                    &request.room_id,
                    &request.user_id,
                    client_operation_id,
                    ChatMessageOperationKind::Pin,
                    &request_hash,
                )
                .await?
            {
                return Ok(ChatPinEventOutcome {
                    event: event.event,
                    inserted: false,
                });
            }
        }

        let current = self
            .chat_repository
            .get_by_room_and_id_from_primary(&request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if current.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }
        let operation = request
            .client_operation_id
            .as_deref()
            .map(|client_operation_id| ChatMessageOperationIdempotency {
                client_operation_id,
                operation_kind: ChatMessageOperationKind::Pin,
                request_hash: &request_hash,
                message_id: request.message_id,
                message_created_at: current.created_at,
            });
        let event = self
            .chat_repository
            .pin_message_with_event(PinChatMessageEventRequest {
                room_id: &request.room_id,
                message_id: request.message_id,
                pinned_by: &request.user_id,
                note: request.note.as_deref(),
                max_pins_per_room: self.max_pinned_chat_messages_per_room()?,
                event_id: &synctv_common::snanoid!(16),
                occurred_at: Utc::now(),
                operation: operation.as_ref(),
            })
            .await?;
        Ok(ChatPinEventOutcome {
            event: event.event.event,
            inserted: event.inserted,
        })
    }

    pub async fn unpin_message_event_outcome(
        &self,
        request: UnpinChatMessage,
    ) -> Result<ChatPinEventOutcome> {
        validate_client_operation_id(request.client_operation_id.as_deref())?;
        self.permission_service
            .check_permission(
                &request.room_id,
                &request.user_id,
                crate::models::RoomPermission::DELETE_CHAT,
            )
            .await?;

        let request_hash = chat_unpin_request_hash(&request)?;
        if let Some(client_operation_id) = request.client_operation_id.as_deref() {
            if let Some(event) = self
                .chat_repository
                .replay_pin_operation_event(
                    &request.room_id,
                    &request.user_id,
                    client_operation_id,
                    ChatMessageOperationKind::Unpin,
                    &request_hash,
                )
                .await?
            {
                return Ok(ChatPinEventOutcome {
                    event: event.event,
                    inserted: false,
                });
            }
        }

        let current = self
            .chat_repository
            .get_by_room_and_id_from_primary(&request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        let operation = request
            .client_operation_id
            .as_deref()
            .map(|client_operation_id| ChatMessageOperationIdempotency {
                client_operation_id,
                operation_kind: ChatMessageOperationKind::Unpin,
                request_hash: &request_hash,
                message_id: request.message_id,
                message_created_at: current.created_at,
            });
        let event = self
            .chat_repository
            .unpin_message_with_event(UnpinChatMessageEventRequest {
                room_id: &request.room_id,
                message_id: request.message_id,
                unpinned_by: &request.user_id,
                event_id: &synctv_common::snanoid!(16),
                occurred_at: Utc::now(),
                operation: operation.as_ref(),
            })
            .await?;
        Ok(ChatPinEventOutcome {
            event: event.event.event,
            inserted: event.inserted,
        })
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

        let inserted = self
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
            event_id = %inserted.event.event.event_id,
            "Chat message reaction changed"
        );

        Ok(ChatMessageEventOutcome {
            event: inserted.event.event,
            inserted: inserted.inserted,
            pin_event: inserted.pin_event.map(|event| event.event),
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
        if max_messages == 0 {
            return Ok(0);
        }

        let deleted = self
            .message_cleanup_service
            .cleanup_room_cap(*room_id, max_messages_to_keep_count(max_messages)?)
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
        if max_messages == 0 {
            return Ok(0);
        }

        let deleted = self
            .message_cleanup_service
            .cleanup_active_rooms_cap(
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
    /// * `runtime_settings_store` - Runtime settings store to get `max_chat_messages` setting
    /// * `interval_seconds` - Cleanup interval in seconds (default: 60 seconds)
    /// * `activity_window_minutes` - Only cleanup rooms with messages in the last N minutes (default: 3 minutes)
    ///
    /// # Returns
    /// `JoinHandle` for the background task
    #[must_use]
    pub fn start_cleanup_task(
        self,
        runtime_settings_store: Arc<crate::service::RuntimeSettingsStore>,
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
                let max_messages = runtime_settings_store
                    .chat
                    .max_messages_per_room
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
            .get_by_room_and_id_from_primary(room_id, reply_to_message_id)
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
        let details = AuditDetails {
            room_id: Some(request.room_id.to_string()),
            message_id: Some(request.message_id.to_string()),
            message_created_at: Some(original.created_at),
            original_author_id: original.user_id.map(|user_id| user_id.to_string()),
            deleted_by: Some(request.user_id.to_string()),
            reason: request.reason.clone(),
            event_id: Some(event.event_id.clone()),
            client_operation_id: request.client_operation_id.clone(),
            ..Default::default()
        };

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

    async fn attach_attachment_view_metadata(
        &self,
        messages: &mut [ChatMessageWithAttachments],
        viewer_user_id: Option<&UserId>,
    ) -> Result<()> {
        let upload_policy = super::chat_attachment_upload_policy();
        let Some(repository) = self.file_storage_service.repository() else {
            if viewer_user_id.is_none() {
                return Ok(());
            }
            for message in messages {
                attach_chat_attachment_reuse_grants(
                    self.file_storage_service.as_ref(),
                    viewer_user_id
                        .copied()
                        .ok_or_else(|| Error::Internal("viewer_user_id disappeared".to_string()))?,
                    &mut message.attachments,
                )?;
            }
            return Ok(());
        };
        for message in messages.iter_mut() {
            crate::service::file_storage::attach_variants_to_chat_attachments(
                self.file_storage_service.as_ref(),
                repository.as_ref(),
                &mut message.attachments,
                upload_policy.object_kind,
            )
            .await?;
        }
        let Some(viewer_user_id) = viewer_user_id.copied() else {
            return Ok(());
        };
        for message in messages {
            attach_chat_attachment_reuse_grants(
                self.file_storage_service.as_ref(),
                viewer_user_id,
                &mut message.attachments,
            )?;
        }
        Ok(())
    }

    async fn attach_event_attachment_view_metadata(
        &self,
        event: &mut ChatMessageEvent,
        viewer_user_id: Option<&UserId>,
    ) -> Result<()> {
        self.attach_attachment_view_metadata(
            std::slice::from_mut(&mut event.message),
            viewer_user_id,
        )
        .await
    }

    async fn attach_event_log_attachment_view_metadata(
        &self,
        events: &mut [ChatMessageEventLog],
        viewer_user_id: Option<&UserId>,
    ) -> Result<()> {
        for event in events {
            self.attach_event_attachment_view_metadata(&mut event.event, viewer_user_id)
                .await?;
        }
        Ok(())
    }

    async fn prepare_submitted_chat_attachments(
        &self,
        user_id: UserId,
        target_room_id: RoomId,
        storage_scope: &str,
        object_kind: crate::models::FileObjectKind,
        client_request_id: Option<&str>,
        attachments: Vec<SubmittedFileReference>,
    ) -> Result<Vec<crate::models::NewStoredFile>> {
        let mut upload_references = Vec::new();
        let mut reuse_files = Vec::new();
        for reference in attachments {
            match reference.kind {
                SubmittedFileReferenceKind::Upload => upload_references.push(reference),
                SubmittedFileReferenceKind::Reuse => {
                    reuse_files.push(
                        self.prepare_chat_reuse_attachment(target_room_id, user_id, &reference)
                            .await?,
                    );
                }
            }
        }

        let context = FileStorageContext {
            user_id,
            storage_scope,
            object_kind,
            client_request_id,
        };
        let mut prepared = self
            .file_storage_service
            .prepare_submitted_files(context, upload_references)
            .await?;
        let mut prepared_reuse = self
            .file_storage_service
            .prepare_files(context, reuse_files)
            .await?;
        prepared.append(&mut prepared_reuse);
        Ok(prepared)
    }

    async fn prepare_chat_reuse_attachment(
        &self,
        target_room_id: RoomId,
        user_id: UserId,
        reference: &SubmittedFileReference,
    ) -> Result<crate::models::NewStoredFile> {
        let storage_scope = chat_file_storage_scope(target_room_id, user_id);
        let upload_policy = super::chat_attachment_upload_policy();
        let grant = self
            .file_storage_service
            .validate_reuse_grant(
                reference.id.trim(),
                FileStorageContext {
                    user_id,
                    storage_scope: &storage_scope,
                    object_kind: upload_policy.object_kind,
                    client_request_id: None,
                },
            )
            .await?;
        if grant.source_kind != CHAT_ATTACHMENT_REUSE_SOURCE_KIND {
            return Err(Error::InvalidInput(
                "file reuse token is not valid for chat attachments".to_string(),
            ));
        }
        let (source_room_id, source_message_id, source_created_at_micros, source_attachment_id) =
            parse_chat_attachment_reuse_source_id(&grant.source_id)?;
        if source_room_id != target_room_id {
            return Err(Error::Authorization(
                "Chat attachment reuse is limited to the source room".to_string(),
            ));
        }
        self.permission_service
            .check_permission(
                &source_room_id,
                &user_id,
                crate::models::RoomPermission::VIEW_CHAT_HISTORY,
            )
            .await?;
        let source = self
            .chat_repository
            .get_with_attachments_by_room_and_id_for_viewer(
                &source_room_id,
                source_message_id,
                Some(&user_id),
            )
            .await?
            .ok_or_else(|| Error::NotFound("Source chat attachment not found".to_string()))?;
        if source.message.status == ChatMessageStatus::Deleted {
            return Err(Error::NotFound(
                "Source chat attachment not found".to_string(),
            ));
        }
        let source_attachment = source
            .attachments
            .iter()
            .find(|attachment| {
                attachment.id == source_attachment_id
                    && attachment.message_created_at.timestamp_micros() == source_created_at_micros
            })
            .ok_or_else(|| Error::NotFound("Source chat attachment not found".to_string()))?;
        Ok(crate::models::NewStoredFile {
            id: format!("att_{}", synctv_common::snanoid!(24)),
            filename: source_attachment.filename.clone(),
            storage_backend: source_attachment.storage_backend.clone(),
            object_key: source_attachment.object_key.clone(),
            object_access: None,
            url: None,
            mime_type: source_attachment.mime_type.clone(),
            size_bytes: source_attachment.size_bytes,
            width: source_attachment.width,
            height: source_attachment.height,
            metadata: source_attachment.metadata.clone(),
        })
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
