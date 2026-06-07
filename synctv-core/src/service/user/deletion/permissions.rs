use std::collections::HashMap;

use sqlx::{Postgres, Transaction};

use crate::{
    models::{RoomId, UserId},
    repository::RoomMemberRepository,
    service::permission::{PermissionService, PermissionWriteFence},
    Result,
};

use super::UserService;

#[derive(Debug, Clone)]
pub(super) struct PendingRemovedMemberFence {
    room_id: RoomId,
    user_id: UserId,
    fence: PermissionWriteFence,
}

#[derive(Debug, Default)]
struct PendingRemovedMemberFences {
    inner: Vec<PendingRemovedMemberFence>,
}

impl PendingRemovedMemberFences {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Vec::with_capacity(capacity),
        }
    }

    fn push(&mut self, fence: PendingRemovedMemberFence) {
        self.inner.push(fence);
    }

    fn into_vec(self) -> Vec<PendingRemovedMemberFence> {
        self.inner
    }

    async fn abort_all(&self, permission_service: &PermissionService) {
        for pending in &self.inner {
            permission_service
                .abort_permission_write(&pending.fence)
                .await;
        }
    }
}

impl UserService {
    pub(super) async fn remove_user_memberships_with_permission_fences(
        &self,
        room_member_repo: &RoomMemberRepository,
        user_id: &UserId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(
        Vec<crate::repository::room_member::RemovedRoomMember>,
        Vec<PendingRemovedMemberFence>,
    )> {
        let pending_permission_fences = if let Some(permission_service) = &self.permission_service {
            let members = sqlx::query!(
                r#"SELECT room_id as "room_id: RoomId",
                          user_id as "user_id: UserId",
                          version
                 FROM room_members
                 WHERE user_id = $1
                 FOR UPDATE"#,
                user_id as &UserId,
            )
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|row| (row.room_id, row.user_id, row.version))
            .collect::<Vec<_>>();

            let mut fences = PendingRemovedMemberFences::with_capacity(members.len());
            for (room_id, member_user_id, version) in members {
                let fence = match permission_service
                    .begin_permission_write(&room_id, &member_user_id, version)
                    .await
                {
                    Ok(fence) => fence,
                    Err(error) => {
                        fences.abort_all(permission_service).await;
                        return Err(error);
                    }
                };
                fences.push(PendingRemovedMemberFence {
                    room_id,
                    user_id: member_user_id,
                    fence,
                });
            }
            fences.into_vec()
        } else {
            Vec::new()
        };

        let removed = match room_member_repo
            .remove_all_for_user_with_executor(user_id, tx)
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                self.abort_removed_member_permission_fences(&pending_permission_fences)
                    .await;
                return Err(error);
            }
        };

        Ok((removed, pending_permission_fences))
    }

    pub(super) async fn reserve_permission_fences_for_rooms(
        &self,
        room_ids: &[RoomId],
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<PendingRemovedMemberFence>> {
        let Some(permission_service) = &self.permission_service else {
            return Ok(Vec::new());
        };
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        let room_id_strs: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();
        let members = sqlx::query!(
            r#"SELECT room_id as "room_id: RoomId",
                      user_id as "user_id: UserId",
                      version
             FROM room_members
             WHERE room_id = ANY($1)
             FOR UPDATE"#,
            &room_id_strs,
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|row| (row.room_id, row.user_id, row.version))
        .collect::<Vec<_>>();

        let mut fences = PendingRemovedMemberFences::with_capacity(members.len());
        for (room_id, member_user_id, version) in members {
            let fence = match permission_service
                .begin_permission_write(&room_id, &member_user_id, version)
                .await
            {
                Ok(fence) => fence,
                Err(error) => {
                    fences.abort_all(permission_service).await;
                    return Err(error);
                }
            };
            fences.push(PendingRemovedMemberFence {
                room_id,
                user_id: member_user_id,
                fence,
            });
        }

        Ok(fences.into_vec())
    }

    pub(super) async fn commit_removed_member_permission_fences(
        &self,
        pending_fences: Vec<PendingRemovedMemberFence>,
        removed_members: &[crate::repository::room_member::RemovedRoomMember],
    ) -> Result<()> {
        let Some(permission_service) = &self.permission_service else {
            return Ok(());
        };

        let removed_versions = removed_members
            .iter()
            .map(|member| ((member.room_id, member.user_id), member.version))
            .collect::<HashMap<_, _>>();

        let mut first_error = None;
        for pending in pending_fences {
            let Some(version) = removed_versions.get(&(pending.room_id, pending.user_id)) else {
                permission_service
                    .abort_permission_write(&pending.fence)
                    .await;
                continue;
            };
            if let Err(error) = permission_service
                .commit_permission_write(&pending.fence, *version)
                .await
            {
                tracing::warn!(
                    error = %error,
                    room_id = %pending.room_id,
                    user_id = %pending.user_id,
                    "Failed to finalize removed member permission fence"
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(super) async fn abort_removed_member_permission_fences(
        &self,
        pending_fences: &[PendingRemovedMemberFence],
    ) {
        let Some(permission_service) = &self.permission_service else {
            return;
        };

        for pending in pending_fences {
            permission_service
                .abort_permission_write(&pending.fence)
                .await;
        }
    }

    pub(super) async fn invalidate_removed_member_permission_caches(
        &self,
        removed_members: &[crate::repository::room_member::RemovedRoomMember],
    ) {
        let Some(permission_service) = &self.permission_service else {
            return;
        };

        for member in removed_members {
            permission_service
                .invalidate_removed_member_cache(&member.room_id, &member.user_id)
                .await;
        }
    }
}
