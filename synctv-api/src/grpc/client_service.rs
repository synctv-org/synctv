use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::impls::messaging::{MessageSender, StreamMessage, StreamMessageHandler};
use synctv_cluster::sync::{ClusterManager, ConnectionManager};
use synctv_core::models::{Room, RoomId, UserId};
use synctv_core::service::{
    ContentFilter, RateLimitConfig, RateLimiter, RoomService as CoreRoomService,
    UserService as CoreUserService,
};

// Use synctv_proto for all gRPC traits and types
use crate::proto::client::{
    auth_service_server::AuthService, email_service_server::EmailService,
    public_service_server::PublicService, room_service_server::RoomService,
    user_service_server::UserService, AddMediaBatchRequest,
    AddMediaBatchResponse, AddMediaRequest, AddMediaResponse, BanMemberRequest, BanMemberResponse,
    CheckRoomPasswordRequest, CheckRoomPasswordResponse, CheckRoomRequest, CheckRoomResponse,
    ClearPlaylistRequest, ClearPlaylistResponse, ClientMessage, ConfirmEmailRequest,
    ConfirmEmailResponse, ConfirmPasswordResetRequest, ConfirmPasswordResetResponse,
    CreatePlaylistRequest, CreatePlaylistResponse, CreatePublishKeyRequest,
    CreatePublishKeyResponse, CreateRoomRequest, CreateRoomResponse, DeleteMediaBatchRequest,
    DeleteMediaBatchResponse, DeleteMediaRequest, DeleteMediaResponse, DeletePlaylistRequest,
    DeletePlaylistResponse, DeleteRoomRequest, DeleteRoomResponse, EditMediaRequest,
    EditMediaResponse, GetChatHistoryRequest, GetChatHistoryResponse, GetHotRoomsRequest,
    GetHotRoomsResponse, GetIceServersRequest, GetIceServersResponse, GetNetworkQualityRequest,
    GetNetworkQualityResponse, GetPlaybackRequest, GetPlaybackResponse, GetPlaylistRequest,
    GetPlaylistResponse, GetProfileRequest, GetProfileResponse, GetPublicSettingsRequest,
    GetPublicSettingsResponse, GetRoomMembersRequest, GetRoomMembersResponse, GetRoomRequest,
    GetRoomResponse, GetRoomSettingsRequest, GetRoomSettingsResponse, GetStreamInfoRequest,
    GetStreamInfoResponse, JoinRoomRequest, JoinRoomResponse, KickMemberRequest,
    KickMemberResponse, LeaveRoomRequest, LeaveRoomResponse, ListCreatedRoomsRequest,
    ListCreatedRoomsResponse, ListParticipatedRoomsRequest, ListParticipatedRoomsResponse,
    ListPlaylistItemsRequest, ListPlaylistItemsResponse, ListPlaylistRequest, ListPlaylistResponse,
    ListPlaylistsRequest, ListPlaylistsResponse, ListRoomStreamsRequest, ListRoomStreamsResponse,
    ListRoomsRequest, ListRoomsResponse, LoginRequest, LoginResponse, LogoutRequest,
    LogoutResponse, RefreshTokenRequest, RefreshTokenResponse, RegisterRequest, RegisterResponse,
    ReorderMediaBatchRequest, ReorderMediaBatchResponse, RequestPasswordResetRequest,
    RequestPasswordResetResponse, ResetRoomSettingsRequest, ResetRoomSettingsResponse,
    SendVerificationEmailRequest, SendVerificationEmailResponse, ServerMessage, SetPasswordRequest,
    SetPasswordResponse, SetRoomPasswordRequest, SetRoomPasswordResponse, SetUsernameRequest,
    SetUsernameResponse, StartPlaybackRequest, StartPlaybackResponse, StopPlaybackRequest,
    StopPlaybackResponse, SwapMediaRequest, SwapMediaResponse, UnbanMemberRequest,
    UnbanMemberResponse, UpdateMemberPermissionsRequest, UpdateMemberPermissionsResponse,
    UpdatePlaylistRequest, UpdatePlaylistResponse, UpdateRoomSettingsRequest,
    UpdateRoomSettingsResponse,
};

/// Buffer size for the outgoing message channel in `MessageStream` connections.
/// Provides backpressure for slow clients without excessive memory usage.
const MESSAGE_STREAM_BUFFER_SIZE: usize = 100;

use super::map_api_error;

#[derive(Debug)]
enum GrpcReceiveOutcome<T, E> {
    Message(Result<Option<T>, E>),
    ResponseStreamClosed,
}

async fn await_grpc_receive_or_response_close<T, E, F>(
    receive_future: F,
    response_sender: tokio::sync::mpsc::Sender<ServerMessage>,
) -> GrpcReceiveOutcome<T, E>
where
    F: std::future::Future<Output = Result<Option<T>, E>>,
{
    tokio::select! {
        result = receive_future => GrpcReceiveOutcome::Message(result),
        () = response_sender.closed() => GrpcReceiveOutcome::ResponseStreamClosed,
    }
}

#[allow(clippy::result_large_err)]
fn extract_authenticated_user_id(
    request: &Request<impl std::fmt::Debug>,
) -> Result<UserId, Status> {
    let user_context = request
        .extensions()
        .get::<super::interceptors::UserContext>()
        .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

    Ok(UserId::from_string(user_context.user_id.clone()))
}

#[allow(clippy::result_large_err)]
fn extract_authenticated_token(
    request: &Request<impl std::fmt::Debug>,
) -> Result<synctv_core::service::AuthenticatedToken, Status> {
    request
        .extensions()
        .get::<synctv_core::service::AuthenticatedToken>()
        .cloned()
        .ok_or_else(|| Status::unauthenticated("Authentication required"))
}

#[allow(clippy::result_large_err)]
fn map_message_stream_join_error(error: String) -> Status {
    let (kind, message) = crate::impls::parse_api_error_string(&error);
    match kind {
        crate::impls::ErrorKind::RateLimited => Status::resource_exhausted(message.to_string()),
        crate::impls::ErrorKind::ServiceUnavailable => Status::unavailable(message.to_string()),
        _ => {
            tracing::error!("Unexpected MessageStream pre_join failure: {error}");
            Status::internal("Failed to establish message stream")
        }
    }
}

#[allow(clippy::result_large_err)]
fn validate_realtime_room_access(room: &Room) -> Result<(), Status> {
    if room.is_banned {
        return Err(Status::permission_denied("This room has been banned"));
    }

    if room.status.is_closed() {
        return Err(Status::permission_denied(
            "This room is closed and not accepting new connections",
        ));
    }

    Ok(())
}

#[allow(clippy::result_large_err)]
fn map_message_stream_membership_error(err: synctv_core::Error) -> Status {
    match crate::impls::ClientApiImpl::map_room_access_error(err) {
        crate::impls::ApiError::Authorization(message) => Status::permission_denied(message),
        crate::impls::ApiError::NotFound(message) => Status::not_found(message),
        crate::impls::ApiError::ServiceUnavailable(message) => Status::unavailable(message),
        other => map_api_error(other),
    }
}

#[allow(clippy::result_large_err)]
fn map_email_flow_error(err: crate::impls::ApiError) -> Status {
    map_api_error(err)
}

#[allow(clippy::result_large_err)]
fn map_message_stream_user_lookup_error(err: synctv_core::Error) -> Status {
    map_api_error(crate::impls::ApiError::from(err))
}

#[allow(clippy::result_large_err)]
fn map_message_stream_room_lookup_error(err: synctv_core::Error) -> Status {
    match crate::impls::ApiError::from(err) {
        crate::impls::ApiError::NotFound(message) => Status::not_found(message),
        other => map_api_error(other),
    }
}

/// Configuration for `ClientService`
#[derive(Clone)]
pub struct ClientServiceConfig {
    pub user_service: CoreUserService,
    pub room_service: CoreRoomService,
    pub chat_service: Arc<synctv_core::service::ChatService>,
    pub cluster_manager: Option<Arc<ClusterManager>>,
    pub rate_limiter: RateLimiter,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_manager: ConnectionManager,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub config: Arc<synctv_core::Config>,
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
}

/// `ClientService` implementation
#[derive(Clone)]
pub struct ClientServiceImpl {
    user_service: Arc<CoreUserService>,
    room_service: Arc<CoreRoomService>,
    chat_service: Arc<synctv_core::service::ChatService>,
    cluster_manager: Option<Arc<ClusterManager>>,
    rate_limiter: Arc<RateLimiter>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    connection_manager: Arc<ConnectionManager>,
    email_service: Option<Arc<synctv_core::service::EmailService>>,
    email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    client_api: Arc<crate::impls::ClientApiImpl>,
    config: Arc<synctv_core::Config>,
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    heartbeat_schedule: crate::impls::HeartbeatSchedule,
}

impl ClientServiceImpl {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        user_service: CoreUserService,
        room_service: CoreRoomService,
        chat_service: Arc<synctv_core::service::ChatService>,
        cluster_manager: Option<Arc<ClusterManager>>,
        rate_limiter: RateLimiter,
        rate_limit_config: RateLimitConfig,
        content_filter: ContentFilter,
        connection_manager: ConnectionManager,
        email_service: Option<Arc<synctv_core::service::EmailService>>,
        email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
        _settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
        _providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
        config: Arc<synctv_core::Config>,
        client_api: Arc<crate::impls::ClientApiImpl>,
    ) -> Self {
        Self {
            user_service: Arc::new(user_service),
            room_service: Arc::new(room_service),
            chat_service,
            cluster_manager,
            rate_limiter: Arc::new(rate_limiter),
            rate_limit_config: Arc::new(rate_limit_config),
            content_filter: Arc::new(content_filter),
            connection_manager: Arc::new(connection_manager),
            email_service,
            email_token_service,
            client_api,
            config,
            notification_service: None,
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
        }
    }

    /// Create `ClientService` from configuration struct
    #[must_use]
    pub fn from_config(config: ClientServiceConfig) -> Self {
        Self {
            user_service: Arc::new(config.user_service),
            room_service: Arc::new(config.room_service),
            chat_service: config.chat_service,
            cluster_manager: config.cluster_manager,
            rate_limiter: Arc::new(config.rate_limiter),
            rate_limit_config: Arc::new(config.rate_limit_config),
            content_filter: Arc::new(config.content_filter),
            connection_manager: Arc::new(config.connection_manager),
            email_service: config.email_service,
            email_token_service: config.email_token_service,
            client_api: config.client_api,
            config: config.config,
            notification_service: config.notification_service,
            heartbeat_schedule: config.heartbeat_schedule,
        }
    }

    /// Build an `EmailApiImpl` from the configured services, or return an error
    fn email_api(&self) -> Result<crate::impls::EmailApiImpl, crate::impls::ApiError> {
        let email_service = self.email_service.as_ref().ok_or_else(|| {
            crate::impls::ApiError::Internal(
                "Email service is not configured on this server. Please contact the administrator."
                    .to_string(),
            )
        })?;
        let email_token_service = self.email_token_service.as_ref().ok_or_else(|| {
            crate::impls::ApiError::Internal(
                "Email verification service is not configured on this server.".to_string(),
            )
        })?;

        Ok(crate::impls::EmailApiImpl::new(
            self.user_service.clone(),
            email_service.clone(),
            email_token_service.clone(),
        ))
    }

    /// Extract `user_id` from `UserContext` (injected by `inject_user` interceptor).
    ///
    /// Authentication and security checks are completed by the transport layer
    /// before this service is called, so this only consumes the injected context.
    #[allow(clippy::result_large_err)]
    async fn get_user_id(&self, request: &Request<impl std::fmt::Debug>) -> Result<UserId, Status> {
        extract_authenticated_user_id(request)
    }

    /// Extract `RoomContext` (injected by `inject_room` interceptor)
    #[allow(clippy::result_large_err)]
    fn get_room_context(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<super::interceptors::RoomContext, Status> {
        request
            .extensions()
            .get::<super::interceptors::RoomContext>()
            .cloned()
            .ok_or_else(|| Status::unauthenticated("Room context required"))
    }

    /// Extract `room_id` from `RoomContext`
    #[allow(clippy::result_large_err)]
    fn get_room_id(&self, request: &Request<impl std::fmt::Debug>) -> Result<RoomId, Status> {
        let room_context = self.get_room_context(request)?;
        Ok(RoomId::from_string(room_context.room_id))
    }
}

// ==================== AuthService Implementation ====================
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AuthService for ClientServiceImpl {
    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        // Extract client IP for brute-force protection (Issue #24)
        let client_ip = super::extract_client_ip(&request, &self.config);
        let req = request.into_inner();
        let response = self
            .client_api
            .register(req, client_ip)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let client_ip = super::extract_client_ip(&request, &self.config);
        let req = request.into_inner();
        let response = self
            .client_api
            .login(req, client_ip)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn refresh_token(
        &self,
        request: Request<RefreshTokenRequest>,
    ) -> Result<Response<RefreshTokenResponse>, Status> {
        let req = request.into_inner();
        let response = self
            .client_api
            .refresh_token(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

// ==================== UserService Implementation ====================
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl UserService for ClientServiceImpl {
    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let authenticated = extract_authenticated_token(&request)?;
        let claims = authenticated.claims;

        if claims.jti.is_empty() {
            return Err(Status::unauthenticated("Authentication required"));
        }

        let now = chrono::Utc::now().timestamp();
        let remaining_ttl = (claims.exp - now).max(0) as u64;
        if remaining_ttl == 0 {
            return Err(Status::unauthenticated("Authentication required"));
        }

        self.user_service
            .blacklist_access_token(&claims.jti, remaining_ttl)
            .await
            .map_err(crate::impls::ApiError::from)
            .map_err(map_api_error)?;

        Ok(Response::new(LogoutResponse {
            success: true,
            message: String::new(),
        }))
    }

    async fn get_profile(
        &self,
        request: Request<GetProfileRequest>,
    ) -> Result<Response<GetProfileResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let response = self
            .client_api
            .get_profile(user_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_username(
        &self,
        request: Request<SetUsernameRequest>,
    ) -> Result<Response<SetUsernameResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self
            .client_api
            .set_username(user_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_password(
        &self,
        request: Request<SetPasswordRequest>,
    ) -> Result<Response<SetPasswordResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self
            .client_api
            .set_password(user_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_room(
        &self,
        request: Request<CreateRoomRequest>,
    ) -> Result<Response<CreateRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self
            .client_api
            .create_room(user_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;
        let response = self
            .client_api
            .get_room(user_id.as_str(), room_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn join_room(
        &self,
        request: Request<JoinRoomRequest>,
    ) -> Result<Response<JoinRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;
        let response = self
            .client_api
            .join_room(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn leave_room(
        &self,
        request: Request<LeaveRoomRequest>,
    ) -> Result<Response<LeaveRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;
        let response = self
            .client_api
            .leave_room(user_id.as_str(), room_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<DeleteRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;
        let response = self
            .client_api
            .delete_room(user_id.as_str(), room_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn check_room_password(
        &self,
        request: Request<CheckRoomPasswordRequest>,
    ) -> Result<Response<CheckRoomPasswordResponse>, Status> {
        let _user_id = self.get_user_id(&request).await?;
        let client_ip = super::extract_client_ip(&request, &self.config)
            .map_or_else(|| "unknown".to_string(), |ip| ip.to_string());
        let req = request.into_inner();
        let room_id = crate::room_id_validation::parse_room_id(&req.room_id)
            .map_err(|e| Status::invalid_argument(format!("Invalid room_id: {e}")))?;
        let response = self
            .client_api
            .check_room_password(room_id.as_str(), req, &client_ip)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_created_rooms(
        &self,
        request: Request<ListCreatedRoomsRequest>,
    ) -> Result<Response<ListCreatedRoomsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self
            .client_api
            .list_created_rooms(user_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_participated_rooms(
        &self,
        request: Request<ListParticipatedRoomsRequest>,
    ) -> Result<Response<ListParticipatedRoomsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let page = u32::try_from(req.page).unwrap_or(1);
        let page_size = u32::try_from(req.page_size).unwrap_or(10).min(100);
        let params = synctv_core::models::PageParams::new(Some(page), Some(page_size));
        let response = self
            .client_api
            .get_joined_rooms(
                user_id.as_str(),
                params.page as i32,
                params.page_size as i32,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

// ==================== RoomService Implementation ====================
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl RoomService for ClientServiceImpl {
    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<UpdateRoomSettingsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .update_room_settings(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<GetRoomMembersResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .get_room_members(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<UpdateMemberPermissionsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .update_member_permissions(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<KickMemberResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .kick_member(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn ban_member(
        &self,
        request: Request<BanMemberRequest>,
    ) -> Result<Response<BanMemberResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .ban_member(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn unban_member(
        &self,
        request: Request<UnbanMemberRequest>,
    ) -> Result<Response<UnbanMemberResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .unban_member(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self
            .client_api
            .get_room_settings(user_id.as_str(), room_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<ResetRoomSettingsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self
            .client_api
            .reset_room_settings(user_id.as_str(), room_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_room_password(
        &self,
        request: Request<SetRoomPasswordRequest>,
    ) -> Result<Response<SetRoomPasswordResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .set_room_password(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    type MessageStreamStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ServerMessage, Status>> + Send + 'static>,
    >;

    async fn message_stream(
        &self,
        request: Request<tonic::Streaming<ClientMessage>>,
    ) -> Result<Response<Self::MessageStreamStream>, Status> {
        use tokio::sync::mpsc;

        // Extract all data from request BEFORE any await points.
        // Request<Streaming<_>> is !Sync, so holding it across .await makes
        // the future !Send, violating the tonic trait requirement.
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?
            .clone();
        let room_id = self.get_room_id(&request)?;
        let user_id = UserId::from_string(user_context.user_id.clone());
        let client_stream = request.into_inner();

        // Get user details from service
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(map_message_stream_user_lookup_error)?;
        let username = user.username;

        // Check room membership before establishing stream
        self.room_service
            .check_membership(&room_id, &user_id)
            .await
            .map_err(map_message_stream_membership_error)?;

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(map_message_stream_room_lookup_error)?;
        validate_realtime_room_access(&room)?;

        tracing::info!(
            user_id = %user_id.as_str(),
            room_id = %room_id.as_str(),
            "Client establishing MessageStream connection"
        );

        // Connection registration is handled by StreamMessageHandler::run()
        // which generates its own connection_id and manages the full lifecycle.

        // ClusterManager is required for real-time messaging; in single-node mode
        // without Redis, streaming is not supported.
        let cluster_manager = self.cluster_manager.clone().ok_or_else(|| {
            Status::unavailable(
                "Real-time messaging requires cluster manager (Redis not configured)",
            )
        })?;

        // Create channel for outgoing messages with bounded capacity to prevent memory exhaustion
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<ServerMessage>(MESSAGE_STREAM_BUFFER_SIZE);

        // Create a single shared gRPC message sender (avoids dual-sender from same channel)
        let grpc_sender = Arc::new(GrpcMessageSender::new(outgoing_tx));

        // Create StreamMessageHandler with all configuration
        let stream_handler = StreamMessageHandler::new(
            room_id.clone(),
            user_id.clone(),
            username.clone(),
            self.room_service.clone(),
            self.chat_service.clone(),
            cluster_manager,
            (*self.connection_manager).clone(),
            self.rate_limiter.clone(),
            self.rate_limit_config.clone(),
            self.content_filter.clone(),
            Arc::clone(&grpc_sender) as Arc<dyn MessageSender>,
        )
        .with_heartbeat_schedule(self.heartbeat_schedule)
        .with_ws_message_rate_limit(
            self.config
                .connection_limits
                .ws_message_rate_limit_per_second,
        );

        // Wire notification service for direct real-time push (matches HTTP WebSocket behavior)
        let stream_handler = if let Some(ref notif_svc) = self.notification_service {
            stream_handler.with_notification_service(Arc::clone(notif_svc))
        } else {
            stream_handler
        };

        // Check connection limits BEFORE returning the response stream.
        // This ensures limit violations are reported as gRPC errors instead of
        // silently failing inside a background task.
        stream_handler
            .pre_join()
            .await
            .map_err(map_message_stream_join_error)?;

        // Create unified GrpcStreamMessage adapter (shares the same sender)
        let mut grpc_stream = GrpcStreamMessage {
            client_stream,
            sender: Arc::clone(&grpc_sender),
            alive: std::sync::atomic::AtomicBool::new(true),
        };

        // Spawn the unified message loop (handles disconnect signals, heartbeat, cleanup)
        // Uses run_after_join since pre_join was already called above.
        tokio::spawn(async move {
            if let Err(e) = stream_handler.run_after_join(&mut grpc_stream).await {
                tracing::error!("gRPC stream handler error: {}", e);
            }
        });

        let output_stream = ReceiverStream::new(outgoing_rx).map(Ok::<_, Status>);

        Ok(Response::new(
            Box::pin(output_stream) as Self::MessageStreamStream
        ))
    }

    async fn get_chat_history(
        &self,
        request: Request<GetChatHistoryRequest>,
    ) -> Result<Response<GetChatHistoryResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .get_chat_history(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_ice_servers(
        &self,
        request: Request<GetIceServersRequest>,
    ) -> Result<Response<GetIceServersResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self
            .client_api
            .get_ice_servers(&room_id, &user_id)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_network_quality(
        &self,
        request: Request<GetNetworkQualityRequest>,
    ) -> Result<Response<GetNetworkQualityResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self
            .client_api
            .get_network_quality(&room_id, &user_id)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn add_media(
        &self,
        request: Request<AddMediaRequest>,
    ) -> Result<Response<AddMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .add_media(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_media(
        &self,
        request: Request<DeleteMediaRequest>,
    ) -> Result<Response<DeleteMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .delete_media(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn edit_media(
        &self,
        request: Request<EditMediaRequest>,
    ) -> Result<Response<EditMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .edit_media(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_playlist(
        &self,
        request: Request<ListPlaylistRequest>,
    ) -> Result<Response<ListPlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .list_media(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_playlist_items(
        &self,
        request: Request<ListPlaylistItemsRequest>,
    ) -> Result<Response<ListPlaylistItemsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .list_playlist_items(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn swap_media(
        &self,
        request: Request<SwapMediaRequest>,
    ) -> Result<Response<SwapMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .swap_media(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn clear_playlist(
        &self,
        request: Request<ClearPlaylistRequest>,
    ) -> Result<Response<ClearPlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self
            .client_api
            .clear_playlist(user_id.as_str(), room_id.as_str())
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn add_media_batch(
        &self,
        request: Request<AddMediaBatchRequest>,
    ) -> Result<Response<AddMediaBatchResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .add_media_batch(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_media_batch(
        &self,
        request: Request<DeleteMediaBatchRequest>,
    ) -> Result<Response<DeleteMediaBatchResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .delete_media_batch(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn reorder_media_batch(
        &self,
        request: Request<ReorderMediaBatchRequest>,
    ) -> Result<Response<ReorderMediaBatchResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .reorder_media_batch(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_playback(
        &self,
        request: Request<StartPlaybackRequest>,
    ) -> Result<Response<StartPlaybackResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .start_playback(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn stop_playback(
        &self,
        request: Request<StopPlaybackRequest>,
    ) -> Result<Response<StopPlaybackResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .stop_playback(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_playback(
        &self,
        request: Request<GetPlaybackRequest>,
    ) -> Result<Response<GetPlaybackResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .get_playback(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_publish_key(
        &self,
        request: Request<CreatePublishKeyRequest>,
    ) -> Result<Response<CreatePublishKeyResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();

        self.client_api
            .create_publish_key(user_id.as_str(), room_id.as_str(), req)
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<GetStreamInfoResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();

        self.client_api
            .get_stream_info(user_id.as_str(), room_id.as_str(), &req.media_id)
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn list_room_streams(
        &self,
        request: Request<ListRoomStreamsRequest>,
    ) -> Result<Response<ListRoomStreamsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let _req = request.into_inner();

        self.client_api
            .list_room_streams(user_id.as_str(), room_id.as_str())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    // Playlist Management
    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<CreatePlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .create_playlist(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_playlist(
        &self,
        request: Request<GetPlaylistRequest>,
    ) -> Result<Response<GetPlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .get_playlist(user_id.as_str(), room_id.as_str(), &req.playlist_id)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_playlist(
        &self,
        request: Request<UpdatePlaylistRequest>,
    ) -> Result<Response<UpdatePlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .update_playlist(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_playlist(
        &self,
        request: Request<DeletePlaylistRequest>,
    ) -> Result<Response<DeletePlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .delete_playlist(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_playlists(
        &self,
        request: Request<ListPlaylistsRequest>,
    ) -> Result<Response<ListPlaylistsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .list_playlists(user_id.as_str(), room_id.as_str(), req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

/// gRPC message sender for `StreamMessageHandler`
struct GrpcMessageSender {
    sender: tokio::sync::mpsc::Sender<ServerMessage>,
}

impl GrpcMessageSender {
    const fn new(sender: tokio::sync::mpsc::Sender<ServerMessage>) -> Self {
        Self { sender }
    }
}

impl MessageSender for GrpcMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        self.sender.try_send(message).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                tracing::warn!(
                    "gRPC outgoing message dropped: client stream buffer is full \
                         (buffer capacity: {}). Client may be too slow to consume messages.",
                    MESSAGE_STREAM_BUFFER_SIZE,
                );
                "Channel full: client too slow to consume messages".to_string()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "Channel closed: client disconnected".to_string()
            }
        })
    }

    fn is_alive(&self) -> bool {
        !self.sender.is_closed()
    }
}

/// gRPC stream implementation of `StreamMessage` trait
///
/// Adapts `tonic::Streaming<ClientMessage>` + `mpsc::Sender<ServerMessage>` to the
/// unified `StreamMessage` interface, enabling full code reuse with the WebSocket path.
struct GrpcStreamMessage {
    client_stream: tonic::Streaming<ClientMessage>,
    sender: Arc<GrpcMessageSender>,
    alive: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl StreamMessage for GrpcStreamMessage {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        match await_grpc_receive_or_response_close(
            self.client_stream.message(),
            self.sender.sender.clone(),
        )
        .await
        {
            GrpcReceiveOutcome::Message(Ok(Some(msg))) => Some(Ok(msg)),
            GrpcReceiveOutcome::Message(Ok(None)) => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                None
            }
            GrpcReceiveOutcome::Message(Err(e)) => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Some(Err(format!("gRPC stream error: {e}")))
            }
            GrpcReceiveOutcome::ResponseStreamClosed => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                None
            }
        }
    }

    fn send(&self, message: ServerMessage) -> Result<(), String> {
        MessageSender::send(&*self.sender, message)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed) && self.sender.is_alive()
    }

    // gRPC uses HTTP/2 PING frames automatically, no application-level ping needed
}

// ==================== PublicService Implementation ====================
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl PublicService for ClientServiceImpl {
    async fn check_room(
        &self,
        request: Request<CheckRoomRequest>,
    ) -> Result<Response<CheckRoomResponse>, Status> {
        let req = request.into_inner();
        let response = self
            .client_api
            .check_room(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<ListRoomsResponse>, Status> {
        let req = request.into_inner();
        let response = self
            .client_api
            .list_rooms(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_hot_rooms(
        &self,
        request: Request<GetHotRoomsRequest>,
    ) -> Result<Response<GetHotRoomsResponse>, Status> {
        let req = request.into_inner();
        let response = self
            .client_api
            .get_hot_rooms(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_public_settings(
        &self,
        _request: Request<GetPublicSettingsRequest>,
    ) -> Result<Response<GetPublicSettingsResponse>, Status> {
        let response = self
            .client_api
            .get_public_settings()
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

// ==================== EmailService Implementation ====================
// Delegates to shared EmailApiImpl to avoid duplicating logic with HTTP handlers.
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl EmailService for ClientServiceImpl {
    async fn send_verification_email(
        &self,
        request: Request<SendVerificationEmailRequest>,
    ) -> Result<Response<SendVerificationEmailResponse>, Status> {
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();

        let result = email_api
            .send_verification_email(&req.email)
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(SendVerificationEmailResponse {
            message: result.message,
        }))
    }

    async fn confirm_email(
        &self,
        request: Request<ConfirmEmailRequest>,
    ) -> Result<Response<ConfirmEmailResponse>, Status> {
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();

        let result = email_api
            .confirm_email(&req.email, &req.token)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(ConfirmEmailResponse {
            message: result.message,
            user_id: result.user_id,
        }))
    }

    async fn request_password_reset(
        &self,
        request: Request<RequestPasswordResetRequest>,
    ) -> Result<Response<RequestPasswordResetResponse>, Status> {
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();

        let result = email_api
            .request_password_reset(&req.email)
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(RequestPasswordResetResponse {
            message: result.message,
        }))
    }

    async fn confirm_password_reset(
        &self,
        request: Request<ConfirmPasswordResetRequest>,
    ) -> Result<Response<ConfirmPasswordResetResponse>, Status> {
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();

        let result = email_api
            .confirm_password_reset(&req.email, &req.token, &req.new_password)
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(ConfirmPasswordResetResponse {
            message: result.message,
            user_id: result.user_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::interceptors::{RoomContext, UserContext};

    // ==================== Error Mapping ====================

    #[test]
    fn test_map_api_error_not_found() {
        let err = crate::impls::ApiError::NotFound("room not found".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::NotFound);
        assert!(status.message().contains("not found"));
    }

    #[test]
    fn test_map_api_error_unauthenticated() {
        let err = crate::impls::ApiError::Authentication("invalid token".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_map_api_error_permission_denied() {
        let err = crate::impls::ApiError::Authorization("forbidden".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn test_map_api_error_already_exists() {
        let err = crate::impls::ApiError::AlreadyExists("user exists".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::AlreadyExists);
    }

    #[test]
    fn test_map_api_error_invalid_argument() {
        let err = crate::impls::ApiError::InvalidInput("bad input".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn test_map_api_error_internal_hides_details() {
        let err = crate::impls::ApiError::Internal("secret DB password=abc123".to_string());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Internal);
        // Internal errors should NOT leak implementation details
        assert_eq!(status.message(), "Internal error");
        assert!(!status.message().contains("password"));
        assert!(!status.message().contains("secret"));
    }

    #[test]
    fn test_create_publish_key_grpc_maps_service_unavailable() {
        let err = crate::impls::ApiError::ServiceUnavailable("publish key backend unavailable".into());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "publish key backend unavailable");
    }

    #[test]
    fn test_get_stream_info_grpc_maps_not_found() {
        let err = crate::impls::ApiError::NotFound("stream not found".into());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::NotFound);
        assert_eq!(status.message(), "stream not found");
    }

    #[test]
    fn test_list_room_streams_grpc_maps_service_unavailable() {
        let err = crate::impls::ApiError::ServiceUnavailable("livestream registry unavailable".into());
        let status = map_api_error(err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "livestream registry unavailable");
    }

    #[test]
    fn test_send_verification_email_grpc_maps_service_unavailable() {
        let err = crate::impls::ApiError::ServiceUnavailable("email backend unavailable".into());
        let status = map_email_flow_error(err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "email backend unavailable");
    }

    #[test]
    fn test_request_password_reset_grpc_maps_service_unavailable() {
        let err = crate::impls::ApiError::ServiceUnavailable("password reset backend unavailable".into());
        let status = map_email_flow_error(err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "password reset backend unavailable");
    }

    #[test]
    fn test_message_stream_user_lookup_backend_outage_stays_unavailable() {
        let status = map_message_stream_user_lookup_error(synctv_core::Error::ServiceUnavailable(
            "user backend unavailable".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "user backend unavailable");
    }

    #[test]
    fn test_message_stream_room_lookup_not_found_stays_not_found() {
        let status = map_message_stream_room_lookup_error(synctv_core::Error::NotFound(
            "Room not found".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::NotFound);
        assert_eq!(status.message(), "Room not found");
    }

    #[test]
    fn test_map_api_error_all_variants() {
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
                crate::impls::ApiError::ServiceUnavailable("x".into()),
                tonic::Code::Unavailable,
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

    // ==================== GrpcMessageSender ====================

    #[test]
    fn test_grpc_message_sender_send_success() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerMessage>(10);
        let sender = GrpcMessageSender::new(tx);

        let msg = ServerMessage::default();
        let result = MessageSender::send(&sender, msg);
        assert!(result.is_ok());

        // Verify message was received
        let received = rx.try_recv();
        assert!(received.is_ok());
    }

    #[test]
    fn test_grpc_message_sender_channel_closed() {
        let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(10);
        let sender = GrpcMessageSender::new(tx);
        drop(rx); // Close receiver

        let msg = ServerMessage::default();
        let result = MessageSender::send(&sender, msg);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disconnected"));
    }

    #[test]
    fn test_grpc_message_sender_channel_full() {
        // Create a channel with capacity 1
        let (tx, _rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
        let sender = GrpcMessageSender::new(tx);

        // Fill the channel
        let msg1 = ServerMessage::default();
        assert!(MessageSender::send(&sender, msg1).is_ok());

        // Second send should fail (channel full)
        let msg2 = ServerMessage::default();
        let result = MessageSender::send(&sender, msg2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("full"));
    }

    #[test]
    fn test_grpc_message_sender_is_alive_until_receiver_closes() {
        let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
        let sender = GrpcMessageSender::new(tx);

        assert!(
            sender.is_alive(),
            "open response channel must be reported alive"
        );
        drop(rx);
        assert!(
            !sender.is_alive(),
            "closed response channel must be reported dead immediately"
        );
    }

    #[tokio::test]
    async fn test_await_grpc_receive_or_response_close_notices_closed_response_stream() {
        let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
        drop(rx);

        let outcome = await_grpc_receive_or_response_close(
            std::future::pending::<Result<Option<ClientMessage>, tonic::Status>>(),
            tx,
        )
        .await;

        assert!(matches!(outcome, GrpcReceiveOutcome::ResponseStreamClosed));
    }

    #[tokio::test]
    async fn test_await_grpc_receive_or_response_close_prefers_received_message() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
        let expected = ClientMessage::default();

        let outcome = await_grpc_receive_or_response_close(
            std::future::ready(Ok::<_, tonic::Status>(Some(expected.clone()))),
            tx,
        )
        .await;

        match outcome {
            GrpcReceiveOutcome::Message(Ok(Some(actual))) => assert_eq!(actual, expected),
            other => panic!("expected received message outcome, got {other:?}"),
        }
    }

    // ==================== Constants ====================

    #[test]
    fn test_message_stream_buffer_size_reasonable() {
        // Buffer should be at least 10 and at most 1000
        const { assert!(MESSAGE_STREAM_BUFFER_SIZE >= 10) };
        const { assert!(MESSAGE_STREAM_BUFFER_SIZE <= 1000) };
    }

    #[test]
    fn test_extract_authenticated_user_id_reads_interceptor_context() {
        let user_id = UserId::new();
        let mut request = tonic::Request::new(());
        request.extensions_mut().insert(UserContext {
            user_id: user_id.as_str().to_string(),
            iat: 1_700_000_000,
            pv: 2,
        });

        let extracted = extract_authenticated_user_id(&request).expect("UserContext should exist");
        assert_eq!(extracted, user_id);
    }

    #[test]
    fn test_extract_authenticated_user_id_requires_user_context() {
        let request = tonic::Request::new(());
        let result = extract_authenticated_user_id(&request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_extract_authenticated_token_reads_extension() {
        let user_id = UserId::new();
        let expected = synctv_core::service::AuthenticatedToken {
            user_id: user_id.clone(),
            claims: synctv_core::service::Claims {
                sub: user_id.as_str().to_string(),
                typ: "access".to_string(),
                jti: "logout-token".to_string(),
                iat: 1_700_000_000,
                exp: 1_800_000_000,
                pv: 1,
                iss: None,
                aud: None,
            },
        };

        let mut request = tonic::Request::new(());
        request.extensions_mut().insert(expected.clone());

        let actual = extract_authenticated_token(&request).expect("authenticated token");
        assert_eq!(actual.user_id, expected.user_id);
        assert_eq!(actual.claims.jti, expected.claims.jti);
    }

    #[test]
    fn test_extract_authenticated_token_requires_extension() {
        let request = tonic::Request::new(());
        let result = extract_authenticated_token(&request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_get_room_context_reads_room_context_extension() {
        let mut request = tonic::Request::new(());
        let expected = RoomContext {
            room_id: "room1234_abx".to_string(),
        };
        request.extensions_mut().insert(expected.clone());

        let room_context = request
            .extensions()
            .get::<RoomContext>()
            .cloned()
            .expect("room context");
        let room_id = RoomId::from_string(room_context.room_id);
        assert_eq!(room_id.as_str(), "room1234_abx");
    }

    #[test]
    fn test_room_context_requires_extension() {
        let request = tonic::Request::new(());
        let room_context = request.extensions().get::<RoomContext>().cloned();
        assert!(room_context.is_none());
    }

    #[test]
    fn test_map_message_stream_join_error_maps_capacity_to_resource_exhausted() {
        let status = map_message_stream_join_error(
            "Rate limited: realtime room capacity exceeded".to_string(),
        );
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(status.message(), "realtime room capacity exceeded");
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_capacity_error() {
        let status =
            map_message_stream_join_error("Room at capacity (42 connections, max: 40)".to_string());
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(status.message(), "Room at capacity (42 connections, max: 40)");
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_user_capacity_error() {
        let status = map_message_stream_join_error(
            "Too many connections for this user across all replicas (max 3)".to_string(),
        );
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            status.message(),
            "Too many connections for this user across all replicas (max 3)"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_total_capacity_error() {
        let status = map_message_stream_join_error(
            "Server at capacity across all replicas (42 connections)".to_string(),
        );
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            status.message(),
            "Server at capacity across all replicas (42 connections)"
        );
    }

    #[test]
    fn test_validate_realtime_room_access_rejects_banned_room() {
        let mut room = Room::new("test-room".to_string(), UserId::new());
        room.ban();

        let status = validate_realtime_room_access(&room).expect_err("banned room must fail");
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert!(status.message().contains("banned"));
    }

    #[test]
    fn test_validate_realtime_room_access_rejects_closed_room() {
        let mut room = Room::new("test-room".to_string(), UserId::new());
        room.status = synctv_core::models::RoomStatus::Closed;

        let status = validate_realtime_room_access(&room).expect_err("closed room must fail");
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert!(status.message().contains("not accepting new connections"));
    }

    #[test]
    fn test_validate_realtime_room_access_allows_active_room() {
        let room = Room::new("test-room".to_string(), UserId::new());
        assert!(validate_realtime_room_access(&room).is_ok());
    }

    #[test]
    fn test_map_message_stream_membership_error_backend_outage_stays_unavailable() {
        let status = map_message_stream_membership_error(synctv_core::Error::ServiceUnavailable(
            "membership backend unavailable".to_string(),
        ));

        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "membership backend unavailable");
    }

    #[test]
    fn test_map_message_stream_membership_error_authorization_stays_permission_denied() {
        let status = map_message_stream_membership_error(synctv_core::Error::Authorization(
            "Not a member of this room".to_string(),
        ));

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert_eq!(status.message(), "Forbidden: Not a member of this room");
    }

    #[test]
    fn test_map_message_stream_join_error_maps_distributed_degradation_to_unavailable() {
        let status = map_message_stream_join_error(
            "Service unavailable: distributed room capacity check unavailable".to_string(),
        );
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "distributed room capacity check unavailable"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_degraded_cluster_error() {
        let status = map_message_stream_join_error(
            "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                .to_string(),
        );
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_degraded_user_check_error() {
        let status = map_message_stream_join_error(
            "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
                .to_string(),
        );
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_degraded_total_check_error() {
        let status = map_message_stream_join_error(
            "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
                .to_string(),
        );
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_hides_unexpected_internal_details() {
        let status =
            map_message_stream_join_error("Connection 'conn123' is already registered".to_string());
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "Failed to establish message stream");
    }
}
