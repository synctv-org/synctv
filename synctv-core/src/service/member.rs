//! Member management service
//!
//! Handles room member operations including joining, leaving, kicking,
//! and role management with Allow/Deny permission pattern.

use crate::{
    cache::CacheInvalidationRuntime,
    models::{
        MemberStatus, MyRoomListQuery, PageParams, PermissionBits, Room, RoomId, RoomMember,
        RoomMemberWithUser, RoomRole, RoomSettings, UserId,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::audit::{AuditAction, AuditService, AuditTargetType},
    service::notification::{NotificationService, PermissionChangedNotification},
    service::permission::PermissionService,
    Error, Result,
};

use std::sync::Arc;

/// Options for adding a member to a room
///
/// # Examples
///
/// ```text
/// // Default options (all checks enabled)
/// let options = AddMemberOptions::new();
///
/// // Skip max members check
/// let options = AddMemberOptions::new().skip_max_members_check();
///
/// // Set custom max members limit
/// let options = AddMemberOptions::new().with_max_members(100);
///
/// // Skip cache invalidation
/// let options = AddMemberOptions::new().skip_cache_invalidation();
///
/// // Combine options
/// let options = AddMemberOptions::new()
///     .skip_max_members_check()
///     .skip_cache_invalidation();
/// ```
#[derive(Debug, Clone, Default)]
pub struct AddMemberOptions {
    /// Check if room is active
    pub check_room_active: bool,
    /// Check for duplicate membership
    pub check_duplicate: bool,
    /// Check max members limit
    pub check_max_members: bool,
    /// Maximum number of members allowed (0 = no limit)
    pub max_members: u64,
    /// Initial lifecycle status for the membership row
    pub initial_status: MemberStatus,
    /// Invalidate permission cache after adding
    pub invalidate_cache: bool,
}

impl AddMemberOptions {
    /// Create default options (all checks enabled, no max limit)
    #[must_use]
    pub const fn new() -> Self {
        Self {
            check_room_active: true,
            check_duplicate: true,
            check_max_members: false, // disabled by default
            max_members: 0,           // 0 means no limit
            initial_status: MemberStatus::Active,
            invalidate_cache: true,
        }
    }

    /// Set max members limit (enables the check)
    #[must_use]
    pub const fn with_max_members(mut self, max: u64) -> Self {
        self.max_members = max;
        self.check_max_members = true;
        self
    }

    /// Skip max members check
    #[must_use]
    pub const fn skip_max_members_check(mut self) -> Self {
        self.check_max_members = false;
        self
    }

    /// Skip room active check
    #[must_use]
    pub const fn skip_active_check(mut self) -> Self {
        self.check_room_active = false;
        self
    }

    /// Skip duplicate membership check
    #[must_use]
    pub const fn skip_duplicate_check(mut self) -> Self {
        self.check_duplicate = false;
        self
    }

    /// Set the initial lifecycle status for the membership row.
    #[must_use]
    pub const fn with_initial_status(mut self, status: MemberStatus) -> Self {
        self.initial_status = status;
        self
    }

    /// Skip cache invalidation
    #[must_use]
    pub const fn skip_cache_invalidation(mut self) -> Self {
        self.invalidate_cache = false;
        self
    }
}

/// Member management service
///
/// Responsible for all member-related operations within rooms.
#[derive(Clone)]
pub struct MemberService {
    member_repo: RoomMemberRepository,
    room_repo: RoomRepository,
    room_settings_repo: Option<RoomSettingsRepository>,
    permission_service: PermissionService,
    audit_service: Option<Arc<AuditService>>,
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    notification_service: NotificationService,
}

pub struct AdminMemberUpdate {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub actor_username: String,
    pub target_user_id: UserId,
    pub role: Option<RoomRole>,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
}

impl std::fmt::Debug for MemberService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberService").finish()
    }
}

impl MemberService {
    const fn uses_admin_overrides(role: RoomRole) -> bool {
        matches!(role, RoomRole::Admin)
    }

    fn validate_assignable_permissions(permissions: u64) -> Result<()> {
        if PermissionBits::includes_only_assignable_in_room(permissions) {
            return Ok(());
        }

        Err(Error::InvalidInput(
            "Permission set includes lifecycle-only permissions that cannot be delegated within a room"
                .to_string(),
        ))
    }

    /// Create a new member service
    #[must_use]
    pub fn new(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        permission_service: PermissionService,
        notification_service: NotificationService,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo: None,
            permission_service,
            audit_service: None,
            cache_invalidation: None,
            notification_service,
        }
    }

    /// Create a member service with optional collaborators wired at construction time.
    #[must_use]
    pub fn new_with_runtime(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        room_settings_repo: Option<RoomSettingsRepository>,
        permission_service: PermissionService,
        audit_service: Option<Arc<AuditService>>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        notification_service: NotificationService,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo,
            permission_service,
            audit_service,
            cache_invalidation,
            notification_service,
        }
    }

    /// Set the room settings repository
    pub fn set_room_settings_repo(&mut self, repo: RoomSettingsRepository) {
        self.room_settings_repo = Some(repo);
    }

    /// Set the cache invalidation service for cross-replica permission cache sync
    pub fn set_cache_invalidation(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        self.permission_service
            .set_invalidation_service(Arc::clone(&service));
        self.cache_invalidation = Some(service);
    }

    async fn notify_permission_changed(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        actor_username: &str,
        member: &RoomMember,
    ) {
        let resolved_actor_username = if actor_username.trim().is_empty() {
            self.lookup_username(actor_id).await
        } else {
            actor_username.to_string()
        };
        let room_settings = if let Some(repo) = self.room_settings_repo.as_ref() {
            repo.get(room_id).await.unwrap_or_default()
        } else {
            RoomSettings::default()
        };
        let effective_permissions = self
            .permission_service
            .effective_member_permissions(member, &room_settings)
            .0;

        if let Err(error) = self.notification_service.notify_permission_changed(
            room_id,
            PermissionChangedNotification {
                user_id: &member.user_id,
                role: i32::from(member.role),
                effective_permissions,
                added_permissions: member.added_permissions,
                removed_permissions: member.removed_permissions,
                admin_added_permissions: member.admin_added_permissions,
                admin_removed_permissions: member.admin_removed_permissions,
                updated_by_user_id: actor_id,
                updated_by_username: &resolved_actor_username,
            },
        ) {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                user_id = %member.user_id,
                "Failed to broadcast permission changed event"
            );
        }
    }

    async fn lookup_username(&self, user_id: &UserId) -> String {
        UserRepository::new(self.member_repo.pool().clone())
            .get_by_id(user_id)
            .await
            .ok()
            .flatten()
            .map(|user| user.username)
            .unwrap_or_default()
    }

    /// Log an audit event if the audit service is configured.
    /// Failures are logged as warnings but never propagated.
    ///
    /// The `actor_username` is passed from the caller (API layer) to avoid
    /// a separate DB lookup. Pass an empty string if the username is not
    /// available (e.g., in background tasks).
    async fn audit_log(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) {
        if let Some(ref audit) = self.audit_service {
            if let Err(e) = audit
                .log(
                    actor_id.to_string(),
                    actor_username.to_string(),
                    action,
                    target_type,
                    target_id,
                    details,
                    None,
                    None,
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to write audit log from MemberService");
            }
        }
    }

    /// Add a user as a member to a room (with default options)
    ///
    /// This is a convenience method that uses default options.
    pub async fn add_member(
        &self,
        room_id: RoomId,
        user_id: UserId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        self.add_member_with_options(room_id, user_id, role, AddMemberOptions::new())
            .await
    }

    /// Add a user as a member to a room with custom options
    ///
    /// This method uses a database transaction to perform all checks and the insert atomically.
    pub async fn add_member_with_options(
        &self,
        room_id: RoomId,
        user_id: UserId,
        role: RoomRole,
        mut options: AddMemberOptions,
    ) -> Result<RoomMember> {
        // Get room settings and apply to options if max_members check is enabled
        if options.check_max_members {
            let room_settings = if let Some(ref settings_repo) = self.room_settings_repo {
                settings_repo.get(&room_id).await?
            } else {
                RoomSettings::default()
            };

            options.max_members = room_settings.max_members.0;
        }

        // Create member object
        let mut member = RoomMember::new(room_id, user_id, role);
        member.status = options.initial_status;

        // Add member with options (transaction happens in repository)
        let created_member = self.member_repo.add_with_options(&member, &options).await?;

        // Invalidate permission cache (outside transaction)
        if options.invalidate_cache {
            self.permission_service
                .invalidate_cache(&room_id, &user_id)
                .await;
        }

        Ok(created_member)
    }

    /// Remove a user from all rooms.
    ///
    /// Used during user deletion to clean up all room memberships.
    /// Returns the number of memberships removed.
    pub async fn remove_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.member_repo.remove_all_for_user(user_id).await
    }

    /// Remove a member from a room
    ///
    /// Uses an atomic SQL operation that combines the membership check with the removal,
    /// eliminating the TOCTOU race between checking membership and performing the removal.
    pub async fn remove_member(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        // Atomic check + removal: the SQL WHERE clause ensures the member exists
        // and hasn't left yet, preventing TOCTOU races.
        let removed = self.member_repo.remove(&room_id, &user_id).await?;
        if !removed {
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        }

        // Invalidate permission cache
        self.permission_service
            .invalidate_cache(&room_id, &user_id)
            .await;

        Ok(())
    }

    /// Kick a member from a room (requires permission)
    ///
    /// Uses an atomic SQL statement that combines the role hierarchy check with the
    /// removal, eliminating the TOCTOU race between checking roles and performing
    /// the kick.
    pub async fn kick_member(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
    ) -> Result<()> {
        // Check if kicker has permission to kick (no cache - security-critical)
        self.permission_service
            .check_permission_no_cache(&room_id, &kicker_id, PermissionBits::KICK_MEMBER)
            .await?;

        // Can't kick yourself
        if kicker_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        // Atomic role check + removal: the SQL WHERE clause ensures the kicker
        // outranks the target, preventing TOCTOU races.
        let removed = self
            .member_repo
            .remove_with_role_check(&room_id, &kicker_id, &target_user_id)
            .await?;
        if !removed {
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        }

        // Invalidate permission cache for kicked user (local)
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Notify local WebSocket clients that member was kicked
        if let Err(e) = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id)
        {
            tracing::warn!(
                error = %e,
                room_id = %room_id,
                user_id = %target_user_id,
                "Failed to notify local clients of member kick"
            );
        }

        // Audit log
        self.audit_log(
            &kicker_id,
            "",
            AuditAction::MemberKicked,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
            }),
        )
        .await;

        Ok(())
    }

    /// Administrative member removal that bypasses room-local permission and role checks.
    ///
    /// This is intended only for the global management plane. The target must
    /// still be an active member, but the actor does not need room membership.
    pub async fn admin_kick_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        actor_username: &str,
        target_user_id: UserId,
    ) -> Result<()> {
        if actor_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let removed = self.member_repo.remove(&room_id, &target_user_id).await?;
        if !removed {
            return Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            ));
        }

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        if let Err(e) = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id)
        {
            tracing::warn!(
                error = %e,
                room_id = %room_id,
                user_id = %target_user_id,
                "Failed to notify local clients of admin member kick"
            );
        }

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberKicked,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "mode": "admin_override",
            }),
        )
        .await;

        Ok(())
    }

    /// Maximum retry attempts for optimistic lock conflicts
    const MAX_RETRIES: u32 = 3;
    /// Base delay for exponential backoff (milliseconds)
    const BACKOFF_BASE_MS: u64 = 5;

    /// Set member Allow/Deny permissions
    ///
    /// This implements the Allow/Deny pattern:
    /// - `added_permissions`: Extra permissions to add to role default
    /// - `removed_permissions`: Permissions to remove from role default
    ///
    /// Retries automatically on optimistic lock conflicts.
    pub async fn set_member_permissions(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> Result<RoomMember> {
        Self::validate_assignable_permissions(added_permissions)?;
        Self::validate_assignable_permissions(removed_permissions)?;

        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                PermissionBits::SET_MEMBER_PERMISSIONS,
            )
            .await?;

        let updated_member = super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;

                if Self::uses_admin_overrides(member.role) {
                    self.member_repo
                        .update_admin_permissions(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            member.version,
                        )
                        .await
                } else {
                    self.member_repo
                        .update_permissions(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            member.version,
                        )
                        .await
                }
            },
        )
        .await?;

        // Invalidate permission cache for target user (local)
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Audit log
        self.audit_log(
            &granter_id,
            "",
            AuditAction::MemberPermissionUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "added_permissions": added_permissions,
                "removed_permissions": removed_permissions,
            }),
        )
        .await;

        self.notify_permission_changed(&room_id, &granter_id, "", &updated_member)
            .await;

        Ok(updated_member)
    }

    /// Administrative member update that bypasses room-local permission and creator checks.
    ///
    /// This is intended for the global management plane only. It preserves the
    /// same role/override invariants as the client path, but the actor is
    /// authorized by global admin/root identity outside the room permission graph.
    pub async fn admin_update_member(&self, update: AdminMemberUpdate) -> Result<RoomMember> {
        let AdminMemberUpdate {
            room_id,
            actor_id,
            actor_username,
            target_user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = update;
        Self::validate_assignable_permissions(added_permissions)?;
        Self::validate_assignable_permissions(removed_permissions)?;
        Self::validate_assignable_permissions(admin_added_permissions)?;
        Self::validate_assignable_permissions(admin_removed_permissions)?;

        let has_permission_changes = added_permissions > 0
            || removed_permissions > 0
            || admin_added_permissions > 0
            || admin_removed_permissions > 0;

        let target_member = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;

        let effective_role = role.unwrap_or(target_member.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);

        if has_permission_changes {
            if effective_is_admin && (added_permissions > 0 || removed_permissions > 0) {
                return Err(Error::Authorization(
                    "Admin members must use admin_added_permissions/admin_removed_permissions"
                        .to_string(),
                ));
            }
            if !effective_is_admin && (admin_added_permissions > 0 || admin_removed_permissions > 0)
            {
                return Err(Error::Authorization(
                    "Only admin members use admin_added_permissions/admin_removed_permissions"
                        .to_string(),
                ));
            }
        }

        if let Some(new_role) = role {
            if new_role == RoomRole::Creator {
                return Err(Error::InvalidInput(
                    "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                        .to_string(),
                ));
            }

            let room = self
                .room_repo
                .get_by_id(&room_id)
                .await?
                .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

            if target_user_id == room.created_by {
                return Err(Error::InvalidInput(
                    "Cannot change the role of the room creator via set_member_role".to_string(),
                ));
            }

            let updated_member = super::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                "Role update failed after maximum retry attempts",
                || async {
                    let member = self
                        .member_repo
                        .get(&room_id, &target_user_id)
                        .await?
                        .ok_or_else(|| {
                            Error::NotFound("User is not a member of this room".to_string())
                        })?;
                    self.member_repo
                        .update_role(&room_id, &target_user_id, new_role, member.version)
                        .await
                },
            )
            .await?;

            self.permission_service
                .invalidate_cache(&room_id, &target_user_id)
                .await;

            if let Some(ref invalidation) = self.cache_invalidation {
                if let Err(error) = invalidation.invalidate_room_settings(&room_id).await {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to broadcast room settings cache invalidation after admin role change"
                    );
                }
            }

            self.audit_log(
                &actor_id,
                &actor_username,
                AuditAction::MemberRoleUpdated,
                AuditTargetType::Member,
                Some(target_user_id.to_string()),
                serde_json::json!({
                    "room_id": room_id,
                    "role": new_role.to_string(),
                    "mode": "admin_override",
                }),
            )
            .await;

            if !has_permission_changes {
                self.notify_permission_changed(
                    &room_id,
                    &actor_id,
                    &actor_username,
                    &updated_member,
                )
                .await;
                return Ok(updated_member);
            }
        }

        let updated_member = super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;

                if Self::uses_admin_overrides(member.role) {
                    self.member_repo
                        .update_admin_permissions(
                            &room_id,
                            &target_user_id,
                            admin_added_permissions,
                            admin_removed_permissions,
                            member.version,
                        )
                        .await
                } else {
                    self.member_repo
                        .update_permissions(
                            &room_id,
                            &target_user_id,
                            added_permissions,
                            removed_permissions,
                            member.version,
                        )
                        .await
                }
            },
        )
        .await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            &actor_username,
            AuditAction::MemberPermissionUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "added_permissions": if effective_is_admin { admin_added_permissions } else { added_permissions },
                "removed_permissions": if effective_is_admin { admin_removed_permissions } else { removed_permissions },
                "mode": "admin_override",
            }),
        )
        .await;

        self.notify_permission_changed(&room_id, &actor_id, &actor_username, &updated_member)
            .await;

        Ok(updated_member)
    }

    /// Grant a specific permission to a member (Allow pattern)
    ///
    /// Uses atomic SQL bitwise OR to avoid TOCTOU race conditions.
    pub async fn grant_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        Self::validate_assignable_permissions(permission)?;

        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                PermissionBits::SET_MEMBER_PERMISSIONS,
            )
            .await?;

        let target_member = self.member_repo.get(&room_id, &target_user_id).await?;
        let target_member = target_member
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;

        // Atomic grant in SQL against the override layer used by the target role.
        let updated_member = if Self::uses_admin_overrides(target_member.role) {
            self.member_repo
                .grant_admin_permission_atomic_for_role(
                    &room_id,
                    &target_user_id,
                    permission,
                    target_member.role,
                )
                .await?
        } else {
            self.member_repo
                .grant_permission_atomic_for_role(
                    &room_id,
                    &target_user_id,
                    permission,
                    target_member.role,
                )
                .await?
        };

        // Invalidate permission cache for target user
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Audit log
        self.audit_log(
            &granter_id,
            "",
            AuditAction::PermissionGranted,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "permission": permission,
            }),
        )
        .await;

        Ok(updated_member)
    }

    /// Revoke a specific permission from a member (Deny pattern)
    ///
    /// Uses atomic SQL bitwise OR to avoid TOCTOU race conditions.
    pub async fn revoke_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        Self::validate_assignable_permissions(permission)?;

        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                PermissionBits::SET_MEMBER_PERMISSIONS,
            )
            .await?;

        let target_member = self.member_repo.get(&room_id, &target_user_id).await?;
        let target_member = target_member
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;

        // Atomic revoke in SQL against the override layer used by the target role.
        let updated_member = if Self::uses_admin_overrides(target_member.role) {
            self.member_repo
                .revoke_admin_permission_atomic_for_role(
                    &room_id,
                    &target_user_id,
                    permission,
                    target_member.role,
                )
                .await?
        } else {
            self.member_repo
                .revoke_permission_atomic_for_role(
                    &room_id,
                    &target_user_id,
                    permission,
                    target_member.role,
                )
                .await?
        };

        // Invalidate permission cache for target user
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Audit log
        self.audit_log(
            &granter_id,
            "",
            AuditAction::PermissionRevoked,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "permission": permission,
            }),
        )
        .await;

        Ok(updated_member)
    }

    /// Reset member permissions to role default (clear Allow/Deny)
    ///
    /// Retries automatically on optimistic lock conflicts.
    pub async fn reset_member_permissions(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
    ) -> Result<RoomMember> {
        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &granter_id,
                PermissionBits::SET_MEMBER_PERMISSIONS,
            )
            .await?;

        let updated_member = super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Permission reset failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                self.member_repo
                    .reset_permissions(&room_id, &target_user_id, member.version)
                    .await
            },
        )
        .await?;

        // Invalidate permission cache for target user
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        Ok(updated_member)
    }

    /// Get all members of a room with user info
    pub async fn list_members(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        self.member_repo.list_by_room(room_id).await
    }

    /// Get members of a room with database-level pagination
    ///
    /// Uses `COUNT(*) OVER()` window function for atomic count + fetch in a single query.
    /// Returns (members, total_count) tuple.
    ///
    /// # Performance
    ///
    /// This method should be used instead of `list_members` + in-memory pagination
    /// when dealing with rooms that may have many members. It only loads the
    /// requested page from the database.
    pub async fn list_members_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        pagination.validate()?;
        self.member_repo
            .list_by_room_paginated(room_id, pagination)
            .await
    }

    pub async fn list_members_query(
        &self,
        room_id: &RoomId,
        query: crate::models::RoomMemberListQuery,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        query.pagination.validate()?;
        self.member_repo.list_by_room_query(room_id, &query).await
    }

    /// Get member count for a room
    pub async fn count_members(&self, room_id: &RoomId) -> Result<i32> {
        self.member_repo.count_by_room(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn count_members_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
        self.member_repo.count_by_rooms_batch(room_ids).await
    }

    /// Check if a user is a member of a room
    pub async fn is_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        self.member_repo.is_member(room_id, user_id).await
    }

    /// Check if a user is banned from a room
    pub async fn is_banned(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        self.member_repo.is_banned(room_id, user_id).await
    }

    /// Get a specific member
    pub async fn get_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<RoomMember>> {
        self.member_repo.get(room_id, user_id).await
    }

    /// List all rooms a user is a member of
    pub async fn list_user_rooms(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomId>, i64)> {
        pagination.validate()?;
        self.member_repo.list_by_user(user_id, pagination).await
    }

    /// List all rooms a user is a member of with full details
    pub async fn list_user_rooms_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        pagination.validate()?;
        self.member_repo
            .list_by_user_with_details(user_id, pagination)
            .await
    }

    /// List all rooms a user is related to through membership with query semantics.
    pub async fn list_user_rooms_with_details_query(
        &self,
        user_id: &UserId,
        query: &MyRoomListQuery,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        query.pagination.validate()?;
        self.member_repo
            .list_by_user_with_query(user_id, query)
            .await
    }

    /// Ban a member from a room
    ///
    /// Uses an atomic SQL statement that combines the role hierarchy check with the
    /// ban update, eliminating the TOCTOU race between checking roles and performing
    /// the ban.
    pub async fn ban_member(
        &self,
        room_id: RoomId,
        admin_id: UserId,
        target_user_id: UserId,
        reason: Option<String>,
    ) -> Result<()> {
        // Check admin permission without cache - critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(&room_id, &admin_id, PermissionBits::BAN_MEMBER)
            .await?;

        // Atomic role check + ban: the SQL WHERE clause ensures the admin outranks
        // the target, preventing TOCTOU races.
        self.member_repo
            .ban_with_role_check(&room_id, &admin_id, &target_user_id, reason.clone())
            .await?;

        // Invalidate permission cache for banned user (local)
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Notify local WebSocket clients that member was kicked (ban implies kick)
        if let Err(e) = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id)
        {
            tracing::warn!(
                error = %e,
                room_id = %room_id,
                user_id = %target_user_id,
                "Failed to notify local clients of member ban"
            );
        }

        // Audit log
        self.audit_log(
            &admin_id,
            "",
            AuditAction::MemberBanned,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "reason": reason,
            }),
        )
        .await;

        Ok(())
    }

    /// Administrative member ban that bypasses room-local permission and role checks.
    ///
    /// This is intended only for the global management plane. The actor does
    /// not need to be a room member.
    pub async fn admin_ban_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        actor_username: &str,
        target_user_id: UserId,
        persisted_banned_by: Option<UserId>,
        reason: Option<String>,
    ) -> Result<()> {
        if actor_id == target_user_id {
            return Err(Error::InvalidInput("Cannot ban yourself".to_string()));
        }

        self.member_repo
            .ban_member(
                &room_id,
                &target_user_id,
                persisted_banned_by.as_ref(),
                reason.clone(),
            )
            .await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        if let Err(e) = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id)
        {
            tracing::warn!(
                error = %e,
                room_id = %room_id,
                user_id = %target_user_id,
                "Failed to notify local clients of admin member ban"
            );
        }

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberBanned,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "reason": reason,
                "mode": "admin_override",
            }),
        )
        .await;

        Ok(())
    }

    /// Unban a member from a room
    pub async fn unban_member(
        &self,
        room_id: RoomId,
        admin_id: UserId,
        target_user_id: UserId,
    ) -> Result<()> {
        // Check admin permission without cache - security-critical
        self.permission_service
            .check_permission_no_cache(&room_id.clone(), &admin_id, PermissionBits::BAN_MEMBER)
            .await?;

        // Unban member
        self.member_repo
            .unban_member(&room_id, &target_user_id)
            .await?;

        // Invalidate permission cache for unbanned user
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Audit log
        self.audit_log(
            &admin_id,
            "",
            AuditAction::MemberUnbanned,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({ "room_id": room_id }),
        )
        .await;

        Ok(())
    }

    /// Administrative member unban that bypasses room-local permission checks.
    ///
    /// This is intended only for the global management plane. The actor does
    /// not need to be a room member.
    pub async fn admin_unban_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        actor_username: &str,
        target_user_id: UserId,
    ) -> Result<()> {
        self.member_repo
            .unban_member(&room_id, &target_user_id)
            .await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberUnbanned,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "mode": "admin_override",
            }),
        )
        .await;

        Ok(())
    }

    /// Set member role (member/admin/creator)
    pub async fn set_member_role(
        &self,
        room_id: RoomId,
        creator_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        if role == RoomRole::Creator {
            return Err(Error::InvalidInput(
                "Creator role is bound to room ownership and cannot be assigned via set_member_role"
                    .to_string(),
            ));
        }

        // Check if user is creator (only creator can change roles)
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by != creator_id {
            return Err(Error::Authorization(
                "Only room creator can change member roles".to_string(),
            ));
        }

        if target_user_id == room.created_by {
            return Err(Error::InvalidInput(
                "Cannot change the role of the room creator via set_member_role".to_string(),
            ));
        }

        // Verify target is a member and update role with optimistic lock retry
        let (updated_member, old_role) = super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Role update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;

                let old_role = member.role;

                let updated = self
                    .member_repo
                    .update_role(&room_id, &target_user_id, role, member.version)
                    .await?;
                Ok((updated, old_role))
            },
        )
        .await?;

        // Invalidate permission cache (local)
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Invalidate room settings cache to ensure fresh role default permissions
        // are used when recalculating the user's effective permissions.
        // This is necessary because the permission calculation depends on both
        // the member's role AND the room's role-specific permission settings.
        if let Some(ref invalidation) = self.cache_invalidation {
            if let Err(e) = invalidation.invalidate_room_settings(&room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast room settings cache invalidation after role change"
                );
            }
        }

        // Audit log
        self.audit_log(
            &creator_id,
            "",
            AuditAction::MemberRoleUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "old_role": format!("{:?}", old_role),
                "new_role": format!("{:?}", role),
            }),
        )
        .await;

        Ok(updated_member)
    }

    /// Set member lifecycle status.
    pub async fn set_member_status(
        &self,
        room_id: RoomId,
        admin_id: UserId,
        target_user_id: UserId,
        status: MemberStatus,
    ) -> Result<RoomMember> {
        match status {
            MemberStatus::Active => {
                if self.is_banned(&room_id, &target_user_id).await? {
                    return Err(Error::InvalidInput(
                        "Use unban_member before changing a banned member lifecycle status"
                            .to_string(),
                    ));
                }
            }
            MemberStatus::Left => {
                return Err(Error::InvalidInput(
                    "Use remove_member or kick_member instead of set_member_status(..., Left)"
                        .to_string(),
                ));
            }
        }

        // Active lifecycle transitions stay on the approval permission path.
        self.permission_service
            .check_permission_no_cache(&room_id, &admin_id, PermissionBits::APPROVE_MEMBER)
            .await?;

        // Get current member and update status with optimistic lock retry
        let (updated_member, old_status) = super::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "Status update failed after maximum retry attempts",
            || async {
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;

                let old_status = member.status;

                let updated = self
                    .member_repo
                    .update_status(&room_id, &target_user_id, status, member.version)
                    .await?;
                Ok((updated, old_status))
            },
        )
        .await?;

        // Invalidate permission cache
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Audit log
        self.audit_log(
            &admin_id,
            "",
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            serde_json::json!({
                "room_id": room_id,
                "old_status": format!("{:?}", old_status),
                "new_status": format!("{:?}", status),
            }),
        )
        .await;

        Ok(updated_member)
    }

    /// List all members including inactive (left) (admin view)
    pub async fn list_members_all(
        &self,
        room_id: &RoomId,
        admin_id: UserId,
    ) -> Result<Vec<RoomMemberWithUser>> {
        // Check admin permission without cache - security-critical operation
        self.permission_service
            .check_permission_no_cache(&room_id.clone(), &admin_id, PermissionBits::KICK_MEMBER)
            .await?;

        // Get all members regardless of left_at status
        self.member_repo.list_by_room_all(room_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_role_to_proto_i32_matches_common_wire_values() {
        assert_eq!(
            i32::from(RoomRole::Creator),
            synctv_proto::common::RoomMemberRole::Creator as i32
        );
        assert_eq!(
            i32::from(RoomRole::Admin),
            synctv_proto::common::RoomMemberRole::Admin as i32
        );
        assert_eq!(
            i32::from(RoomRole::Member),
            synctv_proto::common::RoomMemberRole::Member as i32
        );
        assert_eq!(
            i32::from(RoomRole::Guest),
            synctv_proto::common::RoomMemberRole::Guest as i32
        );
    }
}
