use futures::StreamExt;
use synctv_core::provider::{DouyinDanmakuEvent, ExecutionControl, HlsResourceRequest};
use synctv_core::service::DouyinPlaybackProviderService;
use synctv_proto::playback_provider::douyin::{
    douyin_danmaku_event, ChatEvent, DouyinDanmakuEvent as ProtoDanmakuEvent,
    DouyinHlsResourceKind, DouyinHlsResourceResponse, DouyinResourceResponse,
    GetDouyinHlsResourceRequest, GetDouyinResourceRequest, StreamClosedEvent,
    WatchDouyinDanmakuRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::DouyinProvider::NAME;

pub struct DouyinPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a DouyinPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type DouyinResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyinResourceResponse, ApiError>> + Send + 'static>,
>;
pub type DouyinHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyinHlsResourceResponse, ApiError>> + Send + 'static>,
>;
pub type DouyinDanmakuEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<ProtoDanmakuEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_douyin_resource(
    deps: DouyinPlaybackProviderDeps<'_>,
    req: GetDouyinResourceRequest,
) -> Result<DouyinResourceResponseStream, ApiError> {
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
        playback_provider_route_base(PROVIDER, &req.version, "hls-resources"),
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
        chunk.map(|chunk| DouyinResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_douyin_hls_resource(
    deps: DouyinPlaybackProviderDeps<'_>,
    req: GetDouyinHlsResourceRequest,
) -> Result<DouyinHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = douyin_hls_resource_kind(req.resource_kind)?;
    let kind_name = douyin_hls_resource_kind_name(kind);
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
                is_manifest: kind == DouyinHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == DouyinHlsResourceKind::Manifest {
        let segment_base = format!(
            "{}/{}/{}",
            playback_provider_route_base(PROVIDER, &req.version, "hls-resources"),
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
        chunk.map(|chunk| DouyinHlsResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_douyin_danmaku(
    deps: DouyinPlaybackProviderDeps<'_>,
    req: WatchDouyinDanmakuRequest,
) -> Result<DouyinDanmakuEventStream, ApiError> {
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
            .map(|event| ProtoDanmakuEvent {
                event: Some(match event {
                    DouyinDanmakuEvent::Chat {
                        id,
                        user_id,
                        user_name,
                        text,
                        color,
                        sent_at_ms,
                    } => douyin_danmaku_event::Event::Chat(ChatEvent {
                        id,
                        user_id,
                        user_name,
                        text,
                        color,
                        sent_at_ms,
                    }),
                    DouyinDanmakuEvent::StreamClosed { action, message } => {
                        douyin_danmaku_event::Event::StreamClosed(StreamClosedEvent {
                            action,
                            message,
                        })
                    }
                }),
            })
            .map_err(ApiError::from)
    })))
}

crate::impl_has_playback_provider_access_fields!(DouyinPlaybackProviderDeps<'a>);

impl<'a> DouyinPlaybackProviderDeps<'a> {
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
            proxy_signing_key: self.runtime.proxy_signing_key,
            proxy_http_client: self.runtime.proxy_http_client,
            ssrf_guard: self.runtime.ssrf_guard,
            proxy_slice_cache: self.runtime.proxy_slice_cache,
            request_control: self.request_control,
            hls_rewrite: Some(HlsRewriteSigning {
                segment_base,
                claims,
                resource,
            }),
        }
    }
}

fn douyin_hls_resource_kind(value: i32) -> Result<DouyinHlsResourceKind, ApiError> {
    let kind = DouyinHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Douyin HLS resource kind".to_string()))?;
    match kind {
        DouyinHlsResourceKind::Media | DouyinHlsResourceKind::Manifest => Ok(kind),
        DouyinHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Douyin HLS resource kind is required".to_string(),
        )),
    }
}

const fn douyin_hls_resource_kind_name(kind: DouyinHlsResourceKind) -> &'static str {
    match kind {
        DouyinHlsResourceKind::Media => "media",
        DouyinHlsResourceKind::Manifest => "manifest",
        DouyinHlsResourceKind::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_resource_kind_requires_a_typed_route() {
        assert!(douyin_hls_resource_kind(DouyinHlsResourceKind::Unspecified as i32).is_err());
        assert_eq!(
            douyin_hls_resource_kind(DouyinHlsResourceKind::Manifest as i32)
                .expect("manifest kind should validate"),
            DouyinHlsResourceKind::Manifest
        );
    }
}
