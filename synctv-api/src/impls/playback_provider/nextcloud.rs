use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::NextcloudPlaybackProviderService;
use synctv_proto::playback_provider::nextcloud::{
    GetNextcloudResourceRequest, GetNextcloudSubtitleRequest, NextcloudResourceResponse,
    NextcloudSubtitleResponse,
};

use super::common::{
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    HasPlaybackProviderAccessFields, PlaybackProviderAccessRequest, PlaybackProviderApiRuntime,
    PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::NextcloudProvider::NAME;

pub struct NextcloudPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a NextcloudPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type NextcloudResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<NextcloudResourceResponse, ApiError>> + Send + 'static>,
>;

pub type NextcloudSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<NextcloudSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_nextcloud_resource(
    deps: NextcloudPlaybackProviderDeps<'_>,
    req: GetNextcloudResourceRequest,
) -> Result<NextcloudResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| NextcloudResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_nextcloud_subtitle(
    deps: NextcloudPlaybackProviderDeps<'_>,
    req: GetNextcloudSubtitleRequest,
) -> Result<NextcloudSubtitleResponseStream, ApiError> {
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
        chunk.map(|chunk| NextcloudSubtitleResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(NextcloudPlaybackProviderDeps<'a>);

impl<'a> NextcloudPlaybackProviderDeps<'a> {
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
