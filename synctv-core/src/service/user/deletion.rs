use std::collections::HashMap;

use crate::{
    models::{MediaId, RoomId, RoomPlaybackState, User, UserId},
    repository::{realtime_outbox::NewRealtimeOutboxEvent, RoomMemberRepository},
    Error, Result,
};

use super::UserService;
use permissions::PendingRemovedMemberFence;
use playback::PendingPlaybackResetFence;

mod entries;
mod permissions;
mod playback;
mod resources;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserDeletedRoomImpact {
    pub room_id: RoomId,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
    playback_fence: Option<PendingPlaybackResetFence>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserDeletionSummary {
    pub user_id: UserId,
    pub username: String,
    pub deleted_room_ids: Vec<RoomId>,
    pub membership_room_ids: Vec<RoomId>,
    pub modified_rooms: Vec<UserDeletedRoomImpact>,
}

#[derive(Debug, Clone, Default)]
struct UserDeletionCleanupStats {
    oauth_mappings_deleted: u64,
    email_identities_deleted: u64,
    email_tokens_deleted: u64,
    provider_credentials_deleted: u64,
    notifications_deleted: u64,
    ban_actor_references_cleared: u64,
    chat_messages_anonymized: u64,
    memberships_removed: u64,
    deleted_rooms: usize,
    deleted_playlists: usize,
    deleted_media: usize,
    playback_resets: usize,
}

#[derive(Debug, Clone, Default)]
struct UserDeletionCleanup {
    stats: UserDeletionCleanupStats,
    removed_members: Vec<crate::repository::room_member::RemovedRoomMember>,
    pending_permission_fences: Vec<PendingRemovedMemberFence>,
}

impl UserService {
    /// Close the currently authenticated user's own account.
    pub async fn close_account(&self, user_id: &UserId) -> Result<()> {
        self.delete_user(user_id).await
    }

    /// Soft-delete a user and clean up all related resources.
    ///
    /// Performs the following cleanup in order:
    /// 1. Within a single DB transaction:
    ///    a. Delete rooms owned by the user
    ///    b. Delete playlists/media created by the user in surviving rooms
    ///    c. Reset playback state in affected rooms when deleted entries are currently playing
    ///    d. Delete user-scoped ancillary rows and anonymize surviving chat messages
    ///    e. Mark all remaining room memberships as `Left`
    ///    f. Soft-delete the user row
    /// 2. Reset username-scoped auth/rate-limit state (best-effort)
    /// 3. Invalidate username cache (best-effort)
    /// 4. Invalidate user cache across replicas (best-effort)
    ///
    /// Step 1 is atomic: if any cleanup fails, the soft-delete is rolled back to
    /// prevent partially-deleted users with orphaned state.
    ///
    /// **Token Invalidation**: Tokens are invalidated implicitly because the
    /// security pipeline checks for deleted users (`deleted_at` IS NOT NULL).
    pub async fn delete_user_with_summary(&self, user_id: &UserId) -> Result<UserDeletionSummary> {
        self.delete_user_with_summary_and_outbox(user_id, HashMap::new())
            .await
    }

    pub async fn delete_user_with_summary_and_outbox(
        &self,
        user_id: &UserId,
        deleted_room_outbox_events: HashMap<RoomId, NewRealtimeOutboxEvent>,
    ) -> Result<UserDeletionSummary> {
        // 1. Transactional DB cleanup + soft-delete
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        let user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?;
        let Some(user) = user else {
            return Err(Error::InvalidInput("User is already deleted".to_string()));
        };

        let (cleanup, deleted_room_ids, membership_room_ids, mut modified_rooms) = self
            .cleanup_transactional_user_resources(user_id, &deleted_room_outbox_events, &mut tx)
            .await?;

        let deleted = match self
            .repository
            .delete_with_executor(user_id, &mut *tx)
            .await
        {
            Ok(deleted) => deleted,
            Err(error) => {
                self.abort_playback_reset_fences(&modified_rooms).await;
                self.abort_removed_member_permission_fences(&cleanup.pending_permission_fences)
                    .await;
                return Err(error);
            }
        };
        if !deleted {
            self.abort_playback_reset_fences(&modified_rooms).await;
            self.abort_removed_member_permission_fences(&cleanup.pending_permission_fences)
                .await;
            return Err(Error::InvalidInput("User is already deleted".to_string()));
        }

        if let Err(error) = tx.commit().await {
            self.abort_playback_reset_fences(&modified_rooms).await;
            self.abort_removed_member_permission_fences(&cleanup.pending_permission_fences)
                .await;
            return Err(error.into());
        }
        let cleanup_stats = cleanup.stats;
        if let Err(error) = self.commit_playback_reset_fences(&modified_rooms).await {
            tracing::warn!(
                error = %error,
                user_id = %user_id,
                "Failed to finalize playback fences after committed user deletion; continuing post-commit cleanup"
            );
        }
        if let Err(error) = self
            .commit_removed_member_permission_fences(
                cleanup.pending_permission_fences,
                &cleanup.removed_members,
            )
            .await
        {
            tracing::warn!(
                error = %error,
                user_id = %user_id,
                "Failed to finalize removed member permission fences after committed user deletion; continuing post-commit cleanup"
            );
        }
        self.invalidate_removed_member_permission_caches(&cleanup.removed_members)
            .await;

        // 2. Reset username/user-scoped auth and rate-limit state (best-effort).
        if let Err(e) = self.brute_force.reset(&user.username).await {
            tracing::warn!(
                error = %e,
                user_id = %user_id,
                username = %user.username,
                "Failed to reset brute-force state during user deletion"
            );
        }
        let refresh_rate_limit_key = format!("refresh:{user_id}");
        if let Err(e) = self
            .refresh_rate_limiter
            .reset(&refresh_rate_limit_key)
            .await
        {
            tracing::warn!(
                error = %e,
                user_id = %user_id,
                "Failed to reset refresh rate limit state during user deletion"
            );
        }

        // 3. Invalidate username cache (best-effort)
        if let Err(e) = self.invalidate_username_cache(user_id).await {
            tracing::warn!(
                error = %e,
                user_id = %user_id,
                "Failed to invalidate username cache during user deletion"
            );
        }

        // 4. Invalidate user cache across replicas (best-effort)
        self.notify_user_invalidation(user_id).await;

        tracing::info!(
            user_id = %user_id,
            username = %user.username,
            oauth_mappings_deleted = cleanup_stats.oauth_mappings_deleted,
            email_identities_deleted = cleanup_stats.email_identities_deleted,
            email_tokens_deleted = cleanup_stats.email_tokens_deleted,
            provider_credentials_deleted = cleanup_stats.provider_credentials_deleted,
            notifications_deleted = cleanup_stats.notifications_deleted,
            ban_actor_references_cleared = cleanup_stats.ban_actor_references_cleared,
            chat_messages_anonymized = cleanup_stats.chat_messages_anonymized,
            memberships_removed = cleanup_stats.memberships_removed,
            deleted_rooms = cleanup_stats.deleted_rooms,
            deleted_playlists = cleanup_stats.deleted_playlists,
            deleted_media = cleanup_stats.deleted_media,
            playback_resets = cleanup_stats.playback_resets,
            "User soft-deleted with transactional resource cleanup"
        );

        modified_rooms.sort_by_key(|room| room.room_id);

        Ok(UserDeletionSummary {
            user_id: user.id,
            username: user.username,
            deleted_room_ids,
            membership_room_ids,
            modified_rooms,
        })
    }

    pub async fn delete_user(&self, user_id: &UserId) -> Result<()> {
        self.delete_user_with_summary(user_id).await.map(|_| ())
    }

    pub async fn is_user_banned(&self, user_id: &UserId) -> Result<bool> {
        self.repository.is_banned(user_id).await
    }

    /// Clear a global user ban without changing the user's account facts.
    pub async fn unban_user(&self, user_id: &UserId) -> Result<User> {
        let updated = self.repository.unban(user_id).await?;
        self.notify_user_invalidation(user_id).await;
        Ok(updated)
    }

    /// Ban a user and remove them from all room memberships in the same transaction.
    ///
    /// Ban is independent moderation state. The user's lifecycle status is
    /// preserved so unban does not implicitly approve or reactivate accounts.
    pub async fn ban_user_and_cleanup_memberships(
        &self,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<User> {
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        self.repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        if self.repository.is_banned(user_id).await? {
            return Err(Error::InvalidInput("User is already banned".to_string()));
        }

        self.repository
            .insert_ban_with_executor(user_id, banned_by, reason, &mut *tx)
            .await?;

        let room_member_repo = RoomMemberRepository::new(pool.clone());
        let owned_room_ids = self.query_owned_room_ids_in_tx(user_id, &mut tx).await?;
        let mut pending_permission_fences = self
            .reserve_permission_fences_for_rooms(&owned_room_ids, &mut tx)
            .await?;
        let mut removed_members = match room_member_repo
            .remove_all_for_rooms_with_executor(&owned_room_ids, &mut tx)
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                self.abort_removed_member_permission_fences(&pending_permission_fences)
                    .await;
                return Err(error);
            }
        };
        let (mut banned_user_removed_members, banned_user_permission_fences) = match self
            .remove_user_memberships_with_permission_fences(&room_member_repo, user_id, &mut tx)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.abort_removed_member_permission_fences(&pending_permission_fences)
                    .await;
                return Err(error);
            }
        };
        pending_permission_fences.extend(banned_user_permission_fences);
        removed_members.append(&mut banned_user_removed_members);

        if let Err(error) = tx.commit().await {
            self.abort_removed_member_permission_fences(&pending_permission_fences)
                .await;
            return Err(error.into());
        }
        if let Err(error) = self
            .commit_removed_member_permission_fences(pending_permission_fences, &removed_members)
            .await
        {
            tracing::warn!(
                error = %error,
                user_id = %user_id,
                "Failed to finalize removed member permission fences after committed user ban cleanup; continuing post-commit cleanup"
            );
        }
        self.invalidate_removed_member_permission_caches(&removed_members)
            .await;
        let updated = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        self.notify_user_invalidation(user_id).await;

        Ok(updated)
    }

    // Batch Operations

    /// Maximum number of items allowed in a batch operation
    pub const BATCH_SIZE_LIMIT: usize = 100;

    /// Batch delete multiple users.
    ///
    /// Each user is processed individually - if one user fails, others may still succeed.
    /// Returns per-user results with success/failure status.
    ///
    /// # Errors
    /// - `InvalidInput` if `user_ids` is empty or exceeds `BATCH_SIZE_LIMIT`
    pub async fn batch_delete_users(
        &self,
        user_ids: &[UserId],
    ) -> Result<Vec<(UserId, Result<()>)>> {
        Self::validate_batch_delete_users(user_ids)?;

        let mut results = Vec::with_capacity(user_ids.len());

        for user_id in user_ids {
            let result = self.delete_user(user_id).await;
            results.push((*user_id, result));
        }

        Ok(results)
    }

    fn validate_batch_delete_users(user_ids: &[UserId]) -> Result<()> {
        if user_ids.is_empty() {
            return Err(Error::InvalidInput("user_ids cannot be empty".to_string()));
        }
        if user_ids.len() > Self::BATCH_SIZE_LIMIT {
            return Err(Error::InvalidInput(format!(
                "Batch size {} exceeds limit of {}",
                user_ids.len(),
                Self::BATCH_SIZE_LIMIT
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_delete_users_rejects_empty_input() {
        let err = UserService::validate_batch_delete_users(&[])
            .expect_err("empty batch should be rejected");

        assert!(matches!(err, Error::InvalidInput(message) if message.contains("empty")));
    }

    #[test]
    fn batch_delete_users_rejects_oversized_input() {
        let user_ids: Vec<UserId> = (0..=UserService::BATCH_SIZE_LIMIT)
            .map(|i| UserId::expect_positive(i64::try_from(i + 1).expect("test id fits i64")))
            .collect();

        let err = UserService::validate_batch_delete_users(&user_ids)
            .expect_err("oversized batch should be rejected");

        assert!(matches!(err, Error::InvalidInput(message) if message.contains("exceeds limit")));
    }
}
