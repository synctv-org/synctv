use futures::future::BoxFuture;
use futures::FutureExt;
use std::{future::Future, sync::Arc};

use tonic::{Request, Response, Status};

use crate::impls::admin::RequestContext;
use synctv_core::Config;

// Use synctv_proto for all gRPC types to avoid duplication
use synctv_proto::admin::admin_service_server::AdminService;
use synctv_proto::admin::{
    AddAdminRequest, AddAdminResponse, AddMemberRequest, AddMemberResponse,
    ApproveRoomCreationReviewRequest, ApproveRoomCreationReviewResponse,
    ApproveRoomJoinReviewRequest, ApproveRoomJoinReviewResponse,
    ApproveUserRegistrationReviewRequest, ApproveUserRegistrationReviewResponse, BanRoomRequest,
    BanRoomResponse, BanUserRequest, BanUserResponse, BatchBanRoomsRequest, BatchBanRoomsResponse,
    BatchBanUsersRequest, BatchBanUsersResponse, BatchDeleteRoomsRequest, BatchDeleteRoomsResponse,
    BatchDeleteUsersRequest, BatchDeleteUsersResponse, CreateUserRequest, CreateUserResponse,
    DeleteRoomRequest, DeleteRoomResponse, DeleteUserRequest, DeleteUserResponse,
    GetContentReportRequest, GetContentReportResponse, GetRoomMembersRequest,
    GetRoomMembersResponse, GetRoomRequest, GetRoomResponse, GetRoomSettingsRequest,
    GetRoomSettingsResponse, GetSettingsGroupRequest, GetSettingsGroupResponse, GetSettingsRequest,
    GetSettingsResponse, GetSystemStatsRequest, GetSystemStatsResponse, GetUserPreferencesRequest,
    GetUserPreferencesResponse, GetUserRequest, GetUserResponse, GetUserRoomsRequest,
    GetUserRoomsResponse, KickMemberRequest, KickMemberResponse, KickStreamRequest,
    KickStreamResponse, ListActiveStreamsRequest, ListActiveStreamsResponse, ListAdminsRequest,
    ListAdminsResponse, ListBanRecordsRequest, ListBanRecordsResponse, ListContentReportsRequest,
    ListContentReportsResponse, ListRoomCreationReviewsRequest, ListRoomCreationReviewsResponse,
    ListRoomJoinReviewsRequest, ListRoomJoinReviewsResponse, ListRoomsRequest, ListRoomsResponse,
    ListUserRegistrationReviewsRequest, ListUserRegistrationReviewsResponse, ListUsersRequest,
    ListUsersResponse, RejectRoomCreationReviewRequest, RejectRoomCreationReviewResponse,
    RejectRoomJoinReviewRequest, RejectRoomJoinReviewResponse, RejectUserRegistrationReviewRequest,
    RejectUserRegistrationReviewResponse, RemoveAdminRequest, RemoveAdminResponse,
    ResetRoomSettingsRequest, ResetRoomSettingsResponse, SendTestEmailRequest,
    SendTestEmailResponse, SetUserPasswordRequest, SetUserPasswordResponse, UnbanRoomRequest,
    UnbanRoomResponse, UnbanUserRequest, UnbanUserResponse, UpdateContentReportStatusRequest,
    UpdateContentReportStatusResponse, UpdateMemberPermissionsRequest,
    UpdateMemberPermissionsResponse, UpdateRoomPasswordRequest, UpdateRoomPasswordResponse,
    UpdateRoomSettingsRequest, UpdateRoomSettingsResponse, UpdateSettingsRequest,
    UpdateSettingsResponse, UpdateUserPreferencesRequest, UpdateUserPreferencesResponse,
    UpdateUserRoleRequest, UpdateUserRoleResponse, UpdateUserUsernameRequest,
    UpdateUserUsernameResponse,
};

use crate::impls::AdminApiImpl;

use super::map_api_error;

/// Extract IP address and User-Agent from a gRPC request for audit logging.
fn grpc_request_context<T: std::fmt::Debug>(
    request: &Request<T>,
    config: &Config,
) -> Result<RequestContext, Status> {
    let ip_address = super::extract_client_ip(request, config)?.map(|ip| ip.to_string());
    let user_agent = super::request_user_agent(request)?;
    Ok(RequestContext {
        ip_address,
        user_agent,
    })
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
    ) -> Result<crate::impls::RequestMetadata, Status> {
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
            let metadata = self.request_metadata(&request)?;
            let ctx = grpc_request_context(&request, &self.config)?;
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
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
// Business logic stays in `AdminApiImpl` and returns `ApiError`.
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
        let metadata = self.request_metadata(&request)?;
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

    async fn get_user_preferences(
        &self,
        request: Request<GetUserPreferencesRequest>,
    ) -> Result<Response<GetUserPreferencesResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            api.get_user_preferences(req).await
        })
        .await
    }

    async fn update_user_preferences(
        &self,
        request: Request<UpdateUserPreferencesRequest>,
    ) -> Result<Response<UpdateUserPreferencesResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_user_preferences(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn set_user_password(
        &self,
        request: Request<SetUserPasswordRequest>,
    ) -> Result<Response<SetUserPasswordResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.set_user_password(req, validated.user_id, validated.role, &ctx)
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

    // Batch Operations

    async fn batch_ban_users(
        &self,
        request: Request<BatchBanUsersRequest>,
    ) -> Result<Response<BatchBanUsersResponse>, Status> {
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
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.batch_ban_rooms(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn batch_delete_rooms(
        &self,
        request: Request<BatchDeleteRoomsRequest>,
    ) -> Result<Response<BatchDeleteRoomsResponse>, Status> {
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

    async fn list_user_registration_reviews(
        &self,
        request: Request<ListUserRegistrationReviewsRequest>,
    ) -> Result<Response<ListUserRegistrationReviewsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.list_user_registration_reviews(req, &validated.user_id)
                .await
        })
        .await
    }

    async fn approve_user_registration_review(
        &self,
        request: Request<ApproveUserRegistrationReviewRequest>,
    ) -> Result<Response<ApproveUserRegistrationReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.approve_user_registration_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn reject_user_registration_review(
        &self,
        request: Request<RejectUserRegistrationReviewRequest>,
    ) -> Result<Response<RejectUserRegistrationReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.reject_user_registration_review(req, &validated.user_id)
                .await
        })
        .await
    }

    async fn list_room_creation_reviews(
        &self,
        request: Request<ListRoomCreationReviewsRequest>,
    ) -> Result<Response<ListRoomCreationReviewsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.list_room_creation_reviews(req, &validated.user_id)
                .await
        })
        .await
    }

    async fn approve_room_creation_review(
        &self,
        request: Request<ApproveRoomCreationReviewRequest>,
    ) -> Result<Response<ApproveRoomCreationReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.approve_room_creation_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn reject_room_creation_review(
        &self,
        request: Request<RejectRoomCreationReviewRequest>,
    ) -> Result<Response<RejectRoomCreationReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.reject_room_creation_review(req, &validated.user_id)
                .await
        })
        .await
    }

    async fn list_room_join_reviews(
        &self,
        request: Request<ListRoomJoinReviewsRequest>,
    ) -> Result<Response<ListRoomJoinReviewsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.list_room_join_reviews(req, &validated.user_id).await
        })
        .await
    }

    async fn approve_room_join_review(
        &self,
        request: Request<ApproveRoomJoinReviewRequest>,
    ) -> Result<Response<ApproveRoomJoinReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.approve_room_join_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn reject_room_join_review(
        &self,
        request: Request<RejectRoomJoinReviewRequest>,
    ) -> Result<Response<RejectRoomJoinReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.reject_room_join_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn list_ban_records(
        &self,
        request: Request<ListBanRecordsRequest>,
    ) -> Result<Response<ListBanRecordsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.list_ban_records(req, &validated.user_id).await
        })
        .await
    }

    async fn list_content_reports(
        &self,
        request: Request<ListContentReportsRequest>,
    ) -> Result<Response<ListContentReportsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.list_content_reports(req, &validated.user_id).await
        })
        .await
    }

    async fn get_content_report(
        &self,
        request: Request<GetContentReportRequest>,
    ) -> Result<Response<GetContentReportResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            api.get_content_report(req, &validated.user_id).await
        })
        .await
    }

    async fn update_content_report_status(
        &self,
        request: Request<UpdateContentReportStatusRequest>,
    ) -> Result<Response<UpdateContentReportStatusResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_content_report_status(req, &validated.user_id, &ctx)
                .await
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
        assert_eq!(
            status
                .metadata()
                .get(crate::grpc_support::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("9000")
        );
    }

    #[test]
    fn test_api_err_metadata_includes_application_error_code() {
        let err = crate::impls::ApiError::NotFound("user not found".to_string());
        let status = map_api_error(err);

        assert_eq!(
            status
                .metadata()
                .get(crate::grpc_support::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("2000")
        );
    }

    #[test]
    fn test_rate_limited_error_includes_retry_after_metadata() {
        let err = crate::impls::ApiError::RateLimitedWithRetry {
            message: "Rate limit exceeded. Try again in 7s".to_string(),
            retry_after_seconds: 7,
        };
        let status = map_api_error(err);

        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            status
                .metadata()
                .get(crate::grpc_support::ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("2002")
        );
        assert_eq!(
            status
                .metadata()
                .get(crate::grpc_support::RETRY_AFTER_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("7")
        );
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
}
