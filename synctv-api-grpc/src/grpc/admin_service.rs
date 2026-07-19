use futures::future::BoxFuture;
use futures::FutureExt;
use std::{future::Future, sync::Arc};

use tonic::{Request, Response, Status};

use synctv_api_common::impls::admin::RequestContext;

// Use synctv_proto for all gRPC types to avoid duplication
use synctv_proto::admin::admin_service_server::AdminService;
use synctv_proto::admin::{
    AddAdminRequest, AddMemberRequest, AdminUser, ApproveRoomCreationReviewRequest,
    ApproveRoomCreationReviewResponse, ApproveRoomJoinReviewRequest, ApproveRoomJoinReviewResponse,
    ApproveUserRegistrationReviewRequest, ApproveUserRegistrationReviewResponse, BanRoomRequest,
    BanUserRequest, BatchBanRoomsRequest, BatchBanRoomsResponse, BatchBanUsersRequest,
    BatchBanUsersResponse, BatchDeleteRoomsRequest, BatchDeleteRoomsResponse,
    BatchDeleteUsersRequest, BatchDeleteUsersResponse, ContentReport, CreateUserRequest,
    DeleteRoomCategoryRequest, DeleteRoomCategoryResponse, DeleteRoomLabelRequest,
    DeleteRoomLabelResponse, DeleteRoomRequest, DeleteRoomResponse, DeleteUserRequest,
    DeleteUserResponse, EvictExpiredSliceCacheRequest, EvictExpiredSliceCacheResponse,
    GetContentReportRequest, GetRoomMembersRequest, GetRoomMembersResponse, GetRoomRequest,
    GetRoomSettingsRequest, GetRoomSettingsResponse, GetServiceStateRequest,
    GetServiceStateResponse, GetSettingsRequest, GetSliceCacheStatsRequest,
    GetSliceCacheStatsResponse, GetUserPreferencesRequest, GetUserPreferencesResponse,
    GetUserRequest, GetUserRoomsRequest, GetUserRoomsResponse, KickMemberRequest,
    KickMemberResponse, KickStreamRequest, KickStreamResponse, ListActiveStreamsRequest,
    ListActiveStreamsResponse, ListAdminsRequest, ListAdminsResponse, ListBanRecordsRequest,
    ListBanRecordsResponse, ListContentReportsRequest, ListContentReportsResponse,
    ListRoomCategoriesRequest, ListRoomCategoriesResponse, ListRoomCreationReviewsRequest,
    ListRoomCreationReviewsResponse, ListRoomJoinReviewsRequest, ListRoomJoinReviewsResponse,
    ListRoomLabelsRequest, ListRoomLabelsResponse, ListRoomsRequest, ListRoomsResponse,
    ListUserRegistrationReviewsRequest, ListUserRegistrationReviewsResponse, ListUsersRequest,
    ListUsersResponse, PurgeSliceCacheRequest, PurgeSliceCacheResponse,
    RejectRoomCreationReviewRequest, RejectRoomJoinReviewRequest,
    RejectUserRegistrationReviewRequest, RemoveAdminRequest, RemoveAdminResponse,
    ResetRoomSettingsRequest, Room, RoomCreationReview, RoomJoinReview, SendTestEmailRequest,
    SendTestEmailResponse, SetUserPasswordRequest, SetUserPasswordResponse, UnbanRoomRequest,
    UnbanUserRequest, UpdateContentReportStatusRequest, UpdateContentReportStatusResponse,
    UpdateMemberDisplayTagRequest, UpdateMemberPermissionsRequest, UpdateMemberRemarkNameRequest,
    UpdateRoomPasswordRequest, UpdateRoomPasswordResponse, UpdateRoomSettingsRequest,
    UpdateRoomTaxonomyRequest, UpdateSettingsRequest, UpdateUserPreferencesRequest,
    UpdateUserPreferencesResponse, UpdateUserRoleRequest, UpdateUserUsernameRequest,
    UpsertRoomCategoryRequest, UpsertRoomLabelRequest, UserRegistrationReview,
};
use synctv_proto::client::{RoomCategory, RoomLabel};
use synctv_proto::common::RoomMember;

use synctv_api_common::impls::AdminApiImpl;

use super::map_api_error;

/// Extract IP address and User-Agent from a gRPC request for audit logging.
fn grpc_request_context<T: std::fmt::Debug>(
    request: &Request<T>,
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
) -> Result<RequestContext, Status> {
    let ip_address = super::extract_client_ip(request, runtime_settings)?.map(|ip| ip.to_string());
    let user_agent = super::request_user_agent(request)?;
    Ok(RequestContext {
        ip_address,
        user_agent,
    })
}

fn map_slice_cache_error(
    error: &synctv_api_common::status::SliceCacheManagementError,
) -> synctv_api_common::impls::ApiError {
    match error {
        synctv_api_common::status::SliceCacheManagementError::InvalidSelection => {
            synctv_api_common::impls::ApiError::InvalidInput(error.to_string())
        }
        synctv_api_common::status::SliceCacheManagementError::ClusterUnavailable(_)
        | synctv_api_common::status::SliceCacheManagementError::Cluster(_)
        | synctv_api_common::status::SliceCacheManagementError::MissingClusterSecret
        | synctv_api_common::status::SliceCacheManagementError::InvalidClusterSecret
        | synctv_api_common::status::SliceCacheManagementError::RemoteRequest { .. } => {
            synctv_api_common::impls::ApiError::ServiceUnavailable(error.to_string())
        }
    }
}

/// `AdminService` gRPC implementation.
///
/// Thin wrapper that delegates all business logic to [`AdminApiImpl`],
/// matching how `ClientServiceImpl` delegates to `ClientApiImpl`.
#[derive(Clone)]
pub struct AdminServiceImpl {
    admin_api: Arc<AdminApiImpl>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    slice_cache_runtime: Arc<synctv_api_common::status::SliceCacheManagementRuntime>,
}

impl AdminServiceImpl {
    #[must_use]
    pub const fn new(
        admin_api: Arc<AdminApiImpl>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
        slice_cache_runtime: Arc<synctv_api_common::status::SliceCacheManagementRuntime>,
    ) -> Self {
        Self {
            admin_api,
            runtime_settings,
            slice_cache_runtime,
        }
    }

    fn request_metadata(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<synctv_api_common::impls::RequestMetadata, Status> {
        super::request_metadata(
            request,
            &self.runtime_settings,
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
                synctv_api_common::impls::admin::ValidatedAdmin,
                RequestContext,
                TReq,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<TResp, synctv_api_common::impls::ApiError>> + Send + 'static,
    {
        async move {
            let metadata = self.request_metadata(&request)?;
            let ctx = grpc_request_context(&request, &self.runtime_settings)?;
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
                synctv_api_common::impls::admin::ValidatedAdmin,
                RequestContext,
                TReq,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<TResp, synctv_api_common::impls::ApiError>> + Send + 'static,
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
                synctv_api_common::impls::admin::ValidatedAdmin,
                RequestContext,
                TReq,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = Result<TResp, synctv_api_common::impls::ApiError>> + Send + 'static,
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
    ) -> Result<Response<synctv_proto::admin::RuntimeSettings>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, _| async move {
            api.get_settings(&validated.user_id, &ctx).await
        })
        .await
    }

    async fn update_settings(
        &self,
        request: Request<UpdateSettingsRequest>,
    ) -> Result<Response<synctv_proto::admin::RuntimeSettings>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
                synctv_api_common::impls::validate_proto_request(&req)?;
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
    ) -> Result<Response<AdminUser>, Status> {
        let require_root = request.get_ref().role == synctv_proto::common::UserRole::Root as i32;
        self.execute_scoped_admin_rpc(
            request,
            require_root,
            move |api, validated, ctx, req| async move {
                synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.delete_user(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn list_users(
        &self,
        request: Request<ListUsersRequest>,
    ) -> Result<Response<ListUsersResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_users(req).await
        })
        .await
    }

    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<Response<AdminUser>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_user(req).await
        })
        .await
    }

    async fn get_user_preferences(
        &self,
        request: Request<GetUserPreferencesRequest>,
    ) -> Result<Response<GetUserPreferencesResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_user_preferences(req).await
        })
        .await
    }

    async fn update_user_preferences(
        &self,
        request: Request<UpdateUserPreferencesRequest>,
    ) -> Result<Response<UpdateUserPreferencesResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.set_user_password(req, validated.user_id, validated.role, &ctx)
                .await
        })
        .await
    }

    async fn update_user_username(
        &self,
        request: Request<UpdateUserUsernameRequest>,
    ) -> Result<Response<AdminUser>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_user_username(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn update_user_role(
        &self,
        request: Request<UpdateUserRoleRequest>,
    ) -> Result<Response<AdminUser>, Status> {
        let require_root = request.get_ref().role == synctv_proto::common::UserRole::Root as i32;
        self.execute_scoped_admin_rpc(
            request,
            require_root,
            move |api, validated, ctx, req| async move {
                synctv_api_common::impls::validate_proto_request(&req)?;
                api.update_user_role(req, &validated.user_id, validated.role, &ctx)
                    .await
            },
        )
        .await
    }

    async fn ban_user(
        &self,
        request: Request<BanUserRequest>,
    ) -> Result<Response<AdminUser>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.ban_user(req, &validated.user_id, validated.role, &ctx)
                .await
        })
        .await
    }

    async fn unban_user(
        &self,
        request: Request<UnbanUserRequest>,
    ) -> Result<Response<AdminUser>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.unban_user(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn get_user_rooms(
        &self,
        request: Request<GetUserRoomsRequest>,
    ) -> Result<Response<GetUserRoomsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.batch_ban_rooms(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn batch_delete_rooms(
        &self,
        request: Request<BatchDeleteRoomsRequest>,
    ) -> Result<Response<BatchDeleteRoomsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_rooms(req).await
        })
        .await
    }

    async fn get_room(&self, request: Request<GetRoomRequest>) -> Result<Response<Room>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_room(req).await
        })
        .await
    }

    async fn list_room_categories(
        &self,
        request: Request<ListRoomCategoriesRequest>,
    ) -> Result<Response<ListRoomCategoriesResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_categories(req).await
        })
        .await
    }

    async fn upsert_room_category(
        &self,
        request: Request<UpsertRoomCategoryRequest>,
    ) -> Result<Response<RoomCategory>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.upsert_room_category(req).await
        })
        .await
    }

    async fn delete_room_category(
        &self,
        request: Request<DeleteRoomCategoryRequest>,
    ) -> Result<Response<DeleteRoomCategoryResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.delete_room_category(req).await
        })
        .await
    }

    async fn list_room_labels(
        &self,
        request: Request<ListRoomLabelsRequest>,
    ) -> Result<Response<ListRoomLabelsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_labels(req).await
        })
        .await
    }

    async fn upsert_room_label(
        &self,
        request: Request<UpsertRoomLabelRequest>,
    ) -> Result<Response<RoomLabel>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.upsert_room_label(req).await
        })
        .await
    }

    async fn delete_room_label(
        &self,
        request: Request<DeleteRoomLabelRequest>,
    ) -> Result<Response<DeleteRoomLabelResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.delete_room_label(req).await
        })
        .await
    }

    async fn update_room_taxonomy(
        &self,
        request: Request<UpdateRoomTaxonomyRequest>,
    ) -> Result<Response<Room>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_room_taxonomy(req, &validated.user_id).await
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

    async fn ban_room(&self, request: Request<BanRoomRequest>) -> Result<Response<Room>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.ban_room(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn unban_room(
        &self,
        request: Request<UnbanRoomRequest>,
    ) -> Result<Response<Room>, Status> {
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_room_members(req).await
        })
        .await
    }

    async fn add_member(
        &self,
        request: Request<AddMemberRequest>,
    ) -> Result<Response<RoomMember>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.add_member(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<RoomMember>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_member_permissions(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn update_member_remark_name(
        &self,
        request: Request<UpdateMemberRemarkNameRequest>,
    ) -> Result<Response<RoomMember>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_member_remark_name(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn update_member_display_tag(
        &self,
        request: Request<UpdateMemberDisplayTagRequest>,
    ) -> Result<Response<RoomMember>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            api.update_member_display_tag(req, &validated.user_id, &ctx)
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
    ) -> Result<Response<AdminUser>, Status> {
        self.execute_root_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.add_admin(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn remove_admin(
        &self,
        request: Request<RemoveAdminRequest>,
    ) -> Result<Response<RemoveAdminResponse>, Status> {
        self.execute_root_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.remove_admin(req, &validated.user_id, &ctx).await
        })
        .await
    }

    async fn list_admins(
        &self,
        request: Request<ListAdminsRequest>,
    ) -> Result<Response<ListAdminsResponse>, Status> {
        self.execute_root_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_admins(req).await
        })
        .await
    }

    // Service State

    async fn get_service_state(
        &self,
        request: Request<GetServiceStateRequest>,
    ) -> Result<Response<GetServiceStateResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, _| async move {
            api.get_service_state().await
        })
        .await
    }

    // Slice Cache Management

    async fn get_slice_cache_stats(
        &self,
        request: Request<GetSliceCacheStatsRequest>,
    ) -> Result<Response<GetSliceCacheStatsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        synctv_api_common::impls::validate_proto_request(&req).map_err(map_api_error)?;
        let runtime = self.slice_cache_runtime.clone();
        let executor = self.admin_api.clone();
        let response = executor
            .execute_admin_endpoint(&metadata, move |_| async move {
                runtime
                    .get_stats(synctv_api_common::status::SliceCacheSelection {
                        node_id: (!req.node_id.trim().is_empty()).then_some(req.node_id),
                        all_nodes: req.all_nodes,
                    })
                    .await
                    .map(synctv_api_common::impls::admin::slice_cache_stats_to_admin_proto)
                    .map_err(|error| map_slice_cache_error(&error))
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn purge_slice_cache(
        &self,
        request: Request<PurgeSliceCacheRequest>,
    ) -> Result<Response<PurgeSliceCacheResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        synctv_api_common::impls::validate_proto_request(&req).map_err(map_api_error)?;
        let runtime = self.slice_cache_runtime.clone();
        let executor = self.admin_api.clone();
        let response = executor
            .execute_admin_endpoint(&metadata, move |_| async move {
                runtime
                    .purge(synctv_api_common::status::SliceCacheSelection {
                        node_id: (!req.node_id.trim().is_empty()).then_some(req.node_id),
                        all_nodes: req.all_nodes,
                    })
                    .await
                    .map(synctv_api_common::impls::admin::slice_cache_purge_to_admin_proto)
                    .map_err(|error| map_slice_cache_error(&error))
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn evict_expired_slice_cache(
        &self,
        request: Request<EvictExpiredSliceCacheRequest>,
    ) -> Result<Response<EvictExpiredSliceCacheResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        synctv_api_common::impls::validate_proto_request(&req).map_err(map_api_error)?;
        let runtime = self.slice_cache_runtime.clone();
        let executor = self.admin_api.clone();
        let response = executor
            .execute_admin_endpoint(&metadata, move |_| async move {
                runtime
                    .evict_expired(synctv_api_common::status::SliceCacheSelection {
                        node_id: (!req.node_id.trim().is_empty()).then_some(req.node_id),
                        all_nodes: req.all_nodes,
                    })
                    .await
                    .map(synctv_api_common::impls::admin::slice_cache_evict_expired_to_admin_proto)
                    .map_err(|error| map_slice_cache_error(&error))
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    // Room Settings Management

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        self.execute_admin_rpc(request, move |api, _, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_room_settings(req).await
        })
        .await
    }

    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<synctv_proto::admin::Room>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_room_settings(req, &validated.user_id).await
        })
        .await
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<Room>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_active_streams(req).await
        })
        .await
    }

    async fn kick_stream(
        &self,
        request: Request<KickStreamRequest>,
    ) -> Result<Response<KickStreamResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.approve_user_registration_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn reject_user_registration_review(
        &self,
        request: Request<RejectUserRegistrationReviewRequest>,
    ) -> Result<Response<UserRegistrationReview>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.approve_room_creation_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn reject_room_creation_review(
        &self,
        request: Request<RejectRoomCreationReviewRequest>,
    ) -> Result<Response<RoomCreationReview>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_join_reviews(req, &validated.user_id).await
        })
        .await
    }

    async fn approve_room_join_review(
        &self,
        request: Request<ApproveRoomJoinReviewRequest>,
    ) -> Result<Response<ApproveRoomJoinReviewResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.approve_room_join_review(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }

    async fn reject_room_join_review(
        &self,
        request: Request<RejectRoomJoinReviewRequest>,
    ) -> Result<Response<RoomJoinReview>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
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
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_content_reports(req, &validated.user_id).await
        })
        .await
    }

    async fn get_content_report(
        &self,
        request: Request<GetContentReportRequest>,
    ) -> Result<Response<ContentReport>, Status> {
        self.execute_admin_rpc(request, move |api, validated, _, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_content_report(req, &validated.user_id).await
        })
        .await
    }

    async fn update_content_report_status(
        &self,
        request: Request<UpdateContentReportStatusRequest>,
    ) -> Result<Response<UpdateContentReportStatusResponse>, Status> {
        self.execute_admin_rpc(request, move |api, validated, ctx, req| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_content_report_status(req, &validated.user_id, &ctx)
                .await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic_types::StatusExt;

    fn detail_error_code(status: &Status) -> Option<String> {
        status
            .get_error_details()
            .error_info()
            .and_then(|detail| detail.metadata.get("errorCode").cloned())
    }

    #[test]
    fn test_api_err_internal_hides_details() {
        let err = synctv_api_common::impls::ApiError::Internal(
            "database connection failed with password=secret123".to_string(),
        );
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Internal);
        // Internal errors should NOT leak implementation details
        assert_eq!(status.message(), "Internal error");
        assert!(!status.message().contains("password"));
        assert!(!status.message().contains("database"));
        assert_eq!(detail_error_code(&status).as_deref(), Some("9000"));
    }

    #[test]
    fn test_api_err_metadata_includes_application_error_code() {
        let err = synctv_api_common::impls::ApiError::NotFound("user not found".to_string());
        let status = map_api_error(err);

        assert_eq!(detail_error_code(&status).as_deref(), Some("2000"));
    }

    #[test]
    fn test_rate_limited_error_includes_retry_after_metadata() {
        let err = synctv_api_common::impls::ApiError::RateLimitedWithRetry {
            message: "Rate limit exceeded. Try again in 7s".to_string(),
            retry_after_seconds: 7,
        };
        let status = map_api_error(err);

        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(detail_error_code(&status).as_deref(), Some("2002"));
        assert_eq!(
            status
                .get_error_details()
                .retry_info()
                .and_then(|detail| detail.retry_delay)
                .map(|duration| duration.as_secs()),
            Some(7)
        );
    }

    #[test]
    fn test_api_err_all_variants_mapped() {
        // Verify every ApiError variant maps to a distinct gRPC code
        let variants: Vec<(synctv_api_common::impls::ApiError, tonic::Code)> = vec![
            (
                synctv_api_common::impls::ApiError::NotFound("x".into()),
                tonic::Code::NotFound,
            ),
            (
                synctv_api_common::impls::ApiError::Authentication("x".into()),
                tonic::Code::Unauthenticated,
            ),
            (
                synctv_api_common::impls::ApiError::Authorization("x".into()),
                tonic::Code::PermissionDenied,
            ),
            (
                synctv_api_common::impls::ApiError::AlreadyExists("x".into()),
                tonic::Code::AlreadyExists,
            ),
            (
                synctv_api_common::impls::ApiError::InvalidInput("x".into()),
                tonic::Code::InvalidArgument,
            ),
            (
                synctv_api_common::impls::ApiError::Internal("x".into()),
                tonic::Code::Internal,
            ),
        ];
        for (err, expected_code) in variants {
            let status = map_api_error(err);
            assert_eq!(status.code(), expected_code);
        }
    }
}
