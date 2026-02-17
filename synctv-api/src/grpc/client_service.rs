use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use synctv_cluster::sync::{ClusterManager, ConnectionManager};
use crate::impls::messaging::{StreamMessageHandler, MessageSender, StreamMessage};
use synctv_core::models::{
    RoomId, UserId,
};
use synctv_core::service::{
    ContentFilter, RateLimitConfig, RateLimiter, RoomService as CoreRoomService,
    UserService as CoreUserService,
};

// Use synctv_proto for all gRPC traits and types
use crate::proto::client::{
    auth_service_server::AuthService, email_service_server::EmailService,
    media_service_server::MediaService, public_service_server::PublicService,
    room_service_server::RoomService, user_service_server::UserService,
    ServerMessage,
    RegisterRequest, RegisterResponse, LoginRequest, LoginResponse,
    RefreshTokenRequest, RefreshTokenResponse, LogoutRequest, LogoutResponse,
    GetProfileRequest, GetProfileResponse, SetUsernameRequest, SetUsernameResponse,
    SetPasswordRequest, SetPasswordResponse, ListCreatedRoomsRequest, ListCreatedRoomsResponse,
    ListParticipatedRoomsRequest, ListParticipatedRoomsResponse,
    CreateRoomRequest, CreateRoomResponse, GetRoomRequest, GetRoomResponse,
    JoinRoomRequest, JoinRoomResponse, LeaveRoomRequest, LeaveRoomResponse,
    DeleteRoomRequest, DeleteRoomResponse, UpdateRoomSettingsRequest, UpdateRoomSettingsResponse,
    GetRoomMembersRequest, GetRoomMembersResponse,
    UpdateMemberPermissionsRequest, UpdateMemberPermissionsResponse,
    KickMemberRequest, KickMemberResponse, BanMemberRequest, BanMemberResponse,
    UnbanMemberRequest, UnbanMemberResponse, GetRoomSettingsRequest, GetRoomSettingsResponse,
    ResetRoomSettingsRequest, ResetRoomSettingsResponse,
    SetRoomPasswordRequest, SetRoomPasswordResponse,
    CheckRoomPasswordRequest, CheckRoomPasswordResponse,
    ClientMessage, GetChatHistoryRequest, GetChatHistoryResponse,
    AddMediaRequest, AddMediaResponse, RemoveMediaRequest, RemoveMediaResponse,
    EditMediaRequest, EditMediaResponse, ListPlaylistRequest, ListPlaylistResponse,
    ListPlaylistItemsRequest, ListPlaylistItemsResponse,
    SwapMediaRequest, SwapMediaResponse, ClearPlaylistRequest, ClearPlaylistResponse,
    AddMediaBatchRequest, AddMediaBatchResponse, RemoveMediaBatchRequest, RemoveMediaBatchResponse,
    ReorderMediaBatchRequest, ReorderMediaBatchResponse,
    PlayRequest, PlayResponse, PauseRequest, PauseResponse, SeekRequest, SeekResponse,
    SetPlaybackSpeedRequest, SetPlaybackSpeedResponse,
    GetPlaybackStateRequest, GetPlaybackStateResponse,
    CreatePublishKeyRequest, CreatePublishKeyResponse,
    CreatePlaylistRequest, CreatePlaylistResponse, UpdatePlaylistRequest, UpdatePlaylistResponse,
    DeletePlaylistRequest, DeletePlaylistResponse, ListPlaylistsRequest, ListPlaylistsResponse,
    SetCurrentMediaRequest, SetCurrentMediaResponse,
    CheckRoomRequest, CheckRoomResponse, ListRoomsRequest, ListRoomsResponse,
    GetHotRoomsRequest, GetHotRoomsResponse, GetPublicSettingsRequest, GetPublicSettingsResponse,
    SendVerificationEmailRequest, SendVerificationEmailResponse,
    ConfirmEmailRequest, ConfirmEmailResponse,
    RequestPasswordResetRequest, RequestPasswordResetResponse,
    ConfirmPasswordResetRequest, ConfirmPasswordResetResponse,
    GetIceServersRequest, GetIceServersResponse,
    GetNetworkQualityRequest, GetNetworkQualityResponse,
    GetMovieInfoRequest, GetMovieInfoResponse,
    GetStreamInfoRequest, GetStreamInfoResponse,
    ListRoomStreamsRequest, ListRoomStreamsResponse,
};

use synctv_core::service::auth::JwtValidator;

use super::internal_err;

/// Buffer size for the outgoing message channel in MessageStream connections.
/// Provides backpressure for slow clients without excessive memory usage.
const MESSAGE_STREAM_BUFFER_SIZE: usize = 100;

use super::map_api_error as impls_err_to_status;

/// Configuration for `ClientService`
#[derive(Clone)]
pub struct ClientServiceConfig {
    pub user_service: CoreUserService,
    pub room_service: CoreRoomService,
    pub cluster_manager: Arc<ClusterManager>,
    pub rate_limiter: RateLimiter,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_manager: ConnectionManager,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub token_blacklist_service: synctv_core::service::TokenBlacklistService,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub config: Arc<synctv_core::Config>,
    pub sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
    pub client_api: Arc<crate::impls::ClientApiImpl>,
}

/// `ClientService` implementation
#[derive(Clone)]
pub struct ClientServiceImpl {
    user_service: Arc<CoreUserService>,
    room_service: Arc<CoreRoomService>,
    cluster_manager: Arc<ClusterManager>,
    rate_limiter: Arc<RateLimiter>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    connection_manager: Arc<ConnectionManager>,
    email_service: Option<Arc<synctv_core::service::EmailService>>,
    email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    token_blacklist_service: synctv_core::service::TokenBlacklistService,
    client_api: Arc<crate::impls::ClientApiImpl>,
}

impl ClientServiceImpl {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        user_service: CoreUserService,
        room_service: CoreRoomService,
        cluster_manager: Arc<ClusterManager>,
        rate_limiter: RateLimiter,
        rate_limit_config: RateLimitConfig,
        content_filter: ContentFilter,
        connection_manager: ConnectionManager,
        email_service: Option<Arc<synctv_core::service::EmailService>>,
        email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
        token_blacklist_service: synctv_core::service::TokenBlacklistService,
        _settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
        _providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
        _config: Arc<synctv_core::Config>,
        _sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
        client_api: Arc<crate::impls::ClientApiImpl>,
    ) -> Self {
        Self {
            user_service: Arc::new(user_service),
            room_service: Arc::new(room_service),
            cluster_manager,
            rate_limiter: Arc::new(rate_limiter),
            rate_limit_config: Arc::new(rate_limit_config),
            content_filter: Arc::new(content_filter),
            connection_manager: Arc::new(connection_manager),
            email_service,
            email_token_service,
            token_blacklist_service,
            client_api,
        }
    }

    /// Create `ClientService` from configuration struct
    #[must_use]
    pub fn from_config(config: ClientServiceConfig) -> Self {
        Self {
            user_service: Arc::new(config.user_service),
            room_service: Arc::new(config.room_service),
            cluster_manager: config.cluster_manager,
            rate_limiter: Arc::new(config.rate_limiter),
            rate_limit_config: Arc::new(config.rate_limit_config),
            content_filter: Arc::new(config.content_filter),
            connection_manager: Arc::new(config.connection_manager),
            email_service: config.email_service,
            email_token_service: config.email_token_service,
            token_blacklist_service: config.token_blacklist_service,
            client_api: config.client_api,
        }
    }

    /// Build an `EmailApiImpl` from the configured services, or return an error
    fn email_api(&self) -> Result<crate::impls::EmailApiImpl, crate::impls::ApiError> {
        let email_service = self.email_service.as_ref()
            .ok_or_else(|| crate::impls::ApiError::Internal("Email service is not configured on this server. Please contact the administrator.".to_string()))?;
        let email_token_service = self.email_token_service.as_ref()
            .ok_or_else(|| crate::impls::ApiError::Internal("Email verification service is not configured on this server.".to_string()))?;

        Ok(crate::impls::EmailApiImpl::new(
            self.user_service.clone(),
            email_service.clone(),
            email_token_service.clone(),
        ))
    }

    /// Extract `user_id` from `UserContext` (injected by `inject_user` interceptor).
    ///
    /// Blacklist checking is handled by [`BlacklistCheckLayer`] at the transport
    /// level, so no duplicate check is needed here.
    ///
    /// Additionally checks that the user is not banned or deleted, mirroring the
    /// HTTP `AuthUser` extractor defense-in-depth check.
    #[allow(clippy::result_large_err)]
    async fn get_user_id(&self, request: &Request<impl std::fmt::Debug>) -> Result<UserId, Status> {
        let user_context = request
            .extensions()
            .get::<super::interceptors::UserContext>()
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;

        let user_id = UserId::from_string(user_context.user_id.clone());

        // Defense-in-depth: reject banned/deleted users even if they hold a
        // valid JWT issued before the ban. This matches the HTTP AuthUser check.
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|_| Status::unauthenticated("User not found"))?;

        if user.is_deleted() || user.status.is_banned() {
            return Err(Status::unauthenticated("Authentication failed"));
        }

        Ok(user_id)
    }

    /// Extract `RoomContext` (injected by `inject_room` interceptor)
    #[allow(clippy::result_large_err)]
    fn get_room_context(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<super::interceptors::RoomContext, Status> {
        let room_context = request
            .extensions()
            .get::<super::interceptors::RoomContext>()
            .ok_or_else(|| Status::unauthenticated("Room context required"))?;

        Ok(room_context.clone())
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
        let req = request.into_inner();
        let response = self.client_api.register(req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let req = request.into_inner();
        let response = self.client_api.login(req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn refresh_token(
        &self,
        request: Request<RefreshTokenRequest>,
    ) -> Result<Response<RefreshTokenResponse>, Status> {
        let req = request.into_inner();
        let response = self.client_api.refresh_token(req).await.map_err(impls_err_to_status)?;
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
        // Extract Bearer token from metadata using the shared validator
        // (case-insensitive per RFC 7235, consistent with all other gRPC endpoints)
        let access_token = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| JwtValidator::extract_bearer_token(s))
            .transpose()
            .map_err(|_| Status::unauthenticated("Missing or invalid Bearer token"))?
            .ok_or_else(|| Status::unauthenticated("Authorization header required"))?;

        // Extract optional refresh token from metadata
        let refresh_token = request
            .metadata()
            .get("x-refresh-token")
            .and_then(|v| v.to_str().ok());

        let response = self.client_api.logout(&access_token, refresh_token).await
            .map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_profile(
        &self,
        request: Request<GetProfileRequest>,
    ) -> Result<Response<GetProfileResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let response = self.client_api.get_profile(user_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn set_username(
        &self,
        request: Request<SetUsernameRequest>,
    ) -> Result<Response<SetUsernameResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self.client_api.set_username(user_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn set_password(
        &self,
        request: Request<SetPasswordRequest>,
    ) -> Result<Response<SetPasswordResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self.client_api.set_password(user_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn list_created_rooms(
        &self,
        request: Request<ListCreatedRoomsRequest>,
    ) -> Result<Response<ListCreatedRoomsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self.client_api.list_created_rooms(user_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn list_participated_rooms(
        &self,
        request: Request<ListParticipatedRoomsRequest>,
    ) -> Result<Response<ListParticipatedRoomsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let params = synctv_core::models::PageParams::new(Some(req.page as u32), Some(req.page_size as u32));
        let response = self.client_api.get_joined_rooms(user_id.as_str(), params.page as i32, params.page_size as i32).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }
}

// ==================== RoomService Implementation ====================
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl RoomService for ClientServiceImpl {
    async fn create_room(
        &self,
        request: Request<CreateRoomRequest>,
    ) -> Result<Response<CreateRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let req = request.into_inner();
        let response = self.client_api.create_room(user_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.get_room(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn join_room(
        &self,
        request: Request<JoinRoomRequest>,
    ) -> Result<Response<JoinRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.join_room(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn leave_room(
        &self,
        request: Request<LeaveRoomRequest>,
    ) -> Result<Response<LeaveRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.leave_room(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<DeleteRoomResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.delete_room(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<UpdateRoomSettingsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.update_room_settings(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<GetRoomMembersResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.get_room_members(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<UpdateMemberPermissionsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.update_member_permissions(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<KickMemberResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.kick_member(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn ban_member(
        &self,
        request: Request<BanMemberRequest>,
    ) -> Result<Response<BanMemberResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.ban_member(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn unban_member(
        &self,
        request: Request<UnbanMemberRequest>,
    ) -> Result<Response<UnbanMemberResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.unban_member(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.get_room_settings(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<ResetRoomSettingsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.reset_room_settings(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn set_room_password(
        &self,
        request: Request<SetRoomPasswordRequest>,
    ) -> Result<Response<SetRoomPasswordResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.set_room_password(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn check_room_password(
        &self,
        request: Request<CheckRoomPasswordRequest>,
    ) -> Result<Response<CheckRoomPasswordResponse>, Status> {
        let _user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let client_ip = request
            .remote_addr()
            .map(|addr| addr.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let req = request.into_inner();
        let response = self.client_api.check_room_password(room_id.as_str(), req, &client_ip).await.map_err(impls_err_to_status)?;
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
        // Consume request now so it is not held across await points
        let client_stream = request.into_inner();

        // Check if token has been revoked
        if self
            .token_blacklist_service
            .is_blacklisted(&user_context.raw_token)
            .await
            .unwrap_or(true)
        {
            return Err(Status::unauthenticated("Token has been revoked"));
        }

        let user_id = UserId::from_string(user_context.user_id.clone());

        // Get user details from service
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|e| internal_err("Failed to get user", e))?;
        let username = user.username;

        // Check room membership before establishing stream
        self.room_service
            .check_membership(&room_id, &user_id)
            .await
            .map_err(|e| Status::permission_denied(format!("Not a member of the room: {e}")))?;

        tracing::info!(
            user_id = %user_id.as_str(),
            room_id = %room_id.as_str(),
            "Client establishing MessageStream connection"
        );

        // Connection registration is handled by StreamMessageHandler::run()
        // which generates its own connection_id and manages the full lifecycle.

        // Create channel for outgoing messages with bounded capacity to prevent memory exhaustion
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<ServerMessage>(MESSAGE_STREAM_BUFFER_SIZE);

        // Create gRPC message sender
        let grpc_sender = Arc::new(GrpcMessageSender::new(outgoing_tx.clone()));

        // Create StreamMessageHandler with all configuration
        let stream_handler = StreamMessageHandler::new(
            room_id.clone(),
            user_id.clone(),
            username.clone(),
            self.room_service.clone(),
            self.cluster_manager.clone(),
            (*self.connection_manager).clone(),
            self.rate_limiter.clone(),
            self.rate_limit_config.clone(),
            self.content_filter.clone(),
            grpc_sender,
        );

        // Create unified GrpcStreamMessage adapter
        let mut grpc_stream = GrpcStreamMessage {
            client_stream,
            sender: GrpcMessageSender::new(outgoing_tx),
            alive: std::sync::atomic::AtomicBool::new(true),
        };

        // Spawn the unified message loop (handles disconnect signals, heartbeat, cleanup)
        tokio::spawn(async move {
            if let Err(e) = stream_handler.run(&mut grpc_stream).await {
                tracing::error!("gRPC stream handler error: {}", e);
            }
        });

        // Convert outgoing channel to stream, wrapping items in Ok()
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
        let response = self.client_api.get_chat_history(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_ice_servers(
        &self,
        request: Request<GetIceServersRequest>,
    ) -> Result<Response<GetIceServersResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.get_ice_servers(&room_id, &user_id).await
            .map_err(|e| internal_err("Failed to get ICE servers", e))?;
        Ok(Response::new(response))
    }

    async fn get_network_quality(
        &self,
        request: Request<GetNetworkQualityRequest>,
    ) -> Result<Response<GetNetworkQualityResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.get_network_quality(&room_id, &user_id).await
            .map_err(|e| internal_err("Failed to get network quality", e))?;
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
        // Use try_send to avoid blocking and provide backpressure
        // If channel is full, drop the message (client is too slow)
        self.sender
            .try_send(message)
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    "Channel full: client too slow to consume messages".to_string()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    "Channel closed: client disconnected".to_string()
                }
            })
    }
}

/// gRPC stream implementation of `StreamMessage` trait
///
/// Adapts `tonic::Streaming<ClientMessage>` + `mpsc::Sender<ServerMessage>` to the
/// unified `StreamMessage` interface, enabling full code reuse with the WebSocket path.
struct GrpcStreamMessage {
    client_stream: tonic::Streaming<ClientMessage>,
    sender: GrpcMessageSender,
    alive: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl StreamMessage for GrpcStreamMessage {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        match self.client_stream.message().await {
            Ok(Some(msg)) => Some(Ok(msg)),
            Ok(None) => None, // Stream ended gracefully
            Err(e) => {
                self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
                Some(Err(format!("gRPC stream error: {e}")))
            }
        }
    }

    fn send(&self, message: ServerMessage) -> Result<(), String> {
        MessageSender::send(&self.sender, message)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }

    // gRPC uses HTTP/2 PING frames automatically, no application-level ping needed
}

// ==================== MediaService Implementation ====================
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl MediaService for ClientServiceImpl {
    async fn add_media(
        &self,
        request: Request<AddMediaRequest>,
    ) -> Result<Response<AddMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.add_media(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn remove_media(
        &self,
        request: Request<RemoveMediaRequest>,
    ) -> Result<Response<RemoveMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.remove_media(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn edit_media(
        &self,
        request: Request<EditMediaRequest>,
    ) -> Result<Response<EditMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.edit_media(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn list_playlist(
        &self,
        request: Request<ListPlaylistRequest>,
    ) -> Result<Response<ListPlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.get_playlist(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn list_playlist_items(
        &self,
        request: Request<ListPlaylistItemsRequest>,
    ) -> Result<Response<ListPlaylistItemsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.list_playlist_items(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn swap_media(
        &self,
        request: Request<SwapMediaRequest>,
    ) -> Result<Response<SwapMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.swap_media(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn clear_playlist(
        &self,
        request: Request<ClearPlaylistRequest>,
    ) -> Result<Response<ClearPlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.clear_playlist(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn add_media_batch(
        &self,
        request: Request<AddMediaBatchRequest>,
    ) -> Result<Response<AddMediaBatchResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.add_media_batch(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn remove_media_batch(
        &self,
        request: Request<RemoveMediaBatchRequest>,
    ) -> Result<Response<RemoveMediaBatchResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.remove_media_batch(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn reorder_media_batch(
        &self,
        request: Request<ReorderMediaBatchRequest>,
    ) -> Result<Response<ReorderMediaBatchResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.reorder_media_batch(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn play(&self, request: Request<PlayRequest>) -> Result<Response<PlayResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.play(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn pause(
        &self,
        request: Request<PauseRequest>,
    ) -> Result<Response<PauseResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let response = self.client_api.pause(user_id.as_str(), room_id.as_str()).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn seek(&self, request: Request<SeekRequest>) -> Result<Response<SeekResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.seek(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn set_playback_speed(
        &self,
        request: Request<SetPlaybackSpeedRequest>,
    ) -> Result<Response<SetPlaybackSpeedResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.set_playback_speed(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_playback_state(
        &self,
        request: Request<GetPlaybackStateRequest>,
    ) -> Result<Response<GetPlaybackStateResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.get_playback_state(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
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
            .map_err(|e| internal_err("Failed to create publish key", e))
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
            .map_err(|e| internal_err("Failed to get stream info", e))
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
            .map_err(|e| internal_err("Failed to list room streams", e))
    }

    // Playlist Management
    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<CreatePlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.create_playlist(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn update_playlist(
        &self,
        request: Request<UpdatePlaylistRequest>,
    ) -> Result<Response<UpdatePlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.update_playlist(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn delete_playlist(
        &self,
        request: Request<DeletePlaylistRequest>,
    ) -> Result<Response<DeletePlaylistResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.delete_playlist(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn list_playlists(
        &self,
        request: Request<ListPlaylistsRequest>,
    ) -> Result<Response<ListPlaylistsResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.list_playlists(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn set_current_media(
        &self,
        request: Request<SetCurrentMediaRequest>,
    ) -> Result<Response<SetCurrentMediaResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.set_current_media(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_movie_info(
        &self,
        request: Request<GetMovieInfoRequest>,
    ) -> Result<Response<GetMovieInfoResponse>, Status> {
        let user_id = self.get_user_id(&request).await?;
        let room_id = self.get_room_id(&request)?;
        let req = request.into_inner();
        let response = self.client_api.get_movie_info(user_id.as_str(), room_id.as_str(), req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }
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
        let response = self.client_api.check_room(req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<ListRoomsResponse>, Status> {
        let req = request.into_inner();
        let response = self.client_api.list_rooms(req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_hot_rooms(
        &self,
        request: Request<GetHotRoomsRequest>,
    ) -> Result<Response<GetHotRoomsResponse>, Status> {
        let req = request.into_inner();
        let response = self.client_api.get_hot_rooms(req).await.map_err(impls_err_to_status)?;
        Ok(Response::new(response))
    }

    async fn get_public_settings(
        &self,
        _request: Request<GetPublicSettingsRequest>,
    ) -> Result<Response<GetPublicSettingsResponse>, Status> {
        let response = self.client_api.get_public_settings().map_err(impls_err_to_status)?;
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
        let email_api = self.email_api()
            .map_err(impls_err_to_status)?;
        let req = request.into_inner();

        let result = email_api
            .send_verification_email(&req.email)
            .await
            .map_err(|e| internal_err("Email verification", &e))?;

        Ok(Response::new(SendVerificationEmailResponse {
            message: result.message,
        }))
    }

    async fn confirm_email(
        &self,
        request: Request<ConfirmEmailRequest>,
    ) -> Result<Response<ConfirmEmailResponse>, Status> {
        let email_api = self.email_api()
            .map_err(impls_err_to_status)?;
        let req = request.into_inner();

        let result = email_api
            .confirm_email(&req.email, &req.token)
            .await
            .map_err(Status::invalid_argument)?;

        Ok(Response::new(ConfirmEmailResponse {
            message: result.message,
            user_id: result.user_id,
        }))
    }

    async fn request_password_reset(
        &self,
        request: Request<RequestPasswordResetRequest>,
    ) -> Result<Response<RequestPasswordResetResponse>, Status> {
        let email_api = self.email_api()
            .map_err(impls_err_to_status)?;
        let req = request.into_inner();

        let result = email_api
            .request_password_reset(&req.email)
            .await
            .map_err(|e| internal_err("Password reset", &e))?;

        Ok(Response::new(RequestPasswordResetResponse {
            message: result.message,
        }))
    }

    async fn confirm_password_reset(
        &self,
        request: Request<ConfirmPasswordResetRequest>,
    ) -> Result<Response<ConfirmPasswordResetResponse>, Status> {
        let email_api = self.email_api()
            .map_err(impls_err_to_status)?;
        let req = request.into_inner();

        let result = email_api
            .confirm_password_reset(&req.email, &req.token, &req.new_password)
            .await
            .map_err(Status::invalid_argument)?;

        Ok(Response::new(ConfirmPasswordResetResponse {
            message: result.message,
            user_id: result.user_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Error Mapping ====================

    #[test]
    fn test_impls_err_to_status_not_found() {
        let err = crate::impls::ApiError::NotFound("room not found".to_string());
        let status = impls_err_to_status(err);
        assert_eq!(status.code(), tonic::Code::NotFound);
        assert!(status.message().contains("not found"));
    }

    #[test]
    fn test_impls_err_to_status_unauthenticated() {
        let err = crate::impls::ApiError::Authentication("invalid token".to_string());
        let status = impls_err_to_status(err);
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_impls_err_to_status_permission_denied() {
        let err = crate::impls::ApiError::Authorization("forbidden".to_string());
        let status = impls_err_to_status(err);
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn test_impls_err_to_status_already_exists() {
        let err = crate::impls::ApiError::AlreadyExists("user exists".to_string());
        let status = impls_err_to_status(err);
        assert_eq!(status.code(), tonic::Code::AlreadyExists);
    }

    #[test]
    fn test_impls_err_to_status_invalid_argument() {
        let err = crate::impls::ApiError::InvalidInput("bad input".to_string());
        let status = impls_err_to_status(err);
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn test_impls_err_to_status_internal_hides_details() {
        let err = crate::impls::ApiError::Internal("secret DB password=abc123".to_string());
        let status = impls_err_to_status(err);
        assert_eq!(status.code(), tonic::Code::Internal);
        // Internal errors should NOT leak implementation details
        assert_eq!(status.message(), "Internal error");
        assert!(!status.message().contains("password"));
        assert!(!status.message().contains("secret"));
    }

    #[test]
    fn test_impls_err_to_status_all_variants() {
        let variants: Vec<(crate::impls::ApiError, tonic::Code)> = vec![
            (crate::impls::ApiError::NotFound("x".into()), tonic::Code::NotFound),
            (crate::impls::ApiError::Authentication("x".into()), tonic::Code::Unauthenticated),
            (crate::impls::ApiError::Authorization("x".into()), tonic::Code::PermissionDenied),
            (crate::impls::ApiError::AlreadyExists("x".into()), tonic::Code::AlreadyExists),
            (crate::impls::ApiError::InvalidInput("x".into()), tonic::Code::InvalidArgument),
            (crate::impls::ApiError::Internal("x".into()), tonic::Code::Internal),
        ];
        for (err, expected_code) in variants {
            let status = impls_err_to_status(err);
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

    // ==================== Constants ====================

    #[test]
    fn test_message_stream_buffer_size_reasonable() {
        // Buffer should be at least 10 and at most 1000
        assert!(MESSAGE_STREAM_BUFFER_SIZE >= 10);
        assert!(MESSAGE_STREAM_BUFFER_SIZE <= 1000);
    }
}
