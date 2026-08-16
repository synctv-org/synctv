use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::SynologyPlaybackProviderService;
use synctv_proto::playback_provider::synology::{
    get_synology_image_resource_request, GetSynologyImageResourceRequest,
    GetSynologyResourceRequest, GetSynologySegmentRequest, GetSynologySubtitleRequest,
    SynologyImageResourceResponse, SynologyResourceResponse, SynologySegmentResponse,
    SynologySubtitleResponse,
};

use super::common::{
    decode_playback_resource_owner, playback_provider_route_base,
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    verify_playback_resource_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::SynologyProvider::NAME;

pub struct SynologyPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a SynologyPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type SynologyResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SynologyResourceResponse, ApiError>> + Send + 'static>,
>;
pub type SynologySubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SynologySubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type SynologySegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SynologySegmentResponse, ApiError>> + Send + 'static>,
>;
pub type SynologyImageResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<SynologyImageResourceResponse, ApiError>>
            + Send
            + 'static,
    >,
>;

pub async fn get_synology_resource(
    deps: SynologyPlaybackProviderDeps<'_>,
    req: GetSynologyResourceRequest,
) -> Result<SynologyResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_playback_provider_access_with_deps(
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
    let segment_base = playback_provider_route_base(&req.rid, PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| SynologyResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_synology_segment(
    deps: SynologyPlaybackProviderDeps<'_>,
    req: GetSynologySegmentRequest,
) -> Result<SynologySegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "segments".to_string(),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: Some(&req.target_url),
        },
    )
    .await?;
    let action = deps
        .playback_provider_service
        .segment_action(
            &req.version,
            req.target_url,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base = playback_provider_route_base(&req.rid, PROVIDER, &req.version, "segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| SynologySegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_synology_subtitle(
    deps: SynologyPlaybackProviderDeps<'_>,
    req: GetSynologySubtitleRequest,
) -> Result<SynologySubtitleResponseStream, ApiError> {
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
        chunk.map(|chunk| SynologySubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_synology_image_resource(
    deps: SynologyPlaybackProviderDeps<'_>,
    req: GetSynologyImageResourceRequest,
) -> Result<SynologyImageResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let image = req
        .image
        .ok_or_else(|| ApiError::InvalidInput("Synology image resource is required".to_string()))?;
    let scope = match &image {
        get_synology_image_resource_request::Image::File(file) => {
            crate::synology_image_urls::SynologyImageScope::File {
                server_id: &req.server_id,
                credential_owner_id: &req.credential_owner_id,
                path: &file.path,
                size: &file.size,
            }
        }
        get_synology_image_resource_request::Image::Poster(poster) => {
            crate::synology_image_urls::SynologyImageScope::Poster {
                server_id: &req.server_id,
                credential_owner_id: &req.credential_owner_id,
                item_id: poster.item_id,
                media_type: &poster.media_type,
                poster_mtime: poster.poster_mtime.as_deref(),
            }
        }
    };
    let version = crate::synology_image_urls::signature_version(scope);
    verify_playback_resource_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &version,
            resource: "image".to_string(),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let credential_owner_id =
        decode_playback_resource_owner(deps.runtime.public_id_codec, &req.credential_owner_id)?;
    let action = match image {
        get_synology_image_resource_request::Image::File(file) => {
            deps.playback_provider_service
                .file_image_resource_action(
                    credential_owner_id,
                    &req.server_id,
                    &file.path,
                    &file.size,
                )
                .await
        }
        get_synology_image_resource_request::Image::Poster(poster) => {
            deps.playback_provider_service
                .poster_image_resource_action(
                    credential_owner_id,
                    &req.server_id,
                    poster.item_id,
                    &poster.media_type,
                    poster.poster_mtime.as_deref(),
                )
                .await
        }
    }
    .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| SynologyImageResourceResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(SynologyPlaybackProviderDeps<'a>);

impl<'a> SynologyPlaybackProviderDeps<'a> {
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

    fn chunk_deps_with_hls(
        &self,
        segment_base: &'a str,
        claims: &'a crate::proxy_signature::ProxyUrlClaims,
    ) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base,
                claims,
                resource: "segments",
            }),
            ..self.chunk_deps()
        }
    }
}
