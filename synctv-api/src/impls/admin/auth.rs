use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use synctv_core::models::{UserId, UserRole, UserStatus};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{AuthorizedAdminActor, UserService};

use crate::impls::{EndpointRateLimitCategory, RequestExecutor, RequestMetadata};

use super::{AdminApiImpl, ApiError, LOCAL_MANAGEMENT_ACTOR_USER_ID};

pub(in crate::impls::admin) struct AdminActor {
    pub(in crate::impls::admin) username: String,
    pub(in crate::impls::admin) role: UserRole,
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
/// request metadata before calling this function.
pub async fn validate_admin_auth(
    user_service: &UserService,
    user_id: UserId,
    token_pv: i32,
    _token_iat: i64,
) -> Result<ValidatedAdmin, ApiError> {
    let user = user_service.get_user(&user_id).await.map_err(|e| {
        tracing::debug!(
            user_id = %user_id,
            error = %e,
            "Admin auth rejected: failed to look up user"
        );
        AdminApiImpl::map_admin_auth_user_lookup_error(e)
    })?;

    if user.is_deleted() || user.is_banned || user.status == UserStatus::Banned {
        tracing::debug!(
            user_id = %user_id,
            status = ?user.status,
            deleted = user.is_deleted(),
            "Admin auth rejected: user is deleted or not in an active status"
        );
        return Err(ApiError::Authentication(
            "Authentication failed".to_string(),
        ));
    }

    let password_version = user_service
        .get_password_credential_state(&user_id)
        .await
        .map_err(ApiError::from)?
        .version;

    if token_pv < password_version {
        tracing::debug!(
            user_id = %user_id,
            token_pv = token_pv,
            current_pv = password_version,
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

impl AdminApiImpl {
    fn request_executor(&self) -> Result<&Arc<RequestExecutor>, ApiError> {
        self.request_executor.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Request executor is not configured".to_string())
        })
    }

    pub fn execute_admin_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_admin_endpoint_with_control(metadata, move |_, validated| operation(validated))
    }

    pub fn execute_admin_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        let user_service = Arc::clone(&self.user_service);
        match self.request_executor() {
            Ok(executor) => executor.execute_user_with_control(
                metadata,
                EndpointRateLimitCategory::Admin,
                move |request_control, authenticated| async move {
                    let validated = validate_admin_auth(
                        user_service.as_ref(),
                        authenticated.user_id,
                        authenticated.claims.pv,
                        authenticated.claims.iat,
                    )
                    .await?;
                    if !validated.role.is_admin_or_above() {
                        return Err(ApiError::Authorization("Admin role required".to_string()));
                    }
                    operation(request_control, validated).await
                },
            ),
            Err(err) => async move { Err(err) }.boxed(),
        }
    }

    pub fn execute_root_endpoint<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_root_endpoint_with_control(metadata, move |_, validated| operation(validated))
    }

    pub fn execute_root_endpoint_with_control<'a, T, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        F: FnOnce(ExecutionControl, ValidatedAdmin) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, ApiError>> + Send + 'a,
    {
        self.execute_admin_endpoint_with_control(
            metadata,
            move |request_context, validated| async move {
                if !matches!(validated.role, UserRole::Root) {
                    return Err(ApiError::Authorization("Root role required".to_string()));
                }
                operation(request_context, validated).await
            },
        )
    }

    pub(in crate::impls::admin) fn map_admin_auth_user_lookup_error(
        err: synctv_core::Error,
    ) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => {
                ApiError::Authentication("Authentication failed".to_string())
            }
            other => ApiError::from(other),
        }
    }

    pub(in crate::impls::admin) fn map_target_user_lookup_error(
        err: synctv_core::Error,
    ) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => ApiError::NotFound("User not found".to_string()),
            other => ApiError::from(other),
        }
    }

    pub(in crate::impls::admin) async fn load_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AdminActor, ApiError> {
        if *admin_user_id == LOCAL_MANAGEMENT_ACTOR_USER_ID {
            return Ok(AdminActor {
                username: "local-management".to_string(),
                role: UserRole::Root,
            });
        }

        let user = self.user_service.get_user(admin_user_id).await?;
        Ok(AdminActor {
            username: user.username,
            role: user.role,
        })
    }

    pub(in crate::impls::admin) async fn require_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AdminActor, ApiError> {
        let actor = self.load_admin_actor(admin_user_id).await?;
        if !actor.role.is_admin_or_above() {
            return Err(ApiError::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }
        Ok(actor)
    }

    pub(in crate::impls::admin) async fn require_authorized_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AuthorizedAdminActor, ApiError> {
        let actor = self.require_admin_actor(admin_user_id).await?;
        AuthorizedAdminActor::new(*admin_user_id, actor.username, actor.role)
            .map_err(ApiError::from)
    }
}
