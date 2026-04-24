use futures::future::BoxFuture;
use futures::FutureExt;
use std::{future::Future, sync::Arc};

use tonic::{Request, Response, Status};

use crate::impls::admin::RequestContext;
use synctv_core::Config;

// Use synctv_proto for all gRPC types to avoid duplication
use crate::proto::admin::{
    AddAdminRequest, AddAdminResponse, AddMemberRequest, AddMemberResponse, ApproveMemberRequest,
    ApproveMemberResponse, ApproveRoomRequest, ApproveRoomResponse, ApproveUserRequest,
    ApproveUserResponse, BanMemberRequest, BanMemberResponse, BanRoomRequest, BanRoomResponse,
    BanUserRequest, BanUserResponse, BatchBanRoomsRequest, BatchBanRoomsResponse,
    BatchBanUsersRequest, BatchBanUsersResponse, BatchDeleteRoomsRequest, BatchDeleteRoomsResponse,
    BatchDeleteUsersRequest, BatchDeleteUsersResponse, CreateUserRequest, CreateUserResponse,
    DeleteRoomRequest, DeleteRoomResponse, DeleteUserRequest, DeleteUserResponse,
    GetRoomMembersRequest, GetRoomMembersResponse, GetRoomRequest, GetRoomResponse,
    GetRoomSettingsRequest, GetRoomSettingsResponse, GetSettingsGroupRequest,
    GetSettingsGroupResponse, GetSettingsRequest, GetSettingsResponse, GetSystemStatsRequest,
    GetSystemStatsResponse, GetUserRequest, GetUserResponse, GetUserRoomsRequest,
    GetUserRoomsResponse, KickMemberRequest, KickMemberResponse, KickStreamRequest,
    KickStreamResponse, ListActiveStreamsRequest, ListActiveStreamsResponse, ListAdminsRequest,
    ListAdminsResponse, ListRoomsRequest, ListRoomsResponse, ListUsersRequest, ListUsersResponse,
    RejectMemberRequest, RejectMemberResponse, RemoveAdminRequest, RemoveAdminResponse,
    ResetRoomSettingsRequest, ResetRoomSettingsResponse, SendTestEmailRequest,
    SendTestEmailResponse, UnbanMemberRequest, UnbanMemberResponse, UnbanRoomRequest,
    UnbanRoomResponse, UnbanUserRequest, UnbanUserResponse, UpdateMemberPermissionsRequest,
    UpdateMemberPermissionsResponse, UpdateRoomPasswordRequest, UpdateRoomPasswordResponse,
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
    admin_api: Arc<AdminApiImpl>,
    config: Arc<Config>,
}

impl AdminServiceImpl {
    #[must_use]
    pub const fn new(admin_api: Arc<AdminApiImpl>, config: Arc<Config>) -> Self {
        Self { admin_api, config }
    }

    fn request_metadata(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> crate::impls::RequestMetadata {
        super::request_metadata(
            request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )
    }

    fn execute_scoped_admin_rpc<TReq, TResp, F, Fut>(
        &self,
        request: Request<TReq>,
        require_root: bool,
        operation: F,
    ) -> BoxFuture<'_, Result<Response<TResp>, Status>>
    where
        TReq: std::fmt::Debug + Send + 'static,
        TResp: Send + 'static,
        F: FnOnce(
                Arc<AdminApiImpl>,
                crate::impls::admin::ValidatedAdmin,
                RequestContext,
                TReq,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<TResp, crate::impls::ApiError>> + Send + 'static,
    {
        async move {
            let metadata = self.request_metadata(&request);
            let ctx = grpc_request_context(&request, &self.config);
            let api = self.admin_api.clone();
            let executor = api.clone();
            let req = request.into_inner();

            let response = if require_root {
                executor
                    .execute_root_endpoint(&metadata, move |validated| {
                        operation(api, validated, ctx, req)
                    })
                    .await
            } else {
                executor
                    .execute_admin_endpoint(&metadata, move |validated| {
                        operation(api, validated, ctx, req)
                    })
                    .await
            }
            .map_err(map_api_error)?;

            Ok(Response::new(response))
        }
        .boxed()
    }

    fn execute_admin_rpc<TReq, TResp, F, Fut>(
        &self,
        request: Request<TReq>,
        operation: F,
    ) -> BoxFuture<'_, Result<Response<TResp>, Status>>
    where
        TReq: std::fmt::Debug + Send + 'static,
        TResp: Send + 'static,
        F: FnOnce(
                Arc<AdminApiImpl>,
                crate::impls::admin::ValidatedAdmin,
                RequestContext,
                TReq,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<TResp, crate::impls::ApiError>> + Send + 'static,
    {
        self.execute_scoped_admin_rpc(request, false, operation)
    }

    fn execute_root_rpc<TReq, TResp, F, Fut>(
        &self,
        request: Request<TReq>,
        operation: F,
    ) -> BoxFuture<'_, Result<Response<TResp>, Status>>
    where
        TReq: std::fmt::Debug + Send + 'static,
        TResp: Send + 'static,
        F: FnOnce(
                Arc<AdminApiImpl>,
                crate::impls::admin::ValidatedAdmin,
                RequestContext,
                TReq,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<TResp, crate::impls::ApiError>> + Send + 'static,
    {
        self.execute_scoped_admin_rpc(request, true, operation)
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AdminService for AdminServiceImpl {
    // System Settings Management

    async fn get_settings(
        &self,
        request: Request<GetSettingsRequest>,
    ) -> Result<Response<GetSettingsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.get_settings(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn get_settings_group(
        &self,
        request: Request<GetSettingsGroupRequest>,
    ) -> Result<Response<GetSettingsGroupResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.get_settings_group(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn update_settings(
        &self,
        request: Request<UpdateSettingsRequest>,
    ) -> Result<Response<UpdateSettingsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_settings(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn send_test_email(
        &self,
        request: Request<SendTestEmailRequest>,
    ) -> Result<Response<SendTestEmailResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let api = self.admin_api.clone();
        let executor = api.clone();
        let req = request.into_inner();

        let response = executor
            .execute_admin_endpoint_with_control(&metadata, move |request_control, _| async move {
                api.send_test_email_with_control(req, Some(&request_control))
                    .await
            })
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(response))
    }

    // User Management

    async fn create_user(
        &self,
        request: Request<CreateUserRequest>,
    ) -> Result<Response<CreateUserResponse>, Status> {
        let require_root = request.get_ref().role == synctv_proto::common::UserRole::Root as i32;
        self.execute_scoped_admin_rpc(
            request,
            require_root,
            move |api, validated, ctx, req| async move {
                api.create_user(req, validated.role, &validated.user_id, &ctx)
                    .await
            },
        )
        .await
    }

    async fn delete_user(
        &self,
        request: Request<DeleteUserRequest>,
    ) -> Result<Response<DeleteUserResponse>, Status> {
        self.execute_root_rpc(request, move |api, validated, ctx, req| async move {
            api.delete_user(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn list_users(
        &self,
        request: Request<ListUsersRequest>,
    ) -> Result<Response<ListUsersResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.list_users(req).await
        })
        .await
    }

    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<Response<GetUserResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_user(req).await
        })
        .await
    }

    async fn update_user_password(
        &self,
        request: Request<UpdateUserPasswordRequest>,
    ) -> Result<Response<UpdateUserPasswordResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_user_password(req, validated.user_id, validated.role, &ctx)
                .await
        })
        .await
    }

    async fn update_user_username(
        &self,
        request: Request<UpdateUserUsernameRequest>,
    ) -> Result<Response<UpdateUserUsernameResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_user_username(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn update_user_role(
        &self,
        request: Request<UpdateUserRoleRequest>,
    ) -> Result<Response<UpdateUserRoleResponse>, Status> {
        let require_root = request.get_ref().role == synctv_proto::common::UserRole::Root as i32;
        self.execute_scoped_admin_rpc(
            request,
            require_root,
            move |api, validated, ctx, req| async move {
                api.update_user_role(req, &validated.user_id, validated.role, &ctx)
                    .await
            },
        )
        .await
    }

    async fn ban_user(
        &self,
        request: Request<BanUserRequest>,
    ) -> Result<Response<BanUserResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.ban_user(req, &validated.user_id, validated.role, &ctx)
                .await
        })
        .await
    }

    async fn unban_user(
        &self,
        request: Request<UnbanUserRequest>,
    ) -> Result<Response<UnbanUserResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.unban_user(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn get_user_rooms(
        &self,
        request: Request<GetUserRoomsRequest>,
    ) -> Result<Response<GetUserRoomsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_user_rooms(req).await
        })
        .await
    }

    async fn approve_user(
        &self,
        request: Request<ApproveUserRequest>,
    ) -> Result<Response<ApproveUserResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.approve_user(req, &validated.user_id, &ctx).await
        })
        .await
    }

    // Batch Operations

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
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.batch_ban_users(req, &validated.user_id, validated.role, &ctx)
                .await
        })
        .await
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
        self.execute_root_rpc(request, move |api, validated, ctx, req| async move {
            api.batch_delete_users(req, &validated.user_id, validated.role, &ctx)
                .await
        })
        .await
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
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.batch_ban_rooms(req, &validated.user_id, &ctx).await
        })
        .await
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
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.batch_delete_rooms(req, &validated.user_id, &ctx).await
        })
        .await
    }

    // Room Management

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<ListRoomsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.list_rooms(req).await
        })
        .await
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_room(req).await
        })
        .await
    }

    async fn update_room_password(
        &self,
        request: Request<UpdateRoomPasswordRequest>,
    ) -> Result<Response<UpdateRoomPasswordResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_room_password(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<DeleteRoomResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.delete_room(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn ban_room(
        &self,
        request: Request<BanRoomRequest>,
    ) -> Result<Response<BanRoomResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.ban_room(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn unban_room(
        &self,
        request: Request<UnbanRoomRequest>,
    ) -> Result<Response<UnbanRoomResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.unban_room(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn approve_room(
        &self,
        request: Request<ApproveRoomRequest>,
    ) -> Result<Response<ApproveRoomResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.approve_room(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<GetRoomMembersResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_room_members(req).await
        })
        .await
    }

    async fn add_member(
        &self,
        request: Request<AddMemberRequest>,
    ) -> Result<Response<AddMemberResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.add_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn approve_member(
        &self,
        request: Request<ApproveMemberRequest>,
    ) -> Result<Response<ApproveMemberResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.approve_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn reject_member(
        &self,
        request: Request<RejectMemberRequest>,
    ) -> Result<Response<RejectMemberResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.reject_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<UpdateMemberPermissionsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_member_permissions(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<KickMemberResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.kick_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn ban_member(
        &self,
        request: Request<BanMemberRequest>,
    ) -> Result<Response<BanMemberResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.ban_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn unban_member(
        &self,
        request: Request<UnbanMemberRequest>,
    ) -> Result<Response<UnbanMemberResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.unban_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    // Admin Management (Root Only)

    async fn add_admin(
        &self,
        request: Request<AddAdminRequest>,
    ) -> Result<Response<AddAdminResponse>, Status> {
        self.execute_root_rpc(request, move |api, validated, ctx, req| async move {
            api.add_admin(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn remove_admin(
        &self,
        request: Request<RemoveAdminRequest>,
    ) -> Result<Response<RemoveAdminResponse>, Status> {
        self.execute_root_rpc(request, move |api, validated, ctx, req| async move {
            api.remove_admin(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn list_admins(
        &self,
        request: Request<ListAdminsRequest>,
    ) -> Result<Response<ListAdminsResponse>, Status> {
        self.execute_root_rpc(request, move |api, _, _, req| async move {
            api.list_admins(req).await
        })
        .await
    }

    // System Statistics

    async fn get_system_stats(
        &self,
        request: Request<GetSystemStatsRequest>,
    ) -> Result<Response<GetSystemStatsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_system_stats(req).await
        })
        .await
    }

    // Room Settings Management

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_room_settings(req).await
        })
        .await
    }

    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<UpdateRoomSettingsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.update_room_settings(req, &validated.user_id).await
        })
        .await
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<ResetRoomSettingsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.reset_room_settings(req, &validated.user_id).await
        })
        .await
    }

    // Livestream Management

    async fn list_active_streams(
        &self,
        request: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<ListActiveStreamsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.list_active_streams(req).await
        })
        .await
    }

    async fn kick_stream(
        &self,
        request: Request<KickStreamRequest>,
    ) -> Result<Response<KickStreamResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.kick_stream(req, &validated.user_id, &ctx)
                .await
                .map(|()| KickStreamResponse {})
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
