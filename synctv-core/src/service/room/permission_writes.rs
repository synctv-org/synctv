use std::collections::HashMap;

use sqlx::{Postgres, Transaction};

use crate::{
    models::{RoomId, RoomMember, RoomRole, UserId},
    repository::room_member::{
        MemberPermissionExactVersionUpdate, MemberRolePermissionExactVersionUpdate,
        RemovedRoomMember,
    },
    service::{PermissionWriteFence, RoomService},
    Result,
};

use super::permission_fence_guard::PendingRoomMemberPermissionFence;

pub(super) struct PermissionWriteParams<'a> {
    pub room_id: &'a RoomId,
    pub user_id: &'a UserId,
    pub fence: &'a PermissionWriteFence,
    pub effective_is_admin: bool,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub current_version: i64,
}

pub(super) struct MemberRoleWriteParams<'a> {
    pub room_id: &'a RoomId,
    pub user_id: &'a UserId,
    pub fence: &'a PermissionWriteFence,
    pub role: RoomRole,
    pub current_version: i64,
}

pub(super) struct MemberRolePermissionWriteParams<'a> {
    pub room_id: &'a RoomId,
    pub user_id: &'a UserId,
    pub fence: &'a PermissionWriteFence,
    pub role: RoomRole,
    pub effective_is_admin: bool,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub current_version: i64,
}

impl RoomService {
    pub(super) async fn finalize_committed_permission_write_best_effort(
        &self,
        fence: &PermissionWriteFence,
        room_id: &RoomId,
        user_id: &UserId,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self.commit_permission_write(fence, version).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                user_id = %user_id,
                version,
                operation,
                "Failed to finalize permission fence after committed room/member write"
            );
        }
    }

    pub(super) async fn begin_permission_write(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        db_version: i64,
    ) -> Result<PermissionWriteFence> {
        self.permission_service
            .begin_permission_write(room_id, user_id, db_version)
            .await
    }

    async fn commit_permission_write(
        &self,
        fence: &PermissionWriteFence,
        version: i64,
    ) -> Result<()> {
        self.permission_service
            .commit_permission_write(fence, version)
            .await
    }

    pub(super) async fn abort_permission_write(&self, fence: &PermissionWriteFence) {
        self.permission_service.abort_permission_write(fence).await;
    }

    pub(super) async fn apply_permission_write_with_fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        params: PermissionWriteParams<'_>,
    ) -> Result<RoomMember> {
        let (added, removed) = if params.effective_is_admin {
            (
                params.admin_added_permissions,
                params.admin_removed_permissions,
            )
        } else {
            (params.added_permissions, params.removed_permissions)
        };

        if params.effective_is_admin {
            if params.fence.version() > 0 {
                self.member_repo
                    .update_admin_permissions_with_exact_version_executor(
                        MemberPermissionExactVersionUpdate {
                            room_id: params.room_id,
                            user_id: params.user_id,
                            added_permissions: added,
                            removed_permissions: removed,
                            current_version: params.current_version,
                            new_version: params.fence.version(),
                        },
                        &mut **tx,
                    )
                    .await
            } else {
                self.member_repo
                    .update_admin_permissions_with_executor(
                        params.room_id,
                        params.user_id,
                        added,
                        removed,
                        params.current_version,
                        &mut **tx,
                    )
                    .await
            }
        } else if params.fence.version() > 0 {
            self.member_repo
                .update_permissions_with_exact_version_executor(
                    MemberPermissionExactVersionUpdate {
                        room_id: params.room_id,
                        user_id: params.user_id,
                        added_permissions: added,
                        removed_permissions: removed,
                        current_version: params.current_version,
                        new_version: params.fence.version(),
                    },
                    &mut **tx,
                )
                .await
        } else {
            self.member_repo
                .update_permissions_with_executor(
                    params.room_id,
                    params.user_id,
                    added,
                    removed,
                    params.current_version,
                    &mut **tx,
                )
                .await
        }
    }

    pub(super) async fn apply_permission_write_or_abort_reserved_fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        params: PermissionWriteParams<'_>,
    ) -> Result<RoomMember> {
        let fence = params.fence;
        let abort_on_error = fence.version() > 0;
        match self.apply_permission_write_with_fence(tx, params).await {
            Ok(updated) => Ok(updated),
            Err(error) => {
                if abort_on_error {
                    self.abort_permission_write(fence).await;
                }
                Err(error)
            }
        }
    }

    pub(super) async fn apply_member_role_write_with_fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        params: MemberRoleWriteParams<'_>,
    ) -> Result<RoomMember> {
        if params.fence.version() > 0 {
            self.member_repo
                .update_role_with_exact_version_executor(
                    params.room_id,
                    params.user_id,
                    params.role,
                    params.current_version,
                    params.fence.version(),
                    &mut **tx,
                )
                .await
        } else {
            self.member_repo
                .update_role_with_version_executor(
                    params.room_id,
                    params.user_id,
                    params.role,
                    params.current_version,
                    &mut **tx,
                )
                .await
        }
    }

    pub(super) async fn apply_member_role_write_or_abort_reserved_fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        params: MemberRoleWriteParams<'_>,
    ) -> Result<RoomMember> {
        let fence = params.fence;
        let abort_on_error = fence.version() > 0;
        match self.apply_member_role_write_with_fence(tx, params).await {
            Ok(updated) => Ok(updated),
            Err(error) => {
                if abort_on_error {
                    self.abort_permission_write(fence).await;
                }
                Err(error)
            }
        }
    }

    pub(super) async fn apply_member_role_permission_write_with_fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        params: MemberRolePermissionWriteParams<'_>,
    ) -> Result<RoomMember> {
        let (added, removed) = if params.effective_is_admin {
            (
                params.admin_added_permissions,
                params.admin_removed_permissions,
            )
        } else {
            (params.added_permissions, params.removed_permissions)
        };

        if params.fence.version() > 0 {
            self.member_repo
                .update_role_and_permissions_with_exact_version_executor(
                    MemberRolePermissionExactVersionUpdate {
                        room_id: params.room_id,
                        user_id: params.user_id,
                        role: params.role,
                        added_permissions: added,
                        removed_permissions: removed,
                        use_admin_permissions: params.effective_is_admin,
                        current_version: params.current_version,
                        new_version: params.fence.version(),
                    },
                    &mut **tx,
                )
                .await
        } else {
            let updated_role = self
                .member_repo
                .update_role_with_version_executor(
                    params.room_id,
                    params.user_id,
                    params.role,
                    params.current_version,
                    &mut **tx,
                )
                .await?;
            if params.effective_is_admin {
                self.member_repo
                    .update_admin_permissions_with_executor(
                        params.room_id,
                        params.user_id,
                        added,
                        removed,
                        updated_role.version,
                        &mut **tx,
                    )
                    .await
            } else {
                self.member_repo
                    .update_permissions_with_executor(
                        params.room_id,
                        params.user_id,
                        added,
                        removed,
                        updated_role.version,
                        &mut **tx,
                    )
                    .await
            }
        }
    }

    pub(super) async fn apply_member_role_permission_write_or_abort_reserved_fence(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        params: MemberRolePermissionWriteParams<'_>,
    ) -> Result<RoomMember> {
        let fence = params.fence;
        let abort_on_error = fence.version() > 0;
        match self
            .apply_member_role_permission_write_with_fence(tx, params)
            .await
        {
            Ok(updated) => Ok(updated),
            Err(error) => {
                if abort_on_error {
                    self.abort_permission_write(fence).await;
                }
                Err(error)
            }
        }
    }

    pub(super) async fn reserve_room_member_permission_fences(
        &self,
        room_id: &RoomId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<PendingRoomMemberPermissionFence>> {
        let members = sqlx::query!(
            r#"SELECT room_id as "room_id: RoomId",
                      user_id as "user_id: UserId",
                      version
             FROM room_members
             WHERE room_id = $1
             FOR UPDATE"#,
            room_id as &RoomId,
        )
        .fetch_all(&mut **tx)
        .await?;

        let mut fences = Vec::with_capacity(members.len());
        for member in members {
            let fence = match self
                .begin_permission_write(&member.room_id, &member.user_id, member.version)
                .await
            {
                Ok(fence) => fence,
                Err(error) => {
                    self.abort_room_member_permission_fences(&fences).await;
                    return Err(error);
                }
            };
            fences.push(PendingRoomMemberPermissionFence {
                room_id: member.room_id,
                user_id: member.user_id,
                fence,
            });
        }

        Ok(fences)
    }

    pub(super) async fn abort_room_member_permission_fences(
        &self,
        fences: &[PendingRoomMemberPermissionFence],
    ) {
        for pending in fences {
            self.abort_permission_write(&pending.fence).await;
        }
    }

    pub(super) async fn commit_removed_room_member_permission_fences(
        &self,
        fences: Vec<PendingRoomMemberPermissionFence>,
        removed_members: &[RemovedRoomMember],
    ) -> Result<()> {
        let removed_versions = removed_members
            .iter()
            .map(|member| ((member.room_id, member.user_id), member.version))
            .collect::<HashMap<_, _>>();

        let mut first_error = None;
        for pending in fences {
            let Some(version) = removed_versions.get(&(pending.room_id, pending.user_id)) else {
                self.abort_permission_write(&pending.fence).await;
                continue;
            };
            if let Err(error) = self
                .permission_service
                .commit_permission_write(&pending.fence, *version)
                .await
            {
                tracing::warn!(
                    error = %error,
                    room_id = %pending.room_id,
                    user_id = %pending.user_id,
                    "Failed to finalize removed room member permission fence"
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
}
