use std::pin::Pin;
use std::sync::Arc;

use crate::proxy_signature::{ProxySigningKey, ProxyUrlClaims};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::provider::ExecutionControl;
use synctv_core::provider::{LiveFlvAccess, PlaybackTransportAction, PlaybackTransportServices};
use synctv_core::service::UserService;
use synctv_proto::playback_provider::common::StreamChunk;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::impls::ApiError;
use crate::proxy_signature::ProxySigningKeyQueryExt;
use base64::Engine as _;

pub struct PlaybackProviderAccessDeps<'a> {
    pub proxy_signing_key: &'a ProxySigningKey,
    pub public_id_codec: &'a synctv_adapter::PublicIdCodec,
    pub provider_stores: &'a dyn synctv_core::provider::ProviderStoreResolver,
    pub user_service: &'a UserService,
    pub playback_transport_services: &'a PlaybackTransportServices,
}

struct PlaybackProviderAccessValidator<'a> {
    user_service: &'a UserService,
    playback_transport_services: &'a PlaybackTransportServices,
}

impl PlaybackProviderAccessValidator<'_> {
    async fn validate_fresh_access(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<(), ApiError> {
        let (user, room) = tokio::join!(
            self.user_service.get_user(user_id),
            self.playback_transport_services
                .room_service
                .get_room(room_id),
        );
        let user = user.map_err(ApiError::from)?;
        if user.status != synctv_core::models::UserStatus::Active || user.deleted_at.is_some() {
            return Err(ApiError::Authorization(
                synctv_common::messages::STALE_PROXY_ACCESS.to_string(),
            ));
        }

        let room = room.map_err(ApiError::from)?;
        if room.is_banned || !room.status.is_active() {
            return Err(ApiError::Authorization(
                "Playback provider URL is no longer valid for this room".to_string(),
            ));
        }

        self.playback_transport_services
            .room_service
            .check_membership_with_room(&room, user_id)
            .await
            .map_err(map_playback_provider_membership_probe_error)
    }
}

impl PlaybackProviderAccessDeps<'_> {
    pub async fn validate_fresh_access(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<(), ApiError> {
        PlaybackProviderAccessValidator {
            user_service: self.user_service,
            playback_transport_services: self.playback_transport_services,
        }
        .validate_fresh_access(room_id, user_id)
        .await
    }
}

#[derive(Clone, Copy)]
pub struct PlaybackProviderApiRuntime<'a> {
    pub proxy_signing_key: &'a ProxySigningKey,
    pub public_id_codec: &'a synctv_adapter::PublicIdCodec,
    pub provider_stores: &'a dyn synctv_core::provider::ProviderStoreResolver,
    pub user_service: &'a UserService,
    pub playback_transport_services: &'a PlaybackTransportServices,
    pub proxy_http_client: &'a reqwest::Client,
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub proxy_slice_cache: &'a synctv_proxy::slice_cache::SliceCache,
}

impl PlaybackProviderApiRuntime<'_> {
    pub async fn validate_fresh_access(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<(), ApiError> {
        PlaybackProviderAccessValidator {
            user_service: self.user_service,
            playback_transport_services: self.playback_transport_services,
        }
        .validate_fresh_access(room_id, user_id)
        .await
    }
}

#[derive(Clone, Copy)]
pub struct PlaybackProviderIdentityRuntime<'a> {
    pub public_id_codec: &'a synctv_adapter::PublicIdCodec,
}

pub struct PlaybackProviderAccessRequest<'a> {
    pub version: &'a str,
    pub resource: String,
    pub signature: &'a str,
    pub user_id: &'a str,
    pub room_id: &'a str,
    pub expires_at: i64,
    pub target_url: Option<&'a str>,
}

/// Macro to implement HasPlaybackProviderAccessFields for provider types
/// that have matching field names.
#[macro_export]
macro_rules! impl_has_playback_provider_access_fields {
    ($type:ty) => {
        impl<'a> $crate::playback_provider::common::HasPlaybackProviderAccessFields<'a> for $type {
            fn proxy_signing_key(&self) -> &'a $crate::proxy_signature::ProxySigningKey {
                self.runtime.proxy_signing_key
            }
            fn public_id_codec(&self) -> &'a synctv_adapter::PublicIdCodec {
                self.runtime.public_id_codec
            }
            fn provider_stores(&self) -> &'a dyn synctv_core::provider::ProviderStoreResolver {
                self.runtime.provider_stores
            }
            fn user_service(&self) -> &'a synctv_core::service::UserService {
                self.runtime.user_service
            }
            fn playback_transport_services(
                &self,
            ) -> &'a synctv_core::provider::PlaybackTransportServices {
                self.runtime.playback_transport_services
            }
        }
    };
}

pub trait HasPlaybackProviderAccessFields<'a> {
    fn proxy_signing_key(&self) -> &'a ProxySigningKey;
    fn public_id_codec(&self) -> &'a synctv_adapter::PublicIdCodec;
    fn provider_stores(&self) -> &'a dyn synctv_core::provider::ProviderStoreResolver;
    fn user_service(&self) -> &'a UserService;
    fn playback_transport_services(&self) -> &'a PlaybackTransportServices;

    fn access_deps(&self) -> PlaybackProviderAccessDeps<'a> {
        PlaybackProviderAccessDeps {
            proxy_signing_key: self.proxy_signing_key(),
            public_id_codec: self.public_id_codec(),
            provider_stores: self.provider_stores(),
            user_service: self.user_service(),
            playback_transport_services: self.playback_transport_services(),
        }
    }
}

pub async fn verify_playback_provider_access_with_deps(
    deps: &PlaybackProviderAccessDeps<'_>,
    provider_name: &'static str,
    request: PlaybackProviderAccessRequest<'_>,
) -> Result<
    (
        Arc<dyn synctv_core::provider::ProviderStore>,
        ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_http_access(deps, provider_name, request).await
}

pub fn live_flv_access_from_claims(
    public_id_codec: &synctv_adapter::PublicIdCodec,
    claims: &ProxyUrlClaims,
) -> Result<LiveFlvAccess, ApiError> {
    let user_id = public_id_codec
        .decode_user_id(&claims.user_id)
        .map_err(|error| ApiError::InvalidInput(format!("Invalid uid: {error}")))?;
    Ok(LiveFlvAccess {
        user_id,
        expires_at: claims.expires_at,
    })
}

const MAX_MANIFEST_CONTENT_LENGTH: u64 = 10 * 1024 * 1024;
const MAX_MANIFEST_SIZE: usize = 10 * 1024 * 1024;
const MAX_CONSECUTIVE_FLV_DROPS: u32 = 100;

pub type PlaybackProviderChunkStream =
    Pin<Box<dyn Stream<Item = Result<StreamChunk, ApiError>> + Send + 'static>>;

pub struct PlaybackTransportExecutorDeps<'a> {
    pub proxy_signing_key: &'a ProxySigningKey,
    pub proxy_http_client: &'a reqwest::Client,
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub proxy_slice_cache: &'a synctv_proxy::slice_cache::SliceCache,
    pub request_control: Option<&'a ExecutionControl>,
    pub hls_rewrite: Option<HlsRewriteSigning<'a>>,
}

#[derive(Clone, Copy)]
pub struct HlsRewriteSigning<'a> {
    pub segment_base: &'a str,
    pub claims: &'a ProxyUrlClaims,
    /// A trailing `/*` enables typed child routes (`manifest` and `media`).
    pub resource: &'a str,
}

#[derive(Clone, Copy)]
pub struct DashRewriteSigning<'a> {
    pub resource_base: &'a str,
    pub resource_prefix: &'a str,
    pub claims: &'a ProxyUrlClaims,
}

pub struct LivePlaybackDeps<'a> {
    pub proxy_signing_key: &'a ProxySigningKey,
    pub live_streaming_infrastructure:
        Option<&'a Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub connection_runtime: &'a dyn synctv_realtime::sync::ConnectionRuntime,
    pub livestream_config: &'a crate::api_runtime::LivestreamRuntimeSettings,
    pub runtime_settings_store: Option<&'a synctv_core::service::RuntimeSettingsStore>,
}

#[derive(Clone, Copy)]
pub struct LivePlaybackApiRuntime<'a> {
    pub proxy_signing_key: &'a ProxySigningKey,
    pub live_streaming_infrastructure:
        Option<&'a Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub connection_runtime: &'a dyn synctv_realtime::sync::ConnectionRuntime,
    pub livestream_config: &'a crate::api_runtime::LivestreamRuntimeSettings,
    pub runtime_settings_store: Option<&'a synctv_core::service::RuntimeSettingsStore>,
}

/// Macro to implement HasLivePlaybackFields for provider types
/// that have matching field names.
#[macro_export]
macro_rules! impl_has_live_playback_fields {
    ($type:ty) => {
        impl<'a> $crate::playback_provider::common::HasLivePlaybackFields<'a> for $type {
            fn proxy_signing_key(&self) -> &'a $crate::proxy_signature::ProxySigningKey {
                self.live_runtime.proxy_signing_key
            }
            fn live_streaming_infrastructure(
                &self,
            ) -> Option<&'a std::sync::Arc<synctv_livestream::LiveStreamingInfrastructure>> {
                self.live_runtime.live_streaming_infrastructure
            }
            fn connection_runtime(&self) -> &'a dyn synctv_realtime::sync::ConnectionRuntime {
                self.live_runtime.connection_runtime
            }
            fn livestream_config(&self) -> &'a $crate::api_runtime::LivestreamRuntimeSettings {
                self.live_runtime.livestream_config
            }
            fn runtime_settings_store(
                &self,
            ) -> Option<&'a synctv_core::service::RuntimeSettingsStore> {
                self.live_runtime.runtime_settings_store
            }
        }
    };
}

pub trait HasLivePlaybackFields<'a> {
    fn proxy_signing_key(&self) -> &'a ProxySigningKey;
    fn live_streaming_infrastructure(
        &self,
    ) -> Option<&'a Arc<synctv_livestream::LiveStreamingInfrastructure>>;
    fn connection_runtime(&self) -> &'a dyn synctv_realtime::sync::ConnectionRuntime;
    fn livestream_config(&self) -> &'a crate::api_runtime::LivestreamRuntimeSettings;
    fn runtime_settings_store(&self) -> Option<&'a synctv_core::service::RuntimeSettingsStore>;

    fn live_deps(&self) -> LivePlaybackDeps<'a> {
        LivePlaybackDeps {
            proxy_signing_key: self.proxy_signing_key(),
            live_streaming_infrastructure: self.live_streaming_infrastructure(),
            connection_runtime: self.connection_runtime(),
            livestream_config: self.livestream_config(),
            runtime_settings_store: self.runtime_settings_store(),
        }
    }
}

pub struct LiveFlvChunksRequest {
    pub provider_name: String,
    pub room_id: RoomId,
    pub media_id: MediaId,
    pub user_id: UserId,
    pub expires_at: i64,
    pub external_source: Option<synctv_core::models::LiveProxyMediaSourceConfig>,
    pub head: bool,
}

pub struct LiveHlsMasterChunksRequest {
    pub provider_name: String,
    pub room_id: RoomId,
    pub media_id: MediaId,
    pub version: String,
    pub signature_user_id: String,
    pub signature_room_id: String,
    pub signature_expires_at: i64,
    pub route_provider: String,
    pub external_source: Option<synctv_core::models::LiveProxyMediaSourceConfig>,
}

pub struct LiveHlsPlaylistChunksRequest {
    pub provider_name: String,
    pub room_id: RoomId,
    pub media_id: MediaId,
    pub version: String,
    pub generation_id: String,
    pub signature_user_id: String,
    pub signature_room_id: String,
    pub signature_expires_at: i64,
    pub route_provider: String,
}

pub struct LiveHlsSegmentChunksRequest {
    pub room_id: RoomId,
    pub media_id: MediaId,
    pub generation_id: String,
    pub segment_name: String,
    pub head: bool,
}

pub async fn stream_live_flv_chunks(
    deps: LivePlaybackDeps<'_>,
    req: LiveFlvChunksRequest,
) -> Result<PlaybackProviderChunkStream, ApiError> {
    if req.head {
        return Ok(Box::pin(futures::stream::once(async {
            Ok(StreamChunk {
                status: 200,
                content_type: Some("video/x-flv".to_string()),
                cache_control: Some("no-cache".to_string()),
                ..Default::default()
            })
        })));
    }

    let room_id_key = req.room_id.to_string();
    let media_id_key = req.media_id.to_string();
    tracing::info!(
        room_id = %req.room_id,
        media_id = %req.media_id,
        provider = %req.provider_name,
        "FLV streaming request"
    );

    let infrastructure = deps.live_streaming_infrastructure.ok_or_else(|| {
        ApiError::ServiceUnavailable("Live streaming service is unavailable".to_string())
    })?;
    let (rx, subscriber_guard) = synctv_livestream::FlvStreamingApi::create_session_with_pull(
        infrastructure,
        &room_id_key,
        &media_id_key,
        req.external_source.as_ref(),
    )
    .await
    .map_err(|error| crate::impls::map_livestream_backend_error(error.as_ref()))?;

    let mut disconnect_rx = deps.connection_runtime.subscribe_disconnect();
    let max_connection_duration =
        std::time::Duration::from_secs(deps.livestream_config.flv_max_connection_duration_seconds);
    let write_timeout =
        std::time::Duration::from_secs(deps.livestream_config.flv_write_timeout_seconds);
    let room_id = req.room_id;
    let user_id = req.user_id;
    let expires_at = req.expires_at;

    let (tx, rx_wrapped) = mpsc::channel::<Result<StreamChunk, ApiError>>(512);
    tokio::spawn(async move {
        let _guard = subscriber_guard;
        let mut rx = rx;
        let mut consecutive_drops: u32 = 0;
        let start_time = std::time::Instant::now();
        let mut lifecycle_tick = tokio::time::interval(std::time::Duration::from_secs(1));
        lifecycle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let metadata = StreamChunk {
            status: 200,
            content_type: Some("video/x-flv".to_string()),
            cache_control: Some("no-cache".to_string()),
            ..Default::default()
        };
        if tx.send(Ok(metadata)).await.is_err() {
            return;
        }

        loop {
            tokio::select! {
                _ = lifecycle_tick.tick() => {
                    if synctv_core::SystemClock.now().timestamp() > expires_at {
                        tracing::info!(room_id = %room_id, expires_at, "FLV stream terminated: proxy signature expired");
                        break;
                    }

                    if max_connection_duration.as_secs() > 0
                        && start_time.elapsed() >= max_connection_duration
                    {
                        tracing::info!(
                            room_id = %room_id,
                            max_duration_secs = max_connection_duration.as_secs(),
                            "FLV stream terminated: max connection duration exceeded"
                        );
                        break;
                    }
                }
                data = rx.recv() => {
                    let Some(chunk) = data else {
                        break;
                    };
                    match send_flv_chunk(&tx, chunk, write_timeout).await {
                        FlvChunkSendResult::Delivered => {
                            consecutive_drops = 0;
                        }
                        FlvChunkSendResult::Backpressured => {
                            consecutive_drops += 1;
                            if consecutive_drops >= MAX_CONSECUTIVE_FLV_DROPS {
                                crate::observability::metrics::LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL.inc();
                                break;
                            }
                        }
                        FlvChunkSendResult::Closed => {
                            tracing::info!(room_id = %room_id, "FLV stream terminated: client response channel closed");
                            break;
                        }
                    }
                }
                disconnect = disconnect_rx.recv() => {
                    let Ok(event) = disconnect else {
                        break;
                    };
                    if disconnect_applies_to_live_stream(&event, &room_id, &user_id) {
                        break;
                    }
                }
            }
        }
    });

    Ok(Box::pin(ReceiverStream::new(rx_wrapped)))
}

pub async fn get_live_hls_master_chunks(
    deps: LivePlaybackDeps<'_>,
    req: LiveHlsMasterChunksRequest,
) -> Result<PlaybackProviderChunkStream, ApiError> {
    let room_id_key = req.room_id.to_string();
    let media_id_key = req.media_id.to_string();
    tracing::info!(
        room_id = %req.room_id,
        media_id = %req.media_id,
        provider = %req.provider_name,
        "HLS master playlist request"
    );

    let infrastructure = deps.live_streaming_infrastructure.ok_or_else(|| {
        ApiError::ServiceUnavailable("Live streaming service is unavailable".to_string())
    })?;
    let generation = synctv_livestream::HlsStreamingApi::resolve_active_generation_with_pull(
        infrastructure,
        &room_id_key,
        &media_id_key,
        req.external_source.as_ref(),
    )
    .await
    .map_err(|error| crate::impls::map_livestream_backend_error(error.as_ref()))?;

    let Some(generation) = generation else {
        return Err(ApiError::NotFound(
            "Live stream is not currently available".to_string(),
        ));
    };
    let claims = ProxyUrlClaims {
        provider: req.provider_name,
        version: req.version.clone(),
        resource: format!("hls/{}/index.m3u8", generation.generation_id),
        room_id: req.signature_room_id,
        user_id: req.signature_user_id,
        expires_at: req.signature_expires_at,
        target_url: None,
    };
    let signed_query = deps.proxy_signing_key.build_signed_query(&claims);
    let playlist_url = build_hls_playlist_path(
        &req.route_provider,
        &req.version,
        &generation.generation_id,
        &signed_query,
    );
    let content =
        format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=8000000\n{playlist_url}\n");

    Ok(direct_chunk_stream(
        content,
        "application/vnd.apple.mpegurl",
        200,
        false,
    ))
}

pub async fn get_live_hls_playlist_chunks(
    deps: LivePlaybackDeps<'_>,
    req: LiveHlsPlaylistChunksRequest,
) -> Result<PlaybackProviderChunkStream, ApiError> {
    let infrastructure = deps.live_streaming_infrastructure.ok_or_else(|| {
        ApiError::ServiceUnavailable("Live streaming service is unavailable".to_string())
    })?;
    let segment_disguised_as_png = live_segments_disguised_as_png(deps.runtime_settings_store)?;
    let generation_id = req.generation_id.clone();
    let playlist = synctv_livestream::HlsStreamingApi::generate_playlist(
        infrastructure,
        &req.room_id.to_string(),
        &req.media_id.to_string(),
        &generation_id,
        |ts_name| {
            let extension = if segment_disguised_as_png {
                "png"
            } else {
                "ts"
            };
            let segment_name = format!("{ts_name}.{extension}");
            let claims = ProxyUrlClaims {
                provider: req.provider_name.clone(),
                version: req.version.clone(),
                resource: format!("hls/{generation_id}/{segment_name}"),
                room_id: req.signature_room_id.clone(),
                user_id: req.signature_user_id.clone(),
                expires_at: req.signature_expires_at,
                target_url: None,
            };
            let signed_query = deps.proxy_signing_key.build_signed_query(&claims);
            build_hls_segment_path(
                &req.route_provider,
                &req.version,
                &generation_id,
                &segment_name,
                &signed_query,
            )
        },
    )
    .await
    .map_err(|error| crate::impls::map_livestream_backend_error(error.as_ref()))?;

    let Some(content) = playlist else {
        return Err(ApiError::NotFound(
            "HLS generation is unavailable".to_string(),
        ));
    };

    Ok(direct_chunk_stream(
        content,
        "application/vnd.apple.mpegurl",
        200,
        false,
    ))
}

pub async fn get_live_hls_segment_chunks(
    deps: LivePlaybackDeps<'_>,
    req: LiveHlsSegmentChunksRequest,
) -> Result<PlaybackProviderChunkStream, ApiError> {
    let disguised_as_png = req.segment_name.ends_with(".png");
    let validated_name = normalize_hls_segment_name(&req.segment_name, disguised_as_png)?;
    if req.head {
        return Ok(Box::pin(futures::stream::once(async move {
            Ok(StreamChunk {
                status: 200,
                content_type: Some(live_hls_segment_content_type(disguised_as_png).to_string()),
                cache_control: Some("public, max-age=90".to_string()),
                accept_ranges: Some("bytes".to_string()),
                ..Default::default()
            })
        })));
    }

    let infrastructure = deps.live_streaming_infrastructure.ok_or_else(|| {
        ApiError::ServiceUnavailable("Live streaming service is unavailable".to_string())
    })?;
    let ts_data = synctv_livestream::HlsStreamingApi::get_segment(
        infrastructure,
        &req.room_id.to_string(),
        &req.media_id.to_string(),
        &req.generation_id,
        validated_name,
    )
    .await
    .map_err(|error| crate::impls::map_livestream_backend_error(error.as_ref()))?;

    Ok(direct_chunk_stream(
        ts_data,
        live_hls_segment_content_type(disguised_as_png),
        200,
        false,
    ))
}

pub fn map_playback_provider_membership_probe_error(err: synctv_core::Error) -> ApiError {
    match err {
        synctv_core::Error::KickCooldownDenied => {
            ApiError::Authorization(synctv_core::Error::kick_cooldown_denied_message().to_string())
        }
        synctv_core::Error::Authorization(_) => {
            ApiError::Authorization(synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string())
        }
        other => ApiError::from(other),
    }
}

pub async fn verify_playback_provider_http_access(
    deps: &PlaybackProviderAccessDeps<'_>,
    provider_name: &'static str,
    request: PlaybackProviderAccessRequest<'_>,
) -> Result<
    (
        Arc<dyn synctv_core::provider::ProviderStore>,
        ProxyUrlClaims,
    ),
    ApiError,
> {
    if request.version.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "playback provider path must include a version".to_string(),
        ));
    }
    let claims = ProxyUrlClaims {
        provider: provider_name.to_string(),
        version: request.version.to_string(),
        resource: request.resource,
        room_id: request.room_id.to_string(),
        user_id: request.user_id.to_string(),
        expires_at: request.expires_at,
        target_url: request.target_url.map(ToString::to_string),
    };
    deps.proxy_signing_key
        .verify(&claims, request.signature)
        .map_err(|error| {
            tracing::warn!(
                error = %error,
                message = synctv_common::messages::INVALID_PROXY_SIGNATURE,
                "Playback provider signature validation failed"
            );
            ApiError::Authentication(synctv_common::messages::INVALID_PROXY_SIGNATURE.to_string())
        })?;
    let user_id = deps
        .public_id_codec
        .decode::<UserId>(&claims.user_id)
        .map_err(|error| ApiError::InvalidInput(format!("Invalid {error} in user_id")))?;
    let room_id = deps
        .public_id_codec
        .decode::<RoomId>(&claims.room_id)
        .map_err(|error| ApiError::InvalidInput(format!("Invalid {error} in room_id")))?;
    deps.validate_fresh_access(&room_id, &user_id).await?;
    Ok((deps.provider_stores.load(provider_name), claims))
}

pub async fn playback_transport_action_to_chunk_stream(
    deps: PlaybackTransportExecutorDeps<'_>,
    action: PlaybackTransportAction,
    head: bool,
) -> Result<PlaybackProviderChunkStream, ApiError> {
    match action {
        PlaybackTransportAction::FetchAndForward {
            url,
            headers,
            range_header,
        } => {
            let response = if head {
                synctv_proxy::slice_cache::proxy_head_with_cache_enabled_with_control_and_timeout(
                    deps.proxy_slice_cache,
                    deps.proxy_slice_cache.config().enabled,
                    range_header.as_deref(),
                    &url,
                    &headers,
                    deps.request_control,
                    Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
                )
                .await
            } else {
                synctv_proxy::slice_cache::proxy_with_cache_enabled_with_control_and_timeout(
                    deps.proxy_slice_cache,
                    deps.proxy_slice_cache.config().enabled,
                    range_header.as_deref(),
                    &url,
                    &headers,
                    deps.request_control,
                    Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
                )
                .await
            }
            .map_err(|error| map_proxy_execution_error(&error))?;
            Ok(axum_response_to_chunk_stream(response))
        }
        PlaybackTransportAction::M3u8Rewrite { url, headers } => {
            if head {
                let response = send_playback_provider_request(
                    &deps,
                    reqwest::Method::HEAD,
                    &url,
                    &headers,
                    None,
                )
                .await
                .map_err(|error| map_proxy_execution_error(&error))?;
                return Ok(response_to_chunk_stream(response, true));
            }
            let response =
                send_playback_provider_request(&deps, reqwest::Method::GET, &url, &headers, None)
                    .await
                    .map_err(|error| map_proxy_execution_error(&error))?;
            let status = response.status();
            if !status.is_success() {
                return Err(ApiError::ServiceUnavailable(format!(
                    "Remote M3U8 returned status {status}"
                )));
            }

            // Check Content-Length BEFORE reading body to prevent DoS
            let content_length = response.content_length();
            if let Some(size) = content_length {
                if size > MAX_MANIFEST_CONTENT_LENGTH {
                    return Err(ApiError::ServiceUnavailable(
                        "M3U8 manifest exceeded size limit".to_string(),
                    ));
                }
            }

            let initial_capacity = content_length
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(8192)
                .min(MAX_MANIFEST_SIZE);
            let mut body = bytes::BytesMut::with_capacity(initial_capacity);
            let mut stream = response.bytes_stream();
            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result.map_err(|error| map_reqwest_error(&error))?;

                let new_len = body.len().saturating_add(chunk.len());
                if new_len > MAX_MANIFEST_SIZE {
                    return Err(ApiError::ServiceUnavailable(
                        "M3U8 manifest exceeded size limit during streaming read".to_string(),
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            let body = body.freeze();
            let manifest = std::str::from_utf8(&body)
                .map_err(|_| ApiError::InvalidInput("M3U8 manifest is not UTF-8".to_string()))?;
            let rewritten = if let Some(hls_rewrite) = deps.hls_rewrite {
                synctv_proxy::rewrite_m3u8_with_typed_url_mapper(
                    manifest,
                    &url,
                    hls_rewrite.segment_base,
                    move |segment_base, target_url, kind| {
                        build_hls_resource_url(
                            deps.proxy_signing_key,
                            hls_rewrite,
                            segment_base,
                            target_url,
                            kind,
                        )
                    },
                )
                .map_err(|error| ApiError::ServiceUnavailable(error.to_string()))?
            } else {
                return Err(ApiError::Internal(
                    "HLS rewrite action requires API route signing context".to_string(),
                ));
            };
            Ok(direct_chunk_stream(
                rewritten,
                "application/vnd.apple.mpegurl",
                200,
                false,
            ))
        }
        PlaybackTransportAction::M3u8BodyRewrite { body } => {
            if body.len() > MAX_MANIFEST_SIZE {
                return Err(ApiError::ServiceUnavailable(
                    "M3U8 manifest exceeded size limit".to_string(),
                ));
            }
            let manifest = std::str::from_utf8(&body)
                .map_err(|_| ApiError::InvalidInput("M3U8 manifest is not UTF-8".to_string()))?;
            let hls_rewrite = deps.hls_rewrite.ok_or_else(|| {
                ApiError::Internal(
                    "HLS rewrite action requires API route signing context".to_string(),
                )
            })?;
            let rewritten = synctv_proxy::rewrite_m3u8_with_typed_url_mapper(
                manifest,
                "https://synctv.invalid/bilibili-durl.m3u8",
                hls_rewrite.segment_base,
                move |segment_base, target_url, kind| {
                    build_hls_resource_url(
                        deps.proxy_signing_key,
                        hls_rewrite,
                        segment_base,
                        target_url,
                        kind,
                    )
                },
            )
            .map_err(|error| ApiError::ServiceUnavailable(error.to_string()))?;
            Ok(direct_chunk_stream(
                rewritten,
                "application/vnd.apple.mpegurl",
                200,
                head,
            ))
        }
        PlaybackTransportAction::MpdRewrite { .. }
        | PlaybackTransportAction::MpdBodyRewrite { .. } => Err(ApiError::Internal(
            "MPD rewrite action requires DASH route signing context".to_string(),
        )),
        PlaybackTransportAction::DirectBody {
            body,
            content_type,
            status,
        } => Ok(direct_chunk_stream(body, &content_type, status, head)),
        PlaybackTransportAction::LiveFlv { .. }
        | PlaybackTransportAction::LiveHlsMaster { .. }
        | PlaybackTransportAction::LiveHlsPlaylist { .. }
        | PlaybackTransportAction::LiveHlsSegment { .. } => Err(ApiError::Internal(
            "live stream actions are executed by RTMP and LiveProxy playback provider impls"
                .to_string(),
        )),
    }
}

fn build_hls_resource_url(
    signing_key: &ProxySigningKey,
    rewrite: HlsRewriteSigning<'_>,
    segment_base: &str,
    target_url: &str,
    kind: synctv_proxy::HlsResourceKind,
) -> String {
    let Some(resource_prefix) = rewrite.resource.strip_suffix("/*") else {
        let signed_query = signing_key.build_signed_query_with_target_url(
            rewrite.claims,
            rewrite.resource,
            target_url,
        );
        return format!("{segment_base}?{signed_query}");
    };
    let kind = match kind {
        synctv_proxy::HlsResourceKind::Manifest => "manifest",
        synctv_proxy::HlsResourceKind::Media => "media",
    };
    let signed_query = signing_key.build_signed_query_with_target_url(
        rewrite.claims,
        &format!("{resource_prefix}/{kind}"),
        target_url,
    );
    format!("{segment_base}/{kind}?{signed_query}")
}

pub async fn dash_transport_action_to_chunk_stream(
    deps: PlaybackTransportExecutorDeps<'_>,
    action: PlaybackTransportAction,
    signing: DashRewriteSigning<'_>,
    head: bool,
) -> Result<PlaybackProviderChunkStream, ApiError> {
    let (body, url) = match action {
        PlaybackTransportAction::MpdRewrite { url, headers } => {
            if head {
                let response = send_playback_provider_request(
                    &deps,
                    reqwest::Method::HEAD,
                    &url,
                    &headers,
                    None,
                )
                .await
                .map_err(|error| map_proxy_execution_error(&error))?;
                return Ok(response_to_chunk_stream(response, true));
            }
            let response =
                send_playback_provider_request(&deps, reqwest::Method::GET, &url, &headers, None)
                    .await
                    .map_err(|error| map_proxy_execution_error(&error))?;
            let status = response.status();
            if !status.is_success() {
                return Err(ApiError::ServiceUnavailable(format!(
                    "Remote MPD returned status {status}"
                )));
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_MANIFEST_CONTENT_LENGTH)
            {
                return Err(ApiError::ServiceUnavailable(
                    "MPD manifest exceeded size limit".to_string(),
                ));
            }
            let mut body = bytes::BytesMut::with_capacity(
                response
                    .content_length()
                    .and_then(|size| usize::try_from(size).ok())
                    .unwrap_or(8192)
                    .min(MAX_MANIFEST_SIZE),
            );
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| map_reqwest_error(&error))?;
                if body.len().saturating_add(chunk.len()) > MAX_MANIFEST_SIZE {
                    return Err(ApiError::ServiceUnavailable(
                        "MPD manifest exceeded size limit during streaming read".to_string(),
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            (body.freeze(), url)
        }
        PlaybackTransportAction::MpdBodyRewrite { body, source_url } => {
            if body.len() > MAX_MANIFEST_SIZE {
                return Err(ApiError::ServiceUnavailable(
                    "MPD manifest exceeded size limit".to_string(),
                ));
            }
            (bytes::Bytes::from(body), source_url)
        }
        action => return playback_transport_action_to_chunk_stream(deps, action, head).await,
    };
    let manifest = std::str::from_utf8(&body)
        .map_err(|_| ApiError::InvalidInput("MPD manifest is not UTF-8".to_string()))?;
    let rewritten = synctv_proxy::rewrite_mpd_with_url_mapper(manifest, &url, |scope_url, kind| {
        build_dash_scope_url(&deps, signing, scope_url, kind)
    })
    .map_err(|error| ApiError::ServiceUnavailable(error.to_string()))?;
    Ok(direct_chunk_stream(
        rewritten,
        "application/dash+xml",
        200,
        false,
    ))
}

fn build_dash_scope_url(
    deps: &PlaybackTransportExecutorDeps<'_>,
    signing: DashRewriteSigning<'_>,
    scope_url: &str,
    kind: synctv_proxy::MpdResourceKind,
) -> String {
    let kind = match kind {
        synctv_proxy::MpdResourceKind::Media => "media",
        synctv_proxy::MpdResourceKind::Manifest => "manifest",
    };
    let mut claims = signing.claims.clone();
    claims.resource = format!("{}/{kind}", signing.resource_prefix);
    claims.target_url = Some(scope_url.to_string());
    let signature = deps.proxy_signing_key.sign(&claims);
    let scope = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(scope_url);
    let user_id = urlencoding::encode(&claims.user_id);
    let room_id = urlencoding::encode(&claims.room_id);
    format!(
        "{}/{kind}/{scope}/{user_id}/{room_id}/{}/{signature}",
        signing.resource_base, claims.expires_at
    )
}

fn live_segments_disguised_as_png(
    runtime_settings_store: Option<&synctv_core::service::RuntimeSettingsStore>,
) -> Result<bool, ApiError> {
    let Some(registry) = runtime_settings_store else {
        return Ok(false);
    };
    registry.rtmp.ts_disguised_as_png.get().map_err(|error| {
        tracing::error!(
            error = %error,
            "Failed to read rtmp.ts_disguised_as_png setting"
        );
        ApiError::Internal("Failed to read live streaming settings".to_string())
    })
}

fn build_hls_playlist_path(
    route_provider: &str,
    version: &str,
    generation_id: &str,
    signed_query: &str,
) -> String {
    let query_suffix = if signed_query.is_empty() {
        String::new()
    } else {
        format!("?{signed_query}")
    };
    format!(
        "/api/playback-providers/{route_provider}/{version}/hls/{generation_id}/index.m3u8{query_suffix}"
    )
}

fn build_hls_segment_path(
    route_provider: &str,
    version: &str,
    generation_id: &str,
    segment_name: &str,
    signed_query: &str,
) -> String {
    let query_suffix = if signed_query.is_empty() {
        String::new()
    } else {
        format!("?{signed_query}")
    };

    format!(
        "/api/playback-providers/{route_provider}/{version}/hls/{generation_id}/{segment_name}{query_suffix}"
    )
}

fn normalize_hls_segment_name(
    segment_name: &str,
    disguised_as_png: bool,
) -> Result<&str, ApiError> {
    let extension = if disguised_as_png { ".png" } else { ".ts" };
    let normalized = segment_name
        .strip_suffix(extension)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ApiError::InvalidInput("Invalid segment name".to_string()))?;

    if let Err(error) = synctv_livestream::HlsStreamingApi::validate_segment_name(normalized) {
        tracing::warn!(
            segment = %normalized,
            error = %error,
            "HLS segment name failed validation"
        );
        return Err(ApiError::InvalidInput("Invalid segment name".to_string()));
    }

    Ok(normalized)
}

fn live_hls_segment_content_type(disguised_as_png: bool) -> &'static str {
    if disguised_as_png {
        "image/png"
    } else {
        "video/mp2t"
    }
}

async fn send_flv_chunk(
    tx: &mpsc::Sender<Result<StreamChunk, ApiError>>,
    chunk: Result<Bytes, std::io::Error>,
    write_timeout: std::time::Duration,
) -> FlvChunkSendResult {
    let chunk = chunk
        .map(|data| StreamChunk {
            data,
            status: 0,
            ..Default::default()
        })
        .map_err(|error| ApiError::ServiceUnavailable(error.to_string()));
    if write_timeout.is_zero() {
        match tx.send(chunk).await {
            Ok(()) => FlvChunkSendResult::Delivered,
            Err(_) => FlvChunkSendResult::Closed,
        }
    } else {
        match tokio::time::timeout(write_timeout, tx.send(chunk)).await {
            Ok(Ok(())) => FlvChunkSendResult::Delivered,
            Ok(Err(_)) => FlvChunkSendResult::Closed,
            Err(_) => FlvChunkSendResult::Backpressured,
        }
    }
}

fn disconnect_applies_to_live_stream(
    event: &synctv_realtime::sync::DisconnectSignal,
    room_id: &RoomId,
    user_id: &UserId,
) -> bool {
    match event {
        synctv_realtime::sync::DisconnectSignal::User(uid) => uid == user_id,
        synctv_realtime::sync::DisconnectSignal::Room(rid) => rid == room_id,
        synctv_realtime::sync::DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => uid == user_id && rid == room_id,
        synctv_realtime::sync::DisconnectSignal::Connection(_) => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlvChunkSendResult {
    Delivered,
    Backpressured,
    Closed,
}

async fn send_playback_provider_request(
    deps: &PlaybackTransportExecutorDeps<'_>,
    method: reqwest::Method,
    url: &str,
    provider_headers: &std::collections::HashMap<String, String>,
    range_header: Option<&str>,
) -> Result<reqwest::Response, anyhow::Error> {
    validate_playback_provider_url(url, deps.ssrf_guard)?;
    let request = deps.proxy_http_client.request(method, url);
    let mut request = synctv_proxy::apply_provider_headers(request, url, provider_headers)?;
    if let Some(range_header) = range_header {
        request = request.header(reqwest::header::RANGE, range_header);
    }
    let proxy_response = synctv_proxy::send_with_redirect_validation_with_control_and_timeout(
        deps.proxy_http_client,
        request,
        deps.ssrf_guard,
        deps.request_control,
        Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
    )
    .await?;
    Ok(proxy_response.response)
}

fn validate_playback_provider_url(
    raw_url: &str,
    guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<(), anyhow::Error> {
    let parsed = url::Url::parse(raw_url)
        .map_err(|error| anyhow::anyhow!("invalid playback provider URL: {error}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(anyhow::anyhow!(
            "playback provider URL has disallowed scheme: {scheme}"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("playback provider URL host is required"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("playback provider URL port could not be determined"))?;

    guard
        .validate_url_target(host, port)
        .map_err(|error| anyhow::anyhow!("playback provider URL target is invalid: {error}"))
}

fn response_to_chunk_stream(
    response: reqwest::Response,
    head: bool,
) -> PlaybackProviderChunkStream {
    let status = response.status().as_u16();
    let metadata = stream_metadata_from_headers(response.headers());
    let first = futures::stream::once(async move { Ok(metadata_chunk(status, metadata)) });
    if head {
        return Box::pin(first);
    }
    let body_stream = response.bytes_stream().map(|chunk| match chunk {
        Ok(data) => Ok(StreamChunk {
            data,
            status: 0,
            ..Default::default()
        }),
        Err(error) => Err(map_reqwest_error(&error)),
    });
    Box::pin(first.chain(body_stream))
}

fn axum_response_to_chunk_stream(
    response: axum::response::Response,
) -> PlaybackProviderChunkStream {
    let status = response.status().as_u16();
    let metadata = stream_metadata_from_headers(response.headers());
    let first = futures::stream::once(async move { Ok(metadata_chunk(status, metadata)) });
    let body_stream = response
        .into_body()
        .into_data_stream()
        .map(|chunk| match chunk {
            Ok(data) => Ok(StreamChunk {
                data,
                status: 0,
                ..Default::default()
            }),
            Err(error) => Err(map_axum_body_error(error)),
        });
    Box::pin(first.chain(body_stream))
}

fn direct_chunk_stream(
    body: impl Into<Bytes>,
    content_type: &str,
    status: u16,
    head: bool,
) -> PlaybackProviderChunkStream {
    let body = body.into();
    let content_type = content_type.to_string();
    Box::pin(futures::stream::once(async move {
        Ok(StreamChunk {
            data: if head { Bytes::new() } else { body },
            status: status.into(),
            content_type: Some(content_type),
            ..Default::default()
        })
    }))
}

#[derive(Default)]
struct StreamResponseMetadata {
    content_type: Option<String>,
    content_length: Option<u64>,
    content_range: Option<String>,
    accept_ranges: Option<String>,
    cache_control: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    expires: Option<String>,
    content_disposition: Option<String>,
    location: Option<String>,
}

fn stream_metadata_from_headers(headers: &axum::http::HeaderMap) -> StreamResponseMetadata {
    StreamResponseMetadata {
        content_type: header_string(headers, axum::http::header::CONTENT_TYPE),
        content_length: header_string(headers, axum::http::header::CONTENT_LENGTH)
            .and_then(|value| value.parse::<u64>().ok()),
        content_range: header_string(headers, axum::http::header::CONTENT_RANGE),
        accept_ranges: header_string(headers, axum::http::header::ACCEPT_RANGES),
        cache_control: header_string(headers, axum::http::header::CACHE_CONTROL),
        etag: header_string(headers, axum::http::header::ETAG),
        last_modified: header_string(headers, axum::http::header::LAST_MODIFIED),
        expires: header_string(headers, axum::http::header::EXPIRES),
        content_disposition: header_string(headers, axum::http::header::CONTENT_DISPOSITION),
        location: header_string(headers, axum::http::header::LOCATION),
    }
}

fn header_string(headers: &axum::http::HeaderMap, name: axum::http::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn map_reqwest_error(error: &reqwest::Error) -> ApiError {
    if error.is_timeout() {
        ApiError::Timeout(error.to_string())
    } else {
        ApiError::ServiceUnavailable(error.to_string())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_axum_body_error(error: axum::Error) -> ApiError {
    if let Some(kind) = synctv_proxy::proxy_error_kind_from_std_error(&error) {
        match kind {
            synctv_proxy::ProxyErrorKind::Cancelled | synctv_proxy::ProxyErrorKind::Timeout => {
                return ApiError::Timeout(error.to_string());
            }
            synctv_proxy::ProxyErrorKind::Ssrf => {
                return ApiError::Authorization(
                    "Proxy target is not allowed by SSRF policy".to_string(),
                );
            }
            synctv_proxy::ProxyErrorKind::RangeNotSatisfiable => {
                return ApiError::RangeNotSatisfiable { total_size: 0 };
            }
            synctv_proxy::ProxyErrorKind::InvalidRequest => {
                return ApiError::InvalidInput(error.to_string());
            }
            synctv_proxy::ProxyErrorKind::Connection
            | synctv_proxy::ProxyErrorKind::BodyTooLarge
            | synctv_proxy::ProxyErrorKind::Upstream => {}
        }
    }
    ApiError::ServiceUnavailable(error.to_string())
}

fn map_proxy_execution_error(err: &anyhow::Error) -> ApiError {
    match synctv_proxy::proxy_error_kind(err) {
        Some(synctv_proxy::ProxyErrorKind::Cancelled | synctv_proxy::ProxyErrorKind::Timeout) => {
            ApiError::Timeout(err.to_string())
        }
        Some(synctv_proxy::ProxyErrorKind::Ssrf) => {
            ApiError::Authorization("Proxy target is not allowed by SSRF policy".to_string())
        }
        Some(synctv_proxy::ProxyErrorKind::RangeNotSatisfiable) => ApiError::RangeNotSatisfiable {
            total_size: synctv_proxy::proxy_range_not_satisfiable_total_size(err).unwrap_or(0),
        },
        Some(synctv_proxy::ProxyErrorKind::InvalidRequest) => {
            ApiError::InvalidInput(err.to_string())
        }
        Some(
            synctv_proxy::ProxyErrorKind::Connection
            | synctv_proxy::ProxyErrorKind::BodyTooLarge
            | synctv_proxy::ProxyErrorKind::Upstream,
        )
        | None => ApiError::ServiceUnavailable(err.to_string()),
    }
}

pub fn playback_provider_route_base(route_provider: &str, version: &str, resource: &str) -> String {
    let encoded_version: String =
        url::form_urlencoded::byte_serialize(version.as_bytes()).collect();
    format!("/api/playback-providers/{route_provider}/{encoded_version}/{resource}")
}

fn metadata_chunk(status: u16, metadata: StreamResponseMetadata) -> StreamChunk {
    StreamChunk {
        data: Bytes::new(),
        status: status.into(),
        content_type: metadata.content_type,
        content_length: metadata.content_length,
        content_range: metadata.content_range,
        accept_ranges: metadata.accept_ranges,
        cache_control: metadata.cache_control,
        etag: metadata.etag,
        last_modified: metadata.last_modified,
        expires: metadata.expires,
        content_disposition: metadata.content_disposition,
        location: metadata.location,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn start_mock_server_or_skip() -> anyhow::Result<Option<MockServer>> {
        match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => {
                drop(listener);
                Ok(Some(MockServer::start().await))
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
            Err(error) => Err(anyhow::anyhow!(
                "preflight bind for playback provider test should succeed: {error}"
            )),
        }
    }

    fn mock_public_url(mock_server: &MockServer, path: &str) -> String {
        format!(
            "http://cdn.example.com:{}{path}",
            mock_server.address().port()
        )
    }

    fn mock_proxy_client(mock_server: &MockServer) -> anyhow::Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *mock_server.address())
            .build()?)
    }

    fn test_ssrf_guard() -> synctv_common::ssrf::SsrfGuard {
        synctv_common::ssrf::SsrfGuard::builder()
            .extra_allowed_host("cdn.example.com".to_string())
            .build()
    }

    #[test]
    fn live_flv_access_decodes_signed_public_user_id_at_api_boundary() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let claims = crate::proxy_signature::ProxyUrlClaims {
            provider: "rtmp".to_string(),
            version: "v1".to_string(),
            resource: "flv-stream".to_string(),
            room_id: "room_2".to_string(),
            user_id: "usr_7".to_string(),
            expires_at: 1234,
            target_url: None,
        };

        let access = live_flv_access_from_claims(&codec, &claims)
            .expect("prefixed public user id should decode");

        assert_eq!(
            access.user_id,
            synctv_core::models::UserId::expect_positive(7)
        );
        assert_eq!(access.expires_at, 1234);
    }

    #[test]
    fn live_flv_access_rejects_unprefixed_public_user_id() {
        let codec = synctv_adapter::PublicIdCodec::plain();
        let claims = crate::proxy_signature::ProxyUrlClaims {
            provider: "rtmp".to_string(),
            version: "v1".to_string(),
            resource: "flv-stream".to_string(),
            room_id: "room_2".to_string(),
            user_id: "7".to_string(),
            expires_at: 1234,
            target_url: None,
        };

        assert!(live_flv_access_from_claims(&codec, &claims).is_err());
    }

    #[tokio::test]
    async fn fetch_and_forward_chunk_stream_uses_slice_cache_for_full_body() -> anyhow::Result<()> {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let total_size = 8_u64;

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .and(header("Range", "bytes=0-3"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes([1_u8, 2, 3, 4])
                    .insert_header("Content-Range", "bytes 0-3/8")
                    .insert_header("Content-Length", "4")
                    .insert_header("Content-Type", "video/mp4")
                    .insert_header("Accept-Ranges", "bytes"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .and(header("Range", "bytes=4-7"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes([5_u8, 6, 7, 8])
                    .insert_header("Content-Range", "bytes 4-7/8")
                    .insert_header("Content-Length", "4")
                    .insert_header("Content-Type", "video/mp4")
                    .insert_header("Accept-Ranges", "bytes"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = mock_proxy_client(&mock_server)?;
        let ssrf_guard = test_ssrf_guard();
        let proxy_slice_cache =
            synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
                synctv_proxy::slice_cache::SliceCacheConfig {
                    slice_size: 4,
                    ..Default::default()
                },
                client.clone(),
                ssrf_guard.clone(),
            )?;
        let signing_key = crate::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-secret-key-for-playback-provider-common",
        )?;
        let deps = PlaybackTransportExecutorDeps {
            proxy_signing_key: &signing_key,
            proxy_http_client: &client,
            ssrf_guard: &ssrf_guard,
            proxy_slice_cache: &proxy_slice_cache,
            request_control: None,
            hls_rewrite: None,
        };
        let action = PlaybackTransportAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: None,
        };

        let mut stream = playback_transport_action_to_chunk_stream(deps, action, false)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let metadata = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("stream should emit metadata"))?
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        assert_eq!(metadata.status, 200);
        assert_eq!(metadata.content_length, Some(total_size));
        assert_eq!(metadata.accept_ranges.as_deref(), Some("bytes"));

        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend(chunk.map_err(|error| anyhow::anyhow!("{error:?}"))?.data);
        }
        assert_eq!(body, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        Ok(())
    }

    #[tokio::test]
    async fn m3u8_rewrite_follows_validated_redirects() -> anyhow::Result<()> {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };

        Mock::given(method("GET"))
            .and(path("/redirect/master.m3u8"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "/final/master.m3u8"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/final/master.m3u8"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("#EXTM3U\n#EXTINF:2,\nsegment.ts\n")
                    .insert_header("Content-Type", "application/vnd.apple.mpegurl"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = mock_proxy_client(&mock_server)?;
        let ssrf_guard = test_ssrf_guard();
        let proxy_slice_cache =
            synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
                client.clone(),
                ssrf_guard.clone(),
            )?;
        let signing_key = crate::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-secret-key-for-playback-provider-common",
        )?;
        let claims = crate::proxy_signature::ProxyUrlClaims {
            provider: "direct_url".to_string(),
            version: "v1".to_string(),
            resource: "hls-resources/direct/0/*".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: synctv_core::SystemClock.now().timestamp() + 1800,
            target_url: None,
        };
        let deps = PlaybackTransportExecutorDeps {
            proxy_signing_key: &signing_key,
            proxy_http_client: &client,
            ssrf_guard: &ssrf_guard,
            proxy_slice_cache: &proxy_slice_cache,
            request_control: None,
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base: "/api/playback-providers/direct-url/v1/hls-resources/direct/0",
                claims: &claims,
                resource: "hls-resources/direct/0/*",
            }),
        };
        let action = PlaybackTransportAction::M3u8Rewrite {
            url: mock_public_url(&mock_server, "/redirect/master.m3u8"),
            headers: HashMap::new(),
        };

        let mut stream = playback_transport_action_to_chunk_stream(deps, action, false)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let chunk = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("rewritten manifest should emit one chunk"))?
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let body = std::str::from_utf8(&chunk.data)?.to_string();

        assert_eq!(chunk.status, 200);
        assert!(
            body.contains("/api/playback-providers/direct-url/v1/hls-resources/direct/0/media?")
        );
        assert!(body.contains("targetUrl="));
        Ok(())
    }

    #[tokio::test]
    async fn mpd_rewrite_forwards_headers_and_builds_signed_resource_scopes() -> anyhow::Result<()>
    {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let manifest = r#"<MPD><Location>refresh.mpd?token=next</Location><Period><AdaptationSet><SegmentTemplate initialization="init-$RepresentationID$.m4s" media="video/$RepresentationID$/segment-$Number$.m4s?token=part"/></AdaptationSet></Period></MPD>"#;
        Mock::given(method("GET"))
            .and(path("/dash/manifest.mpd"))
            .and(header("Authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(manifest)
                    .insert_header("Content-Type", "application/dash+xml"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = mock_proxy_client(&mock_server)?;
        let ssrf_guard = test_ssrf_guard();
        let proxy_slice_cache =
            synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
                client.clone(),
                ssrf_guard.clone(),
            )?;
        let signing_key = crate::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-secret-key-for-direct-url-dash",
        )?;
        let claims = crate::proxy_signature::ProxyUrlClaims {
            provider: "direct_url".to_string(),
            version: "v1".to_string(),
            resource: "dash-manifests/direct/0".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: synctv_core::SystemClock.now().timestamp() + 1_800,
            target_url: None,
        };
        let deps = PlaybackTransportExecutorDeps {
            proxy_signing_key: &signing_key,
            proxy_http_client: &client,
            ssrf_guard: &ssrf_guard,
            proxy_slice_cache: &proxy_slice_cache,
            request_control: None,
            hls_rewrite: None,
        };
        let action = PlaybackTransportAction::MpdRewrite {
            url: mock_public_url(&mock_server, "/dash/manifest.mpd"),
            headers: HashMap::from([("Authorization".to_string(), "Bearer secret".to_string())]),
        };

        let mut stream = dash_transport_action_to_chunk_stream(
            deps,
            action,
            DashRewriteSigning {
                resource_base: "/api/playback-providers/direct-url/v1/dash-resources/direct/0",
                resource_prefix: "dash-resources/direct/0",
                claims: &claims,
            },
            false,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let chunk = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("rewritten MPD should emit one chunk"))?
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let body = std::str::from_utf8(&chunk.data)?;
        let root_scope = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "http://cdn.example.com:{}/dash/",
            mock_server.address().port()
        ));
        let refresh_scope = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "http://cdn.example.com:{}/dash/refresh.mpd?token=next",
            mock_server.address().port()
        ));

        assert_eq!(chunk.content_type.as_deref(), Some("application/dash+xml"));
        assert!(body.contains(&format!("/media/{root_scope}/user-1/room-1/")));
        assert!(body.contains("init-$RepresentationID$.m4s"));
        assert!(body.contains("segment-$Number$.m4s?token=part"));
        assert!(body.contains(&format!("/manifest/{refresh_scope}/user-1/room-1/")));

        let generated_claims = crate::proxy_signature::ProxyUrlClaims {
            provider: "bilibili".to_string(),
            version: "v1".to_string(),
            resource: "dash-manifests/dash/proxy".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: synctv_core::SystemClock.now().timestamp() + 1_800,
            target_url: None,
        };
        let deps = PlaybackTransportExecutorDeps {
            proxy_signing_key: &signing_key,
            proxy_http_client: &client,
            ssrf_guard: &ssrf_guard,
            proxy_slice_cache: &proxy_slice_cache,
            request_control: None,
            hls_rewrite: None,
        };
        let action = PlaybackTransportAction::MpdBodyRewrite {
            body: br#"<MPD><Period><AdaptationSet><Representation><BaseURL>https://upos.example/video.m4s?token=private</BaseURL><SegmentBase indexRange="100-200"><Initialization range="0-99"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#.to_vec(),
            source_url: "https://synctv.invalid/bilibili-generated.mpd".to_string(),
        };
        let mut stream = dash_transport_action_to_chunk_stream(
            deps,
            action,
            DashRewriteSigning {
                resource_base: "/api/playback-providers/bilibili/v1/dash-resources/dash",
                resource_prefix: "dash-resources/dash",
                claims: &generated_claims,
            },
            false,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let generated = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("generated MPD should emit one chunk"))?
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let generated_body = std::str::from_utf8(&generated.data)?;
        let generated_scope = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode("https://upos.example/video.m4s?token=private");
        assert!(generated_body.contains(&format!(
            "/api/playback-providers/bilibili/v1/dash-resources/dash/media/{generated_scope}/user-1/room-1/"
        )));
        assert!(generated_body.contains("<SegmentBase indexRange=\"100-200\">"));
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_m3u8_rewrite_signs_each_segment() -> anyhow::Result<()> {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let client = mock_proxy_client(&mock_server)?;
        let ssrf_guard = test_ssrf_guard();
        let proxy_slice_cache =
            synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
                client.clone(),
                ssrf_guard.clone(),
            )?;
        let signing_key = crate::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-secret-key-for-in-memory-m3u8",
        )?;
        let claims = crate::proxy_signature::ProxyUrlClaims {
            provider: "bilibili".to_string(),
            version: "v1".to_string(),
            resource: "hls-resources/durl/0/*".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: synctv_core::SystemClock.now().timestamp() + 1_800,
            target_url: None,
        };
        let deps = PlaybackTransportExecutorDeps {
            proxy_signing_key: &signing_key,
            proxy_http_client: &client,
            ssrf_guard: &ssrf_guard,
            proxy_slice_cache: &proxy_slice_cache,
            request_control: None,
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base: "/api/playback-providers/bilibili/v1/hls-resources/durl/0",
                claims: &claims,
                resource: "hls-resources/durl/0/*",
            }),
        };
        let action = PlaybackTransportAction::M3u8BodyRewrite {
            body: b"#EXTM3U\n#EXTINF:2,\nhttps://cdn.example/part-1.mp4\n#EXT-X-DISCONTINUITY\n#EXTINF:3,\nhttps://cdn.example/part-2.mp4\n#EXT-X-ENDLIST\n".to_vec(),
        };

        let mut stream = playback_transport_action_to_chunk_stream(deps, action, false)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let chunk = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("rewritten manifest should emit one chunk"))?
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let body = std::str::from_utf8(&chunk.data)?;

        assert_eq!(
            body.matches("/api/playback-providers/bilibili/v1/hls-resources/durl/0/media?")
                .count(),
            2
        );
        assert_eq!(body.matches("targetUrl=").count(), 2);
        assert!(!body.contains("https://cdn.example/part-1.mp4\n"));
        Ok(())
    }
}
