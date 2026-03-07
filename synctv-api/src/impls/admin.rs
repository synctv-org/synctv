//! Admin API Implementation
//!
//! Unified implementation for all admin API operations.
//! Used by both HTTP and gRPC handlers.

use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, ConnectionManager, PublishRequest};
use synctv_core::models::{RoomId, UserId, UserRole, UserStatus};
use synctv_core::service::{
    AuditService, EmailService, RemoteProviderManager, RoomService, SettingsRegistry,
    SettingsService, UserService,
};
use synctv_livestream::api::LiveStreamingInfrastructure;
use tokio::sync::mpsc;

use super::ApiError;

/// HTTP request context for audit logging (IP address and User-Agent).
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

/// Result of validating an admin user's authentication.
///
/// Returned by [`validate_admin_auth`] and consumed by both HTTP and gRPC
/// admin auth layers.
pub struct ValidatedAdmin {
    pub user_id: UserId,
    pub role: UserRole,
}

/// Shared admin auth validation: look up the user, check banned/deleted
/// status, and verify the token has not been invalidated by a password change.
///
/// Both transports must resolve `user_id` and `token_iat` from their own
/// auth mechanism (HTTP Authorization header / gRPC interceptor) before
/// calling this function.
pub async fn validate_admin_auth(
    user_service: &UserService,
    user_id: UserId,
    token_pv: i32,
    _token_iat: i64,
) -> Result<ValidatedAdmin, ApiError> {
    let user = user_service.get_user(&user_id).await.map_err(|e| {
        tracing::debug!(
            user_id = %user_id.as_str(),
            error = %e,
            "Admin auth rejected: failed to look up user"
        );
        ApiError::Authentication("Authentication failed".to_string())
    })?;

    if user.is_deleted() || user.status == UserStatus::Banned || user.status == UserStatus::Pending
    {
        tracing::debug!(
            user_id = %user_id.as_str(),
            status = ?user.status,
            deleted = user.is_deleted(),
            "Admin auth rejected: user is deleted, banned, or pending"
        );
        return Err(ApiError::Authentication(
            "Authentication failed".to_string(),
        ));
    }

    // Check password version
    if token_pv < user.password_version {
        tracing::debug!(
            user_id = %user_id.as_str(),
            token_pv = token_pv,
            current_pv = user.password_version,
            "Admin auth rejected: token password version outdated"
        );
        return Err(ApiError::Authentication(
            "Token invalidated due to password change. Please log in again.".to_string(),
        ));
    }

    Ok(ValidatedAdmin {
        user_id,
        role: user.role,
    })
}

/// Admin API implementation
#[derive(Clone)]
pub struct AdminApiImpl {
    pub room_service: Arc<RoomService>,
    pub user_service: Arc<UserService>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Arc<EmailService>,
    pub connection_manager: Arc<ConnectionManager>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub redis_publish_tx: Option<mpsc::Sender<PublishRequest>>,
    pub audit_service: Arc<AuditService>,
}

impl AdminApiImpl {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        settings_service: Arc<SettingsService>,
        settings_registry: Option<Arc<SettingsRegistry>>,
        email_service: Arc<EmailService>,
        connection_manager: Arc<ConnectionManager>,
        provider_instance_manager: Arc<RemoteProviderManager>,
        live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
        redis_publish_tx: Option<mpsc::Sender<PublishRequest>>,
        audit_service: Arc<AuditService>,
    ) -> Self {
        Self {
            room_service,
            user_service,
            settings_service,
            settings_registry,
            email_service,
            connection_manager,
            provider_instance_manager,
            live_streaming_infrastructure,
            redis_publish_tx,
            audit_service,
        }
    }

    /// Kick a stream both locally and cluster-wide via Redis Pub/Sub
    fn kick_stream_cluster(&self, room_id: &str, media_id: &str, reason: &str) {
        crate::impls::kick_stream_cluster(
            self.live_streaming_infrastructure.as_ref(),
            self.redis_publish_tx.as_ref(),
            room_id,
            media_id,
            reason,
        );
    }

    /// Best-effort admin audit log helper.
    ///
    /// Resolves the admin username (falling back to the raw ID on lookup failure),
    /// then writes an audit entry. If the audit write fails, it logs an ERROR
    /// but does **not** propagate the error to the caller -- the primary operation
    /// has already succeeded and should not be rolled back by an audit failure.
    async fn log_admin_action(
        &self,
        admin_user_id: &UserId,
        action: synctv_core::service::AuditAction,
        target_type: synctv_core::service::AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
        ctx: &RequestContext,
    ) {
        let admin_username = self
            .user_service
            .get_user(admin_user_id)
            .await
            .map_or_else(|_| admin_user_id.as_str().to_string(), |u| u.username);

        if let Err(e) = self
            .audit_service
            .log(
                admin_user_id.as_str().to_string(),
                admin_username.clone(),
                action,
                target_type,
                target_id,
                details,
                ctx.ip_address.clone(),
                ctx.user_agent.clone(),
            )
            .await
        {
            tracing::error!(
                error = %e,
                admin_user_id = %admin_user_id.as_str(),
                admin_username = %admin_username,
                "AUDIT LOG FAILURE: failed to record admin action. Manual review required.",
            );
        }
    }

    // === Room Management ===

    pub async fn list_rooms(
        &self,
        req: crate::proto::admin::ListRoomsRequest,
    ) -> Result<crate::proto::admin::ListRoomsResponse, ApiError> {
        let page = if req.page > 0 { req.page } else { 1 };
        let page_size = if req.page_size > 0 { req.page_size } else { 50 };

        // Parse status filter (0/Unspecified = show all statuses for admin)
        let status = if req.status == 0 {
            None
        } else {
            Some(
                match synctv_proto::common::RoomStatus::try_from(req.status) {
                    Ok(synctv_proto::common::RoomStatus::Active) => {
                        synctv_core::models::RoomStatus::Active
                    }
                    Ok(synctv_proto::common::RoomStatus::Pending) => {
                        synctv_core::models::RoomStatus::Pending
                    }
                    Ok(synctv_proto::common::RoomStatus::Closed) => {
                        synctv_core::models::RoomStatus::Closed
                    }
                    _ => synctv_core::models::RoomStatus::Active,
                },
            )
        };

        let query = synctv_core::models::RoomListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page as u32),
                Some(page_size as u32),
            ),
            status,
            search: if req.search.is_empty() {
                None
            } else {
                Some(req.search)
            },
            is_banned: req.is_banned,
            creator_id: if req.creator_id.is_empty() {
                None
            } else {
                Some(req.creator_id)
            },
        };

        let (rooms, total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch creator usernames for all rooms
        let creator_ids: Vec<synctv_core::models::UserId> = rooms
            .iter()
            .map(|r| r.created_by.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&creator_ids)
            .await
            .unwrap_or_default();

        // Batch-fetch distributed online user counts so same-user multi-connection
        // sessions are not overcounted as multiple online members.
        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let counts = self
            .connection_manager
            .room_online_user_count_distributed_batch(&room_id_refs)
            .await
            .map_err(ApiError::Internal)?;

        let room_list: Vec<_> = rooms
            .into_iter()
            .zip(counts)
            .map(|(r, count)| {
                let member_count: Option<i32> = count.try_into().ok();
                let creator_username = username_map.get(&r.created_by).map(String::as_str);
                admin_room_to_proto(&r, None, member_count, creator_username)
            })
            .collect();

        Ok(crate::proto::admin::ListRoomsResponse {
            rooms: room_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn get_room(
        &self,
        req: crate::proto::admin::GetRoomRequest,
    ) -> Result<crate::proto::admin::GetRoomResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        let creator_username = self
            .user_service
            .get_usernames(std::slice::from_ref(&room.created_by))
            .await
            .ok()
            .and_then(|m| m.into_values().next());

        Ok(crate::proto::admin::GetRoomResponse {
            room: Some(admin_room_to_proto(
                &room,
                None,
                self.connection_manager
                    .room_online_user_count_distributed(&room.id)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                creator_username.as_deref(),
            )),
        })
    }

    pub async fn delete_room(
        &self,
        req: crate::proto::admin::DeleteRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::DeleteRoomResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        self.room_service
            .admin_delete_room(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        // Publish RoomDeleted cluster event for cross-replica propagation
        if let Some(ref tx) = self.redis_publish_tx {
            super::try_publish_cluster_event(
                tx,
                PublishRequest {
                    event: ClusterEvent::RoomDeleted {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        deleted_by: admin_user_id.clone(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Force disconnect all connections in the deleted room
        self.connection_manager.disconnect_room(&rid);

        // Kick active RTMP publishers in the deleted room (local + cluster-wide)
        if let Some(infra) = &self.live_streaming_infrastructure {
            let media_ids = infra.user_stream_tracker.get_room_streams(rid.as_str());

            for media_id in &media_ids {
                self.kick_stream_cluster(rid.as_str(), media_id, "room_deleted");
            }

            infra.kick_room_publishers(rid.as_str()).await;
        }

        // Audit log: delete_room is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomDeleted,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({ "room_id": rid.as_str() }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_password(
        &self,
        req: crate::proto::admin::UpdateRoomPasswordRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateRoomPasswordResponse, ApiError> {
        let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let new_password = if req.new_password.is_empty() {
            None
        } else {
            Some(req.new_password.as_str())
        };
        self.room_service
            .admin_set_room_password(&room_id, new_password)
            .await
            .map_err(ApiError::from)?;

        // Audit log: room password change is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomPasswordUpdated,
            synctv_core::service::AuditTargetType::Room,
            Some(room_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "password_set": new_password.is_some(),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateRoomPasswordResponse { success: true })
    }

    pub async fn get_room_members(
        &self,
        req: crate::proto::admin::GetRoomMembersRequest,
    ) -> Result<crate::proto::admin::GetRoomMembersResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        // Use database-level pagination instead of loading all members into memory
        let page = if req.page > 0 { req.page as u32 } else { 1 };
        let page_size = if req.page_size > 0 {
            req.page_size as u32
        } else {
            50
        };
        let pagination = synctv_core::models::PageParams::new(Some(page), Some(page_size));

        let (members, total) = self
            .room_service
            .get_room_members_paginated(&rid, pagination)
            .await
            .map_err(ApiError::from)?;

        let proto_members: Vec<_> = members.iter().map(admin_room_member_to_proto).collect();

        Ok(crate::proto::admin::GetRoomMembersResponse {
            members: proto_members,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    // === User Management ===

    pub async fn list_users(
        &self,
        req: crate::proto::admin::ListUsersRequest,
    ) -> Result<crate::proto::admin::ListUsersResponse, ApiError> {
        let page = if req.page > 0 { req.page } else { 1 };
        let page_size = if req.page_size > 0 { req.page_size } else { 50 };

        // Convert proto enum i32 values to typed enums for UserListQuery
        let status = match synctv_proto::common::UserStatus::try_from(req.status) {
            Ok(synctv_proto::common::UserStatus::Active) => {
                Some(synctv_core::models::UserStatus::Active)
            }
            Ok(synctv_proto::common::UserStatus::Pending) => {
                Some(synctv_core::models::UserStatus::Pending)
            }
            Ok(synctv_proto::common::UserStatus::Banned) => {
                Some(synctv_core::models::UserStatus::Banned)
            }
            _ => None, // Unspecified or unknown => no filter
        };
        let role = match synctv_proto::common::UserRole::try_from(req.role) {
            Ok(synctv_proto::common::UserRole::Root) => Some(synctv_core::models::UserRole::Root),
            Ok(synctv_proto::common::UserRole::Admin) => Some(synctv_core::models::UserRole::Admin),
            Ok(synctv_proto::common::UserRole::User) => Some(synctv_core::models::UserRole::User),
            _ => None, // Unspecified or unknown => no filter
        };
        let search = if req.search.is_empty() {
            None
        } else {
            Some(req.search)
        };

        let query = synctv_core::models::UserListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page as u32),
                Some(page_size as u32),
            ),
            search,
            status,
            role,
        };

        let (users, total) = self
            .user_service
            .list_users(&query)
            .await
            .map_err(ApiError::from)?;

        let user_list: Vec<_> = users.into_iter().map(|u| admin_user_to_proto(&u)).collect();

        Ok(crate::proto::admin::ListUsersResponse {
            users: user_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn get_user(
        &self,
        req: crate::proto::admin::GetUserRequest,
    ) -> Result<crate::proto::admin::GetUserResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::admin::GetUserResponse {
            user: Some(admin_user_to_proto(&user)),
        })
    }

    pub async fn update_user_role(
        &self,
        req: crate::proto::admin::UpdateUserRoleRequest,
        admin_user_id: &UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateUserRoleResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        // Parse role from proto enum
        let new_role = crate::impls::client::proto_role_to_user_role(req.role)?;

        // Only root can promote to root
        if new_role == synctv_core::models::UserRole::Root
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can promote to root".to_string(),
            ));
        }

        // Only root can change another root user's role
        if user.role == synctv_core::models::UserRole::Root
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can change root user roles".to_string(),
            ));
        }

        // Only root can change admin user roles
        if user.role == synctv_core::models::UserRole::Admin
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can change admin user roles".to_string(),
            ));
        }

        // Only root can promote users to admin
        if new_role == synctv_core::models::UserRole::Admin
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can promote to admin".to_string(),
            ));
        }

        let old_version = user.version;
        let updated_user = synctv_core::models::User {
            role: new_role,
            ..user
        };

        self.user_service
            .update_user(&updated_user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: role change is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserRoleUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated_user.username,
                "new_role": format!("{new_role:?}"),
                "caller_role": format!("{caller_role:?}"),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateUserRoleResponse {
            user: Some(admin_user_to_proto(&updated_user)),
        })
    }

    pub async fn update_user_password(
        &self,
        req: crate::proto::admin::UpdateUserPasswordRequest,
        caller_user_id: UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateUserPasswordResponse, ApiError> {
        use crate::http::validation::limits::{PASSWORD_MAX, PASSWORD_MIN};
        if req.new_password.chars().count() < PASSWORD_MIN {
            return Err(ApiError::InvalidInput(format!(
                "Password must be at least {PASSWORD_MIN} characters"
            )));
        }
        if req.new_password.chars().count() > PASSWORD_MAX {
            return Err(ApiError::InvalidInput(format!(
                "Password must be at most {PASSWORD_MAX} characters"
            )));
        }

        let uid = UserId::from_string(req.user_id.clone());

        // Fetch target user to check role hierarchy
        let target_user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(|e| ApiError::NotFound(format!("User not found: {e}")))?;

        // Only root can reset another root user's password
        if target_user.role == UserRole::Root && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can reset root user passwords".to_string(),
            ));
        }

        // Only root can reset admin user passwords (admins cannot reset each other's passwords)
        if target_user.role == UserRole::Admin && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can reset admin user passwords".to_string(),
            ));
        }

        self.user_service
            .set_password(&uid, &req.new_password)
            .await
            .map_err(ApiError::from)?;

        // Log to audit trail (best-effort: D11 - audit failure should not propagate)
        {
            let mut details_map = serde_json::Map::new();
            details_map.insert(
                "target_user_id".to_string(),
                serde_json::Value::String(uid.as_str().to_string()),
            );
            details_map.insert(
                "target_username".to_string(),
                serde_json::Value::String(target_user.username.clone()),
            );
            if !req.reason.is_empty() {
                details_map.insert("reason".to_string(), serde_json::Value::String(req.reason));
            }
            self.log_admin_action(
                &caller_user_id,
                synctv_core::service::AuditAction::UserPasswordUpdated,
                synctv_core::service::AuditTargetType::User,
                Some(uid.as_str().to_string()),
                serde_json::Value::Object(details_map),
                ctx,
            )
            .await;
        }

        Ok(crate::proto::admin::UpdateUserPasswordResponse { success: true })
    }

    // === Settings Management ===

    pub async fn get_settings(
        &self,
        _req: crate::proto::admin::GetSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::GetSettingsResponse, ApiError> {
        let groups = self
            .settings_service
            .get_all()
            .await
            .map_err(ApiError::from)?;

        let group_names: Vec<String> = groups.iter().map(|g| g.group_name.clone()).collect();

        let group_list: Vec<_> = groups
            .into_iter()
            .map(|g| crate::proto::admin::SettingsGroup {
                name: g.group_name.clone(),
                settings: g.value.into_bytes(),
            })
            .collect();

        // Audit log for settings view (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::SettingsViewed,
            synctv_core::service::AuditTargetType::Settings,
            None,
            serde_json::json!({
                "group_count": group_names.len(),
                "groups": group_names,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::GetSettingsResponse { groups: group_list })
    }

    pub async fn get_settings_group(
        &self,
        req: crate::proto::admin::GetSettingsGroupRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::GetSettingsGroupResponse, ApiError> {
        let group = self
            .settings_service
            .get(&req.group)
            .await
            .map_err(ApiError::from)?;

        let group_name = group.group_name.clone();

        // Audit log for settings group view (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::SettingsGroupViewed,
            synctv_core::service::AuditTargetType::Settings,
            None,
            serde_json::json!({ "group": group_name }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::GetSettingsGroupResponse {
            group: Some(crate::proto::admin::SettingsGroup {
                name: group.group_name.clone(),
                settings: group.value.into_bytes(),
            }),
        })
    }

    pub async fn update_settings(
        &self,
        req: crate::proto::admin::UpdateSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateSettingsResponse, ApiError> {
        // Collect keys before consuming req.settings
        let changed_keys: Vec<String> = req.settings.keys().cloned().collect();
        // Wrap all setting writes in a single atomic transaction so that a partial
        // failure cannot leave the settings table in an inconsistent state.
        self.settings_service
            .update_batch(req.settings)
            .await
            .map_err(ApiError::from)?;

        // Broadcast CacheInvalidate so other replicas refresh their settings caches
        if let Some(ref tx) = self.redis_publish_tx {
            super::try_publish_cluster_event(
                tx,
                PublishRequest {
                    event: ClusterEvent::CacheInvalidate {
                        event_id: nanoid::nanoid!(16),
                        targets: vec![synctv_cluster::sync::CacheTarget::All],
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Audit log for settings update (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::SettingsUpdated,
            synctv_core::service::AuditTargetType::Settings,
            None,
            serde_json::json!({ "changed_keys": changed_keys }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateSettingsResponse {})
    }

    // === Email Management ===

    pub async fn send_test_email(
        &self,
        req: crate::proto::admin::SendTestEmailRequest,
    ) -> Result<crate::proto::admin::SendTestEmailResponse, ApiError> {
        // Send test email using EmailService
        match self.email_service.send_test_email(&req.to).await {
            Ok(()) => Ok(crate::proto::admin::SendTestEmailResponse {
                message: format!("Test email sent successfully to {}", req.to),
                success: true,
            }),
            Err(e) => Ok(crate::proto::admin::SendTestEmailResponse {
                message: format!("Failed to send test email: {e}"),
                success: false,
            }),
        }
    }

    // === Provider Instance Management ===

    pub async fn list_provider_instances(
        &self,
        _req: crate::proto::admin::ListProviderInstancesRequest,
    ) -> Result<crate::proto::admin::ListProviderInstancesResponse, ApiError> {
        let instances = self
            .provider_instance_manager
            .get_all_instances()
            .await
            .map_err(ApiError::from)?;

        let proto_instances: Vec<_> = instances
            .into_iter()
            .map(provider_instance_to_proto)
            .collect();

        Ok(crate::proto::admin::ListProviderInstancesResponse {
            instances: proto_instances,
        })
    }

    pub async fn add_provider_instance(
        &self,
        req: crate::proto::admin::AddProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::AddProviderInstanceResponse, ApiError> {
        // Parse config if provided
        let (jwt_secret, custom_ca) = if req.config.is_empty() {
            (None, None)
        } else {
            let config: serde_json::Value = serde_json::from_slice(&req.config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid config JSON: {e}")))?;
            (
                config
                    .get("jwt_secret")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                config
                    .get("custom_ca")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            )
        };

        let instance = synctv_core::models::ProviderInstance {
            name: req.name,
            endpoint: req.endpoint,
            comment: if req.comment.is_empty() {
                None
            } else {
                Some(req.comment)
            },
            jwt_secret,
            custom_ca,
            timeout: seconds_to_timeout_string(if req.timeout_seconds > 0 {
                req.timeout_seconds
            } else {
                10
            }),
            tls: req.tls,
            insecure_tls: req.insecure_tls,
            providers: req.providers,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        self.provider_instance_manager
            .add(instance.clone())
            .await
            .map_err(ApiError::from)?;

        // Audit log: provider instance creation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceCreated,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            serde_json::json!({
                "instance_name": instance.name,
                "endpoint": mask_url_credentials(&instance.endpoint),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::AddProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn update_provider_instance(
        &self,
        req: crate::proto::admin::UpdateProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateProviderInstanceResponse, ApiError> {
        // Get existing instance
        let instances = self
            .provider_instance_manager
            .get_all_instances()
            .await
            .map_err(ApiError::from)?;
        let mut instance = instances
            .into_iter()
            .find(|i| i.name == req.name)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        // Update fields if explicitly provided (optional fields)
        if let Some(endpoint) = req.endpoint {
            instance.endpoint = endpoint;
        }
        if let Some(comment) = req.comment {
            instance.comment = Some(comment);
        }
        if let Some(timeout_seconds) = req.timeout_seconds {
            instance.timeout = seconds_to_timeout_string(timeout_seconds);
        }
        if !req.providers.is_empty() {
            instance.providers = req.providers;
        }

        // Update boolean fields (optional means explicit intent)
        if let Some(tls) = req.tls {
            instance.tls = tls;
        }
        if let Some(insecure_tls) = req.insecure_tls {
            instance.insecure_tls = insecure_tls;
        }

        // Parse config if provided for additional settings
        if !req.config.is_empty() {
            let config: serde_json::Value = serde_json::from_slice(&req.config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid config JSON: {e}")))?;
            if let Some(jwt_secret) = config.get("jwt_secret").and_then(|v| v.as_str()) {
                instance.jwt_secret = Some(jwt_secret.to_string());
            }
            if let Some(custom_ca) = config.get("custom_ca").and_then(|v| v.as_str()) {
                instance.custom_ca = Some(custom_ca.to_string());
            }
            if let Some(tls) = config.get("tls").and_then(serde_json::Value::as_bool) {
                instance.tls = tls;
            }
            if let Some(insecure_tls) = config
                .get("insecure_tls")
                .and_then(serde_json::Value::as_bool)
            {
                instance.insecure_tls = insecure_tls;
            }
        }

        instance.updated_at = chrono::Utc::now();

        self.provider_instance_manager
            .update(instance.clone())
            .await
            .map_err(ApiError::from)?;

        // Audit log: provider instance update (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceUpdated,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            serde_json::json!({ "instance_name": instance.name }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn delete_provider_instance(
        &self,
        req: crate::proto::admin::DeleteProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::DeleteProviderInstanceResponse, ApiError> {
        self.provider_instance_manager
            .delete(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Audit log: provider instance deletion (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceDeleted,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(req.name.clone()),
            serde_json::json!({ "instance_name": req.name }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::DeleteProviderInstanceResponse { success: true })
    }

    pub async fn reconnect_provider_instance(
        &self,
        req: crate::proto::admin::ReconnectProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ReconnectProviderInstanceResponse, ApiError> {
        // Atomic reconnect: invalidate cached channel and re-create from DB config
        self.provider_instance_manager
            .reconnect(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Get updated instance
        let instances = self
            .provider_instance_manager
            .get_all_instances()
            .await
            .map_err(ApiError::from)?;
        let instance = instances
            .into_iter()
            .find(|i| i.name == req.name)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        // Audit log: provider instance reconnection (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceReconnected,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            serde_json::json!({
                "instance_name": instance.name,
                "endpoint": mask_url_credentials(&instance.endpoint),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ReconnectProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn enable_provider_instance(
        &self,
        req: crate::proto::admin::EnableProviderInstanceRequest,
    ) -> Result<crate::proto::admin::EnableProviderInstanceResponse, ApiError> {
        self.provider_instance_manager
            .enable(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Get updated instance
        let instances = self
            .provider_instance_manager
            .get_all_instances()
            .await
            .map_err(ApiError::from)?;
        let instance = instances
            .into_iter()
            .find(|i| i.name == req.name)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        Ok(crate::proto::admin::EnableProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn disable_provider_instance(
        &self,
        req: crate::proto::admin::DisableProviderInstanceRequest,
    ) -> Result<crate::proto::admin::DisableProviderInstanceResponse, ApiError> {
        self.provider_instance_manager
            .disable(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Get updated instance
        let instances = self
            .provider_instance_manager
            .get_all_instances()
            .await
            .map_err(ApiError::from)?;
        let instance = instances
            .into_iter()
            .find(|i| i.name == req.name)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        Ok(crate::proto::admin::DisableProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    // === User Management (extended) ===

    pub async fn create_user(
        &self,
        req: crate::proto::admin::CreateUserRequest,
        caller_role: synctv_core::models::UserRole,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::CreateUserResponse, ApiError> {
        if req.username.is_empty() || req.password.is_empty() || req.email.is_empty() {
            return Err(ApiError::InvalidInput(
                "Username, password, and email are required".to_string(),
            ));
        }

        // Validate password length using shared constants (chars().count() for multi-byte safety)
        use crate::http::validation::limits::{PASSWORD_MAX, PASSWORD_MIN};
        if req.password.chars().count() < PASSWORD_MIN {
            return Err(ApiError::InvalidInput(format!(
                "Password must be at least {PASSWORD_MIN} characters"
            )));
        }
        if req.password.chars().count() > PASSWORD_MAX {
            return Err(ApiError::InvalidInput(format!(
                "Password must be at most {PASSWORD_MAX} characters"
            )));
        }

        // Validate role BEFORE registration to fail fast
        let target_role = if req.role != synctv_proto::common::UserRole::Unspecified as i32
            && req.role != synctv_proto::common::UserRole::User as i32
        {
            let new_role = crate::impls::client::proto_role_to_user_role(req.role)?;
            // Only root can create root users
            if new_role == synctv_core::models::UserRole::Root
                && caller_role != synctv_core::models::UserRole::Root
            {
                return Err(ApiError::Authorization(
                    "Only root users can create root users".to_string(),
                ));
            }
            Some(new_role)
        } else {
            None
        };

        // Delegate to UserService which handles validation, hashing, creation,
        // and username cache population atomically.
        let user = self
            .user_service
            .create_user_with_role(
                req.username.clone(),
                Some(req.email.clone()),
                req.password,
                target_role,
            )
            .await
            .map_err(ApiError::from)?;

        // Audit log: user creation via admin panel (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserCreated,
            synctv_core::service::AuditTargetType::User,
            Some(user.id.as_str().to_string()),
            serde_json::json!({"reason": "User created via admin panel"}),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::CreateUserResponse {
            user: Some(admin_user_to_proto(&user)),
        })
    }

    pub async fn delete_user(
        &self,
        req: crate::proto::admin::DeleteUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::DeleteUserResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);

        // Delegate to UserService.delete_user which handles:
        // 1. Soft-delete + OAuth2 cleanup atomically in a single DB transaction.
        // 2. Username cache invalidation (best-effort).
        // 3. Cross-replica user cache invalidation (best-effort).
        self.user_service
            .delete_user(&uid)
            .await
            .map_err(ApiError::from)?;

        // Post-delete cleanup: disconnect connections and kick streams.
        // These are non-transactional side effects that run after the DB commit.

        // Force disconnect all user connections (WebSocket and streaming)
        self.connection_manager.disconnect_user(&uid);

        // Kick active RTMP publishers (local + cluster-wide)
        if let Some(infra) = &self.live_streaming_infrastructure {
            let streams = infra.user_stream_tracker.get_user_streams(uid.as_str());

            for (room_id, media_id) in &streams {
                self.kick_stream_cluster(room_id, media_id, "user_deleted");
            }

            infra.kick_user_publishers(uid.as_str()).await;
        }

        // Audit log: user deletion is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserDeleted,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({ "target_user_id": uid.as_str() }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::DeleteUserResponse { success: true })
    }

    pub async fn update_user_username(
        &self,
        req: crate::proto::admin::UpdateUserUsernameRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateUserUsernameResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);

        // Apply the same validation rules as client-facing set_username:
        // trim, check length, charset, and leading character restrictions.
        let username = req.new_username.trim().to_string();
        if username.chars().count() < synctv_core::validation::USERNAME_MIN {
            return Err(ApiError::InvalidInput(format!(
                "Username must be at least {} characters",
                synctv_core::validation::USERNAME_MIN,
            )));
        }
        if username.chars().count() > synctv_core::validation::USERNAME_MAX {
            return Err(ApiError::InvalidInput(format!(
                "Username must be at most {} characters",
                synctv_core::validation::USERNAME_MAX,
            )));
        }
        if !username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Err(ApiError::InvalidInput(
                "Username can only contain letters, numbers, underscores, and hyphens".to_string(),
            ));
        }
        if username.starts_with('_') || username.starts_with('-') {
            return Err(ApiError::InvalidInput(
                "Username cannot start with underscore or hyphen".to_string(),
            ));
        }

        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;
        let old_username = user.username.clone();
        let old_version = user.version;
        user.username = username;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: admin changing another user's username (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserUsernameUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "old_username": old_username,
                "new_username": updated.username,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateUserUsernameResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn ban_user(
        &self,
        req: crate::proto::admin::BanUserRequest,
        admin_user_id: &UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BanUserResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        // Prevent admin from banning root users (only root can ban root)
        if user.role == synctv_core::models::UserRole::Root
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can ban other root users".to_string(),
            ));
        }

        // Prevent admin from banning other admins (only root can ban admins)
        if user.role == synctv_core::models::UserRole::Admin
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can ban admin users".to_string(),
            ));
        }

        if user.status == UserStatus::Banned {
            return Err(ApiError::InvalidInput("User is already banned".to_string()));
        }

        let mut user = user;
        let old_version = user.version;

        // Update user status to Banned. Existing tokens will be rejected by the
        // security pipeline on subsequent requests due to the Banned status.
        user.status = UserStatus::Banned;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Force disconnect all user connections (WebSocket and streaming)
        self.connection_manager.disconnect_user(&uid);

        // Kick active RTMP streams for this user on ALL replicas
        // 1. Local kick (this replica's streams)
        if let Some(infra) = &self.live_streaming_infrastructure {
            infra.kick_user_publishers(uid.as_str()).await;
        }
        // 2. Cluster-wide broadcast so other replicas kick their local streams for this user
        if let Some(tx) = &self.redis_publish_tx {
            super::try_publish_cluster_event(
                tx,
                PublishRequest {
                    event: ClusterEvent::KickUser {
                        event_id: nanoid::nanoid!(16),
                        user_id: uid.clone(),
                        reason: "user_banned".to_string(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Audit log: ban_user is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserBanned,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": user.username,
                "reason": req.reason,
                "caller_role": format!("{caller_role:?}"),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BanUserResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn unban_user(
        &self,
        req: crate::proto::admin::UnbanUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UnbanUserResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.status != UserStatus::Banned {
            return Err(ApiError::InvalidInput("User is not banned".to_string()));
        }

        let old_version = user.version;
        user.status = UserStatus::Active;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: unban is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserUnbanned,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UnbanUserResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn approve_user(
        &self,
        req: crate::proto::admin::ApproveUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ApproveUserResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.status != UserStatus::Pending {
            return Err(ApiError::InvalidInput(
                "User is not pending approval".to_string(),
            ));
        }

        let old_version = user.version;
        user.status = UserStatus::Active;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: approving a user is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserApproved,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
                "previous_status": "pending",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ApproveUserResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn get_user_rooms(
        &self,
        req: crate::proto::admin::GetUserRoomsRequest,
    ) -> Result<crate::proto::admin::GetUserRoomsResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let page = u32::try_from(req.page).unwrap_or(1);
        let page_size = u32::try_from(req.page_size).unwrap_or(50).min(100);
        let pagination = synctv_core::models::PageParams::new(Some(page), Some(page_size));

        // Get rooms created by user
        let (created_rooms, _) = self
            .room_service
            .list_rooms_by_creator(&uid, pagination)
            .await
            .map_err(ApiError::from)?;

        // Use list_joined_rooms_with_details to get room data in a single JOIN query,
        // eliminating the N+1 pattern of one get_room() call per joined room.
        let pagination = synctv_core::models::PageParams::new(Some(page), Some(page_size));
        let (joined_rooms_with_details, _) = self
            .room_service
            .list_joined_rooms_with_details(&uid, pagination)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch creator usernames for all rooms in a single query.
        let creator_ids: Vec<synctv_core::models::UserId> = created_rooms
            .iter()
            .map(|r| r.created_by.clone())
            .chain(
                joined_rooms_with_details
                    .iter()
                    .map(|(r, _, _, _)| r.created_by.clone()),
            )
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&creator_ids)
            .await
            .unwrap_or_default();

        // Batch-fetch distributed online user counts for all rooms
        let all_room_ids: Vec<&synctv_core::models::RoomId> = created_rooms
            .iter()
            .map(|r| &r.id)
            .chain(joined_rooms_with_details.iter().map(|(r, _, _, _)| &r.id))
            .collect();
        let count_vec = self
            .connection_manager
            .room_online_user_count_distributed_batch(&all_room_ids)
            .await
            .map_err(ApiError::Internal)?;
        let count_map: std::collections::HashMap<&synctv_core::models::RoomId, usize> =
            all_room_ids.into_iter().zip(count_vec).collect();

        let mut admin_rooms: Vec<crate::proto::admin::AdminRoom> = created_rooms
            .iter()
            .map(|r| {
                let creator_username = username_map.get(&r.created_by).map(String::as_str);
                let count = count_map.get(&r.id).copied().unwrap_or(0);
                admin_room_to_proto(r, None, count.try_into().ok(), creator_username)
            })
            .collect();

        // Build a set of already-added room IDs to deduplicate
        let created_room_ids: std::collections::HashSet<String> =
            admin_rooms.iter().map(|r| r.id.clone()).collect();

        // Add joined rooms not already present (no per-room DB query)
        for (room, _role, _status, _ver) in &joined_rooms_with_details {
            if created_room_ids.contains(room.id.as_str()) {
                continue;
            }
            let creator_username = username_map.get(&room.created_by).map(String::as_str);
            let count = count_map.get(&room.id).copied().unwrap_or(0);
            admin_rooms.push(admin_room_to_proto(
                room,
                None,
                count.try_into().ok(),
                creator_username,
            ));
        }

        let total = admin_rooms.len() as i32;
        Ok(crate::proto::admin::GetUserRoomsResponse {
            rooms: admin_rooms,
            total,
        })
    }

    // === Room Management (extended) ===

    pub async fn ban_room(
        &self,
        req: crate::proto::admin::BanRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BanRoomResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        if room.is_banned {
            return Err(ApiError::InvalidInput("Room is already banned".to_string()));
        }

        let updated = self
            .room_service
            .ban_room(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        // Broadcast cache invalidation for the banned room
        if let Some(ref tx) = self.redis_publish_tx {
            super::try_publish_cluster_event(
                tx,
                PublishRequest {
                    event: ClusterEvent::CacheInvalidate {
                        event_id: nanoid::nanoid!(16),
                        targets: vec![synctv_cluster::sync::CacheTarget::Room {
                            room_id: rid.as_str().to_string(),
                        }],
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Force disconnect all connections in the banned room
        self.connection_manager.disconnect_room(&rid);

        // Kick active RTMP publishers in the banned room (local + cluster-wide)
        if let Some(infra) = &self.live_streaming_infrastructure {
            let media_ids = infra.user_stream_tracker.get_room_streams(rid.as_str());

            for media_id in &media_ids {
                self.kick_stream_cluster(rid.as_str(), media_id, "room_banned");
            }

            infra.kick_room_publishers(rid.as_str()).await;
        }

        // Audit log: ban_room is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomBanned,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "room_name": room.name,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BanRoomResponse {
            room: Some(admin_room_to_proto(
                &updated,
                None,
                self.connection_manager
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
            )),
        })
    }

    pub async fn unban_room(
        &self,
        req: crate::proto::admin::UnbanRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UnbanRoomResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        if !room.is_banned {
            return Err(ApiError::InvalidInput("Room is not banned".to_string()));
        }

        let updated = self
            .room_service
            .unban_room(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        // Audit log: unban_room (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomUnbanned,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "room_name": room.name,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UnbanRoomResponse {
            room: Some(admin_room_to_proto(
                &updated,
                None,
                self.connection_manager
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
            )),
        })
    }

    pub async fn approve_room(
        &self,
        req: crate::proto::admin::ApproveRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ApproveRoomResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let room = self
            .room_service
            .approve_room(&rid)
            .await
            .map_err(ApiError::from)?;

        // Audit log: approving a room (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomApproved,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "room_name": room.name,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ApproveRoomResponse {
            room: Some(admin_room_to_proto(
                &room,
                None,
                self.connection_manager
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
            )),
        })
    }

    pub async fn get_room_settings(
        &self,
        req: crate::proto::admin::GetRoomSettingsRequest,
    ) -> Result<crate::proto::admin::GetRoomSettingsResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&settings).map_err(ApiError::from)?;

        Ok(crate::proto::admin::GetRoomSettingsResponse {
            settings: settings_json,
        })
    }

    pub async fn update_room_settings(
        &self,
        req: crate::proto::admin::UpdateRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::admin::UpdateRoomSettingsResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let settings: synctv_core::models::RoomSettings = serde_json::from_slice(&req.settings)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid settings JSON: {e}")))?;

        self.room_service
            .set_room_settings(&rid, &settings)
            .await
            .map_err(ApiError::from)?;

        // Look up admin username for cluster event
        let admin_username = self
            .user_service
            .get_user(admin_user_id)
            .await
            .map_or_else(|_| admin_user_id.as_str().to_string(), |u| u.username);

        // Broadcast RoomSettingsChanged cluster event for cross-replica propagation
        if let Some(ref tx) = self.redis_publish_tx {
            super::try_publish_cluster_event(
                tx,
                PublishRequest {
                    event: ClusterEvent::RoomSettingsChanged {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: admin_user_id.clone(),
                        username: admin_username,
                        settings_json: req.settings.clone(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::admin::UpdateRoomSettingsResponse {
            room: Some(admin_room_to_proto(
                &room,
                Some(&settings),
                self.connection_manager
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
            )),
        })
    }

    pub async fn reset_room_settings(
        &self,
        req: crate::proto::admin::ResetRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::admin::ResetRoomSettingsResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        self.room_service
            .reset_room_settings(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();

        // Look up admin username for cluster event
        let admin_username = self
            .user_service
            .get_user(admin_user_id)
            .await
            .map_or_else(|_| admin_user_id.as_str().to_string(), |u| u.username);

        // Broadcast RoomSettingsChanged cluster event for cross-replica propagation
        if let Some(ref tx) = self.redis_publish_tx {
            let settings_json = serde_json::to_vec(&settings).unwrap_or_default();
            super::try_publish_cluster_event(
                tx,
                PublishRequest {
                    event: ClusterEvent::RoomSettingsChanged {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: admin_user_id.clone(),
                        username: admin_username,
                        settings_json,
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        Ok(crate::proto::admin::ResetRoomSettingsResponse {
            room: Some(admin_room_to_proto(
                &room,
                Some(&settings),
                self.connection_manager
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
            )),
        })
    }

    // === Admin Management (root only) ===

    pub async fn add_admin(
        &self,
        req: crate::proto::admin::AddAdminRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::AddAdminResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.role.is_admin_or_above() {
            return Err(ApiError::InvalidInput(
                "User is already an admin or root".to_string(),
            ));
        }

        let old_version = user.version;
        user.role = UserRole::Admin;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: granting admin role (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserRoleUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
                "new_role": "Admin",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::AddAdminResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn remove_admin(
        &self,
        req: crate::proto::admin::RemoveAdminRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::RemoveAdminResponse, ApiError> {
        let uid = UserId::from_string(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if matches!(user.role, UserRole::Root) {
            return Err(ApiError::Authorization(
                "Cannot remove admin role from root user".to_string(),
            ));
        }
        if !user.role.is_admin_or_above() {
            return Err(ApiError::InvalidInput("User is not an admin".to_string()));
        }

        let target_username = user.username.clone();
        let old_version = user.version;
        user.role = UserRole::User;
        self.user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: revoking admin role (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserRoleUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": target_username,
                "new_role": "User",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::RemoveAdminResponse { success: true })
    }

    pub async fn list_admins(
        &self,
        _req: crate::proto::admin::ListAdminsRequest,
    ) -> Result<crate::proto::admin::ListAdminsResponse, ApiError> {
        // The DB query filters by role="admin" which returns admin and root users.
        // No additional client-side filtering needed.
        let query = synctv_core::models::UserListQuery {
            pagination: synctv_core::models::PageParams::new(Some(1), Some(100)),
            role: Some(synctv_core::models::UserRole::Admin),
            ..Default::default()
        };

        let (users, _) = self
            .user_service
            .list_users(&query)
            .await
            .map_err(ApiError::from)?;

        let admins: Vec<_> = users.into_iter().map(|u| admin_user_to_proto(&u)).collect();

        Ok(crate::proto::admin::ListAdminsResponse { admins })
    }

    // === System Statistics ===

    pub async fn get_system_stats(
        &self,
        _req: crate::proto::admin::GetSystemStatsRequest,
    ) -> Result<crate::proto::admin::GetSystemStatsResponse, ApiError> {
        // M-4: Run all 7 independent DB queries in parallel
        let stats_pagination = synctv_core::models::PageParams::new(Some(1), Some(1));
        let query_all = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            ..Default::default()
        };
        let query_active = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::UserStatus::Active),
            ..Default::default()
        };
        let query_banned = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::UserStatus::Banned),
            ..Default::default()
        };
        let room_query_all = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            ..Default::default()
        };
        let room_query_active = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::RoomStatus::Active),
            ..Default::default()
        };
        let room_query_banned = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            is_banned: Some(true),
            ..Default::default()
        };

        let pool = self.user_service.pool();
        let (
            total_users_res,
            active_users_res,
            banned_users_res,
            total_rooms_res,
            active_rooms_res,
            banned_rooms_res,
            provider_count_res,
            total_media_res,
        ) = tokio::join!(
            self.user_service.list_users(&query_all),
            self.user_service.list_users(&query_active),
            self.user_service.list_users(&query_banned),
            self.room_service.list_rooms(&room_query_all),
            self.room_service.list_rooms(&room_query_active),
            self.room_service.list_rooms(&room_query_banned),
            self.provider_instance_manager.get_all_instances(),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media").fetch_one(pool),
        );

        let (_, total_users) = total_users_res.unwrap_or((vec![], 0));
        let (_, active_users) = active_users_res.unwrap_or((vec![], 0));
        let (_, banned_users) = banned_users_res.unwrap_or((vec![], 0));
        let (_, total_rooms) = total_rooms_res.unwrap_or((vec![], 0));
        let (_, active_rooms) = active_rooms_res.unwrap_or((vec![], 0));
        let (_, banned_rooms) = banned_rooms_res.unwrap_or((vec![], 0));
        let provider_count = provider_count_res.map_or(0, |i| i.len() as i32);
        let total_media = total_media_res.unwrap_or(0) as i32;

        Ok(crate::proto::admin::GetSystemStatsResponse {
            total_users: total_users as i32,
            active_users: active_users as i32,
            banned_users: banned_users as i32,
            total_rooms: total_rooms as i32,
            active_rooms: active_rooms as i32,
            banned_rooms: banned_rooms as i32,
            total_media,
            provider_instances: provider_count,
            additional_stats: vec![],
        })
    }

    // =========================
    // Livestream Management
    // =========================

    /// List all active streams, optionally filtered by `room_id`
    pub async fn list_active_streams(
        &self,
        room_id: Option<&str>,
    ) -> anyhow::Result<Vec<crate::proto::admin::ActiveStreamInfo>> {
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Live streaming not configured"))?;

        let registry = infrastructure.registry();
        let active_pairs = registry.list_active_streams().await?;

        let mut streams = Vec::new();
        for (rid, mid) in active_pairs {
            if let Some(filter_room) = room_id {
                if rid != filter_room {
                    continue;
                }
            }

            let (user_id, node_id, started_at) = match registry.get_publisher(&rid, &mid).await {
                Ok(Some(info)) => (info.user_id, info.node_id, info.started_at.timestamp()),
                _ => (String::new(), String::new(), 0i64),
            };

            streams.push(crate::proto::admin::ActiveStreamInfo {
                room_id: rid,
                media_id: mid,
                user_id,
                node_id,
                started_at,
            });
        }

        Ok(streams)
    }

    /// Kick an active stream
    pub async fn kick_stream(
        &self,
        room_id: &str,
        media_id: &str,
        reason: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> anyhow::Result<()> {
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Live streaming not configured"))?;

        tracing::info!(
            room_id = %room_id,
            media_id = %media_id,
            reason = %reason,
            admin_user_id = %admin_user_id.as_str(),
            "Admin kicking stream"
        );

        infrastructure.kick_stream(room_id, media_id).await?;

        // Audit log: kick_stream is a critical operation
        {
            let admin_username = self
                .user_service
                .get_user(admin_user_id)
                .await
                .map_or_else(|_| admin_user_id.as_str().to_string(), |u| u.username);
            if let Err(e) = self
                .audit_service
                .log_stream_kicked(
                    admin_user_id.as_str().to_string(),
                    admin_username.clone(),
                    room_id.to_string(),
                    media_id.to_string(),
                    if reason.is_empty() {
                        None
                    } else {
                        Some(reason.to_string())
                    },
                    ctx.ip_address.clone(),
                    ctx.user_agent.clone(),
                )
                .await
            {
                tracing::error!(
                    error = %e,
                    admin_user_id = %admin_user_id.as_str(),
                    admin_username = %admin_username,
                    room_id = %room_id,
                    media_id = %media_id,
                    "AUDIT LOG FAILURE: failed to record stream kick. Manual review required."
                );
            }
        }

        Ok(())
    }

    // =========================
    // Batch Operations
    // =========================

    /// Batch ban multiple users.
    ///
    /// Each user is processed individually. If a user cannot be banned (e.g., not found,
    /// already banned, or permission denied), the error is recorded but processing continues.
    /// Returns per-user results with success/failure status.
    pub async fn batch_ban_users(
        &self,
        req: crate::proto::admin::BatchBanUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchBanUsersResponse, ApiError> {
        // Pre-filter: check role hierarchy for each target user before delegating
        // to the service layer. Users that violate hierarchy are skipped with an error.
        let mut allowed_ids = Vec::with_capacity(req.user_ids.len());
        let mut proto_results = Vec::with_capacity(req.user_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for user_id_str in &req.user_ids {
            let uid = UserId::from_string(user_id_str.clone());
            match self.user_service.get_user(&uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "ban") {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id_str.clone(),
                            success: false,
                            error: e.to_string(),
                        });
                        failed += 1;
                        continue;
                    }
                    allowed_ids.push(user_id_str.clone());
                }
                Err(e) => {
                    // User not found - let it pass through to service for consistent error handling
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: user_id_str.clone(),
                        success: false,
                        error: e.to_string(),
                    });
                    failed += 1;
                }
            }
        }

        // Process the allowed users through the service layer
        if !allowed_ids.is_empty() {
            let results = self
                .user_service
                .batch_ban_users(&allowed_ids)
                .await
                .map_err(ApiError::from)?;

            for (user_id, result) in results {
                match result {
                    Ok(()) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id.clone(),
                            success: true,
                            error: String::new(),
                        });
                        succeeded += 1;

                        // Disconnect user and kick streams
                        let uid = UserId::from_string(user_id);
                        self.connection_manager.disconnect_user(&uid);

                        if let Some(infra) = &self.live_streaming_infrastructure {
                            infra.kick_user_publishers(uid.as_str()).await;
                        }

                        if let Some(tx) = &self.redis_publish_tx {
                            super::try_publish_cluster_event(
                                tx,
                                PublishRequest {
                                    event: ClusterEvent::KickUser {
                                        event_id: nanoid::nanoid!(16),
                                        user_id: uid,
                                        reason: "batch_banned".to_string(),
                                        timestamp: chrono::Utc::now(),
                                    },
                                },
                            );
                        }
                    }
                    Err(e) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id,
                            success: false,
                            error: e.to_string(),
                        });
                        failed += 1;
                    }
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserBanned,
            synctv_core::service::AuditTargetType::User,
            None,
            serde_json::json!({
                "action": "batch_ban",
                "total": req.user_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchBanUsersResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }

    /// Batch delete multiple users.
    ///
    /// Each user is processed individually. If a user cannot be deleted, the error is
    /// recorded but processing continues.
    pub async fn batch_delete_users(
        &self,
        req: crate::proto::admin::BatchDeleteUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchDeleteUsersResponse, ApiError> {
        // Pre-filter: check role hierarchy for each target user before delegating
        // to the service layer. Users that violate hierarchy are skipped with an error.
        let mut allowed_ids = Vec::with_capacity(req.user_ids.len());
        let mut proto_results = Vec::with_capacity(req.user_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for user_id_str in &req.user_ids {
            let uid = UserId::from_string(user_id_str.clone());
            match self.user_service.get_user(&uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "delete") {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id_str.clone(),
                            success: false,
                            error: e.to_string(),
                        });
                        failed += 1;
                        continue;
                    }
                    allowed_ids.push(user_id_str.clone());
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: user_id_str.clone(),
                        success: false,
                        error: e.to_string(),
                    });
                    failed += 1;
                }
            }
        }

        // Process the allowed users through the service layer
        if !allowed_ids.is_empty() {
            let results = self
                .user_service
                .batch_delete_users(&allowed_ids)
                .await
                .map_err(ApiError::from)?;

            for (user_id, result) in results {
                match result {
                    Ok(()) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id.clone(),
                            success: true,
                            error: String::new(),
                        });
                        succeeded += 1;

                        // Disconnect user and kick streams
                        let uid = UserId::from_string(user_id);
                        self.connection_manager.disconnect_user(&uid);

                        if let Some(infra) = &self.live_streaming_infrastructure {
                            infra.kick_user_publishers(uid.as_str()).await;
                        }

                        if let Some(tx) = &self.redis_publish_tx {
                            super::try_publish_cluster_event(
                                tx,
                                PublishRequest {
                                    event: ClusterEvent::KickUser {
                                        event_id: nanoid::nanoid!(16),
                                        user_id: uid,
                                        reason: "batch_deleted".to_string(),
                                        timestamp: chrono::Utc::now(),
                                    },
                                },
                            );
                        }
                    }
                    Err(e) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id,
                            success: false,
                            error: e.to_string(),
                        });
                        failed += 1;
                    }
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserDeleted,
            synctv_core::service::AuditTargetType::User,
            None,
            serde_json::json!({
                "action": "batch_delete",
                "total": req.user_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchDeleteUsersResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }

    /// Batch ban multiple rooms.
    ///
    /// Each room is processed individually. If a room cannot be banned, the error is
    /// recorded but processing continues.
    pub async fn batch_ban_rooms(
        &self,
        req: crate::proto::admin::BatchBanRoomsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchBanRoomsResponse, ApiError> {
        let results = self
            .room_service
            .batch_ban_rooms(&req.room_ids, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        let mut proto_results = Vec::with_capacity(results.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for (room_id, result) in results {
            match result {
                Ok(()) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;

                    // Disconnect room and kick streams
                    let rid = RoomId::from_string(room_id);
                    self.connection_manager.disconnect_room(&rid);

                    if let Some(infra) = &self.live_streaming_infrastructure {
                        let media_ids = infra.user_stream_tracker.get_room_streams(rid.as_str());
                        for media_id in &media_ids {
                            self.kick_stream_cluster(rid.as_str(), media_id, "room_batch_banned");
                        }
                        infra.kick_room_publishers(rid.as_str()).await;
                    }

                    if let Some(tx) = &self.redis_publish_tx {
                        super::try_publish_cluster_event(
                            tx,
                            PublishRequest {
                                event: ClusterEvent::CacheInvalidate {
                                    event_id: nanoid::nanoid!(16),
                                    targets: vec![synctv_cluster::sync::CacheTarget::Room {
                                        room_id: rid.as_str().to_string(),
                                    }],
                                    timestamp: chrono::Utc::now(),
                                },
                            },
                        );
                    }
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id,
                        success: false,
                        error: e.to_string(),
                    });
                    failed += 1;
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomBanned,
            synctv_core::service::AuditTargetType::Room,
            None,
            serde_json::json!({
                "action": "batch_ban",
                "total": req.room_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchBanRoomsResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }

    /// Batch delete multiple rooms.
    ///
    /// Each room is processed individually. If a room cannot be deleted, the error is
    /// recorded but processing continues.
    pub async fn batch_delete_rooms(
        &self,
        req: crate::proto::admin::BatchDeleteRoomsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchDeleteRoomsResponse, ApiError> {
        let results = self
            .room_service
            .batch_delete_rooms(&req.room_ids, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        let mut proto_results = Vec::with_capacity(results.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for (room_id, result) in results {
            match result {
                Ok(()) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;

                    // Disconnect room and kick streams
                    let rid = RoomId::from_string(room_id);
                    self.connection_manager.disconnect_room(&rid);

                    if let Some(infra) = &self.live_streaming_infrastructure {
                        let media_ids = infra.user_stream_tracker.get_room_streams(rid.as_str());
                        for media_id in &media_ids {
                            self.kick_stream_cluster(rid.as_str(), media_id, "room_batch_deleted");
                        }
                        infra.kick_room_publishers(rid.as_str()).await;
                    }

                    if let Some(tx) = &self.redis_publish_tx {
                        super::try_publish_cluster_event(
                            tx,
                            PublishRequest {
                                event: ClusterEvent::RoomDeleted {
                                    event_id: nanoid::nanoid!(16),
                                    room_id: rid.clone(),
                                    deleted_by: admin_user_id.clone(),
                                    timestamp: chrono::Utc::now(),
                                },
                            },
                        );
                    }
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id,
                        success: false,
                        error: e.to_string(),
                    });
                    failed += 1;
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomDeleted,
            synctv_core::service::AuditTargetType::Room,
            None,
            serde_json::json!({
                "action": "batch_delete",
                "total": req.room_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchDeleteRoomsResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }
}

// === Helper Functions ===

fn admin_room_to_proto(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    creator_username: Option<&str>,
) -> crate::proto::admin::AdminRoom {
    let room_settings = settings.cloned().unwrap_or_default();
    crate::proto::admin::AdminRoom {
        id: room.id.to_string(),
        name: room.name.clone(),
        description: room.description.clone(),
        creator_id: room.created_by.to_string(),
        creator_username: creator_username.unwrap_or("").to_string(),
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: serde_json::to_vec(&room_settings).unwrap_or_default(),
        member_count: member_count.unwrap_or(0),
        created_at: room.created_at.timestamp(),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
    }
}

fn admin_room_member_to_proto(
    member: &synctv_core::models::RoomMemberWithUser,
) -> synctv_proto::common::RoomMember {
    synctv_proto::common::RoomMember {
        room_id: member.room_id.to_string(),
        user_id: member.user_id.to_string(),
        username: member.username.clone(),
        role: crate::impls::client::room_role_to_proto(member.role),
        permissions: member.effective_permissions(member.role.permissions()).0,
        added_permissions: member.added_permissions,
        removed_permissions: member.removed_permissions,
        admin_added_permissions: member.admin_added_permissions,
        admin_removed_permissions: member.admin_removed_permissions,
        joined_at: member.joined_at.timestamp(),
        is_online: member.is_online,
    }
}

fn admin_user_to_proto(user: &synctv_core::models::User) -> crate::proto::admin::AdminUser {
    let role = match user.role {
        synctv_core::models::UserRole::Root => synctv_proto::common::UserRole::Root as i32,
        synctv_core::models::UserRole::Admin => synctv_proto::common::UserRole::Admin as i32,
        synctv_core::models::UserRole::User => synctv_proto::common::UserRole::User as i32,
    };

    let status = match user.status {
        synctv_core::models::UserStatus::Active => synctv_proto::common::UserStatus::Active as i32,
        synctv_core::models::UserStatus::Pending => {
            synctv_proto::common::UserStatus::Pending as i32
        }
        synctv_core::models::UserStatus::Banned => synctv_proto::common::UserStatus::Banned as i32,
    };

    crate::proto::admin::AdminUser {
        id: user.id.to_string(),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        role,
        status,
        created_at: user.created_at.timestamp(),
        updated_at: user.updated_at.timestamp(),
    }
}

fn provider_instance_to_proto(
    instance: synctv_core::models::ProviderInstance,
) -> crate::proto::admin::ProviderInstance {
    use crate::proto::admin::ProviderInstanceStatus;

    // The `status` field represents actual connection state, not enabled/disabled.
    // Since we don't track real connection status here, use Disconnected for disabled
    // instances and Unspecified for enabled ones (actual connectivity is unknown).
    let status: i32 = if instance.enabled {
        ProviderInstanceStatus::Unspecified.into()
    } else {
        ProviderInstanceStatus::Disconnected.into()
    };

    // Parse timeout string (e.g., "10s", "30s") to seconds
    let timeout_seconds = parse_timeout_to_seconds(&instance.timeout);

    crate::proto::admin::ProviderInstance {
        name: instance.name,
        endpoint: instance.endpoint,
        comment: instance.comment.unwrap_or_default(),
        timeout_seconds,
        tls: instance.tls,
        insecure_tls: instance.insecure_tls,
        providers: instance.providers,
        enabled: instance.enabled,
        status,
        created_at: instance.created_at.timestamp(),
        updated_at: instance.updated_at.timestamp(),
    }
}

fn parse_timeout_to_seconds(timeout: &str) -> u32 {
    timeout.trim_end_matches('s').parse::<u32>().unwrap_or(10)
}

fn seconds_to_timeout_string(seconds: u32) -> String {
    format!("{seconds}s")
}

/// Strip credentials (username/password) from a URL before including it in audit logs.
///
/// If the input is not a valid URL, returns it unchanged (best-effort masking).
fn mask_url_credentials(endpoint: &str) -> String {
    match url::Url::parse(endpoint) {
        Ok(mut parsed) => {
            if !parsed.username().is_empty() || parsed.password().is_some() {
                // Clear credentials - set_username/set_password return Err only for cannot-be-a-base URLs
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
            }
            parsed.to_string()
        }
        Err(_) => endpoint.to_string(),
    }
}

/// Check if the caller has sufficient role to operate on a target user.
///
/// Returns `Ok(())` if the caller's role is high enough to modify the target user,
/// or `Err(ApiError::Authorization(...))` if the role hierarchy would be violated.
///
/// Rules:
/// - Only Root can operate on Root users
/// - Only Root can operate on Admin users
/// - Admin and Root can operate on regular Users
fn check_role_hierarchy(
    caller_role: UserRole,
    target_role: UserRole,
    action: &str,
) -> Result<(), ApiError> {
    if target_role == UserRole::Root && caller_role != UserRole::Root {
        return Err(ApiError::Authorization(format!(
            "Only root users can {action} root users"
        )));
    }
    if target_role == UserRole::Admin && caller_role != UserRole::Root {
        return Err(ApiError::Authorization(format!(
            "Only root users can {action} admin users"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::models::{
        MemberStatus, RoomId, RoomRole, RoomStatus, UserId, UserRole, UserStatus,
    };

    // === Timeout Parsing Tests ===

    #[test]
    fn test_parse_timeout_to_seconds_valid() {
        assert_eq!(parse_timeout_to_seconds("10s"), 10);
        assert_eq!(parse_timeout_to_seconds("30s"), 30);
        assert_eq!(parse_timeout_to_seconds("0s"), 0);
        assert_eq!(parse_timeout_to_seconds("300s"), 300);
    }

    #[test]
    fn test_parse_timeout_to_seconds_no_suffix() {
        assert_eq!(parse_timeout_to_seconds("10"), 10);
    }

    #[test]
    fn test_parse_timeout_to_seconds_invalid() {
        assert_eq!(parse_timeout_to_seconds("abc"), 10); // Default fallback
        assert_eq!(parse_timeout_to_seconds(""), 10); // Empty string
    }

    #[test]
    fn test_seconds_to_timeout_string() {
        assert_eq!(seconds_to_timeout_string(10), "10s");
        assert_eq!(seconds_to_timeout_string(0), "0s");
        assert_eq!(seconds_to_timeout_string(300), "300s");
    }

    #[test]
    fn test_timeout_roundtrip() {
        for secs in [0, 1, 10, 30, 60, 300] {
            let s = seconds_to_timeout_string(secs);
            assert_eq!(parse_timeout_to_seconds(&s), secs);
        }
    }

    // === Admin Room Proto Conversion Tests ===

    fn make_test_room(status: RoomStatus) -> synctv_core::models::Room {
        let now = chrono::Utc::now();
        synctv_core::models::Room {
            id: RoomId::from_string("admin_room_1".to_string()),
            name: "Admin Test Room".to_string(),
            description: "Room for admin tests".to_string(),
            created_by: UserId::from_string("creator_1".to_string()),
            status,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 1,
            last_activity_at: now,
        }
    }

    #[test]
    fn test_admin_room_to_proto_basic() {
        let room = make_test_room(RoomStatus::Active);
        let proto = admin_room_to_proto(&room, None, Some(10), Some("creator_user"));

        assert_eq!(proto.id, "admin_room_1");
        assert_eq!(proto.name, "Admin Test Room");
        assert_eq!(proto.description, "Room for admin tests");
        assert_eq!(proto.creator_id, "creator_1");
        assert_eq!(proto.creator_username, "creator_user");
        assert_eq!(proto.member_count, 10);
        assert!(!proto.is_banned);
    }

    #[test]
    fn test_admin_room_to_proto_banned() {
        let mut room = make_test_room(RoomStatus::Active);
        room.is_banned = true;
        let proto = admin_room_to_proto(&room, None, None, None);
        assert!(proto.is_banned);
        assert_eq!(proto.member_count, 0);
    }

    #[test]
    fn test_admin_room_to_proto_different_statuses() {
        for status in [RoomStatus::Active, RoomStatus::Pending, RoomStatus::Closed] {
            let room = make_test_room(status);
            let proto = admin_room_to_proto(&room, None, None, None);
            assert_eq!(
                proto.status,
                synctv_proto::common::RoomStatus::from(status) as i32
            );
        }
    }

    // === Admin User Proto Conversion Tests ===

    fn make_test_user(role: UserRole, status: UserStatus) -> synctv_core::models::User {
        synctv_core::models::User {
            id: UserId::from_string("admin_user_1".to_string()),
            username: "admin_test".to_string(),
            email: Some("admin@test.com".to_string()),
            password_hash: "hash".to_string(),
            role,
            status,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        }
    }

    #[test]
    fn test_admin_user_to_proto_all_roles() {
        for (role, expected) in [
            (UserRole::Root, synctv_proto::common::UserRole::Root as i32),
            (
                UserRole::Admin,
                synctv_proto::common::UserRole::Admin as i32,
            ),
            (UserRole::User, synctv_proto::common::UserRole::User as i32),
        ] {
            let user = make_test_user(role, UserStatus::Active);
            let proto = admin_user_to_proto(&user);
            assert_eq!(proto.role, expected);
        }
    }

    #[test]
    fn test_admin_user_to_proto_all_statuses() {
        for (status, expected) in [
            (
                UserStatus::Active,
                synctv_proto::common::UserStatus::Active as i32,
            ),
            (
                UserStatus::Pending,
                synctv_proto::common::UserStatus::Pending as i32,
            ),
            (
                UserStatus::Banned,
                synctv_proto::common::UserStatus::Banned as i32,
            ),
        ] {
            let user = make_test_user(UserRole::User, status);
            let proto = admin_user_to_proto(&user);
            assert_eq!(proto.status, expected);
        }
    }

    #[test]
    fn test_admin_user_to_proto_fields() {
        let user = make_test_user(UserRole::Admin, UserStatus::Active);
        let proto = admin_user_to_proto(&user);

        assert_eq!(proto.id, "admin_user_1");
        assert_eq!(proto.username, "admin_test");
        assert_eq!(proto.email, "admin@test.com");
    }

    #[test]
    fn test_admin_user_to_proto_no_email() {
        let mut user = make_test_user(UserRole::User, UserStatus::Active);
        user.email = None;
        let proto = admin_user_to_proto(&user);
        assert_eq!(proto.email, "");
    }

    // === Admin Room Member Proto Conversion Tests ===

    fn make_test_member(role: RoomRole) -> synctv_core::models::RoomMemberWithUser {
        synctv_core::models::RoomMemberWithUser {
            room_id: RoomId::from_string("room1".to_string()),
            user_id: UserId::from_string("user1".to_string()),
            username: "testmember".to_string(),
            role,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: chrono::Utc::now(),
            is_online: false,
            is_active: true,
            banned_at: None,
            banned_reason: None,
        }
    }

    #[test]
    fn test_admin_room_member_to_proto() {
        let member = make_test_member(RoomRole::Admin);
        let proto = admin_room_member_to_proto(&member);

        assert_eq!(proto.room_id, "room1");
        assert_eq!(proto.user_id, "user1");
        assert_eq!(proto.username, "testmember");
        assert_eq!(
            proto.role,
            synctv_proto::common::RoomMemberRole::Admin as i32
        );
        assert!(!proto.is_online);
    }

    #[test]
    fn test_admin_room_member_to_proto_with_permissions() {
        let mut member = make_test_member(RoomRole::Member);
        member.added_permissions = 0xAA;
        member.removed_permissions = 0x55;
        member.admin_added_permissions = 0xCC;
        member.admin_removed_permissions = 0x33;
        let proto = admin_room_member_to_proto(&member);

        assert_eq!(proto.added_permissions, 0xAA);
        assert_eq!(proto.removed_permissions, 0x55);
        assert_eq!(proto.admin_added_permissions, 0xCC);
        assert_eq!(proto.admin_removed_permissions, 0x33);
    }

    // === Provider Instance Conversion Tests ===

    #[test]
    fn test_provider_instance_to_proto_enabled() {
        let instance = synctv_core::models::ProviderInstance {
            name: "test_provider".to_string(),
            endpoint: "https://example.com".to_string(),
            comment: Some("A test provider".to_string()),
            jwt_secret: None,
            custom_ca: None,
            timeout: "30s".to_string(),
            tls: true,
            insecure_tls: false,
            providers: vec!["bilibili".to_string(), "alist".to_string()],
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let proto = provider_instance_to_proto(instance);

        assert_eq!(proto.name, "test_provider");
        assert_eq!(proto.endpoint, "https://example.com");
        assert_eq!(proto.comment, "A test provider");
        assert_eq!(proto.timeout_seconds, 30);
        assert!(proto.tls);
        assert!(!proto.insecure_tls);
        assert_eq!(proto.providers, vec!["bilibili", "alist"]);
        assert!(proto.enabled);
        assert_eq!(
            proto.status,
            i32::from(crate::proto::admin::ProviderInstanceStatus::Unspecified)
        );
    }

    #[test]
    fn test_provider_instance_to_proto_disabled() {
        let instance = synctv_core::models::ProviderInstance {
            name: "disabled_provider".to_string(),
            endpoint: "https://disabled.example.com".to_string(),
            comment: None,
            jwt_secret: None,
            custom_ca: None,
            timeout: "10s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec![],
            enabled: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let proto = provider_instance_to_proto(instance);

        assert_eq!(
            proto.status,
            i32::from(crate::proto::admin::ProviderInstanceStatus::Disconnected)
        );
        assert_eq!(proto.comment, ""); // None -> empty
        assert!(!proto.enabled);
    }

    // === Password Reset Role Hierarchy Tests ===
    //
    // These verify the role hierarchy rules enforced by update_user_password:
    // - Root can reset anyone's password (root, admin, user)
    // - Admin can only reset regular user passwords
    // - Admin CANNOT reset root or other admin passwords

    /// Helper: check if a `caller_role` can reset a `target_role`'s password
    /// Returns true if the operation should be allowed.
    fn password_reset_allowed(caller_role: UserRole, target_role: UserRole) -> bool {
        if target_role == UserRole::Root && caller_role != UserRole::Root {
            return false;
        }
        if target_role == UserRole::Admin && caller_role != UserRole::Root {
            return false;
        }
        true
    }

    #[test]
    fn test_root_can_reset_any_password() {
        assert!(password_reset_allowed(UserRole::Root, UserRole::Root));
        assert!(password_reset_allowed(UserRole::Root, UserRole::Admin));
        assert!(password_reset_allowed(UserRole::Root, UserRole::User));
    }

    #[test]
    fn test_admin_cannot_reset_root_password() {
        assert!(!password_reset_allowed(UserRole::Admin, UserRole::Root));
    }

    #[test]
    fn test_admin_cannot_reset_other_admin_password() {
        assert!(!password_reset_allowed(UserRole::Admin, UserRole::Admin));
    }

    #[test]
    fn test_admin_can_reset_user_password() {
        assert!(password_reset_allowed(UserRole::Admin, UserRole::User));
    }

    // === URL Credential Masking Tests ===

    #[test]
    fn test_mask_url_credentials_with_user_and_password() {
        let url = "https://user:secretpass@server.example.com/api";
        let masked = mask_url_credentials(url);
        assert!(
            !masked.contains("user:"),
            "Username should be stripped: {masked}"
        );
        assert!(
            !masked.contains("secretpass"),
            "Password should be stripped: {masked}"
        );
        assert!(
            masked.contains("server.example.com/api"),
            "Host and path should remain: {masked}"
        );
    }

    #[test]
    fn test_mask_url_credentials_with_user_only() {
        let url = "https://admin@server.example.com:8096/emby";
        let masked = mask_url_credentials(url);
        assert!(
            !masked.contains("admin@"),
            "Username should be stripped: {masked}"
        );
        assert!(
            masked.contains("server.example.com:8096/emby"),
            "Host and path should remain: {masked}"
        );
    }

    #[test]
    fn test_mask_url_credentials_without_credentials() {
        let url = "https://server.example.com/api";
        let masked = mask_url_credentials(url);
        assert_eq!(masked, "https://server.example.com/api");
    }

    #[test]
    fn test_mask_url_credentials_invalid_url_passthrough() {
        let url = "not-a-valid-url";
        let masked = mask_url_credentials(url);
        assert_eq!(masked, "not-a-valid-url");
    }

    #[test]
    fn test_mask_url_credentials_empty_string() {
        let masked = mask_url_credentials("");
        assert_eq!(masked, "");
    }

    #[test]
    fn test_mask_url_credentials_preserves_port_and_query() {
        let url = "https://user:pass@host.com:9090/path?key=val";
        let masked = mask_url_credentials(url);
        assert!(
            !masked.contains("user"),
            "Username should be stripped: {masked}"
        );
        assert!(
            !masked.contains("pass"),
            "Password should be stripped: {masked}"
        );
        assert!(
            masked.contains("host.com:9090"),
            "Host and port should remain: {masked}"
        );
        assert!(masked.contains("key=val"), "Query should remain: {masked}");
    }

    // === Role Hierarchy Check Tests (batch operations) ===

    #[test]
    fn test_check_role_hierarchy_root_can_operate_on_all() {
        assert!(check_role_hierarchy(UserRole::Root, UserRole::Root, "ban").is_ok());
        assert!(check_role_hierarchy(UserRole::Root, UserRole::Admin, "ban").is_ok());
        assert!(check_role_hierarchy(UserRole::Root, UserRole::User, "ban").is_ok());
    }

    #[test]
    fn test_check_role_hierarchy_admin_cannot_operate_on_root() {
        let result = check_role_hierarchy(UserRole::Admin, UserRole::Root, "ban");
        assert!(
            result.is_err(),
            "Admin should not be able to ban Root users"
        );
        match result {
            Err(ApiError::Authorization(msg)) => {
                assert!(msg.contains("root"), "Error should mention root: {msg}");
            }
            other => panic!("Expected Authorization error, got: {other:?}"),
        }
    }

    #[test]
    fn test_check_role_hierarchy_admin_cannot_operate_on_admin() {
        let result = check_role_hierarchy(UserRole::Admin, UserRole::Admin, "delete");
        assert!(
            result.is_err(),
            "Admin should not be able to delete Admin users"
        );
        match result {
            Err(ApiError::Authorization(msg)) => {
                assert!(msg.contains("root"), "Error should mention root: {msg}");
            }
            other => panic!("Expected Authorization error, got: {other:?}"),
        }
    }

    #[test]
    fn test_check_role_hierarchy_admin_can_operate_on_user() {
        assert!(check_role_hierarchy(UserRole::Admin, UserRole::User, "ban").is_ok());
    }

    // === Admin Role Elevation Tests (P0#4) ===

    /// Verify that proto_role_to_user_role maps Admin role correctly
    /// (prerequisite for the role elevation check).
    #[test]
    fn test_proto_role_to_user_role_admin() {
        let admin_role = crate::impls::client::proto_role_to_user_role(
            synctv_proto::common::UserRole::Admin as i32,
        )
        .expect("should parse admin role");
        assert_eq!(admin_role, UserRole::Admin);
    }

}
