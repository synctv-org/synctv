use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::impls::messaging::{
    GuestRealtimeIdentity, MessageSender, RealtimeJoinError, RealtimePrincipal, StreamMessage,
    StreamMessageHandler,
};
use crate::runtime::{RealtimeConnectionService, RealtimeEventService};
use synctv_core::models::{Room, RoomId};
use synctv_core::service::{
    ContentFilter, RateLimitConfig, RequestRateLimiterService, RoomService as CoreRoomService,
    UserService as CoreUserService,
};

// Use synctv_proto for all gRPC traits and types
use crate::proto::client::{
    auth_service_server::AuthService, email_service_server::EmailService,
    public_service_server::PublicService, room_service_server::RoomService,
    user_service_server::UserService, AddMediaBatchRequest, AddMediaBatchResponse, AddMediaRequest,
    AddMediaResponse, AddMemberRequest, AddMemberResponse, ApproveRoomJoinReviewRequest,
    ApproveRoomJoinReviewResponse, BanMemberRequest, BanMemberResponse, CheckRoomRequest,
    CheckRoomResponse, ClearPlaylistRequest, ClearPlaylistResponse, ClientMessage,
    ConfirmEmailRequest, ConfirmEmailResponse, ConfirmPasswordResetRequest,
    ConfirmPasswordResetResponse, CreateGuestTokenRequest, CreateGuestTokenResponse,
    CreatePlaylistRequest, CreatePlaylistResponse, CreateRoomRequest, CreateRoomResponse,
    DeleteEntriesRequest, DeleteEntriesResponse, DeleteMediaRequest, DeleteMediaResponse,
    DeletePasskeyRequest, DeletePasskeyResponse, DeletePlaylistRequest, DeletePlaylistResponse,
    DeleteRoomRequest, DeleteRoomResponse, EditMediaRequest, EditMediaResponse,
    FinishMfaPasskeyRequest, FinishOpaqueLoginRequest, FinishOpaquePasswordUpdateRequest,
    FinishOpaquePasswordUpdateResponse, FinishOpaqueRegistrationRequest, FinishPasskeyBindRequest,
    FinishPasskeyLoginRequest, FinishPasskeyRegistrationRequest, GetChatHistoryRequest,
    GetChatHistoryResponse, GetHotRoomsRequest, GetHotRoomsResponse, GetIceServersRequest,
    GetIceServersResponse, GetPlaybackRequest, GetPlaybackResponse, GetPlaylistRequest,
    GetPlaylistResponse, GetProfileRequest, GetProfileResponse, GetPublicSettingsRequest,
    GetPublicSettingsResponse, GetRoomMembersRequest, GetRoomMembersResponse, GetRoomRequest,
    GetRoomResponse, GetRoomSettingsRequest, GetRoomSettingsResponse, GetUserPreferencesRequest,
    GetUserPreferencesResponse, JoinRoomRequest, JoinRoomResponse, KickMemberRequest,
    KickMemberResponse, LeaveRoomRequest, LeaveRoomResponse, ListMyRoomsRequest,
    ListMyRoomsResponse, ListPasskeysRequest, ListPasskeysResponse, ListPlaylistItemsRequest,
    ListPlaylistItemsResponse, ListPlaylistsRequest, ListPlaylistsResponse,
    ListRoomJoinReviewsRequest, ListRoomJoinReviewsResponse, ListRoomStreamsRequest,
    ListRoomStreamsResponse, ListRoomsRequest, ListRoomsResponse, LoginRequest, LoginResponse,
    LogoutRequest, LogoutResponse, MoveMediaRequest, MoveMediaResponse, MovePlaylistRequest,
    MovePlaylistResponse, PasskeyCredentialResponse, RefreshTokenRequest, RefreshTokenResponse,
    RegisterRequest, RegisterResponse, RejectRoomJoinReviewRequest, RejectRoomJoinReviewResponse,
    RequestEmailLoginRequest, RequestEmailLoginResponse, RequestMfaEmailCodeRequest,
    RequestMfaEmailCodeResponse, RequestPasswordResetRequest, RequestPasswordResetResponse,
    ResetRoomSettingsRequest, ResetRoomSettingsResponse, SendVerificationEmailRequest,
    SendVerificationEmailResponse, ServerMessage, SetPasswordRequest, SetPasswordResponse,
    SetRoomPasswordRequest, SetRoomPasswordResponse, SetUsernameRequest, SetUsernameResponse,
    StartMfaPasskeyRequest, StartMfaPasskeyResponse, StartOpaqueLoginRequest,
    StartOpaqueLoginResponse, StartOpaquePasswordUpdateRequest, StartOpaquePasswordUpdateResponse,
    StartOpaqueRegistrationRequest, StartOpaqueRegistrationResponse, StartPasskeyBindRequest,
    StartPasskeyBindResponse, StartPasskeyLoginRequest, StartPasskeyLoginResponse,
    StartPasskeyRegistrationRequest, StartPasskeyRegistrationResponse, StartPlaybackRequest,
    StartPlaybackResponse, StopPlaybackRequest, StopPlaybackResponse, TransferRoomOwnershipRequest,
    TransferRoomOwnershipResponse, UnbanMemberRequest, UnbanMemberResponse,
    UpdateMemberPermissionsRequest, UpdateMemberPermissionsResponse, UpdatePlaylistRequest,
    UpdatePlaylistResponse, UpdateRoomSettingsRequest, UpdateRoomSettingsResponse,
    UpdateUserPreferencesRequest, UpdateUserPreferencesResponse, VerifyMfaEmailCodeRequest,
    VerifyMfaPasswordRequest,
};

/// Buffer size for the outgoing message channel in `MessageStream` connections.
/// Provides backpressure for slow clients without excessive memory usage.
const MESSAGE_STREAM_BUFFER_SIZE: usize = 100;

use super::map_api_error;
use crate::impls::EndpointRateLimitCategory;

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
fn map_message_stream_join_error(error: RealtimeJoinError) -> Status {
    match error {
        RealtimeJoinError::RateLimited(message) => Status::resource_exhausted(message),
        RealtimeJoinError::ServiceUnavailable(message) => Status::unavailable(message),
        RealtimeJoinError::PermissionDenied(message) => Status::permission_denied(message),
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
    pub event_service: Option<Arc<dyn RealtimeEventService>>,
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
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
    event_service: Option<Arc<dyn RealtimeEventService>>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    connection_service: Arc<dyn RealtimeConnectionService>,
    email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    client_api: Arc<crate::impls::ClientApiImpl>,
    config: Arc<synctv_core::Config>,
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    heartbeat_schedule: crate::impls::HeartbeatSchedule,
}

impl ClientServiceImpl {
    fn email_api_unavailable_error() -> crate::impls::ApiError {
        crate::impls::ApiError::ServiceUnavailable(
            synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE.to_string(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        user_service: CoreUserService,
        room_service: CoreRoomService,
        chat_service: Arc<synctv_core::service::ChatService>,
        event_service: Option<Arc<dyn RealtimeEventService>>,
        rate_limiter: Arc<dyn RequestRateLimiterService>,
        rate_limit_config: RateLimitConfig,
        content_filter: ContentFilter,
        connection_service: Arc<dyn RealtimeConnectionService>,
        email_api: Option<Arc<crate::impls::EmailApiImpl>>,
        _settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
        _providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
        config: Arc<synctv_core::Config>,
        client_api: Arc<crate::impls::ClientApiImpl>,
    ) -> Self {
        Self {
            user_service: Arc::new(user_service),
            room_service: Arc::new(room_service),
            chat_service,
            event_service,
            rate_limiter,
            rate_limit_config: Arc::new(rate_limit_config),
            content_filter: Arc::new(content_filter),
            connection_service,
            email_api,
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
            event_service: config.event_service,
            rate_limiter: config.rate_limiter,
            rate_limit_config: Arc::new(config.rate_limit_config),
            content_filter: Arc::new(config.content_filter),
            connection_service: config.connection_service,
            email_api: config.email_api,
            client_api: config.client_api,
            config: config.config,
            notification_service: config.notification_service,
            heartbeat_schedule: config.heartbeat_schedule,
        }
    }

    /// Resolve the shared `EmailApiImpl`, or return an error when email is not configured.
    fn email_api(&self) -> Result<&Arc<crate::impls::EmailApiImpl>, crate::impls::ApiError> {
        self.email_api
            .as_ref()
            .ok_or_else(Self::email_api_unavailable_error)
    }

    fn request_metadata<T>(&self, request: &Request<T>) -> crate::impls::RequestMetadata {
        super::request_metadata(
            request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )
    }

    #[allow(clippy::result_large_err)]
    fn extract_public_room_id_from_metadata(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<String, Status> {
        let room_id = request
            .metadata()
            .get("x-room-id")
            .ok_or_else(|| Status::invalid_argument("Missing x-room-id header"))?
            .to_str()
            .map_err(|_| Status::invalid_argument("Invalid x-room-id header"))?;

        self.client_api
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(|error| Status::invalid_argument(format!("Invalid room_id: {error}")))?;

        Ok(room_id.to_string())
    }

    #[allow(clippy::result_large_err)]
    fn extract_room_id_from_metadata(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<RoomId, Status> {
        let room_id = self.extract_public_room_id_from_metadata(request)?;
        self.client_api
            .public_id_codec
            .decode_room_id(&room_id)
            .map_err(|error| Status::invalid_argument(format!("Invalid room_id: {error}")))
    }

    #[allow(clippy::result_large_err)]
    fn room_request_context(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<(crate::impls::RequestMetadata, String), Status> {
        Ok((
            self.request_metadata(request),
            self.extract_public_room_id_from_metadata(request)?,
        ))
    }

    #[allow(clippy::result_large_err)]
    fn internal_room_request_context(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<(crate::impls::RequestMetadata, RoomId), Status> {
        Ok((
            self.request_metadata(request),
            self.extract_room_id_from_metadata(request)?,
        ))
    }

    fn extract_guest_token_from_authorization(
        authorization: Option<&str>,
    ) -> Result<Option<String>, Status> {
        let Some(authorization) = authorization else {
            return Ok(None);
        };
        let token = synctv_core::service::auth::JwtValidator::extract_bearer_token(authorization)
            .map_err(|_| Status::unauthenticated("Invalid authorization header"))?;
        if synctv_core::service::JwtService::token_type_hint(&token)
            == Some(synctv_core::service::TokenType::Guest)
        {
            Ok(Some(token))
        } else {
            Ok(None)
        }
    }

    async fn execute_room_actor_endpoint<T, F, Fut>(
        &self,
        metadata: crate::impls::RequestMetadata,
        public_room_id: String,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(Arc<crate::impls::ClientApiImpl>, crate::impls::client::RoomActor) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<T, crate::impls::ApiError>> + Send + 'static,
    {
        let token = metadata
            .authorization
            .as_deref()
            .map(synctv_core::service::auth::JwtValidator::extract_bearer_token)
            .transpose()
            .map_err(|_| Status::unauthenticated("Invalid authorization header"))?
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let is_guest_token = synctv_core::service::JwtService::token_type_hint(&token)
            == Some(synctv_core::service::TokenType::Guest);
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        if is_guest_token {
            executor
                .execute_public_endpoint(&metadata, category, move || {
                    let client_api = client_api.clone();
                    async move {
                        let access = client_api
                            .validate_guest_room_access(&token, &public_room_id)
                            .await?;
                        operation(client_api, crate::impls::client::RoomActor::Guest(access)).await
                    }
                })
                .await
                .map_err(map_api_error)
        } else {
            executor
                .execute_user_endpoint(&metadata, category, move |authenticated| {
                    let client_api = client_api.clone();
                    async move {
                        let actor = client_api
                            .room_actor_for_user(&authenticated.user_id, &public_room_id)
                            .await?;
                        operation(client_api, actor).await
                    }
                })
                .await
                .map_err(map_api_error)
        }
    }

    async fn execute_room_actor_endpoint_with_control<T, F, Fut>(
        &self,
        metadata: crate::impls::RequestMetadata,
        public_room_id: String,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(
                Arc<crate::impls::ClientApiImpl>,
                synctv_core::provider::ExecutionControl,
                crate::impls::client::RoomActor,
            ) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<T, crate::impls::ApiError>> + Send + 'static,
    {
        let token = metadata
            .authorization
            .as_deref()
            .map(synctv_core::service::auth::JwtValidator::extract_bearer_token)
            .transpose()
            .map_err(|_| Status::unauthenticated("Invalid authorization header"))?
            .ok_or_else(|| Status::unauthenticated("Authentication required"))?;
        let is_guest_token = synctv_core::service::JwtService::token_type_hint(&token)
            == Some(synctv_core::service::TokenType::Guest);
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        if is_guest_token {
            executor
                .execute_public_endpoint_with_control(&metadata, category, move |request_control| {
                    let client_api = client_api.clone();
                    async move {
                        let access = client_api
                            .validate_guest_room_access(&token, &public_room_id)
                            .await?;
                        operation(
                            client_api,
                            request_control,
                            crate::impls::client::RoomActor::Guest(access),
                        )
                        .await
                    }
                })
                .await
                .map_err(map_api_error)
        } else {
            executor
                .execute_user_endpoint_with_control(
                    &metadata,
                    category,
                    move |request_control, authenticated| {
                        let client_api = client_api.clone();
                        async move {
                            let actor = client_api
                                .room_actor_for_user(&authenticated.user_id, &public_room_id)
                                .await?;
                            operation(client_api, request_control, actor).await
                        }
                    },
                )
                .await
                .map_err(map_api_error)
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AuthService for ClientServiceImpl {
    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .register_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_opaque_registration(
        &self,
        request: Request<StartOpaqueRegistrationRequest>,
    ) -> Result<Response<StartOpaqueRegistrationResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_opaque_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_opaque_registration(
        &self,
        request: Request<FinishOpaqueRegistrationRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_opaque_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let email_api = self.email_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .login_request_with_control(
                            email_api.as_deref(),
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_guest_token(
        &self,
        request: Request<CreateGuestTokenRequest>,
    ) -> Result<Response<CreateGuestTokenResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .create_guest_token_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_opaque_login(
        &self,
        request: Request<StartOpaqueLoginRequest>,
    ) -> Result<Response<StartOpaqueLoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_opaque_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_opaque_login(
        &self,
        request: Request<FinishOpaqueLoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_opaque_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_passkey_registration(
        &self,
        request: Request<StartPasskeyRegistrationRequest>,
    ) -> Result<Response<StartPasskeyRegistrationResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_passkey_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_passkey_registration(
        &self,
        request: Request<FinishPasskeyRegistrationRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_passkey_registration_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_passkey_login(
        &self,
        request: Request<StartPasskeyLoginRequest>,
    ) -> Result<Response<StartPasskeyLoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .start_passkey_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_passkey_login(
        &self,
        request: Request<FinishPasskeyLoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_passkey_login_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn request_email_login(
        &self,
        request: Request<RequestEmailLoginRequest>,
    ) -> Result<Response<RequestEmailLoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_email_flow_error)?;
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    email_api
                        .request_email_login_with_control(&req.email, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(RequestEmailLoginResponse {
            message: result.message,
        }))
    }

    async fn request_mfa_email_code(
        &self,
        request: Request<RequestMfaEmailCodeRequest>,
    ) -> Result<Response<RequestMfaEmailCodeResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_email_flow_error)?;
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    email_api
                        .request_mfa_email_code_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(result))
    }

    async fn verify_mfa_email_code(
        &self,
        request: Request<VerifyMfaEmailCodeRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let email_api = self.email_api().map_err(map_email_flow_error)?;
        let email_api = email_api.clone();
        let public_id_codec = self.client_api.public_id_codec.clone();
        let executor = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    let outcome = email_api
                        .verify_mfa_email_code_request_with_control(
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await?;
                    Ok::<_, crate::impls::ApiError>(crate::impls::client::login_outcome_to_proto(
                        outcome,
                        &public_id_codec,
                    ))
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(response))
    }

    async fn start_mfa_passkey(
        &self,
        request: Request<StartMfaPasskeyRequest>,
    ) -> Result<Response<StartMfaPasskeyResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |_request_control| async move {
                    client_api.start_mfa_passkey_with_control(req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_mfa_passkey(
        &self,
        request: Request<FinishMfaPasskeyRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .finish_mfa_passkey_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn verify_mfa_password(
        &self,
        request: Request<VerifyMfaPasswordRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .verify_mfa_password_with_control(req, client_ip, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn refresh_token(
        &self,
        request: Request<RefreshTokenRequest>,
    ) -> Result<Response<RefreshTokenResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Auth,
                move |request_control| async move {
                    client_api
                        .refresh_token_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl UserService for ClientServiceImpl {
    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let authorization = metadata.authorization.clone();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |_| async move {
                    let auth_value = authorization.ok_or_else(|| {
                        crate::impls::ApiError::Authentication(
                            synctv_common::messages::AUTHENTICATION_REQUIRED.to_string(),
                        )
                    })?;
                    let token =
                        synctv_core::service::auth::JwtValidator::extract_bearer_token(&auth_value)
                            .map_err(|_| {
                                crate::impls::ApiError::Authentication(
                                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                                )
                            })?;
                    client_api.logout(&token).await?;
                    Ok::<(), crate::impls::ApiError>(())
                },
            )
            .await
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
        let metadata = self.request_metadata(&request);
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response =
            executor
                .execute_user_endpoint(
                    &metadata,
                    EndpointRateLimitCategory::Read,
                    move |authenticated| async move {
                        client_api.get_profile(&authenticated.user_id).await
                    },
                )
                .await
                .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_username(
        &self,
        request: Request<SetUsernameRequest>,
    ) -> Result<Response<SetUsernameResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.set_username(&authenticated.user_id, req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_password(
        &self,
        request: Request<SetPasswordRequest>,
    ) -> Result<Response<SetPasswordResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.set_password(&authenticated.user_id, req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_opaque_password_update(
        &self,
        request: Request<StartOpaquePasswordUpdateRequest>,
    ) -> Result<Response<StartOpaquePasswordUpdateResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_opaque_password_update(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_opaque_password_update(
        &self,
        request: Request<FinishOpaquePasswordUpdateRequest>,
    ) -> Result<Response<FinishOpaquePasswordUpdateResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .finish_opaque_password_update(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_passkey_bind(
        &self,
        request: Request<StartPasskeyBindRequest>,
    ) -> Result<Response<StartPasskeyBindResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_passkey_bind(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_passkey_bind(
        &self,
        request: Request<FinishPasskeyBindRequest>,
    ) -> Result<Response<PasskeyCredentialResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .finish_passkey_bind_request(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_passkeys(
        &self,
        request: Request<ListPasskeysRequest>,
    ) -> Result<Response<ListPasskeysResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api.list_passkeys(&authenticated.user_id).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_passkey(
        &self,
        request: Request<DeletePasskeyRequest>,
    ) -> Result<Response<DeletePasskeyResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.delete_passkey(&authenticated.user_id, req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_user_preferences(
        &self,
        request: Request<GetUserPreferencesRequest>,
    ) -> Result<Response<GetUserPreferencesResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .get_user_preferences(&authenticated.user_id)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_user_preferences(
        &self,
        request: Request<UpdateUserPreferencesRequest>,
    ) -> Result<Response<UpdateUserPreferencesResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .update_user_preferences(&authenticated.user_id, req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_room(
        &self,
        request: Request<CreateRoomRequest>,
    ) -> Result<Response<CreateRoomResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.create_room(&authenticated.user_id, req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let room_id = req.room_id.clone();
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move { client_api.get_room_for_actor(&actor).await },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn join_room(
        &self,
        request: Request<JoinRoomRequest>,
    ) -> Result<Response<JoinRoomResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let client_ip = metadata.client_ip.map(|ip| ip.to_string());
        let req = request.into_inner();
        let room_id = req.room_id.clone();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |request_control, authenticated| async move {
                    client_api
                        .join_room_with_control(
                            &authenticated.user_id,
                            &room_id,
                            req,
                            client_ip.as_deref(),
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_my_rooms(
        &self,
        request: Request<ListMyRoomsRequest>,
    ) -> Result<Response<ListMyRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api.list_my_rooms(&authenticated.user_id, req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl RoomService for ClientServiceImpl {
    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<UpdateRoomSettingsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .update_room_settings(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<GetRoomMembersResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.get_room_members_for_actor(&actor, req).await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn list_room_streams(
        &self,
        request: Request<ListRoomStreamsRequest>,
    ) -> Result<Response<ListRoomStreamsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .list_room_streams(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn add_member(
        &self,
        request: Request<AddMemberRequest>,
    ) -> Result<Response<AddMemberResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .add_member(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_room_join_reviews(
        &self,
        request: Request<ListRoomJoinReviewsRequest>,
    ) -> Result<Response<ListRoomJoinReviewsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .list_room_join_reviews(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn approve_room_join_review(
        &self,
        request: Request<ApproveRoomJoinReviewRequest>,
    ) -> Result<Response<ApproveRoomJoinReviewResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .approve_room_join_review(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn reject_room_join_review(
        &self,
        request: Request<RejectRoomJoinReviewRequest>,
    ) -> Result<Response<RejectRoomJoinReviewResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .reject_room_join_review(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<UpdateMemberPermissionsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .update_member_permissions(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<KickMemberResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .kick_member(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn ban_member(
        &self,
        request: Request<BanMemberRequest>,
    ) -> Result<Response<BanMemberResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .ban_member(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn unban_member(
        &self,
        request: Request<UnbanMemberRequest>,
    ) -> Result<Response<UnbanMemberResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .unban_member(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.get_room_settings_for_actor(&actor).await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<ResetRoomSettingsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .reset_room_settings(&authenticated.user_id, room_id.as_str())
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn transfer_room_ownership(
        &self,
        request: Request<TransferRoomOwnershipRequest>,
    ) -> Result<Response<TransferRoomOwnershipResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .transfer_room_ownership(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_room_password(
        &self,
        request: Request<SetRoomPasswordRequest>,
    ) -> Result<Response<SetRoomPasswordResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .set_room_password(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn leave_room(
        &self,
        request: Request<LeaveRoomRequest>,
    ) -> Result<Response<LeaveRoomResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .leave_room(&authenticated.user_id, room_id.as_str())
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<DeleteRoomResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .delete_room(&authenticated.user_id, room_id.as_str())
                        .await
                },
            )
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
        // Request<Streaming<_>> is !Sync, so holding it across.await makes
        // the future !Send, violating the tonic trait requirement.
        let (metadata, room_id) = self.internal_room_request_context(&request)?;
        let guest_token =
            Self::extract_guest_token_from_authorization(metadata.authorization.as_deref())?;
        let client_stream = request.into_inner();
        let executor = self.client_api.clone();
        let guest_principal = if let Some(guest_token) = guest_token {
            let public_room_id = self
                .client_api
                .public_id_codec
                .encode_room_id(room_id)
                .map_err(|error| Status::invalid_argument(format!("Invalid room_id: {error}")))?;
            let client_api = self.client_api.clone();
            Some(
                executor
                    .execute_public_endpoint(
                        &metadata,
                        EndpointRateLimitCategory::Streaming,
                        move || async move {
                            let access = client_api
                                .validate_guest_room_access(&guest_token, &public_room_id)
                                .await?;
                            let identity = GuestRealtimeIdentity {
                                guest_id: access.guest_id,
                                display_name: access.display_name,
                                session_id: access.session_id,
                                token_jti: access.token_jti,
                                room_guest_version: access.room_guest_version,
                                permissions: access.permissions,
                            };
                            Ok::<_, crate::impls::ApiError>(RealtimePrincipal::guest(
                                room_id, identity,
                            ))
                        },
                    )
                    .await
                    .map_err(map_api_error)?,
            )
        } else {
            None
        };
        let (user_id, _username, principal) = if let Some(principal) = guest_principal {
            (
                principal.connection_user_id(),
                principal.username().to_string(),
                principal,
            )
        } else {
            let user_id = executor
                .execute_user_endpoint(
                    &metadata,
                    EndpointRateLimitCategory::Streaming,
                    move |authenticated| async move {
                        Ok::<_, crate::impls::ApiError>(authenticated.user_id)
                    },
                )
                .await
                .map_err(map_api_error)?;

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

            (
                user_id,
                username.clone(),
                RealtimePrincipal::user(user_id, username),
            )
        };

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(map_message_stream_room_lookup_error)?;
        validate_realtime_room_access(&room)?;

        tracing::info!(
            user_id = %user_id,
            room_id = %room_id,
            "Client establishing MessageStream connection"
        );

        // Connection registration is handled by StreamMessageHandler::run()
        // which generates its own connection_id and manages the full lifecycle.

        // RealtimeManager is required for real-time messaging; in single-node mode
        // without Redis, streaming is not supported.
        let event_service = self.event_service.clone().ok_or_else(|| {
            Status::unavailable(
                "Real-time messaging requires realtime manager (Redis not configured)",
            )
        })?;

        // Create channel for outgoing messages with bounded capacity to prevent memory exhaustion
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<ServerMessage>(MESSAGE_STREAM_BUFFER_SIZE);

        // Create a single shared gRPC message sender (avoids dual-sender from same channel)
        let grpc_sender = Arc::new(GrpcMessageSender::new(outgoing_tx));

        // Create StreamMessageHandler with all configuration
        let stream_handler = StreamMessageHandler::new_with_principal(
            room_id,
            principal,
            &self.room_service,
            self.chat_service.clone(),
            event_service,
            self.connection_service.clone(),
            self.rate_limiter.clone(),
            self.rate_limit_config.clone(),
            self.content_filter.clone(),
            self.client_api.public_id_codec.clone(),
            Arc::clone(&grpc_sender) as Arc<dyn MessageSender>,
        )
        .with_playback_snapshot_service(self.client_api.clone())
        .with_playlist_items_snapshot_service(self.client_api.clone())
        .with_room_members_snapshot_service(self.client_api.clone())
        .with_heartbeat_schedule(self.heartbeat_schedule)
        .with_filter_private_ice_candidates(self.config.webrtc.filter_private_ice_candidates)
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

        // Start the shared real-time actor before returning the response stream so
        // admission failures still surface as gRPC status errors.
        let (incoming_tx, cancel_token) = stream_handler
            .start()
            .await
            .map_err(|error| map_message_stream_join_error(error.into()))?;

        // Pump transport input into the shared handler. The handler owns all
        // business logic and cleanup; this task only decodes transport frames.
        let transport_sender = Arc::clone(&grpc_sender);
        let transport_cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut grpc_stream = GrpcStreamMessage {
                client_stream,
                sender: transport_sender,
                alive: std::sync::atomic::AtomicBool::new(true),
            };

            loop {
                match grpc_stream.recv().await {
                    Some(Ok(message)) => {
                        if incoming_tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::error!("gRPC stream receive error: {}", error);
                        transport_cancel_token.cancel();
                        break;
                    }
                    None => {
                        // Request-stream EOF is only a half-close for bidi gRPC.
                        // Keep the shared real-time actor running so it can still
                        // deliver the response to the client until the response
                        // stream itself closes or a business-level disconnect
                        // signal arrives.
                        break;
                    }
                }
            }
        });

        let response_close_sender = grpc_sender.sender.clone();
        let response_close_token = cancel_token.clone();
        tokio::spawn(async move {
            response_close_sender.closed().await;
            response_close_token.cancel();
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
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.get_chat_history_for_actor(&actor, req).await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn get_ice_servers(
        &self,
        request: Request<GetIceServersRequest>,
    ) -> Result<Response<GetIceServersResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.get_ice_servers_for_actor(&actor).await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_media(
        &self,
        request: Request<AddMediaRequest>,
    ) -> Result<Response<AddMediaResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .add_media(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_media(
        &self,
        request: Request<DeleteMediaRequest>,
    ) -> Result<Response<DeleteMediaResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .delete_media(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_entries(
        &self,
        request: Request<DeleteEntriesRequest>,
    ) -> Result<Response<DeleteEntriesResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .delete_entries(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn edit_media(
        &self,
        request: Request<EditMediaRequest>,
    ) -> Result<Response<EditMediaResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .edit_media(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_playlist_items(
        &self,
        request: Request<ListPlaylistItemsRequest>,
    ) -> Result<Response<ListPlaylistItemsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.list_playlist_items_for_actor(&actor, req).await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn move_media(
        &self,
        request: Request<MoveMediaRequest>,
    ) -> Result<Response<MoveMediaResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .move_media(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn clear_playlist(
        &self,
        request: Request<ClearPlaylistRequest>,
    ) -> Result<Response<ClearPlaylistResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .clear_playlist(&authenticated.user_id, room_id.as_str())
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn add_media_batch(
        &self,
        request: Request<AddMediaBatchRequest>,
    ) -> Result<Response<AddMediaBatchResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .add_media_batch(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_playback(
        &self,
        request: Request<StartPlaybackRequest>,
    ) -> Result<Response<StartPlaybackResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .start_playback(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn stop_playback(
        &self,
        request: Request<StopPlaybackRequest>,
    ) -> Result<Response<StopPlaybackResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .stop_playback(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_playback(
        &self,
        request: Request<GetPlaybackRequest>,
    ) -> Result<Response<GetPlaybackResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let response = self
            .execute_room_actor_endpoint_with_control(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, request_control, actor| async move {
                    client_api
                        .get_playback_for_actor(&actor, req, Some(&request_control))
                        .await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    // Playlist Management
    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<CreatePlaylistResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .create_playlist(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_playlist(
        &self,
        request: Request<GetPlaylistRequest>,
    ) -> Result<Response<GetPlaylistResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let playlist_id = req.playlist_id;
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api
                        .get_playlist_for_actor(&actor, &playlist_id)
                        .await
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn update_playlist(
        &self,
        request: Request<UpdatePlaylistRequest>,
    ) -> Result<Response<UpdatePlaylistResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .update_playlist(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn move_playlist(
        &self,
        request: Request<MovePlaylistRequest>,
    ) -> Result<Response<MovePlaylistResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .move_playlist(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_playlist(
        &self,
        request: Request<DeletePlaylistRequest>,
    ) -> Result<Response<DeletePlaylistResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Media,
                move |authenticated| async move {
                    client_api
                        .delete_playlist(&authenticated.user_id, room_id.as_str(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_playlists(
        &self,
        request: Request<ListPlaylistsRequest>,
    ) -> Result<Response<ListPlaylistsResponse>, Status> {
        let (metadata, room_id) = self.room_request_context(&request)?;
        let req = request.into_inner();
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.list_playlists_for_actor(&actor, req).await
                },
            )
            .await?;
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
            GrpcReceiveOutcome::Message(Ok(None)) => None,
            GrpcReceiveOutcome::ResponseStreamClosed => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                None
            }
            GrpcReceiveOutcome::Message(Err(e)) => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Some(Err(format!("gRPC stream error: {e}")))
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

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl PublicService for ClientServiceImpl {
    async fn check_room(
        &self,
        request: Request<CheckRoomRequest>,
    ) -> Result<Response<CheckRoomResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.check_room(req).await
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<ListRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.list_rooms(req).await
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_hot_rooms(
        &self,
        request: Request<GetHotRoomsRequest>,
    ) -> Result<Response<GetHotRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.get_hot_rooms(req).await
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_public_settings(
        &self,
        request: Request<GetPublicSettingsRequest>,
    ) -> Result<Response<GetPublicSettingsResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.get_public_settings()
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}

// Delegates to shared EmailApiImpl to avoid duplicating logic with HTTP handlers.
#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl EmailService for ClientServiceImpl {
    async fn send_verification_email(
        &self,
        request: Request<SendVerificationEmailRequest>,
    ) -> Result<Response<SendVerificationEmailResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .send_verification_email_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(result))
    }

    async fn confirm_email(
        &self,
        request: Request<ConfirmEmailRequest>,
    ) -> Result<Response<ConfirmEmailResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .confirm_email_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(result))
    }

    async fn request_password_reset(
        &self,
        request: Request<RequestPasswordResetRequest>,
    ) -> Result<Response<RequestPasswordResetResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .request_password_reset_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(result))
    }

    async fn confirm_password_reset(
        &self,
        request: Request<ConfirmPasswordResetRequest>,
    ) -> Result<Response<ConfirmPasswordResetResponse>, Status> {
        let metadata = self.request_metadata(&request);
        let email_api = self.email_api().map_err(map_api_error)?;
        let req = request.into_inner();
        let email_api = email_api.clone();
        let executor = self.client_api.clone();
        let result = executor
            .execute_public_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Email,
                move |request_control| async move {
                    email_api
                        .confirm_password_reset_response_with_control(req, Some(&request_control))
                        .await
                },
            )
            .await
            .map_err(map_email_flow_error)?;

        Ok(Response::new(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::models::UserId;

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
        let err =
            crate::impls::ApiError::ServiceUnavailable("publish key backend unavailable".into());
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
        let err =
            crate::impls::ApiError::ServiceUnavailable("livestream registry unavailable".into());
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
        let err =
            crate::impls::ApiError::ServiceUnavailable("password reset backend unavailable".into());
        let status = map_email_flow_error(err);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(status.message(), "password reset backend unavailable");
    }

    #[test]
    fn test_email_api_missing_maps_to_service_unavailable() {
        let err = ClientServiceImpl::email_api_unavailable_error();
        assert!(matches!(
            err.classify(),
            crate::impls::ErrorKind::ServiceUnavailable
        ));
        assert_eq!(
            err.message(),
            synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE
        );
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

    #[test]
    fn test_message_stream_buffer_size_reasonable() {
        // Buffer should be at least 10 and at most 1000
        const { assert!(MESSAGE_STREAM_BUFFER_SIZE >= 10) };
        const { assert!(MESSAGE_STREAM_BUFFER_SIZE <= 1000) };
    }

    #[test]
    fn test_map_message_stream_join_error_maps_capacity_to_resource_exhausted() {
        let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
            "realtime room capacity exceeded".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(status.message(), "realtime room capacity exceeded");
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_capacity_error() {
        let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
            "Room at capacity (42 connections, max: 40)".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            status.message(),
            "Room at capacity (42 connections, max: 40)"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_user_capacity_error() {
        let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
            "Too many connections for this user across all replicas (max 3)".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            status.message(),
            "Too many connections for this user across all replicas (max 3)"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_total_capacity_error() {
        let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
            "Server at capacity across all replicas (42 connections)".to_string(),
        ));
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
        let status = map_message_stream_join_error(RealtimeJoinError::ServiceUnavailable(
            "distributed room capacity check unavailable".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "distributed room capacity check unavailable"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_raw_degraded_cluster_error() {
        let status = map_message_stream_join_error(
            RealtimeJoinError::ServiceUnavailable(
                "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                    .to_string(),
            ),
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
            RealtimeJoinError::ServiceUnavailable(
                "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
                    .to_string(),
            ),
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
            RealtimeJoinError::ServiceUnavailable(
                "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
                    .to_string(),
            ),
        );
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            status.message(),
            "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_maps_business_denial_to_permission_denied() {
        let status = map_message_stream_join_error(RealtimeJoinError::PermissionDenied(
            "User is no longer allowed to use real-time messaging".to_string(),
        ));

        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert_eq!(
            status.message(),
            "User is no longer allowed to use real-time messaging"
        );
    }

    #[test]
    fn test_map_message_stream_join_error_hides_unexpected_internal_details() {
        let status = map_message_stream_join_error(RealtimeJoinError::Internal(
            "Connection 'conn123' is already registered".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "Failed to establish message stream");
    }
}
