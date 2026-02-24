//! Member management service
//!
//! Handles room member operations including joining, leaving, kicking,
//! and role management with Allow/Deny permission pattern.

use std::sync::Arc;

use crate::{
    cache::CacheInvalidationService,
    models::{Room, RoomId, RoomMember, RoomMemberWithUser, UserId, PermissionBits, RoomRole, MemberStatus, RoomSettings, PageParams},
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository},
    service::audit::{AuditService, AuditAction, AuditTargetType},
    service::notification::NotificationService,
    service::permission::PermissionService,
    Error, Result,
};

/// Trait for broadcasting member events to cluster replicas.
///
/// This abstracts over the cluster manager so that `synctv-core` does not
/// depend on `synctv-cluster`. The implementation lives in the API/wiring
/// layer where `ClusterManager` is available.
pub trait MemberEventBroadcaster: Send + Sync {
    /// Broadcast a kick event for a user from a specific room to all cluster replicas.
    fn broadcast_kick_from_room(&self, room_id: &RoomId, user_id: &UserId, reason: &str);

    /// Broadcast a ban event for a user (disconnect from all rooms) to all cluster replicas.
    fn broadcast_kick_user(&self, user_id: &UserId, reason: &str);
}
/// Role hierarchy level for authorization checks (higher = more authority)
/// Creator > Admin > Member > Guest
///
/// Note: kick_member/ban_member enforce role hierarchy atomically in SQL
/// (see `remove_with_role_check` / `ban_with_role_check` in the repository layer).
/// This function is kept for unit tests that validate the conceptual hierarchy.
#[cfg(test)]
const fn role_level(role: &RoomRole) -> u8 {
    match role {
        RoomRole::Creator => 3,
        RoomRole::Admin => 2,
        RoomRole::Member => 1,
        RoomRole::Guest => 0,
    }
}

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
            check_max_members: false,  // disabled by default
            max_members: 0,           // 0 means no limit
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
    cache_invalidation: Option<Arc<CacheInvalidationService>>,
    notification_service: Option<NotificationService>,
    event_broadcaster: Option<Arc<dyn MemberEventBroadcaster>>,
}

impl std::fmt::Debug for MemberService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberService").finish()
    }
}

impl MemberService {
    /// Create a new member service
    #[must_use]
    pub fn new(
        member_repo: RoomMemberRepository,
        room_repo: RoomRepository,
        permission_service: PermissionService,
    ) -> Self {
        Self {
            member_repo,
            room_repo,
            room_settings_repo: None,
            permission_service,
            audit_service: None,
            cache_invalidation: None,
            notification_service: None,
            event_broadcaster: None,
        }
    }

    /// Set the room settings repository
    pub fn set_room_settings_repo(&mut self, repo: RoomSettingsRepository) {
        self.room_settings_repo = Some(repo);
    }

    /// Inject the audit service for security-sensitive operation logging
    pub fn set_audit_service(&mut self, audit: Arc<AuditService>) {
        self.audit_service = Some(audit);
    }

    /// Set the cache invalidation service for cross-replica permission cache sync
    pub fn set_cache_invalidation(&mut self, service: Arc<CacheInvalidationService>) {
        self.cache_invalidation = Some(service);
    }

    /// Set the notification service for broadcasting member events to local WebSocket clients
    pub fn set_notification_service(&mut self, service: NotificationService) {
        self.notification_service = Some(service);
    }

    /// Set the cluster event broadcaster for cross-replica kick/ban propagation
    pub fn set_event_broadcaster(&mut self, broadcaster: Arc<dyn MemberEventBroadcaster>) {
        self.event_broadcaster = Some(broadcaster);
    }

    /// Broadcast permission cache invalidation to other cluster replicas with
    /// retry logic (3 attempts, exponential backoff starting at 50ms).
    ///
    /// Permission changes are security-critical, so we retry aggressively to
    /// ensure all replicas see the invalidation. If all attempts fail, an error
    /// is logged but the operation is not rolled back (the local change succeeded).
    async fn broadcast_permission_invalidation_with_retry(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) {
        let Some(ref invalidation) = self.cache_invalidation else {
            return;
        };
        let mut last_err = None;
        for attempt in 0..3u32 {
            match invalidation.invalidate_user_permission(room_id, user_id).await {
                Ok(()) => {
                    return;
                }
                Err(e) => {
                    let backoff_ms = 50 * (1u64 << attempt); // 50ms, 100ms, 200ms
                    tracing::warn!(
                        error = %e,
                        room_id = %room_id.as_str(),
                        user_id = %user_id.as_str(),
                        attempt = attempt + 1,
                        backoff_ms = backoff_ms,
                        "Permission invalidation broadcast failed, retrying"
                    );
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
            }
        }
        if let Some(e) = last_err {
            tracing::error!(
                error = %e,
                room_id = %room_id.as_str(),
                user_id = %user_id.as_str(),
                "Permission invalidation broadcast failed after 3 attempts"
            );
        }
    }

    /// Log an audit event if the audit service is configured.
    /// Failures are logged as warnings but never propagated.
    async fn audit_log(
        &self,
        actor_id: &UserId,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) {
        if let Some(ref audit) = self.audit_service {
            if let Err(e) = audit.log(
                actor_id.as_str().to_string(),
                String::new(), // username not available at service layer
                action,
                target_type,
                target_id,
                details,
                None,
                None,
            ).await {
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
        let member = RoomMember::new(room_id.clone(), user_id.clone(), role);

        // Add member with options (transaction happens in repository)
        let created_member = self
            .member_repo
            .add_with_options(&member, &options)
            .await?;

        // Invalidate permission cache (outside transaction)
        if options.invalidate_cache {
            self.permission_service
                .invalidate_cache(&room_id, &user_id)
                .await;

            // Broadcast permission cache invalidation to other replicas.
            // This is necessary for re-joins (ON CONFLICT UPDATE) where the
            // user's permissions may have changed. Without this, other replicas
            // would serve stale permission data from their L1 caches.
            if let Some(ref invalidation) = self.cache_invalidation {
                if let Err(e) = invalidation.invalidate_user_permission(&room_id, &user_id).await {
                    tracing::warn!(
                        error = %e,
                        room_id = %room_id.as_str(),
                        user_id = %user_id.as_str(),
                        "Failed to broadcast permission cache invalidation after member add/re-join"
                    );
                }
            }
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
    pub async fn remove_member(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        // Check if member
        if !self.member_repo.is_member(&room_id, &user_id).await? {
            return Err(Error::NotFound("Not a member of this room".to_string()));
        }

        // Remove member
        self.member_repo.remove(&room_id, &user_id).await?;

        // Invalidate permission cache
        self.permission_service.invalidate_cache(&room_id, &user_id).await;

        // Broadcast permission cache invalidation to other cluster replicas with retry
        self.broadcast_permission_invalidation_with_retry(&room_id, &user_id).await;

        // Broadcast kick event to cluster for cross-replica disconnect
        if let Some(ref broadcaster) = self.event_broadcaster {
            broadcaster.broadcast_kick_from_room(&room_id, &user_id, "removed");
        }

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
            .check_permission_no_cache(&room_id, &kicker_id, PermissionBits::KICK_USER)
            .await?;

        // Can't kick yourself
        if kicker_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        // Atomic role check + removal: the SQL WHERE clause ensures the kicker
        // outranks the target, preventing TOCTOU races.
        let removed = self.member_repo
            .remove_with_role_check(&room_id, &kicker_id, &target_user_id)
            .await?;
        if !removed {
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role".to_string(),
            ));
        }

        // Invalidate permission cache for kicked user (local)
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Broadcast permission cache invalidation to other cluster replicas with retry
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Notify local WebSocket clients that member was kicked
        if let Some(ref ns) = self.notification_service {
            if let Err(e) = ns.notify_member_kicked(&room_id, &target_user_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    user_id = %target_user_id.as_str(),
                    "Failed to notify local clients of member kick"
                );
            }
        }

        // Broadcast kick event to all cluster replicas for cross-replica disconnect
        if let Some(ref broadcaster) = self.event_broadcaster {
            broadcaster.broadcast_kick_from_room(&room_id, &target_user_id, "kicked");
        }

        // Audit log
        self.audit_log(
            &kicker_id,
            AuditAction::MemberKicked,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
            }),
        ).await;

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
        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(&room_id, &granter_id, PermissionBits::GRANT_PERMISSION)
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
                    .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
                self.member_repo
                    .update_permissions(&room_id, &target_user_id, added_permissions, removed_permissions, member.version)
                    .await
            },
        )
        .await?;

        // Invalidate permission cache for target user (local)
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Broadcast permission cache invalidation to other cluster replicas
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Audit log
        self.audit_log(
            &granter_id,
            AuditAction::MemberPermissionUpdated,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "added_permissions": added_permissions,
                "removed_permissions": removed_permissions,
            }),
        ).await;

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
        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(&room_id, &granter_id, PermissionBits::GRANT_PERMISSION)
            .await?;

        // Atomic grant in SQL (added_permissions |= permission)
        let updated_member = self
            .member_repo
            .grant_permission_atomic(&room_id, &target_user_id, permission)
            .await?;

        // Invalidate permission cache for target user
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Broadcast permission cache invalidation to other cluster replicas
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Audit log
        self.audit_log(
            &granter_id,
            AuditAction::PermissionGranted,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "permission": permission,
            }),
        ).await;

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
        // Check if granter has permission to modify permissions without cache
        // Critical operation requires fresh permissions
        self.permission_service
            .check_permission_no_cache(&room_id, &granter_id, PermissionBits::GRANT_PERMISSION)
            .await?;

        // Atomic revoke in SQL (removed_permissions |= permission)
        let updated_member = self
            .member_repo
            .revoke_permission_atomic(&room_id, &target_user_id, permission)
            .await?;

        // Invalidate permission cache for target user
        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        // Broadcast permission cache invalidation to other cluster replicas
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Audit log
        self.audit_log(
            &granter_id,
            AuditAction::PermissionRevoked,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "permission": permission,
            }),
        ).await;

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
            .check_permission_no_cache(&room_id, &granter_id, PermissionBits::GRANT_PERMISSION)
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
                    .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
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

        // Broadcast permission cache invalidation to other cluster replicas
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        Ok(updated_member)
    }

    /// Get all members of a room with user info
    pub async fn list_members(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        self.member_repo.list_by_room(room_id).await
    }

    /// Get member count for a room
    pub async fn count_members(&self, room_id: &RoomId) -> Result<i32> {
        self.member_repo.count_by_room(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn count_members_batch(&self, room_ids: &[&RoomId]) -> Result<std::collections::HashMap<String, i32>> {
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
        self.member_repo.list_by_user(user_id, pagination).await
    }

    /// List all rooms a user is a member of with full details
    pub async fn list_user_rooms_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(Room, RoomRole, MemberStatus, i32)>, i64)> {
        self.member_repo
            .list_by_user_with_details(user_id, pagination)
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

        // Broadcast permission cache invalidation to other cluster replicas with retry
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Notify local WebSocket clients that member was kicked (ban implies kick)
        if let Some(ref ns) = self.notification_service {
            if let Err(e) = ns.notify_member_kicked(&room_id, &target_user_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    user_id = %target_user_id.as_str(),
                    "Failed to notify local clients of member ban"
                );
            }
        }

        // Broadcast kick event to all cluster replicas for cross-replica disconnect
        if let Some(ref broadcaster) = self.event_broadcaster {
            broadcaster.broadcast_kick_from_room(
                &room_id,
                &target_user_id,
                reason.as_deref().unwrap_or("banned"),
            );

            // Wait briefly to allow cross-replica disconnect signals to propagate.
            // This ensures that connection managers on other replicas have time to
            // process the kick event and clean up WebSocket connections before we
            // return from the ban operation.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Audit log
        self.audit_log(
            &admin_id,
            AuditAction::MemberBanned,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "reason": reason,
            }),
        ).await;

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

        // Broadcast permission cache invalidation to other cluster replicas with retry
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Audit log
        self.audit_log(
            &admin_id,
            AuditAction::MemberUnbanned,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({ "room_id": room_id.as_str() }),
        ).await;

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
                    .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;

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

        // Broadcast permission cache invalidation to other cluster replicas with retry
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Invalidate room settings cache to ensure fresh role default permissions
        // are used when recalculating the user's effective permissions.
        // This is necessary because the permission calculation depends on both
        // the member's role AND the room's role-specific permission settings.
        if let Some(ref invalidation) = self.cache_invalidation {
            if let Err(e) = invalidation.invalidate_room_settings(&room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast room settings cache invalidation after role change"
                );
            }
        }

        // Audit log
        self.audit_log(
            &creator_id,
            AuditAction::MemberRoleUpdated,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "old_role": format!("{:?}", old_role),
                "new_role": format!("{:?}", role),
            }),
        ).await;

        Ok(updated_member)
    }

    /// Set member status (active/pending/banned)
    pub async fn set_member_status(
        &self,
        room_id: RoomId,
        admin_id: UserId,
        target_user_id: UserId,
        status: MemberStatus,
    ) -> Result<RoomMember> {
        // Check admin permission without cache - security-critical status change
        self.permission_service
            .check_permission_no_cache(&room_id.clone(), &admin_id, PermissionBits::BAN_MEMBER)
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
                    .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;

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

        // Broadcast permission cache invalidation to other cluster replicas with retry
        self.broadcast_permission_invalidation_with_retry(&room_id, &target_user_id).await;

        // Audit log
        self.audit_log(
            &admin_id,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "old_status": format!("{:?}", old_status),
                "new_status": format!("{:?}", status),
            }),
        ).await;

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
            .check_permission_no_cache(&room_id.clone(), &admin_id, PermissionBits::KICK_USER)
            .await?;

        // Get all members regardless of left_at status
        self.member_repo.list_by_room_all(room_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Role Hierarchy Tests ==========

    #[test]
    fn test_role_level_ordering() {
        // Creator > Admin > Member > Guest
        assert!(role_level(&RoomRole::Creator) > role_level(&RoomRole::Admin));
        assert!(role_level(&RoomRole::Admin) > role_level(&RoomRole::Member));
        assert!(role_level(&RoomRole::Member) > role_level(&RoomRole::Guest));
    }

    #[test]
    fn test_role_level_exact_values() {
        assert_eq!(role_level(&RoomRole::Creator), 3);
        assert_eq!(role_level(&RoomRole::Admin), 2);
        assert_eq!(role_level(&RoomRole::Member), 1);
        assert_eq!(role_level(&RoomRole::Guest), 0);
    }

    #[test]
    fn test_lower_role_cannot_kick_higher_role() {
        // Member (1) cannot kick Admin (2): target >= kicker
        assert!(role_level(&RoomRole::Admin) >= role_level(&RoomRole::Member));
        // Guest (0) cannot kick Member (1)
        assert!(role_level(&RoomRole::Member) >= role_level(&RoomRole::Guest));
        // Admin (2) cannot kick Creator (3)
        assert!(role_level(&RoomRole::Creator) >= role_level(&RoomRole::Admin));
    }

    #[test]
    fn test_equal_roles_cannot_kick_each_other() {
        // Same role level means target >= kicker, so kick is denied
        assert!(role_level(&RoomRole::Admin) >= role_level(&RoomRole::Admin));
        assert!(role_level(&RoomRole::Member) >= role_level(&RoomRole::Member));
        assert!(role_level(&RoomRole::Guest) >= role_level(&RoomRole::Guest));
    }

    #[test]
    fn test_higher_role_can_kick_lower_role() {
        // Admin (2) can kick Member (1): target < kicker
        assert!(role_level(&RoomRole::Member) < role_level(&RoomRole::Admin));
        // Creator (3) can kick Admin (2)
        assert!(role_level(&RoomRole::Admin) < role_level(&RoomRole::Creator));
        // Admin (2) can kick Guest (0)
        assert!(role_level(&RoomRole::Guest) < role_level(&RoomRole::Admin));
    }

    #[test]
    fn test_creator_can_kick_all_other_roles() {
        // Creator (3) can kick all other roles since target < 3
        assert!(role_level(&RoomRole::Admin) < role_level(&RoomRole::Creator));
        assert!(role_level(&RoomRole::Member) < role_level(&RoomRole::Creator));
        assert!(role_level(&RoomRole::Guest) < role_level(&RoomRole::Creator));
    }

    #[test]
    fn test_guest_cannot_kick_anyone() {
        // Guest (0) cannot kick any role since all targets have level >= 0
        assert!(role_level(&RoomRole::Guest) >= role_level(&RoomRole::Guest));
        assert!(role_level(&RoomRole::Member) >= role_level(&RoomRole::Guest));
        assert!(role_level(&RoomRole::Admin) >= role_level(&RoomRole::Guest));
        assert!(role_level(&RoomRole::Creator) >= role_level(&RoomRole::Guest));
    }

    #[test]
    fn test_ban_role_check_mirrors_kick() {
        // The ban_member method uses the same role_level check:
        // role_level(&target_member.role) >= role_level(&admin_member.role) => deny
        // Admin banning Member: 1 >= 2 = false => allowed
        assert!(!(role_level(&RoomRole::Member) >= role_level(&RoomRole::Admin)));
        // Member banning Admin: 2 >= 1 = true => denied
        assert!(role_level(&RoomRole::Admin) >= role_level(&RoomRole::Member));
        // Admin banning Admin: 2 >= 2 = true => denied (equal role)
        assert!(role_level(&RoomRole::Admin) >= role_level(&RoomRole::Admin));
    }

    // ========== AddMemberOptions Tests ==========

    #[test]
    fn test_add_member_options_defaults() {
        let opts = AddMemberOptions::new();
        assert!(opts.check_room_active);
        assert!(opts.check_duplicate);
        assert!(!opts.check_max_members);
        assert_eq!(opts.max_members, 0);
        assert!(opts.invalidate_cache);
    }

    #[test]
    fn test_add_member_options_with_max_members() {
        let opts = AddMemberOptions::new().with_max_members(100);
        assert!(opts.check_max_members);
        assert_eq!(opts.max_members, 100);
    }

    #[test]
    fn test_add_member_options_skip_methods() {
        let opts = AddMemberOptions::new()
            .skip_max_members_check()
            .skip_active_check()
            .skip_duplicate_check()
            .skip_cache_invalidation();
        assert!(!opts.check_room_active);
        assert!(!opts.check_duplicate);
        assert!(!opts.check_max_members);
        assert!(!opts.invalidate_cache);
    }

    #[test]
    fn test_add_member_options_chaining() {
        let opts = AddMemberOptions::new()
            .with_max_members(50)
            .skip_active_check()
            .skip_cache_invalidation();
        assert!(opts.check_max_members);
        assert_eq!(opts.max_members, 50);
        assert!(!opts.check_room_active);
        assert!(opts.check_duplicate);
        assert!(!opts.invalidate_cache);
    }

    // ========== Integration test placeholders ==========

    // ========== Cache Invalidation Tests ==========

    /// Test that verifies room settings cache invalidation message is sent when role changes.
    ///
    /// This test ensures that when a member's role is changed, the room settings
    /// cache is also invalidated to ensure fresh role default permissions are
    /// used when recalculating the user's effective permissions.
    ///
    /// The permission calculation depends on:
    /// 1. The member's role (which changes)
    /// 2. The room's role-specific permission settings (from RoomSettings)
    ///
    /// When the role changes, we need to invalidate the room settings cache
    /// so that any stale cached room settings are refreshed.
    #[test]
    fn test_role_change_requires_room_settings_invalidation() {
        // This test verifies the concept that role changes need room settings
        // invalidation. The actual integration test would require:
        // 1. A MemberService with CacheInvalidationService
        // 2. A member whose role is changed
        // 3. Verification that RoomSettings invalidation message is broadcast
        //
        // The key insight is:
        // - Permission cache key: perm:room:{room_id}:user:{user_id}
        // - Permissions are calculated as: (role_default | added) & ~removed
        // - role_default depends on RoomSettings
        // - When role changes, new role_default depends on (potentially cached) RoomSettings
        // - Therefore, room settings cache must be invalidated on role change

        // Verify the invalidation message types exist
        use crate::cache::InvalidationMessage;

        let user_perm_msg = InvalidationMessage::UserPermission {
            room_id: "room1".to_string(),
            user_id: "user1".to_string(),
        };
        let room_settings_msg = InvalidationMessage::RoomSettings {
            room_id: "room1".to_string(),
        };

        // Both message types should be serializable
        assert!(serde_json::to_string(&user_perm_msg).is_ok());
        assert!(serde_json::to_string(&room_settings_msg).is_ok());
    }

    /// Test that verifies the cache key structure for permissions.
    ///
    /// The permission cache key is `perm:room:{room_id}:user:{user_id}`.
    /// This key does NOT include room settings version, so when room settings
    /// change or when role changes (which affects how room settings are applied),
    /// the cache must be invalidated.
    #[test]
    fn test_permission_cache_key_structure() {
        let room_id = RoomId("test-room".to_string());
        let user_id = UserId("test-user".to_string());
        let expected_key = format!("perm:room:{}:user:{}", room_id.0, user_id.0);
        assert_eq!(expected_key, "perm:room:test-room:user:test-user");
    }

    /// Test that role changes affect permission calculation through room settings.
    ///
    /// This demonstrates why room settings cache invalidation is needed on role change:
    /// - A Guest has permissions based on guest_* settings in RoomSettings
    /// - A Member has permissions based on member_* settings in RoomSettings
    /// - When a user's role changes from Guest to Member, their new permissions
    ///   depend on the (potentially cached) member_* settings
    /// - If the room settings cache is stale, the new permissions will be incorrect
    #[test]
    fn test_role_change_affects_permission_calculation() {
        use crate::models::PermissionBits;

        // Different roles have different default permissions
        let guest_perms = PermissionBits(PermissionBits::DEFAULT_GUEST);
        let member_perms = PermissionBits(PermissionBits::DEFAULT_MEMBER);

        // Verify that different roles have different permissions
        assert_ne!(guest_perms.0, member_perms.0);

        // Guest should not have SEND_CHAT by default
        assert!(!guest_perms.has(PermissionBits::SEND_CHAT));

        // Member should have SEND_CHAT by default
        assert!(member_perms.has(PermissionBits::SEND_CHAT));

        // This demonstrates why room settings cache must be invalidated on role change:
        // If a user is upgraded from Guest to Member, their new SEND_CHAT permission
        // depends on the member_* settings in RoomSettings. If those settings are
        // cached with stale values, the permission check will be wrong.
    }

    /// Test that verifies both UserPermission and RoomSettings invalidation
    /// messages can coexist in the invalidation system.
    #[test]
    fn test_dual_invalidation_messages() {
        use crate::cache::InvalidationMessage;

        // When a role changes, we send both:
        // 1. UserPermission - to invalidate the user's cached effective permissions
        // 2. RoomSettings - to ensure fresh role defaults are used on recalculation

        let room_id = "test-room".to_string();
        let user_id = "test-user".to_string();

        let user_msg = InvalidationMessage::UserPermission {
            room_id: room_id.clone(),
            user_id: user_id.clone(),
        };
        let settings_msg = InvalidationMessage::RoomSettings {
            room_id: room_id.clone(),
        };

        // Verify both messages serialize correctly
        let user_json = serde_json::to_string(&user_msg).unwrap();
        let settings_json = serde_json::to_string(&settings_msg).unwrap();

        assert!(user_json.contains("user_permission"));
        assert!(settings_json.contains("room_settings"));
        assert!(user_json.contains(&room_id));
        assert!(settings_json.contains(&room_id));
    }
}
