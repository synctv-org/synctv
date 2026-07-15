use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::TrueNasPlaybackProviderService;
use synctv_proto::playback_provider::truenas::{
    GetTrueNasResourceRequest, GetTrueNasSubtitleRequest, TrueNasResourceResponse,
    TrueNasSubtitleResponse,
};

use super::common::{
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    HasPlaybackProviderAccessFields, PlaybackProviderAccessRequest, PlaybackProviderApiRuntime,
    PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::TrueNasProvider::NAME;

pub struct TrueNasPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a TrueNasPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type TrueNasResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TrueNasResourceResponse, ApiError>> + Send + 'static>,
>;

pub type TrueNasSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TrueNasSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_truenas_resource(
    deps: TrueNasPlaybackProviderDeps<'_>,
    req: GetTrueNasResourceRequest,
) -> Result<TrueNasResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("resources/{}/{}", req.mode_name, req.media_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .resource_action(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TrueNasResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_truenas_subtitle(
    deps: TrueNasPlaybackProviderDeps<'_>,
    req: GetTrueNasSubtitleRequest,
) -> Result<TrueNasSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("subtitles/{}/{}", req.mode_name, req.subtitle_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .subtitle_action(
            &req.version,
            &req.mode_name,
            req.subtitle_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TrueNasSubtitleResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(TrueNasPlaybackProviderDeps<'a>);

impl<'a> TrueNasPlaybackProviderDeps<'a> {
    fn chunk_deps(&self) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            proxy_signing_key: self.runtime.proxy_signing_key,
            proxy_http_client: self.runtime.proxy_http_client,
            ssrf_guard: self.runtime.ssrf_guard,
            proxy_slice_cache: self.runtime.proxy_slice_cache,
            request_control: self.request_control,
            hls_rewrite: None,
        }
    }
}
