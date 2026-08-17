use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::CloudrevePlaybackProviderService;
use synctv_proto::playback_provider::cloudreve::{
    CloudreveHlsManifestResponse, CloudreveHlsResourceKind, CloudreveHlsResourceResponse,
    CloudreveResourceResponse, CloudreveSubtitleResponse, GetCloudreveHlsManifestRequest,
    GetCloudreveHlsResourceRequest, GetCloudreveResourceRequest, GetCloudreveSubtitleRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::CloudreveProvider::NAME;

pub struct CloudrevePlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a CloudrevePlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type CloudreveResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<CloudreveResourceResponse, ApiError>> + Send + 'static>,
>;
pub type CloudreveHlsManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<CloudreveHlsManifestResponse, ApiError>> + Send + 'static,
    >,
>;
pub type CloudreveHlsResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<CloudreveHlsResourceResponse, ApiError>> + Send + 'static,
    >,
>;
pub type CloudreveSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<CloudreveSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_cloudreve_resource(
    deps: CloudrevePlaybackProviderDeps<'_>,
    req: GetCloudreveResourceRequest,
) -> Result<CloudreveResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| CloudreveResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_cloudreve_hls_manifest(
    deps: CloudrevePlaybackProviderDeps<'_>,
    req: GetCloudreveHlsManifestRequest,
) -> Result<CloudreveHlsManifestResponseStream, ApiError> {
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
    let (segment_base, resource) =
        cloudreve_hls_rewrite_routes(&req.rid, &req.version, &req.mode_name, req.media_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| CloudreveHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_cloudreve_hls_resource(
    deps: CloudrevePlaybackProviderDeps<'_>,
    req: GetCloudreveHlsResourceRequest,
) -> Result<CloudreveHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = cloudreve_hls_resource_kind(req.resource_kind)?;
    let kind_name = cloudreve_hls_resource_kind_name(kind);
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
    let action = deps
        .playback_provider_service
        .hls_resource_action(
            synctv_core::provider::CloudreveHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest: kind == CloudreveHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == CloudreveHlsResourceKind::Manifest {
        let (segment_base, resource) =
            cloudreve_hls_rewrite_routes(&req.rid, &req.version, &req.mode_name, req.media_index);
        playback_transport_action_to_chunk_stream(
            deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
            action,
            req.head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| CloudreveHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn cloudreve_hls_resource_kind(value: i32) -> Result<CloudreveHlsResourceKind, ApiError> {
    let kind = CloudreveHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Cloudreve HLS resource kind".to_string()))?;
    match kind {
        CloudreveHlsResourceKind::Media | CloudreveHlsResourceKind::Manifest => Ok(kind),
        CloudreveHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Cloudreve HLS resource kind is required".to_string(),
        )),
    }
}

const fn cloudreve_hls_resource_kind_name(kind: CloudreveHlsResourceKind) -> &'static str {
    match kind {
        CloudreveHlsResourceKind::Media => "media",
        CloudreveHlsResourceKind::Manifest => "manifest",
        CloudreveHlsResourceKind::Unspecified => "unspecified",
    }
}

fn cloudreve_hls_rewrite_routes(
    room_id: &str,
    version: &str,
    mode_name: &str,
    media_index: u32,
) -> (String, String) {
    (
        format!(
            "{}/{}/{}",
            playback_provider_route_base(room_id, PROVIDER, version, "hls-resources"),
            urlencoding::encode(mode_name),
            media_index
        ),
        format!("hls-resources/{mode_name}/{media_index}/*"),
    )
}

pub async fn get_cloudreve_subtitle(
    deps: CloudrevePlaybackProviderDeps<'_>,
    req: GetCloudreveSubtitleRequest,
) -> Result<CloudreveSubtitleResponseStream, ApiError> {
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
        chunk.map(|chunk| CloudreveSubtitleResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(CloudrevePlaybackProviderDeps<'a>);

impl<'a> CloudrevePlaybackProviderDeps<'a> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_resource_kind_requires_a_typed_route() {
        assert_eq!(
            cloudreve_hls_resource_kind(CloudreveHlsResourceKind::Media as i32)
                .expect("media kind should be valid"),
            CloudreveHlsResourceKind::Media
        );
        assert!(cloudreve_hls_resource_kind(CloudreveHlsResourceKind::Unspecified as i32).is_err());
    }
}
