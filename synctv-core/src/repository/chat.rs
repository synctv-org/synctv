use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Row, Transaction};

use crate::{
    models::{
        ChatEventKind, ChatHistoryCursor, ChatImage, ChatMessage, ChatMessageContext,
        ChatMessageEvent, ChatMessageEventLog, ChatMessageStatus, ChatMessageWithImages,
        ChatPlaybackMessagesQuery, ChatReactionSummary, ChatReactionUser, ChatReactionUsersCursor,
        ChatReactionUsersPage, ChatReadState, NewChatImage, RoomId, SetChatReaction, UserId,
    },
    repository::FileStorageRepository,
    Error, Result,
};

#[derive(Clone)]
pub struct ChatRepository {
    pool: PgPool,
}

impl ChatRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn create(&self, message: &ChatMessage) -> Result<ChatMessage> {
        let inserted = sqlx::query_as_unchecked!(
            ChatMessage,
            r"
            INSERT INTO chat_messages (
                room_id, user_id, client_message_id, content, message_type,
                status, version, reply_to_message_id, reply_to_message_created_at,
                metadata, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id, room_id, user_id, client_message_id, content, message_type,
                      status, version, reply_to_message_id, reply_to_message_created_at,
                      metadata, edited_at, deleted_at, deleted_by, delete_reason, created_at
            ",
            message.room_id.as_i64(),
            message.user_id.map(|id| id.as_i64()),
            &message.client_message_id,
            &message.content,
            i16::from(message.message_type),
            i16::from(message.status),
            message.version,
            message.reply_to_message_id,
            message.reply_to_message_created_at,
            &message.metadata,
            message.created_at
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(inserted)
    }

    pub async fn insert_message_event_idempotent(
        &self,
        message: &ChatMessage,
        images: &[NewChatImage],
        request_hash: &str,
        event_id: &str,
        occurred_at: DateTime<Utc>,
    ) -> Result<IdempotentChatEventInsert> {
        let mut tx = self.pool.begin().await?;

        if let Some(client_message_id) = &message.client_message_id {
            let user_id = message.user_id.ok_or_else(|| {
                Error::InvalidInput("Idempotent chat send requires a user".to_string())
            })?;
            let inserted_idempotency = sqlx::query!(
                r"
                INSERT INTO chat_message_idempotency (
                    room_id, user_id, client_message_id, request_hash
                )
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (room_id, user_id, client_message_id) DO NOTHING
                ",
                message.room_id.as_i64(),
                user_id.as_i64(),
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
                        &user_id,
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
                    });
                }
            }
        }

        let inserted = self.insert_message_in_tx(&mut tx, message).await?;
        let inserted_images = self.insert_images_in_tx(&mut tx, &inserted, images).await?;
        let event = ChatMessageEvent {
            event_id: event_id.to_string(),
            room_id: message.room_id,
            actor_user_id: message.user_id.ok_or_else(|| {
                Error::InvalidInput("Chat event requires a message sender".to_string())
            })?,
            kind: ChatEventKind::Created,
            message: ChatMessageWithImages {
                message: inserted,
                images: inserted_images,
                reactions: Vec::new(),
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
                message.user_id.expect("checked above").as_i64(),
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
        })
    }

    pub async fn insert_event(&self, event: &ChatMessageEvent) -> Result<ChatMessageEventLog> {
        let mut tx = self.pool.begin().await?;
        let logged = self.insert_event_in_tx(&mut tx, event).await?;
        tx.commit().await?;
        Ok(logged)
    }

    pub async fn set_reaction_with_event(
        &self,
        request: &SetChatReaction,
        event_id: &str,
        occurred_at: DateTime<Utc>,
    ) -> Result<ChatMessageEventLog> {
        let mut tx = self.pool.begin().await?;
        let message = sqlx::query_as::<_, ChatMessage>(
            r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            ",
        )
        .bind(request.room_id.as_i64())
        .bind(request.message_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;

        if message.status == ChatMessageStatus::Deleted {
            return Err(Error::Conflict("Message has been deleted".to_string()));
        }

        if request.enabled {
            sqlx::query(
                r"
                INSERT INTO chat_message_reactions (
                    room_id, message_id, message_created_at, user_id, reaction_key
                )
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (room_id, message_id, message_created_at, user_id, reaction_key)
                DO UPDATE SET updated_at = NOW()
                ",
            )
            .bind(request.room_id.as_i64())
            .bind(message.id)
            .bind(message.created_at)
            .bind(request.user_id.as_i64())
            .bind(&request.reaction_key)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                r"
                DELETE FROM chat_message_reactions
                WHERE room_id = $1
                  AND message_id = $2
                  AND message_created_at = $3
                  AND user_id = $4
                  AND reaction_key = $5
                ",
            )
            .bind(request.room_id.as_i64())
            .bind(message.id)
            .bind(message.created_at)
            .bind(request.user_id.as_i64())
            .bind(&request.reaction_key)
            .execute(&mut *tx)
            .await?;
        }

        let images = self
            .images_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let mut reactions = self
            .reaction_summaries_for_messages_with_executor(
                &mut *tx,
                std::slice::from_ref(&message),
                Some(&request.user_id),
            )
            .await?
            .remove(&(message.id, message.created_at))
            .unwrap_or_default();
        reactions.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.key.cmp(&right.key))
        });
        let event = ChatMessageEvent {
            event_id: event_id.to_string(),
            room_id: request.room_id,
            actor_user_id: request.user_id,
            kind: ChatEventKind::ReactionsChanged,
            message: ChatMessageWithImages {
                message,
                images,
                reactions,
            },
            occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;
        tx.commit().await?;
        Ok(logged)
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

        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)::bigint
            FROM chat_message_reactions
            WHERE room_id = $1
              AND message_id = $2
              AND message_created_at = $3
              AND reaction_key = $4
            ",
        )
        .bind(room_id.as_i64())
        .bind(message.id)
        .bind(message.created_at)
        .bind(reaction_key)
        .fetch_one(&self.pool)
        .await?;

        let fetch_limit = limit.saturating_add(1);
        let cursor_reacted_at = cursor.map(|cursor| cursor.reacted_at);
        let cursor_user_id = cursor.map(|cursor| cursor.user_id.as_i64());
        let rows = sqlx::query_as::<_, ChatReactionUser>(
            r"
            SELECT
                user_id,
                reaction_key,
                updated_at AS reacted_at
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
            ",
        )
        .bind(room_id.as_i64())
        .bind(message.id)
        .bind(message.created_at)
        .bind(reaction_key)
        .bind(cursor_reacted_at)
        .bind(cursor_user_id)
        .bind(i64::from(fetch_limit))
        .fetch_all(&self.pool)
        .await?;

        let mut users = rows;
        let next_cursor = if users.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
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
        let mut tx = self.pool.begin().await?;
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
        let mut tx = self.pool.begin().await?;
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
                .get_with_images_in_tx(&mut tx, room_id, message_id, created_at)
                .await?
                .ok_or_else(|| {
                    Error::Internal(
                        "idempotency record points to a missing chat message".to_string(),
                    )
                })?;
            let event = ChatMessageEvent {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                actor_user_id: *user_id,
                kind: ChatEventKind::Created,
                message: loaded,
                occurred_at: Utc::now(),
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
        operation_kind: ChatEventKind,
        request_hash: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let mut tx = self.pool.begin().await?;
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

    async fn insert_file_reference_for_image_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        image: &ChatImage,
    ) -> Result<()> {
        let reference_id = file_reference_id_for_chat_image(image);
        FileStorageRepository::insert_reference_in_tx(
            tx,
            &image.storage_backend,
            &image.object_key,
            "chat_message_image",
            &reference_id,
            None,
            &image.metadata,
        )
        .await?
        .ok_or_else(|| {
            crate::Error::InvalidInput("chat image object is not registered".to_string())
        })?;
        Ok(())
    }

    pub async fn list_events_after(
        &self,
        room_id: &RoomId,
        after_event_id: Option<&str>,
        limit: i32,
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

        let rows = if let Some(sequence) = after_sequence {
            sqlx::query_as_unchecked!(
                ChatEventRow,
                r"
                SELECT sequence, event_id, room_id, message_id, message_created_at,
                       actor_user_id, kind, message_version, event_payload, occurred_at
                FROM chat_message_events
                WHERE room_id = $1 AND sequence > $2
                ORDER BY sequence ASC
                LIMIT $3
                ",
                room_id.as_i64(),
                sequence,
                limit
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as_unchecked!(
                ChatEventRow,
                r"
                SELECT sequence, event_id, room_id, message_id, message_created_at,
                       actor_user_id, kind, message_version, event_payload, occurred_at
                FROM chat_message_events
                WHERE room_id = $1
                ORDER BY sequence ASC
                LIMIT $2
                ",
                room_id.as_i64(),
                limit
            )
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(ChatEventRow::try_into_log).collect()
    }

    pub async fn latest_event_for_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query_as_unchecked!(
            ChatEventRow,
            r"
            SELECT sequence, event_id, room_id, message_id, message_created_at,
                   actor_user_id, kind, message_version, event_payload, occurred_at
            FROM chat_message_events
            WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3
            ORDER BY sequence DESC
            LIMIT 1
            ",
            room_id.as_i64(),
            message_id,
            message_created_at
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(ChatEventRow::try_into_log).transpose()
    }

    pub async fn created_event_for_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query_as_unchecked!(
            ChatEventRow,
            r"
            SELECT sequence, event_id, room_id, message_id, message_created_at,
                   actor_user_id, kind, message_version, event_payload, occurred_at
            FROM chat_message_events
            WHERE room_id = $1 AND message_id = $2 AND message_created_at = $3 AND kind = $4
            ORDER BY sequence ASC
            LIMIT 1
            ",
            room_id.as_i64(),
            message_id,
            message_created_at,
            i16::from(ChatEventKind::Created)
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(ChatEventRow::try_into_log).transpose()
    }

    pub async fn get_read_state(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<ChatReadState>> {
        let state = sqlx::query_as_unchecked!(
            ChatReadState,
            r"
            SELECT room_id, user_id, last_read_message_id, last_read_message_created_at,
                   last_read_event_id, last_read_event_sequence, updated_at
            FROM chat_read_states
            WHERE room_id = $1 AND user_id = $2
            ",
            room_id.as_i64(),
            user_id.as_i64()
        )
        .fetch_optional(&self.pool)
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
        let state = sqlx::query_as_unchecked!(
ChatReadState,
r"
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
            RETURNING room_id, user_id, last_read_message_id, last_read_message_created_at,
                      last_read_event_id, last_read_event_sequence, updated_at
            ",
room_id.as_i64(),
user_id.as_i64(),
message_id,
message_created_at,
event_id,
event_sequence
)
        .fetch_optional(&self.pool)
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
        let count = if let Some(state) = state {
            if let (Some(message_id), Some(created_at)) = (
                state.last_read_message_id,
                state.last_read_message_created_at,
            ) {
                sqlx::query_scalar_unchecked!(
                    r"
                    SELECT COUNT(*)
                    FROM chat_messages
                    WHERE room_id = $1
                      AND status <> $2
                      AND (user_id IS NULL OR user_id <> $3)
                      AND (created_at, id) > ($4, $5)
                    ",
                    room_id.as_i64(),
                    i16::from(ChatMessageStatus::Deleted),
                    user_id.as_i64(),
                    created_at,
                    message_id
                )
                .fetch_one(&self.pool)
                .await?
                .unwrap_or(0)
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

    async fn count_unread_after_event_sequence(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        sequence: i64,
    ) -> Result<i64> {
        let count = sqlx::query_scalar_unchecked!(
            r"
            SELECT COUNT(*)
            FROM chat_message_events e
            JOIN chat_messages m
              ON m.id = e.message_id AND m.created_at = e.message_created_at
            WHERE e.room_id = $1
              AND e.sequence > $2
              AND e.kind = $3
              AND m.status <> $4
              AND (m.user_id IS NULL OR m.user_id <> $5)
            ",
            room_id.as_i64(),
            sequence,
            i16::from(ChatEventKind::Created),
            i16::from(ChatMessageStatus::Deleted),
            user_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        Ok(count)
    }

    async fn count_unread_without_state(&self, room_id: &RoomId, user_id: &UserId) -> Result<i64> {
        let count = sqlx::query_scalar_unchecked!(
            r"
            SELECT COUNT(*)
            FROM chat_messages
            WHERE room_id = $1
              AND status <> $2
              AND (user_id IS NULL OR user_id <> $3)
              AND created_at >= NOW() - INTERVAL '90 days'
            ",
            room_id.as_i64(),
            i16::from(ChatMessageStatus::Deleted),
            user_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        Ok(count)
    }

    pub async fn list_by_room_cursor(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
    ) -> Result<(Vec<ChatMessageWithImages>, Option<ChatHistoryCursor>)> {
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
    ) -> Result<(Vec<ChatMessageWithImages>, Option<ChatHistoryCursor>)> {
        let limit = limit.clamp(1, 100);
        let messages = if let Some(cursor) = cursor {
            sqlx::query_as_unchecked!(
ChatMessage,
r"
                SELECT id, room_id, user_id, client_message_id, content, message_type,
                       status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                       deleted_at, deleted_by, delete_reason, created_at
                FROM chat_messages
                WHERE room_id = $1
                  AND ($2 OR status <> $3)
                  AND (created_at, id) < ($4, $5)
                ORDER BY created_at DESC, id DESC
                LIMIT $6
                ",
room_id.as_i64(),
include_deleted,
i16::from(ChatMessageStatus::Deleted),
cursor.created_at,
cursor.id,
limit
)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as_unchecked!(
ChatMessage,
r"
                SELECT id, room_id, user_id, client_message_id, content, message_type,
                       status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                       deleted_at, deleted_by, delete_reason, created_at
                FROM chat_messages
                WHERE room_id = $1
                  AND ($2 OR status <> $3)
                  AND created_at >= NOW() - INTERVAL '90 days'
                ORDER BY created_at DESC, id DESC
                LIMIT $4
                ",
room_id.as_i64(),
include_deleted,
i16::from(ChatMessageStatus::Deleted),
limit
)
            .fetch_all(&self.pool)
            .await?
        };

        let next_cursor = if i32::try_from(messages.len()).ok() == Some(limit) {
            messages.last().map(|m| ChatHistoryCursor {
                created_at: m.created_at,
                id: m.id,
            })
        } else {
            None
        };

        let messages = self
            .attach_images_and_reactions_to_messages(messages, viewer_user_id)
            .await?;

        Ok((messages, next_cursor))
    }

    pub async fn list_playback_messages(
        &self,
        query: &ChatPlaybackMessagesQuery,
    ) -> Result<Vec<ChatMessageWithImages>> {
        self.list_playback_messages_for_viewer(query, None).await
    }

    pub async fn list_playback_messages_for_viewer(
        &self,
        query: &ChatPlaybackMessagesQuery,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithImages>> {
        let limit = query.limit.clamp(1, 500);
        let start_seconds = (query.position_seconds - query.before_seconds).max(0.0);
        let end_seconds = query.position_seconds + query.after_seconds;
        let media_id = query.media_id.map(|id| id.as_i64().to_string());
        let playlist_id = query.playlist_id.map(|id| id.as_i64().to_string());
        let position_expr = r"
            CASE
                WHEN jsonb_typeof(metadata #> '{playback,position_seconds}') = 'number'
                THEN (metadata #>> '{playback,position_seconds}')::double precision
                ELSE NULL
            END
        ";
        let sql = format!(
            r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE room_id = $1
              AND ($2 OR status <> $3)
              AND ($4::text IS NULL OR metadata #>> '{{playback,media_id}}' = $4)
              AND ($5::text IS NULL OR metadata #>> '{{playback,playlist_id}}' = $5)
              AND ($6::text IS NULL OR metadata #>> '{{playback,target_hash}}' = $6)
              AND {position_expr} BETWEEN $7 AND $8
            ORDER BY {position_expr} ASC, created_at ASC, id ASC
            LIMIT $9
            "
        );
        let messages = sqlx::query_as::<_, ChatMessage>(&sql)
            .bind(query.room_id.as_i64())
            .bind(query.include_deleted)
            .bind(i16::from(ChatMessageStatus::Deleted))
            .bind(media_id)
            .bind(playlist_id)
            .bind(query.target_hash.as_deref())
            .bind(start_seconds)
            .bind(end_seconds)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        self.attach_images_and_reactions_to_messages(messages, viewer_user_id)
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
        let mut before = sqlx::query_as_unchecked!(
ChatMessage,
r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE room_id = $1
              AND ($2 OR status <> $3)
              AND (created_at, id) < ($4, $5)
            ORDER BY created_at DESC, id DESC
            LIMIT $6
            ",
room_id.as_i64(),
include_deleted,
i16::from(ChatMessageStatus::Deleted),
anchor.created_at,
anchor.id,
before_limit
)
        .fetch_all(&self.pool)
        .await?;
        before.reverse();

        let after = sqlx::query_as_unchecked!(
ChatMessage,
r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE room_id = $1
              AND ($2 OR status <> $3)
              AND (created_at, id) > ($4, $5)
            ORDER BY created_at ASC, id ASC
            LIMIT $6
            ",
room_id.as_i64(),
include_deleted,
i16::from(ChatMessageStatus::Deleted),
anchor.created_at,
anchor.id,
after_limit
)
        .fetch_all(&self.pool)
        .await?;

        let anchor = self
            .attach_images_and_reactions_to_messages(vec![anchor], viewer_user_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Internal("Chat context anchor disappeared".to_string()))?;
        Ok(Some(ChatMessageContext {
            before: self
                .attach_images_and_reactions_to_messages(before, viewer_user_id)
                .await?,
            anchor,
            after: self
                .attach_images_and_reactions_to_messages(after, viewer_user_id)
                .await?,
        }))
    }

    pub async fn get_by_id(&self, message_id: i64) -> Result<Option<ChatMessage>> {
        let msg = sqlx::query_as_unchecked!(
ChatMessage,
r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE id = $1
            ",
message_id
)
        .fetch_optional(&self.pool)
        .await?;

        Ok(msg)
    }

    pub async fn get_by_room_and_id(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessage>> {
        let msg = sqlx::query_as_unchecked!(
ChatMessage,
r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE room_id = $1 AND id = $2
            ",
room_id.as_i64(),
message_id
)
        .fetch_optional(&self.pool)
        .await?;

        Ok(msg)
    }

    pub async fn get_with_images_by_room_and_id(
        &self,
        room_id: &RoomId,
        message_id: i64,
    ) -> Result<Option<ChatMessageWithImages>> {
        self.get_with_images_by_room_and_id_for_viewer(room_id, message_id, None)
            .await
    }

    pub async fn get_with_images_by_room_and_id_for_viewer(
        &self,
        room_id: &RoomId,
        message_id: i64,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Option<ChatMessageWithImages>> {
        let Some(message) = self.get_by_room_and_id(room_id, message_id).await? else {
            return Ok(None);
        };
        let images = if message.status == ChatMessageStatus::Deleted {
            Vec::new()
        } else {
            self.images_for_message(message.id, message.created_at)
                .await?
        };
        let reactions = self
            .reaction_summaries_for_messages(std::slice::from_ref(&message), viewer_user_id)
            .await?
            .remove(&(message.id, message.created_at))
            .unwrap_or_default();
        Ok(Some(ChatMessageWithImages {
            message,
            images,
            reactions,
        }))
    }

    pub async fn edit(
        &self,
        room_id: &RoomId,
        message_id: i64,
        content: &str,
        metadata: &serde_json::Value,
        expected_version: Option<i64>,
    ) -> Result<Option<ChatMessageWithImages>> {
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = ",
        );
        builder.push_bind(content);
        builder.push(", metadata = ");
        builder.push_bind(metadata);
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
            .fetch_optional(&self.pool)
            .await?;
        let Some(message) = message else {
            return Ok(None);
        };
        let images = self
            .images_for_message(message.id, message.created_at)
            .await?;
        Ok(Some(ChatMessageWithImages {
            message,
            images,
            reactions: Vec::new(),
        }))
    }

    pub async fn edit_with_event(
        &self,
        request: EditChatMessageEventRequest<'_>,
    ) -> Result<Option<IdempotentChatEventInsert>> {
        let mut tx = self.pool.begin().await?;
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
                }));
            }
        }
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = ",
        );
        builder.push_bind(request.content);
        builder.push(", metadata = ");
        builder.push_bind(request.metadata);
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
        let images = self
            .images_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            room_id: *request.room_id,
            actor_user_id: *request.actor_user_id,
            kind: ChatEventKind::Edited,
            message: ChatMessageWithImages {
                message,
                images,
                reactions: Vec::new(),
            },
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;
        if let Some(operation) = request.operation {
            self.complete_message_operation_in_tx(
                &mut tx,
                request.room_id,
                request.actor_user_id,
                operation,
                &logged.event.event_id,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Some(IdempotentChatEventInsert {
            event: logged,
            inserted: true,
        }))
    }

    pub async fn soft_delete(
        &self,
        room_id: &RoomId,
        message_id: i64,
        deleted_by: &UserId,
        reason: Option<&str>,
        expected_version: Option<i64>,
    ) -> Result<Option<ChatMessageWithImages>> {
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = '', status = ",
        );
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        builder.push(", version = version + 1, deleted_at = NOW(), deleted_by = ");
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
            .fetch_optional(&self.pool)
            .await?;
        let Some(message) = message else {
            return Ok(None);
        };
        Ok(Some(ChatMessageWithImages {
            message,
            images: Vec::new(),
            reactions: Vec::new(),
        }))
    }

    pub async fn soft_delete_with_event(
        &self,
        request: DeleteChatMessageEventRequest<'_>,
    ) -> Result<Option<IdempotentChatEventInsert>> {
        let mut tx = self.pool.begin().await?;
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
                }));
            }
        }
        let mut builder = sqlx::QueryBuilder::<Postgres>::new(
            r"
            UPDATE chat_messages
            SET content = '', status = ",
        );
        builder.push_bind(i16::from(ChatMessageStatus::Deleted));
        builder.push(", version = version + 1, deleted_at = NOW(), deleted_by = ");
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
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            room_id: *request.room_id,
            actor_user_id: *request.deleted_by,
            kind: ChatEventKind::Deleted,
            message: ChatMessageWithImages {
                message,
                images: Vec::new(),
                reactions: Vec::new(),
            },
            occurred_at: request.occurred_at,
        };
        let logged = self.insert_event_in_tx(&mut tx, &event).await?;
        if let Some(operation) = request.operation {
            self.complete_message_operation_in_tx(
                &mut tx,
                request.room_id,
                request.deleted_by,
                operation,
                &logged.event.event_id,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Some(IdempotentChatEventInsert {
            event: logged,
            inserted: true,
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
        .execute(&self.pool)
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
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i64> {
        let count = sqlx::query_scalar_unchecked!(
            r"
            SELECT COUNT(*)
            FROM chat_messages
            WHERE room_id = $1
              AND created_at >= NOW() - INTERVAL '90 days'
            ",
            room_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

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
        .execute(&self.pool)
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
        .execute(&self.pool)
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
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    async fn insert_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
    ) -> Result<ChatMessage> {
        let inserted = sqlx::query_as_unchecked!(
            ChatMessage,
            r"
            INSERT INTO chat_messages (
                room_id, user_id, client_message_id, content, message_type,
                status, version, reply_to_message_id, reply_to_message_created_at,
                metadata, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id, room_id, user_id, client_message_id, content, message_type,
                      status, version, reply_to_message_id, reply_to_message_created_at,
                      metadata, edited_at, deleted_at, deleted_by, delete_reason, created_at
            ",
            message.room_id.as_i64(),
            message.user_id.map(|id| id.as_i64()),
            &message.client_message_id,
            &message.content,
            i16::from(message.message_type),
            i16::from(message.status),
            message.version,
            message.reply_to_message_id,
            message.reply_to_message_created_at,
            &message.metadata,
            message.created_at
        )
        .fetch_one(&mut **tx)
        .await?;

        Ok(inserted)
    }

    async fn insert_images_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message: &ChatMessage,
        images: &[NewChatImage],
    ) -> Result<Vec<ChatImage>> {
        let mut inserted = Vec::with_capacity(images.len());
        for image in images {
            let row = sqlx::query_as_unchecked!(
                ChatImage,
                r"
                INSERT INTO chat_message_images (
                    id, room_id, message_id, message_created_at, storage_backend,
                    object_key, url, mime_type, size_bytes, width, height, metadata
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                RETURNING id, room_id, message_id, message_created_at,
                          storage_backend, object_key, url, mime_type, size_bytes,
                          width, height, metadata, created_at
                ",
                &image.id,
                message.room_id.as_i64(),
                message.id,
                message.created_at,
                &image.storage_backend,
                &image.object_key,
                &image.url,
                &image.mime_type,
                image.size_bytes,
                image.width,
                image.height,
                &image.metadata
            )
            .fetch_one(&mut **tx)
            .await?;
            self.insert_file_reference_for_image_in_tx(tx, &row).await?;
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
        let row = sqlx::query(
            r"
            SELECT request_hash, message_id, message_created_at, event_id
            FROM chat_message_idempotency
            WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
            FOR UPDATE
            ",
        )
        .bind(room_id.as_i64())
        .bind(user_id.as_i64())
        .bind(client_message_id)
        .fetch_optional(&mut **tx)
        .await?;

        Ok(row.map(|row| IdempotencyRow {
            request_hash: row.get("request_hash"),
            message_id: row.get("message_id"),
            message_created_at: row.get("message_created_at"),
            event_id: row.get("event_id"),
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
            .get_with_images_in_tx(tx, &request.message.room_id, message_id, created_at)
            .await?
            .ok_or_else(|| {
                Error::Internal("idempotency record points to a missing chat message".to_string())
            })?;

        if let Some(existing_event_id) = request.existing.event_id {
            return self
                .get_event_by_id_in_tx(tx, &request.message.room_id, &existing_event_id)
                .await?
                .ok_or_else(|| {
                    Error::Internal("idempotency record points to a missing chat event".to_string())
                })
                .map(Some);
        }

        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
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
            request.message.user_id.expect("checked above").as_i64(),
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

    async fn replay_message_operation_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        client_operation_id: &str,
        operation_kind: ChatEventKind,
        request_hash: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query(
            r"
            SELECT operation_kind, request_hash, event_id
            FROM chat_message_operation_idempotency
            WHERE room_id = $1 AND user_id = $2 AND client_operation_id = $3
            FOR UPDATE
            ",
        )
        .bind(room_id.as_i64())
        .bind(user_id.as_i64())
        .bind(client_operation_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        let existing_kind: i16 = row.get("operation_kind");
        let existing_hash: String = row.get("request_hash");
        if existing_kind != i16::from(operation_kind) || existing_hash != request_hash {
            return Err(Error::Conflict(
                "client_operation_id was already used with a different operation".to_string(),
            ));
        }
        let Some(event_id) = row.get::<Option<String>, _>("event_id") else {
            return Ok(None);
        };
        self.get_event_by_id_in_tx(tx, room_id, &event_id)
            .await?
            .ok_or_else(|| {
                Error::Internal(
                    "operation idempotency record points to a missing chat event".to_string(),
                )
            })
            .map(Some)
    }

    async fn complete_message_operation_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        user_id: &UserId,
        operation: &ChatMessageOperationIdempotency<'_>,
        event_id: &str,
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
            event_id
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

    async fn get_with_images_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        message_id: i64,
        created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageWithImages>> {
        let message = sqlx::query_as_unchecked!(
ChatMessage,
r"
            SELECT id, room_id, user_id, client_message_id, content, message_type,
                   status, version, reply_to_message_id, reply_to_message_created_at, metadata, edited_at,
                   deleted_at, deleted_by, delete_reason, created_at
            FROM chat_messages
            WHERE room_id = $1 AND id = $2 AND created_at = $3
            ",
room_id.as_i64(),
message_id,
created_at
)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(message) = message else {
            return Ok(None);
        };
        let images = self
            .images_for_message_in_tx(tx, message.id, message.created_at)
            .await?;

        Ok(Some(ChatMessageWithImages {
            message,
            images,
            reactions: Vec::new(),
        }))
    }

    async fn images_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatImage>> {
        let images = sqlx::query_as_unchecked!(
            ChatImage,
            r"
            SELECT id, room_id, message_id, message_created_at, storage_backend,
                   object_key, url, mime_type, size_bytes, width, height, metadata, created_at
            FROM chat_message_images
            WHERE message_id = $1 AND message_created_at = $2
            ORDER BY created_at ASC, id ASC
            ",
            message_id,
            message_created_at
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(images)
    }

    async fn insert_event_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ChatMessageEvent,
    ) -> Result<ChatMessageEventLog> {
        let payload = serde_json::to_value(event)?;
        let row = sqlx::query_as_unchecked!(
            ChatEventRow,
            r"
            INSERT INTO chat_message_events (
                event_id, room_id, message_id, message_created_at, actor_user_id,
                kind, message_version, event_payload, occurred_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING sequence, event_id, room_id, message_id, message_created_at,
                      actor_user_id, kind, message_version, event_payload, occurred_at
            ",
            &event.event_id,
            event.room_id.as_i64(),
            event.message.message.id,
            event.message.message.created_at,
            event.actor_user_id.as_i64(),
            i16::from(event.kind),
            event.message.message.version,
            payload,
            event.occurred_at
        )
        .fetch_one(&mut **tx)
        .await?;
        row.try_into_log()
    }

    async fn get_event_by_id_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        event_id: &str,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query_as_unchecked!(
            ChatEventRow,
            r"
            SELECT sequence, event_id, room_id, message_id, message_created_at,
                   actor_user_id, kind, message_version, event_payload, occurred_at
            FROM chat_message_events
            WHERE room_id = $1 AND event_id = $2
            ",
            room_id.as_i64(),
            event_id
        )
        .fetch_optional(&mut **tx)
        .await?;

        row.map(ChatEventRow::try_into_log).transpose()
    }

    async fn images_for_message(
        &self,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatImage>> {
        let images = sqlx::query_as_unchecked!(
            ChatImage,
            r"
            SELECT id, room_id, message_id, message_created_at, storage_backend,
                   object_key, url, mime_type, size_bytes, width, height, metadata, created_at
            FROM chat_message_images
            WHERE message_id = $1 AND message_created_at = $2
            ORDER BY created_at ASC, id ASC
            ",
            message_id,
            message_created_at
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(images)
    }

    async fn images_for_messages(&self, messages: &[ChatMessage]) -> Result<Vec<ChatImage>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
        let created_ats: Vec<DateTime<Utc>> = messages.iter().map(|m| m.created_at).collect();
        let images = sqlx::query_as_unchecked!(
ChatImage,
r"
            SELECT a.id, a.room_id, a.message_id, a.message_created_at, a.storage_backend, a.object_key, a.url, a.mime_type, a.size_bytes,
                   a.width, a.height, a.metadata, a.created_at
            FROM chat_message_images a
            JOIN unnest($1::bigint[], $2::timestamptz[]) AS m(id, created_at)
              ON a.message_id = m.id AND a.message_created_at = m.created_at
            ORDER BY a.message_created_at DESC, a.message_id DESC, a.created_at ASC, a.id ASC
            ",
&ids,
&created_ats
)
        .fetch_all(&self.pool)
        .await?;

        Ok(images)
    }

    async fn attach_images_and_reactions_to_messages(
        &self,
        mut messages: Vec<ChatMessage>,
        viewer_user_id: Option<&UserId>,
    ) -> Result<Vec<ChatMessageWithImages>> {
        let visible_image_messages = messages
            .iter()
            .filter(|message| message.status != ChatMessageStatus::Deleted)
            .cloned()
            .collect::<Vec<_>>();
        let images = self.images_for_messages(&visible_image_messages).await?;
        let mut grouped = std::collections::HashMap::<(i64, DateTime<Utc>), Vec<ChatImage>>::new();
        for image in images {
            grouped
                .entry((image.message_id, image.message_created_at))
                .or_default()
                .push(image);
        }
        let mut reaction_grouped = self
            .reaction_summaries_for_messages(&messages, viewer_user_id)
            .await?;

        Ok(messages
            .drain(..)
            .map(|message| {
                let key = (message.id, message.created_at);
                let images = grouped.remove(&key).unwrap_or_default();
                let reactions = reaction_grouped.remove(&key).unwrap_or_default();
                ChatMessageWithImages {
                    message,
                    images,
                    reactions,
                }
            })
            .collect())
    }

    async fn reaction_summaries_for_messages(
        &self,
        messages: &[ChatMessage],
        viewer_user_id: Option<&UserId>,
    ) -> Result<std::collections::HashMap<(i64, DateTime<Utc>), Vec<ChatReactionSummary>>> {
        self.reaction_summaries_for_messages_with_executor(&self.pool, messages, viewer_user_id)
            .await
    }

    async fn reaction_summaries_for_messages_with_executor<'e, E>(
        &self,
        executor: E,
        messages: &[ChatMessage],
        viewer_user_id: Option<&UserId>,
    ) -> Result<std::collections::HashMap<(i64, DateTime<Utc>), Vec<ChatReactionSummary>>>
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
        let rows = sqlx::query(
            r"
            SELECT
                r.message_id,
                r.message_created_at,
                r.reaction_key AS key,
                COUNT(*)::bigint AS count,
                COALESCE(BOOL_OR($3::bigint IS NOT NULL AND r.user_id = $3), FALSE) AS reacted_by_me
            FROM chat_message_reactions r
            JOIN unnest($1::bigint[], $2::timestamptz[]) AS m(id, created_at)
              ON r.message_id = m.id AND r.message_created_at = m.created_at
            GROUP BY r.message_id, r.message_created_at, r.reaction_key
            ORDER BY count DESC, r.reaction_key ASC
            ",
        )
        .bind(&ids)
        .bind(&created_ats)
        .bind(viewer_id)
        .fetch_all(executor)
        .await?;

        let mut grouped =
            std::collections::HashMap::<(i64, DateTime<Utc>), Vec<ChatReactionSummary>>::new();
        for row in rows {
            let message_id: i64 = row.try_get("message_id")?;
            let message_created_at: DateTime<Utc> = row.try_get("message_created_at")?;
            let key: String = row.try_get("key")?;
            let count: i64 = row.try_get("count")?;
            let reacted_by_me: bool = row.try_get("reacted_by_me")?;
            grouped
                .entry((message_id, message_created_at))
                .or_default()
                .push(ChatReactionSummary {
                    key,
                    count,
                    reacted_by_me,
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
    pub operation_kind: ChatEventKind,
    pub request_hash: &'a str,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
}

pub struct EditChatMessageEventRequest<'a> {
    pub room_id: &'a RoomId,
    pub message_id: i64,
    pub message_created_at: DateTime<Utc>,
    pub content: &'a str,
    pub metadata: &'a serde_json::Value,
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
}

#[derive(sqlx::FromRow)]
struct ChatEventRow {
    sequence: i64,
    event_id: String,
    room_id: i64,
    message_id: i64,
    message_created_at: DateTime<Utc>,
    actor_user_id: i64,
    kind: i16,
    message_version: i64,
    event_payload: serde_json::Value,
    occurred_at: DateTime<Utc>,
}

impl ChatEventRow {
    fn try_into_log(self) -> Result<ChatMessageEventLog> {
        let event: ChatMessageEvent = serde_json::from_value(self.event_payload)?;
        if event.event_id != self.event_id
            || event.room_id.as_i64() != self.room_id
            || event.actor_user_id.as_i64() != self.actor_user_id
            || i16::from(event.kind) != self.kind
            || event.message.message.id != self.message_id
            || event.message.message.created_at != self.message_created_at
            || event.message.message.version != self.message_version
            || event.occurred_at != self.occurred_at
        {
            return Err(Error::Internal(
                "Chat event outbox payload does not match indexed columns".to_string(),
            ));
        }
        Ok(ChatMessageEventLog {
            sequence: self.sequence,
            event,
        })
    }
}

fn file_reference_id_for_chat_image(image: &ChatImage) -> String {
    format!(
        "{}:{}:{}:{}",
        image.room_id.as_i64(),
        image.message_id,
        image.message_created_at.timestamp_micros(),
        image.id
    )
}
