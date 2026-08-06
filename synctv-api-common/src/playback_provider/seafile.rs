use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, SeafileHlsResourceRequest};
use synctv_core::service::SeafilePlaybackProviderService;
use synctv_proto::playback_provider::seafile::{
    GetSeafileHlsManifestRequest, GetSeafileHlsResourceRequest, GetSeafileResourceRequest,
    GetSeafileSubtitleRequest, SeafileHlsManifestResponse, SeafileHlsResourceKind,
    SeafileHlsResourceResponse, SeafileResourceResponse, SeafileSubtitleResponse,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::SeafileProvider::NAME;

pub struct SeafilePlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a SeafilePlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type SeafileResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SeafileResourceResponse, ApiError>> + Send + 'static>,
>;
pub type SeafileHlsManifestResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SeafileHlsManifestResponse, ApiError>> + Send + 'static>,
>;
pub type SeafileHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SeafileHlsResourceResponse, ApiError>> + Send + 'static>,
>;

pub type SeafileSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SeafileSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_seafile_resource(
    deps: SeafilePlaybackProviderDeps<'_>,
    req: GetSeafileResourceRequest,
) -> Result<SeafileResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| SeafileResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_seafile_hls_manifest(
    deps: SeafilePlaybackProviderDeps<'_>,
    req: GetSeafileHlsManifestRequest,
) -> Result<SeafileHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("hls-manifests/{}/{}", req.mode_name, req.media_index),
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
        .hls_manifest_action(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let resource_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(PROVIDER, &req.version, "hls-resources"),
        urlencoding::encode(&req.mode_name),
        req.media_index
    );
    let resource_prefix = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&resource_base, &claims, &resource_prefix),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| SeafileHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_seafile_hls_resource(
    deps: SeafilePlaybackProviderDeps<'_>,
    req: GetSeafileHlsResourceRequest,
) -> Result<SeafileHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = seafile_hls_resource_kind(req.resource_kind)?;
    let kind_name = seafile_hls_resource_kind_name(kind);
    let (store, claims) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "hls-resources/{}/{}/{kind_name}",
                req.mode_name, req.media_index
            ),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: Some(&req.target_url),
        },
    )
    .await?;
    let is_manifest = kind == SeafileHlsResourceKind::Manifest;
    let action = deps
        .playback_provider_service
        .hls_resource_action(
            SeafileHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if is_manifest {
        let resource_base = format!(
            "{}/{}/{}",
            playback_provider_route_base(PROVIDER, &req.version, "hls-resources"),
            urlencoding::encode(&req.mode_name),
            req.media_index
        );
        let resource_prefix = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
        playback_transport_action_to_chunk_stream(
            deps.chunk_deps_with_hls(&resource_base, &claims, &resource_prefix),
            action,
            req.head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| SeafileHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn seafile_hls_resource_kind(value: i32) -> Result<SeafileHlsResourceKind, ApiError> {
    let kind = SeafileHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Seafile HLS resource kind".to_string()))?;
    match kind {
        SeafileHlsResourceKind::Media | SeafileHlsResourceKind::Manifest => Ok(kind),
        SeafileHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Seafile HLS resource kind is required".to_string(),
        )),
    }
}

const fn seafile_hls_resource_kind_name(kind: SeafileHlsResourceKind) -> &'static str {
    match kind {
        SeafileHlsResourceKind::Media => "media",
        SeafileHlsResourceKind::Manifest => "manifest",
        SeafileHlsResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_seafile_subtitle(
    deps: SeafilePlaybackProviderDeps<'_>,
    req: GetSeafileSubtitleRequest,
) -> Result<SeafileSubtitleResponseStream, ApiError> {
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
        chunk.map(|chunk| SeafileSubtitleResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(SeafilePlaybackProviderDeps<'a>);

impl<'a> SeafilePlaybackProviderDeps<'a> {
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
        resource: &'a str,
    ) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base,
                claims,
                resource,
            }),
            ..self.chunk_deps()
        }
    }
}
