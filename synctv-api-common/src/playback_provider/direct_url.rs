use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::provider::{DirectUrlDashResourceRequest, DirectUrlHlsResourceRequest};
use synctv_core::service::DirectUrlPlaybackProviderService;
use synctv_proto::playback_provider::direct_url::{
    DirectUrlDashManifestResponse, DirectUrlDashResourceResponse, DirectUrlHlsManifestResponse,
    DirectUrlHlsResourceResponse, DirectUrlManifestResourceKind, DirectUrlStreamResponse,
    DirectUrlSubtitleResponse, GetDirectUrlDashManifestRequest, GetDirectUrlDashResourceRequest,
    GetDirectUrlHlsManifestRequest, GetDirectUrlHlsResourceRequest, GetDirectUrlStreamRequest,
    GetDirectUrlSubtitleRequest,
};

use super::common::{
    dash_transport_action_to_chunk_stream, playback_provider_route_base,
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    DashRewriteSigning, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::DirectUrlProvider::NAME;

pub struct DirectUrlPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a DirectUrlPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type DirectUrlStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DirectUrlStreamResponse, ApiError>> + Send + 'static>,
>;
pub type DirectUrlHlsManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<DirectUrlHlsManifestResponse, ApiError>> + Send + 'static,
    >,
>;
pub type DirectUrlHlsResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<DirectUrlHlsResourceResponse, ApiError>> + Send + 'static,
    >,
>;
pub type DirectUrlDashManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<DirectUrlDashManifestResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub type DirectUrlDashResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<DirectUrlDashResourceResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub type DirectUrlSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DirectUrlSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_direct_url_stream(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlStreamRequest,
) -> Result<DirectUrlStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_direct_url_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("streams/{}/{}", req.mode_name, req.url_index),
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
        .stream_action(
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
        chunk.map(|chunk| DirectUrlStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_hls_manifest(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlHlsManifestRequest,
) -> Result<DirectUrlHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_direct_url_access(
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
    let resource_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(&req.rid, "direct-url", &req.version, "hls-resources"),
        urlencoding::encode(&req.mode_name),
        req.url_index
    );
    let resource_prefix = format!("hls-resources/{}/{}/*", req.mode_name, req.url_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&resource_base, &claims, &resource_prefix),
        action,
        false,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DirectUrlHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_hls_resource(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlHlsResourceRequest,
) -> Result<DirectUrlHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = manifest_resource_kind(req.resource_kind)?;
    let kind_name = manifest_resource_kind_name(kind);
    let head = req.head;
    let (store, claims) = verify_direct_url_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "hls-resources/{}/{}/{kind_name}",
                req.mode_name, req.url_index
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
            DirectUrlHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                url_index: req.url_index as usize,
                target_url: &req.target_url,
                is_manifest: kind == DirectUrlManifestResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == DirectUrlManifestResourceKind::Manifest {
        let resource_base = format!(
            "{}/{}/{}",
            playback_provider_route_base(&req.rid, "direct-url", &req.version, "hls-resources"),
            urlencoding::encode(&req.mode_name),
            req.url_index
        );
        let resource_prefix = format!("hls-resources/{}/{}/*", req.mode_name, req.url_index);
        playback_transport_action_to_chunk_stream(
            deps.chunk_deps_with_hls(&resource_base, &claims, &resource_prefix),
            action,
            head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DirectUrlHlsResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_dash_manifest(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlDashManifestRequest,
) -> Result<DirectUrlDashManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_direct_url_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("dash-manifests/{}/{}", req.mode_name, req.url_index),
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
        .dash_manifest_action(
            &req.version,
            &req.mode_name,
            req.url_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let resource_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(&req.rid, "direct-url", &req.version, "dash-resources"),
        urlencoding::encode(&req.mode_name),
        req.url_index
    );
    let resource_prefix = format!("dash-resources/{}/{}", req.mode_name, req.url_index);
    let stream = dash_transport_action_to_chunk_stream(
        deps.chunk_deps(),
        action,
        DashRewriteSigning {
            resource_base: &resource_base,
            resource_prefix: &resource_prefix,
            claims: &claims,
        },
        false,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DirectUrlDashManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_direct_url_dash_resource(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlDashResourceRequest,
) -> Result<DirectUrlDashResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = manifest_resource_kind(req.resource_kind)?;
    let kind_name = manifest_resource_kind_name(kind);
    let (store, claims) = verify_direct_url_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "dash-resources/{}/{}/{kind_name}",
                req.mode_name, req.url_index
            ),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: Some(&req.scope_url),
        },
    )
    .await?;
    let is_manifest = kind == DirectUrlManifestResourceKind::Manifest;
    let action = deps
        .playback_provider_service
        .dash_resource_action(
            DirectUrlDashResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                url_index: req.url_index as usize,
                scope_url: &req.scope_url,
                resource_path: &req.resource_path,
                resource_query: req.resource_query.as_deref(),
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
            playback_provider_route_base(&req.rid, "direct-url", &req.version, "dash-resources"),
            urlencoding::encode(&req.mode_name),
            req.url_index
        );
        let resource_prefix = format!("dash-resources/{}/{}", req.mode_name, req.url_index);
        dash_transport_action_to_chunk_stream(
            deps.chunk_deps(),
            action,
            DashRewriteSigning {
                resource_base: &resource_base,
                resource_prefix: &resource_prefix,
                claims: &claims,
            },
            req.head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| DirectUrlDashResourceResponse { chunk: Some(chunk) })
    })))
}

fn manifest_resource_kind(value: i32) -> Result<DirectUrlManifestResourceKind, ApiError> {
    let kind = DirectUrlManifestResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid manifest resource kind".to_string()))?;
    match kind {
        DirectUrlManifestResourceKind::Media | DirectUrlManifestResourceKind::Manifest => Ok(kind),
        DirectUrlManifestResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Manifest resource kind is required".to_string(),
        )),
    }
}

const fn manifest_resource_kind_name(kind: DirectUrlManifestResourceKind) -> &'static str {
    match kind {
        DirectUrlManifestResourceKind::Media => "media",
        DirectUrlManifestResourceKind::Manifest => "manifest",
        DirectUrlManifestResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_direct_url_subtitle(
    deps: DirectUrlPlaybackProviderDeps<'_>,
    req: GetDirectUrlSubtitleRequest,
) -> Result<DirectUrlSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_direct_url_access(
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
        chunk.map(|chunk| DirectUrlSubtitleResponse { chunk: Some(chunk) })
    })))
}

async fn verify_direct_url_access(
    deps: &DirectUrlPlaybackProviderDeps<'_>,
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

crate::impl_has_playback_provider_access_fields!(DirectUrlPlaybackProviderDeps<'a>);

impl<'a> DirectUrlPlaybackProviderDeps<'a> {
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
