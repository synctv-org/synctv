use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, HlsResourceRequest};
use synctv_core::service::AcFunPlaybackProviderService;
use synctv_proto::playback_provider::acfun::{
    AcFunDanmakuEvent, AcFunDanmakuFileResponse, AcFunHlsResourceKind, AcFunHlsResourceResponse,
    AcFunResourceResponse, GetAcFunDanmakuFileRequest, GetAcFunHlsResourceRequest,
    GetAcFunResourceRequest, WatchAcFunDanmakuRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::AcFunProvider::NAME;
pub struct AcFunPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a AcFunPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type AcFunResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunResourceResponse, ApiError>> + Send + 'static>,
>;
pub type AcFunHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunHlsResourceResponse, ApiError>> + Send + 'static>,
>;
pub type AcFunDanmakuFileResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunDanmakuFileResponse, ApiError>> + Send + 'static>,
>;
pub type AcFunDanmakuEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunDanmakuEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_acfun_resource(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: GetAcFunResourceRequest,
) -> Result<AcFunResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| AcFunResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_acfun_hls_resource(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: GetAcFunHlsResourceRequest,
) -> Result<AcFunHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = acfun_hls_resource_kind(req.resource_kind)?;
    let kind_name = acfun_hls_resource_kind_name(kind);
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
                is_manifest: kind == AcFunHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == AcFunHlsResourceKind::Manifest {
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
        chunk.map(|chunk| AcFunHlsResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_acfun_danmaku_file(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: GetAcFunDanmakuFileRequest,
) -> Result<AcFunDanmakuFileResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("danmaku-files/{}/{}", req.mode_name, req.media_index),
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
        .danmaku_file_action(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, req.head).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AcFunDanmakuFileResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_acfun_danmaku(
    deps: AcFunPlaybackProviderDeps<'_>,
    req: WatchAcFunDanmakuRequest,
) -> Result<AcFunDanmakuEventStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("danmakus/{}/{}", req.mode_name, req.media_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let stream = deps
        .playback_provider_service
        .watch_danmaku(
            &req.version,
            &req.mode_name,
            req.media_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    Ok(Box::pin(stream.map(|event| {
        event
            .map(|event| AcFunDanmakuEvent {
                id: event.id,
                user_id: event.user_id,
                user_name: event.user_name,
                avatar_url: event.avatar_url,
                text: event.text,
                color: event.color,
                badge_name: event.badge_name,
                badge_level: event.badge_level,
                sent_at_ms: event.sent_at_ms,
            })
            .map_err(ApiError::from)
    })))
}

crate::impl_has_playback_provider_access_fields!(AcFunPlaybackProviderDeps<'a>);

impl<'a> AcFunPlaybackProviderDeps<'a> {
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

fn acfun_hls_resource_kind(value: i32) -> Result<AcFunHlsResourceKind, ApiError> {
    let kind = AcFunHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid AcFun HLS resource kind".to_string()))?;
    match kind {
        AcFunHlsResourceKind::Media | AcFunHlsResourceKind::Manifest => Ok(kind),
        AcFunHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "AcFun HLS resource kind is required".to_string(),
        )),
    }
}

const fn acfun_hls_resource_kind_name(kind: AcFunHlsResourceKind) -> &'static str {
    match kind {
        AcFunHlsResourceKind::Media => "media",
        AcFunHlsResourceKind::Manifest => "manifest",
        AcFunHlsResourceKind::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_resource_kind_requires_a_typed_route() {
        assert!(acfun_hls_resource_kind(AcFunHlsResourceKind::Unspecified as i32).is_err());
        assert_eq!(
            acfun_hls_resource_kind(AcFunHlsResourceKind::Manifest as i32)
                .expect("manifest kind should validate"),
            AcFunHlsResourceKind::Manifest
        );
    }
}
