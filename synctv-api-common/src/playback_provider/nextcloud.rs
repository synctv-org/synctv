use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::NextcloudPlaybackProviderService;
use synctv_proto::playback_provider::nextcloud::{
    GetNextcloudHlsManifestRequest, GetNextcloudHlsResourceRequest, GetNextcloudResourceRequest,
    GetNextcloudSubtitleRequest, NextcloudHlsManifestResponse, NextcloudHlsResourceKind,
    NextcloudHlsResourceResponse, NextcloudResourceResponse, NextcloudSubtitleResponse,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
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

pub type NextcloudHlsManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<NextcloudHlsManifestResponse, ApiError>> + Send + 'static,
    >,
>;

pub type NextcloudHlsResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<NextcloudHlsResourceResponse, ApiError>> + Send + 'static,
    >,
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

pub async fn get_nextcloud_hls_manifest(
    deps: NextcloudPlaybackProviderDeps<'_>,
    req: GetNextcloudHlsManifestRequest,
) -> Result<NextcloudHlsManifestResponseStream, ApiError> {
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
        nextcloud_hls_rewrite_routes(&req.version, &req.mode_name, req.media_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| NextcloudHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_nextcloud_hls_resource(
    deps: NextcloudPlaybackProviderDeps<'_>,
    req: GetNextcloudHlsResourceRequest,
) -> Result<NextcloudHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = nextcloud_hls_resource_kind(req.resource_kind)?;
    let kind_name = nextcloud_hls_resource_kind_name(kind);
    let head = req.head;
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
            synctv_core::provider::NextcloudHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest: kind == NextcloudHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == NextcloudHlsResourceKind::Manifest {
        let (segment_base, resource) =
            nextcloud_hls_rewrite_routes(&req.version, &req.mode_name, req.media_index);
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
        chunk.map(|chunk| NextcloudHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn nextcloud_hls_resource_kind(value: i32) -> Result<NextcloudHlsResourceKind, ApiError> {
    let kind = NextcloudHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Nextcloud HLS resource kind".to_string()))?;
    match kind {
        NextcloudHlsResourceKind::Media | NextcloudHlsResourceKind::Manifest => Ok(kind),
        NextcloudHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Nextcloud HLS resource kind is required".to_string(),
        )),
    }
}

const fn nextcloud_hls_resource_kind_name(kind: NextcloudHlsResourceKind) -> &'static str {
    match kind {
        NextcloudHlsResourceKind::Media => "media",
        NextcloudHlsResourceKind::Manifest => "manifest",
        NextcloudHlsResourceKind::Unspecified => "unspecified",
    }
}

fn nextcloud_hls_rewrite_routes(
    version: &str,
    mode_name: &str,
    media_index: u32,
) -> (String, String) {
    (
        format!(
            "{}/{}/{}",
            playback_provider_route_base(PROVIDER, version, "hls-resources"),
            urlencoding::encode(mode_name),
            media_index
        ),
        format!("hls-resources/{mode_name}/{media_index}/*"),
    )
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
            nextcloud_hls_resource_kind(NextcloudHlsResourceKind::Media as i32)
                .expect("media kind should be valid"),
            NextcloudHlsResourceKind::Media
        );
        assert_eq!(
            nextcloud_hls_resource_kind_name(NextcloudHlsResourceKind::Manifest),
            "manifest"
        );
        assert!(nextcloud_hls_resource_kind(NextcloudHlsResourceKind::Unspecified as i32).is_err());
        assert!(nextcloud_hls_resource_kind(i32::MAX).is_err());
    }

    #[test]
    fn hls_rewrite_route_uses_an_encoded_http_path_and_signed_wildcard() {
        let (segment_base, resource) = nextcloud_hls_rewrite_routes("v1", "proxy hls", 3);

        assert_eq!(
            segment_base,
            "/api/playback-providers/nextcloud/v1/hls-resources/proxy%20hls/3"
        );
        assert_eq!(resource, "hls-resources/proxy hls/3/*");
    }
}
