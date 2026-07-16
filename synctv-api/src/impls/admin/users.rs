use synctv_core::models::{AuditDetails, AuditUpdatedFields, UserId, UserRole, UserStatus};

use super::{
    auth_factors_to_proto, check_role_hierarchy, i64_to_i32_api, user_preferences_to_proto,
    AdminApiImpl, ApiError, RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};

fn optional_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn create_user_role(value: i32) -> Result<Option<UserRole>, ApiError> {
    if value == synctv_proto::common::UserRole::Unspecified as i32
        || value == synctv_proto::common::UserRole::User as i32
    {
        return Ok(None);
    }
    crate::impls::client::proto_role_to_user_role(value).map(Some)
}

fn user_status_filter(value: i32) -> Result<Option<UserStatus>, ApiError> {
    if value == synctv_proto::common::UserStatus::Unspecified as i32 {
        return Ok(None);
    }
    UserStatus::try_from(value)
        .map(Some)
        .map_err(|_| ApiError::InvalidInput("Unsupported user status".to_string()))
}

fn list_users_query(
    req: &synctv_proto::admin::ListUsersRequest,
) -> Result<synctv_core::models::UserListQuery, ApiError> {
    Ok(synctv_core::models::UserListQuery {
        pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
        search: optional_trimmed(&req.search),
        status: super::proto_user_status_filter(req.status)?,
        role: super::proto_user_role_filter(req.role)?,
        is_banned: req.is_banned,
        sort_by: super::proto_admin_user_list_sort_by(req.sort_by)?,
        sort_direction: super::proto_admin_sort_direction(
            req.sort_direction,
            synctv_core::models::SortDirection::Desc,
        )?,
    })
}

fn list_admins_query(
    req: &synctv_proto::admin::ListAdminsRequest,
) -> Result<synctv_core::models::UserListQuery, ApiError> {
    Ok(synctv_core::models::UserListQuery {
        pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
        search: optional_trimmed(&req.search),
        sort_by: super::proto_admin_user_list_sort_by(req.sort_by)?,
        sort_direction: super::proto_admin_sort_direction(
            req.sort_direction,
            synctv_core::models::SortDirection::Desc,
        )?,
        ..Default::default()
    })
}

impl AdminApiImpl {
    pub async fn create_user(
        &self,
        req: synctv_proto::admin::CreateUserRequest,
        caller_role: synctv_core::models::UserRole,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let role = create_user_role(req.role)?;
        let status = user_status_filter(req.status)?;
        let password = (!req.password.is_empty()).then_some(req.password);
        if role == Some(synctv_core::models::UserRole::Root)
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can create root users".to_string(),
            ));
        }

        // Delegate to UserService which handles validation, creation, and
        // username cache population atomically.
        let initial_banned_by = (status == Some(UserStatus::Banned)
            && *admin_user_id != LOCAL_MANAGEMENT_ACTOR_USER_ID)
            .then_some(admin_user_id);
        let user = self
            .user_service
            .create_user_with_optional_direct_password(
                req.username,
                optional_trimmed(&req.email),
                password,
                role,
                status,
                initial_banned_by,
            )
            .await
            .map_err(ApiError::from)?;

        // Audit log: user creation via admin panel (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserCreated,
            synctv_core::models::AuditTargetType::User,
            Some(user.id.to_string()),
            AuditDetails::reason("User created via admin panel"),
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&user).await
    }

    pub async fn delete_user(
        &self,
        req: synctv_proto::admin::DeleteUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::DeleteUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let owned_room_ids = self
            .list_owned_room_ids(&uid)
            .await
            .map_err(ApiError::from)?;
        let (deleted_room_outbox_events, deleted_room_fanout) =
            self.prepare_deleted_room_outbox_fanout(&owned_room_ids, admin_user_id)?;
        let summary = self
            .user_service
            .delete_user_with_summary_and_outbox(&uid, deleted_room_outbox_events)
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .finalize_user_deletion(
                self.room_service.as_ref(),
                &summary,
                admin_user_id,
                "user_deleted",
                deleted_room_fanout,
            )
            .await;

        // Audit log: user deletion is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserDeleted,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::DeleteUserResponse { success: true })
    }

    pub async fn update_user_username(
        &self,
        req: synctv_proto::admin::UpdateUserUsernameRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;

        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;
        let old_username = user.username.clone();
        let updated = self
            .user_service
            .update_profile(&uid, Some(req.new_username))
            .await
            .map_err(ApiError::from)?;

        // Audit log: admin changing another user's username (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserUsernameUpdated,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                old_username: Some(old_username),
                new_username: Some(updated.username.clone()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated).await
    }

    pub async fn ban_user(
        &self,
        req: synctv_proto::admin::BanUserRequest,
        admin_user_id: &UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let reason = req.reason.trim();
        let reason = (!reason.is_empty()).then(|| reason.to_string());
        let updated = self
            .ban_user_with_cleanup(&uid, admin_user_id, caller_role, reason.clone())
            .await?;

        // Audit log: ban_user is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserBanned,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                target_username: Some(updated.username.clone()),
                reason,
                caller_role: Some(format!("{caller_role:?}")),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated).await
    }

    pub async fn unban_user(
        &self,
        req: synctv_proto::admin::UnbanUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if !user.is_banned {
            return Err(ApiError::InvalidInput("User is not banned".to_string()));
        }

        let updated = self
            .user_service
            .unban_user(&uid)
            .await
            .map_err(ApiError::from)?;

        // Audit log: unban is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserUnbanned,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                target_username: Some(updated.username.clone()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated).await
    }

    pub async fn list_users(
        &self,
        req: synctv_proto::admin::ListUsersRequest,
    ) -> Result<synctv_proto::admin::ListUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let query = list_users_query(&req)?;
        let (users, total) = self
            .user_service
            .list_users_eventually_consistent(&query)
            .await
            .map_err(ApiError::from)?;

        let user_list = self.admin_users_to_proto_with_email(&users).await?;

        Ok(synctv_proto::admin::ListUsersResponse {
            users: user_list,
            total: i64_to_i32_api(total, "user count")?,
        })
    }

    pub async fn get_user(
        &self,
        req: synctv_proto::admin::GetUserRequest,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user_id = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(ApiError::from)?;

        self.admin_user_to_proto_with_email(&user).await
    }

    pub async fn get_user_preferences(
        &self,
        req: synctv_proto::admin::GetUserPreferencesRequest,
    ) -> Result<synctv_proto::admin::GetUserPreferencesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user_id = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(ApiError::from)?;
        let (preferences, auth_factors) = self
            .user_service
            .get_user_preferences(&user_id)
            .await
            .map_err(ApiError::from)?;

        Ok(synctv_proto::admin::GetUserPreferencesResponse {
            user: Some(self.admin_user_to_proto_with_email(&user).await?),
            preferences: Some(user_preferences_to_proto(&preferences)?),
            auth_factors: Some(auth_factors_to_proto(&auth_factors)?),
        })
    }

    pub async fn update_user_preferences(
        &self,
        req: synctv_proto::admin::UpdateUserPreferencesRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::UpdateUserPreferencesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid =
            crate::impls::proto_validated_user_id(req.user_id.clone(), &self.public_id_codec)?;
        let update = crate::impls::client::user_preferences_update_from_proto(
            synctv_proto::client::UpdateUserPreferencesRequest {
                two_factor_enabled: req.two_factor_enabled,
                notifications: req.notifications,
            },
        );
        if update.is_empty() {
            return Err(ApiError::InvalidInput(
                "No valid user preference fields provided".to_string(),
            ));
        }

        let actor = self.require_admin_actor(admin_user_id).await?;
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(Self::map_target_user_lookup_error)?;
        check_role_hierarchy(actor.role, user.role, "update preferences")?;

        let (preferences, auth_factors) = self
            .user_service
            .update_user_preferences(&uid, update.clone())
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserPreferencesUpdated,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                target_username: Some(user.username.clone()),
                updated_fields: Some(AuditUpdatedFields {
                    two_factor_enabled: update.two_factor_enabled.is_some(),
                    notifications: update.notifications.is_some(),
                }),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::UpdateUserPreferencesResponse {
            user: Some(self.admin_user_to_proto_with_email(&user).await?),
            preferences: Some(user_preferences_to_proto(&preferences)?),
            auth_factors: Some(auth_factors_to_proto(&auth_factors)?),
        })
    }

    pub async fn update_user_role(
        &self,
        req: synctv_proto::admin::UpdateUserRoleRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        let new_role = crate::impls::client::proto_role_to_user_role(req.role)?;

        if new_role == UserRole::Root && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can promote to root".to_string(),
            ));
        }

        if user.role == UserRole::Root && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can change root user roles".to_string(),
            ));
        }

        if user.role == UserRole::Admin && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can change admin user roles".to_string(),
            ));
        }

        if new_role == UserRole::Admin && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can promote to admin".to_string(),
            ));
        }

        let old_version = user.version;
        let updated_user = self
            .user_service
            .update_role(&uid, new_role, old_version)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserRoleUpdated,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                target_username: Some(updated_user.username.clone()),
                new_role: Some(format!("{new_role:?}")),
                caller_role: Some(format!("{caller_role:?}")),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated_user).await
    }

    pub async fn set_user_password(
        &self,
        req: synctv_proto::admin::SetUserPasswordRequest,
        caller_user_id: UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::SetUserPasswordResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;

        let target_user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(Self::map_target_user_lookup_error)?;

        if target_user.role == UserRole::Root && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can reset root user passwords".to_string(),
            ));
        }

        if target_user.role == UserRole::Admin && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can reset admin user passwords".to_string(),
            ));
        }

        self.user_service
            .set_direct_password(&uid, &req.password)
            .await
            .map_err(ApiError::from)?;

        {
            self.log_admin_action(
                &caller_user_id,
                synctv_core::models::AuditAction::UserPasswordResetRequired,
                synctv_core::models::AuditTargetType::User,
                Some(uid.to_string()),
                AuditDetails {
                    target_user_id: Some(uid.to_string()),
                    target_username: Some(target_user.username.clone()),
                    reason: (!req.reason.is_empty()).then_some(req.reason),
                    credential_updated: Some(true),
                    ..Default::default()
                },
                ctx,
            )
            .await;
        }

        let updated_user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;
        Ok(synctv_proto::admin::SetUserPasswordResponse {
            success: true,
            user: Some(self.admin_user_to_proto_with_email(&updated_user).await?),
        })
    }

    pub async fn add_admin(
        &self,
        req: synctv_proto::admin::AddAdminRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::AdminUser, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.role.is_admin_or_above() {
            return Err(ApiError::InvalidInput(
                "User is already an admin or root".to_string(),
            ));
        }

        let updated = self
            .user_service
            .update_role(&uid, UserRole::Admin, user.version)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserRoleUpdated,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                target_username: Some(updated.username.clone()),
                new_role: Some("Admin".to_string()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        self.admin_user_to_proto_with_email(&updated).await
    }

    pub async fn remove_admin(
        &self,
        req: synctv_proto::admin::RemoveAdminRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<synctv_proto::admin::RemoveAdminResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id, &self.public_id_codec)?;
        let user = self
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
        self.user_service
            .update_role(&uid, UserRole::User, user.version)
            .await
            .map_err(ApiError::from)?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::models::AuditAction::UserRoleUpdated,
            synctv_core::models::AuditTargetType::User,
            Some(uid.to_string()),
            AuditDetails {
                target_user_id: Some(uid.to_string()),
                target_username: Some(target_username),
                new_role: Some("User".to_string()),
                ..Default::default()
            },
            ctx,
        )
        .await;

        Ok(synctv_proto::admin::RemoveAdminResponse { success: true })
    }

    pub async fn list_admins(
        &self,
        req: synctv_proto::admin::ListAdminsRequest,
    ) -> Result<synctv_proto::admin::ListAdminsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let query = list_admins_query(&req)?;
        let (users, total) = self
            .user_service
            .list_admins_eventually_consistent(&query)
            .await
            .map_err(ApiError::from)?;

        let admins = self.admin_users_to_proto_with_email(&users).await?;

        Ok(synctv_proto::admin::ListAdminsResponse {
            admins,
            total: i64_to_i32_api(total, "admin total")?,
        })
    }
}
