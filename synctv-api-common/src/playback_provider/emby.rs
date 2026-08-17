use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::EmbyPlaybackProviderService;
use synctv_proto::playback_provider::emby::{
    EmbyHlsManifestResponse, EmbyHlsResourceKind, EmbyHlsResourceResponse, EmbyMediaStreamResponse,
    EmbySubtitleResponse, EmbyThumbnailResourceResponse, GetEmbyHlsManifestRequest,
    GetEmbyHlsResourceRequest, GetEmbyMediaStreamRequest, GetEmbySubtitleRequest,
    GetEmbyThumbnailResourceRequest,
};

use super::common::{
    decode_playback_resource_owner, playback_provider_route_base,
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    verify_playback_resource_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::EmbyProvider::NAME;

pub struct EmbyPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a EmbyPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type EmbyMediaStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<EmbyMediaStreamResponse, ApiError>> + Send + 'static>,
>;
pub type EmbyHlsManifestResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<EmbyHlsManifestResponse, ApiError>> + Send + 'static>,
>;
pub type EmbyHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<EmbyHlsResourceResponse, ApiError>> + Send + 'static>,
>;
pub type EmbySubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<EmbySubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type EmbyThumbnailResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<EmbyThumbnailResourceResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub async fn get_emby_media_stream(
    deps: EmbyPlaybackProviderDeps<'_>,
    req: GetEmbyMediaStreamRequest,
) -> Result<EmbyMediaStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_emby_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("media-streams/{}/{}", req.mode_name, req.url_index),
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
        .media_stream_action(
            &req.version,
            &req.mode_name,
            req.url_index as usize,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| EmbyMediaStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_emby_hls_manifest(
    deps: EmbyPlaybackProviderDeps<'_>,
    req: GetEmbyHlsManifestRequest,
) -> Result<EmbyHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_emby_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("hls-manifests/{}/{}", req.mode_name, req.url_index),
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
            req.url_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(&req.rid, "emby", &req.version, "hls-resources"),
        urlencoding::encode(&req.mode_name),
        req.url_index
    );
    let resource = format!("hls-resources/{}/{}/*", req.mode_name, req.url_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
        action,
        false,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| EmbyHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_emby_hls_resource(
    deps: EmbyPlaybackProviderDeps<'_>,
    req: GetEmbyHlsResourceRequest,
) -> Result<EmbyHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = emby_hls_resource_kind(req.resource_kind)?;
    let kind_name = emby_hls_resource_kind_name(kind);
    let head = req.head;
    let (store, claims) = verify_emby_access(
        &deps,
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
    let action = deps
        .playback_provider_service
        .hls_resource_action(
            synctv_core::provider::EmbyHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest: kind == EmbyHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == EmbyHlsResourceKind::Manifest {
        let segment_base = format!(
            "{}/{}/{}",
            playback_provider_route_base(&req.rid, "emby", &req.version, "hls-resources"),
            urlencoding::encode(&req.mode_name),
            req.media_index
        );
        let resource = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
        playback_transport_action_to_chunk_stream(
            deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
            action,
            head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| EmbyHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn emby_hls_resource_kind(value: i32) -> Result<EmbyHlsResourceKind, ApiError> {
    let kind = EmbyHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Emby HLS resource kind".to_string()))?;
    match kind {
        EmbyHlsResourceKind::Media | EmbyHlsResourceKind::Manifest => Ok(kind),
        EmbyHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Emby HLS resource kind is required".to_string(),
        )),
    }
}

const fn emby_hls_resource_kind_name(kind: EmbyHlsResourceKind) -> &'static str {
    match kind {
        EmbyHlsResourceKind::Media => "media",
        EmbyHlsResourceKind::Manifest => "manifest",
        EmbyHlsResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_emby_subtitle(
    deps: EmbyPlaybackProviderDeps<'_>,
    req: GetEmbySubtitleRequest,
) -> Result<EmbySubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_emby_access(
        &deps,
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
        chunk.map(|chunk| EmbySubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_emby_thumbnail_resource(
    deps: EmbyPlaybackProviderDeps<'_>,
    req: GetEmbyThumbnailResourceRequest,
) -> Result<EmbyThumbnailResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let scope = crate::emby_thumbnail_urls::ThumbnailSignatureScope {
        item_id: &req.item_id,
        server_id: &req.server_id,
        credential_owner_id: &req.credential_owner_id,
        max_height: req.max_height,
        max_width: req.max_width,
    };
    let version = crate::emby_thumbnail_urls::thumbnail_signature_version(scope);
    verify_playback_resource_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &version,
            resource: "thumbnail".to_string(),
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
    let action = deps
        .playback_provider_service
        .thumbnail_resource_action(
            credential_owner_id,
            &req.server_id,
            &req.item_id,
            req.max_height,
            req.max_width,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| EmbyThumbnailResourceResponse { chunk: Some(chunk) })
    })))
}

async fn verify_emby_access(
    deps: &EmbyPlaybackProviderDeps<'_>,
    request: PlaybackProviderAccessRequest<'_>,
) -> Result<
    (
        std::sync::Arc<dyn synctv_core::provider::ProviderStore>,
        crate::proxy_signature::ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_access_with_deps(&deps.access_deps(), PROVIDER, request).await
}

crate::impl_has_playback_provider_access_fields!(EmbyPlaybackProviderDeps<'a>);

impl<'a> EmbyPlaybackProviderDeps<'a> {
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
