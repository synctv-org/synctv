use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, HlsResourceRequest};
use synctv_core::service::TikTokPlaybackProviderService;
use synctv_proto::playback_provider::tiktok::{
    GetTikTokHlsResourceRequest, GetTikTokResourceRequest, GetTikTokSubtitleRequest,
    TikTokHlsResourceKind, TikTokHlsResourceResponse, TikTokResourceResponse,
    TikTokSubtitleResponse,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::TikTokProvider::NAME;

pub struct TikTokPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a TikTokPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type TikTokResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TikTokResourceResponse, ApiError>> + Send + 'static>,
>;
pub type TikTokHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TikTokHlsResourceResponse, ApiError>> + Send + 'static>,
>;
pub type TikTokSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<TikTokSubtitleResponse, ApiError>> + Send + 'static>,
>;

pub async fn get_tiktok_resource(
    deps: TikTokPlaybackProviderDeps<'_>,
    req: GetTikTokResourceRequest,
) -> Result<TikTokResourceResponseStream, ApiError> {
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
    let segment_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(&req.rid, PROVIDER, &req.version, "hls-resources"),
        urlencoding::encode(&req.mode_name),
        req.media_index
    );
    let resource = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
        action,
        req.head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TikTokResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_tiktok_hls_resource(
    deps: TikTokPlaybackProviderDeps<'_>,
    req: GetTikTokHlsResourceRequest,
) -> Result<TikTokHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = tiktok_hls_resource_kind(req.resource_kind)?;
    let kind_name = tiktok_hls_resource_kind_name(kind);
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
            HlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest: kind == TikTokHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == TikTokHlsResourceKind::Manifest {
        let segment_base = format!(
            "{}/{}/{}",
            playback_provider_route_base(&req.rid, PROVIDER, &req.version, "hls-resources"),
            urlencoding::encode(&req.mode_name),
            req.media_index
        );
        let resource = format!("hls-resources/{}/{}/*", req.mode_name, req.media_index);
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
        chunk.map(|chunk| TikTokHlsResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_tiktok_subtitle(
    deps: TikTokPlaybackProviderDeps<'_>,
    req: GetTikTokSubtitleRequest,
) -> Result<TikTokSubtitleResponseStream, ApiError> {
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
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| TikTokSubtitleResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(TikTokPlaybackProviderDeps<'a>);

impl<'a> TikTokPlaybackProviderDeps<'a> {
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

fn tiktok_hls_resource_kind(value: i32) -> Result<TikTokHlsResourceKind, ApiError> {
    let kind = TikTokHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid TikTok HLS resource kind".to_string()))?;
    match kind {
        TikTokHlsResourceKind::Media | TikTokHlsResourceKind::Manifest => Ok(kind),
        TikTokHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "TikTok HLS resource kind is required".to_string(),
        )),
    }
}

const fn tiktok_hls_resource_kind_name(kind: TikTokHlsResourceKind) -> &'static str {
    match kind {
        TikTokHlsResourceKind::Media => "media",
        TikTokHlsResourceKind::Manifest => "manifest",
        TikTokHlsResourceKind::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_resource_kind_requires_a_typed_route() {
        assert!(tiktok_hls_resource_kind(TikTokHlsResourceKind::Unspecified as i32).is_err());
        assert_eq!(
            tiktok_hls_resource_kind(TikTokHlsResourceKind::Manifest as i32)
                .expect("manifest kind should validate"),
            TikTokHlsResourceKind::Manifest
        );
    }
}
