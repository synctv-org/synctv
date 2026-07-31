use std::sync::Arc;

use crate::models::{MediaId, RoomId, SourceProvider, UserId};
use crate::provider::{
    AlistHlsResourceRequest, BilibiliDashResourceRequest, BilibiliHlsResourceRequest,
    BilibiliProvider, DirectUrlDashResourceRequest, DirectUrlHlsResourceRequest,
    EmbyHlsResourceRequest, ExecutionControl, PlaybackTransportAction, ProviderAccessService,
    ProviderContext, ProviderError, ProviderSet,
};
use crate::provider::{LiveFlvAccess, PlaybackTransportServices};
use crate::provider::{ProviderStore, ProviderStoreResolver};

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

    pub async fn transcoded_hls_resource_action(
        &self,
        request: AlistHlsResourceRequest<'_>,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .alist
            .get_transcoded_hls_resource(Some(&store), request, request_control)
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

    pub fn dash_resource_action(
        &self,
        request: BilibiliDashResourceRequest<'_>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.bilibili.get_dash_resource(request)
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

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.acfun.get_segment(target_url, range)
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

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.douyin.get_segment(target_url, range)
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

    pub fn segment_action(
        &self,
        target_url: String,
        range: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime.providers.tiktok.get_segment(target_url, range)
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

    pub async fn hls_playlist_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .rtmp
            .get_hls_playlist(Some(&store), version, request_control)
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
            .get_hls_segment(Some(&store), version, segment_name, request_control)
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

    pub async fn hls_playlist_action(
        &self,
        version: &str,
        store: Arc<dyn ProviderStore>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.runtime
            .providers
            .live_proxy
            .get_hls_playlist(Some(&store), version, request_control)
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
            .get_hls_segment(Some(&store), version, segment_name, request_control)
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
    provider_stores: Arc<dyn ProviderStoreResolver>,
    playback_transport_services: Arc<PlaybackTransportServices>,
    provider_access_service: Arc<dyn ProviderAccessService>,
}

impl PlaybackProviderRuntime {
    fn new(deps: PlaybackProviderServiceDeps) -> Self {
        Self {
            providers: deps.providers,
            provider_stores: deps.provider_stores,
            playback_transport_services: deps.playback_transport_services,
            provider_access_service: deps.provider_access_service,
        }
    }

    async fn watch_bilibili_live_danmaku(
        &self,
        request: BilibiliLiveDanmakuRequest<'_>,
    ) -> Result<crate::provider::BilibiliLiveDanmakuStream, ProviderError> {
        let media = self
            .playback_transport_services
            .room_service
            .media_service()
            .get_media(&request.media_id)
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
                crate::models::RoomPermission::BROWSE_LIBRARY,
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
        let credential_owner_id = media.creator_id.unwrap_or(request.actor_user_id);
        let mut ctx = ProviderContext::new("playback-provider")
            .with_user_id(request.actor_user_id)
            .with_credential_owner_id(credential_owner_id)
            .with_room_id(media.room_id)
            .with_media_id(media.id)
            .with_provider_access_service(self.provider_access_service.clone())
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
    pub provider_access_service: Arc<dyn ProviderAccessService>,
}

pub struct BilibiliLiveDanmakuRequest<'a> {
    pub media_id: MediaId,
    pub actor_user_id: UserId,
    pub request_control: Option<&'a ExecutionControl>,
}

fn membership_error_to_provider_error(error: crate::Error) -> ProviderError {
    match error {
        crate::Error::KickCooldownDenied => {
            ProviderError::Authentication(crate::Error::kick_cooldown_denied_message().to_string())
        }
        crate::Error::Authorization(_) => ProviderError::Authentication(
            synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
        ),
        other => core_error_to_provider_error(other),
    }
}

fn core_error_to_provider_error(error: crate::Error) -> ProviderError {
    match error {
        crate::Error::KickCooldownDenied => {
            ProviderError::Authentication(crate::Error::kick_cooldown_denied_message().to_string())
        }
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
