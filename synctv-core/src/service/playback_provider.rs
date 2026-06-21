use std::sync::Arc;

use crate::models::{LiveProxyMediaSourceConfig, MediaId, RoomId, SourceProvider, UserId};
use crate::provider::playback_transport::PlaybackTransportServices;
use crate::provider::store::{ProviderStore, ProviderStoreResolver};
use crate::provider::{
    BilibiliProvider, ExecutionControl, PlaybackTransportAction, ProviderAccessService,
    ProviderContext, ProviderError, ProviderSet,
};
use crate::proxy_signature::{ProxySigningKey, ProxyUrlClaims};
use crate::{PublicIdCodec, PublicIdType};

#[derive(Clone)]
pub struct AlistPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl AlistPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn file_stream_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_file_stream(
                Some(&store),
                version,
                mode_name,
                url_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn transcoded_hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_transcoded_hls_manifest(
                Some(&store),
                version,
                mode_name,
                url_index,
                request_control,
            )
            .await
    }

    pub async fn transcoded_hls_segment_action(
        &self,
        version: &str,
        target_url: &str,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_transcoded_hls_segment(
                Some(&store),
                version,
                target_url.to_string(),
                request_control,
                range,
            )
            .await
    }

    pub async fn subtitle_action(
        &self,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }

    pub async fn thumbnail_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_thumbnail(Some(&store), version, request_control)
            .await
    }
}

#[derive(Clone)]
pub struct BilibiliPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl BilibiliPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn media_stream_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_media_stream(
                Some(&store),
                version,
                mode_name,
                url_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_hls_manifest(Some(&store), version, mode_name, url_index, request_control)
            .await
    }

    pub async fn hls_segment_action(
        &self,
        version: &str,
        target_url: &str,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_hls_segment(
                Some(&store),
                version,
                target_url.to_string(),
                request_control,
                range,
            )
            .await
    }

    pub async fn dash_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        mode: crate::provider::bilibili::BilibiliDashManifestMode,
        store: Arc<dyn ProviderStore>,
        proxy_url_for: Option<&mut crate::provider::bilibili::BilibiliDashProxyUrlMapper<'_>>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_dash_manifest(
                Some(&store),
                version,
                mode_name,
                mode,
                request_control,
                proxy_url_for,
            )
            .await
    }

    pub async fn dash_segment_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_dash_segment(
                Some(&store),
                version,
                mode_name,
                url_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn subtitle_action(
        &self,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }

    pub async fn danmaku_file_action(
        &self,
        version: &str,
        danmaku_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_danmaku_file(Some(&store), version, danmaku_index, request_control)
            .await
    }

    pub async fn watch_live_danmaku(
        &self,
        request: BilibiliLiveDanmakuRequest<'_>,
    ) -> Result<crate::provider::BilibiliLiveDanmakuStream, ProviderError> {
        self.runtime.watch_bilibili_live_danmaku(request).await
    }
}

#[derive(Clone)]
pub struct DirectUrlPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl DirectUrlPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn stream_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .direct_url
            .get_stream(
                Some(&store),
                version,
                mode_name,
                url_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .direct_url
            .get_hls_manifest(Some(&store), version, mode_name, url_index, request_control)
            .await
    }

    pub async fn hls_segment_action(
        &self,
        version: &str,
        target_url: &str,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .direct_url
            .get_hls_segment(
                Some(&store),
                version,
                target_url.to_string(),
                request_control,
                range,
            )
            .await
    }

    pub async fn subtitle_action(
        &self,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .direct_url
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }
}

#[derive(Clone)]
pub struct EmbyPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl EmbyPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn media_stream_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .emby
            .get_media_stream(
                Some(&store),
                version,
                mode_name,
                url_index,
                request_control,
                range,
            )
            .await
    }
    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        url_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .emby
            .get_hls_manifest(Some(&store), version, mode_name, url_index, request_control)
            .await
    }
    pub async fn hls_segment_action(
        &self,
        version: &str,
        target_url: &str,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .emby
            .get_hls_segment(
                Some(&store),
                version,
                target_url.to_string(),
                request_control,
                range,
            )
            .await
    }
    pub async fn subtitle_action(
        &self,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .emby
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }
}

#[derive(Clone)]
pub struct RtmpPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl RtmpPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn flv_stream_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        claims: &ProxyUrlClaims,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_flv_stream(
                Some(&store),
                version,
                request_control,
                claims,
                &self.runtime.public_id_codec,
            )
            .await
    }

    pub async fn hls_playlist_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_hls_playlist(
                Some(&store),
                version,
                request_control,
                &self.runtime.public_id_codec,
            )
            .await
    }

    pub async fn hls_segment_action(
        &self,
        version: &str,
        segment_name: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_hls_segment(
                Some(&store),
                version,
                segment_name,
                request_control,
                &self.runtime.public_id_codec,
            )
            .await
    }
}

#[derive(Clone)]
pub struct LiveProxyPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl LiveProxyPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn flv_stream_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        claims: &ProxyUrlClaims,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_flv_stream(
                Some(&store),
                version,
                request_control,
                claims,
                &self.runtime.public_id_codec,
            )
            .await
    }

    pub async fn hls_playlist_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_hls_playlist(
                Some(&store),
                version,
                request_control,
                &self.runtime.public_id_codec,
            )
            .await
    }

    pub async fn hls_segment_action(
        &self,
        version: &str,
        segment_name: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_hls_segment(
                Some(&store),
                version,
                segment_name,
                request_control,
                &self.runtime.public_id_codec,
            )
            .await
    }

    pub async fn source_url_for_media(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Option<String> {
        self.runtime
            .playback_transport_services
            .room_service
            .media_service()
            .get_room_media(room_id, media_id)
            .await
            .ok()
            .flatten()
            .filter(|media| media.source_provider == SourceProvider::LiveProxy)
            .and_then(|media| {
                serde_json::from_value::<LiveProxyMediaSourceConfig>(media.source_config)
                    .ok()
                    .map(|config| config.url)
            })
    }
}

#[derive(Clone)]
struct PlaybackProviderRuntime {
    providers: ProviderSet,
    provider_stores: Arc<dyn ProviderStoreResolver>,
    playback_transport_services: Arc<PlaybackTransportServices>,
    public_id_codec: Arc<PublicIdCodec>,
    signing_key: Arc<ProxySigningKey>,
    provider_access_service: Arc<dyn ProviderAccessService>,
}

impl PlaybackProviderRuntime {
    fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            providers: deps.providers,
            provider_stores: deps.provider_stores,
            playback_transport_services: deps.playback_transport_services,
            public_id_codec: deps.public_id_codec,
            signing_key: deps.signing_key,
            provider_access_service: deps.provider_access_service,
        }
    }

    async fn watch_bilibili_live_danmaku(
        &self,
        request: BilibiliLiveDanmakuRequest<'_>,
    ) -> Result<crate::provider::BilibiliLiveDanmakuStream, ProviderError> {
        let media_id =
            parse_public_id::<MediaId>(&self.public_id_codec, request.media_id, "media_id")?;
        let media = self
            .playback_transport_services
            .room_service
            .media_service()
            .get_media(&media_id)
            .await
            .map_err(core_error_to_provider_error)?
            .ok_or(ProviderError::NotFound)?;
        if media.source_provider != SourceProvider::Bilibili {
            return Err(ProviderError::InvalidConfig(
                "Bilibili live danmaku requires Bilibili media".to_string(),
            ));
        }
        self.playback_transport_services
            .room_service
            .check_membership(&media.room_id, &request.actor_user_id)
            .await
            .map_err(membership_error_to_provider_error)?;
        self.playback_transport_services
            .permission_service
            .check_permission(
                &media.room_id,
                &request.actor_user_id,
                crate::models::RoomPermission::VIEW_MEDIA_RESOURCES,
            )
            .await
            .map_err(core_error_to_provider_error)?;
        let provider = self
            .playback_transport_services
            .room_service
            .media_service()
            .providers_manager()
            .resolve_provider(
                SourceProvider::Bilibili,
                media.provider_instance_name.as_deref(),
            )
            .await
            .map_err(core_error_to_provider_error)?;
        let live_danmaku_provider =
            provider
                .as_bilibili_live_danmaku_provider()
                .ok_or_else(|| {
                    ProviderError::ApiError(
                        "Bilibili provider does not expose live danmaku".to_string(),
                    )
                })?;
        let public_user_id = self
            .public_id_codec
            .encode_user_id(request.actor_user_id)
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to encode user public id: {error}"))
            })?;
        let public_room_id =
            self.public_id_codec
                .encode_room_id(media.room_id)
                .map_err(|error| {
                    ProviderError::Internal(format!("Failed to encode room public id: {error}"))
                })?;
        let public_media_id = self
            .public_id_codec
            .encode_media_id(media.id)
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to encode media public id: {error}"))
            })?;
        let credential_owner_id = media.creator_id.unwrap_or(request.actor_user_id);
        let public_credential_owner_id = self
            .public_id_codec
            .encode_user_id(credential_owner_id)
            .map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to encode credential owner public id: {error}"
            ))
        })?;
        let mut ctx = ProviderContext::new("playback-provider")
            .with_user_id(request.actor_user_id)
            .with_public_user_id(public_user_id)
            .with_credential_owner_id(credential_owner_id)
            .with_public_credential_owner_id(public_credential_owner_id)
            .with_room_id(media.room_id)
            .with_public_room_id(public_room_id)
            .with_media_id(media.id)
            .with_public_media_id(public_media_id)
            .with_provider_access_service(self.provider_access_service.clone())
            .with_signing_key(&self.signing_key)
            .with_store(self.provider_stores.load(BilibiliProvider::NAME))
            .with_request_context(request.request_control.map(ExecutionControl::child));
        if let Some(provider_instance_name) = media.provider_instance_name.as_deref() {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(repo) = self
            .playback_transport_services
            .room_service
            .media_service()
            .credential_repo()
        {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self
            .playback_transport_services
            .room_service
            .media_service()
            .credential_encryption()
        {
            ctx = ctx.with_credential_encryption(enc);
        }
        live_danmaku_provider
            .watch_bilibili_live_danmaku(&ctx, &media.source_config)
            .await
    }
}

#[derive(Clone)]
pub struct PlaybackProviderServiceDeps {
    pub providers: ProviderSet,
    pub provider_stores: Arc<dyn ProviderStoreResolver>,
    pub playback_transport_services: Arc<PlaybackTransportServices>,
    pub public_id_codec: Arc<PublicIdCodec>,
    pub signing_key: Arc<ProxySigningKey>,
    pub provider_access_service: Arc<dyn ProviderAccessService>,
}

pub struct BilibiliLiveDanmakuRequest<'a> {
    pub media_id: &'a str,
    pub actor_user_id: UserId,
    pub request_control: Option<&'a ExecutionControl>,
}

fn parse_public_id<T>(
    codec: &PublicIdCodec,
    value: &str,
    field: &'static str,
) -> Result<T, ProviderError>
where
    T: PublicIdType,
{
    codec
        .decode::<T>(value)
        .map_err(|error| ProviderError::InvalidConfig(format!("Invalid {field}: {error}")))
}

fn membership_error_to_provider_error(error: crate::Error) -> ProviderError {
    match error {
        crate::Error::Authorization(message)
            if message == crate::repository::room_member::KICK_COOLDOWN_DENIED_MESSAGE =>
        {
            ProviderError::Authentication(message)
        }
        crate::Error::Authorization(_) => ProviderError::Authentication(
            synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
        ),
        other => core_error_to_provider_error(other),
    }
}

fn core_error_to_provider_error(error: crate::Error) -> ProviderError {
    match error {
        crate::Error::Authentication(message) | crate::Error::Authorization(message) => {
            ProviderError::Authentication(message)
        }
        crate::Error::NotFound(_) => ProviderError::NotFound,
        crate::Error::InvalidInput(message) => ProviderError::InvalidConfig(message),
        crate::Error::RangeNotSatisfiable { total_size } => {
            ProviderError::InvalidConfig(format!("Range not satisfiable: total size {total_size}"))
        }
        crate::Error::RateLimited(message) => ProviderError::ApiError(message),
        crate::Error::ServiceUnavailable(message) | crate::Error::Timeout(message) => {
            ProviderError::NetworkError(message)
        }
        crate::Error::Internal(message)
        | crate::Error::AlreadyExists(message)
        | crate::Error::Conflict(message)
        | crate::Error::LockConflict(message) => ProviderError::Internal(message),
        crate::Error::Serialization(error) => ProviderError::Internal(error.to_string()),
        crate::Error::Deserialization { context } => ProviderError::Internal(context),
        crate::Error::Database(error) => ProviderError::Internal(error.to_string()),
        crate::Error::Redis(error) => ProviderError::Internal(error.to_string()),
        crate::Error::OptimisticLockConflict => {
            ProviderError::Internal("Optimistic lock conflict".to_string())
        }
    }
}
