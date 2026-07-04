//! RAII guard for room member permission fences
//!
//! Provides explicit async abort/commit paths for room-member permission fences.
//! Drop remains a best-effort fallback for unexpected early exits.

use crate::{
    models::{RoomId, UserId},
    repository::room_member::RemovedRoomMember,
    service::PermissionWriteFence,
    Result,
};
use std::sync::Arc;

/// A single pending permission fence for a room member
#[derive(Debug)]
pub(crate) struct PendingRoomMemberPermissionFence {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub fence: PermissionWriteFence,
}

/// Guard for room deletion permission fences
///
/// Callers should use `abort().await` on transaction error paths before
/// returning, and `commit().await` after a successful database commit.
///
/// # Example
///
/// ```ignore
/// let guard = PermissionFenceGuard::reserve(room_service, &room_id, &mut tx).await?;
///
/// // ... perform database operations ...
/// // If any error occurs here, abort before returning.
///
/// tx.commit().await?;
/// guard.commit(&removed_members).await?;
/// ```
pub(super) struct PermissionFenceGuard {
    fences: Option<Vec<PendingRoomMemberPermissionFence>>,
    room_service: Arc<super::RoomService>,
}

impl PermissionFenceGuard {
    /// Reserve permission fences for all members of a room
    ///
    /// This method locks all room members in the database transaction and
    /// reserves permission fences for them. The guard will automatically
    /// abort these fences if dropped without calling `commit()`.
    pub async fn reserve(
        room_service: Arc<super::RoomService>,
        room_id: &RoomId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Self> {
        let fences = room_service
            .reserve_room_member_permission_fences(room_id, tx)
            .await?;

        Ok(Self {
            fences: Some(fences),
            room_service,
        })
    }

    /// Commit the permission fences after successful transaction
    ///
    /// This prevents the Drop implementation from aborting the fences.
    /// Should be called after the database transaction has been committed.
    pub async fn commit(mut self, removed_members: &[RemovedRoomMember]) -> Result<()> {
        let fences = self.fences.take().expect("fences already consumed");

        self.room_service
            .commit_removed_room_member_permission_fences(fences, removed_members)
            .await
    }

    /// Abort the permission fences before returning from a failed transaction path.
    pub async fn abort(mut self) {
        if let Some(fences) = self.fences.take() {
            let count = fences.len();
            self.room_service
                .abort_room_member_permission_fences(&fences)
                .await;
            tracing::debug!(count, "Aborted room deletion permission fences");
        }
    }
}

impl Drop for PermissionFenceGuard {
    fn drop(&mut self) {
        if let Some(fences) = self.fences.take() {
            let count = fences.len();
            tracing::warn!(
                count,
                "PermissionFenceGuard dropped without explicit commit or abort; aborting fences asynchronously"
            );
            let room_service = self.room_service.clone();
            tokio::spawn(async move {
                room_service
                    .abort_room_member_permission_fences(&fences)
                    .await;
                tracing::debug!(
                    count,
                    "Auto-aborted room deletion permission fences from Drop fallback"
                );
            });
        }
    }
}
