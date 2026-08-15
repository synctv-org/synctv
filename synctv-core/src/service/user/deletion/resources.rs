use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use crate::{
    models::{ChatMessageType, DeletionSource, MediaId, PlaylistId, RoomId, UserId},
    repository::{realtime_outbox::NewRealtimeOutboxEvent, RoomMemberRepository},
    Result,
};

use super::{
    UserDeletedChatMessage, UserDeletedRoomImpact, UserDeletionCleanup, UserDeletionCleanupStats,
    UserService,
};

#[derive(Debug, Default)]
pub(super) struct UserOwnedRoomEntries {
    pub(super) playlist_ids: Vec<PlaylistId>,
    pub(super) media_ids: Vec<MediaId>,
}

#[derive(Debug, sqlx::FromRow)]
struct AccountDeletedChatMessageRow {
    id: i64,
    room_id: RoomId,
    message_type: ChatMessageType,
    version: i64,
    created_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

impl UserService {
    async fn insert_deleted_room_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        deleted_room_outbox_events: &HashMap<RoomId, NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        if let (Some(outbox), Some(event)) = (
            &self.realtime_outbox,
            deleted_room_outbox_events.get(room_id),
        ) {
            outbox.insert_with_executor(event, &mut **tx).await?;
        }
        Ok(())
    }

    pub(super) async fn query_owned_room_ids_in_tx(
        &self,
        user_id: &UserId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<RoomId>> {
        let room_ids = sqlx::query_scalar!(
            r#"SELECT id AS "id: RoomId"
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL
             ORDER BY id
             FOR UPDATE"#,
            user_id.as_i64(),
        )
        .fetch_all(&mut **tx)
        .await?;

        Ok(room_ids)
    }

    async fn query_membership_room_ids_in_tx(
        &self,
        user_id: &UserId,
        owned_room_ids: &HashSet<RoomId>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<RoomId>> {
        let rows = sqlx::query!(
            r#"SELECT DISTINCT rm.room_id AS "room_id: RoomId"
             FROM room_members rm
             JOIN rooms r ON r.id = rm.room_id
             WHERE rm.user_id = $1
               AND r.deleted_at IS NULL
             ORDER BY rm.room_id"#,
            user_id.as_i64(),
        )
        .fetch_all(&mut **tx)
        .await?;

        let mut room_ids = Vec::new();
        for row in rows {
            let room_id = row.room_id;
            if !owned_room_ids.contains(&room_id) {
                room_ids.push(room_id);
            }
        }

        Ok(room_ids)
    }

    async fn query_owned_room_entries_in_tx(
        &self,
        user_id: &UserId,
        owned_room_ids: &HashSet<RoomId>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<HashMap<RoomId, UserOwnedRoomEntries>> {
        let mut entries_by_room = HashMap::<RoomId, UserOwnedRoomEntries>::new();

        let playlist_rows = sqlx::query!(
            r#"SELECT p.id AS "id: PlaylistId", p.room_id AS "room_id: RoomId"
             FROM playlists p
             JOIN rooms r ON r.id = p.room_id
             WHERE p.creator_id = $1
               AND p.deleted_at IS NULL
               AND r.deleted_at IS NULL
             ORDER BY p.room_id, p.id"#,
            user_id.as_i64(),
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in playlist_rows {
            let room_id = row.room_id;
            if owned_room_ids.contains(&room_id) {
                continue;
            }
            entries_by_room
                .entry(room_id)
                .or_default()
                .playlist_ids
                .push(row.id);
        }

        let media_rows = sqlx::query!(
            r#"SELECT m.id AS "id: MediaId", m.room_id AS "room_id: RoomId"
             FROM media m
             JOIN rooms r ON r.id = m.room_id
             WHERE m.creator_id = $1
               AND m.deleted_at IS NULL
               AND r.deleted_at IS NULL
             ORDER BY m.room_id, m.id"#,
            user_id.as_i64(),
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in media_rows {
            let room_id = row.room_id;
            if owned_room_ids.contains(&room_id) {
                continue;
            }
            entries_by_room
                .entry(room_id)
                .or_default()
                .media_ids
                .push(row.id);
        }

        Ok(entries_by_room)
    }

    pub(super) async fn cleanup_transactional_user_resources(
        &self,
        user_id: &UserId,
        deleted_room_outbox_events: &HashMap<RoomId, NewRealtimeOutboxEvent>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(
        UserDeletionCleanup,
        Vec<RoomId>,
        Vec<RoomId>,
        Vec<UserDeletedChatMessage>,
        Vec<UserDeletedRoomImpact>,
    )> {
        let owned_room_ids = self.query_owned_room_ids_in_tx(user_id, tx).await?;
        let owned_room_id_set: HashSet<RoomId> = owned_room_ids.iter().copied().collect();
        let membership_room_ids = self
            .query_membership_room_ids_in_tx(user_id, &owned_room_id_set, tx)
            .await?;
        let entries_by_room = self
            .query_owned_room_entries_in_tx(user_id, &owned_room_id_set, tx)
            .await?;

        let mut modified_rooms = Vec::new();
        let mut pending_permission_fences = Vec::new();
        let mut deleted_playlists = 0usize;
        let mut deleted_media = 0usize;
        let mut playback_resets = 0usize;

        let cleanup_result: Result<_> = async {
            let mut memberships_removed = Vec::new();
            let mut modified_room_ids: Vec<RoomId> = entries_by_room.keys().copied().collect();
            modified_room_ids.sort_unstable();
            for room_id in modified_room_ids {
                let Some(entries) = entries_by_room.get(&room_id) else {
                    continue;
                };
                deleted_playlists += entries.playlist_ids.len();
                let impact = self
                    .delete_owned_entries_in_room_in_tx(
                        user_id,
                        &room_id,
                        entries.playlist_ids.clone(),
                        entries.media_ids.clone(),
                        tx,
                    )
                    .await?;
                deleted_media += impact.deleted_media_ids.len();
                if impact.playback_reset {
                    playback_resets += 1;
                }
                modified_rooms.push(impact);
            }

            let owned_room_permission_fences = self
                .reserve_permission_fences_for_rooms(&owned_room_ids, tx)
                .await?;
            pending_permission_fences.extend(owned_room_permission_fences);

            for room_id in &owned_room_ids {
                let impact =
                    crate::service::soft_delete_room_and_cleanup_in_tx(
                        tx,
                        room_id,
                        DeletionSource::Account,
                    )
                    .await?;
                sqlx::query!(
                    "UPDATE rooms SET deletion_source = $3, deleted_owner_id = $2 WHERE id = $1",
                    room_id.as_i64(),
                    user_id.as_i64(),
                    DeletionSource::Account as DeletionSource,
                )
                .execute(&mut **tx)
                .await?;
                let playlist_id_strs: Vec<i64> = impact
                    .deleted_playlist_ids
                    .iter()
                    .map(PlaylistId::as_i64)
                    .collect();
                if !playlist_id_strs.is_empty() {
                    sqlx::query!(
                        "UPDATE playlists SET deletion_source = $3, deleted_owner_id = $2 WHERE id = ANY($1) AND deleted_at IS NOT NULL",
                        &playlist_id_strs,
                        user_id.as_i64(),
                        DeletionSource::Account as DeletionSource,
                    )
                    .execute(&mut **tx)
                    .await?;
                }
                let media_id_strs: Vec<i64> = impact
                    .deleted_media_ids
                    .iter()
                    .map(MediaId::as_i64)
                    .collect();
                if !media_id_strs.is_empty() {
                    sqlx::query!(
                        "UPDATE media SET deletion_source = $3, deleted_owner_id = $2 WHERE id = ANY($1) AND deleted_at IS NOT NULL",
                        &media_id_strs,
                        user_id.as_i64(),
                        DeletionSource::Account as DeletionSource,
                    )
                    .execute(&mut **tx)
                    .await?;
                }
                if impact.chat_deleted > 0 {
                    let Some(deletion_timestamp) = impact.deletion_timestamp else {
                        return Err(crate::Error::Internal(
                            "room deletion did not return a deletion timestamp".to_string(),
                        ));
                    };
                    sqlx::query!(
                        "UPDATE chat_messages SET deletion_source = $4, deleted_owner_id = $2 WHERE room_id = $1 AND deleted_at = $3 AND deletion_source = $5 AND deleted_owner_id IS NULL",
                        room_id.as_i64(),
                        user_id.as_i64(),
                        deletion_timestamp,
                        DeletionSource::Account as DeletionSource,
                        DeletionSource::Room as DeletionSource,
                    )
                    .execute(&mut **tx)
                    .await?;
                }
                self.insert_deleted_room_outbox_tx(tx, room_id, deleted_room_outbox_events)
                    .await?;
                deleted_playlists += impact.deleted_playlist_ids.len();
                deleted_media += impact.deleted_media_ids.len();
                if impact.playback_rows_deleted > 0 {
                    playback_resets += 1;
                }
                memberships_removed.extend(impact.removed_members);
            }

            let oauth_mappings_deleted = sqlx::query!(
                "UPDATE auth_oauth2_identities SET deleted_at = CURRENT_TIMESTAMP, deletion_source = $2 WHERE user_id = $1 AND deleted_at IS NULL",
                user_id.as_i64(),
                DeletionSource::Account as DeletionSource,
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            let email_tokens_deleted = sqlx::query!(
                "DELETE FROM auth_email_tokens WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            let email_bind_requests_deleted = self
                .email_bind_repository
                .delete_unused_for_user_with_executor(user_id, &mut **tx)
                .await?;

            let email_identities_deleted = sqlx::query!(
                "UPDATE auth_email_identities SET deleted_at = CURRENT_TIMESTAMP, deletion_source = $2, updated_at = CURRENT_TIMESTAMP WHERE user_id = $1 AND deleted_at IS NULL",
                user_id.as_i64(),
                DeletionSource::Account as DeletionSource,
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            let provider_credentials_deleted = sqlx::query!(
                "DELETE FROM user_media_provider_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            let notifications_deleted = sqlx::query!(
                "DELETE FROM notifications WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            let mut ban_actor_references_cleared = sqlx::query!(
                "UPDATE user_bans SET banned_by = NULL WHERE banned_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();
            ban_actor_references_cleared += sqlx::query!(
                "UPDATE user_bans SET revoked_by = NULL WHERE revoked_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();
            ban_actor_references_cleared += sqlx::query!(
                "UPDATE room_bans SET banned_by = NULL WHERE banned_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();
            ban_actor_references_cleared += sqlx::query!(
                "UPDATE room_bans SET revoked_by = NULL WHERE revoked_by = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            // Messages authored in surviving rooms belong to the account
            // recovery aggregate as well. Keep their body and author for
            // restoration, while account-level visibility filters hide them
            // until the account is restored or permanently purged.
            let deleted_chat_messages = sqlx::query_as!(
                AccountDeletedChatMessageRow,
                r#"
                UPDATE chat_messages
                SET deleted_at = CURRENT_TIMESTAMP,
                    deletion_source = $2,
                    deleted_owner_id = $1,
                    version = version + 1
                WHERE user_id = $1
                  AND deleted_at IS NULL
                RETURNING id, room_id AS "room_id!: RoomId", message_type AS "message_type!: ChatMessageType", version, created_at, deleted_at
                "#,
                user_id.as_i64(),
                DeletionSource::Account as DeletionSource,
            )
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|row| {
                let deleted_at = row.deleted_at.ok_or_else(|| {
                    crate::Error::Internal(
                        "account-deleted chat message did not return deleted_at".to_string(),
                    )
                })?;
                Ok(UserDeletedChatMessage {
                    room_id: row.room_id,
                    message_id: row.id,
                    message_created_at: row.created_at,
                    message_type: row.message_type,
                    version: row.version,
                    deleted_at,
                })
            })
            .collect::<Result<Vec<_>>>()?;
            let chat_messages_soft_deleted = deleted_chat_messages.len() as u64;

            let room_member_repo = RoomMemberRepository::new(self.repository.pool().clone());
            let (user_memberships_removed, permission_fences) = self
                .remove_user_memberships_with_permission_fences(&room_member_repo, user_id, tx)
                .await?;
            pending_permission_fences.extend(permission_fences);
            memberships_removed.extend(user_memberships_removed);

            Ok((
                oauth_mappings_deleted,
                email_tokens_deleted,
                email_bind_requests_deleted,
                email_identities_deleted,
                provider_credentials_deleted,
                notifications_deleted,
                ban_actor_references_cleared,
                chat_messages_soft_deleted,
                deleted_chat_messages,
                memberships_removed,
            ))
        }
        .await;

        let (
            oauth_mappings_deleted,
            email_tokens_deleted,
            email_bind_requests_deleted,
            email_identities_deleted,
            provider_credentials_deleted,
            notifications_deleted,
            ban_actor_references_cleared,
            chat_messages_soft_deleted,
            deleted_chat_messages,
            memberships_removed,
        ) = match cleanup_result {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_playback_reset_fences(&modified_rooms).await;
                self.abort_removed_member_permission_fences(&pending_permission_fences)
                    .await;
                return Err(error);
            }
        };
        let memberships_removed_count = memberships_removed.len() as u64;

        Ok((
            UserDeletionCleanup {
                stats: UserDeletionCleanupStats {
                    oauth_mappings_deleted,
                    email_identities_deleted,
                    email_tokens_deleted,
                    email_bind_requests_deleted,
                    provider_credentials_deleted,
                    notifications_deleted,
                    ban_actor_references_cleared,
                    chat_messages_soft_deleted,
                    memberships_removed: memberships_removed_count,
                    deleted_rooms: owned_room_ids.len(),
                    deleted_playlists,
                    deleted_media,
                    playback_resets,
                },
                removed_members: memberships_removed,
                pending_permission_fences,
            },
            owned_room_ids,
            membership_room_ids,
            deleted_chat_messages,
            modified_rooms,
        ))
    }
}
