use std::collections::HashMap;

use crate::repository::RoomResourceEventPayload;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Executor, PgPool, Postgres, Transaction};

use crate::{
    models::{
        ChatAttachment, ChatAttachmentKind, ChatEventKind, ChatHistoryCursor, ChatHistoryPage,
        ChatMention, ChatMentionInput, ChatMessage, ChatMessageContext, ChatMessageEvent,
        ChatMessageEventLog, ChatMessageOperationKind, ChatMessagePin,
        ChatMessageReadReceiptMember, ChatMessageReadReceiptUser, ChatMessageReadReceiptsPage,
        ChatMessageSelection, ChatMessageStatus, ChatMessageType, ChatMessageWithAttachments,
        ChatMetadata, ChatPinEvent, ChatPinEventKind, ChatPinEventLog, ChatPinnedMessage,
        ChatPlaybackMessagesQuery, ChatReactionSummary, ChatReactionUser, ChatReactionUsersCursor,
        ChatReactionUsersPage, ChatReadState, ChatSearchMessagesPage, ChatSearchMessagesQuery,
        EventCursor, NewStoredFile, RoomId, SetChatReaction, User, UserId,
        CHAT_ATTACHMENT_FILENAME_MAX_CHARS, CHAT_ATTACHMENT_ID_MAX_CHARS,
        CHAT_CLIENT_MESSAGE_ID_MAX_CHARS, CHAT_CLIENT_OPERATION_ID_MAX_CHARS,
        CHAT_EVENT_ID_MAX_CHARS, CHAT_EVENT_TYPE_MAX_CHARS, CHAT_PIN_NOTE_MAX_CHARS,
        CHAT_REACTION_KEY_MAX_CHARS, FILE_OBJECT_KEY_MAX_CHARS, FILE_STORAGE_BACKEND_MAX_CHARS,
    },
    repository::{
        pools::RepoPools, room_resource_event::insert_room_resource_event_with_executor,
        FileStorageRepository, NewRoomResourceEvent, RoomResourceEventScope,
    },
    Error, Result,
};

type ChatMessageKey = (i64, DateTime<Utc>);

pub struct InsertChatMessageEvent<'a> {
    pub message: &'a ChatMessage,
    pub attachments: &'a [NewStoredFile],
    pub mentions: &'a [ChatMentionInput],
    pub actor_user_id: UserId,
    pub event_id: &'a str,
    pub occurred_at: DateTime<Utc>,
}

struct ChatHistoryCursorRequest<'a> {
    room_id: &'a RoomId,
    cursor: Option<ChatHistoryCursor>,
    limit: i32,
    include_deleted: bool,
    viewer_user_id: Option<&'a UserId>,
    selection: &'a ChatMessageSelection,
}

const CHAT_MESSAGE_CREATED_EVENT_TYPE: &str = "chat_message_created";
const CHAT_MESSAGE_EDITED_EVENT_TYPE: &str = "chat_message_edited";
const CHAT_MESSAGE_DELETED_EVENT_TYPE: &str = "chat_message_deleted";
const CHAT_MESSAGE_REACTIONS_CHANGED_EVENT_TYPE: &str = "chat_message_reactions_changed";
const CHAT_MESSAGE_EVENT_TYPES: [&str; 4] = [
    CHAT_MESSAGE_CREATED_EVENT_TYPE,
    CHAT_MESSAGE_EDITED_EVENT_TYPE,
    CHAT_MESSAGE_DELETED_EVENT_TYPE,
    CHAT_MESSAGE_REACTIONS_CHANGED_EVENT_TYPE,
];
const CHAT_PINS_RESOURCE_TYPE: &str = "chat_pins";

#[derive(sqlx::FromRow)]
struct ChatMessageRow {
    id: i64,
    room_id: RoomId,
    user_id: Option<UserId>,
    client_message_id: Option<String>,
    content: String,
    message_type: ChatMessageType,
    status: ChatMessageStatus,
    version: i64,
    reply_to_message_id: Option<i64>,
    reply_to_message_created_at: Option<DateTime<Utc>>,
    metadata: Option<ChatMetadata>,
    edited_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    deleted_by: Option<UserId>,
    delete_reason: Option<String>,
    created_at: DateTime<Utc>,
}

impl TryFrom<ChatMessageRow> for ChatMessage {
    type Error = crate::Error;

    fn try_from(row: ChatMessageRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            room_id: row.room_id,
            user_id: row.user_id,
            client_message_id: row.client_message_id,
            content: row.content,
            message_type: row.message_type,
            status: row.status,
            version: row.version,
            reply_to_message_id: row.reply_to_message_id,
            reply_to_message_created_at: row.reply_to_message_created_at,
            metadata: row.metadata,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
            deleted_by: row.deleted_by,
            delete_reason: row.delete_reason,
            created_at: row.created_at,
        })
    }
}

fn chat_message_event_types() -> Vec<String> {
    CHAT_MESSAGE_EVENT_TYPES
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn chat_message_key(message: &ChatMessage) -> ChatMessageKey {
    (message.id, message.created_at)
}

fn chat_message_from_row(row: ChatMessageRow) -> Result<ChatMessage> {
    row.try_into()
}

fn chat_messages_from_rows(rows: Vec<ChatMessageRow>) -> Result<Vec<ChatMessage>> {
    rows.into_iter().map(chat_message_from_row).collect()
}

fn optional_chat_message_from_row(row: Option<ChatMessageRow>) -> Result<Option<ChatMessage>> {
    row.map(chat_message_from_row).transpose()
}

fn chat_pin_resource_id(message: &ChatMessage) -> String {
    chat_pin_resource_id_parts(message.id, message.created_at)
}

fn chat_pin_resource_id_parts(message_id: i64, message_created_at: DateTime<Utc>) -> String {
    format!("{}:{}", message_id, message_created_at.timestamp_micros())
}

fn chat_attachment_message_key(attachment: &ChatAttachment) -> ChatMessageKey {
    (attachment.message_id, attachment.message_created_at)
}

fn validate_optional_text(value: Option<&str>, field: &str, max_chars: usize) -> Result<()> {
    if let Some(value) = value {
        validate_required_text(value, field, max_chars)?;
    }
    Ok(())
}

fn validate_required_text(value: &str, field: &str, max_chars: usize) -> Result<()> {
    let len = value.chars().count();
    if value.trim().is_empty() || len > max_chars {
        return Err(Error::InvalidInput(format!(
            "{field} must be between 1 and {max_chars} characters"
        )));
    }
    Ok(())
}

fn validate_message_for_insert(message: &ChatMessage) -> Result<()> {
    validate_optional_text(
        message.client_message_id.as_deref(),
        "client_message_id",
        CHAT_CLIENT_MESSAGE_ID_MAX_CHARS,
    )?;
    if message.version < 1 {
        return Err(Error::InvalidInput(
            "chat message version must be positive".to_string(),
        ));
    }
    if message
        .metadata
        .as_ref()
        .is_some_and(|metadata| metadata.message_type() != message.message_type)
    {
        return Err(Error::InvalidInput(
            "chat metadata type must match chat message type".to_string(),
        ));
    }
    match (
        message.reply_to_message_id,
        message.reply_to_message_created_at,
    ) {
        (Some(_), Some(_)) | (None, None) => Ok(()),
        _ => Err(Error::InvalidInput(
            "reply target requires both message id and created_at".to_string(),
        )),
    }
}

fn validate_chat_attachment_for_insert(attachment: &NewStoredFile) -> Result<()> {
    validate_required_text(
        &attachment.id,
        "chat attachment id",
        CHAT_ATTACHMENT_ID_MAX_CHARS,
    )?;
    validate_optional_text(
        attachment.filename.as_deref(),
        "chat attachment filename",
        CHAT_ATTACHMENT_FILENAME_MAX_CHARS,
    )?;
    validate_required_text(
        &attachment.storage_backend,
        "file storage_backend",
        FILE_STORAGE_BACKEND_MAX_CHARS,
    )?;
    validate_required_text(
        &attachment.object_key,
        "file object_key",
        FILE_OBJECT_KEY_MAX_CHARS,
    )?;
    if attachment.size_bytes.is_some_and(|size| size <= 0)
        || attachment.width.is_some_and(|width| width <= 0)
        || attachment.height.is_some_and(|height| height <= 0)
    {
        return Err(Error::InvalidInput(
            "chat attachment size and dimensions must be positive".to_string(),
        ));
    }
    Ok(())
}

fn validate_chat_event_for_insert(event: &ChatMessageEvent, event_type: &str) -> Result<()> {
    validate_required_text(&event.event_id, "chat event_id", CHAT_EVENT_ID_MAX_CHARS)?;
    validate_required_text(event_type, "chat event_type", CHAT_EVENT_TYPE_MAX_CHARS)?;
    if event.message.message.version < 1 {
        return Err(Error::InvalidInput(
            "chat message_version must be positive".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct ChatRepository {
    pools: RepoPools,
}

impl ChatRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pools: RepoPools::new(pool),
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
        }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        self.pools.primary()
    }

    #[must_use]
    pub fn eventually_consistent_pool(&self) -> &PgPool {
        self.pools.read()
    }

    pub async fn create(&self, message: &ChatMessage) -> Result<ChatMessage> {
        validate_message_for_insert(message)?;
        let metadata = ChatMetadata::normalized_for_optional_storage(message.metadata.as_ref())?;
        let inserted = sqlx::query_as!(
            ChatMessageRow,
            r#"
            INSERT INTO chat_messages (
                room_id, user_id, client_message_id, content, message_type,
                status, version, reply_to_message_id, reply_to_message_created_at,
                metadata, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id AS "id!",
                      room_id AS "room_id!: RoomId",
                      user_id AS "user_id?: UserId",
                      client_message_id,
                      content AS "content!",
                      message_type AS "message_type!: ChatMessageType",
                      status AS "status!: ChatMessageStatus",
                      version AS "version!",
                      reply_to_message_id,
                      reply_to_message_created_at,
                      metadata AS "metadata?: ChatMetadata",
                      edited_at,
                      deleted_at,
                      deleted_by AS "deleted_by?: UserId",
                      delete_reason,
                      created_at AS "created_at!"
            "#,
            message.room_id.as_i64(),
            message.user_id.map(|id| id.as_i64()),
            message.client_message_id.as_deref(),
            &message.content,
            i16::from(message.message_type),
            i16::from(message.status),
            message.version,
            message.reply_to_message_id,
            message.reply_to_message_created_at,
            &metadata as _,
            message.created_at
        )
        .fetch_one(self.pool())
        .await?;

        chat_message_from_row(inserted)
    }

    pub async fn insert_message_event_idempotent(
        &self,
        message: &ChatMessage,
        attachments: &[NewStoredFile],
        mentions: &[ChatMentionInput],
        request_hash: &str,
        event_id: &str,
        occurred_at: DateTime<Utc>,
    ) -> Result<IdempotentChatEventInsert> {
        validate_message_for_insert(message)?;
        if let Some(client_message_id) = message.client_message_id.as_deref() {
            validate_required_text(
                client_message_id,
                "client_message_id",
                CHAT_CLIENT_MESSAGE_ID_MAX_CHARS,
            )?;
        }
        for attachment in attachments {
            validate_chat_attachment_for_insert(attachment)?;
        }
        let sender_id = message.user_id.ok_or_else(|| {
            Error::InvalidInput("Chat message event insert requires a sender".to_string())
        })?;
        let mut tx = self.pool().begin().await?;

        if let Some(client_message_id) = &message.client_message_id {
            let inserted_idempotency = sqlx::query!(
                r"
                INSERT INTO chat_message_idempotency (
                    room_id, user_id, client_message_id, request_hash
                )
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (room_id, user_id, client_message_id) DO NOTHING
                ",
                message.room_id.as_i64(),
                sender_id.as_i64(),
                client_message_id,
                request_hash
            )
            .execute(&mut *tx)
            .await?;

            if inserted_idempotency.rows_affected() == 0 {
                let existing = self
                    .get_idempotent_message_for_update(
                        &mut tx,
                        &message.room_id,
                        &sender_id,
                        client_message_id,
                    )
                    .await?
                    .ok_or_else(|| {
                        Error::Internal("idempotency record disappeared during insert".to_string())
                    })?;
                if let Some(event) = self
                    .existing_idempotent_event_in_tx(
                        &mut tx,
                        ExistingIdempotentEventRequest {
                            message,
                            client_message_id,
                            request_hash,
                            existing,
                            event_id,
                            occurred_at,
                        },
                    )
                    .await?
                {
                    tx.commit().await?;
                    return Ok(IdempotentChatEventInsert {
                        event,
                        inserted: false,
                        pin_event: None,
                    });
                }
            }
        }

        let inserted = self.insert_message_in_tx(&mut tx, message).await?;
        let inserted_attachments = self
            .insert_attachments_in_tx(&mut tx, &inserted, attachments)
            .await?;
        let inserted_mentions = self
            .insert_mentions_in_tx(&mut tx, &inserted, mentions)
            .await?;
        let event = ChatMessageEvent {
            event_id: event_id.to_string(),
            sequence: 0,
            room_id: message.room_id,
            actor_user_id: sender_id,
            kind: ChatEventKind::Created,
            message: ChatMessageWithAttachments {
                message: inserted,
                attachments: inserted_attachments,
                reactions: Vec::new(),
                mentions: inserted_mentions,
                pin: None,
            },
            occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;

        if let Some(client_message_id) = &message.client_message_id {
            sqlx::query!(
                r"
                UPDATE chat_message_idempotency
                SET message_id = $4, message_created_at = $5, event_id = $6
                WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
                ",
                message.room_id.as_i64(),
                sender_id.as_i64(),
                client_message_id,
                logged.event.message.message.id,
                logged.event.message.message.created_at,
                &logged.event.event_id
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(IdempotentChatEventInsert {
            event: logged,
            inserted: true,
            pin_event: None,
        })
    }

    pub async fn insert_event(&self, event: &ChatMessageEvent) -> Result<ChatMessageEventLog> {
        let mut tx = self.pool().begin().await?;
        let logged = self.insert_event_in_tx(&mut tx, event).await?;
        tx.commit().await?;
        Ok(logged)
    }

    pub async fn insert_message_event(
        &self,
        message: &ChatMessage,
        attachments: &[NewStoredFile],
        mentions: &[ChatMentionInput],
        actor_user_id: UserId,
        event_id: &str,
        occurred_at: DateTime<Utc>,
    ) -> Result<ChatMessageEventLog> {
        let mut tx = self.pool().begin().await?;
        let logged = self
            .insert_message_event_in_tx(
                &mut tx,
                InsertChatMessageEvent {
                    message,
                    attachments,
                    mentions,
                    actor_user_id,
                    event_id,
                    occurred_at,
                },
            )
            .await?;
        tx.commit().await?;
        Ok(logged)
    }

    pub async fn insert_message_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: InsertChatMessageEvent<'_>,
    ) -> Result<ChatMessageEventLog> {
        validate_message_for_insert(request.message)?;
        for attachment in request.attachments {
            validate_chat_attachment_for_insert(attachment)?;
        }

        let inserted = self.insert_message_in_tx(tx, request.message).await?;
        let inserted_attachments = self
            .insert_attachments_in_tx(tx, &inserted, request.attachments)
            .await?;
        let inserted_mentions = self
            .insert_mentions_in_tx(tx, &inserted, request.mentions)
            .await?;
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: request.message.room_id,
            actor_user_id: request.actor_user_id,
            kind: ChatEventKind::Created,
            message: ChatMessageWithAttachments {
                message: inserted,
                attachments: inserted_attachments,
                reactions: Vec::new(),
                mentions: inserted_mentions,
                pin: None,
            },
            occurred_at: request.occurred_at,
        };
        self.insert_event_in_tx(tx, &event).await
    }

    pub async fn list_pinned_messages_for_viewer(
        &self,
        room_id: &RoomId,
        limit: i32,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatPinnedMessage>> {
        let limit = limit.clamp(1, 100);
        let pool = self.eventually_consistent_pool();
        let rows = sqlx::query_as!(
            ChatMessagePin,
            r#"
            SELECT p.room_id AS "room_id!: RoomId",
                   p.message_id AS "message_id!",
                   p.message_created_at AS "message_created_at!",
                   p.pinned_by AS "pinned_by?: UserId",
                   u.username AS pinned_by_username,
                   p.note,
                   p.pinned_at AS "pinned_at!"
            FROM chat_message_pins p
            LEFT JOIN users u ON u.id = p.pinned_by
            JOIN chat_messages m
              ON m.room_id = p.room_id
             AND m.id = p.message_id
             AND m.created_at = p.message_created_at
            WHERE p.room_id = $1
              AND m.status <> $2
              AND m.deletion_source IS DISTINCT FROM 'account'
            ORDER BY p.pinned_at DESC, p.message_id DESC
            LIMIT $3
            "#,
            room_id.as_i64(),
            i16::from(ChatMessageStatus::Deleted),
            i64::from(limit)
        )
        .fetch_all(pool)
        .await?;

        let messages = self
            .messages_for_pin_rows(room_id, &rows, viewer_user_id)
            .await?;
        Ok(rows
            .into_iter()
            .zip(messages)
            .map(|(pin, message)| ChatPinnedMessage { pin, message })
            .collect())
    }

    pub async fn pin_message_with_event(
        &self,
        request: PinChatMessageEventRequest<'_>,
    ) -> Result<IdempotentChatPinEventInsert> {
        validate_optional_text(request.note, "chat pin note", CHAT_PIN_NOTE_MAX_CHARS)?;
        let mut tx = self.pool().begin().await?;
        if let Some(operation) = request.operation {
            if let Some(event) = self
                .begin_pin_operation_in_tx(&mut tx, request.room_id, request.pinned_by, operation)
                .await?
            {
                tx.commit().await?;
                return Ok(IdempotentChatPinEventInsert {
                    event,
                    inserted: false,
                });
            }
        }

        let message = self
            .get_message_for_update_in_tx(&mut tx, request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if message.status == ChatMessageStatus::Deleted {
            if let Some(operation) = request.operation {
                self.clear_incomplete_message_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.pinned_by,
                    operation,
                )
                .await?;
            }
            tx.commit().await?;
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }

        if self
            .pin_for_message_in_tx(&mut tx, request.room_id, message.id, message.created_at)
            .await?
            .is_some()
        {
            let event = self
                .latest_pin_event_for_message_in_tx(
                    &mut tx,
                    request.room_id,
                    message.id,
                    message.created_at,
                    ChatPinEventKind::Pinned,
                )
                .await?
                .ok_or_else(|| {
                    Error::Internal("chat pin exists without durable pin event".to_string())
                })?;
            if let Some(operation) = request.operation {
                self.complete_pin_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.pinned_by,
                    operation,
                    &event.event,
                )
                .await?;
            }
            tx.commit().await?;
            return Ok(IdempotentChatPinEventInsert {
                event,
                inserted: false,
            });
        }

        if let Some(max_pins_per_room) = request.max_pins_per_room {
            self.lock_room_pins_in_tx(&mut tx, request.room_id).await?;
            let current_count = self
                .count_active_pins_in_tx(&mut tx, request.room_id)
                .await?;
            if current_count >= max_pins_per_room {
                if let Some(operation) = request.operation {
                    self.clear_incomplete_message_operation_in_tx(
                        &mut tx,
                        request.room_id,
                        request.pinned_by,
                        operation,
                    )
                    .await?;
                }
                tx.commit().await?;
                return Err(Error::Conflict(format!(
                    "Room pinned chat message limit reached ({max_pins_per_room})"
                )));
            }
        }

        let inserted_pin = self
            .upsert_pin_in_tx(
                &mut tx,
                &message,
                request.pinned_by,
                request.note,
                request.occurred_at,
            )
            .await?;
        if !inserted_pin {
            let event = self
                .latest_pin_event_for_message_in_tx(
                    &mut tx,
                    request.room_id,
                    message.id,
                    message.created_at,
                    ChatPinEventKind::Pinned,
                )
                .await?
                .ok_or_else(|| {
                    Error::Internal("chat pin exists without durable pin event".to_string())
                })?;
            if let Some(operation) = request.operation {
                self.complete_pin_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.pinned_by,
                    operation,
                    &event.event,
                )
                .await?;
            }
            tx.commit().await?;
            return Ok(IdempotentChatPinEventInsert {
                event,
                inserted: false,
            });
        }

        let loaded = self
            .message_event_payload_in_tx(
                &mut tx,
                request.room_id,
                &message,
                Some(request.pinned_by),
            )
            .await?;
        let pin = self
            .pin_for_message_in_tx(&mut tx, request.room_id, message.id, message.created_at)
            .await?;
        let event = ChatPinEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: *request.room_id,
            actor_user_id: *request.pinned_by,
            kind: ChatPinEventKind::Pinned,
            message: loaded,
            pin,
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_pin_event_in_tx(&mut tx, &event).await?;
        if let Some(operation) = request.operation {
            self.complete_pin_operation_in_tx(
                &mut tx,
                request.room_id,
                request.pinned_by,
                operation,
                &logged.event,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(IdempotentChatPinEventInsert {
            event: logged,
            inserted: true,
        })
    }

    pub async fn unpin_message_with_event(
        &self,
        request: UnpinChatMessageEventRequest<'_>,
    ) -> Result<IdempotentChatPinEventInsert> {
        let mut tx = self.pool().begin().await?;
        if let Some(operation) = request.operation {
            if let Some(event) = self
                .begin_pin_operation_in_tx(&mut tx, request.room_id, request.unpinned_by, operation)
                .await?
            {
                tx.commit().await?;
                return Ok(IdempotentChatPinEventInsert {
                    event,
                    inserted: false,
                });
            }
        }

        let message = self
            .get_message_for_update_in_tx(&mut tx, request.room_id, request.message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        let deleted = sqlx::query!(
            r"
            DELETE FROM chat_message_pins
            WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3
            ",
            request.room_id.as_i64(),
            message.id,
            message.created_at
        )
        .execute(&mut *tx)
        .await?;
        if deleted.rows_affected() == 0 {
            if let Some(operation) = request.operation {
                self.clear_incomplete_message_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.unpinned_by,
                    operation,
                )
                .await?;
            }
            tx.commit().await?;
            return Err(Error::NotFound("Chat message pin not found".to_string()));
        }

        let loaded = self
            .message_event_payload_in_tx(
                &mut tx,
                request.room_id,
                &message,
                Some(request.unpinned_by),
            )
            .await?;
        let event = ChatPinEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: *request.room_id,
            actor_user_id: *request.unpinned_by,
            kind: ChatPinEventKind::Unpinned,
            message: loaded,
            pin: None,
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_pin_event_in_tx(&mut tx, &event).await?;
        if let Some(operation) = request.operation {
            self.complete_pin_operation_in_tx(
                &mut tx,
                request.room_id,
                request.unpinned_by,
                operation,
                &logged.event,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(IdempotentChatPinEventInsert {
            event: logged,
            inserted: true,
        })
    }

    pub async fn set_reaction_with_event(
        &self,
        request: &SetChatReaction,
        event_id: &str,
        occurred_at: DateTime<Utc>,
    ) -> Result<IdempotentChatEventInsert> {
        validate_required_text(
            &request.reaction_key,
            "reaction_key",
            CHAT_REACTION_KEY_MAX_CHARS,
        )?;
        validate_required_text(event_id, "chat event_id", CHAT_EVENT_ID_MAX_CHARS)?;
        let mut tx = self.pool().begin().await?;
        let message = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
              AND id = $2
              AND deletion_source IS DISTINCT FROM 'account'
            FOR UPDATE
            "#,
            request.room_id.as_i64(),
            request.message_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        let message = chat_message_from_row(message)?;

        if message.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }

        if request.enabled {
            sqlx::query!(
                r"
                INSERT INTO chat_message_reactions (
                    room_id, message_id, message_created_at, user_id, reaction_key
                )
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (room_id, message_id, message_created_at, user_id, reaction_key)
                DO UPDATE SET updated_at = NOW()
                ",
                request.room_id.as_i64(),
                message.id,
                message.created_at,
                request.user_id.as_i64(),
                &request.reaction_key
            )
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query!(
                r"
                DELETE FROM chat_message_reactions
                WHERE room_id = $1
                  AND message_id = $2
                  AND message_created_at = $3
                  AND user_id = $4
                  AND reaction_key = $5
                ",
                request.room_id.as_i64(),
                message.id,
                message.created_at,
                request.user_id.as_i64(),
                &request.reaction_key
            )
            .execute(&mut *tx)
            .await?;
        }

        let attachments = self
            .attachments_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let mut reactions = self
            .reaction_summaries_for_messages_with_executor(
                &mut *tx,
                std::slice::from_ref(&message),
                Some(&request.user_id),
            )
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        reactions.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.key.cmp(&right.key))
        });
        let mentions = self
            .mentions_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let pin = self
            .pin_for_message_in_tx(&mut tx, &request.room_id, message.id, message.created_at)
            .await?;
        let event = ChatMessageEvent {
            event_id: event_id.to_string(),
            sequence: 0,
            room_id: request.room_id,
            actor_user_id: request.user_id,
            kind: ChatEventKind::ReactionsChanged,
            message: ChatMessageWithAttachments {
                message,
                attachments,
                reactions,
                mentions,
                pin: pin.clone(),
            },
            occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;
        let pin_event = if pin.is_some() {
            Some(
                self.insert_pin_event_in_tx(
                    &mut tx,
                    &ChatPinEvent {
                        event_id: synctv_common::snanoid!(16),
                        sequence: 0,
                        room_id: request.room_id,
                        actor_user_id: request.user_id,
                        kind: ChatPinEventKind::MessageUpdated,
                        message: logged.event.message.clone(),
                        pin,
                        occurred_at,
                    },
                )
                .await?,
            )
        } else {
            None
        };
        tx.commit().await?;
        Ok(IdempotentChatEventInsert {
            event: logged,
            inserted: true,
            pin_event,
        })
    }

    pub async fn list_reaction_users(
        &self,
        room_id: &RoomId,
        message_id: i64,
        reaction_key: &str,
        cursor: Option<ChatReactionUsersCursor>,
        limit: i32,
    ) -> Result<ChatReactionUsersPage> {
        let limit = limit.clamp(1, 100);
        let message = self
            .get_by_room_and_id(room_id, message_id)
            .await?
            .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;
        if message.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }

        let pool = self.eventually_consistent_pool();
        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)::bigint AS "total!"
            FROM chat_message_reactions
            WHERE room_id = $1
              AND message_id = $2
              AND message_created_at = $3
              AND reaction_key = $4
            "#,
            room_id.as_i64(),
            message.id,
            message.created_at,
            reaction_key
        )
        .fetch_one(pool)
        .await?;

        let page_limit = usize::try_from(limit)
            .map_err(|_| Error::Internal("chat reaction user limit exceeds usize::MAX".into()))?;
        let fetch_limit = limit + 1;
        let cursor_reacted_at = cursor.map(|cursor| cursor.reacted_at);
        let cursor_user_id = cursor.map(|cursor| cursor.user_id.as_i64());
        let rows = sqlx::query_as!(
            ChatReactionUser,
            r#"
            SELECT
                user_id AS "user_id!: UserId",
                reaction_key AS "reaction_key!",
                updated_at AS "reacted_at!"
            FROM chat_message_reactions
            WHERE room_id = $1
              AND message_id = $2
              AND message_created_at = $3
              AND reaction_key = $4
              AND (
                  $5::timestamptz IS NULL
                  OR (updated_at, user_id) < ($5::timestamptz, $6::bigint)
              )
            ORDER BY updated_at DESC, user_id DESC
            LIMIT $7
            "#,
            room_id.as_i64(),
            message.id,
            message.created_at,
            reaction_key,
            cursor_reacted_at,
            cursor_user_id,
            i64::from(fetch_limit)
        )
        .fetch_all(pool)
        .await?;

        let mut users = rows;
        let next_cursor = if users.len() > page_limit {
            users.pop();
            users.last().map(|last| ChatReactionUsersCursor {
                reacted_at: last.reacted_at,
                user_id: last.user_id,
            })
        } else {
            None
        };

        Ok(ChatReactionUsersPage {
            users,
            next_cursor,
            total,
        })
    }

    pub async fn get_event_by_id(
        &self,
        room_id: &RoomId,
        event_id: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let mut tx = self.pool().begin().await?;
        let event = self
            .get_event_by_id_in_tx(&mut tx, room_id, event_id)
            .await?;
        tx.commit().await?;
        Ok(event)
    }

    pub async fn replay_idempotent_send_event(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        client_message_id: &str,
        request_hash: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let mut tx = self.pool().begin().await?;
        let existing = self
            .get_idempotent_message_for_update(&mut tx, room_id, user_id, client_message_id)
            .await?;
        let Some(existing) = existing else {
            tx.commit().await?;
            return Ok(None);
        };
        if existing.request_hash != request_hash {
            return Err(Error::Conflict(
                "client_message_id was already used with a different payload".to_string(),
            ));
        }

        let (message_id, created_at) = match (existing.message_id, existing.message_created_at) {
            (Some(message_id), Some(created_at)) => (message_id, created_at),
            (None, None) => {
                tx.commit().await?;
                return Ok(None);
            }
            _ => {
                return Err(Error::Internal(
                    "idempotency record has an incomplete message cursor".to_string(),
                ));
            }
        };

        let event = if let Some(existing_event_id) = existing.event_id {
            self.get_event_by_id_in_tx(&mut tx, room_id, &existing_event_id)
                .await?
                .ok_or_else(|| {
                    Error::Internal("idempotency record points to a missing chat event".to_string())
                })?
        } else {
            let loaded = self
                .get_with_attachments_in_tx(&mut tx, room_id, message_id, created_at)
                .await?
                .ok_or_else(|| {
                    Error::Internal(
                        "idempotency record points to a missing chat message".to_string(),
                    )
                })?;
            let event = ChatMessageEvent {
                event_id: synctv_common::snanoid!(16),
                sequence: 0,
                room_id: *room_id,
                actor_user_id: *user_id,
                kind: ChatEventKind::Created,
                message: loaded,
                occurred_at: crate::SystemClock.now(),
            };
            let logged = self.insert_event_in_tx(&mut tx, &event).await?;
            sqlx::query!(
                r"
                UPDATE chat_message_idempotency
                SET event_id = $4
                WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
                ",
                room_id.as_i64(),
                user_id.as_i64(),
                client_message_id,
                &logged.event.event_id
            )
            .execute(&mut *tx)
            .await?;
            logged
        };
        tx.commit().await?;
        Ok(Some(event))
    }

    pub async fn replay_message_operation_event(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        client_operation_id: &str,
        operation_kind: ChatMessageOperationKind,
        request_hash: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let mut tx = self.pool().begin().await?;
        let event = self
            .replay_message_operation_event_in_tx(
                &mut tx,
                room_id,
                user_id,
                client_operation_id,
                operation_kind,
                request_hash,
            )
            .await?;
        tx.commit().await?;
        Ok(event)
    }

    pub async fn replay_pin_operation_event(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        client_operation_id: &str,
        operation_kind: ChatMessageOperationKind,
        request_hash: &str,
    ) -> Result<Option<ChatPinEventLog>> {
        let mut tx = self.pool().begin().await?;
        let event = self
            .replay_pin_operation_event_in_tx(
                &mut tx,
                room_id,
                user_id,
                client_operation_id,
                operation_kind,
                request_hash,
            )
            .await?;
        tx.commit().await?;
        Ok(event)
    }

    async fn insert_file_reference_for_attachment_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        attachment: &ChatAttachment,
    ) -> Result<()> {
        let reference_id = file_reference_id_for_chat_attachment(attachment);
        FileStorageRepository::insert_reference_in_tx(
            tx,
            &attachment.storage_backend,
            &attachment.object_key,
            "chat_message_attachment",
            &reference_id,
            None,
            &crate::models::FileReferenceMetadata::File(crate::models::FileMetadata::default()),
        )
        .await?
        .ok_or_else(|| {
            crate::Error::InvalidInput("chat attachment object is not registered".to_string())
        })?;
        Ok(())
    }

    pub async fn list_events_after(
        &self,
        room_id: &RoomId,
        after_event_id: Option<&str>,
        limit: i32,
    ) -> Result<Vec<ChatMessageEventLog>> {
        self.list_events_after_with_selection(
            room_id,
            after_event_id,
            limit,
            &ChatMessageSelection::user_default(),
        )
        .await
    }

    pub async fn list_events_after_with_selection(
        &self,
        room_id: &RoomId,
        after_event_id: Option<&str>,
        limit: i32,
        selection: &ChatMessageSelection,
    ) -> Result<Vec<ChatMessageEventLog>> {
        let limit = limit.clamp(1, 500);
        let after_sequence =
            if let Some(event_id) = after_event_id.filter(|id| !id.trim().is_empty()) {
                Some(
                    self.get_event_by_id(room_id, event_id)
                        .await?
                        .ok_or_else(|| Error::NotFound("Chat event not found".to_string()))?
                        .sequence,
                )
            } else {
                None
            };

        let event_types = chat_message_event_types();
        let included_message_type_strings = selection.message_type_strings();
        let rows = if let Some(sequence) = after_sequence {
            sqlx::query_as!(
                ChatEventRow,
                r#"
                SELECT e.sequence AS "sequence!",
                       e.event_id AS "event_id!",
                       e.room_id AS "room_id?",
                       e.actor_user_id AS "actor_user_id?",
                       e.payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                       e.occurred_at AS "occurred_at!"
                FROM chat_message_events e
                JOIN chat_messages m
                  ON m.room_id = e.room_id
                 AND m.id = e.message_id
                 AND m.created_at = e.message_created_at
                 AND m.deletion_source IS DISTINCT FROM 'account'
                WHERE e.room_id = $1
                  AND e.sequence > $2
                  AND e.event_type = ANY($3::text[])
                  AND (e.payload #>> '{message,message,messageType}') = ANY($5::text[])
                ORDER BY sequence ASC
                LIMIT $4
                "#,
                room_id.as_i64(),
                sequence,
                &event_types,
                i64::from(limit),
                &included_message_type_strings,
            )
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query_as!(
                ChatEventRow,
                r#"
                SELECT e.sequence AS "sequence!",
                       e.event_id AS "event_id!",
                       e.room_id AS "room_id?",
                       e.actor_user_id AS "actor_user_id?",
                       e.payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                       e.occurred_at AS "occurred_at!"
                FROM chat_message_events e
                JOIN chat_messages m
                  ON m.room_id = e.room_id
                 AND m.id = e.message_id
                 AND m.created_at = e.message_created_at
                 AND m.deletion_source IS DISTINCT FROM 'account'
                WHERE e.room_id = $1
                  AND e.event_type = ANY($2::text[])
                  AND (e.payload #>> '{message,message,messageType}') = ANY($4::text[])
                ORDER BY sequence ASC
                LIMIT $3
                "#,
                room_id.as_i64(),
                &event_types,
                i64::from(limit),
                &included_message_type_strings,
            )
            .fetch_all(self.pool())
            .await?
        };

        rows.into_iter().map(ChatEventRow::try_into_log).collect()
    }

    pub async fn list_events_after_sequence(
        &self,
        room_id: &RoomId,
        after_sequence: i64,
        limit: i32,
    ) -> Result<Vec<ChatMessageEventLog>> {
        self.list_events_after_sequence_with_selection(
            room_id,
            after_sequence,
            limit,
            &ChatMessageSelection::user_default(),
        )
        .await
    }

    pub async fn list_events_after_sequence_with_selection(
        &self,
        room_id: &RoomId,
        after_sequence: i64,
        limit: i32,
        selection: &ChatMessageSelection,
    ) -> Result<Vec<ChatMessageEventLog>> {
        let limit = limit.clamp(1, 500);
        let after_sequence = after_sequence.max(0);
        let event_types = chat_message_event_types();
        let included_message_type_strings = selection.message_type_strings();
        let rows = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT e.sequence AS "sequence!",
                   e.event_id AS "event_id!",
                   e.room_id AS "room_id?",
                   e.actor_user_id AS "actor_user_id?",
                   e.payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                   e.occurred_at AS "occurred_at!"
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.room_id = e.room_id
             AND m.id = e.message_id
             AND m.created_at = e.message_created_at
             AND m.deletion_source IS DISTINCT FROM 'account'
            WHERE e.room_id = $1
              AND e.sequence > $2
              AND e.event_type = ANY($3::text[])
              AND (e.payload #>> '{message,message,messageType}') = ANY($5::text[])
            ORDER BY sequence ASC
            LIMIT $4
            "#,
            room_id.as_i64(),
            after_sequence,
            &event_types,
            i64::from(limit),
            &included_message_type_strings,
        )
        .fetch_all(self.pool())
        .await?;

        rows.into_iter().map(ChatEventRow::try_into_log).collect()
    }

    pub async fn latest_event_cursor_for_room(&self, room_id: &RoomId) -> Result<EventCursor> {
        self.latest_event_cursor_for_room_with_selection(
            room_id,
            &ChatMessageSelection::user_default(),
        )
        .await
    }

    pub async fn latest_event_cursor_for_room_with_selection(
        &self,
        room_id: &RoomId,
        selection: &ChatMessageSelection,
    ) -> Result<EventCursor> {
        let event_types = chat_message_event_types();
        let included_message_type_strings = selection.message_type_strings();
        let row = sqlx::query_as!(
            EventCursor,
            r#"
            SELECT e.event_id AS "event_id?", e.sequence AS "sequence!"
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.room_id = e.room_id
             AND m.id = e.message_id
             AND m.created_at = e.message_created_at
             AND m.deletion_source IS DISTINCT FROM 'account'
            WHERE e.room_id = $1
              AND e.event_type = ANY($2::text[])
              AND (e.payload #>> '{message,message,messageType}') = ANY($3::text[])
            ORDER BY e.sequence DESC
            LIMIT 1
            "#,
            room_id.as_i64(),
            &event_types,
            &included_message_type_strings,
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(row.unwrap_or(EventCursor {
            event_id: None,
            sequence: 0,
        }))
    }

    pub async fn retained_chat_event_sequence_bounds(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<(i64, i64)>> {
        let event_types = chat_message_event_types();
        let row = sqlx::query!(
            r"
            SELECT MIN(sequence) AS min_sequence, MAX(sequence) AS max_sequence
            FROM chat_message_events
            WHERE room_id = $1
              AND event_type = ANY($2::text[])
            ",
            room_id.as_i64(),
            &event_types
        )
        .fetch_one(self.pool())
        .await?;

        Ok(row.min_sequence.zip(row.max_sequence))
    }

    pub async fn latest_event_for_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageEventLog>> {
        let event_types = chat_message_event_types();
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT e.sequence AS "sequence!",
                   e.event_id AS "event_id!",
                   e.room_id AS "room_id?",
                   e.actor_user_id AS "actor_user_id?",
                   e.payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                   e.occurred_at AS "occurred_at!"
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.room_id = e.room_id
             AND m.id = e.message_id
             AND m.created_at = e.message_created_at
             AND m.deletion_source IS DISTINCT FROM 'account'
            WHERE e.room_id = $1
              AND e.event_type = ANY($2::text[])
              AND e.message_id = $3
              AND e.message_created_at = $4
            ORDER BY sequence DESC
            LIMIT 1
            "#,
            room_id.as_i64(),
            &event_types,
            message_id,
            message_created_at
        )
        .fetch_optional(self.pool())
        .await?;

        row.map(ChatEventRow::try_into_log).transpose()
    }

    pub async fn created_event_for_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT e.sequence AS "sequence!",
                   e.event_id AS "event_id!",
                   e.room_id AS "room_id?",
                   e.actor_user_id AS "actor_user_id?",
                   e.payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                   e.occurred_at AS "occurred_at!"
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.room_id = e.room_id
             AND m.id = e.message_id
             AND m.created_at = e.message_created_at
             AND m.deletion_source IS DISTINCT FROM 'account'
            WHERE e.event_type = $1
              AND e.room_id = $2
              AND e.message_id = $3
              AND e.message_created_at = $4
            ORDER BY sequence ASC
            LIMIT 1
            "#,
            CHAT_MESSAGE_CREATED_EVENT_TYPE,
            room_id.as_i64(),
            message_id,
            message_created_at
        )
        .fetch_optional(self.pool())
        .await?;

        row.map(ChatEventRow::try_into_log).transpose()
    }

    pub async fn get_read_state(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<ChatReadState>> {
        let state = sqlx::query_as!(
            ChatReadState,
            r#"
            SELECT room_id AS "room_id!: RoomId",
                   user_id AS "user_id!: UserId",
                   last_read_message_id,
                   last_read_message_created_at,
                   last_read_event_id,
                   last_read_event_sequence,
                   updated_at AS "updated_at!"
            FROM chat_read_states
            WHERE room_id = $1 AND user_id = $2
            "#,
            room_id.as_i64(),
            user_id.as_i64()
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(state)
    }

    pub async fn upsert_read_state(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
        event_id: Option<&str>,
        event_sequence: Option<i64>,
    ) -> Result<ChatReadState> {
        let state = sqlx::query_as!(
            ChatReadState,
            r#"
            INSERT INTO chat_read_states (
                room_id, user_id, last_read_message_id, last_read_message_created_at,
                last_read_event_id, last_read_event_sequence, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, NOW())
            ON CONFLICT (room_id, user_id) DO UPDATE
            SET last_read_message_id = EXCLUDED.last_read_message_id,
                last_read_message_created_at = EXCLUDED.last_read_message_created_at,
                last_read_event_id = EXCLUDED.last_read_event_id,
                last_read_event_sequence = EXCLUDED.last_read_event_sequence,
                updated_at = NOW()
            WHERE chat_read_states.last_read_message_created_at IS NULL
               OR (chat_read_states.last_read_message_created_at, chat_read_states.last_read_message_id)
                  < (EXCLUDED.last_read_message_created_at, EXCLUDED.last_read_message_id)
               OR (
                    EXCLUDED.last_read_event_sequence IS NOT NULL
                    AND (
                        chat_read_states.last_read_event_sequence IS NULL
                        OR chat_read_states.last_read_event_sequence < EXCLUDED.last_read_event_sequence
                    )
               )
            RETURNING room_id AS "room_id!: RoomId",
                      user_id AS "user_id!: UserId",
                      last_read_message_id,
                      last_read_message_created_at,
                      last_read_event_id,
                      last_read_event_sequence,
                      updated_at AS "updated_at!"
            "#,
            room_id.as_i64(),
            user_id.as_i64(),
            message_id,
            message_created_at,
            event_id,
            event_sequence
        )
        .fetch_optional(self.pool())
        .await?;

        if let Some(state) = state {
            return Ok(state);
        }
        self.get_read_state(room_id, user_id)
            .await?
            .ok_or_else(|| Error::Internal("chat read state upsert returned no row".to_string()))
    }

    pub async fn unread_count_after_read_state(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        state: Option<&ChatReadState>,
    ) -> Result<i64> {
        let pool = self.eventually_consistent_pool();
        let count = if let Some(state) = state {
            if let (Some(message_id), Some(created_at)) = (
                state.last_read_message_id,
                state.last_read_message_created_at,
            ) {
                sqlx::query_scalar!(
                    r#"
                    SELECT COUNT(*) AS "count!"
                    FROM chat_messages
                    WHERE room_id = $1
                      AND deletion_source IS DISTINCT FROM 'account'
                      AND status <> $2
                      AND (user_id IS NULL OR user_id <> $3)
                      AND (created_at, id) > ($4, $5)
                    "#,
                    room_id.as_i64(),
                    i16::from(ChatMessageStatus::Deleted),
                    user_id.as_i64(),
                    created_at,
                    message_id
                )
                .fetch_one(pool)
                .await?
            } else if let Some(sequence) = state.last_read_event_sequence {
                self.count_unread_after_event_sequence(room_id, user_id, sequence)
                    .await?
            } else {
                self.count_unread_without_state(room_id, user_id).await?
            }
        } else {
            self.count_unread_without_state(room_id, user_id).await?
        };

        Ok(count)
    }

    pub async fn message_read_receipts(
        &self,
        room_id: &RoomId,
        message: &ChatMessage,
        event: Option<&ChatMessageEventLog>,
        page: i32,
        page_size: i32,
    ) -> Result<ChatMessageReadReceiptsPage> {
        let page = page.max(1);
        let limit = page_size.clamp(1, 100);
        let offset = i64::from(page - 1) * i64::from(limit);
        let sender_user_id = message.user_id.map(|id| id.as_i64());
        let event_sequence = event.map(|event| event.sequence);
        let pool = self.eventually_consistent_pool();

        let reader_total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM room_members rm
            JOIN users u ON u.id = rm.user_id AND u.deleted_at IS NULL
            JOIN chat_read_states crs
              ON crs.room_id = rm.room_id
             AND crs.user_id = rm.user_id
            WHERE rm.room_id = $1
              AND ($2::BIGINT IS NULL OR rm.user_id <> $2)
              AND (
                    (crs.last_read_event_sequence IS NOT NULL
                     AND $3::BIGINT IS NOT NULL
                     AND crs.last_read_event_sequence >= $3)
                 OR (crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         > ($4, $5))
                 OR ($3::BIGINT IS NULL
                     AND crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         = ($4, $5))
              )
            "#,
            room_id.as_i64(),
            sender_user_id,
            event_sequence,
            message.created_at,
            message.id
        )
        .fetch_one(pool)
        .await?;

        let unread_total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM room_members rm
            JOIN users u ON u.id = rm.user_id AND u.deleted_at IS NULL
            LEFT JOIN chat_read_states crs
              ON crs.room_id = rm.room_id
             AND crs.user_id = rm.user_id
            WHERE rm.room_id = $1
              AND ($2::BIGINT IS NULL OR rm.user_id <> $2)
              AND NOT COALESCE((
                    (crs.last_read_event_sequence IS NOT NULL
                     AND $3::BIGINT IS NOT NULL
                     AND crs.last_read_event_sequence >= $3)
                 OR (crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         > ($4, $5))
                 OR ($3::BIGINT IS NULL
                     AND crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         = ($4, $5))
              ), FALSE)
            "#,
            room_id.as_i64(),
            sender_user_id,
            event_sequence,
            message.created_at,
            message.id
        )
        .fetch_one(pool)
        .await?;

        let reader_rows = sqlx::query!(
            r#"
            SELECT u.id AS "id!: UserId",
                   u.username AS "username!",
                   u.role AS "role!: crate::models::UserRole",
                   u.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: crate::models::UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS "banned_at?",
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   u.signup_method AS "signup_method!: crate::models::SignupMethod",
                   u.created_at AS "created_at!",
                   u.updated_at AS "updated_at!",
                   u.version AS "version!",
                   u.deleted_at,
                   crs.updated_at AS "read_at!"
            FROM room_members rm
            JOIN users u ON u.id = rm.user_id AND u.deleted_at IS NULL
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = u.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            JOIN chat_read_states crs
              ON crs.room_id = rm.room_id
             AND crs.user_id = rm.user_id
            WHERE rm.room_id = $1
              AND ($2::BIGINT IS NULL OR rm.user_id <> $2)
              AND (
                    (crs.last_read_event_sequence IS NOT NULL
                     AND $3::BIGINT IS NOT NULL
                     AND crs.last_read_event_sequence >= $3)
                 OR (crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         > ($4, $5))
                 OR ($3::BIGINT IS NULL
                     AND crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         = ($4, $5))
              )
            ORDER BY crs.updated_at ASC, u.username ASC, u.id ASC
            LIMIT $6 OFFSET $7
            "#,
            room_id.as_i64(),
            sender_user_id,
            event_sequence,
            message.created_at,
            message.id,
            i64::from(limit),
            offset
        )
        .fetch_all(pool)
        .await?;
        let readers = reader_rows
            .into_iter()
            .map(|row| ChatMessageReadReceiptUser {
                user: User {
                    id: row.id,
                    username: row.username,
                    role: row.role,
                    avatar_file_reference_id: row.avatar_file_reference_id,
                    status: row.status,
                    is_banned: row.is_banned,
                    banned_at: row.banned_at,
                    banned_by: row.banned_by,
                    banned_reason: row.banned_reason,
                    signup_method: row.signup_method,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                    version: row.version,
                    deleted_at: row.deleted_at,
                },
                read_at: row.read_at,
            })
            .collect();

        let unread_rows = sqlx::query!(
            r#"
            SELECT u.id AS "id!: UserId",
                   u.username AS "username!",
                   u.role AS "role!: crate::models::UserRole",
                   u.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: crate::models::UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS "banned_at?",
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   u.signup_method AS "signup_method!: crate::models::SignupMethod",
                   u.created_at AS "created_at!",
                   u.updated_at AS "updated_at!",
                   u.version AS "version!",
                   u.deleted_at
            FROM room_members rm
            JOIN users u ON u.id = rm.user_id AND u.deleted_at IS NULL
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = u.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            LEFT JOIN chat_read_states crs
              ON crs.room_id = rm.room_id
             AND crs.user_id = rm.user_id
            WHERE rm.room_id = $1
              AND ($2::BIGINT IS NULL OR rm.user_id <> $2)
              AND NOT COALESCE((
                    (crs.last_read_event_sequence IS NOT NULL
                     AND $3::BIGINT IS NOT NULL
                     AND crs.last_read_event_sequence >= $3)
                 OR (crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         > ($4, $5))
                 OR ($3::BIGINT IS NULL
                     AND crs.last_read_message_created_at IS NOT NULL
                     AND crs.last_read_message_id IS NOT NULL
                     AND (crs.last_read_message_created_at, crs.last_read_message_id)
                         = ($4, $5))
              ), FALSE)
            ORDER BY u.username ASC, u.id ASC
            LIMIT $6 OFFSET $7
            "#,
            room_id.as_i64(),
            sender_user_id,
            event_sequence,
            message.created_at,
            message.id,
            i64::from(limit),
            offset
        )
        .fetch_all(pool)
        .await?;
        let unread_members = unread_rows
            .into_iter()
            .map(|row| ChatMessageReadReceiptMember {
                user: User {
                    id: row.id,
                    username: row.username,
                    role: row.role,
                    avatar_file_reference_id: row.avatar_file_reference_id,
                    status: row.status,
                    is_banned: row.is_banned,
                    banned_at: row.banned_at,
                    banned_by: row.banned_by,
                    banned_reason: row.banned_reason,
                    signup_method: row.signup_method,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                    version: row.version,
                    deleted_at: row.deleted_at,
                },
            })
            .collect();

        Ok(ChatMessageReadReceiptsPage {
            readers,
            unread_members,
            reader_total,
            unread_total,
        })
    }

    async fn count_unread_after_event_sequence(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        sequence: i64,
    ) -> Result<i64> {
        let pool = self.eventually_consistent_pool();
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.room_id = e.room_id
             AND m.id = e.message_id
             AND m.created_at = e.message_created_at
            WHERE e.room_id = $1
              AND e.sequence > $2
              AND e.event_type = $3
              AND m.status <> $4
              AND m.deletion_source IS DISTINCT FROM 'account'
              AND (m.user_id IS NULL OR m.user_id <> $5)
            "#,
            room_id.as_i64(),
            sequence,
            CHAT_MESSAGE_CREATED_EVENT_TYPE,
            i16::from(ChatMessageStatus::Deleted),
            user_id.as_i64()
        )
        .fetch_one(pool)
        .await?;

        Ok(count)
    }

    async fn count_unread_without_state(&self, room_id: &RoomId, user_id: &UserId) -> Result<i64> {
        let pool = self.eventually_consistent_pool();
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_messages
            WHERE room_id = $1
              AND deletion_source IS DISTINCT FROM 'account'
              AND status <> $2
              AND (user_id IS NULL OR user_id <> $3)
              AND created_at >= NOW() - INTERVAL '90 days'
            "#,
            room_id.as_i64(),
            i16::from(ChatMessageStatus::Deleted),
            user_id.as_i64()
        )
        .fetch_one(pool)
        .await?;

        Ok(count)
    }

    pub async fn list_by_room_cursor(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
    ) -> Result<(Vec<ChatMessageWithAttachments>, Option<ChatHistoryCursor>)> {
        self.list_by_room_cursor_for_viewer(room_id, cursor, limit, include_deleted, None)
            .await
    }

    pub async fn list_by_room_cursor_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<(Vec<ChatMessageWithAttachments>, Option<ChatHistoryCursor>)> {
        self.list_by_room_cursor_for_viewer_with_selection(
            room_id,
            cursor,
            limit,
            include_deleted,
            viewer_user_id,
            &ChatMessageSelection::user_default(),
        )
        .await
    }

    pub async fn list_by_room_cursor_for_viewer_with_selection(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
        selection: &ChatMessageSelection,
    ) -> Result<(Vec<ChatMessageWithAttachments>, Option<ChatHistoryCursor>)> {
        self.list_by_room_cursor_for_viewer_from_pool(
            self.eventually_consistent_pool(),
            ChatHistoryCursorRequest {
                room_id,
                cursor,
                limit,
                include_deleted,
                viewer_user_id,
                selection,
            },
        )
        .await
    }

    async fn list_by_room_cursor_for_viewer_from_pool(
        &self,
        pool: &PgPool,
        request: ChatHistoryCursorRequest<'_>,
    ) -> Result<(Vec<ChatMessageWithAttachments>, Option<ChatHistoryCursor>)> {
        let limit = request.limit.clamp(1, 100);
        let included_message_type_codes = request.selection.message_type_codes();
        let messages = if let Some(cursor) = request.cursor {
            sqlx::query_as!(
                ChatMessageRow,
                r#"
                SELECT id AS "id!",
                       room_id AS "room_id!: RoomId",
                       user_id AS "user_id?: UserId",
                       client_message_id,
                       content AS "content!",
                       message_type AS "message_type!: ChatMessageType",
                       status AS "status!: ChatMessageStatus",
                       version AS "version!",
                       reply_to_message_id,
                       reply_to_message_created_at,
                       metadata AS "metadata?: ChatMetadata",
                       edited_at,
                       deleted_at,
                       deleted_by AS "deleted_by?: UserId",
                       delete_reason,
                       created_at AS "created_at!"
                FROM chat_messages
            WHERE room_id = $1
                  AND deletion_source IS DISTINCT FROM 'account'
                  AND ($2 OR status <> $3)
                  AND (created_at, id) < ($4, $5)
                  AND message_type = ANY($7::smallint[])
                ORDER BY created_at DESC, id DESC
                LIMIT $6
                "#,
                request.room_id.as_i64(),
                request.include_deleted,
                i16::from(ChatMessageStatus::Deleted),
                cursor.created_at,
                cursor.id,
                i64::from(limit),
                &included_message_type_codes,
            )
            .fetch_all(pool)
            .await?
        } else {
            sqlx::query_as!(
                ChatMessageRow,
                r#"
                SELECT id AS "id!",
                       room_id AS "room_id!: RoomId",
                       user_id AS "user_id?: UserId",
                       client_message_id,
                       content AS "content!",
                       message_type AS "message_type!: ChatMessageType",
                       status AS "status!: ChatMessageStatus",
                       version AS "version!",
                       reply_to_message_id,
                       reply_to_message_created_at,
                       metadata AS "metadata?: ChatMetadata",
                       edited_at,
                       deleted_at,
                       deleted_by AS "deleted_by?: UserId",
                       delete_reason,
                       created_at AS "created_at!"
                FROM chat_messages
            WHERE room_id = $1
                  AND deletion_source IS DISTINCT FROM 'account'
                  AND ($2 OR status <> $3)
                  AND created_at >= NOW() - INTERVAL '90 days'
                  AND message_type = ANY($5::smallint[])
                ORDER BY created_at DESC, id DESC
                LIMIT $4
                "#,
                request.room_id.as_i64(),
                request.include_deleted,
                i16::from(ChatMessageStatus::Deleted),
                i64::from(limit),
                &included_message_type_codes,
            )
            .fetch_all(pool)
            .await?
        };

        let messages = chat_messages_from_rows(messages)?;
        let next_cursor = if i32::try_from(messages.len()).ok() == Some(limit) {
            messages.last().map(|m| ChatHistoryCursor {
                created_at: m.created_at,
                id: m.id,
            })
        } else {
            None
        };

        let messages = self
            .attach_attachments_and_reactions_to_messages_from_pool(
                pool,
                messages,
                request.viewer_user_id,
            )
            .await?;

        Ok((messages, next_cursor))
    }

    pub async fn list_history_page_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatHistoryPage> {
        self.list_history_page_for_viewer_with_selection(
            room_id,
            cursor,
            limit,
            include_deleted,
            viewer_user_id,
            &ChatMessageSelection::user_default(),
        )
        .await
    }

    pub async fn list_history_page_for_viewer_with_selection(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
        selection: &ChatMessageSelection,
    ) -> Result<ChatHistoryPage> {
        let event_cursor = self
            .latest_event_cursor_for_room_with_selection(room_id, selection)
            .await?;
        let (messages, next_cursor) = self
            .list_by_room_cursor_for_viewer_from_pool(
                self.pool(),
                ChatHistoryCursorRequest {
                    room_id,
                    cursor,
                    limit,
                    include_deleted,
                    viewer_user_id,
                    selection,
                },
            )
            .await?;

        Ok(ChatHistoryPage {
            messages,
            next_cursor,
            event_cursor,
        })
    }

    pub async fn search_messages_for_viewer(
        &self,
        query: &ChatSearchMessagesQuery,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatSearchMessagesPage> {
        let event_cursor = self.latest_event_cursor_for_room(&query.room_id).await?;
        let pattern = crate::repository::query_builder::ilike_contains_pattern(&query.query)
            .ok_or_else(|| Error::InvalidInput("chat search query is required".to_string()))?;
        let limit = query.limit.clamp(1, 100);
        let fetch_limit = i64::from(limit) + 1;
        let user_id = query.user_id.map(|id| id.as_i64());
        let pool = self.pool();
        let messages = if let Some(cursor) = query.cursor {
            sqlx::query_as!(
                ChatMessageRow,
                r#"
                WITH search_terms AS (
                    SELECT websearch_to_tsquery('simple', $2) AS tsquery
                )
                SELECT m.id AS "id!",
                       m.room_id AS "room_id!: RoomId",
                       m.user_id AS "user_id?: UserId",
                       m.client_message_id,
                       m.content AS "content!",
                       m.message_type AS "message_type!: ChatMessageType",
                       m.status AS "status!: ChatMessageStatus",
                       m.version AS "version!",
                       m.reply_to_message_id,
                       m.reply_to_message_created_at,
                       m.metadata AS "metadata?: ChatMetadata",
                       m.edited_at,
                       m.deleted_at,
                       m.deleted_by AS "deleted_by?: UserId",
                       m.delete_reason,
                       m.created_at AS "created_at!"
                FROM chat_messages m
                CROSS JOIN search_terms st
                WHERE m.room_id = $1
                  AND m.deletion_source IS DISTINCT FROM 'account'
                  AND (m.content_search @@ st.tsquery OR m.content ILIKE $3 ESCAPE '\')
                  AND ($4 OR m.status <> $5)
                  AND ($6::bigint IS NULL OR m.user_id = $6)
                  AND (m.created_at, m.id) < ($7, $8)
                ORDER BY m.created_at DESC, m.id DESC
                LIMIT $9
                "#,
                query.room_id.as_i64(),
                &query.query,
                &pattern,
                query.include_deleted,
                i16::from(ChatMessageStatus::Deleted),
                user_id,
                cursor.created_at,
                cursor.id,
                fetch_limit
            )
            .fetch_all(pool)
            .await?
        } else {
            sqlx::query_as!(
                ChatMessageRow,
                r#"
                WITH search_terms AS (
                    SELECT websearch_to_tsquery('simple', $2) AS tsquery
                )
                SELECT m.id AS "id!",
                       m.room_id AS "room_id!: RoomId",
                       m.user_id AS "user_id?: UserId",
                       m.client_message_id,
                       m.content AS "content!",
                       m.message_type AS "message_type!: ChatMessageType",
                       m.status AS "status!: ChatMessageStatus",
                       m.version AS "version!",
                       m.reply_to_message_id,
                       m.reply_to_message_created_at,
                       m.metadata AS "metadata?: ChatMetadata",
                       m.edited_at,
                       m.deleted_at,
                       m.deleted_by AS "deleted_by?: UserId",
                       m.delete_reason,
                       m.created_at AS "created_at!"
                FROM chat_messages m
                CROSS JOIN search_terms st
                WHERE m.room_id = $1
                  AND m.deletion_source IS DISTINCT FROM 'account'
                  AND (m.content_search @@ st.tsquery OR m.content ILIKE $3 ESCAPE '\')
                  AND ($4 OR m.status <> $5)
                  AND ($6::bigint IS NULL OR m.user_id = $6)
                ORDER BY m.created_at DESC, m.id DESC
                LIMIT $7
                "#,
                query.room_id.as_i64(),
                &query.query,
                &pattern,
                query.include_deleted,
                i16::from(ChatMessageStatus::Deleted),
                user_id,
                fetch_limit
            )
            .fetch_all(pool)
            .await?
        };

        let mut messages = chat_messages_from_rows(messages)?;
        let page_size = usize::try_from(limit)
            .map_err(|_| Error::Internal("chat search limit overflowed usize".to_string()))?;
        let has_next = messages.len() > page_size;
        if has_next {
            messages.truncate(page_size);
        }
        let next_cursor =
            has_next
                .then(|| messages.last())
                .flatten()
                .map(|message| ChatHistoryCursor {
                    created_at: message.created_at,
                    id: message.id,
                });
        let messages = self
            .attach_attachments_and_reactions_to_messages_from_pool(pool, messages, viewer_user_id)
            .await?;

        Ok(ChatSearchMessagesPage {
            messages,
            next_cursor,
            event_cursor,
        })
    }

    pub async fn list_playback_messages(
        &self,
        query: &ChatPlaybackMessagesQuery,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        self.list_playback_messages_for_viewer(query, None).await
    }

    pub async fn list_playback_messages_for_viewer(
        &self,
        query: &ChatPlaybackMessagesQuery,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        let limit = query.limit.clamp(1, 500);
        let start_seconds = (query.position_seconds - query.before_seconds).max(0.0);
        let end_seconds = query.position_seconds + query.after_seconds;
        let media_id = query.media_id.map(|id| id.as_i64().to_string());
        let playlist_id = query.playlist_id.map(|id| id.as_i64().to_string());
        let target_hash = query
            .target
            .as_ref()
            .map(|target| crate::models::try_hash_playback_target(Some(target)))
            .transpose()?;
        let included_message_type_codes = query.selection.message_type_codes();
        let pool = self.eventually_consistent_pool();
        let rows = sqlx::query_as!(
            ChatMessageRow,
            r#"
            WITH candidates AS (
                SELECT id,
                       room_id,
                       user_id,
                       client_message_id,
                       content,
                       message_type,
                       status,
                       version,
                       reply_to_message_id,
                       reply_to_message_created_at,
                       metadata,
                       edited_at,
                       deleted_at,
                       deleted_by,
                       delete_reason,
                       created_at,
                       CASE
                           WHEN jsonb_typeof(metadata #> '{playback,positionSeconds}') = 'number'
                           THEN (metadata #>> '{playback,positionSeconds}')::double precision
                           ELSE NULL
                       END AS playback_position
                FROM chat_messages
                WHERE room_id = $1
                  AND deletion_source IS DISTINCT FROM 'account'
                  AND ($2 OR status <> $3)
                  AND ($4::text IS NULL OR metadata #>> '{playback,mediaId}' = $4)
                  AND ($5::text IS NULL OR metadata #>> '{playback,playlistId}' = $5)
                  AND ($6::text IS NULL OR metadata #>> '{playback,targetHash}' = $6)
                  AND message_type = ANY($7::smallint[])
            )
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM candidates
            WHERE playback_position BETWEEN $8 AND $9
            ORDER BY playback_position ASC, created_at ASC, id ASC
            LIMIT $10
            "#,
            query.room_id.as_i64(),
            query.include_deleted,
            i16::from(ChatMessageStatus::Deleted),
            media_id,
            playlist_id,
            target_hash.as_deref(),
            &included_message_type_codes,
            start_seconds,
            end_seconds,
            i64::from(limit),
        )
        .fetch_all(pool)
        .await?;
        let messages = chat_messages_from_rows(rows)?;

        self.attach_attachments_and_reactions_to_messages(messages, viewer_user_id)
            .await
    }

    pub async fn list_context_around_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        before_limit: i32,
        after_limit: i32,
        include_deleted: bool,
    ) -> Result<Option<ChatMessageContext>> {
        self.list_context_around_message_for_viewer(
            room_id,
            message_id,
            before_limit,
            after_limit,
            include_deleted,
            None,
        )
        .await
    }

    pub async fn list_context_around_message_for_viewer(
        &self,
        room_id: &RoomId,
        message_id: i64,
        before_limit: i32,
        after_limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Option<ChatMessageContext>> {
        let Some(anchor) = self.get_by_room_and_id(room_id, message_id).await? else {
            return Ok(None);
        };
        if anchor.status == ChatMessageStatus::Deleted && !include_deleted {
            return Ok(None);
        }

        let before_limit = before_limit.clamp(0, 50);
        let after_limit = after_limit.clamp(0, 50);
        let pool = self.eventually_consistent_pool();
        let mut before = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
              AND deletion_source IS DISTINCT FROM 'account'
              AND ($2 OR status <> $3)
              AND (created_at, id) < ($4, $5)
            ORDER BY created_at DESC, id DESC
            LIMIT $6
            "#,
            room_id.as_i64(),
            include_deleted,
            i16::from(ChatMessageStatus::Deleted),
            anchor.created_at,
            anchor.id,
            i64::from(before_limit)
        )
        .fetch_all(pool)
        .await?;
        before.reverse();

        let after = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
              AND deletion_source IS DISTINCT FROM 'account'
              AND ($2 OR status <> $3)
              AND (created_at, id) > ($4, $5)
            ORDER BY created_at ASC, id ASC
            LIMIT $6
            "#,
            room_id.as_i64(),
            include_deleted,
            i16::from(ChatMessageStatus::Deleted),
            anchor.created_at,
            anchor.id,
            i64::from(after_limit)
        )
        .fetch_all(pool)
        .await?;
        let before = chat_messages_from_rows(before)?;
        let after = chat_messages_from_rows(after)?;

        let anchor = self
            .attach_attachments_and_reactions_to_messages(vec![anchor], viewer_user_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Internal("Chat context anchor disappeared".to_string()))?;
        Ok(Some(ChatMessageContext {
            before: self
                .attach_attachments_and_reactions_to_messages(before, viewer_user_id)
                .await?,
            anchor,
            after: self
                .attach_attachments_and_reactions_to_messages(after, viewer_user_id)
                .await?,
        }))
    }

    pub async fn get_by_id(&self, message_id: i64) -> Result<Option<ChatMessage>> {
        let pool = self.eventually_consistent_pool();
        let msg = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE id = $1
              AND deletion_source IS DISTINCT FROM 'account'
            "#,
            message_id
        )
        .fetch_optional(pool)
        .await?;

        optional_chat_message_from_row(msg)
    }

    pub async fn get_by_room_and_id(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        self.get_by_room_and_id_from_pool(self.eventually_consistent_pool(), room_id, message_id)
            .await
    }

    pub async fn get_by_room_and_id_from_primary(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        self.get_by_room_and_id_from_pool(self.pool(), room_id, message_id)
            .await
    }

    async fn get_by_room_and_id_from_pool(
        &self,
        pool: &PgPool,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        let msg = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1 AND id = $2
              AND deletion_source IS DISTINCT FROM 'account'
            "#,
            room_id.as_i64(),
            message_id
        )
        .fetch_optional(pool)
        .await?;

        optional_chat_message_from_row(msg)
    }

    pub async fn get_with_attachments_by_room_and_id(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        self.get_with_attachments_by_room_and_id_for_viewer(room_id, message_id, None)
            .await
    }

    pub async fn get_with_attachments_by_room_and_id_for_viewer(
        &self,
        room_id: &RoomId,
        message_id: i64,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        self.get_with_attachments_by_room_and_id_from_pool(
            self.eventually_consistent_pool(),
            room_id,
            message_id,
            viewer_user_id,
        )
        .await
    }

    pub async fn get_with_attachments_by_room_and_id_from_primary(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        self.get_with_attachments_by_room_and_id_from_pool(self.pool(), room_id, message_id, None)
            .await
    }

    async fn get_with_attachments_by_room_and_id_from_pool(
        &self,
        pool: &PgPool,
        room_id: &RoomId,
        message_id: i64,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        let Some(message) = self
            .get_by_room_and_id_from_pool(pool, room_id, message_id)
            .await?
        else {
            return Ok(None);
        };
        let attachments = if message.status == ChatMessageStatus::Deleted {
            Vec::new()
        } else {
            self.attachments_for_message_from_pool(pool, message.id, message.created_at)
                .await?
        };
        let reactions = self
            .reaction_summaries_for_messages_with_executor(
                pool,
                std::slice::from_ref(&message),
                viewer_user_id,
            )
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        let mentions = self
            .mentions_for_messages_from_pool(pool, std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        let pin = self
            .pins_for_messages_from_pool(pool, std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message));
        Ok(Some(ChatMessageWithAttachments {
            message,
            attachments,
            reactions,
            mentions,
            pin,
        }))
    }

    pub async fn edit(
        &self,
        room_id: &RoomId,
        message_id: i64,
        content: &str,
        metadata: &Option<ChatMetadata>,
        expected_version: Option<i64>,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        let metadata = ChatMetadata::normalized_for_optional_storage(metadata.as_ref())?;
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = ",
        );
        builder.push_bind(content);
        builder.push(", metadata = ");
        builder.push_bind(&metadata);
        builder.push(", status = ");
        builder.push_bind(i16::from(ChatMessageStatus::Edited));
        builder.push(
            ", version = version + 1, edited_at = NOW()
            WHERE room_id = ",
        );
        builder.push_bind(room_id.as_i64());
        builder.push(" AND id = ");
        builder.push_bind(message_id);
        builder.push(" AND status <> ");
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        if let Some(version) = expected_version {
            builder.push(" AND version = ");
            builder.push_bind(version);
        }
        builder.push(
            r"
            RETURNING id, room_id, user_id, client_message_id, content, message_type,
                      status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                      deleted_at, deleted_by, delete_reason, created_at
            ",
        );

        let message = builder
            .build_query_as::<ChatMessage>()
            .fetch_optional(self.pool())
            .await?;
        let Some(message) = message else {
            return Ok(None);
        };
        let attachments = self
            .attachments_for_message(message.id, message.created_at)
            .await?;
        let mentions = self
            .mentions_for_messages(std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        Ok(Some(ChatMessageWithAttachments {
            message,
            attachments,
            reactions: Vec::new(),
            mentions,
            pin: None,
        }))
    }

    pub async fn edit_with_event(
        &self,
        request: EditChatMessageEventRequest<'_>,
    ) -> Result<Option<IdempotentChatEventInsert>> {
        let mut tx = self.pool().begin().await?;
        if let Some(operation) = request.operation {
            if let Some(event) = self
                .begin_message_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.actor_user_id,
                    operation,
                )
                .await?
            {
                tx.commit().await?;
                return Ok(Some(IdempotentChatEventInsert {
                    event,
                    inserted: false,
                    pin_event: None,
                }));
            }
        }
        let metadata = ChatMetadata::normalized_for_optional_storage(request.metadata.as_ref())?;
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = ",
        );
        builder.push_bind(request.content);
        builder.push(", metadata = ");
        builder.push_bind(&metadata);
        builder.push(", status = ");
        builder.push_bind(i16::from(ChatMessageStatus::Edited));
        builder.push(
            ", version = version + 1, edited_at = NOW()
            WHERE room_id = ",
        );
        builder.push_bind(request.room_id.as_i64());
        builder.push(" AND id = ");
        builder.push_bind(request.message_id);
        builder.push(" AND created_at = ");
        builder.push_bind(request.message_created_at);
        builder.push(" AND status <> ");
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        if let Some(version) = request.expected_version {
            builder.push(" AND version = ");
            builder.push_bind(version);
        }
        builder.push(
            r"
            RETURNING id, room_id, user_id, client_message_id, content, message_type,
                      status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                      deleted_at, deleted_by, delete_reason, created_at
            ",
        );

        let message = builder
            .build_query_as::<ChatMessage>()
            .fetch_optional(&mut *tx)
            .await?;
        let Some(message) = message else {
            if let Some(operation) = request.operation {
                self.clear_incomplete_message_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.actor_user_id,
                    operation,
                )
                .await?;
            }
            tx.commit().await?;
            return Ok(None);
        };
        let message_payload = self
            .message_event_payload_in_tx(
                &mut tx,
                request.room_id,
                &message,
                Some(request.actor_user_id),
            )
            .await?;
        let pin = message_payload.pin.clone();
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: *request.room_id,
            actor_user_id: *request.actor_user_id,
            kind: ChatEventKind::Edited,
            message: message_payload,
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;
        let pin_event = if pin.is_some() {
            Some(
                self.insert_pin_event_in_tx(
                    &mut tx,
                    &ChatPinEvent {
                        event_id: synctv_common::snanoid!(16),
                        sequence: 0,
                        room_id: *request.room_id,
                        actor_user_id: *request.actor_user_id,
                        kind: ChatPinEventKind::MessageUpdated,
                        message: logged.event.message.clone(),
                        pin,
                        occurred_at: request.occurred_at,
                    },
                )
                .await?,
            )
        } else {
            None
        };
        if let Some(operation) = request.operation {
            self.complete_message_operation_in_tx(
                &mut tx,
                request.room_id,
                request.actor_user_id,
                operation,
                &logged.event,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Some(IdempotentChatEventInsert {
            event: logged,
            inserted: true,
            pin_event,
        }))
    }

    pub async fn soft_delete(
        &self,
        room_id: &RoomId,
        message_id: i64,
        deleted_by: &UserId,
        reason: Option<&str>,
        expected_version: Option<i64>,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = '', status = ",
        );
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        builder.push(
            ", version = version + 1, deleted_at = NOW(), deletion_source = 'user', deleted_by = ",
        );
        builder.push_bind(deleted_by.as_i64());
        builder.push(", delete_reason = ");
        builder.push_bind(reason);
        builder.push(" WHERE room_id = ");
        builder.push_bind(room_id.as_i64());
        builder.push(" AND id = ");
        builder.push_bind(message_id);
        builder.push(" AND status <> ");
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        if let Some(version) = expected_version {
            builder.push(" AND version = ");
            builder.push_bind(version);
        }
        builder.push(
            r"
            RETURNING id, room_id, user_id, client_message_id, content, message_type,
                      status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                      deleted_at, deleted_by, delete_reason, created_at
            ",
        );

        let message = builder
            .build_query_as::<ChatMessage>()
            .fetch_optional(self.pool())
            .await?;
        let Some(message) = message else {
            return Ok(None);
        };
        self.delete_pin_for_message(message.room_id, message.id, message.created_at)
            .await?;
        let mentions = self
            .mentions_for_messages(std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        Ok(Some(ChatMessageWithAttachments {
            message,
            attachments: Vec::new(),
            reactions: Vec::new(),
            mentions,
            pin: None,
        }))
    }

    pub async fn soft_delete_with_event(
        &self,
        request: DeleteChatMessageEventRequest<'_>,
    ) -> Result<Option<IdempotentChatEventInsert>> {
        let mut tx = self.pool().begin().await?;
        if let Some(operation) = request.operation {
            if let Some(event) = self
                .begin_message_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.deleted_by,
                    operation,
                )
                .await?
            {
                tx.commit().await?;
                return Ok(Some(IdempotentChatEventInsert {
                    event,
                    inserted: false,
                    pin_event: None,
                }));
            }
        }
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = '', status = ",
        );
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        builder.push(
            ", version = version + 1, deleted_at = NOW(), deletion_source = 'user', deleted_by = ",
        );
        builder.push_bind(request.deleted_by.as_i64());
        builder.push(", delete_reason = ");
        builder.push_bind(request.reason);
        builder.push(" WHERE room_id = ");
        builder.push_bind(request.room_id.as_i64());
        builder.push(" AND id = ");
        builder.push_bind(request.message_id);
        builder.push(" AND created_at = ");
        builder.push_bind(request.message_created_at);
        builder.push(" AND status <> ");
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        if let Some(version) = request.expected_version {
            builder.push(" AND version = ");
            builder.push_bind(version);
        }
        builder.push(
            r"
            RETURNING id, room_id, user_id, client_message_id, content, message_type,
                      status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                      deleted_at, deleted_by, delete_reason, created_at
            ",
        );

        let message = builder
            .build_query_as::<ChatMessage>()
            .fetch_optional(&mut *tx)
            .await?;
        let Some(message) = message else {
            if let Some(operation) = request.operation {
                self.clear_incomplete_message_operation_in_tx(
                    &mut tx,
                    request.room_id,
                    request.deleted_by,
                    operation,
                )
                .await?;
            }
            tx.commit().await?;
            return Ok(None);
        };
        let pin = self
            .pin_for_message_in_tx(&mut tx, request.room_id, message.id, message.created_at)
            .await?;
        self.delete_pin_for_message_in_tx(&mut tx, &message).await?;
        let mentions = self
            .mentions_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: *request.room_id,
            actor_user_id: *request.deleted_by,
            kind: ChatEventKind::Deleted,
            message: ChatMessageWithAttachments {
                message,
                attachments: Vec::new(),
                reactions: Vec::new(),
                mentions,
                pin: None,
            },
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;
        let pin_event = if pin.is_some() {
            Some(
                self.insert_pin_event_in_tx(
                    &mut tx,
                    &ChatPinEvent {
                        event_id: synctv_common::snanoid!(16),
                        sequence: 0,
                        room_id: *request.room_id,
                        actor_user_id: *request.deleted_by,
                        kind: ChatPinEventKind::MessageDeleted,
                        message: logged.event.message.clone(),
                        pin: None,
                        occurred_at: request.occurred_at,
                    },
                )
                .await?,
            )
        } else {
            None
        };
        if let Some(operation) = request.operation {
            self.complete_message_operation_in_tx(
                &mut tx,
                request.room_id,
                request.deleted_by,
                operation,
                &logged.event,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Some(IdempotentChatEventInsert {
            event: logged,
            inserted: true,
            pin_event,
        }))
    }

    pub async fn delete(&self, message_id: i64, created_at: DateTime<Utc>) -> Result<bool> {
        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE id = $1 AND created_at = $2
            ",
            message_id,
            created_at
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_in_room(
        &self,
        room_id: &RoomId,
        message_id: i64,
        created_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE room_id = $1 AND id = $2 AND created_at = $3
            ",
            room_id.as_i64(),
            message_id,
            created_at
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_messages
            WHERE room_id = $1
              AND created_at >= NOW() - INTERVAL '90 days'
            "#,
            room_id.as_i64()
        )
        .fetch_one(self.pool())
        .await?;

        Ok(count)
    }

    pub async fn cleanup_old_messages(&self, room_id: &RoomId, keep_count: i32) -> Result<u64> {
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE room_id = $1
              AND created_at > NOW() - INTERVAL '90 days'
              AND (id, created_at) IN (
                SELECT id, created_at FROM (
                    SELECT id, created_at,
                           ROW_NUMBER() OVER (ORDER BY created_at DESC, id DESC) as rn
                    FROM chat_messages
                    WHERE room_id = $1
                      AND created_at > NOW() - INTERVAL '90 days'
                ) ranked
                WHERE rn > $2
            )
            ",
            room_id.as_i64(),
            i64::from(keep_count)
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn delete_messages_older_than_retention(&self) -> Result<u64> {
        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE created_at <= NOW() - INTERVAL '90 days'
            "
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn cleanup_all_rooms(
        &self,
        keep_count: i32,
        activity_window_minutes: i32,
    ) -> Result<u64> {
        if keep_count <= 0 {
            return Ok(0);
        }

        let result = sqlx::query!(
            r"
            DELETE FROM chat_messages
            WHERE created_at > NOW() - INTERVAL '90 days'
              AND (id, created_at) IN (
                SELECT id, created_at FROM (
                    SELECT id, created_at, room_id,
                           ROW_NUMBER() OVER (PARTITION BY room_id ORDER BY created_at DESC, id DESC) as rn
                    FROM chat_messages
                    WHERE room_id IN (
                        SELECT DISTINCT room_id
                        FROM chat_messages
                        WHERE created_at >= NOW() - make_interval(mins => $2)
                    )
                      AND created_at > NOW() - INTERVAL '90 days'
                ) ranked_messages
                WHERE rn > $1
            )
            ",
            i64::from(keep_count),
            activity_window_minutes
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }

    async fn insert_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
    ) -> Result<ChatMessage> {
        validate_message_for_insert(message)?;
        let metadata = ChatMetadata::normalized_for_optional_storage(message.metadata.as_ref())?;
        let inserted = sqlx::query_as!(
            ChatMessageRow,
            r#"
            INSERT INTO chat_messages (
                room_id, user_id, client_message_id, content, message_type,
                status, version, reply_to_message_id, reply_to_message_created_at,
                metadata, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id AS "id!",
                      room_id AS "room_id!: RoomId",
                      user_id AS "user_id?: UserId",
                      client_message_id,
                      content AS "content!",
                      message_type AS "message_type!: ChatMessageType",
                      status AS "status!: ChatMessageStatus",
                      version AS "version!",
                      reply_to_message_id,
                      reply_to_message_created_at,
                      metadata AS "metadata?: ChatMetadata",
                      edited_at,
                      deleted_at,
                      deleted_by AS "deleted_by?: UserId",
                      delete_reason,
                      created_at AS "created_at!"
            "#,
            message.room_id.as_i64(),
            message.user_id.map(|id| id.as_i64()),
            message.client_message_id.as_deref(),
            &message.content,
            i16::from(message.message_type),
            i16::from(message.status),
            message.version,
            message.reply_to_message_id,
            message.reply_to_message_created_at,
            &metadata as _,
            message.created_at
        )
        .fetch_one(&mut **tx)
        .await?;

        chat_message_from_row(inserted)
    }

    async fn insert_attachments_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
        attachments: &[NewStoredFile],
    ) -> Result<Vec<ChatAttachment>> {
        let mut inserted = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            validate_chat_attachment_for_insert(attachment)?;
            let kind = attachment
                .mime_type
                .as_deref()
                .map_or(ChatAttachmentKind::File, ChatAttachmentKind::from_mime_type);
            let row = sqlx::query_as!(
                ChatAttachment,
                r#"
                INSERT INTO chat_message_attachments (
                    id, kind, room_id, message_id, message_created_at, filename,
                    storage_backend, object_key, url, mime_type, size_bytes, width, height, metadata
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                RETURNING id AS "id!",
                          kind AS "kind!: ChatAttachmentKind",
                          room_id AS "room_id!: RoomId",
                          message_id AS "message_id!",
                          message_created_at AS "message_created_at!",
                          filename,
                          storage_backend AS "storage_backend!",
                          object_key AS "object_key!",
                          url,
                          mime_type,
                          size_bytes,
                          width,
                          height,
                          metadata AS "metadata!: crate::models::FileMetadata",
                          created_at AS "created_at!",
                          NULL::JSONB AS "object_access?: crate::models::FileObjectAccess",

                          NULL::TEXT AS "reuse_token?",
                          NULL::TIMESTAMPTZ AS "reuse_expires_at?"
                "#,
                &attachment.id,
                i16::from(kind),
                message.room_id.as_i64(),
                message.id,
                message.created_at,
                attachment.filename.as_deref(),
                &attachment.storage_backend,
                &attachment.object_key,
                attachment.url.as_deref(),
                attachment.mime_type.as_deref(),
                attachment.size_bytes,
                attachment.width,
                attachment.height,
                &attachment.metadata as _
            )
            .fetch_one(&mut **tx)
            .await?;
            self.insert_file_reference_for_attachment_in_tx(tx, &row)
                .await?;
            inserted.push(row);
        }
        Ok(inserted)
    }

    async fn insert_mentions_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
        mentions: &[ChatMentionInput],
    ) -> Result<Vec<ChatMention>> {
        let mut inserted = Vec::with_capacity(mentions.len());
        for mention in mentions {
            let row = sqlx::query_as!(
                ChatMention,
                r#"
                INSERT INTO chat_message_mentions (
                    room_id, message_id, message_created_at, mentioned_user_id,
                    start_char, length_chars
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT (room_id, message_id, message_created_at, start_char, mentioned_user_id)
                DO UPDATE SET created_at = chat_message_mentions.created_at
                RETURNING room_id AS "room_id!: RoomId",
                          message_id AS "message_id!",
                          message_created_at AS "message_created_at!",
                          mentioned_user_id AS "mentioned_user_id!: UserId",
                          NULL::TEXT AS "username?",
                          start_char AS "start!",
                          length_chars AS "length!",
                          created_at AS "created_at!"
                "#,
                message.room_id.as_i64(),
                message.id,
                message.created_at,
                mention.user_id.as_i64(),
                mention.start,
                mention.length
            )
            .fetch_one(&mut **tx)
            .await?;
            inserted.push(row);
        }
        Ok(inserted)
    }

    async fn get_idempotent_message_for_update(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        client_message_id: &str,
    ) -> Result<Option<IdempotencyRow>> {
        let row = sqlx::query!(
            r"
            SELECT request_hash, message_id, message_created_at, event_id
            FROM chat_message_idempotency
            WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
            FOR UPDATE
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            client_message_id
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(row.map(|row| IdempotencyRow {
            request_hash: row.request_hash,
            message_id: row.message_id,
            message_created_at: row.message_created_at,
            event_id: row.event_id,
        }))
    }

    async fn existing_idempotent_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: ExistingIdempotentEventRequest<'_>,
    ) -> Result<Option<ChatMessageEventLog>> {
        if request.existing.request_hash != request.request_hash {
            return Err(Error::Conflict(
                "client_message_id was already used with a different payload".to_string(),
            ));
        }

        let (message_id, created_at) = match (
            request.existing.message_id,
            request.existing.message_created_at,
        ) {
            (Some(message_id), Some(created_at)) => (message_id, created_at),
            (None, None) => return Ok(None),
            _ => {
                return Err(Error::Internal(
                    "idempotency record has an incomplete message cursor".to_string(),
                ));
            }
        };

        let loaded = self
            .get_with_attachments_in_tx(tx, &request.message.room_id, message_id, created_at)
            .await?
            .ok_or_else(|| {
                Error::Internal("idempotency record points to a missing chat message".to_string())
            })?;

        if let Some(existing_event_id) = request.existing.event_id {
            if let Some(event) = self
                .get_event_by_id_in_tx(tx, &request.message.room_id, &existing_event_id)
                .await?
            {
                return Ok(Some(event));
            }
            return Err(Error::Internal(
                "idempotency record points to a missing durable chat event".to_string(),
            ));
        }

        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: request.message.room_id,
            actor_user_id: request.message.user_id.ok_or_else(|| {
                Error::InvalidInput("Chat event requires a message sender".to_string())
            })?,
            kind: ChatEventKind::Created,
            message: loaded,
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_event_in_tx(tx, &event).await?;
        sqlx::query!(
            r"
            UPDATE chat_message_idempotency
            SET event_id = $4
            WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
            ",
            request.message.room_id.as_i64(),
            event.actor_user_id.as_i64(),
            request.client_message_id,
            &logged.event.event_id
        )
        .execute(&mut **tx)
        .await?;

        Ok(Some(logged))
    }

    async fn begin_message_operation_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        operation: &ChatMessageOperationIdempotency<'_>,
    ) -> Result<Option<ChatMessageEventLog>> {
        validate_required_text(
            operation.client_operation_id,
            "client_operation_id",
            CHAT_CLIENT_OPERATION_ID_MAX_CHARS,
        )?;
        let inserted = sqlx::query!(
            r"
            INSERT INTO chat_message_operation_idempotency (
                room_id, user_id, client_operation_id, operation_kind,
                request_hash, message_id, message_created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (room_id, user_id, client_operation_id) DO NOTHING
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            operation.client_operation_id,
            i16::from(operation.operation_kind),
            operation.request_hash,
            operation.message_id,
            operation.message_created_at
        )
        .execute(&mut **tx)
        .await?;
        if inserted.rows_affected() == 1 {
            return Ok(None);
        }

        self.replay_message_operation_event_in_tx(
            tx,
            room_id,
            user_id,
            operation.client_operation_id,
            operation.operation_kind,
            operation.request_hash,
        )
        .await
    }

    async fn begin_pin_operation_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        operation: &ChatMessageOperationIdempotency<'_>,
    ) -> Result<Option<ChatPinEventLog>> {
        validate_required_text(
            operation.client_operation_id,
            "client_operation_id",
            CHAT_CLIENT_OPERATION_ID_MAX_CHARS,
        )?;
        let inserted = sqlx::query!(
            r"
            INSERT INTO chat_message_operation_idempotency (
                room_id, user_id, client_operation_id, operation_kind,
                request_hash, message_id, message_created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (room_id, user_id, client_operation_id) DO NOTHING
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            operation.client_operation_id,
            i16::from(operation.operation_kind),
            operation.request_hash,
            operation.message_id,
            operation.message_created_at
        )
        .execute(&mut **tx)
        .await?;
        if inserted.rows_affected() == 1 {
            return Ok(None);
        }

        self.replay_pin_operation_event_in_tx(
            tx,
            room_id,
            user_id,
            operation.client_operation_id,
            operation.operation_kind,
            operation.request_hash,
        )
        .await
    }

    async fn replay_message_operation_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        client_operation_id: &str,
        operation_kind: ChatMessageOperationKind,
        request_hash: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query!(
            r#"
            SELECT
                operation_kind,
                request_hash,
                event_id AS "event_id?: String"
            FROM chat_message_operation_idempotency
            WHERE room_id = $1 AND user_id = $2 AND client_operation_id = $3
            FOR UPDATE
            "#,
            room_id.as_i64(),
            user_id.as_i64(),
            client_operation_id
        )
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        let existing_kind = row.operation_kind;
        let existing_hash = row.request_hash;
        if existing_kind != i16::from(operation_kind) || existing_hash != request_hash {
            return Err(Error::Conflict(
                "client_operation_id was already used with a different operation".to_string(),
            ));
        }
        let Some(event_id) = row.event_id else {
            return Ok(None);
        };
        if let Some(event) = self.get_event_by_id_in_tx(tx, room_id, &event_id).await? {
            return Ok(Some(event));
        }
        Err(Error::Internal(
            "operation idempotency record points to a missing durable chat event".to_string(),
        ))
    }

    async fn replay_pin_operation_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        client_operation_id: &str,
        operation_kind: ChatMessageOperationKind,
        request_hash: &str,
    ) -> Result<Option<ChatPinEventLog>> {
        let row = sqlx::query!(
            r#"
            SELECT
                operation_kind,
                request_hash,
                event_id AS "event_id?: String"
            FROM chat_message_operation_idempotency
            WHERE room_id = $1 AND user_id = $2 AND client_operation_id = $3
            FOR UPDATE
            "#,
            room_id.as_i64(),
            user_id.as_i64(),
            client_operation_id
        )
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        if row.operation_kind != i16::from(operation_kind) || row.request_hash != request_hash {
            return Err(Error::Conflict(
                "client_operation_id was already used with a different operation".to_string(),
            ));
        }
        let Some(event_id) = row.event_id else {
            return Ok(None);
        };
        if let Some(event) = self
            .latest_pin_event_by_id_in_tx(tx, room_id, &event_id)
            .await?
        {
            return Ok(Some(event));
        }
        Err(Error::Internal(
            "operation idempotency record points to a missing durable chat pin event".to_string(),
        ))
    }

    async fn complete_message_operation_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        operation: &ChatMessageOperationIdempotency<'_>,
        event: &ChatMessageEvent,
    ) -> Result<()> {
        sqlx::query!(
            r"
            UPDATE chat_message_operation_idempotency
            SET event_id = $4
            WHERE room_id = $1 AND user_id = $2 AND client_operation_id = $3
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            operation.client_operation_id,
            &event.event_id
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn complete_pin_operation_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        operation: &ChatMessageOperationIdempotency<'_>,
        event: &ChatPinEvent,
    ) -> Result<()> {
        sqlx::query!(
            r"
            UPDATE chat_message_operation_idempotency
            SET event_id = $4
            WHERE room_id = $1 AND user_id = $2 AND client_operation_id = $3
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            operation.client_operation_id,
            &event.event_id
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn clear_incomplete_message_operation_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        operation: &ChatMessageOperationIdempotency<'_>,
    ) -> Result<()> {
        sqlx::query!(
            r"
            DELETE FROM chat_message_operation_idempotency
            WHERE room_id = $1
              AND user_id = $2
              AND client_operation_id = $3
              AND event_id IS NULL
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            operation.client_operation_id
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn get_with_attachments_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        message_id: i64,
        created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageWithAttachments>> {
        let message = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
              AND id = $2
              AND created_at = $3
              AND deletion_source IS DISTINCT FROM 'account'
            "#,
            room_id.as_i64(),
            message_id,
            created_at
        )
        .fetch_optional(&mut **tx)
        .await?;
        let Some(message) = message else {
            return Ok(None);
        };
        let message = chat_message_from_row(message)?;
        let attachments = self
            .attachments_for_message_in_tx(tx, message.id, message.created_at)
            .await?;
        let mentions = self
            .mentions_for_message_in_tx(tx, message.id, message.created_at)
            .await?;

        Ok(Some(ChatMessageWithAttachments {
            message,
            attachments,
            reactions: Vec::new(),
            mentions,
            pin: None,
        }))
    }

    async fn attachments_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatAttachment>> {
        let attachments = sqlx::query_as!(
            ChatAttachment,
            r#"
            SELECT id AS "id!",
                   kind AS "kind!: ChatAttachmentKind",
                   room_id AS "room_id!: RoomId",
                   message_id AS "message_id!",
                   message_created_at AS "message_created_at!",
                   filename,
                   storage_backend AS "storage_backend!",
                   object_key AS "object_key!",
                   url,
                   mime_type,
                   size_bytes,
                   width,
                   height,
                   metadata AS "metadata!: crate::models::FileMetadata",
                   created_at AS "created_at!",
                   NULL::JSONB AS "object_access?: crate::models::FileObjectAccess",

                   NULL::TEXT AS "reuse_token?",
                   NULL::TIMESTAMPTZ AS "reuse_expires_at?"
            FROM chat_message_attachments
            WHERE message_id = $1 AND message_created_at = $2
            ORDER BY created_at ASC, id ASC
            "#,
            message_id,
            message_created_at
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(attachments)
    }

    async fn mentions_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatMention>> {
        let mentions = sqlx::query_as!(
            ChatMention,
            r#"
            SELECT m.room_id AS "room_id!: RoomId",
                   m.message_id AS "message_id!",
                   m.message_created_at AS "message_created_at!",
                   m.mentioned_user_id AS "mentioned_user_id!: UserId",
                   u.username AS "username?",
                   m.start_char AS "start!",
                   m.length_chars AS "length!",
                   m.created_at AS "created_at!"
            FROM chat_message_mentions m
            LEFT JOIN users u ON u.id = m.mentioned_user_id
            WHERE m.message_id = $1 AND m.message_created_at = $2
            ORDER BY m.start_char ASC, m.mentioned_user_id ASC
            "#,
            message_id,
            message_created_at
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(mentions)
    }

    async fn insert_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ChatMessageEvent,
    ) -> Result<ChatMessageEventLog> {
        let summary = chat_event_summary(event);
        let event_type = chat_event_type(event.kind);
        validate_chat_event_for_insert(event, event_type)?;
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            INSERT INTO chat_message_events (
                event_id, room_id, actor_user_id, message_id, message_created_at,
                event_type, event_version, message_version, payload, summary, occurred_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $8, $9, $10)
            RETURNING sequence AS "sequence!",
                      event_id AS "event_id!",
                      room_id AS "room_id?",
                      actor_user_id AS "actor_user_id?",
                      payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                      occurred_at AS "occurred_at!"
            "#,
            &event.event_id,
            event.room_id.as_i64(),
            event.actor_user_id.as_i64(),
            event.message.message.id,
            event.message.message.created_at,
            event_type,
            event.message.message.version,
            sqlx::types::Json(event) as _,
            sqlx::types::Json(summary) as _,
            event.occurred_at
        )
        .fetch_one(&mut **tx)
        .await?;
        row.try_into_log()
    }

    async fn insert_pin_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ChatPinEvent,
    ) -> Result<ChatPinEventLog> {
        validate_required_text(
            &event.event_id,
            "chat pin event_id",
            CHAT_EVENT_ID_MAX_CHARS,
        )?;
        let summary = chat_pin_event_summary(event);
        insert_room_resource_event_with_executor(
            &NewRoomResourceEvent {
                event_id: event.event_id.clone(),
                scope_type: RoomResourceEventScope::Room,
                room_id: Some(event.room_id.as_i64()),
                user_id: None,
                aggregate_type: "chat_message".to_string(),
                aggregate_id: event.message.message.id.to_string(),
                resource_type: crate::repository::RoomResourceKind::ChatPins,
                resource_id: chat_pin_resource_id(&event.message.message),
                event_type: event.kind.as_str().to_string(),
                event_version: 1,
                aggregate_version: Some(event.message.message.version),
                actor_user_id: Some(event.actor_user_id.as_i64()),
                payload: Some(crate::repository::RoomResourceEventPayload::ChatPin {
                    event: event.clone(),
                }),
                summary,
                occurred_at: event.occurred_at,
            },
            &mut **tx,
        )
        .await?;

        self.latest_pin_event_by_id_in_tx(tx, &event.room_id, &event.event_id)
            .await?
            .ok_or_else(|| Error::Internal("chat pin event was not inserted".to_string()))
    }

    async fn get_event_by_id_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        event_id: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let event_types = chat_message_event_types();
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT e.sequence AS "sequence!",
                   e.event_id AS "event_id!",
                   e.room_id AS "room_id?",
                   e.actor_user_id AS "actor_user_id?",
                   e.payload AS "event_payload?: sqlx::types::Json<ChatMessageEvent>",
                   e.occurred_at AS "occurred_at!"
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.room_id = e.room_id
             AND m.id = e.message_id
             AND m.created_at = e.message_created_at
             AND m.deletion_source IS DISTINCT FROM 'account'
            WHERE e.room_id = $1
              AND e.event_id = $2
              AND e.event_type = ANY($3::text[])
            "#,
            room_id.as_i64(),
            event_id,
            &event_types
        )
        .fetch_optional(&mut **tx)
        .await?;

        row.map(ChatEventRow::try_into_log).transpose()
    }

    async fn latest_pin_event_by_id_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        event_id: &str,
    ) -> Result<Option<ChatPinEventLog>> {
        let row = sqlx::query_as!(
            ChatPinEventRow,
            r#"
            SELECT event_id AS "event_id!",
                   sequence AS "sequence!",
                   payload AS "event_payload?: sqlx::types::Json<RoomResourceEventPayload>",
                   occurred_at AS "occurred_at!"
            FROM room_resource_events
            WHERE room_id = $1
              AND event_id = $2
              AND resource_type = $3
            LIMIT 1
            "#,
            room_id.as_i64(),
            event_id,
            CHAT_PINS_RESOURCE_TYPE
        )
        .fetch_optional(&mut **tx)
        .await?;

        row.map(ChatPinEventRow::try_into_log).transpose()
    }

    async fn latest_pin_event_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
        kind: ChatPinEventKind,
    ) -> Result<Option<ChatPinEventLog>> {
        let row = sqlx::query_as!(
            ChatPinEventRow,
            r#"
            SELECT event_id AS "event_id!",
                   sequence AS "sequence!",
                   payload AS "event_payload?: sqlx::types::Json<RoomResourceEventPayload>",
                   occurred_at AS "occurred_at!"
            FROM room_resource_events
            WHERE room_id = $1
              AND resource_type = $2
              AND event_type = $3
              AND resource_id = $4
            ORDER BY sequence DESC
            LIMIT 1
            "#,
            room_id.as_i64(),
            CHAT_PINS_RESOURCE_TYPE,
            kind.as_str(),
            chat_pin_resource_id_parts(message_id, message_created_at)
        )
        .fetch_optional(&mut **tx)
        .await?;

        row.map(ChatPinEventRow::try_into_log).transpose()
    }

    async fn attachments_for_message(
        &self,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatAttachment>> {
        self.attachments_for_message_from_pool(
            self.eventually_consistent_pool(),
            message_id,
            message_created_at,
        )
        .await
    }

    async fn attachments_for_message_from_pool(
        &self,
        pool: &PgPool,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatAttachment>> {
        let attachments = sqlx::query_as!(
            ChatAttachment,
            r#"
            SELECT id AS "id!",
                   kind AS "kind!: ChatAttachmentKind",
                   room_id AS "room_id!: RoomId",
                   message_id AS "message_id!",
                   message_created_at AS "message_created_at!",
                   filename,
                   storage_backend AS "storage_backend!",
                   object_key AS "object_key!",
                   url,
                   mime_type,
                   size_bytes,
                   width,
                   height,
                   metadata AS "metadata!: crate::models::FileMetadata",
                   created_at AS "created_at!",
                   NULL::JSONB AS "object_access?: crate::models::FileObjectAccess",

                   NULL::TEXT AS "reuse_token?",
                   NULL::TIMESTAMPTZ AS "reuse_expires_at?"
            FROM chat_message_attachments
            WHERE message_id = $1 AND message_created_at = $2
            ORDER BY created_at ASC, id ASC
            "#,
            message_id,
            message_created_at
        )
        .fetch_all(pool)
        .await?;

        Ok(attachments)
    }

    async fn attachments_for_messages_from_pool(
        &self,
        pool: &PgPool,
        messages: &[&ChatMessage],
    ) -> Result<Vec<ChatAttachment>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
        let created_ats: Vec<DateTime<Utc>> = messages.iter().map(|m| m.created_at).collect();
        let attachments = sqlx::query_as!(
            ChatAttachment,
            r#"
            SELECT a.id AS "id!",
                   a.kind AS "kind!: ChatAttachmentKind",
                   a.room_id AS "room_id!: RoomId",
                   a.message_id AS "message_id!",
                   a.message_created_at AS "message_created_at!",
                   a.filename,
                   a.storage_backend AS "storage_backend!",
                   a.object_key AS "object_key!",
                   a.url,
                   a.mime_type,
                   a.size_bytes,
                   a.width,
                   a.height,
                   a.metadata AS "metadata!: crate::models::FileMetadata",
                   a.created_at AS "created_at!",
                   NULL::JSONB AS "object_access?: crate::models::FileObjectAccess",

                   NULL::TEXT AS "reuse_token?",
                   NULL::TIMESTAMPTZ AS "reuse_expires_at?"
            FROM chat_message_attachments a
            JOIN unnest($1::bigint[], $2::timestamptz[]) AS m(id, created_at)
              ON a.message_id = m.id AND a.message_created_at = m.created_at
            ORDER BY a.message_created_at DESC, a.message_id DESC, a.created_at ASC, a.id ASC
            "#,
            &ids,
            &created_ats
        )
        .fetch_all(pool)
        .await?;

        Ok(attachments)
    }

    async fn attach_attachments_and_reactions_to_messages(
        &self,
        messages: Vec<ChatMessage>,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        self.attach_attachments_and_reactions_to_messages_from_pool(
            self.eventually_consistent_pool(),
            messages,
            viewer_user_id,
        )
        .await
    }

    async fn attach_attachments_and_reactions_to_messages_from_pool(
        &self,
        pool: &PgPool,
        messages: Vec<ChatMessage>,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        let visible_attachment_messages = messages
            .iter()
            .filter(|message| message.status != ChatMessageStatus::Deleted)
            .collect::<Vec<_>>();
        let (attachments, mut reaction_grouped, mut mention_grouped, mut pin_grouped) = tokio::try_join!(
            self.attachments_for_messages_from_pool(pool, &visible_attachment_messages),
            self.reaction_summaries_for_messages_from_pool(pool, &messages, viewer_user_id),
            self.mentions_for_messages_from_pool(pool, &messages),
            self.pins_for_messages_from_pool(pool, &messages),
        )?;
        let mut grouped = HashMap::<ChatMessageKey, Vec<ChatAttachment>>::new();
        for attachment in attachments {
            grouped
                .entry(chat_attachment_message_key(&attachment))
                .or_default()
                .push(attachment);
        }
        Ok(messages
            .into_iter()
            .map(|message| {
                let key = chat_message_key(&message);
                let attachments = grouped.remove(&key).unwrap_or_default();
                let reactions = reaction_grouped.remove(&key).unwrap_or_default();
                let mentions = mention_grouped.remove(&key).unwrap_or_default();
                let pin = pin_grouped.remove(&key);
                ChatMessageWithAttachments {
                    message,
                    attachments,
                    reactions,
                    mentions,
                    pin,
                }
            })
            .collect())
    }

    async fn messages_for_pin_rows(
        &self,
        room_id: &RoomId,
        pins: &[ChatMessagePin],
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithAttachments>> {
        if pins.is_empty() {
            return Ok(Vec::new());
        }
        let pool = self.eventually_consistent_pool();
        let ids = pins.iter().map(|pin| pin.message_id).collect::<Vec<_>>();
        let created_ats = pins
            .iter()
            .map(|pin| pin.message_created_at)
            .collect::<Vec<_>>();
        let messages = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT m.id AS "id!",
                   m.room_id AS "room_id!: RoomId",
                   m.user_id AS "user_id?: UserId",
                   m.client_message_id,
                   m.content AS "content!",
                   m.message_type AS "message_type!: ChatMessageType",
                   m.status AS "status!: ChatMessageStatus",
                   m.version AS "version!",
                   m.reply_to_message_id,
                   m.reply_to_message_created_at,
                   m.metadata AS "metadata?: ChatMetadata",
                   m.edited_at,
                   m.deleted_at,
                   m.deleted_by AS "deleted_by?: UserId",
                   m.delete_reason,
                   m.created_at AS "created_at!"
            FROM chat_messages m
            JOIN unnest($2::bigint[], $3::timestamptz[]) AS wanted(id, created_at)
              ON m.id = wanted.id AND m.created_at = wanted.created_at
            WHERE m.room_id = $1
              AND m.status <> $4
              AND m.deletion_source IS DISTINCT FROM 'account'
            "#,
            room_id.as_i64(),
            &ids,
            &created_ats,
            i16::from(ChatMessageStatus::Deleted)
        )
        .fetch_all(pool)
        .await?;
        let messages = chat_messages_from_rows(messages)?;
        let mut grouped = self
            .attach_attachments_and_reactions_to_messages(messages, viewer_user_id)
            .await?
            .into_iter()
            .map(|message| (chat_message_key(&message.message), message))
            .collect::<HashMap<_, _>>();

        pins.iter()
            .map(|pin| {
                grouped
                    .remove(&(pin.message_id, pin.message_created_at))
                    .ok_or_else(|| {
                        Error::Internal("chat pin points to missing message".to_string())
                    })
            })
            .collect()
    }

    async fn message_event_payload_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        message: &ChatMessage,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatMessageWithAttachments> {
        let attachments = self
            .attachments_for_message_in_tx(tx, message.id, message.created_at)
            .await?;
        let reactions = self
            .reaction_summaries_for_messages_with_executor(
                &mut **tx,
                std::slice::from_ref(message),
                viewer_user_id,
            )
            .await?
            .remove(&chat_message_key(message))
            .unwrap_or_default();
        let mentions = self
            .mentions_for_message_in_tx(tx, message.id, message.created_at)
            .await?;
        let pin = self
            .pin_for_message_in_tx(tx, room_id, message.id, message.created_at)
            .await?;
        if message.room_id != *room_id {
            return Err(Error::Internal(
                "chat message event payload room mismatch".to_string(),
            ));
        }
        Ok(ChatMessageWithAttachments {
            message: message.clone(),
            attachments,
            reactions,
            mentions,
            pin,
        })
    }

    async fn get_message_for_update_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        let message = sqlx::query_as!(
            ChatMessageRow,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   user_id AS "user_id?: UserId",
                   client_message_id,
                   content AS "content!",
                   message_type AS "message_type!: ChatMessageType",
                   status AS "status!: ChatMessageStatus",
                   version AS "version!",
                   reply_to_message_id,
                   reply_to_message_created_at,
                   metadata AS "metadata?: ChatMetadata",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
              AND id = $2
              AND deletion_source IS DISTINCT FROM 'account'
            FOR UPDATE
            "#,
            room_id.as_i64(),
            message_id
        )
        .fetch_optional(&mut **tx)
        .await?;
        optional_chat_message_from_row(message)
    }

    fn pin_scope_lock_key(room_id: &RoomId) -> i64 {
        super::stable_scope_lock_key(room_id.as_i64(), Some(1))
    }

    async fn lock_room_pins_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
    ) -> Result<()> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1)",
            Self::pin_scope_lock_key(room_id)
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn count_active_pins_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
    ) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_message_pins p
            JOIN chat_messages m
              ON m.room_id = p.room_id
             AND m.id = p.message_id
             AND m.created_at = p.message_created_at
            WHERE p.room_id = $1
              AND m.status <> $2
            "#,
            room_id.as_i64(),
            i16::from(ChatMessageStatus::Deleted)
        )
        .fetch_one(&mut **tx)
        .await?;
        Ok(count)
    }

    async fn upsert_pin_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
        pinned_by: &UserId,
        note: Option<&str>,
        pinned_at: DateTime<Utc>,
    ) -> Result<bool> {
        let inserted = sqlx::query!(
            r"
            INSERT INTO chat_message_pins (
                room_id, message_id, message_created_at, pinned_by, note, pinned_at
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (room_id, message_id, message_created_at) DO NOTHING
            ",
            message.room_id.as_i64(),
            message.id,
            message.created_at,
            pinned_by.as_i64(),
            note,
            pinned_at
        )
        .execute(&mut **tx)
        .await?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn delete_pin_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
    ) -> Result<()> {
        sqlx::query!(
            r"
            DELETE FROM chat_message_pins
            WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3
            ",
            message.room_id.as_i64(),
            message.id,
            message.created_at
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn delete_pin_for_message(
        &self,
        room_id: RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query!(
            r"
            DELETE FROM chat_message_pins
            WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3
            ",
            room_id.as_i64(),
            message_id,
            message_created_at
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn mentions_for_messages(
        &self,
        messages: &[ChatMessage],
    ) -> Result<HashMap<ChatMessageKey, Vec<ChatMention>>> {
        self.mentions_for_messages_from_pool(self.eventually_consistent_pool(), messages)
            .await
    }

    async fn mentions_for_messages_from_pool(
        &self,
        pool: &PgPool,
        messages: &[ChatMessage],
    ) -> Result<HashMap<ChatMessageKey, Vec<ChatMention>>> {
        if messages.is_empty() {
            return Ok(HashMap::new());
        }
        let message_ids = messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        let message_created_at = messages
            .iter()
            .map(|message| message.created_at)
            .collect::<Vec<_>>();
        let mentions = sqlx::query_as!(
            ChatMention,
            r#"
            SELECT m.room_id AS "room_id!: RoomId",
                   m.message_id AS "message_id!",
                   m.message_created_at AS "message_created_at!",
                   m.mentioned_user_id AS "mentioned_user_id!: UserId",
                   u.username AS "username?",
                   m.start_char AS "start!",
                   m.length_chars AS "length!",
                   m.created_at AS "created_at!"
            FROM chat_message_mentions m
            LEFT JOIN users u ON u.id = m.mentioned_user_id
            WHERE (m.message_id, m.message_created_at) IN (
                SELECT * FROM UNNEST($1::BIGINT[], $2::TIMESTAMPTZ[])
            )
            ORDER BY m.message_created_at ASC, m.message_id ASC, m.start_char ASC, m.mentioned_user_id ASC
            "#,
            &message_ids,
            &message_created_at
        )
        .fetch_all(pool)
        .await?;
        let mut grouped = HashMap::<ChatMessageKey, Vec<ChatMention>>::new();
        for mention in mentions {
            grouped
                .entry((mention.message_id, mention.message_created_at))
                .or_default()
                .push(mention);
        }
        Ok(grouped)
    }

    async fn pins_for_messages_from_pool(
        &self,
        pool: &PgPool,
        messages: &[ChatMessage],
    ) -> Result<HashMap<ChatMessageKey, ChatMessagePin>> {
        if messages.is_empty() {
            return Ok(HashMap::new());
        }
        let message_ids = messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        let message_created_at = messages
            .iter()
            .map(|message| message.created_at)
            .collect::<Vec<_>>();
        let pins = sqlx::query_as!(
            ChatMessagePin,
            r#"
            SELECT p.room_id AS "room_id!: RoomId",
                   p.message_id AS "message_id!",
                   p.message_created_at AS "message_created_at!",
                   p.pinned_by AS "pinned_by?: UserId",
                   u.username AS "pinned_by_username?",
                   p.note,
                   p.pinned_at AS "pinned_at!"
            FROM chat_message_pins p
            LEFT JOIN users u ON u.id = p.pinned_by
            WHERE (p.message_id, p.message_created_at) IN (
                SELECT * FROM UNNEST($1::BIGINT[], $2::TIMESTAMPTZ[])
            )
            "#,
            &message_ids,
            &message_created_at
        )
        .fetch_all(pool)
        .await?;
        Ok(pins
            .into_iter()
            .map(|pin| ((pin.message_id, pin.message_created_at), pin))
            .collect())
    }

    async fn pin_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessagePin>> {
        let pin = sqlx::query_as!(
            ChatMessagePin,
            r#"
            SELECT p.room_id AS "room_id!: RoomId",
                   p.message_id AS "message_id!",
                   p.message_created_at AS "message_created_at!",
                   p.pinned_by AS "pinned_by?: UserId",
                   u.username AS "pinned_by_username?",
                   p.note,
                   p.pinned_at AS "pinned_at!"
            FROM chat_message_pins p
            LEFT JOIN users u ON u.id = p.pinned_by
            WHERE p.room_id = $1
              AND p.message_id = $2
              AND p.message_created_at = $3
            "#,
            room_id.as_i64(),
            message_id,
            message_created_at
        )
        .fetch_optional(&mut **tx)
        .await?;
        Ok(pin)
    }

    async fn reaction_summaries_for_messages_from_pool(
        &self,
        pool: &PgPool,
        messages: &[ChatMessage],
        viewer_user_id: Option<&UserId>,
    ) -> Result<HashMap<ChatMessageKey, Vec<ChatReactionSummary>>> {
        self.reaction_summaries_for_messages_with_executor(pool, messages, viewer_user_id)
            .await
    }

    async fn reaction_summaries_for_messages_with_executor<'e, E>(
        &self,
        executor: E,
        messages: &[ChatMessage],
        viewer_user_id: Option<&UserId>,
    ) -> Result<HashMap<ChatMessageKey, Vec<ChatReactionSummary>>>
    where
        E: Executor<'e, Database = Postgres>,
    {
        if messages.is_empty() {
            return Ok(Default::default());
        }
        let ids: Vec<i64> = messages.iter().map(|message| message.id).collect();
        let created_ats: Vec<DateTime<Utc>> =
            messages.iter().map(|message| message.created_at).collect();
        let viewer_id = viewer_user_id.map(UserId::as_i64);
        let rows = sqlx::query!(
            r#"
            SELECT
                r.message_id AS "message_id!",
                r.message_created_at AS "message_created_at!",
                r.reaction_key AS key,
                COUNT(*)::bigint AS "count!",
                COALESCE(BOOL_OR($3::bigint IS NOT NULL AND r.user_id = $3), FALSE) AS "reacted_by_me!"
            FROM chat_message_reactions r
            JOIN unnest($1::bigint[], $2::timestamptz[]) AS m(id, created_at)
              ON r.message_id = m.id AND r.message_created_at = m.created_at
            GROUP BY r.message_id, r.message_created_at, r.reaction_key
            ORDER BY COUNT(*) DESC, r.reaction_key ASC
            "#,
            &ids,
            &created_ats,
            viewer_id
        )
        .fetch_all(executor)
        .await?;

        let mut grouped = HashMap::<ChatMessageKey, Vec<ChatReactionSummary>>::new();
        for row in rows {
            grouped
                .entry((row.message_id, row.message_created_at))
                .or_default()
                .push(ChatReactionSummary {
                    key: row.key,
                    count: row.count,
                    reacted_by_me: row.reacted_by_me,
                });
        }
        Ok(grouped)
    }
}

struct IdempotencyRow {
    request_hash: String,
    message_id: Option<i64>,
    message_created_at: Option<DateTime<Utc>>,
    event_id: Option<String>,
}

pub struct ChatMessageOperationIdempotency<'a> {
    pub client_operation_id: &'a str,
    pub operation_kind: ChatMessageOperationKind,
    pub request_hash: &'a str,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
}

pub struct EditChatMessageEventRequest<'a> {
    pub room_id: &'a RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub content: &'a str,
    pub metadata: &'a Option<ChatMetadata>,
    pub expected_version: Option<i64>,
    pub event_id: &'a str,
    pub actor_user_id: &'a UserId,
    pub occurred_at: DateTime<Utc>,
    pub operation: Option<&'a ChatMessageOperationIdempotency<'a>>,
}

pub struct DeleteChatMessageEventRequest<'a> {
    pub room_id: &'a RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub deleted_by: &'a UserId,
    pub reason: Option<&'a str>,
    pub expected_version: Option<i64>,
    pub event_id: &'a str,
    pub occurred_at: DateTime<Utc>,
    pub operation: Option<&'a ChatMessageOperationIdempotency<'a>>,
}

pub struct PinChatMessageEventRequest<'a> {
    pub room_id: &'a RoomId,
    pub message_id: i64,
    pub pinned_by: &'a UserId,
    pub note: Option<&'a str>,
    pub max_pins_per_room: Option<i64>,
    pub event_id: &'a str,
    pub occurred_at: DateTime<Utc>,
    pub operation: Option<&'a ChatMessageOperationIdempotency<'a>>,
}

pub struct UnpinChatMessageEventRequest<'a> {
    pub room_id: &'a RoomId,
    pub message_id: i64,
    pub unpinned_by: &'a UserId,
    pub event_id: &'a str,
    pub occurred_at: DateTime<Utc>,
    pub operation: Option<&'a ChatMessageOperationIdempotency<'a>>,
}

struct ExistingIdempotentEventRequest<'a> {
    message: &'a ChatMessage,
    client_message_id: &'a str,
    request_hash: &'a str,
    existing: IdempotencyRow,
    event_id: &'a str,
    occurred_at: DateTime<Utc>,
}

pub struct IdempotentChatEventInsert {
    pub event: ChatMessageEventLog,
    pub inserted: bool,
    pub pin_event: Option<ChatPinEventLog>,
}

pub struct IdempotentChatPinEventInsert {
    pub event: ChatPinEventLog,
    pub inserted: bool,
}

fn chat_event_type(kind: ChatEventKind) -> &'static str {
    match kind {
        ChatEventKind::Created => CHAT_MESSAGE_CREATED_EVENT_TYPE,
        ChatEventKind::Edited => CHAT_MESSAGE_EDITED_EVENT_TYPE,
        ChatEventKind::Deleted => CHAT_MESSAGE_DELETED_EVENT_TYPE,
        ChatEventKind::ReactionsChanged => CHAT_MESSAGE_REACTIONS_CHANGED_EVENT_TYPE,
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatEventSummary {
    kind: i16,
    message_id: i64,
    message_created_at: DateTime<Utc>,
    message_version: i64,
    actor_user_id: i64,
}

fn chat_event_summary(event: &ChatMessageEvent) -> ChatEventSummary {
    ChatEventSummary {
        kind: i16::from(event.kind),
        message_id: event.message.message.id,
        message_created_at: event.message.message.created_at,
        message_version: event.message.message.version,
        actor_user_id: event.actor_user_id.as_i64(),
    }
}

fn chat_pin_event_summary(event: &ChatPinEvent) -> crate::repository::RoomResourceEventSummary {
    crate::repository::RoomResourceEventSummary {
        event_type: event.kind.as_str().to_string(),
        room_id: Some(event.room_id.as_i64()),
        actor_user_id: Some(event.actor_user_id.as_i64()),
        resource_type: crate::repository::RoomResourceKind::ChatPins,
        details: crate::repository::RoomResourceEventSummaryDetails::ChatPin {
            event_kind: i16::from(event.kind),
            message_id: event.message.message.id,
            message_created_at: event.message.message.created_at,
            message_version: event.message.message.version,
            actor_user_id: event.actor_user_id.as_i64(),
            pinned: event.pin.is_some(),
        },
    }
}

#[derive(sqlx::FromRow)]
struct ChatEventRow {
    sequence: i64,
    event_id: String,
    room_id: Option<i64>,
    actor_user_id: Option<i64>,
    event_payload: Option<sqlx::types::Json<ChatMessageEvent>>,
    occurred_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ChatPinEventRow {
    sequence: i64,
    event_id: String,
    event_payload: Option<sqlx::types::Json<RoomResourceEventPayload>>,
    occurred_at: DateTime<Utc>,
}

impl ChatPinEventRow {
    fn try_into_log(self) -> Result<ChatPinEventLog> {
        let payload = self.event_payload.ok_or_else(|| {
            Error::Internal("Chat pin resource event is missing replay payload".to_string())
        })?;
        let RoomResourceEventPayload::ChatPin { mut event } = payload.0 else {
            return Err(Error::Internal(
                "Chat pin resource event has unexpected payload kind".to_string(),
            ));
        };
        if event.event_id != self.event_id
            || !datetime_matches_database_precision(event.occurred_at, self.occurred_at)
        {
            return Err(Error::Internal(
                "Chat pin resource event payload does not match indexed columns".to_string(),
            ));
        }
        event.occurred_at = self.occurred_at;
        event.sequence = self.sequence;
        Ok(ChatPinEventLog {
            sequence: self.sequence,
            event,
        })
    }
}

impl ChatEventRow {
    fn try_into_log(self) -> Result<ChatMessageEventLog> {
        let payload = self.event_payload.ok_or_else(|| {
            Error::Internal("Chat resource event is missing replay payload".to_string())
        })?;
        let mut event = payload.0;
        if event.event_id != self.event_id
            || Some(event.room_id.as_i64()) != self.room_id
            || Some(event.actor_user_id.as_i64()) != self.actor_user_id
            || !datetime_matches_database_precision(event.occurred_at, self.occurred_at)
        {
            return Err(Error::Internal(
                "Chat event outbox payload does not match indexed columns".to_string(),
            ));
        }
        event.occurred_at = self.occurred_at;
        event.sequence = self.sequence;
        Ok(ChatMessageEventLog {
            sequence: self.sequence,
            event,
        })
    }
}

fn datetime_matches_database_precision(left: DateTime<Utc>, right: DateTime<Utc>) -> bool {
    left.signed_duration_since(right).abs() <= chrono::Duration::milliseconds(1)
}

fn file_reference_id_for_chat_attachment(attachment: &ChatAttachment) -> String {
    format!(
        "{}:{}:{}:{}",
        attachment.room_id.as_i64(),
        attachment.message_id,
        attachment.message_created_at.timestamp_micros(),
        attachment.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_message() -> ChatMessage {
        ChatMessage::new(
            RoomId::expect_positive(10),
            UserId::expect_positive(20),
            "hello".to_string(),
        )
    }

    fn valid_attachment() -> NewStoredFile {
        NewStoredFile {
            filename: None,
            id: "attachment-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "database/chat/attachments/attachment-1.webp".to_string(),
            object_access: None,
            url: None,
            mime_type: Some("image/webp".to_string()),
            size_bytes: Some(1),
            width: Some(1),
            height: Some(1),
            metadata: crate::models::FileMetadata::default(),
        }
    }

    fn valid_event() -> ChatMessageEvent {
        let message = valid_message();
        ChatMessageEvent {
            event_id: "event-1".to_string(),
            sequence: 0,
            room_id: message.room_id,
            actor_user_id: message.user_id.expect("message user should exist"),
            kind: ChatEventKind::Created,
            message: ChatMessageWithAttachments {
                message,
                attachments: Vec::new(),
                reactions: Vec::new(),
                mentions: Vec::new(),
                pin: None,
            },
            occurred_at: crate::SystemClock.now(),
        }
    }

    #[test]
    fn validates_chat_message_business_fields_in_rust() {
        let mut message = valid_message();
        message.client_message_id = Some(String::new());
        assert!(matches!(
            validate_message_for_insert(&message),
            Err(Error::InvalidInput(error)) if error.contains("client_message_id")
        ));

        let mut message = valid_message();
        message.version = 0;
        assert!(matches!(
            validate_message_for_insert(&message),
            Err(Error::InvalidInput(error)) if error.contains("version")
        ));

        let mut message = valid_message();
        message.reply_to_message_id = Some(1);
        assert!(matches!(
            validate_message_for_insert(&message),
            Err(Error::InvalidInput(error)) if error.contains("reply target")
        ));
    }

    #[test]
    fn validates_chat_attachment_business_fields_in_rust() {
        let mut attachment = valid_attachment();
        attachment.id = "x".repeat(CHAT_ATTACHMENT_ID_MAX_CHARS + 1);
        assert!(matches!(
            validate_chat_attachment_for_insert(&attachment),
            Err(Error::InvalidInput(error)) if error.contains("attachment id")
        ));

        let mut attachment = valid_attachment();
        attachment.object_key = String::new();
        assert!(matches!(
            validate_chat_attachment_for_insert(&attachment),
            Err(Error::InvalidInput(error)) if error.contains("object_key")
        ));

        let mut attachment = valid_attachment();
        attachment.size_bytes = Some(0);
        assert!(matches!(
            validate_chat_attachment_for_insert(&attachment),
            Err(Error::InvalidInput(error)) if error.contains("positive")
        ));
    }

    #[test]
    fn validates_chat_event_business_fields_in_rust() {
        let mut event = valid_event();
        event.event_id = String::new();
        assert!(matches!(
            validate_chat_event_for_insert(&event, CHAT_MESSAGE_CREATED_EVENT_TYPE),
            Err(Error::InvalidInput(error)) if error.contains("event_id")
        ));

        let event = valid_event();
        let long_event_type = "x".repeat(CHAT_EVENT_TYPE_MAX_CHARS + 1);
        assert!(matches!(
            validate_chat_event_for_insert(&event, &long_event_type),
            Err(Error::InvalidInput(error)) if error.contains("event_type")
        ));

        let mut event = valid_event();
        event.message.message.version = 0;
        assert!(matches!(
            validate_chat_event_for_insert(&event, CHAT_MESSAGE_CREATED_EVENT_TYPE),
            Err(Error::InvalidInput(error)) if error.contains("message_version")
        ));
    }
}
