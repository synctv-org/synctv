// Room management HTTP handlers
// Thin transport layer: delegates all business logic to the impls layer.
// Request and response types are proto-generated structs.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures::future::BoxFuture;
use futures::FutureExt;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use super::validation::ProtoQuery;
use super::websocket::RealtimeTransportFormat;
use super::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::messaging::{
    MessageSender, RealtimeJoinError, ResourceWatchSession, ResourceWatchSessionConfig,
};
use crate::impls::{ApiError, EndpointRateLimitCategory, EndpointRateLimitScope};
use crate::proto::client::{
    AddMediaBatchRequest, AddMediaRequest, AddMediaResponse, ChatMessageEventResponse,
    ChatReadStateResponse, CheckRoomResponse, ClearPlaylistRequest, ClearPlaylistResponse,
    ClearRoomPasswordRequest, CreateChatImageUploadSessionRequest,
    CreateChatImageUploadSessionResponse, CreatePlaylistCoverUploadSessionRequest,
    CreatePlaylistCoverUploadSessionResponse, CreatePlaylistRequest, CreatePlaylistResponse,
    CreateRoomCoverUploadSessionRequest, CreateRoomCoverUploadSessionResponse, CreateRoomRequest,
    CreateRoomResponse, CreateVideoCoverUploadSessionRequest,
    CreateVideoCoverUploadSessionResponse, DeleteChatMessageRequest, DeleteEntriesRequest,
    DeleteEntriesResponse, DeleteMediaQuery, DeleteMediaRequest, DeleteMediaResponse,
    DeletePlaylistQuery, DeletePlaylistRequest, DeletePlaylistResponse, DeleteRoomResponse,
    EditChatMessageRequest, EditMediaResponse, FinishRoomPasswordLoginRequest,
    FinishRoomPasswordRegistrationRequest, GetChatHistoryRequest, GetChatHistoryResponse,
    GetChatMessageContextRequest, GetChatMessageContextResponse, GetChatMessageRequest,
    GetChatMessageResponse, GetChatPlaybackMessagesRequest, GetChatPlaybackMessagesResponse,
    GetChatReadStateRequest, GetHotRoomsRequest, GetHotRoomsResponse, GetPlaybackRequest,
    GetPlaybackResponse, GetRoomMembersRequest, GetRoomMembersResponse, GetRoomResponse,
    GetRoomStreamInfoRequest, GetRoomStreamInfoResponse, JoinRoomResponse, KickRoomStreamRequest,
    KickRoomStreamResponse, LeaveRoomResponse, ListChatReactionUsersRequest,
    ListChatReactionUsersResponse, ListPlaylistItemsRequest, ListPlaylistsRequest,
    ListPlaylistsResponse, ListRoomStreamsRequest, ListRoomStreamsResponse, ListRoomsRequest,
    ListRoomsResponse, MarkChatReadRequest, MoveMediaRequest, MoveMediaResponse,
    MovePlaylistResponse, ResetRoomSettingsResponse, SendChatMessageRequest,
    SetChatReactionRequest, SetChatReactionResponse, SetRoomPasswordResponse, StartPlaybackRequest,
    StartPlaybackResponse, StartRoomPasswordLoginRequest, StartRoomPasswordLoginResponse,
    StartRoomPasswordRegistrationRequest, StartRoomPasswordRegistrationResponse,
    StopPlaybackRequest, StopPlaybackResponse, TransferRoomOwnershipRequest,
    TransferRoomOwnershipResponse, UpdatePlaybackRequest, UpdatePlaylistCoverRequest,
    UpdatePlaylistResponse, UpdateRoomCoverRequest, UpdateRoomSettingsResponse,
    UpdateVideoCoverRequest, WatchChatEventsRequest, WatchPlaybackSnapshotRequest,
    WatchPlaybackStateRequest, WatchPlaylistItemsRequest, WatchRoomMembersRequest,
    WatchRoomSettingsRequest,
};

pub type StartRoomPasswordRegistrationBody = StartRoomPasswordRegistrationRequest;
pub type FinishRoomPasswordRegistrationBody = FinishRoomPasswordRegistrationRequest;
pub type StartRoomPasswordLoginBody = StartRoomPasswordLoginRequest;
pub type FinishRoomPasswordLoginBody = FinishRoomPasswordLoginRequest;
pub type UpdateRoomSettingsBody = synctv_proto::http_serde::ClientUpdateRoomSettingsRequestDef;
pub type TransferRoomOwnershipBody = TransferRoomOwnershipRequest;
pub type StartPlaybackBody = StartPlaybackRequest;
pub type StopPlaybackBody = StopPlaybackRequest;
pub type AddMediaBody = AddMediaRequest;
pub type DeleteEntriesBody = DeleteEntriesRequest;
pub type ClearPlaylistBody = ClearPlaylistRequest;
pub type SendChatMessageBody = SendChatMessageRequest;
pub type CreateChatImageUploadSessionBody = CreateChatImageUploadSessionRequest;
pub type CreateVideoCoverUploadSessionBody = CreateVideoCoverUploadSessionRequest;
pub type UpdateVideoCoverBody = UpdateVideoCoverRequest;
pub type CreateRoomCoverUploadSessionBody = CreateRoomCoverUploadSessionRequest;
pub type UpdateRoomCoverBody = UpdateRoomCoverRequest;
pub type CreatePlaylistCoverUploadSessionBody = CreatePlaylistCoverUploadSessionRequest;
pub type UpdatePlaylistCoverBody = UpdatePlaylistCoverRequest;
pub type EditChatMessageBody = EditChatMessageRequest;
pub type DeleteChatMessageBody = DeleteChatMessageRequest;
pub type MarkChatReadBody = MarkChatReadRequest;
#[derive(Debug, serde::Deserialize)]
pub struct ChatMessagePath {
    pub room_id: String,
    pub message_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatReactionPath {
    pub room_id: String,
    pub message_id: String,
    pub reaction_key: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatImageObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatImageObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct VideoCoverObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoCoverObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct RoomCoverObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomCoverObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct PlaylistCoverObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaylistCoverObjectQuery {
    pub token: String,
}
#[derive(Debug, Default, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct KickRoomStreamBody {
    #[serde(default)]
    pub reason: String,
}

#[cfg(test)]
fn parse_optional_query_i32(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> AppResult<Option<i32>> {
    params
        .get(key)
        .map(|value| {
            value.parse::<i32>().map_err(|_| {
                super::AppError::bad_request(format!(
                    "Invalid {key} query parameter '{value}'. Expected an integer"
                ))
            })
        })
        .transpose()
}

#[cfg(test)]
fn parse_optional_query_bool(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> AppResult<Option<bool>> {
    params
        .get(key)
        .map(|value| {
            value.parse::<bool>().map_err(|_| {
                super::AppError::bad_request(format!(
                    "Invalid {key} query parameter '{value}'. Expected true or false"
                ))
            })
        })
        .transpose()
}

pub type AddMediaBatchBody = AddMediaBatchRequest;
pub type CreatePlaylistBody = CreatePlaylistRequest;
pub type UpdatePlaylistBody = crate::proto::client::UpdatePlaylistRequest;
pub type MovePlaylistBody = crate::proto::client::MovePlaylistRequest;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct GetPlaybackQuery {
    pub delivery_preference: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<String>,
    pub subtitle_preference: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchQuery {
    pub delivery_mode: Option<String>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchPlaybackSnapshotQuery {
    pub delivery_mode: Option<String>,
    pub format: Option<String>,
    pub after_event_sequence: Option<i64>,
    pub media_id: Option<String>,
    pub playlist_id: Option<String>,
    #[serde(default, with = "synctv_proto::http_serde::json_bytes")]
    pub target: Vec<u8>,
    pub delivery_preference: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<String>,
    pub subtitle_preference: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct RoomStreamPath {
    pub room_id: String,
    pub media_id: String,
}

struct HttpWatchMessageSender {
    sender: tokio::sync::mpsc::Sender<crate::proto::client::ServerMessage>,
}

impl MessageSender for HttpWatchMessageSender {
    fn send(&self, message: crate::proto::client::ServerMessage) -> Result<(), String> {
        self.sender.try_send(message).map_err(|error| match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                "SSE watch client is too slow to consume messages".to_string()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "SSE watch client disconnected".to_string()
            }
        })
    }

    fn is_alive(&self) -> bool {
        !self.sender.is_closed()
    }
}

fn map_resource_watch_prepare_error(error: RealtimeJoinError) -> super::AppError {
    error.log_if_internal("http_resource_watch_prepare");
    super::AppError::from(crate::impls::ApiError::from(error))
}

struct CancelOnDropStream<S> {
    inner: S,
    cancel_token: tokio_util::sync::CancellationToken,
}

impl<S> CancelOnDropStream<S> {
    fn new(inner: S, cancel_token: tokio_util::sync::CancellationToken) -> Self {
        Self {
            inner,
            cancel_token,
        }
    }
}

impl<S> Stream for CancelOnDropStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for CancelOnDropStream<S> {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

fn parse_watch_delivery_mode(value: Option<&str>) -> AppResult<i32> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("push_snapshot") => {
            Ok(crate::proto::client::ResourceDeliveryMode::PushSnapshot as i32)
        }
        Some("notify_only") => Ok(crate::proto::client::ResourceDeliveryMode::NotifyOnly as i32),
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid delivery_mode '{other}'. Expected push_snapshot or notify_only"
        ))),
    }
}

fn watch_after_event_sequence(
    headers: &HeaderMap,
    query_sequence: Option<i64>,
) -> AppResult<Option<i64>> {
    fn validate_sequence(sequence: i64) -> AppResult<i64> {
        if sequence < 0 {
            return Err(super::AppError::bad_request(
                "Invalid event sequence; expected a non-negative integer",
            ));
        }
        Ok(sequence)
    }

    let Some(header_value) = headers.get("last-event-id") else {
        return query_sequence.map(validate_sequence).transpose();
    };
    let header_value = header_value
        .to_str()
        .map_err(|_| super::AppError::bad_request("Invalid Last-Event-ID event sequence"))?
        .trim();

    if header_value.is_empty() {
        query_sequence.map(validate_sequence).transpose()
    } else {
        let sequence = header_value
            .parse::<i64>()
            .map_err(|_| super::AppError::bad_request("Invalid Last-Event-ID event sequence"))?;
        validate_sequence(sequence).map(Some)
    }
}

fn parse_delivery_preference(
    value: Option<&str>,
) -> Result<crate::proto::client::PlaybackDeliveryPreference, super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(crate::proto::client::PlaybackDeliveryPreference::Unspecified),
        Some("auto") => Ok(crate::proto::client::PlaybackDeliveryPreference::Auto),
        Some("direct_play") => Ok(crate::proto::client::PlaybackDeliveryPreference::DirectPlay),
        Some("transcode") => Ok(crate::proto::client::PlaybackDeliveryPreference::Transcode),
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid delivery_preference '{other}'. Expected auto, direct_play, or transcode"
        ))),
    }
}

fn parse_subtitle_preference(
    value: Option<&str>,
) -> Result<crate::proto::client::PlaybackSubtitlePreference, super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(crate::proto::client::PlaybackSubtitlePreference::Unspecified),
        Some("external") => Ok(crate::proto::client::PlaybackSubtitlePreference::External),
        Some("embedded_or_external") => {
            Ok(crate::proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal)
        }
        Some("none") => Ok(crate::proto::client::PlaybackSubtitlePreference::None),
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid subtitle_preference '{other}'. Expected external, embedded_or_external, or none"
        ))),
    }
}

fn parse_video_codecs(value: Option<&str>) -> Result<Vec<i32>, super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|codec| match codec {
            "h264" => Ok(crate::proto::client::PlaybackVideoCodec::H264 as i32),
            "hevc" => Ok(crate::proto::client::PlaybackVideoCodec::Hevc as i32),
            "vp9" => Ok(crate::proto::client::PlaybackVideoCodec::Vp9 as i32),
            "av1" => Ok(crate::proto::client::PlaybackVideoCodec::Av1 as i32),
            other => Err(super::AppError::bad_request(format!(
                "Invalid video codec '{other}'. Expected h264, hevc, vp9, or av1"
            ))),
        })
        .collect()
}

fn parse_containers(value: Option<&str>) -> Result<Vec<i32>, super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|container| match container {
            "mp4" => Ok(crate::proto::client::PlaybackContainer::Mp4 as i32),
            "mkv" => Ok(crate::proto::client::PlaybackContainer::Mkv as i32),
            "webm" => Ok(crate::proto::client::PlaybackContainer::Webm as i32),
            other => Err(super::AppError::bad_request(format!(
                "Invalid container '{other}'. Expected mp4, mkv, or webm"
            ))),
        })
        .collect()
}

fn parse_audio_capability(
    value: Option<&str>,
) -> Result<crate::proto::client::PlaybackAudioCapability, super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(crate::proto::client::PlaybackAudioCapability::Unspecified),
        Some("stereo") => Ok(crate::proto::client::PlaybackAudioCapability::Stereo),
        Some("surround") => Ok(crate::proto::client::PlaybackAudioCapability::Surround),
        Some("lossless_surround") => {
            Ok(crate::proto::client::PlaybackAudioCapability::LosslessSurround)
        }
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid audio_capability '{other}'. Expected stereo, surround, or lossless_surround"
        ))),
    }
}

fn build_get_playback_request(query: &GetPlaybackQuery) -> AppResult<GetPlaybackRequest> {
    let has_profile = query.delivery_preference.is_some()
        || query.max_streaming_bitrate.is_some()
        || query.max_audio_channels.is_some()
        || query.video_codecs.is_some()
        || query.containers.is_some()
        || query.audio_capability.is_some()
        || query.subtitle_preference.is_some();

    let playback_client_profile = if has_profile {
        Some(crate::proto::client::PlaybackClientProfile {
            delivery_preference: parse_delivery_preference(query.delivery_preference.as_deref())?
                as i32,
            max_streaming_bitrate: query.max_streaming_bitrate,
            max_audio_channels: query.max_audio_channels,
            supported_video_codecs: parse_video_codecs(query.video_codecs.as_deref())?,
            supported_containers: parse_containers(query.containers.as_deref())?,
            audio_capability: parse_audio_capability(query.audio_capability.as_deref())? as i32,
            subtitle_preference: parse_subtitle_preference(query.subtitle_preference.as_deref())?
                as i32,
        })
    } else {
        None
    };

    let request = GetPlaybackRequest {
        playback_client_profile,
    };
    Ok(request)
}

fn build_playback_client_profile_from_watch_query(
    query: &WatchPlaybackSnapshotQuery,
) -> AppResult<Option<crate::proto::client::PlaybackClientProfile>> {
    build_get_playback_request(&GetPlaybackQuery {
        delivery_preference: query.delivery_preference.clone(),
        max_streaming_bitrate: query.max_streaming_bitrate,
        max_audio_channels: query.max_audio_channels,
        video_codecs: query.video_codecs.clone(),
        containers: query.containers.clone(),
        audio_capability: query.audio_capability.clone(),
        subtitle_preference: query.subtitle_preference.clone(),
    })
    .map(|request| request.playback_client_profile)
}

fn encode_resource_watch_sse_data<M>(
    format: RealtimeTransportFormat,
    message: &M,
) -> Result<String, serde_json::Error>
where
    M: prost::Message + serde::Serialize,
{
    match format {
        RealtimeTransportFormat::Json => serde_json::to_string(message),
        RealtimeTransportFormat::Protobuf => Ok(BASE64_STANDARD.encode(message.encode_to_vec())),
    }
}

fn sse_event_from_server_message(
    format: RealtimeTransportFormat,
    message: crate::proto::client::ServerMessage,
) -> Option<Result<Event, Infallible>> {
    use crate::proto::client::server_message::Message;

    let (event_name, event_id, data) = match message.message? {
        Message::ResourceObserved(observed) => (
            "observed",
            None,
            encode_resource_watch_sse_data(format, &observed),
        ),
        Message::ResourceChanged(changed) => {
            let event_id = sse_event_id_from_resource_changed(&changed);
            (
                "changed",
                event_id,
                encode_resource_watch_sse_data(format, &changed),
            )
        }
        Message::ResourceObserveError(error) => (
            "error",
            None,
            encode_resource_watch_sse_data(format, &error),
        ),
        _ => return None,
    };
    let data = match data {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(error = %error, "Failed to serialize resource watch SSE event");
            return Some(Ok(Event::default()
                .event("error")
                .data(r#"{"message":"Failed to serialize resource watch event"}"#)));
        }
    };
    let mut event = Event::default().event(event_name).data(data);
    if let Some(event_id) = event_id {
        event = event.id(event_id);
    }
    Some(Ok(event))
}

fn sse_event_id_from_resource_changed(
    changed: &crate::proto::client::ResourceChanged,
) -> Option<String> {
    changed
        .event_cursor
        .as_ref()
        .map(|cursor| cursor.sequence.to_string())
        .or_else(|| {
            let Some(crate::proto::client::resource_changed::Payload::ChatEvent(event)) =
                changed.payload.as_ref()
            else {
                return None;
            };
            Some(event.sequence.to_string())
        })
}

async fn open_resource_watch_sse(
    state: AppState,
    request_meta: RequestMetadata,
    public_room_id: String,
    observe: crate::proto::client::ObserveResource,
    format: RealtimeTransportFormat,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let event_service = state.event_service.clone();
    let room_id = state
        .shared_api_runtime
        .public_id_codec
        .decode_room_id(&public_room_id)
        .map_err(|error| super::AppError::bad_request(format!("Invalid room_id: {error}")))?;
    let request_meta = request_metadata(request_meta).with_timeout(None);
    let principal = {
        let client_api = state.shared_api_runtime.client_api.clone();
        let user_service = state.user_service.clone();
        crate::impls::ClientApiImpl::execute_room_actor_endpoint(
            client_api,
            &request_meta,
            public_room_id,
            EndpointRateLimitCategory::WebSocket,
            move |_client_api, actor| async move {
                Ok::<_, crate::impls::ApiError>(match actor {
                    crate::impls::client::RoomActor::User { user_id, .. } => {
                        let username = user_service
                            .get_user(&user_id)
                            .await
                            .map_err(crate::impls::ApiError::from)?
                            .username;
                        crate::impls::messaging::RealtimePrincipal::user(user_id, username)
                    }
                    crate::impls::client::RoomActor::Guest(access) => {
                        let identity = crate::impls::messaging::GuestRealtimeIdentity {
                            guest_id: access.guest_id,
                            display_name: access.display_name,
                            session_id: access.session_id,
                            token_jti: access.token_jti,
                            room_guest_version: access.room_guest_version,
                            permissions: access.permissions,
                        };
                        crate::impls::messaging::RealtimePrincipal::guest(room_id, identity)
                            .map_err(|error| crate::impls::ApiError::Internal(error.to_string()))?
                    }
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?
    };

    let (outgoing_tx, outgoing_rx) =
        tokio::sync::mpsc::channel::<crate::proto::client::ServerMessage>(64);
    let sender = Arc::new(HttpWatchMessageSender {
        sender: outgoing_tx,
    });
    let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
        room_id,
        principal,
        room_service: state.room_service.clone(),
        chat_service: state.chat_service.clone(),
        event_service,
        connection_service: state.connection_manager.clone(),
        public_id_codec: state.shared_api_runtime.public_id_codec.clone(),
        sender,
        playback_snapshot_service: Some(state.shared_api_runtime.client_api.clone()),
        playlist_items_snapshot_service: Some(state.shared_api_runtime.client_api.clone()),
        room_members_snapshot_service: Some(state.shared_api_runtime.client_api.clone()),
        room_settings_snapshot_service: None,
    });
    let prepared_session = session
        .prepare(&observe)
        .await
        .map_err(map_resource_watch_prepare_error)?;
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let session_cancel = cancel_token.clone();
    tokio::spawn(async move {
        if let Err(error) = prepared_session.run(session_cancel).await {
            tracing::warn!(error = %error, "HTTP resource watch session ended with error");
        }
    });

    let stream = ReceiverStream::new(outgoing_rx)
        .filter_map(move |message| sse_event_from_server_message(format, message));
    let stream = CancelOnDropStream::new(stream, cancel_token);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn extract_room_id(path: crate::proto::client::RoomPathRequest) -> String {
    path.room_id
}

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

fn execute_public_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(std::sync::Arc<crate::impls::ClientApiImpl>) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let executor = state.shared_api_runtime.client_api.clone();
        let client_api = state.shared_api_runtime.client_api.clone();
        executor
            .execute_scoped_public_endpoint(&request_meta, category, scope, move || {
                operation(client_api)
            })
            .await
            .map_err(super::error::map_api_error)
    }
    .boxed()
}

fn execute_user_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(
            std::sync::Arc<crate::impls::ClientApiImpl>,
            synctv_core::service::AuthenticatedToken,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let executor = state.shared_api_runtime.client_api.clone();
        let client_api = state.shared_api_runtime.client_api.clone();
        executor
            .execute_scoped_user_endpoint(&request_meta, category, scope, move |authenticated| {
                operation(client_api, authenticated)
            })
            .await
            .map_err(super::error::map_api_error)
    }
    .boxed()
}

pub(super) fn execute_room_actor_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    public_room_id: String,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(std::sync::Arc<crate::impls::ClientApiImpl>, crate::impls::client::RoomActor) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        crate::impls::ClientApiImpl::execute_scoped_room_actor_endpoint(
            client_api,
            &request_meta,
            public_room_id,
            category,
            scope,
            operation,
        )
        .await
        .map_err(super::error::map_api_error)
    }
    .boxed()
}

fn execute_room_actor_endpoint_with_control<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    public_room_id: String,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(
            std::sync::Arc<crate::impls::ClientApiImpl>,
            synctv_core::provider::ExecutionControl,
            crate::impls::client::RoomActor,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        crate::impls::ClientApiImpl::execute_scoped_room_actor_endpoint_with_control(
            client_api,
            &request_meta,
            public_room_id,
            category,
            scope,
            operation,
        )
        .await
        .map_err(super::error::map_api_error)
    }
    .boxed()
}

/// Create a new room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms",
        tag = "Room",
        request_body = CreateRoomRequest,
        responses(
            (status = 200, description = "Room created", body = CreateRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<CreateRoomRequest>,
) -> AppResult<Json<CreateRoomResponse>> {
    tracing::info!(room_name = %req.name, "Creating new room");

    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCreate,
        move |client_api, authenticated| async move {
            client_api.create_room(&authenticated.user_id, req).await
        },
    )
    .await?;

    tracing::info!(
        room_id = response
            .room
            .as_ref()
            .map_or("unknown", |room| room.id.as_str()),
        "Room created successfully"
    );
    Ok(Json(response))
}

/// Get room information
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room details", body = GetRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<GetRoomResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id.clone(),
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomGet,
        move |client_api, actor| async move { client_api.get_room_for_actor(&actor).await },
    )
    .await?;

    Ok(Json(response))
}

/// Join a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{room_id}/members/@me",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = crate::proto::client::JoinRoomRequest,
        responses(
            (status = 200, description = "Joined room", body = JoinRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn join_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(mut req): Json<crate::proto::client::JoinRoomRequest>,
) -> AppResult<Json<JoinRoomResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let room_id = extract_room_id(path);
    req.room_id = room_id.clone();
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_scoped_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomJoin,
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
        .await?;

    Ok(Json(response))
}

/// Start room password OPAQUE login
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/password/opaque/login/start",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StartRoomPasswordLoginRequest,
        responses(
            (status = 200, description = "Room password login challenge created", body = StartRoomPasswordLoginResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_room_password_login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(mut req): Json<StartRoomPasswordLoginBody>,
) -> AppResult<Json<StartRoomPasswordLoginResponse>> {
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let room_id = extract_room_id(path);
    req.room_id = room_id;
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_scoped_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomJoin,
            move |request_control, authenticated| async move {
                client_api
                    .start_room_password_login_with_control(
                        &authenticated.user_id,
                        req,
                        client_ip.as_deref(),
                        Some(&request_control),
                    )
                    .await
            },
        )
        .await?;

    Ok(Json(response))
}

/// Finish room password OPAQUE login and join the room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/password/opaque/login/finish",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = FinishRoomPasswordLoginRequest,
        responses(
            (status = 200, description = "Joined room", body = JoinRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_room_password_login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<FinishRoomPasswordLoginBody>,
) -> AppResult<Json<JoinRoomResponse>> {
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let room_id = extract_room_id(path);
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_scoped_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomJoin,
            move |_request_control, authenticated| async move {
                client_api
                    .finish_room_password_login_with_control(
                        &authenticated.user_id,
                        Some(&room_id),
                        req,
                        client_ip.as_deref(),
                    )
                    .await
            },
        )
        .await?;

    Ok(Json(response))
}

/// Leave a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/members/@me",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Left room", body = LeaveRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn leave_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<LeaveRoomResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomJoin,
        move |client_api, authenticated| async move {
            client_api
                .leave_room(&authenticated.user_id, &room_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Delete a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room deleted", body = DeleteRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<DeleteRoomResponse>> {
    let room_id = extract_room_id(path);
    tracing::info!(room_id = %room_id, "Deleting room");
    let room_id_for_log = room_id.clone();

    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCreate,
        move |client_api, authenticated| async move {
            client_api
                .delete_room(&authenticated.user_id, &room_id)
                .await
        },
    )
    .await?;

    tracing::info!(room_id = %room_id_for_log, "Room deleted successfully");
    Ok(Json(response))
}

/// Add media to playlist
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = AddMediaRequest,
        responses(
            (status = 200, description = "Media added", body = AddMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn add_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<AddMediaBody>,
) -> AppResult<Json<AddMediaResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .add_media(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Delete media from playlist
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/media/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID"),
            ("force" = Option<bool>, Query, description = "Force delete")
        ),
        responses(
            (status = 200, description = "Media deleted", body = DeleteMediaResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Media not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
    ProtoQuery(query): ProtoQuery<DeleteMediaQuery>,
) -> AppResult<Json<DeleteMediaResponse>> {
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let proto_req = DeleteMediaRequest {
        media_id,
        force: query.force,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .delete_media(&authenticated.user_id, &room_id, proto_req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Delete a mixed set of playlist and media entries.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/entries",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = DeleteEntriesRequest,
        responses(
            (status = 200, description = "Entries deleted", body = DeleteEntriesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_entries(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<DeleteEntriesBody>,
) -> AppResult<Json<DeleteEntriesResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .delete_entries(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Move a media item relative to a sibling.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/move",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = MoveMediaRequest,
        responses(
            (status = 200, description = "Media moved", body = MoveMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn move_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<MoveMediaRequest>,
) -> AppResult<Json<MoveMediaResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .move_media(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// List items for a room root, static playlist, or dynamic playlist target.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/list",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = ListPlaylistItemsRequest,
        responses(
            (status = 200, description = "Playlist items", body = crate::proto::client::ListPlaylistItemsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_playlist_items(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<ListPlaylistItemsRequest>,
) -> AppResult<Json<crate::proto::client::ListPlaylistItemsResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id.clone(),
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomPlaylist,
        move |client_api, actor| async move {
            client_api.list_playlist_items_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Play (resume playback)
/// POST /`api/rooms/{room_id}/playback/start` - Start playback of a specific media
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playback/start",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StartPlaybackRequest,
        responses(
            (status = 200, description = "Playback started", body = StartPlaybackResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<StartPlaybackBody>,
) -> AppResult<Json<StartPlaybackResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .start_playback(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// POST /`api/rooms/{room_id}/playback/stop` - Stop current playback
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playback/stop",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StopPlaybackRequest,
        responses(
            (status = 200, description = "Playback stopped", body = StopPlaybackResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn stop_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<StopPlaybackBody>,
) -> AppResult<Json<StopPlaybackResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .stop_playback(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// GET /`api/rooms/{room_id}/playback` - Get current playback state and complete playback information
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playback",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            GetPlaybackQuery
        ),
        responses(
            (status = 200, description = "Current playback state", body = GetPlaybackResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(query): ProtoQuery<GetPlaybackQuery>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = extract_room_id(path);
    let req = build_get_playback_request(&query)?;
    let response = execute_room_actor_endpoint_with_control(
        &state,
        request_meta,
        room_id.clone(),
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, request_control, actor| async move {
            client_api
                .get_playback_for_actor(&actor, req, Some(&request_control))
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn watch_playback_state(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = extract_room_id(path);
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchPlaybackStateRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        playback_state: Some(crate::proto::client::ObservePlaybackState {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_playback_state_observe(request)
        .map_err(super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_playback_snapshot(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchPlaybackSnapshotQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = extract_room_id(path);
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let playback_client_profile = build_playback_client_profile_from_watch_query(&query)?;
    let request = WatchPlaybackSnapshotRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        playback_snapshot: Some(crate::proto::client::ObservePlaybackSnapshot {
            media_id: query.media_id,
            playlist_id: query.playlist_id,
            target: query.target,
            playback_client_profile,
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_playback_snapshot_observe(request)
        .map_err(super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = extract_room_id(path);
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchRoomSettingsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        room_settings: Some(crate::proto::client::ObserveRoomSettings {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_room_settings_observe(request)
        .map_err(super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_playlist_items(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
    ProtoQuery(request): ProtoQuery<ListPlaylistItemsRequest>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = extract_room_id(path);
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchPlaylistItemsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        playlist_items: Some(crate::proto::client::ObservePlaylistItems {
            request: Some(request),
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_playlist_items_observe(request)
        .map_err(super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_room_members(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
    ProtoQuery(request): ProtoQuery<GetRoomMembersRequest>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = extract_room_id(path);
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchRoomMembersRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        room_members: Some(crate::proto::client::ObserveRoomMembers {
            request: Some(request),
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_room_members_observe(request)
        .map_err(super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/watch/chat-events",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("format" = Option<String>, Query, description = "SSE payload format: json or protobuf"),
            ("after_event_sequence" = Option<i64>, Query, description = "Replay chat events strictly after this durable event sequence"),
            ("delivery_mode" = Option<String>, Query, description = "Resource watch delivery mode")
        ),
        responses(
            (status = 200, description = "SSE stream of chat resource events"),
            (status = 400, description = "Invalid request or event cursor", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Realtime manager unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn watch_chat_events(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = extract_room_id(path);
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchChatEventsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        chat_events: Some(crate::proto::client::ObserveChatEvents {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_chat_events_observe(request)
        .map_err(super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

/// Get room members with pagination.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/members",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            GetRoomMembersRequest
        ),
        responses(
            (status = 200, description = "Room members", body = GetRoomMembersResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_members(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetRoomMembersRequest>,
) -> AppResult<Json<GetRoomMembersResponse>> {
    let room_id = extract_room_id(path);
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id.clone(),
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomMembers,
            move |client_api, actor| async move {
                client_api.get_room_members_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/streams",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ListRoomStreamsRequest
        ),
        responses(
            (status = 200, description = "Active room live streams", body = ListRoomStreamsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_room_streams(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListRoomStreamsRequest>,
) -> AppResult<Json<ListRoomStreamsResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .list_room_streams(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/streams/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Room live stream information", body = GetRoomStreamInfoResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Stream not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_stream_info(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomStreamPath>,
) -> AppResult<Json<GetRoomStreamInfoResponse>> {
    let room_id = path.room_id;
    let req = GetRoomStreamInfoRequest {
        media_id: path.media_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .get_room_stream_info(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/streams/{media_id}/kick",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        request_body = KickRoomStreamBody,
        responses(
            (status = 200, description = "Room live stream kicked", body = KickRoomStreamResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Stream not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn kick_room_stream(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomStreamPath>,
    Json(req): Json<KickRoomStreamBody>,
) -> AppResult<Json<KickRoomStreamResponse>> {
    let room_id = path.room_id;
    let req = KickRoomStreamRequest {
        media_id: path.media_id,
        reason: req.reason,
    };
    execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .kick_room_stream(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(KickRoomStreamResponse {}))
}

/// Check if room exists (public endpoint)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/check",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room availability and status; exists=false when the room is not found", body = CheckRoomResponse),
            (status = 400, description = "Invalid room ID", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn check_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<crate::proto::client::CheckRoomRequest>,
) -> AppResult<Json<CheckRoomResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomGet,
        move |client_api| async move { client_api.check_room(req).await },
    )
    .await?;

    Ok(Json(response))
}

/// Start room password registration
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/password/opaque/registration/start",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StartRoomPasswordRegistrationRequest,
        responses(
            (status = 200, description = "Room password registration challenge created", body = StartRoomPasswordRegistrationResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_room_password_registration(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<StartRoomPasswordRegistrationBody>,
) -> AppResult<Json<StartRoomPasswordRegistrationResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomPassword,
        move |client_api, authenticated| async move {
            client_api
                .start_room_password_registration(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Finish room password OPAQUE registration
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/password/opaque/registration/finish",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = FinishRoomPasswordRegistrationRequest,
        responses(
            (status = 200, description = "Room password updated", body = SetRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_room_password_registration(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<FinishRoomPasswordRegistrationBody>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomPassword,
        move |client_api, authenticated| async move {
            client_api
                .finish_room_password_registration(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Clear room password
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/password",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room password cleared", body = SetRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn clear_room_password(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomPassword,
        move |client_api, authenticated| async move {
            client_api
                .clear_room_password(
                    &authenticated.user_id,
                    &room_id,
                    ClearRoomPasswordRequest {},
                )
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Get room settings (requires authentication and room membership)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/settings",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room settings", body = crate::proto::client::GetRoomSettingsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<crate::proto::client::GetRoomSettingsResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id.clone(),
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, actor| async move { client_api.get_room_settings_for_actor(&actor).await },
    )
    .await?;

    Ok(Json(response))
}

/// Push multiple media items to playlist
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/batch",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = AddMediaBatchRequest,
        responses(
            (status = 200, description = "Batch media added", body = crate::proto::client::AddMediaBatchResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn push_media_batch(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<AddMediaBatchBody>,
) -> AppResult<Json<crate::proto::client::AddMediaBatchResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .add_media_batch(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Edit media
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/media/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        request_body = crate::proto::client::EditMediaRequest,
        responses(
            (status = 200, description = "Media updated", body = EditMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn edit_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<crate::proto::client::EditMediaRequest>,
) -> AppResult<Json<EditMediaResponse>> {
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .edit_media(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn create_video_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<CreateVideoCoverUploadSessionBody>,
) -> AppResult<Json<CreateVideoCoverUploadSessionResponse>> {
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.room_id = room_id.clone();
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaCover,
        move |client_api, authenticated| async move {
            client_api
                .create_video_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_video_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<UpdateVideoCoverBody>,
) -> AppResult<Json<EditMediaResponse>> {
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.room_id = room_id.clone();
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaCover,
        move |client_api, authenticated| async move {
            client_api
                .update_video_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_video_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<EditMediaResponse>> {
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let req = crate::proto::client::ClearVideoCoverRequest {
        room_id: room_id.clone(),
        media_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaCover,
        move |client_api, authenticated| async move {
            client_api
                .clear_video_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn create_room_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<CreateRoomCoverUploadSessionBody>,
) -> AppResult<Json<CreateRoomCoverUploadSessionResponse>> {
    req.room_id = room_id.clone();
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api, authenticated| async move {
            client_api
                .create_room_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_room_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<UpdateRoomCoverBody>,
) -> AppResult<Json<GetRoomResponse>> {
    req.room_id = room_id.clone();
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api, authenticated| async move {
            client_api
                .update_room_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_room_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<GetRoomResponse>> {
    let req = crate::proto::client::ClearRoomCoverRequest {
        room_id: room_id.clone(),
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api, authenticated| async move {
            client_api
                .clear_room_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn create_playlist_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    Json(mut req): Json<CreatePlaylistCoverUploadSessionBody>,
) -> AppResult<Json<CreatePlaylistCoverUploadSessionResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    req.room_id = room_id.clone();
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api, authenticated| async move {
            client_api
                .create_playlist_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_playlist_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    Json(mut req): Json<UpdatePlaylistCoverBody>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    req.room_id = room_id.clone();
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api, authenticated| async move {
            client_api
                .update_playlist_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_playlist_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = crate::proto::client::ClearPlaylistCoverRequest {
        room_id: room_id.clone(),
        playlist_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api, authenticated| async move {
            client_api
                .clear_playlist_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Clear playlist
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/media",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = ClearPlaylistRequest,
        responses(
            (status = 200, description = "Playlist cleared", body = ClearPlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn clear_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<ClearPlaylistRequest>,
) -> AppResult<Json<ClearPlaylistResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .clear_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// GET /`api/rooms/:room_id/media/:media_id` - Get media record from database
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/media/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Media details", body = crate::proto::client::Media),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Media not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::Media>> {
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let media =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id.clone(),
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomGet,
            move |client_api, actor| async move {
                client_api.get_media_for_actor(&actor, &media_id).await
            },
        )
        .await?;

    Ok(Json(media))
}

/// GET /`api/rooms/:room_id/playlists/:playlist_id` - Get single playlist info
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID")
        ),
        responses(
            (status = 200, description = "Playlist details", body = crate::proto::client::GetPlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Playlist not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::GetPlaylistResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id.clone(),
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomGet,
        move |client_api, actor| async move {
            client_api
                .get_playlist_for_actor(&actor, &playlist_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Unified handler for listing rooms (with query params) or getting single room by ID
/// GET /api/rooms (list) or GET /api/rooms?id=xxx (single)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms",
        tag = "Room",
        params(ListRoomsRequest),
        responses(
            (status = 200, description = "Rooms list", body = ListRoomsResponse),
            (status = 400, description = "Invalid query", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn list_or_get_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<ListRoomsRequest>,
) -> AppResult<Json<ListRoomsResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomList,
        move |client_api| async move { client_api.list_rooms(req).await },
    )
    .await?;

    Ok(Json(response))
}

/// Unified handler for updating room settings via PATCH
/// PATCH /`api/rooms/:room_id/settings`
///
/// PATCH semantics: only specified fields are updated; unspecified fields retain
/// their current values. Current settings are fetched first, then merged.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/settings",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = synctv_proto::http_serde::ClientUpdateRoomSettingsRequestDef,
        responses(
            (status = 200, description = "Room settings updated", body = UpdateRoomSettingsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<UpdateRoomSettingsBody>,
) -> AppResult<Json<UpdateRoomSettingsResponse>> {
    let room_id = extract_room_id(path);
    let req = crate::proto::client::UpdateRoomSettingsRequest::from(req);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, authenticated| async move {
            client_api
                .update_room_settings(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/owner",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = TransferRoomOwnershipRequest,
        responses(
            (status = 200, description = "Room ownership transferred", body = TransferRoomOwnershipResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn transfer_room_ownership(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<TransferRoomOwnershipBody>,
) -> AppResult<Json<TransferRoomOwnershipResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, authenticated| async move {
            client_api
                .transfer_room_ownership(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Unified handler for updating playback state via PATCH
/// PATCH /`api/rooms/:room_id/playback`
/// Supports play/pause/seek/speed state updates. Playback target changes are
/// handled by start/stop endpoints.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/playback",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = UpdatePlaybackRequest,
        responses(
            (status = 200, description = "Playback updated", body = GetPlaybackResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<UpdatePlaybackRequest>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .update_playback(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}

/// Reset room settings to defaults
/// POST /`api/rooms/:room_id/settings/reset`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/settings/reset",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room settings reset", body = ResetRoomSettingsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn reset_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<ResetRoomSettingsResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, authenticated| async move {
            client_api
                .reset_room_settings(&authenticated.user_id, &room_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Get chat history for a room
/// GET /`api/rooms/:room_id/chat/history`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/history",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            GetChatHistoryRequest
        ),
        responses(
            (status = 200, description = "Chat history", body = GetChatHistoryResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_history(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetChatHistoryRequest>,
) -> AppResult<Json<GetChatHistoryResponse>> {
    let room_id = extract_room_id(path);
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id.clone(),
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.get_chat_history_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("include_deleted" = Option<bool>, Query, description = "Include soft-deleted message metadata when allowed")
        ),
        responses(
            (status = 200, description = "Chat message", body = GetChatMessageResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Insufficient room permission", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    ProtoQuery(mut req): ProtoQuery<GetChatMessageRequest>,
) -> AppResult<Json<GetChatMessageResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.get_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/context",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Anchor chat message ID"),
            ("before_limit" = Option<i32>, Query, description = "Messages before anchor"),
            ("after_limit" = Option<i32>, Query, description = "Messages after anchor"),
            ("include_deleted" = Option<bool>, Query, description = "Include soft-deleted messages when allowed")
        ),
        responses(
            (status = 200, description = "Chat message context", body = GetChatMessageContextResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Insufficient room permission", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_message_context(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    ProtoQuery(mut req): ProtoQuery<GetChatMessageContextRequest>,
) -> AppResult<Json<GetChatMessageContextResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .get_chat_message_context_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/playback-messages",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playback_media_id" = Option<String>, Query, description = "Playback media ID"),
            ("playback_playlist_id" = Option<String>, Query, description = "Playback playlist ID"),
            ("playback_target" = Option<Vec<u8>>, Query, description = "Playback target bytes"),
            ("position_seconds" = Option<f64>, Query, description = "Playback position in seconds"),
            ("before_seconds" = Option<f64>, Query, description = "Seconds before position"),
            ("after_seconds" = Option<f64>, Query, description = "Seconds after position"),
            ("limit" = Option<i32>, Query, description = "Maximum messages to return"),
            ("include_deleted" = Option<bool>, Query, description = "Include deleted messages")
        ),
        responses(
            (status = 200, description = "Chat messages around playback position", body = GetChatPlaybackMessagesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Insufficient room permission", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_playback_messages(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetChatPlaybackMessagesRequest>,
) -> AppResult<Json<GetChatPlaybackMessagesResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .get_chat_playback_messages_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/chat/messages",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = SendChatMessageBody,
        responses(
            (status = 200, description = "Chat message event", body = ChatMessageEventResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Chat disabled or insufficient permission", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn send_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<SendChatMessageBody>,
) -> AppResult<Json<ChatMessageEventResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.send_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/chat/images/upload-session",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = CreateChatImageUploadSessionBody,
        responses(
            (status = 200, description = "Chat image upload session", body = CreateChatImageUploadSessionResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Insufficient room permission", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_chat_image_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<CreateChatImageUploadSessionBody>,
) -> AppResult<Json<CreateChatImageUploadSessionResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .create_chat_image_upload_session_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn upload_chat_image_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatImageObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let encoded_object_key = path.encoded_object_key;
    let upload_token = upload_token.to_string();
    let content_type = content_type.map(str::to_string);
    let data = body.to_vec();
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move {
            let chat_service = client_api.chat_service.as_ref().ok_or_else(|| {
                crate::impls::ApiError::ServiceUnavailable(
                    "Chat service is unavailable".to_string(),
                )
            })?;
            chat_service
                .store_image_upload_object(
                    &encoded_object_key,
                    &upload_token,
                    content_type.as_deref(),
                    data,
                )
                .await
                .map(|_| ())
                .map_err(crate::impls::ApiError::from)
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_chat_image_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatImageObjectPath>,
    Query(query): Query<ChatImageObjectQuery>,
) -> AppResult<Response> {
    let encoded_object_key = path.encoded_object_key;
    let token = query.token;
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move {
            let chat_service = client_api.chat_service.as_ref().ok_or_else(|| {
                crate::impls::ApiError::ServiceUnavailable(
                    "Chat service is unavailable".to_string(),
                )
            })?;
            chat_service
                .get_image_object(&encoded_object_key, &token)
                .await
                .map_err(crate::impls::ApiError::from)
        },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

pub async fn upload_video_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<VideoCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let req = crate::proto::client::UploadVideoCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        data: body.to_vec(),
    };
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.upload_video_cover_object(req).await },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_video_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<VideoCoverObjectPath>,
    Query(query): Query<VideoCoverObjectQuery>,
) -> AppResult<Response> {
    let req = crate::proto::client::GetVideoCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
    };
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move { client_api.get_video_cover_object(req).await },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

pub async fn upload_room_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let req = crate::proto::client::UploadRoomCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        data: body.to_vec(),
    };
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.upload_room_cover_object(req).await },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_room_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    Query(query): Query<RoomCoverObjectQuery>,
) -> AppResult<Response> {
    let req = crate::proto::client::GetRoomCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
    };
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.get_room_cover_object(req).await },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

pub async fn upload_playlist_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let req = crate::proto::client::UploadPlaylistCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        data: body.to_vec(),
    };
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api| async move { client_api.upload_playlist_cover_object(req).await },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_playlist_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    Query(query): Query<PlaylistCoverObjectQuery>,
) -> AppResult<Response> {
    let req = crate::proto::client::GetPlaylistCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
    };
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.get_playlist_cover_object(req).await },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID")
        ),
        request_body = EditChatMessageBody,
        responses(
            (status = 200, description = "Chat message edited event", body = ChatMessageEventResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Only the sender can edit this message", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc),
            (status = 409, description = "Optimistic lock conflict", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn edit_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    Json(mut req): Json<EditChatMessageBody>,
) -> AppResult<Json<ChatMessageEventResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.edit_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID")
        ),
        request_body = DeleteChatMessageBody,
        responses(
            (status = 200, description = "Chat message deleted event", body = ChatMessageEventResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Sender or DELETE_CHAT permission required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc),
            (status = 409, description = "Optimistic lock conflict", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    Json(mut req): Json<DeleteChatMessageBody>,
) -> AppResult<Json<ChatMessageEventResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.delete_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("reaction_key" = String, Path, description = "Reaction key, for example like or an emoji")
        ),
        responses(
            (status = 200, description = "Chat reaction changed event", body = SetChatReactionResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn set_chat_reaction(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatReactionPath>,
) -> AppResult<Json<SetChatReactionResponse>> {
    let req = SetChatReactionRequest {
        message_id: path.message_id,
        reaction_key: path.reaction_key,
        enabled: true,
    };
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.set_chat_reaction_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("reaction_key" = String, Path, description = "Reaction key, for example like or an emoji")
        ),
        responses(
            (status = 200, description = "Chat reaction changed event", body = SetChatReactionResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn clear_chat_reaction(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatReactionPath>,
) -> AppResult<Json<SetChatReactionResponse>> {
    let req = SetChatReactionRequest {
        message_id: path.message_id,
        reaction_key: path.reaction_key,
        enabled: false,
    };
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.set_chat_reaction_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}/users",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("reaction_key" = String, Path, description = "Reaction key, for example like or an emoji"),
            ListChatReactionUsersRequest
        ),
        responses(
            (status = 200, description = "Users who reacted to the chat message", body = ListChatReactionUsersResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_chat_reaction_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatReactionPath>,
    ProtoQuery(mut req): ProtoQuery<ListChatReactionUsersRequest>,
) -> AppResult<Json<ListChatReactionUsersResponse>> {
    req.message_id = path.message_id;
    req.reaction_key = path.reaction_key;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .list_chat_reaction_users_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/chat/read-state",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = MarkChatReadBody,
        responses(
            (status = 200, description = "Chat read state", body = ChatReadStateResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Message not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn mark_chat_read(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<MarkChatReadBody>,
) -> AppResult<Json<ChatReadStateResponse>> {
    let room_id = extract_room_id(path);
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.mark_chat_read_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/read-state",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Chat read state", body = ChatReadStateResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_read_state(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetChatReadStateRequest>,
) -> AppResult<Json<ChatReadStateResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.get_chat_read_state_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Create a playlist
/// POST /`api/rooms/:room_id/playlists`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playlists",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = CreatePlaylistRequest,
        responses(
            (status = 200, description = "Playlist created", body = CreatePlaylistResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<CreatePlaylistBody>,
) -> AppResult<Json<CreatePlaylistResponse>> {
    let room_id = extract_room_id(path);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .create_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Update a playlist
/// PATCH /`api/rooms/:room_id/playlists/:playlist_id`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID")
        ),
        request_body = crate::proto::client::UpdatePlaylistRequest,
        responses(
            (status = 200, description = "Playlist updated", body = UpdatePlaylistResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    Json(mut req): Json<crate::proto::client::UpdatePlaylistRequest>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .update_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}/move",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID")
        ),
        request_body = synctv_proto::http_serde::MovePlaylistRequestDef,
        responses(
            (status = 200, description = "Playlist moved", body = MovePlaylistResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn move_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    Json(req): Json<synctv_proto::http_serde::MovePlaylistRequestDef>,
) -> AppResult<Json<MovePlaylistResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let mut req =
        crate::proto::client::MovePlaylistRequest::try_from(req).map_err(ApiError::InvalidInput)?;
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlaylist,
        move |client_api, authenticated| async move {
            client_api
                .move_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Delete a playlist
/// DELETE /`api/rooms/:room_id/playlists/:playlist_id`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID"),
            ("force" = Option<bool>, Query, description = "Force delete")
        ),
        responses(
            (status = 200, description = "Playlist deleted", body = DeletePlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Playlist not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    ProtoQuery(query): ProtoQuery<DeletePlaylistQuery>,
) -> AppResult<Json<DeletePlaylistResponse>> {
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = DeletePlaylistRequest {
        playlist_id,
        force: query.force,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlaylist,
        move |client_api, authenticated| async move {
            client_api
                .delete_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// List playlists in a room
/// GET /`api/rooms/:room_id/playlists`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playlists",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ListPlaylistsRequest
        ),
        responses(
            (status = 200, description = "Playlists in room", body = ListPlaylistsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_playlists(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListPlaylistsRequest>,
) -> AppResult<Json<ListPlaylistsResponse>> {
    let room_id = extract_room_id(path);
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id.clone(),
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomPlaylist,
            move |client_api, actor| async move {
                client_api.list_playlists_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

/// Get hot rooms (sorted by online count)
/// GET /api/rooms/hot
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/hot",
        tag = "Room",
        params(GetHotRoomsRequest),
        responses(
            (status = 200, description = "Hot rooms", body = GetHotRoomsResponse),
            (status = 400, description = "Invalid query", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn get_hot_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<GetHotRoomsRequest>,
) -> AppResult<Json<GetHotRoomsResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomList,
        move |client_api| async move { client_api.get_hot_rooms(req).await },
    )
    .await?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        build_get_playback_request, parse_optional_query_bool, parse_optional_query_i32,
        sse_event_from_server_message, sse_event_id_from_resource_changed,
        watch_after_event_sequence, AddMediaBatchBody, CancelOnDropStream, ChatImageObjectQuery,
        CreatePlaylistBody, DeleteEntriesBody, GetPlaybackQuery, PlaylistCoverObjectQuery,
        RoomCoverObjectQuery, UpdatePlaybackRequest, VideoCoverObjectQuery,
        WatchPlaybackSnapshotQuery, WatchQuery,
    };
    use crate::proto::client::{
        DeleteMediaQuery, DeletePlaylistQuery, GetChatHistoryRequest, GetChatMessageContextRequest,
        GetChatMessageRequest, GetHotRoomsRequest, GetRoomMembersRequest, ListPlaylistItemsRequest,
        ListPlaylistsRequest, ListRoomsRequest, MoveMediaRequest,
    };
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    #[test]
    fn test_update_playback_deserialize_playing_update() {
        let json = r#"{"type":1,"playing":true}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Play as i32
        );
        assert_eq!(req.playing, Some(true));
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
    }

    #[test]
    fn test_update_playback_deserialize_seek_update() {
        let json = r#"{"type":3,"position": 42.5}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Seek as i32
        );
        assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
        assert!(req.speed.is_none());
    }

    #[test]
    fn test_update_playback_deserialize_speed_update() {
        let json = r#"{"type":4,"speed": 2.0}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Speed as i32
        );
        assert!(req.position.is_none());
        assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_update_playback_deserialize_full_state() {
        let json = r#"{"type":3,"playing":false,"position":42.5,"speed":1.25,"version":9}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Seek as i32
        );
        assert_eq!(req.playing, Some(false));
        assert_eq!(req.position, Some(42.5));
        assert_eq!(req.speed, Some(1.25));
        assert_eq!(req.version, Some(9));
    }

    #[test]
    fn test_watch_after_event_sequence_prefers_last_event_id() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("42"));

        let sequence = watch_after_event_sequence(&headers, Some(7))
            .expect("valid Last-Event-ID should parse");

        assert_eq!(sequence, Some(42));
    }

    #[test]
    fn test_watch_after_event_sequence_rejects_invalid_last_event_id() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("event-42"));

        let error = watch_after_event_sequence(&headers, Some(7))
            .expect_err("invalid Last-Event-ID should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("Last-Event-ID"));
    }

    #[test]
    fn test_watch_after_event_sequence_rejects_negative_last_event_id() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("-1"));

        let error = watch_after_event_sequence(&headers, Some(7))
            .expect_err("negative Last-Event-ID should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("event sequence"));
    }

    #[test]
    fn test_watch_after_event_sequence_rejects_negative_query_sequence() {
        let headers = HeaderMap::new();

        let error = watch_after_event_sequence(&headers, Some(-1))
            .expect_err("negative query event sequence should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("event sequence"));
    }

    #[test]
    fn test_watch_after_event_sequence_rejects_non_utf8_last_event_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "last-event-id",
            HeaderValue::from_bytes(&[0xff]).expect("header bytes should build"),
        );

        let error = watch_after_event_sequence(&headers, Some(7))
            .expect_err("non-UTF-8 Last-Event-ID should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("Last-Event-ID"));
    }

    #[test]
    fn test_build_get_playback_request_parses_generic_profile_query() {
        let request = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: Some("transcode".to_string()),
            max_streaming_bitrate: Some(8_000_000),
            max_audio_channels: Some(2),
            video_codecs: Some("h264,av1".to_string()),
            containers: Some("mp4,webm".to_string()),
            audio_capability: Some("surround".to_string()),
            subtitle_preference: Some("embedded_or_external".to_string()),
        })
        .expect("playback query should parse");

        let profile = request
            .playback_client_profile
            .expect("query should produce playback client profile");
        assert_eq!(
            profile.delivery_preference,
            crate::proto::client::PlaybackDeliveryPreference::Transcode as i32
        );
        assert_eq!(profile.max_streaming_bitrate, Some(8_000_000));
        assert_eq!(profile.max_audio_channels, Some(2));
        assert_eq!(
            profile.supported_video_codecs,
            vec![
                crate::proto::client::PlaybackVideoCodec::H264 as i32,
                crate::proto::client::PlaybackVideoCodec::Av1 as i32,
            ]
        );
        assert_eq!(
            profile.supported_containers,
            vec![
                crate::proto::client::PlaybackContainer::Mp4 as i32,
                crate::proto::client::PlaybackContainer::Webm as i32,
            ]
        );
        assert_eq!(
            profile.audio_capability,
            crate::proto::client::PlaybackAudioCapability::Surround as i32
        );
        assert_eq!(
            profile.subtitle_preference,
            crate::proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal as i32
        );
    }

    #[test]
    fn test_build_get_playback_request_omits_profile_when_query_is_empty() {
        let request = build_get_playback_request(&GetPlaybackQuery::default())
            .expect("empty query should be valid");

        assert!(request.playback_client_profile.is_none());
    }

    #[test]
    fn test_handwritten_room_queries_reject_unknown_fields() {
        assert!(serde_urlencoded::from_str::<GetPlaybackQuery>(
            "delivery_preference=direct&extra=true"
        )
        .is_err());
        assert!(serde_urlencoded::from_str::<WatchQuery>(
            "format=json&after_event_sequence=12&extra=true"
        )
        .is_err());
        assert!(serde_urlencoded::from_str::<WatchPlaybackSnapshotQuery>(
            "format=json&media_id=media_1&extra=true"
        )
        .is_err());
        assert!(
            serde_urlencoded::from_str::<ChatImageObjectQuery>("token=token&extra=true").is_err()
        );
        assert!(
            serde_urlencoded::from_str::<VideoCoverObjectQuery>("token=token&extra=true").is_err()
        );
        assert!(
            serde_urlencoded::from_str::<RoomCoverObjectQuery>("token=token&extra=true").is_err()
        );
        assert!(
            serde_urlencoded::from_str::<PlaylistCoverObjectQuery>("token=token&extra=true")
                .is_err()
        );
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_video_codec() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: Some("h264,divx".to_string()),
            containers: None,
            audio_capability: None,
            subtitle_preference: None,
        })
        .expect_err("unknown codec must be rejected");

        assert!(error.message.contains("video codec"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_delivery_preference() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: Some("download".to_string()),
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: None,
            audio_capability: None,
            subtitle_preference: None,
        })
        .expect_err("unknown delivery preference must be rejected");

        assert!(error.message.contains("delivery_preference"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_container() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: Some("mp4,avi".to_string()),
            audio_capability: None,
            subtitle_preference: None,
        })
        .expect_err("unknown container must be rejected");

        assert!(error.message.contains("container"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_audio_capability() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: None,
            audio_capability: Some("mono".to_string()),
            subtitle_preference: None,
        })
        .expect_err("unknown audio capability must be rejected");

        assert!(error.message.contains("audio_capability"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_subtitle_preference() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: None,
            audio_capability: None,
            subtitle_preference: Some("burn_in".to_string()),
        })
        .expect_err("unknown subtitle preference must be rejected");

        assert!(error.message.contains("subtitle_preference"), "{error:?}");
    }

    #[test]
    fn test_members_query_params_deserialize_sorting_and_filters() {
        let json =
            r#"{"page":2,"page_size":25,"search":"alice","role":2,"sort_by":2,"sort_direction":1}"#;
        let query: GetRoomMembersRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "alice");
        assert_eq!(
            query.role,
            Some(synctv_proto::common::RoomMemberRole::Admin as i32)
        );
        assert_eq!(
            query.sort_by,
            crate::proto::client::RoomMemberListSortBy::Username as i32
        );
        assert_eq!(
            query.sort_direction,
            crate::proto::client::SortDirection::Asc as i32
        );
    }

    #[test]
    fn test_scalar_query_parsers_reject_invalid_values() {
        let mut params = HashMap::new();
        params.insert("page".to_string(), "abc".to_string());
        assert!(parse_optional_query_i32(&params, "page").is_err());

        let mut params = HashMap::new();
        params.insert("dynamic_only".to_string(), "sometimes".to_string());
        assert!(parse_optional_query_bool(&params, "dynamic_only").is_err());

        assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=definitely").is_err());
        assert!(serde_urlencoded::from_str::<DeletePlaylistQuery>("force=definitely").is_err());
    }

    #[test]
    fn test_list_rooms_query_deserializes_proto_defaults() {
        let query: ListRoomsRequest = serde_urlencoded::from_str("").unwrap();

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert!(query.search.is_empty());
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
    }

    #[test]
    fn test_list_rooms_query_deserializes_explicit_values() {
        let query: ListRoomsRequest = serde_urlencoded::from_str(
            "page=2&page_size=25&search=room&sort_by=4&sort_direction=1",
        )
        .unwrap();

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "room");
        assert_eq!(
            query.sort_by,
            crate::proto::client::RoomListSortBy::Name as i32
        );
        assert_eq!(
            query.sort_direction,
            crate::proto::client::SortDirection::Asc as i32
        );
    }

    #[test]
    fn test_check_room_path_deserializes_proto_field_name() {
        let req: crate::proto::client::CheckRoomRequest =
            serde_json::from_str(r#"{"room_id":"room_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
    }

    #[test]
    fn test_room_path_request_deserializes_proto_field_name() {
        let req: crate::proto::client::RoomPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
    }

    #[test]
    fn test_room_media_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomMediaTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1","media_id":"med_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
        assert_eq!(req.media_id, "med_1");
    }

    #[test]
    fn test_kick_room_stream_body_does_not_require_path_media_id() {
        let empty: super::KickRoomStreamBody = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.reason, "");

        let with_reason: super::KickRoomStreamBody =
            serde_json::from_str(r#"{"reason":"moderation"}"#).unwrap();
        assert_eq!(with_reason.reason, "moderation");
    }

    #[test]
    fn test_room_playlist_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomPlaylistTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1","playlist_id":"pl_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
        assert_eq!(req.playlist_id, "pl_1");
    }

    #[test]
    fn test_list_playlists_query_deserializes_proto_defaults() {
        let query: ListPlaylistsRequest = serde_urlencoded::from_str("").unwrap();

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
        assert_eq!(query.availability, 0);
    }

    #[test]
    fn test_list_playlists_query_deserializes_explicit_values() {
        let query: ListPlaylistsRequest = serde_urlencoded::from_str(
            "page=2&page_size=25&sort_by=4&sort_direction=2&availability=2",
        )
        .unwrap();

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(
            query.sort_by,
            crate::proto::client::PlaylistListSortBy::UpdatedAt as i32
        );
        assert_eq!(
            query.sort_direction,
            crate::proto::client::SortDirection::Desc as i32
        );
        assert_eq!(
            query.availability,
            crate::proto::client::ResourceAvailabilityFilter::Unavailable as i32
        );
    }

    #[test]
    fn test_chat_history_parser_rejects_invalid_limit() {
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "many".to_string());
        assert!(serde_urlencoded::from_str::<GetChatHistoryRequest>("limit=many").is_err());
    }

    #[test]
    fn test_chat_history_query_preserves_limit_for_shared_validation() {
        let req: GetChatHistoryRequest = serde_urlencoded::from_str("limit=101").unwrap();

        assert_eq!(req.limit, 101);
        assert!(crate::impls::validate_proto_request(&req).is_err());
    }

    #[test]
    fn test_hot_rooms_query_preserves_limit_for_shared_validation() {
        let req: GetHotRoomsRequest = serde_urlencoded::from_str("limit=51").unwrap();

        assert_eq!(req.limit, 51);
        assert!(crate::impls::validate_proto_request(&req).is_err());
    }

    #[test]
    fn test_list_playlist_items_body_deserialize_room_root() {
        let json = r"{}";
        let req: ListPlaylistItemsRequest = serde_json::from_str(json).unwrap();
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
        assert_eq!(req.page, 0);
        assert_eq!(req.page_size, 0);
        assert_eq!(req.availability, 0);
    }

    #[test]
    fn test_list_playlist_items_body_deserialize_dynamic_target() {
        let json =
            r#"{"playlist_id":"pl1","target":{"cursor":"season-1"},"page":2,"page_size":25}"#;
        let req: ListPlaylistItemsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.playlist_id, "pl1");
        let target: serde_json::Value = serde_json::from_slice(&req.target).unwrap();
        assert_eq!(target, serde_json::json!({"cursor":"season-1"}));
        assert_eq!(req.page, 2);
        assert_eq!(req.page_size, 25);
        assert_eq!(req.availability, 0);
    }

    #[test]
    fn test_update_playback_request_deserialize_with_version() {
        let json = r#"{"type": 1, "version": 42}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Play as i32
        );
        assert_eq!(req.version, Some(42));
    }

    #[test]
    fn test_add_media_batch_body_deserializes_without_room_id_in_nested_items() {
        let json = r#"{
            "items": [
                {
                    "playlist_id": "playlist-1",
                    "source_provider": "yt-dlp",
                    "provider_instance_name": "default",
                    "source_config": [1, 2, 3],
                    "name": "Example"
                }
            ]
        }"#;
        let body: AddMediaBatchBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.items.len(), 1);
    }

    #[test]
    fn test_move_media_request_deserializes_anchor_fields_without_wrapper() {
        let json = r#"{
            "media_ids": ["media-1"],
            "before_media_id": "media-2"
        }"#;
        let req: MoveMediaRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.media_ids, vec!["media-1".to_string()]);
        assert_eq!(req.before_media_id.as_deref(), Some("media-2"));
        assert!(req.after_media_id.is_none());
    }

    #[test]
    fn test_parse_chat_history_request_accepts_cursor_only() {
        let req: GetChatHistoryRequest = serde_urlencoded::from_str(
            "limit=20&cursor=2026-03-31T12%3A00%3A00%2B00%3A00%7Cmsg_123",
        )
        .expect("deserialize cursor request");

        assert_eq!(req.limit, 20);
        assert_eq!(req.cursor, "2026-03-31T12:00:00+00:00|msg_123");
    }

    #[test]
    fn test_chat_message_path_injected_queries_deserialize_without_message_id() {
        let message: GetChatMessageRequest = serde_urlencoded::from_str("include_deleted=true")
            .expect("deserialize chat message query");
        assert!(message.message_id.is_empty());
        assert!(message.include_deleted);

        let context: GetChatMessageContextRequest =
            serde_urlencoded::from_str("before_limit=5&after_limit=6&include_deleted=true")
                .expect("deserialize chat message context query");
        assert!(context.message_id.is_empty());
        assert_eq!(context.before_limit, 5);
        assert_eq!(context.after_limit, 6);
        assert!(context.include_deleted);
    }

    #[test]
    fn test_delete_force_query_deserialization_accepts_bool_only() {
        let query: DeleteMediaQuery = serde_urlencoded::from_str("force=true").unwrap();
        assert!(query.force);

        let query: DeletePlaylistQuery = serde_urlencoded::from_str("force=false").unwrap();
        assert!(!query.force);

        assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=1").is_err());
    }

    #[test]
    fn test_delete_entries_body_deserializes_force_true() {
        let body: DeleteEntriesBody = serde_json::from_str(
            r#"{"playlist_ids":["playlist-1"],"media_ids":["media-1"],"force":true}"#,
        )
        .unwrap();

        assert_eq!(body.playlist_ids, vec!["playlist-1"]);
        assert_eq!(body.media_ids, vec!["media-1"]);
        assert!(body.force);
    }

    #[test]
    fn test_create_playlist_body_deserializes_dynamic_fields() {
        let body: CreatePlaylistBody = serde_json::from_str(
            r#"{
                "name":"Dynamic Folder",
                "parent_id":"playlist-root",
                "source_provider":"alist",
                "source_config":{"path":"/tv"},
                "provider_instance_name":"alist-main"
            }"#,
        )
        .unwrap();

        assert_eq!(body.name, "Dynamic Folder");
        assert_eq!(body.parent_id, "playlist-root");
        assert_eq!(body.source_provider, "alist");
        let source_config: serde_json::Value = serde_json::from_slice(&body.source_config).unwrap();
        assert_eq!(source_config, serde_json::json!({"path":"/tv"}));
        assert_eq!(body.provider_instance_name, "alist-main");
    }

    #[test]
    fn test_move_playlist_body_deserializes_without_path_playlist_id() {
        let body: crate::proto::client::MovePlaylistRequest =
            serde_json::from_str(r#"{"before_playlist_id":"playlist-2"}"#).expect("deserialize");

        assert!(body.playlist_id.is_empty());
        assert_eq!(
            body.anchor,
            Some(
                crate::proto::client::move_playlist_request::Anchor::BeforePlaylistId(
                    "playlist-2".to_string()
                )
            )
        );
    }

    #[test]
    fn test_sse_event_id_from_resource_changed_uses_event_sequence() {
        let changed = crate::proto::client::ResourceChanged {
            observe_id: "chat-events".to_string(),
            payload: Some(crate::proto::client::resource_changed::Payload::ChatEvent(
                crate::proto::client::ChatMessageEvent {
                    event_id: " chat-event-3 ".to_string(),
                    room_id: "room_test".to_string(),
                    kind: crate::proto::client::ChatMessageEventKind::Created as i32,
                    message: None,
                    occurred_at: 123,
                    sequence: 3,
                },
            )),
            event_cursor: None,
        };

        assert_eq!(
            sse_event_id_from_resource_changed(&changed).as_deref(),
            Some("3")
        );
    }

    #[tokio::test]
    async fn test_chat_resource_changed_sse_event_includes_event_sequence() {
        use crate::proto::client::resource_changed::Payload;
        use crate::proto::client::server_message::Message;
        use axum::response::IntoResponse;

        let message = crate::proto::client::ServerMessage {
            message: Some(Message::ResourceChanged(
                crate::proto::client::ResourceChanged {
                    observe_id: "chat-events".to_string(),
                    payload: Some(Payload::ChatEvent(crate::proto::client::ChatMessageEvent {
                        event_id: "chat-event-3".to_string(),
                        room_id: "room_test".to_string(),
                        kind: crate::proto::client::ChatMessageEventKind::Created as i32,
                        message: None,
                        occurred_at: 123,
                        sequence: 3,
                    })),
                    event_cursor: None,
                },
            )),
        };
        let event = sse_event_from_server_message(
            crate::http::websocket::RealtimeTransportFormat::Json,
            message,
        )
        .expect("resource changed should produce SSE event")
        .expect("SSE event should serialize");
        let response = axum::response::sse::Sse::new(tokio_stream::iter([Ok::<
            _,
            std::convert::Infallible,
        >(event)]))
        .into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("SSE body should render");
        let rendered = std::str::from_utf8(&body).expect("SSE body should be utf-8");

        assert!(rendered.contains("id: 3\n"));
        assert!(rendered.contains("event: changed\n"));
    }

    #[test]
    fn test_cancel_on_drop_stream_cancels_token() {
        let token = tokio_util::sync::CancellationToken::new();
        let stream = tokio_stream::iter([Ok::<_, std::convert::Infallible>(
            axum::response::sse::Event::default(),
        )]);
        let wrapped = CancelOnDropStream::new(stream, token.clone());

        assert!(!token.is_cancelled());
        drop(wrapped);
        assert!(token.is_cancelled());
    }
}
