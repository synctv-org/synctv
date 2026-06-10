use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use crate::impls::messaging::{
    GuestRealtimeIdentity, MessageSender, RealtimeJoinError, RealtimePrincipal,
    ResourceWatchSession, ResourceWatchSessionConfig,
};
use crate::runtime::RealtimeEventService;
use synctv_core::models::{Room, RoomId};
use synctv_core::service::{
    ContentFilter, RateLimitConfig, RequestRateLimiterService, RoomService as CoreRoomService,
    UserService as CoreUserService,
};
use synctv_proto::client::ServerMessage;
use synctv_realtime::sync::ConnectionRuntime;

use super::map_api_error;
use crate::impls::{ApiError, EndpointRateLimitCategory};

mod auth;
mod email;
mod public;
mod room;
mod streaming;
mod user;
use streaming::{GrpcMessageSender, WATCH_STREAM_BUFFER_SIZE};

fn map_message_stream_join_error(error: RealtimeJoinError) -> Status {
    error.log_if_internal("grpc_message_stream_pre_join");
    map_api_error(ApiError::from(error))
}

fn invalid_argument_status(message: impl Into<String>) -> Status {
    map_api_error(ApiError::InvalidInput(message.into()))
}

fn unauthenticated_status(message: impl Into<String>) -> Status {
    map_api_error(ApiError::Authentication(message.into()))
}

fn permission_denied_status(message: impl Into<String>) -> Status {
    map_api_error(ApiError::Authorization(message.into()))
}

#[cfg(test)]
fn unavailable_status(message: impl Into<String>) -> Status {
    map_api_error(ApiError::ServiceUnavailable(message.into()))
}

fn realtime_room_access_error(room: &Room) -> Option<Status> {
    if room.is_banned {
        return Some(permission_denied_status("This room has been banned"));
    }

    if room.status.is_closed() {
        return Some(permission_denied_status(
            "This room is closed and not accepting new connections",
        ));
    }

    None
}

fn map_message_stream_membership_error(err: synctv_core::Error) -> Status {
    map_api_error(crate::impls::ClientApiImpl::map_room_access_error(err))
}

fn map_email_flow_error(err: crate::impls::ApiError) -> Status {
    map_api_error(err)
}

fn map_message_stream_user_lookup_error(err: synctv_core::Error) -> Status {
    map_api_error(ApiError::from(err))
}

fn map_message_stream_room_lookup_error(err: synctv_core::Error) -> Status {
    map_api_error(ApiError::from(err))
}

/// Configuration for `ClientService`
#[derive(Clone)]
pub struct ClientServiceConfig {
    pub user_service: CoreUserService,
    pub room_service: CoreRoomService,
    pub chat_service: Arc<synctv_core::service::ChatService>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_service: Arc<dyn ConnectionRuntime>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
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
    event_service: Arc<dyn RealtimeEventService>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    connection_service: Arc<dyn ConnectionRuntime>,
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

    #[must_use]
    pub fn new(config: ClientServiceConfig) -> Self {
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

    fn request_metadata<T>(
        &self,
        request: &Request<T>,
    ) -> Result<crate::impls::RequestMetadata, Status> {
        super::request_metadata(
            request,
            &self.config,
            Some(super::grpc_unary_request_timeout()),
        )
    }

    fn extract_public_room_id_from_metadata(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<String, Status> {
        let room_id = request
            .metadata()
            .get("x-room-id")
            .ok_or_else(|| invalid_argument_status("Missing x-room-id header"))?
            .to_str()
            .map_err(|_| invalid_argument_status("Invalid x-room-id header"))?;

        self.client_api
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(|error| invalid_argument_status(format!("Invalid room_id: {error}")))?;

        Ok(room_id.to_string())
    }

    fn extract_room_id_from_metadata(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<RoomId, Status> {
        let room_id = self.extract_public_room_id_from_metadata(request)?;
        self.client_api
            .public_id_codec
            .decode_room_id(&room_id)
            .map_err(|error| invalid_argument_status(format!("Invalid room_id: {error}")))
    }

    fn room_request_context(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<(crate::impls::RequestMetadata, String), Status> {
        Ok((
            self.request_metadata(request)?,
            self.extract_public_room_id_from_metadata(request)?,
        ))
    }

    fn internal_room_request_context(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<(crate::impls::RequestMetadata, RoomId), Status> {
        Ok((
            self.request_metadata(request)?,
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
            .map_err(|_| unauthenticated_status("Invalid authorization header"))?;
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
        let client_api = self.client_api.clone();
        crate::impls::ClientApiImpl::execute_room_actor_endpoint(
            client_api,
            &metadata,
            public_room_id,
            category,
            operation,
        )
        .await
        .map_err(map_api_error)
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
        let client_api = self.client_api.clone();
        crate::impls::ClientApiImpl::execute_room_actor_endpoint_with_control(
            client_api,
            &metadata,
            public_room_id,
            category,
            operation,
        )
        .await
        .map_err(map_api_error)
    }

    async fn watch_principal(
        &self,
        metadata: &crate::impls::RequestMetadata,
        room_id: RoomId,
    ) -> Result<RealtimePrincipal, Status> {
        let executor = self.client_api.clone();
        if let Some(guest_token) =
            Self::extract_guest_token_from_authorization(metadata.authorization.as_deref())?
        {
            let public_room_id = self
                .client_api
                .public_id_codec
                .encode_room_id(room_id)
                .map_err(|error| invalid_argument_status(format!("Invalid room_id: {error}")))?;
            let client_api = self.client_api.clone();
            return executor
                .execute_public_endpoint(
                    metadata,
                    EndpointRateLimitCategory::WebSocket,
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
                        RealtimePrincipal::guest(room_id, identity)
                            .map_err(|error| crate::impls::ApiError::Internal(error.to_string()))
                    },
                )
                .await
                .map_err(map_api_error);
        }

        let user_id = executor
            .execute_user_endpoint(
                metadata,
                EndpointRateLimitCategory::WebSocket,
                move |authenticated| async move {
                    Ok::<_, crate::impls::ApiError>(authenticated.user_id)
                },
            )
            .await
            .map_err(map_api_error)?;
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(map_message_stream_user_lookup_error)?;
        Ok(RealtimePrincipal::user(user_id, user.username))
    }

    #[allow(clippy::result_large_err)]
    async fn open_watch_stream<E, F>(
        &self,
        metadata: crate::impls::RequestMetadata,
        room_id: RoomId,
        observe: synctv_proto::client::ObserveResource,
        map_event: F,
    ) -> Result<
        Response<
            std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<E, Status>> + Send + 'static>>,
        >,
        Status,
    >
    where
        E: Send + 'static,
        F: Fn(ServerMessage) -> Option<E> + Send + Sync + 'static,
    {
        let event_service = self.event_service.clone();
        let principal = self.watch_principal(&metadata, room_id).await?;

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(map_message_stream_room_lookup_error)?;
        if let Some(status) = realtime_room_access_error(&room) {
            return Err(status);
        }

        let (outgoing_tx, outgoing_rx) =
            tokio::sync::mpsc::channel::<ServerMessage>(WATCH_STREAM_BUFFER_SIZE);
        let sender = Arc::new(GrpcMessageSender::new(outgoing_tx));
        let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
            room_id,
            principal,
            room_service: self.room_service.clone(),
            chat_service: Some(self.chat_service.clone()),
            event_service,
            connection_service: self.connection_service.clone(),
            public_id_codec: self.client_api.public_id_codec.clone(),
            sender: Arc::clone(&sender) as Arc<dyn MessageSender>,
            playback_service: self.client_api.clone(),
            playlist_items_snapshot_service: self.client_api.clone(),
            room_members_snapshot_service: self.client_api.clone(),
            room_settings_snapshot_service:
                crate::impls::room_settings_snapshot::default_room_settings_snapshot_service(
                    self.room_service.clone(),
                ),
        });

        let prepared_session = session
            .prepare(&observe)
            .await
            .map_err(map_message_stream_join_error)?;
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let session_cancel = cancel_token.clone();
        tokio::spawn(async move {
            if let Err(error) = prepared_session.run(session_cancel).await {
                tracing::warn!(error = %error, "Resource watch session ended with error");
            }
        });

        let response_close_sender = sender.sender.clone();
        let response_close_token = cancel_token.clone();
        tokio::spawn(async move {
            response_close_sender.closed().await;
            response_close_token.cancel();
        });

        let output_stream = ReceiverStream::new(outgoing_rx).filter_map(move |message| {
            let event = map_event(message);
            event.map(Ok::<_, Status>)
        });

        Ok(Response::new(Box::pin(output_stream)))
    }
}

#[cfg(test)]
mod tests;
