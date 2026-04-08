use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tonic::{Request, Response, Status};

use crate::lifecycle::{LifecycleEvent, ManagementLifecycleController, ShutdownMode};
use crate::proto::{
    management_service_server::ManagementService, AddAdminRequest, AddDirectUrlMediaRequest,
    AddMediaRequest, AddProviderInstanceRequest, ApproveRoomRequest, ApproveUserRequest,
    BanMemberRequest, BanRoomRequest, BanUserRequest, BatchBanRoomsRequest, BatchBanUsersRequest,
    BatchDeleteRoomsRequest, BatchDeleteUsersRequest, CreatePlaylistRequest,
    CreatePublishKeyRequest, CreateRoomRequest, CreateUserRequest, DeleteMediaRequest,
    DeletePlaylistRequest, DeleteProviderInstanceRequest, DeleteRoomRequest, DeleteUserRequest,
    DisableProviderInstanceRequest, EditMediaRequest, EnableProviderInstanceRequest,
    GetPlaybackRequest, GetPlaylistRequest, GetRoomMembersRequest, GetRoomRequest,
    GetRoomSettingsRequest, GetSettingsGroupRequest, GetSettingsRequest, GetStreamInfoRequest,
    GetSystemStatsRequest, GetUserByUsernameRequest, GetUserRequest, GetUserRoomsRequest,
    KickMemberRequest, KickStreamRequest, ListActiveStreamsRequest, ListAdminsRequest,
    ListMediaRequest, ListPlaylistsRequest, ListProviderInstancesRequest, ListRoomStreamsRequest,
    ListRoomsRequest, ListUsersRequest, MoveMediaRequest, MovePlaylistRequest,
    ReconnectProviderInstanceRequest, RemoveAdminRequest, ResetRoomSettingsRequest, RoomStatus,
    SendTestEmailRequest, ShutdownMode as ProtoShutdownMode, StartPlaybackRequest,
    StopPlaybackRequest, StopServerEvent, StopServerRequest, TransferRoomOwnershipRequest,
    UnbanMemberRequest, UnbanRoomRequest, UnbanUserRequest, UpdateMemberPermissionsRequest,
    UpdatePlaylistRequest, UpdateProviderInstanceRequest, UpdateRoomPasswordRequest,
    UpdateRoomSettingsRequest, UpdateSettingsRequest, UpdateUserPasswordRequest,
    UpdateUserRoleRequest, UpdateUserUsernameRequest, UserRole, UserStatus,
};
use synctv_api::impls::admin::{RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID};
use synctv_api::impls::{AdminApiImpl, ApiError, ClientApiImpl, ErrorKind};
use synctv_core::models::{UserId, UserRole as CoreUserRole, UserStatus as CoreUserStatus};
use synctv_core::service::UserService;
use synctv_proto::{admin as admin_proto, client as client_proto, common as common_proto};

struct ValidatedManagementUser {
    user_id: UserId,
    role: CoreUserRole,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ManagementAccessController {
    required_bearer_token: Option<String>,
}

impl ManagementAccessController {
    pub(crate) fn new(auth_token: &str) -> Self {
        let trimmed = auth_token.trim();
        let required_bearer_token = (!trimmed.is_empty()).then(|| trimmed.to_string());
        Self {
            required_bearer_token,
        }
    }

    pub(crate) fn authorize<T: std::fmt::Debug>(
        &self,
        request: &Request<T>,
    ) -> Result<(), Status> {
        let Some(expected_token) = &self.required_bearer_token else {
            return Ok(());
        };

        let header_value = request
            .metadata()
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("Management authentication required"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid management authorization header"))?;

        let provided_token =
            synctv_core::service::auth::JwtValidator::extract_bearer_token(header_value)
                .map_err(|_| Status::unauthenticated("Invalid management authorization header"))?;

        if constant_time_eq(provided_token.as_bytes(), expected_token.as_bytes()) {
            Ok(())
        } else {
            Err(Status::unauthenticated(
                "Invalid management authorization header",
            ))
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let left_hash = Sha256::digest(left);
    let right_hash = Sha256::digest(right);
    left_hash.ct_eq(&right_hash).into()
}

#[derive(Clone)]
pub struct ManagementServiceImpl {
    user_service: Arc<UserService>,
    admin_api: Arc<AdminApiImpl>,
    client_api: Arc<ClientApiImpl>,
    lifecycle_controller: Arc<ManagementLifecycleController>,
    access_controller: ManagementAccessController,
}

impl ManagementServiceImpl {
    #[must_use]
    pub fn new(
        user_service: Arc<UserService>,
        admin_api: Arc<AdminApiImpl>,
        client_api: Arc<ClientApiImpl>,
        lifecycle_controller: Arc<ManagementLifecycleController>,
        management_auth_token: String,
    ) -> Self {
        Self {
            user_service,
            admin_api,
            client_api,
            lifecycle_controller,
            access_controller: ManagementAccessController::new(&management_auth_token),
        }
    }

    async fn management_actor(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<ValidatedManagementUser, Status> {
        self.access_controller.authorize(request)?;
        Ok(ValidatedManagementUser {
            user_id: UserId::from_string(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string()),
            role: CoreUserRole::Root,
        })
    }

    async fn check_admin_get_validated(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<ValidatedManagementUser, Status> {
        self.management_actor(request).await
    }

    async fn check_root(&self, request: &Request<impl std::fmt::Debug>) -> Result<(), Status> {
        self.management_actor(request).await?;
        Ok(())
    }

    async fn resolve_client_actor_user_id(&self, actor_user_id: &str) -> Result<String, Status> {
        let actor_user_id = actor_user_id.trim();
        if actor_user_id.is_empty() {
            return Err(Status::invalid_argument(
                "actor_user_id is required for this command",
            ));
        }

        let user = self
            .user_service
            .get_user(&UserId::from_string(actor_user_id.to_string()))
            .await
            .map_err(map_management_user_lookup_error)?;
        validate_client_actor_user(&user)?;
        Ok(user.id.to_string())
    }

    fn grpc_request_context<T: std::fmt::Debug>(&self, request: &Request<T>) -> RequestContext {
        let ip_address = request
            .extensions()
            .get::<tonic::transport::server::TcpConnectInfo>()
            .and_then(tonic::transport::server::TcpConnectInfo::remote_addr)
            .map(|addr| addr.ip().to_string());
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

    fn proto_response<T>(value: T) -> Result<Response<T>, Status> {
        Ok(Response::new(value))
    }
}

#[tonic::async_trait]
impl ManagementService for ManagementServiceImpl {
    type StopServerStream =
        Pin<Box<dyn Stream<Item = Result<StopServerEvent, Status>> + Send + 'static>>;

    async fn list_users(
        &self,
        request: Request<ListUsersRequest>,
    ) -> Result<Response<admin_proto::ListUsersResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_users(admin_proto::ListUsersRequest {
                page: req.page,
                page_size: req.page_size,
                status: map_user_status(req.status),
                role: map_user_role(req.role),
                search: req.search,
                sort_by: match crate::proto::UserListSortBy::try_from(req.sort_by) {
                    Ok(crate::proto::UserListSortBy::Username) => {
                        admin_proto::UserListSortBy::Username as i32
                    }
                    Ok(crate::proto::UserListSortBy::Email) => {
                        admin_proto::UserListSortBy::Email as i32
                    }
                    Ok(crate::proto::UserListSortBy::Status) => {
                        admin_proto::UserListSortBy::Status as i32
                    }
                    Ok(crate::proto::UserListSortBy::Role) => {
                        admin_proto::UserListSortBy::Role as i32
                    }
                    Ok(crate::proto::UserListSortBy::UpdatedAt) => {
                        admin_proto::UserListSortBy::UpdatedAt as i32
                    }
                    _ => admin_proto::UserListSortBy::CreatedAt as i32,
                },
                sort_direction: match crate::proto::SortDirection::try_from(req.sort_direction) {
                    Ok(crate::proto::SortDirection::Asc) => admin_proto::SortDirection::Asc as i32,
                    _ => admin_proto::SortDirection::Desc as i32,
                },
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<Response<admin_proto::GetUserResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_user(admin_proto::GetUserRequest {
                user_id: req.user_id,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_user_by_username(
        &self,
        request: Request<GetUserByUsernameRequest>,
    ) -> Result<Response<admin_proto::GetUserResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let username = req.username.trim();
        if username.is_empty() {
            return Err(Status::invalid_argument("username is required"));
        }

        let user = self
            .user_service
            .get_user_by_username(username)
            .await
            .map_err(map_management_user_lookup_error)?;
        let response = self
            .admin_api
            .get_user(admin_proto::GetUserRequest {
                user_id: user.id.to_string(),
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn add_admin(
        &self,
        request: Request<AddAdminRequest>,
    ) -> Result<Response<admin_proto::AddAdminResponse>, Status> {
        self.check_root(&request).await?;
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .add_admin(
                admin_proto::AddAdminRequest {
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn remove_admin(
        &self,
        request: Request<RemoveAdminRequest>,
    ) -> Result<Response<admin_proto::RemoveAdminResponse>, Status> {
        self.check_root(&request).await?;
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .remove_admin(
                admin_proto::RemoveAdminRequest {
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_admins(
        &self,
        request: Request<ListAdminsRequest>,
    ) -> Result<Response<admin_proto::ListAdminsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_admins(admin_proto::ListAdminsRequest {
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                sort_by: req.sort_by,
                sort_direction: req.sort_direction,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn create_user(
        &self,
        request: Request<CreateUserRequest>,
    ) -> Result<Response<admin_proto::CreateUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .create_user(
                admin_proto::CreateUserRequest {
                    username: req.username,
                    password: req.password,
                    email: req.email,
                    role: map_user_role(req.role),
                    status: map_user_status(req.status),
                },
                validated.role,
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn delete_user(
        &self,
        request: Request<DeleteUserRequest>,
    ) -> Result<Response<admin_proto::DeleteUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_user(
                admin_proto::DeleteUserRequest {
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn ban_user(
        &self,
        request: Request<BanUserRequest>,
    ) -> Result<Response<admin_proto::BanUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .ban_user(
                admin_proto::BanUserRequest {
                    user_id: req.user_id,
                    reason: req.reason,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn unban_user(
        &self,
        request: Request<UnbanUserRequest>,
    ) -> Result<Response<admin_proto::UnbanUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .unban_user(
                admin_proto::UnbanUserRequest {
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn approve_user(
        &self,
        request: Request<ApproveUserRequest>,
    ) -> Result<Response<admin_proto::ApproveUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .approve_user(
                admin_proto::ApproveUserRequest {
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_user_role(
        &self,
        request: Request<UpdateUserRoleRequest>,
    ) -> Result<Response<admin_proto::UpdateUserRoleResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_user_role(
                admin_proto::UpdateUserRoleRequest {
                    user_id: req.user_id,
                    role: map_user_role(req.role),
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_user_password(
        &self,
        request: Request<UpdateUserPasswordRequest>,
    ) -> Result<Response<admin_proto::UpdateUserPasswordResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_user_password(
                admin_proto::UpdateUserPasswordRequest {
                    user_id: req.user_id,
                    new_password: req.new_password,
                    reason: req.reason,
                },
                validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_user_username(
        &self,
        request: Request<UpdateUserUsernameRequest>,
    ) -> Result<Response<admin_proto::UpdateUserUsernameResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_user_username(
                admin_proto::UpdateUserUsernameRequest {
                    user_id: req.user_id,
                    new_username: req.new_username,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_user_rooms(
        &self,
        request: Request<GetUserRoomsRequest>,
    ) -> Result<Response<admin_proto::GetUserRoomsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_user_rooms(admin_proto::GetUserRoomsRequest {
                user_id: req.user_id,
                page: req.page,
                page_size: req.page_size,
                status: map_room_status(req.status),
                search: req.search,
                is_banned: req.is_banned,
                sort_by: match crate::proto::RoomListSortBy::try_from(req.sort_by) {
                    Ok(crate::proto::RoomListSortBy::Name) => {
                        admin_proto::RoomListSortBy::Name as i32
                    }
                    Ok(crate::proto::RoomListSortBy::UpdatedAt) => {
                        admin_proto::RoomListSortBy::UpdatedAt as i32
                    }
                    Ok(crate::proto::RoomListSortBy::LastActivityAt) => {
                        admin_proto::RoomListSortBy::LastActivityAt as i32
                    }
                    _ => admin_proto::RoomListSortBy::CreatedAt as i32,
                },
                sort_direction: match crate::proto::SortDirection::try_from(req.sort_direction) {
                    Ok(crate::proto::SortDirection::Asc) => admin_proto::SortDirection::Asc as i32,
                    _ => admin_proto::SortDirection::Desc as i32,
                },
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn batch_ban_users(
        &self,
        request: Request<BatchBanUsersRequest>,
    ) -> Result<Response<admin_proto::BatchBanUsersResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .batch_ban_users(
                admin_proto::BatchBanUsersRequest {
                    user_ids: req.user_ids,
                    reason: req.reason,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn batch_delete_users(
        &self,
        request: Request<BatchDeleteUsersRequest>,
    ) -> Result<Response<admin_proto::BatchDeleteUsersResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .batch_delete_users(
                admin_proto::BatchDeleteUsersRequest {
                    user_ids: req.user_ids,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn create_room(
        &self,
        request: Request<CreateRoomRequest>,
    ) -> Result<Response<client_proto::CreateRoomResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_client_actor_user_id(&req.actor_user_id)
            .await?;
        let response = self
            .client_api
            .create_room(
                &actor_user_id,
                client_proto::CreateRoomRequest {
                    name: req.name,
                    password: req.password,
                    settings: req.settings_json,
                    description: req.description,
                },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<admin_proto::ListRoomsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_rooms(admin_proto::ListRoomsRequest {
                page: req.page,
                page_size: req.page_size,
                status: map_room_status(req.status),
                search: req.search,
                creator_id: req.creator_id,
                is_banned: req.is_banned,
                sort_by: match crate::proto::RoomListSortBy::try_from(req.sort_by) {
                    Ok(crate::proto::RoomListSortBy::Name) => {
                        admin_proto::RoomListSortBy::Name as i32
                    }
                    Ok(crate::proto::RoomListSortBy::UpdatedAt) => {
                        admin_proto::RoomListSortBy::UpdatedAt as i32
                    }
                    Ok(crate::proto::RoomListSortBy::LastActivityAt) => {
                        admin_proto::RoomListSortBy::LastActivityAt as i32
                    }
                    _ => admin_proto::RoomListSortBy::CreatedAt as i32,
                },
                sort_direction: match crate::proto::SortDirection::try_from(req.sort_direction) {
                    Ok(crate::proto::SortDirection::Asc) => admin_proto::SortDirection::Asc as i32,
                    _ => admin_proto::SortDirection::Desc as i32,
                },
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<admin_proto::GetRoomResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room(admin_proto::GetRoomRequest {
                room_id: req.room_id,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<admin_proto::GetRoomMembersResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room_members(admin_proto::GetRoomMembersRequest {
                room_id: req.room_id,
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                role: req.role,
                status: req.status,
                sort_by: match crate::proto::RoomMemberListSortBy::try_from(req.sort_by) {
                    Ok(crate::proto::RoomMemberListSortBy::Username) => {
                        admin_proto::RoomMemberListSortBy::Username as i32
                    }
                    Ok(crate::proto::RoomMemberListSortBy::Role) => {
                        admin_proto::RoomMemberListSortBy::Role as i32
                    }
                    Ok(crate::proto::RoomMemberListSortBy::Status) => {
                        admin_proto::RoomMemberListSortBy::Status as i32
                    }
                    _ => admin_proto::RoomMemberListSortBy::JoinedAt as i32,
                },
                sort_direction: match crate::proto::SortDirection::try_from(req.sort_direction) {
                    Ok(crate::proto::SortDirection::Desc) => {
                        admin_proto::SortDirection::Desc as i32
                    }
                    _ => admin_proto::SortDirection::Asc as i32,
                },
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<client_proto::UpdateMemberPermissionsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_member_permissions(
                admin_proto::UpdateMemberPermissionsRequest {
                    room_id: req.room_id,
                    user_id: req.user_id,
                    role: req.role,
                    added_permissions: req.added_permissions,
                    removed_permissions: req.removed_permissions,
                    admin_added_permissions: req.admin_added_permissions,
                    admin_removed_permissions: req.admin_removed_permissions,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(client_proto::UpdateMemberPermissionsResponse {
            member: response.member,
        })
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<client_proto::KickMemberResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .kick_member(
                admin_proto::KickMemberRequest {
                    room_id: req.room_id,
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(client_proto::KickMemberResponse {
            success: response.success,
        })
    }

    async fn ban_member(
        &self,
        request: Request<BanMemberRequest>,
    ) -> Result<Response<client_proto::BanMemberResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .ban_member(
                admin_proto::BanMemberRequest {
                    room_id: req.room_id,
                    user_id: req.user_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(client_proto::BanMemberResponse {
            success: response.success,
        })
    }

    async fn unban_member(
        &self,
        request: Request<UnbanMemberRequest>,
    ) -> Result<Response<client_proto::UnbanMemberResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .unban_member(
                admin_proto::UnbanMemberRequest {
                    room_id: req.room_id,
                    user_id: req.user_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(client_proto::UnbanMemberResponse {
            success: response.success,
        })
    }

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::GetRoomSettingsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room_settings(admin_proto::GetRoomSettingsRequest {
                room_id: req.room_id,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::UpdateRoomSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_room_settings(
                admin_proto::UpdateRoomSettingsRequest {
                    room_id: req.room_id,
                    settings: req.settings_json,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::ResetRoomSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .reset_room_settings(
                admin_proto::ResetRoomSettingsRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn transfer_room_ownership(
        &self,
        request: Request<TransferRoomOwnershipRequest>,
    ) -> Result<Response<client_proto::TransferRoomOwnershipResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_client_actor_user_id(&req.actor_user_id)
            .await?;
        let response = self
            .client_api
            .transfer_room_ownership(
                &actor_user_id,
                &req.room_id,
                client_proto::TransferRoomOwnershipRequest {
                    new_owner_user_id: req.new_owner_user_id,
                },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_room_password(
        &self,
        request: Request<UpdateRoomPasswordRequest>,
    ) -> Result<Response<admin_proto::UpdateRoomPasswordResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_room_password(
                admin_proto::UpdateRoomPasswordRequest {
                    room_id: req.room_id,
                    new_password: if req.clear {
                        String::new()
                    } else {
                        req.new_password.unwrap_or_default()
                    },
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn ban_room(
        &self,
        request: Request<BanRoomRequest>,
    ) -> Result<Response<admin_proto::BanRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .ban_room(
                admin_proto::BanRoomRequest {
                    room_id: req.room_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn unban_room(
        &self,
        request: Request<UnbanRoomRequest>,
    ) -> Result<Response<admin_proto::UnbanRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .unban_room(
                admin_proto::UnbanRoomRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<admin_proto::DeleteRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_room(
                admin_proto::DeleteRoomRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn approve_room(
        &self,
        request: Request<ApproveRoomRequest>,
    ) -> Result<Response<admin_proto::ApproveRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .approve_room(
                admin_proto::ApproveRoomRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn batch_ban_rooms(
        &self,
        request: Request<BatchBanRoomsRequest>,
    ) -> Result<Response<admin_proto::BatchBanRoomsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .batch_ban_rooms(
                admin_proto::BatchBanRoomsRequest {
                    room_ids: req.room_ids,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn batch_delete_rooms(
        &self,
        request: Request<BatchDeleteRoomsRequest>,
    ) -> Result<Response<admin_proto::BatchDeleteRoomsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .batch_delete_rooms(
                admin_proto::BatchDeleteRoomsRequest {
                    room_ids: req.room_ids,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn start_playback(
        &self,
        request: Request<StartPlaybackRequest>,
    ) -> Result<Response<client_proto::StartPlaybackResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .start_playback(
                &req.room_id,
                client_proto::StartPlaybackRequest {
                    media_id: req.media_id,
                    playlist_id: req.playlist_id,
                    target: req.target_json,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn stop_playback(
        &self,
        request: Request<StopPlaybackRequest>,
    ) -> Result<Response<client_proto::StopPlaybackResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .stop_playback(&req.room_id, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_playback(
        &self,
        request: Request<GetPlaybackRequest>,
    ) -> Result<Response<client_proto::GetPlaybackResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_playback(&req.room_id, &validated.user_id)
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn create_publish_key(
        &self,
        request: Request<CreatePublishKeyRequest>,
    ) -> Result<Response<client_proto::CreatePublishKeyResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_client_actor_user_id(&req.actor_user_id)
            .await?;
        let response = self
            .client_api
            .create_publish_key(
                &actor_user_id,
                &req.room_id,
                client_proto::CreatePublishKeyRequest { id: req.media_id },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<client_proto::GetStreamInfoResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_stream_info(&req.room_id, &req.media_id)
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_room_streams(
        &self,
        request: Request<ListRoomStreamsRequest>,
    ) -> Result<Response<client_proto::ListRoomStreamsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_room_streams(
                &req.room_id,
                client_proto::ListRoomStreamsRequest {
                    page: req.page,
                    page_size: req.page_size,
                    search: req.search,
                    sort_by: match crate::proto::RoomStreamListSortBy::try_from(req.sort_by) {
                        Ok(crate::proto::RoomStreamListSortBy::MediaId) => {
                            client_proto::RoomStreamListSortBy::MediaId as i32
                        }
                        _ => client_proto::RoomStreamListSortBy::Unspecified as i32,
                    },
                    sort_direction: match crate::proto::SortDirection::try_from(req.sort_direction)
                    {
                        Ok(crate::proto::SortDirection::Desc) => {
                            client_proto::SortDirection::Desc as i32
                        }
                        Ok(crate::proto::SortDirection::Asc) => {
                            client_proto::SortDirection::Asc as i32
                        }
                        _ => client_proto::SortDirection::Unspecified as i32,
                    },
                },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_playlists(
        &self,
        request: Request<ListPlaylistsRequest>,
    ) -> Result<Response<client_proto::ListPlaylistsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_playlists(
                &req.room_id,
                client_proto::ListPlaylistsRequest {
                    parent_id: req.parent_id,
                    page: req.page,
                    page_size: req.page_size,
                    search: req.search,
                    source_provider: req.source_provider,
                    provider_instance_name: req.provider_instance_name,
                    dynamic_only: req.dynamic_only,
                    sort_by: req.sort_by,
                    sort_direction: req.sort_direction,
                    availability: client_proto::ResourceAvailabilityFilter::All as i32,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_playlist(
        &self,
        request: Request<GetPlaylistRequest>,
    ) -> Result<Response<client_proto::GetPlaylistResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_playlist(&req.room_id, &req.playlist_id, &validated.user_id)
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<client_proto::CreatePlaylistResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_client_actor_user_id(&req.actor_user_id)
            .await?;
        let response = self
            .client_api
            .create_playlist(
                &actor_user_id,
                &req.room_id,
                client_proto::CreatePlaylistRequest {
                    name: req.name,
                    parent_id: req.parent_id,
                    source_provider: req.source_provider,
                    source_config: req.source_config_json,
                    provider_instance_name: req.provider_instance_name,
                },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_playlist(
        &self,
        request: Request<UpdatePlaylistRequest>,
    ) -> Result<Response<client_proto::UpdatePlaylistResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_playlist(
                &req.room_id,
                client_proto::UpdatePlaylistRequest {
                    playlist_id: req.playlist_id,
                    name: req.name.unwrap_or_default(),
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn move_playlist(
        &self,
        request: Request<MovePlaylistRequest>,
    ) -> Result<Response<client_proto::MovePlaylistResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .move_playlist(
                &req.room_id,
                client_proto::MovePlaylistRequest {
                    playlist_id: req.playlist_id,
                    anchor: req.anchor.map(|anchor| match anchor {
                        crate::proto::move_playlist_request::Anchor::BeforePlaylistId(id) => {
                            client_proto::move_playlist_request::Anchor::BeforePlaylistId(id)
                        }
                        crate::proto::move_playlist_request::Anchor::AfterPlaylistId(id) => {
                            client_proto::move_playlist_request::Anchor::AfterPlaylistId(id)
                        }
                    }),
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn delete_playlist(
        &self,
        request: Request<DeletePlaylistRequest>,
    ) -> Result<Response<client_proto::DeletePlaylistResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_playlist(
                &req.room_id,
                client_proto::DeletePlaylistRequest {
                    playlist_id: req.playlist_id,
                    force: req.force,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_media(
        &self,
        request: Request<ListMediaRequest>,
    ) -> Result<Response<client_proto::ListPlaylistItemsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_media(
                &req.room_id,
                client_proto::ListPlaylistItemsRequest {
                    playlist_id: req.playlist_id,
                    target: req.target_json,
                    page: req.page,
                    page_size: req.page_size,
                    search: req.search,
                    source_provider: req.source_provider,
                    provider_instance_name: req.provider_instance_name,
                    sort_by: req.sort_by,
                    sort_direction: req.sort_direction,
                    availability: client_proto::ResourceAvailabilityFilter::All as i32,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn add_media(
        &self,
        request: Request<AddMediaRequest>,
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_client_actor_user_id(&req.actor_user_id)
            .await?;
        let response = self
            .client_api
            .add_media(
                &actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    provider: req.provider,
                    provider_instance_name: req.provider_instance_name,
                    source_config: req.source_config_json,
                    title: req.title,
                },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn add_direct_url_media(
        &self,
        request: Request<AddDirectUrlMediaRequest>,
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_client_actor_user_id(&req.actor_user_id)
            .await?;
        let response = self
            .client_api
            .add_media(
                &actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    provider: "direct_url".to_string(),
                    provider_instance_name: String::new(),
                    source_config: serde_json::to_vec(&serde_json::json!({ "url": req.url }))
                        .map_err(|error| {
                            tracing::error!(error = %error, "failed to encode media source config");
                            Status::internal("failed to encode media source config")
                        })?,
                    title: req.title,
                },
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn edit_media(
        &self,
        request: Request<EditMediaRequest>,
    ) -> Result<Response<client_proto::EditMediaResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .edit_media(
                &req.room_id,
                client_proto::EditMediaRequest {
                    media_id: req.media_id,
                    title: req.title,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn delete_media(
        &self,
        request: Request<DeleteMediaRequest>,
    ) -> Result<Response<client_proto::DeleteMediaResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_media(
                &req.room_id,
                client_proto::DeleteMediaRequest {
                    media_id: req.media_id,
                    force: req.force,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn move_media(
        &self,
        request: Request<MoveMediaRequest>,
    ) -> Result<Response<client_proto::MoveMediaResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .move_media(
                &req.room_id,
                client_proto::MoveMediaRequest {
                    media_ids: req.media_ids,
                    source_playlist_id: req.source_playlist_id,
                    target_playlist_id: req.target_playlist_id,
                    all_from_scope: req.all_from_scope,
                    before_media_id: req.anchor.as_ref().and_then(|anchor| match anchor {
                        crate::proto::move_media_request::Anchor::BeforeMediaId(id) => {
                            Some(id.clone())
                        }
                        crate::proto::move_media_request::Anchor::AfterMediaId(_) => None,
                    }),
                    after_media_id: req.anchor.and_then(|anchor| match anchor {
                        crate::proto::move_media_request::Anchor::BeforeMediaId(_) => None,
                        crate::proto::move_media_request::Anchor::AfterMediaId(id) => Some(id),
                    }),
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_provider_instances(
        &self,
        request: Request<ListProviderInstancesRequest>,
    ) -> Result<Response<admin_proto::ListProviderInstancesResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_provider_instances(admin_proto::ListProviderInstancesRequest {
                page: req.page,
                page_size: req.page_size,
                provider_type: req.provider_type,
                search: req.search,
                enabled: req.enabled,
                tls: req.tls,
                sort_by: req.sort_by,
                sort_direction: req.sort_direction,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn add_provider_instance(
        &self,
        request: Request<AddProviderInstanceRequest>,
    ) -> Result<Response<admin_proto::AddProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .add_provider_instance(
                admin_proto::AddProviderInstanceRequest {
                    name: req.name,
                    endpoint: req.endpoint,
                    comment: req.comment,
                    timeout_seconds: req.timeout_seconds,
                    tls: req.tls,
                    insecure_tls: req.insecure_tls,
                    providers: req.providers,
                    config: req.config_json,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_provider_instance(
        &self,
        request: Request<UpdateProviderInstanceRequest>,
    ) -> Result<Response<admin_proto::UpdateProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let comment = if req.clear_comment {
            Some(String::new())
        } else {
            req.comment
        };
        let response = self
            .admin_api
            .update_provider_instance(
                admin_proto::UpdateProviderInstanceRequest {
                    name: req.name,
                    endpoint: req.endpoint,
                    comment,
                    timeout_seconds: req.timeout_seconds,
                    tls: req.tls,
                    insecure_tls: req.insecure_tls,
                    providers: req.providers,
                    config: req.config_json,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn delete_provider_instance(
        &self,
        request: Request<DeleteProviderInstanceRequest>,
    ) -> Result<Response<admin_proto::DeleteProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_provider_instance(
                admin_proto::DeleteProviderInstanceRequest { name: req.name },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn reconnect_provider_instance(
        &self,
        request: Request<ReconnectProviderInstanceRequest>,
    ) -> Result<Response<admin_proto::ReconnectProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .reconnect_provider_instance(
                admin_proto::ReconnectProviderInstanceRequest { name: req.name },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn enable_provider_instance(
        &self,
        request: Request<EnableProviderInstanceRequest>,
    ) -> Result<Response<admin_proto::EnableProviderInstanceResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .enable_provider_instance(admin_proto::EnableProviderInstanceRequest { name: req.name })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn disable_provider_instance(
        &self,
        request: Request<DisableProviderInstanceRequest>,
    ) -> Result<Response<admin_proto::DisableProviderInstanceResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .disable_provider_instance(admin_proto::DisableProviderInstanceRequest {
                name: req.name,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_settings(
        &self,
        request: Request<GetSettingsRequest>,
    ) -> Result<Response<admin_proto::GetSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let response = self
            .admin_api
            .get_settings(admin_proto::GetSettingsRequest {}, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_settings_group(
        &self,
        request: Request<GetSettingsGroupRequest>,
    ) -> Result<Response<admin_proto::GetSettingsGroupResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_settings_group(
                admin_proto::GetSettingsGroupRequest { group: req.group },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn update_settings(
        &self,
        request: Request<UpdateSettingsRequest>,
    ) -> Result<Response<admin_proto::UpdateSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_settings(
                admin_proto::UpdateSettingsRequest {
                    group: req.group,
                    settings: req.settings,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn send_test_email(
        &self,
        request: Request<SendTestEmailRequest>,
    ) -> Result<Response<admin_proto::SendTestEmailResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .send_test_email(admin_proto::SendTestEmailRequest { to: req.to })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn get_system_stats(
        &self,
        request: Request<GetSystemStatsRequest>,
    ) -> Result<Response<admin_proto::GetSystemStatsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let response = self
            .admin_api
            .get_system_stats(admin_proto::GetSystemStatsRequest {})
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn list_active_streams(
        &self,
        request: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<admin_proto::ListActiveStreamsResponse>, Status> {
        self.check_admin_get_validated(&request).await?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_active_streams(admin_proto::ListActiveStreamsRequest {
                page: req.page,
                page_size: req.page_size,
                room_id: req.room_id,
                user_id: req.user_id,
                node_id: req.node_id,
                search: req.search,
                sort_by: req.sort_by,
                sort_direction: req.sort_direction,
            })
            .await
            .map_err(map_api_error)?;
        Self::proto_response(response)
    }

    async fn kick_stream(
        &self,
        request: Request<KickStreamRequest>,
    ) -> Result<Response<admin_proto::KickStreamResponse>, Status> {
        let validated = self.check_admin_get_validated(&request).await?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        self.admin_api
            .kick_stream(
                admin_proto::KickStreamRequest {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Self::proto_response(admin_proto::KickStreamResponse {})
    }

    async fn stop_server(
        &self,
        request: Request<StopServerRequest>,
    ) -> Result<Response<Self::StopServerStream>, Status> {
        self.check_admin_get_validated(&request).await?;

        let request = request.into_inner();
        let requested_mode = match ProtoShutdownMode::try_from(request.mode)
            .unwrap_or(ProtoShutdownMode::Unspecified)
        {
            ProtoShutdownMode::Force => ShutdownMode::Force,
            ProtoShutdownMode::Graceful | ProtoShutdownMode::Unspecified => ShutdownMode::Graceful,
        };

        let subscription = self.lifecycle_controller.subscribe();
        let requested_event = self.lifecycle_controller.request_shutdown(requested_mode);
        let events = stop_server_event_stream(
            subscription.snapshot,
            requested_event,
            subscription.receiver,
        );
        Ok(Response::new(Box::pin(events)))
    }
}

fn stop_server_event_stream(
    snapshot: LifecycleEvent,
    requested_event: LifecycleEvent,
    receiver: tokio::sync::broadcast::Receiver<LifecycleEvent>,
) -> impl Stream<Item = Result<StopServerEvent, Status>> + Send + 'static {
    futures::stream::unfold(
        (Some(snapshot), Some(requested_event), receiver, false),
        |(snapshot, requested_event, mut receiver, done)| async move {
            if done {
                return None;
            }

            if let Some(snapshot) = snapshot {
                let done = snapshot.terminal;
                return Some((
                    Ok(snapshot.to_proto()),
                    (None, requested_event, receiver, done),
                ));
            }

            if let Some(requested_event) = requested_event {
                let done = requested_event.terminal;
                return Some((Ok(requested_event.to_proto()), (None, None, receiver, done)));
            }

            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        let done = event.terminal;
                        return Some((Ok(event.to_proto()), (None, None, receiver, done)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    )
}

fn map_user_role(role: i32) -> i32 {
    match UserRole::try_from(role).unwrap_or(UserRole::Unspecified) {
        UserRole::User => common_proto::UserRole::User as i32,
        UserRole::Admin => common_proto::UserRole::Admin as i32,
        UserRole::Root => common_proto::UserRole::Root as i32,
        UserRole::Unspecified => common_proto::UserRole::Unspecified as i32,
    }
}

fn map_user_status(status: i32) -> i32 {
    match UserStatus::try_from(status).unwrap_or(UserStatus::Unspecified) {
        UserStatus::Active => common_proto::UserStatus::Active as i32,
        UserStatus::Pending => common_proto::UserStatus::Pending as i32,
        UserStatus::Rejected => common_proto::UserStatus::Rejected as i32,
        UserStatus::Banned => common_proto::UserStatus::Banned as i32,
        UserStatus::Unspecified => common_proto::UserStatus::Unspecified as i32,
    }
}

fn map_room_status(status: i32) -> i32 {
    match RoomStatus::try_from(status).unwrap_or(RoomStatus::Unspecified) {
        RoomStatus::Active => common_proto::RoomStatus::Active as i32,
        RoomStatus::Pending => common_proto::RoomStatus::Pending as i32,
        RoomStatus::Rejected => common_proto::RoomStatus::Rejected as i32,
        RoomStatus::Closed => common_proto::RoomStatus::Closed as i32,
        RoomStatus::Unspecified => common_proto::RoomStatus::Unspecified as i32,
    }
}

fn validate_client_actor_user(user: &synctv_core::models::User) -> Result<(), Status> {
    if user.is_deleted() {
        return Err(Status::permission_denied(format!(
            "actor user '{}' is deleted",
            user.username
        )));
    }
    match user.status {
        CoreUserStatus::Active => {}
        CoreUserStatus::Pending => {
            return Err(Status::permission_denied(format!(
                "actor user '{}' is pending and cannot perform this operation",
                user.username
            )));
        }
        CoreUserStatus::Rejected => {
            return Err(Status::permission_denied(format!(
                "actor user '{}' is rejected and cannot perform this operation",
                user.username
            )));
        }
        CoreUserStatus::Banned => {
            return Err(Status::permission_denied(format!(
                "actor user '{}' is banned",
                user.username
            )));
        }
    }
    Ok(())
}

fn map_management_user_lookup_error(err: synctv_core::Error) -> Status {
    match err {
        synctv_core::Error::NotFound(_) => Status::not_found("User not found"),
        synctv_core::Error::InvalidInput(message) => Status::invalid_argument(message),
        synctv_core::Error::Authentication(message) => Status::unauthenticated(message),
        synctv_core::Error::Authorization(message) => Status::permission_denied(message),
        synctv_core::Error::AlreadyExists(message) => Status::already_exists(message),
        synctv_core::Error::RateLimited(message) => Status::resource_exhausted(message),
        synctv_core::Error::ServiceUnavailable(message) => Status::unavailable(message),
        synctv_core::Error::Timeout(message) => Status::deadline_exceeded(message),
        synctv_core::Error::OptimisticLockConflict => {
            Status::aborted("management actor user was modified concurrently")
        }
        synctv_core::Error::LockConflict(message) => Status::aborted(message),
        synctv_core::Error::EmailNotVerified => {
            Status::permission_denied("management actor user email is not verified")
        }
        other => {
            tracing::error!("Management user lookup failed: {other}");
            Status::internal("Internal error")
        }
    }
}

fn map_api_error(err: ApiError) -> tonic::Status {
    let msg = err.message().to_string();
    match err.classify() {
        ErrorKind::NotFound => tonic::Status::not_found(msg),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(msg),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(msg),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(msg),
        ErrorKind::RateLimited => tonic::Status::resource_exhausted(msg),
        ErrorKind::ServiceUnavailable => tonic::Status::unavailable(msg),
        ErrorKind::Internal => {
            tracing::error!("Management API internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_client_actor_user, ManagementAccessController};
    use synctv_core::models::{SignupMethod, User, UserStatus};
    use tonic::{Code, Request};

    fn make_actor_user(username: &str, status: UserStatus) -> User {
        let mut user = User::new_with_status(
            username.to_string(),
            Some(format!("{username}@example.com")),
            "hash".to_string(),
            SignupMethod::Email,
            status,
        );
        user.email_verified = true;
        user
    }

    #[test]
    fn validate_client_actor_user_accepts_active_user() {
        let user = make_actor_user("root", UserStatus::Active);
        validate_client_actor_user(&user).expect("active actor should be accepted");
    }

    #[test]
    fn validate_client_actor_user_rejects_pending_user_with_explicit_message() {
        let user = make_actor_user("root", UserStatus::Pending);
        let error =
            validate_client_actor_user(&user).expect_err("pending actor should be rejected");

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert_eq!(
            error.message(),
            "actor user 'root' is pending and cannot perform this operation"
        );
    }

    #[test]
    fn validate_client_actor_user_rejects_banned_user_with_explicit_message() {
        let user = make_actor_user("root", UserStatus::Banned);
        let error = validate_client_actor_user(&user).expect_err("banned actor should be rejected");

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert_eq!(error.message(), "actor user 'root' is banned");
    }

    #[test]
    fn validate_client_actor_user_rejects_deleted_user_with_explicit_message() {
        let mut user = make_actor_user("root", UserStatus::Active);
        user.deleted_at = Some(user.created_at);
        let error =
            validate_client_actor_user(&user).expect_err("deleted actor should be rejected");

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert_eq!(error.message(), "actor user 'root' is deleted");
    }

    #[test]
    fn map_api_error_preserves_service_unavailable() {
        let status = super::map_api_error(synctv_api::impls::ApiError::ServiceUnavailable(
            "live streaming backend unavailable".to_string(),
        ));

        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "live streaming backend unavailable");
    }

    #[test]
    fn map_api_error_hides_internal_details() {
        let status = super::map_api_error(synctv_api::impls::ApiError::Internal(
            "redis://user:secret@localhost:6379 failure".to_string(),
        ));

        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "Internal error");
        assert!(!status.message().contains("secret"));
    }

    #[test]
    fn management_access_controller_allows_missing_header_when_token_disabled() {
        let controller = ManagementAccessController::new("");
        let request = Request::new(());

        controller
            .authorize(&request)
            .expect("disabled management token should allow local requests");
    }

    #[test]
    fn management_access_controller_rejects_missing_header_when_token_configured() {
        let controller = ManagementAccessController::new("management-secret");
        let request = Request::new(());

        let error = controller
            .authorize(&request)
            .expect_err("missing auth header must be rejected when management token is configured");

        assert_eq!(error.code(), Code::Unauthenticated);
        assert_eq!(error.message(), "Management authentication required");
    }

    #[test]
    fn management_access_controller_rejects_incorrect_bearer_token() {
        let controller = ManagementAccessController::new("management-secret");
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("authorization", "Bearer wrong-secret".parse().unwrap());

        let error = controller
            .authorize(&request)
            .expect_err("wrong management bearer token must be rejected");

        assert_eq!(error.code(), Code::Unauthenticated);
        assert_eq!(error.message(), "Invalid management authorization header");
    }

    #[test]
    fn management_access_controller_accepts_matching_bearer_token() {
        let controller = ManagementAccessController::new("management-secret");
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("authorization", "Bearer management-secret".parse().unwrap());

        controller
            .authorize(&request)
            .expect("matching management bearer token should be accepted");
    }
}
