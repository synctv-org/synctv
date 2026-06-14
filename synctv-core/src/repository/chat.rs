use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{Executor, PgPool, Postgres, Row as _, Transaction};

use crate::{
    models::{
        ChatEventKind, ChatHistoryCursor, ChatHistoryPage, ChatImage, ChatMention,
        ChatMentionInput, ChatMessage, ChatMessageContext, ChatMessageEvent, ChatMessageEventLog,
        ChatMessageReadReceiptMember, ChatMessageReadReceiptUser, ChatMessageReadReceiptsPage,
        ChatMessageStatus, ChatMessageType, ChatMessageWithImages, ChatPlaybackMessagesQuery,
        ChatReactionSummary, ChatReactionUser, ChatReactionUsersCursor, ChatReactionUsersPage,
        ChatReadState, EventCursor, NewStoredFile, RoomId, SetChatReaction, User, UserId,
    },
    repository::FileStorageRepository,
    Error, Result,
};

type ChatMessageKey = (i64, DateTime<Utc>);

fn chat_message_key(message: &ChatMessage) -> ChatMessageKey {
    (message.id, message.created_at)
}

fn chat_image_message_key(image: &ChatImage) -> ChatMessageKey {
    (image.message_id, image.message_created_at)
}

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
        let inserted = sqlx::query_as!(
            ChatMessage,
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
                      metadata AS "metadata!: serde_json::Value",
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
        images: &[NewStoredFile],
        mentions: &[ChatMentionInput],
        request_hash: &str,
        event_id: &str,
        occurred_at: DateTime<Utc>,
    ) -> Result<IdempotentChatEventInsert> {
        let sender_id = message.user_id.ok_or_else(|| {
            Error::InvalidInput("Chat message event insert requires a sender".to_string())
        })?;
        let mut tx = self.pool.begin().await?;

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
                    });
                }
            }
        }

        let inserted = self.insert_message_in_tx(&mut tx, message).await?;
        let inserted_images = self.insert_images_in_tx(&mut tx, &inserted, images).await?;
        let inserted_mentions = self
            .insert_mentions_in_tx(&mut tx, &inserted, mentions)
            .await?;
        let event = ChatMessageEvent {
            event_id: event_id.to_string(),
            sequence: 0,
            room_id: message.room_id,
            actor_user_id: sender_id,
            kind: ChatEventKind::Created,
            message: ChatMessageWithImages {
                message: inserted,
                images: inserted_images,
                reactions: Vec::new(),
                mentions: inserted_mentions,
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
        let message = sqlx::query_as!(
            ChatMessage,
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "#,
            request.room_id.as_i64(),
            request.message_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::NotFound("Message not found".to_string()))?;

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
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        reactions.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.key.cmp(&right.key))
        });
        let mentions = self
            .mentions_for_messages(std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        let event = ChatMessageEvent {
            event_id: event_id.to_string(),
            sequence: 0,
            room_id: request.room_id,
            actor_user_id: request.user_id,
            kind: ChatEventKind::ReactionsChanged,
            message: ChatMessageWithImages {
                message,
                images,
                reactions,
                mentions,
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
        .fetch_one(&self.pool)
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
        .fetch_all(&self.pool)
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
                sequence: 0,
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
            sqlx::query_as!(
                ChatEventRow,
                r#"
                SELECT sequence AS "sequence!",
                       event_id AS "event_id!",
                       room_id AS "room_id?",
                       actor_user_id AS "actor_user_id?",
                       payload AS "event_payload?: serde_json::Value",
                       occurred_at AS "occurred_at!"
                FROM chat_message_events
                WHERE room_id = $1
                  AND sequence > $2
                  AND event_type IN (
                      'chat_message_created',
                      'chat_message_edited',
                      'chat_message_deleted',
                      'chat_message_reactions_changed'
                  )
                ORDER BY sequence ASC
                LIMIT $3
                "#,
                room_id.as_i64(),
                sequence,
                i64::from(limit)
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as!(
                ChatEventRow,
                r#"
                SELECT sequence AS "sequence!",
                       event_id AS "event_id!",
                       room_id AS "room_id?",
                       actor_user_id AS "actor_user_id?",
                       payload AS "event_payload?: serde_json::Value",
                       occurred_at AS "occurred_at!"
                FROM chat_message_events
                WHERE room_id = $1
                  AND event_type IN (
                      'chat_message_created',
                      'chat_message_edited',
                      'chat_message_deleted',
                      'chat_message_reactions_changed'
                  )
                ORDER BY sequence ASC
                LIMIT $2
                "#,
                room_id.as_i64(),
                i64::from(limit)
            )
            .fetch_all(&self.pool)
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
        let limit = limit.clamp(1, 500);
        let after_sequence = after_sequence.max(0);
        let rows = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT sequence AS "sequence!",
                   event_id AS "event_id!",
                   room_id AS "room_id?",
                   actor_user_id AS "actor_user_id?",
                   payload AS "event_payload?: serde_json::Value",
                   occurred_at AS "occurred_at!"
            FROM chat_message_events
            WHERE room_id = $1
              AND sequence > $2
              AND event_type IN (
                  'chat_message_created',
                  'chat_message_edited',
                  'chat_message_deleted',
                  'chat_message_reactions_changed'
              )
            ORDER BY sequence ASC
            LIMIT $3
            "#,
            room_id.as_i64(),
            after_sequence,
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(ChatEventRow::try_into_log).collect()
    }

    pub async fn latest_event_cursor_for_room(&self, room_id: &RoomId) -> Result<EventCursor> {
        let row = sqlx::query!(
            r"
            SELECT event_id, sequence
            FROM chat_message_events
            WHERE room_id = $1
              AND event_type IN (
                  'chat_message_created',
                  'chat_message_edited',
                  'chat_message_deleted',
                  'chat_message_reactions_changed'
              )
            ORDER BY sequence DESC
            LIMIT 1
            ",
            room_id.as_i64()
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map_or(
            EventCursor {
                event_id: None,
                sequence: 0,
            },
            |row| EventCursor {
                event_id: Some(row.event_id),
                sequence: row.sequence,
            },
        ))
    }

    pub async fn retained_chat_event_sequence_bounds(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<(i64, i64)>> {
        let row = sqlx::query!(
            r"
            SELECT MIN(sequence) AS min_sequence, MAX(sequence) AS max_sequence
            FROM chat_message_events
            WHERE room_id = $1
              AND event_type IN (
                  'chat_message_created',
                  'chat_message_edited',
                  'chat_message_deleted',
                  'chat_message_reactions_changed'
              )
            ",
            room_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.min_sequence.zip(row.max_sequence))
    }

    pub async fn latest_event_for_message(
        &self,
        room_id: &RoomId,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Option<ChatMessageEventLog>> {
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT sequence AS "sequence!",
                   event_id AS "event_id!",
                   room_id AS "room_id?",
                   actor_user_id AS "actor_user_id?",
                   payload AS "event_payload?: serde_json::Value",
                   occurred_at AS "occurred_at!"
            FROM chat_message_events
            WHERE room_id = $1
              AND event_type IN (
                  'chat_message_created',
                  'chat_message_edited',
                  'chat_message_deleted',
                  'chat_message_reactions_changed'
              )
              AND message_id = $2
              AND message_created_at = $3
            ORDER BY sequence DESC
            LIMIT 1
            "#,
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
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT sequence AS "sequence!",
                   event_id AS "event_id!",
                   room_id AS "room_id?",
                   actor_user_id AS "actor_user_id?",
                   payload AS "event_payload?: serde_json::Value",
                   occurred_at AS "occurred_at!"
            FROM chat_message_events
            WHERE event_type = 'chat_message_created'
              AND room_id = $1
              AND message_id = $2
              AND message_created_at = $3
            ORDER BY sequence ASC
            LIMIT 1
            "#,
            room_id.as_i64(),
            message_id,
            message_created_at
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
                sqlx::query_scalar!(
                    r#"
                    SELECT COUNT(*) AS "count!"
                    FROM chat_messages
                    WHERE room_id = $1
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
                .fetch_one(&self.pool)
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
        .fetch_one(&self.pool)
        .await?;

        let unread_total = sqlx::query_scalar::<_, i64>(
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
        )
        .bind(room_id.as_i64())
        .bind(sender_user_id)
        .bind(event_sequence)
        .bind(message.created_at)
        .bind(message.id)
        .fetch_one(&self.pool)
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
        .fetch_all(&self.pool)
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

        let unread_rows = sqlx::query(
            r"
            SELECT u.id,
                   u.username,
                   u.role,
                   u.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS status,
                   (active_ban.user_id IS NOT NULL) AS is_banned,
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS banned_by,
                   active_ban.reason AS banned_reason,
                   u.signup_method,
                   u.created_at,
                   u.updated_at,
                   u.version,
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
            ",
        )
        .bind(room_id.as_i64())
        .bind(sender_user_id)
        .bind(event_sequence)
        .bind(message.created_at)
        .bind(message.id)
        .bind(i64::from(limit))
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        let unread_members = unread_rows
            .into_iter()
            .map(|row| ChatMessageReadReceiptMember {
                user: User {
                    id: row.get("id"),
                    username: row.get("username"),
                    role: row.get("role"),
                    avatar_file_reference_id: row.get("avatar_file_reference_id"),
                    status: row.get("status"),
                    is_banned: row.get("is_banned"),
                    banned_at: row.get("banned_at"),
                    banned_by: row.get("banned_by"),
                    banned_reason: row.get("banned_reason"),
                    signup_method: row.get("signup_method"),
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                    version: row.get("version"),
                    deleted_at: row.get("deleted_at"),
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
              AND e.event_type = 'chat_message_created'
              AND m.status <> $3
              AND (m.user_id IS NULL OR m.user_id <> $4)
            "#,
            room_id.as_i64(),
            sequence,
            i16::from(ChatMessageStatus::Deleted),
            user_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    async fn count_unread_without_state(&self, room_id: &RoomId, user_id: &UserId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_messages
            WHERE room_id = $1
              AND status <> $2
              AND (user_id IS NULL OR user_id <> $3)
              AND created_at >= NOW() - INTERVAL '90 days'
            "#,
            room_id.as_i64(),
            i16::from(ChatMessageStatus::Deleted),
            user_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?;

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
            sqlx::query_as!(
                ChatMessage,
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
                       metadata AS "metadata!: serde_json::Value",
                       edited_at,
                       deleted_at,
                       deleted_by AS "deleted_by?: UserId",
                       delete_reason,
                       created_at AS "created_at!"
                FROM chat_messages
                WHERE room_id = $1
                  AND ($2 OR status <> $3)
                  AND (created_at, id) < ($4, $5)
                ORDER BY created_at DESC, id DESC
                LIMIT $6
                "#,
                room_id.as_i64(),
                include_deleted,
                i16::from(ChatMessageStatus::Deleted),
                cursor.created_at,
                cursor.id,
                i64::from(limit)
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as!(
                ChatMessage,
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
                       metadata AS "metadata!: serde_json::Value",
                       edited_at,
                       deleted_at,
                       deleted_by AS "deleted_by?: UserId",
                       delete_reason,
                       created_at AS "created_at!"
                FROM chat_messages
                WHERE room_id = $1
                  AND ($2 OR status <> $3)
                  AND created_at >= NOW() - INTERVAL '90 days'
                ORDER BY created_at DESC, id DESC
                LIMIT $4
                "#,
                room_id.as_i64(),
                include_deleted,
                i16::from(ChatMessageStatus::Deleted),
                i64::from(limit)
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

    pub async fn list_history_page_for_viewer(
        &self,
        room_id: &RoomId,
        cursor: Option<ChatHistoryCursor>,
        limit: i32,
        include_deleted: bool,
        viewer_user_id: Option<&UserId>,
    ) -> Result<ChatHistoryPage> {
        let event_cursor = self.latest_event_cursor_for_room(room_id).await?;
        let (messages, next_cursor) = self
            .list_by_room_cursor_for_viewer(room_id, cursor, limit, include_deleted, viewer_user_id)
            .await?;

        Ok(ChatHistoryPage {
            messages,
            next_cursor,
            event_cursor,
        })
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
        let target_hex = query.target.as_ref().map(hex::encode);
        let messages = sqlx::query_as!(
            ChatMessage,
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
                           WHEN jsonb_typeof(metadata #> '{playback,position_seconds}') = 'number'
                           THEN (metadata #>> '{playback,position_seconds}')::double precision
                           ELSE NULL
                       END AS playback_position
                FROM chat_messages
                WHERE room_id = $1
                  AND ($2 OR status <> $3)
                  AND ($4::text IS NULL OR metadata #>> '{playback,media_id}' = $4)
                  AND ($5::text IS NULL OR metadata #>> '{playback,playlist_id}' = $5)
                  AND ($6::text IS NULL OR metadata #>> '{playback,target_hex}' = $6)
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM candidates
            WHERE playback_position BETWEEN $7 AND $8
            ORDER BY playback_position ASC, created_at ASC, id ASC
            LIMIT $9
            "#,
            query.room_id.as_i64(),
            query.include_deleted,
            i16::from(ChatMessageStatus::Deleted),
            media_id,
            playlist_id,
            target_hex.as_deref(),
            start_seconds,
            end_seconds,
            i64::from(limit)
        )
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
        let mut before = sqlx::query_as!(
            ChatMessage,
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
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
        .fetch_all(&self.pool)
        .await?;
        before.reverse();

        let after = sqlx::query_as!(
            ChatMessage,
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1
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
        let msg = sqlx::query_as!(
            ChatMessage,
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE id = $1
            "#,
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
        let msg = sqlx::query_as!(
            ChatMessage,
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1 AND id = $2
            "#,
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
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        let mentions = self
            .mentions_for_messages(std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        Ok(Some(ChatMessageWithImages {
            message,
            images,
            reactions,
            mentions,
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
        let mentions = self
            .mentions_for_messages(std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        Ok(Some(ChatMessageWithImages {
            message,
            images,
            reactions: Vec::new(),
            mentions,
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
        let mentions = self
            .mentions_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: *request.room_id,
            actor_user_id: *request.actor_user_id,
            kind: ChatEventKind::Edited,
            message: ChatMessageWithImages {
                message,
                images,
                reactions: Vec::new(),
                mentions,
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
                &logged.event,
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
        let mentions = self
            .mentions_for_messages(std::slice::from_ref(&message))
            .await?
            .remove(&chat_message_key(&message))
            .unwrap_or_default();
        Ok(Some(ChatMessageWithImages {
            message,
            images: Vec::new(),
            reactions: Vec::new(),
            mentions,
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
        let mentions = self
            .mentions_for_message_in_tx(&mut tx, message.id, message.created_at)
            .await?;
        let event = ChatMessageEvent {
            event_id: request.event_id.to_string(),
            sequence: 0,
            room_id: *request.room_id,
            actor_user_id: *request.deleted_by,
            kind: ChatEventKind::Deleted,
            message: ChatMessageWithImages {
                message,
                images: Vec::new(),
                reactions: Vec::new(),
                mentions,
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
                &logged.event,
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
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM chat_messages
            WHERE room_id = $1
              AND created_at >= NOW() - INTERVAL '90 days'
            "#,
            room_id.as_i64()
        )
        .fetch_one(&self.pool)
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
        let inserted = sqlx::query_as!(
            ChatMessage,
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
                      metadata AS "metadata!: serde_json::Value",
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
        images: &[NewStoredFile],
    ) -> Result<Vec<ChatImage>> {
        let mut inserted = Vec::with_capacity(images.len());
        for image in images {
            let row = sqlx::query_as!(
                ChatImage,
                r#"
                INSERT INTO chat_message_images (
                    id, room_id, message_id, message_created_at, storage_backend,
                    object_key, url, mime_type, size_bytes, width, height, metadata
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                RETURNING id AS "id!",
                          room_id AS "room_id!: RoomId",
                          message_id AS "message_id!",
                          message_created_at AS "message_created_at!",
                          storage_backend AS "storage_backend!",
                          object_key AS "object_key!",
                          url,
                          mime_type,
                          size_bytes,
                          width,
                          height,
                          metadata AS "metadata!: serde_json::Value",
                          created_at AS "created_at!"
                "#,
                &image.id,
                message.room_id.as_i64(),
                message.id,
                message.created_at,
                &image.storage_backend,
                &image.object_key,
                image.url.as_deref(),
                image.mime_type.as_deref(),
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
            .get_with_images_in_tx(tx, &request.message.room_id, message_id, created_at)
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
        let message = sqlx::query_as!(
            ChatMessage,
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
                   metadata AS "metadata!: serde_json::Value",
                   edited_at,
                   deleted_at,
                   deleted_by AS "deleted_by?: UserId",
                   delete_reason,
                   created_at AS "created_at!"
            FROM chat_messages
            WHERE room_id = $1 AND id = $2 AND created_at = $3
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
        let images = self
            .images_for_message_in_tx(tx, message.id, message.created_at)
            .await?;
        let mentions = self
            .mentions_for_message_in_tx(tx, message.id, message.created_at)
            .await?;

        Ok(Some(ChatMessageWithImages {
            message,
            images,
            reactions: Vec::new(),
            mentions,
        }))
    }

    async fn images_for_message_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        message_id: i64,
        message_created_at: DateTime<Utc>,
    ) -> Result<Vec<ChatImage>> {
        let images = sqlx::query_as!(
            ChatImage,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   message_id AS "message_id!",
                   message_created_at AS "message_created_at!",
                   storage_backend AS "storage_backend!",
                   object_key AS "object_key!",
                   url,
                   mime_type,
                   size_bytes,
                   width,
                   height,
                   metadata AS "metadata!: serde_json::Value",
                   created_at AS "created_at!"
            FROM chat_message_images
            WHERE message_id = $1 AND message_created_at = $2
            ORDER BY created_at ASC, id ASC
            "#,
            message_id,
            message_created_at
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(images)
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
        let payload = serde_json::to_value(event)?;
        let summary = chat_event_summary(event);
        let event_type = chat_event_type(event.kind);
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
                      payload AS "event_payload?: serde_json::Value",
                      occurred_at AS "occurred_at!"
            "#,
            &event.event_id,
            event.room_id.as_i64(),
            event.actor_user_id.as_i64(),
            event.message.message.id,
            event.message.message.created_at,
            event_type,
            event.message.message.version,
            payload,
            summary,
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
        let row = sqlx::query_as!(
            ChatEventRow,
            r#"
            SELECT sequence AS "sequence!",
                   event_id AS "event_id!",
                   room_id AS "room_id?",
                   actor_user_id AS "actor_user_id?",
                   payload AS "event_payload?: serde_json::Value",
                   occurred_at AS "occurred_at!"
            FROM chat_message_events
            WHERE room_id = $1
              AND event_id = $2
              AND event_type IN (
                  'chat_message_created',
                  'chat_message_edited',
                  'chat_message_deleted',
                  'chat_message_reactions_changed'
              )
            "#,
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
        let images = sqlx::query_as!(
            ChatImage,
            r#"
            SELECT id AS "id!",
                   room_id AS "room_id!: RoomId",
                   message_id AS "message_id!",
                   message_created_at AS "message_created_at!",
                   storage_backend AS "storage_backend!",
                   object_key AS "object_key!",
                   url,
                   mime_type,
                   size_bytes,
                   width,
                   height,
                   metadata AS "metadata!: serde_json::Value",
                   created_at AS "created_at!"
            FROM chat_message_images
            WHERE message_id = $1 AND message_created_at = $2
            ORDER BY created_at ASC, id ASC
            "#,
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
        let images = sqlx::query_as!(
            ChatImage,
            r#"
            SELECT a.id AS "id!",
                   a.room_id AS "room_id!: RoomId",
                   a.message_id AS "message_id!",
                   a.message_created_at AS "message_created_at!",
                   a.storage_backend AS "storage_backend!",
                   a.object_key AS "object_key!",
                   a.url,
                   a.mime_type,
                   a.size_bytes,
                   a.width,
                   a.height,
                   a.metadata AS "metadata!: serde_json::Value",
                   a.created_at AS "created_at!"
            FROM chat_message_images a
            JOIN unnest($1::bigint[], $2::timestamptz[]) AS m(id, created_at)
              ON a.message_id = m.id AND a.message_created_at = m.created_at
            ORDER BY a.message_created_at DESC, a.message_id DESC, a.created_at ASC, a.id ASC
            "#,
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
        let mut grouped = HashMap::<ChatMessageKey, Vec<ChatImage>>::new();
        for image in images {
            grouped
                .entry(chat_image_message_key(&image))
                .or_default()
                .push(image);
        }
        let mut reaction_grouped = self
            .reaction_summaries_for_messages(&messages, viewer_user_id)
            .await?;
        let mut mention_grouped = self.mentions_for_messages(&messages).await?;

        Ok(messages
            .drain(..)
            .map(|message| {
                let key = chat_message_key(&message);
                let images = grouped.remove(&key).unwrap_or_default();
                let reactions = reaction_grouped.remove(&key).unwrap_or_default();
                let mentions = mention_grouped.remove(&key).unwrap_or_default();
                ChatMessageWithImages {
                    message,
                    images,
                    reactions,
                    mentions,
                }
            })
            .collect())
    }

    async fn mentions_for_messages(
        &self,
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
        .fetch_all(&self.pool)
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

    async fn reaction_summaries_for_messages(
        &self,
        messages: &[ChatMessage],
        viewer_user_id: Option<&UserId>,
    ) -> Result<HashMap<ChatMessageKey, Vec<ChatReactionSummary>>> {
        self.reaction_summaries_for_messages_with_executor(&self.pool, messages, viewer_user_id)
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

fn chat_event_type(kind: ChatEventKind) -> &'static str {
    match kind {
        ChatEventKind::Created => "chat_message_created",
        ChatEventKind::Edited => "chat_message_edited",
        ChatEventKind::Deleted => "chat_message_deleted",
        ChatEventKind::ReactionsChanged => "chat_message_reactions_changed",
    }
}

fn chat_event_summary(event: &ChatMessageEvent) -> serde_json::Value {
    serde_json::json!({
        "kind": i16::from(event.kind),
        "message_id": event.message.message.id,
        "message_created_at": event.message.message.created_at,
        "message_version": event.message.message.version,
        "actor_user_id": event.actor_user_id.as_i64(),
    })
}

#[derive(sqlx::FromRow)]
struct ChatEventRow {
    sequence: i64,
    event_id: String,
    room_id: Option<i64>,
    actor_user_id: Option<i64>,
    event_payload: Option<serde_json::Value>,
    occurred_at: DateTime<Utc>,
}

impl ChatEventRow {
    fn try_into_log(self) -> Result<ChatMessageEventLog> {
        let payload = self.event_payload.ok_or_else(|| {
            Error::Internal("Chat resource event is missing replay payload".to_string())
        })?;
        let mut event: ChatMessageEvent = serde_json::from_value(payload)?;
        if event.event_id != self.event_id
            || Some(event.room_id.as_i64()) != self.room_id
            || Some(event.actor_user_id.as_i64()) != self.actor_user_id
            || event.occurred_at != self.occurred_at
        {
            return Err(Error::Internal(
                "Chat event outbox payload does not match indexed columns".to_string(),
            ));
        }
        event.sequence = self.sequence;
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
