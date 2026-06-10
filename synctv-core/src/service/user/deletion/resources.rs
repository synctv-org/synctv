use std::collections::{HashMap, HashSet};

use sqlx::{Postgres, Transaction};

use crate::{
    models::{MediaId, PlaylistId, RoomId, UserId},
    repository::{realtime_outbox::NewRealtimeOutboxEvent, RoomMemberRepository},
    Result,
};

use super::{UserDeletedRoomImpact, UserDeletionCleanup, UserDeletionCleanupStats, UserService};

#[derive(Debug, Default)]
pub(super) struct UserOwnedRoomEntries {
    pub(super) playlist_ids: Vec<PlaylistId>,
    pub(super) media_ids: Vec<MediaId>,
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
                    crate::service::room::soft_delete_room_and_cleanup_in_tx(tx, room_id).await?;
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
                "DELETE FROM auth_oauth2_identities WHERE user_id = $1",
                user_id.as_i64(),
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

            let email_identities_deleted = sqlx::query!(
                "DELETE FROM auth_email_identities WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            sqlx::query!(
                "DELETE FROM auth_password_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?;

            sqlx::query!(
                "DELETE FROM auth_webauthn_credentials WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?;

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

            let chat_messages_anonymized = sqlx::query!(
                "UPDATE chat_messages SET user_id = NULL WHERE user_id = $1",
                user_id.as_i64(),
            )
            .execute(&mut **tx)
            .await?
            .rows_affected();

            let room_member_repo = RoomMemberRepository::new(self.repository.pool().clone());
            let (user_memberships_removed, permission_fences) = self
                .remove_user_memberships_with_permission_fences(&room_member_repo, user_id, tx)
                .await?;
            pending_permission_fences.extend(permission_fences);
            memberships_removed.extend(user_memberships_removed);

            Ok((
                oauth_mappings_deleted,
                email_tokens_deleted,
                email_identities_deleted,
                provider_credentials_deleted,
                notifications_deleted,
                ban_actor_references_cleared,
                chat_messages_anonymized,
                memberships_removed,
            ))
        }
        .await;

        let (
            oauth_mappings_deleted,
            email_tokens_deleted,
            email_identities_deleted,
            provider_credentials_deleted,
            notifications_deleted,
            ban_actor_references_cleared,
            chat_messages_anonymized,
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
                    provider_credentials_deleted,
                    notifications_deleted,
                    ban_actor_references_cleared,
                    chat_messages_anonymized,
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
            modified_rooms,
        ))
    }
}
