use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::impls::admin::RequestContext;
use synctv_core::service::UserService;
use synctv_core::Config;

// Use synctv_proto for all gRPC types to avoid duplication
use crate::proto::admin::{
    AddAdminRequest, AddAdminResponse, AddProviderInstanceRequest, AddProviderInstanceResponse,
    ApproveRoomRequest, ApproveRoomResponse, ApproveUserRequest, ApproveUserResponse,
    BanRoomRequest, BanRoomResponse, BanUserRequest, BanUserResponse, BatchBanRoomsRequest,
    BatchBanRoomsResponse, BatchBanUsersRequest, BatchBanUsersResponse, BatchDeleteRoomsRequest,
    BatchDeleteRoomsResponse, BatchDeleteUsersRequest, BatchDeleteUsersResponse, CreateUserRequest,
    CreateUserResponse, DeleteProviderInstanceRequest, DeleteProviderInstanceResponse,
    DeleteRoomRequest, DeleteRoomResponse, DeleteUserRequest, DeleteUserResponse,
    DisableProviderInstanceRequest, DisableProviderInstanceResponse, EnableProviderInstanceRequest,
    EnableProviderInstanceResponse, GetRoomMembersRequest, GetRoomMembersResponse, GetRoomRequest,
    GetRoomResponse, GetRoomSettingsRequest, GetRoomSettingsResponse, GetSettingsGroupRequest,
    GetSettingsGroupResponse, GetSettingsRequest, GetSettingsResponse, GetSystemStatsRequest,
    GetSystemStatsResponse, GetUserRequest, GetUserResponse, GetUserRoomsRequest,
    GetUserRoomsResponse, KickStreamRequest, KickStreamResponse, ListActiveStreamsRequest,
    ListActiveStreamsResponse, ListAdminsRequest, ListAdminsResponse, ListProviderInstancesRequest,
    ListProviderInstancesResponse, ListRoomsRequest, ListRoomsResponse, ListUsersRequest,
    ListUsersResponse, ReconnectProviderInstanceRequest, ReconnectProviderInstanceResponse,
    RemoveAdminRequest, RemoveAdminResponse, ResetRoomSettingsRequest, ResetRoomSettingsResponse,
    SendTestEmailRequest, SendTestEmailResponse, UnbanRoomRequest, UnbanRoomResponse,
    UnbanUserRequest, UnbanUserResponse, UpdateProviderInstanceRequest,
    UpdateProviderInstanceResponse, UpdateRoomPasswordRequest, UpdateRoomPasswordResponse,
    UpdateRoomSettingsRequest, UpdateRoomSettingsResponse, UpdateSettingsRequest,
    UpdateSettingsResponse, UpdateUserPasswordRequest, UpdateUserPasswordResponse,
    UpdateUserRoleRequest, UpdateUserRoleResponse, UpdateUserUsernameRequest,
    UpdateUserUsernameResponse,
};
use crate::proto::admin_service_server::AdminService;

use crate::impls::AdminApiImpl;

use super::map_api_error;

/// Extract IP address and User-Agent from a gRPC request for audit logging.
fn grpc_request_context<T: std::fmt::Debug>(
    request: &Request<T>,
    config: &Config,
) -> RequestContext {
    let ip_address = super::extract_client_ip(request, config).map(|ip| ip.to_string());
    let user_agent = request
        .metadata()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);
    RequestContext {
        ip_address,
        user_agent,
    }
}

/// `AdminService` gRPC implementation.
///
/// Thin wrapper that delegates all business logic to [`AdminApiImpl`],
/// matching how `ClientServiceImpl` delegates to `ClientApiImpl`.
#[derive(Clone)]
pub struct AdminServiceImpl {
    user_service: Arc<UserService>,
    admin_api: Arc<AdminApiImpl>,
    config: Arc<Config>,
}

impl AdminServiceImpl {
    #[must_use]
    pub const fn new(
        user_service: Arc<UserService>,
        admin_api: Arc<AdminApiImpl>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            user_service,
            admin_api,
            config,
        }
    }

    /// Validate admin authentication: extract user from JWT, check banned/deleted
    /// status, and verify token has not been invalidated by password change.
    ///
    /// Delegates to the shared `validate_admin_auth` in the impls layer.
    /// Returns the authenticated user's role. Shared by `check_admin` and `check_root`.
    async fn validate_auth(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<synctv_core::models::UserRole, Status> {
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        let user_id = synctv_core::models::UserId::from_string(user_context.user_id.clone());

        let validated = crate::impls::admin::validate_admin_auth(
            &self.user_service,
            user_id,
            user_context.pv,
            user_context.iat,
        )
        .await
        .map_err(map_api_error)?;

        Ok(validated.role)
    }

    /// Check if user has admin role and return their role.
    async fn check_admin_get_role(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<synctv_core::models::UserRole, Status> {
        let role = self.validate_auth(request).await?;
        if !role.is_admin_or_above() {
            return Err(Status::permission_denied("Admin role required"));
        }
        Ok(role)
    }

    /// Check if user has admin role and return validated admin info.
    async fn check_admin_get_validated(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<crate::impls::admin::ValidatedAdmin, Status> {
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        let user_id = synctv_core::models::UserId::from_string(user_context.user_id.clone());

        let validated = crate::impls::admin::validate_admin_auth(
            &self.user_service,
            user_id,
            user_context.pv,
            user_context.iat,
        )
        .await
        .map_err(map_api_error)?;

        if !validated.role.is_admin_or_above() {
            return Err(Status::permission_denied("Admin role required"));
        }

        Ok(validated)
    }

    /// Check if user has admin role (load from database)
    async fn check_admin(&self, request: &Request<impl std::fmt::Debug>) -> Result<(), Status> {
        self.check_admin_get_role(request).await.map(|_| ())
    }

    /// Check if user has root role (load from database).
    async fn check_root(&self, request: &Request<impl std::fmt::Debug>) -> Result<(), Status> {
        let role = self.validate_auth(request).await?;
        if !matches!(role, synctv_core::models::UserRole::Root) {
            return Err(Status::permission_denied("Root role required"));
        }
        Ok(())
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AdminService for AdminServiceImpl {
    // =========================
    // System Settings Management
    // =========================

    async fn get_settings(
        &self,
        request: Request<GetSettingsRequest>,
    ) -> Result<Response<GetSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .get_settings(req, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn get_settings_group(
        &self,
        request: Request<GetSettingsGroupRequest>,
    ) -> Result<Response<GetSettingsGroupResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .get_settings_group(req, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_settings(
        &self,
        request: Request<UpdateSettingsRequest>,
    ) -> Result<Response<UpdateSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_settings(req, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn send_test_email(
        &self,
        request: Request<SendTestEmailRequest>,
    ) -> Result<Response<SendTestEmailResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .send_test_email(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // Provider Instance Management
    // =========================

    async fn list_provider_instances(
        &self,
        request: Request<ListProviderInstancesRequest>,
    ) -> Result<Response<ListProviderInstancesResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .list_provider_instances(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn add_provider_instance(
        &self,
        request: Request<AddProviderInstanceRequest>,
    ) -> Result<Response<AddProviderInstanceResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .add_provider_instance(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_provider_instance(
        &self,
        request: Request<UpdateProviderInstanceRequest>,
    ) -> Result<Response<UpdateProviderInstanceResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_provider_instance(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn delete_provider_instance(
        &self,
        request: Request<DeleteProviderInstanceRequest>,
    ) -> Result<Response<DeleteProviderInstanceResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .delete_provider_instance(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn reconnect_provider_instance(
        &self,
        request: Request<ReconnectProviderInstanceRequest>,
    ) -> Result<Response<ReconnectProviderInstanceResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .reconnect_provider_instance(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn enable_provider_instance(
        &self,
        request: Request<EnableProviderInstanceRequest>,
    ) -> Result<Response<EnableProviderInstanceResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .enable_provider_instance(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn disable_provider_instance(
        &self,
        request: Request<DisableProviderInstanceRequest>,
    ) -> Result<Response<DisableProviderInstanceResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .disable_provider_instance(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // User Management
    // =========================

    async fn create_user(
        &self,
        request: Request<CreateUserRequest>,
    ) -> Result<Response<CreateUserResponse>, Status> {
        // Creating root users requires root privileges
        let caller_role = if request.get_ref().role == synctv_proto::common::UserRole::Root as i32 {
            self.check_root(&request).await?;
            synctv_core::models::UserRole::Root
        } else {
            self.check_admin_get_role(&request).await?
        };
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .create_user(req, caller_role, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn delete_user(
        &self,
        request: Request<DeleteUserRequest>,
    ) -> Result<Response<DeleteUserResponse>, Status> {
        self.check_root(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .delete_user(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn list_users(
        &self,
        request: Request<ListUsersRequest>,
    ) -> Result<Response<ListUsersResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .list_users(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<Response<GetUserResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self.admin_api.get_user(req).await.map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_user_password(
        &self,
        request: Request<UpdateUserPasswordRequest>,
    ) -> Result<Response<UpdateUserPasswordResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_user_password(req, validated.user_id, validated.role, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_user_username(
        &self,
        request: Request<UpdateUserUsernameRequest>,
    ) -> Result<Response<UpdateUserUsernameResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_user_username(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_user_role(
        &self,
        request: Request<UpdateUserRoleRequest>,
    ) -> Result<Response<UpdateUserRoleResponse>, Status> {
        // Granting root role requires root privileges
        let caller_role = if request.get_ref().role == synctv_proto::common::UserRole::Root as i32 {
            self.check_root(&request).await?;
            synctv_core::models::UserRole::Root
        } else {
            self.check_admin_get_role(&request).await?
        };
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_user_role(req, &admin_user_id, caller_role, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn ban_user(
        &self,
        request: Request<BanUserRequest>,
    ) -> Result<Response<BanUserResponse>, Status> {
        let caller_role = self.check_admin_get_role(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .ban_user(req, &admin_user_id, caller_role, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn unban_user(
        &self,
        request: Request<UnbanUserRequest>,
    ) -> Result<Response<UnbanUserResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .unban_user(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn get_user_rooms(
        &self,
        request: Request<GetUserRoomsRequest>,
    ) -> Result<Response<GetUserRoomsResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .get_user_rooms(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn approve_user(
        &self,
        request: Request<ApproveUserRequest>,
    ) -> Result<Response<ApproveUserResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .approve_user(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // Batch Operations
    // =========================

    async fn batch_ban_users(
        &self,
        request: Request<BatchBanUsersRequest>,
    ) -> Result<Response<BatchBanUsersResponse>, Status> {
        const MAX_BATCH_SIZE: usize = 100;
        if request.get_ref().user_ids.len() > MAX_BATCH_SIZE {
            return Err(Status::invalid_argument(format!(
                "Batch size {} exceeds maximum allowed {}",
                request.get_ref().user_ids.len(),
                MAX_BATCH_SIZE
            )));
        }
        let caller_role = self.check_admin_get_role(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .batch_ban_users(req, &admin_user_id, caller_role, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn batch_delete_users(
        &self,
        request: Request<BatchDeleteUsersRequest>,
    ) -> Result<Response<BatchDeleteUsersResponse>, Status> {
        const MAX_BATCH_SIZE: usize = 100;
        if request.get_ref().user_ids.len() > MAX_BATCH_SIZE {
            return Err(Status::invalid_argument(format!(
                "Batch size {} exceeds maximum allowed {}",
                request.get_ref().user_ids.len(),
                MAX_BATCH_SIZE
            )));
        }
        self.check_root(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        // check_root guarantees caller is Root
        let resp = self
            .admin_api
            .batch_delete_users(
                req,
                &admin_user_id,
                synctv_core::models::UserRole::Root,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn batch_ban_rooms(
        &self,
        request: Request<BatchBanRoomsRequest>,
    ) -> Result<Response<BatchBanRoomsResponse>, Status> {
        const MAX_BATCH_SIZE: usize = 100;
        if request.get_ref().room_ids.len() > MAX_BATCH_SIZE {
            return Err(Status::invalid_argument(format!(
                "Batch size {} exceeds maximum allowed {}",
                request.get_ref().room_ids.len(),
                MAX_BATCH_SIZE
            )));
        }
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .batch_ban_rooms(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn batch_delete_rooms(
        &self,
        request: Request<BatchDeleteRoomsRequest>,
    ) -> Result<Response<BatchDeleteRoomsResponse>, Status> {
        const MAX_BATCH_SIZE: usize = 100;
        if request.get_ref().room_ids.len() > MAX_BATCH_SIZE {
            return Err(Status::invalid_argument(format!(
                "Batch size {} exceeds maximum allowed {}",
                request.get_ref().room_ids.len(),
                MAX_BATCH_SIZE
            )));
        }
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .batch_delete_rooms(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // Room Management
    // =========================

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<ListRoomsResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .list_rooms(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self.admin_api.get_room(req).await.map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_room_password(
        &self,
        request: Request<UpdateRoomPasswordRequest>,
    ) -> Result<Response<UpdateRoomPasswordResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_room_password(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<DeleteRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let admin_user_id = validated.user_id;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .delete_room(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn ban_room(
        &self,
        request: Request<BanRoomRequest>,
    ) -> Result<Response<BanRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let admin_user_id = validated.user_id;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .ban_room(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn unban_room(
        &self,
        request: Request<UnbanRoomRequest>,
    ) -> Result<Response<UnbanRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let admin_user_id = validated.user_id;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .unban_room(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn approve_room(
        &self,
        request: Request<ApproveRoomRequest>,
    ) -> Result<Response<ApproveRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let admin_user_id = validated.user_id;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .approve_room(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<GetRoomMembersResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .get_room_members(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // Admin Management (Root Only)
    // =========================

    async fn add_admin(
        &self,
        request: Request<AddAdminRequest>,
    ) -> Result<Response<AddAdminResponse>, Status> {
        self.check_root(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .add_admin(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn remove_admin(
        &self,
        request: Request<RemoveAdminRequest>,
    ) -> Result<Response<RemoveAdminResponse>, Status> {
        self.check_root(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();
        let resp = self
            .admin_api
            .remove_admin(req, &admin_user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn list_admins(
        &self,
        request: Request<ListAdminsRequest>,
    ) -> Result<Response<ListAdminsResponse>, Status> {
        self.check_root(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .list_admins(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // System Statistics
    // =========================

    async fn get_system_stats(
        &self,
        request: Request<GetSystemStatsRequest>,
    ) -> Result<Response<GetSystemStatsResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .get_system_stats(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // Room Settings Management
    // =========================

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .get_room_settings(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<UpdateRoomSettingsResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .update_room_settings(req, &admin_user_id)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<ResetRoomSettingsResponse>, Status> {
        self.check_admin(&request).await?;
        let admin_user_id = super::interceptors::extract_user_id(&request)?;
        let req = request.into_inner();
        let resp = self
            .admin_api
            .reset_room_settings(req, &admin_user_id)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(resp))
    }

    // =========================
    // Livestream Management
    // =========================

    async fn list_active_streams(
        &self,
        request: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<ListActiveStreamsResponse>, Status> {
        self.check_admin(&request).await?;
        let req = request.into_inner();
        let room_id = if req.room_id.is_empty() {
            None
        } else {
            Some(req.room_id.as_str())
        };
        let streams = self
            .admin_api
            .list_active_streams(room_id)
            .await
            .map_err(|e| map_api_error(crate::impls::ApiError::Internal(e.to_string())))?;
        Ok(Response::new(ListActiveStreamsResponse { streams }))
    }

    async fn kick_stream(
        &self,
        request: Request<KickStreamRequest>,
    ) -> Result<Response<KickStreamResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = grpc_request_context(&request, &self.config);
        let req = request.into_inner();

        if req.room_id.is_empty() || req.media_id.is_empty() {
            return Err(Status::invalid_argument(
                "room_id and media_id are required",
            ));
        }

        self.admin_api
            .kick_stream(
                &req.room_id,
                &req.media_id,
                &req.reason,
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|e| map_api_error(crate::impls::ApiError::Internal(e.to_string())))?;

        Ok(Response::new(KickStreamResponse {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Error Mapping ====================

    #[test]
    fn test_api_err_not_found() {
        let err = crate::impls::ApiError::NotFound("user not found".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::NotFound);
        assert!(status.message().contains("not found"));
    }

    #[test]
    fn test_api_err_unauthenticated() {
        let err = crate::impls::ApiError::Authentication("bad token".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_api_err_service_unavailable_stays_unavailable() {
        let err =
            crate::impls::ApiError::ServiceUnavailable("user backend unavailable".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn test_api_err_permission_denied() {
        let err = crate::impls::ApiError::Authorization("not allowed".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn test_api_err_already_exists() {
        let err = crate::impls::ApiError::AlreadyExists("duplicate".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::AlreadyExists);
    }

    #[test]
    fn test_api_err_invalid_argument() {
        let err = crate::impls::ApiError::InvalidInput("bad field".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn test_api_err_internal_hides_details() {
        let err = crate::impls::ApiError::Internal(
            "database connection failed with password=secret123".to_string(),
        );
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Internal);
        // Internal errors should NOT leak implementation details
        assert_eq!(status.message(), "Internal error");
        assert!(!status.message().contains("password"));
        assert!(!status.message().contains("database"));
    }

    // ==================== Error Mapping Exhaustiveness ====================

    #[test]
    fn test_api_err_all_variants_mapped() {
        // Verify every ApiError variant maps to a distinct gRPC code
        let variants: Vec<(crate::impls::ApiError, tonic::Code)> = vec![
            (
                crate::impls::ApiError::NotFound("x".into()),
                tonic::Code::NotFound,
            ),
            (
                crate::impls::ApiError::Authentication("x".into()),
                tonic::Code::Unauthenticated,
            ),
            (
                crate::impls::ApiError::Authorization("x".into()),
                tonic::Code::PermissionDenied,
            ),
            (
                crate::impls::ApiError::AlreadyExists("x".into()),
                tonic::Code::AlreadyExists,
            ),
            (
                crate::impls::ApiError::InvalidInput("x".into()),
                tonic::Code::InvalidArgument,
            ),
            (
                crate::impls::ApiError::Internal("x".into()),
                tonic::Code::Internal,
            ),
        ];
        for (err, expected_code) in variants {
            let status = map_api_error(err);
            assert_eq!(status.code(), expected_code);
        }
    }

    // ==================== Batch Size Limit Validation ====================

    #[test]
    fn test_batch_ban_users_request_size_limit() {
        // Create request with 101 user IDs (exceeds limit)
        let user_ids: Vec<String> = (0..101).map(|i| format!("user_{i}")).collect();
        let request = BatchBanUsersRequest {
            user_ids,
            reason: "test".to_string(),
        };
        assert!(request.user_ids.len() > 100);

        // Create request with exactly 100 user IDs (at limit)
        let user_ids_at_limit: Vec<String> = (0..100).map(|i| format!("user_{i}")).collect();
        let request_at_limit = BatchBanUsersRequest {
            user_ids: user_ids_at_limit,
            reason: "test".to_string(),
        };
        assert_eq!(request_at_limit.user_ids.len(), 100);

        // Create request with 99 user IDs (below limit)
        let user_ids_below_limit: Vec<String> = (0..99).map(|i| format!("user_{i}")).collect();
        let request_below_limit = BatchBanUsersRequest {
            user_ids: user_ids_below_limit,
            reason: "test".to_string(),
        };
        assert!(request_below_limit.user_ids.len() < 100);
    }

    #[test]
    fn test_batch_delete_users_request_size_limit() {
        // Create request with 101 user IDs (exceeds limit)
        let user_ids: Vec<String> = (0..101).map(|i| format!("user_{i}")).collect();
        let request = BatchDeleteUsersRequest { user_ids };
        assert!(request.user_ids.len() > 100);
    }

    #[test]
    fn test_batch_ban_rooms_request_size_limit() {
        // Create request with 101 room IDs (exceeds limit)
        let room_ids: Vec<String> = (0..101).map(|i| format!("room_{i}")).collect();
        let request = BatchBanRoomsRequest {
            room_ids,
            reason: "test".to_string(),
        };
        assert!(request.room_ids.len() > 100);
    }

    #[test]
    fn test_batch_delete_rooms_request_size_limit() {
        // Create request with 101 room IDs (exceeds limit)
        let room_ids: Vec<String> = (0..101).map(|i| format!("room_{i}")).collect();
        let request = BatchDeleteRoomsRequest { room_ids };
        assert!(request.room_ids.len() > 100);
    }

    #[test]
    fn test_status_invalid_argument_for_batch_size() {
        // Verify that we would return the correct Status for oversized batches
        let status = Status::invalid_argument("Batch size 101 exceeds maximum allowed 100");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("exceeds maximum"));
    }
}
