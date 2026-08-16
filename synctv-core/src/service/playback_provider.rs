use std::sync::Arc;

use crate::models::{MediaId, RoomId, SourceProvider, UserId};
use crate::provider::ProviderStore;
use crate::provider::{
    AlistFileStreamRequest, AlistHlsResourceRequest, BilibiliDashResourceRequest,
    BilibiliHlsResourceRequest, CloudreveHlsResourceRequest, DirectUrlDashResourceRequest,
    DirectUrlHlsResourceRequest, EmbyHlsResourceRequest, ExecutionControl, HlsResourceRequest,
    PlaybackTransportAction, ProviderAccessService, ProviderError, ProviderSet,
};
use crate::provider::{LiveFlvAccess, PlaybackTransportServices};

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
                AlistFileStreamRequest {
                    version,
                    mode_name,
                    url_index,
                    range_header: range,
                },
                Some(&store),
                self.runtime.provider_access_service.as_ref(),
                request_control,
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
                self.runtime.provider_access_service.as_ref(),
                request_control,
            )
            .await
    }

    pub async fn transcoded_hls_resource_action(
        &self,
        request: AlistHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_transcoded_hls_resource(
                Some(&store),
                request,
                self.runtime.provider_access_service.as_ref(),
                request_control,
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
                self.runtime.provider_access_service.as_ref(),
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

    pub async fn invalidate_playback_access(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<(), ProviderError> {
        self.runtime
            .providers
            .alist
            .invalidate_playback_access(
                Some(&store),
                version,
                self.runtime.provider_access_service.as_ref(),
                request_control,
            )
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

    pub async fn hls_resource_action(
        &self,
        request: BilibiliHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_hls_resource(Some(&store), request, request_control)
            .await
    }

    pub async fn dash_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        mode: crate::provider::BilibiliDashManifestMode,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_dash_manifest(Some(&store), version, mode_name, mode, request_control)
            .await
    }

    pub async fn dash_resource_action(
        &self,
        request: BilibiliDashResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .bilibili
            .get_dash_resource(Some(&store), request, request_control)
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

    pub async fn hls_resource_action(
        &self,
        request: DirectUrlHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .direct_url
            .get_hls_resource(Some(&store), request, request_control)
            .await
    }

    pub async fn dash_manifest_action(
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
            .get_dash_manifest(Some(&store), version, mode_name, url_index, request_control)
            .await
    }

    pub async fn dash_resource_action(
        &self,
        request: DirectUrlDashResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .direct_url
            .get_dash_resource(Some(&store), request, request_control)
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

#[derive(Clone)]
pub struct TwitchPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct YoutubePlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct HuyaPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct DouyuPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct DouyinPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct TikTokPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct AcFunPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct CctvPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct FnosPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct QnapPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct SynologyPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct NextcloudPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct CloudrevePlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct SeafilePlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

#[derive(Clone)]
pub struct TrueNasPlaybackProviderService {
    runtime: Arc<PlaybackProviderRuntime>,
}

impl TrueNasPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .truenas
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .truenas
            .get_hls_manifest(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: crate::provider::TrueNasHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .truenas
            .get_hls_resource(Some(&store), request, request_control)
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
            .truenas
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

impl SeafilePlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .seafile
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .seafile
            .get_hls_manifest(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: crate::provider::SeafileHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .seafile
            .get_hls_resource(Some(&store), request, request_control)
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
            .seafile
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }

    pub async fn thumbnail_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        repository_id: &str,
        path: &str,
        size: u32,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .seafile
            .thumbnail_action(credential_owner_id, server_id, repository_id, path, size)
            .await
    }
}

impl NextcloudPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .nextcloud
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .nextcloud
            .get_hls_manifest(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: crate::provider::NextcloudHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .nextcloud
            .get_hls_resource(Some(&store), request, request_control)
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
            .nextcloud
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }

    pub async fn preview_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        file_id: u64,
        width: u32,
        height: u32,
        crop: bool,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .nextcloud
            .thumbnail_action(credential_owner_id, server_id, file_id, width, height, crop)
            .await
    }
}

impl CloudrevePlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .cloudreve
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .cloudreve
            .get_hls_manifest(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: CloudreveHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .cloudreve
            .get_hls_resource(Some(&store), request, request_control)
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
            .cloudreve
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

impl SynologyPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .synology
            .get_resource(crate::provider::StatefulPlaybackResourceRequest {
                store: Some(&store),
                session_repo: &self
                    .runtime
                    .playback_transport_services
                    .playback_session_repo,
                version,
                mode_name,
                media_index,
                request_context: request_control,
                range_header: range,
            })
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
            .synology
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
            )
            .await
    }

    pub async fn segment_action(
        &self,
        version: &str,
        target_url: String,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .synology
            .get_segment(Some(&store), version, target_url, request_control, range)
            .await
    }

    pub async fn file_image_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        path: &str,
        size: &str,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .synology
            .file_thumbnail_action(credential_owner_id, server_id, path, size)
            .await
    }

    pub async fn poster_image_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        item_id: i64,
        media_type: &str,
        poster_mtime: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .synology
            .poster_action(
                credential_owner_id,
                server_id,
                item_id,
                media_type,
                poster_mtime,
            )
            .await
    }
}

impl QnapPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .qnap
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_manifest_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .qnap
            .get_hls_manifest(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: crate::provider::QnapHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .qnap
            .get_hls_resource(Some(&store), request, request_control)
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
            .qnap
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
            .qnap
            .get_thumbnail(Some(&store), version, request_control)
            .await
    }

    pub async fn thumbnail_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        path: &str,
        size: u32,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .qnap
            .thumbnail_action(credential_owner_id, server_id, path, size)
            .await
    }
}

impl FnosPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .fnos
            .get_resource(crate::provider::StatefulPlaybackResourceRequest {
                store: Some(&store),
                session_repo: &self
                    .runtime
                    .playback_transport_services
                    .playback_session_repo,
                version,
                mode_name,
                media_index,
                request_context: request_control,
                range_header: range,
            })
            .await
    }

    pub async fn segment_action(
        &self,
        version: &str,
        target_url: String,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .fnos
            .get_segment(Some(&store), version, target_url, request_control, range)
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
            .fnos
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
            .fnos
            .get_thumbnail(Some(&store), version, request_control)
            .await
    }

    pub async fn image_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        image_path: &str,
        width: u32,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .fnos
            .image_action(credential_owner_id, server_id, image_path, width)
            .await
    }
}

impl CctvPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resolve_resource(
        &self,
        resource: &str,
    ) -> Result<synctv_media_providers::cctv::CctvMedia, ProviderError> {
        self.runtime.providers.cctv.resolve_resource(resource).await
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .cctv
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.cctv.get_segment(target_url, range)
    }
}

impl AcFunPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resolve_resource(
        &self,
        resource: &str,
    ) -> Result<synctv_media_providers::acfun::AcFunMedia, ProviderError> {
        self.runtime
            .providers
            .acfun
            .resolve_resource(resource)
            .await
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .acfun
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: HlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .acfun
            .get_hls_resource(Some(&store), request, request_control)
            .await
    }

    pub async fn danmaku_file_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .acfun
            .get_danmaku_file(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }

    pub async fn watch_danmaku(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::provider::AcFunDanmakuStream, ProviderError> {
        self.runtime
            .providers
            .acfun
            .watch_danmaku(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }
}

impl DouyuPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resolve_resource(
        &self,
        resource: &str,
    ) -> Result<synctv_media_providers::douyu::DouyuMedia, ProviderError> {
        self.runtime
            .providers
            .douyu
            .resolve_resource(resource)
            .await
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .douyu
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.douyu.get_segment(target_url, range)
    }

    pub async fn watch_danmaku(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::provider::DouyuDanmakuStream, ProviderError> {
        self.runtime
            .providers
            .douyu
            .watch_danmaku(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }
}

impl DouyinPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .douyin
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: HlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .douyin
            .get_hls_resource(Some(&store), request, request_control)
            .await
    }

    pub async fn watch_danmaku(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::provider::DouyinDanmakuStream, ProviderError> {
        self.runtime
            .providers
            .douyin
            .watch_danmaku(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }
}

impl TikTokPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .tiktok
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub async fn hls_resource_action(
        &self,
        request: HlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .tiktok
            .get_hls_resource(Some(&store), request, request_control)
            .await
    }

    pub async fn subtitle_action(
        &self,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .tiktok
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
                range,
            )
            .await
    }
}

impl HuyaPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resolve_resource(
        &self,
        resource: &str,
    ) -> Result<synctv_media_providers::huya::HuyaMedia, ProviderError> {
        self.runtime.providers.huya.resolve_resource(resource).await
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .huya
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.huya.get_segment(target_url, range)
    }

    pub async fn watch_danmaku(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::provider::HuyaDanmakuStream, ProviderError> {
        self.runtime
            .providers
            .huya
            .watch_danmaku(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }
}

impl TwitchPlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .twitch
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.twitch.get_segment(target_url, range)
    }

    pub async fn watch_chat(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::provider::TwitchChatStream, ProviderError> {
        self.runtime
            .providers
            .twitch
            .watch_chat(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
            )
            .await
    }
}

impl YoutubePlaybackProviderService {
    #[must_use]
    pub fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            runtime: Arc::new(PlaybackProviderRuntime::new(deps)),
        }
    }

    pub async fn resource_action(
        &self,
        version: &str,
        mode_name: &str,
        media_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .youtube
            .get_resource(
                Some(&store),
                version,
                mode_name,
                media_index,
                request_control,
                range,
            )
            .await
    }

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .youtube
            .get_segment(target_url, range)
    }

    pub async fn subtitle_action(
        &self,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        range: Option<&str>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .youtube
            .get_subtitle(
                Some(&store),
                version,
                mode_name,
                subtitle_index,
                request_control,
                range,
            )
            .await
    }
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
    pub async fn hls_resource_action(
        &self,
        request: EmbyHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .emby
            .get_hls_resource(Some(&store), request, request_control)
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

    pub async fn thumbnail_resource_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        item_id: &str,
        max_height: u32,
        max_width: u32,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let access = self
            .runtime
            .provider_access_service
            .emby_access(credential_owner_id, server_id, None, request_control)
            .await?;
        crate::provider::EmbyProvider::thumbnail_action(
            item_id,
            &access.host,
            &access.api_key,
            max_height,
            max_width,
        )
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
        access: LiveFlvAccess,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_flv_stream(Some(&store), version, request_control, access)
            .await
    }

    pub async fn hls_master_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_hls_master(Some(&store), version, request_control)
            .await
    }

    pub async fn hls_playlist_action(
        &self,
        version: &str,
        generation_id: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_hls_playlist(Some(&store), version, generation_id, request_control)
            .await
    }

    pub async fn hls_segment_action(
        &self,
        version: &str,
        generation_id: &str,
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
                generation_id,
                segment_name,
                request_control,
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
        access: LiveFlvAccess,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_flv_stream(Some(&store), version, request_control, access)
            .await
    }

    pub async fn hls_master_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_hls_master(Some(&store), version, request_control)
            .await
    }

    pub async fn hls_playlist_action(
        &self,
        version: &str,
        generation_id: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_hls_playlist(Some(&store), version, generation_id, request_control)
            .await
    }

    pub async fn hls_segment_action(
        &self,
        version: &str,
        generation_id: &str,
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
                generation_id,
                segment_name,
                request_control,
            )
            .await
    }

    pub async fn source_config_for_media(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Option<crate::models::LiveProxyMediaSourceConfig> {
        self.runtime
            .playback_transport_services
            .room_service
            .media_service()
            .get_room_media(room_id, media_id)
            .await
            .ok()
            .flatten()
            .filter(|media| media.source_provider == SourceProvider::LiveProxy)
            .and_then(|media| match media.source_config {
                crate::models::MediaSourceConfig::LiveProxy(config) => Some(config),
                _ => None,
            })
    }
}

#[derive(Clone)]
struct PlaybackProviderRuntime {
    providers: ProviderSet,
    playback_transport_services: Arc<PlaybackTransportServices>,
    provider_access_service: Arc<dyn ProviderAccessService>,
}

impl PlaybackProviderRuntime {
    fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            providers: deps.providers,
            playback_transport_services: deps.playback_transport_services,
            provider_access_service: deps.provider_access_service,
        }
    }
}

#[derive(Clone)]
pub struct PlaybackProviderServiceDeps {
    pub providers: ProviderSet,
    pub playback_transport_services: Arc<PlaybackTransportServices>,
    pub provider_access_service: Arc<dyn ProviderAccessService>,
}
