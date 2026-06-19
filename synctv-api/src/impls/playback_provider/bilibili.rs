use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{BilibiliLiveDanmakuRequest, BilibiliPlaybackProviderService};
use synctv_proto::playback_provider::bilibili::{
    BilibiliDanmakuFileResponse, BilibiliDashManifestMode, BilibiliDashManifestResponse,
    BilibiliDashSegmentResponse, BilibiliHlsManifestResponse, BilibiliHlsSegmentResponse,
    BilibiliLiveDanmakuEvent, BilibiliLiveDanmakuEventType, BilibiliMediaStreamResponse,
    BilibiliSubtitleResponse, GetBilibiliDanmakuFileRequest, GetBilibiliDashManifestRequest,
    GetBilibiliDashSegmentRequest, GetBilibiliHlsManifestRequest, GetBilibiliHlsSegmentRequest,
    GetBilibiliMediaStreamRequest, GetBilibiliSubtitleRequest, WatchBilibiliLiveDanmakuRequest,
};

use super::common::{
    playback_transport_action_to_chunk_stream, verify_playback_provider_http_access,
    HlsRewriteSigning, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::BilibiliProvider::NAME;

pub struct BilibiliPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a BilibiliPlaybackProviderService,
    pub proxy_signing_key: &'a synctv_core::proxy_signature::ProxySigningKey,
    pub public_id_codec: &'a synctv_core::PublicIdCodec,
    pub provider_stores: &'a dyn synctv_core::provider::store::ProviderStoreResolver,
    pub user_service: &'a synctv_core::service::UserService,
    pub playback_transport_services:
        &'a synctv_core::provider::playback_transport::PlaybackTransportServices,
    pub request_control: Option<&'a ExecutionControl>,
    pub proxy_http_client: &'a reqwest::Client,
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub proxy_slice_cache: &'a synctv_proxy::slice_cache::SliceCache,
}

pub struct BilibiliLiveDanmakuDeps<'a> {
    pub playback_provider_service: &'a BilibiliPlaybackProviderService,
    pub actor_user_id: synctv_core::models::UserId,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type BilibiliMediaStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliMediaStreamResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliHlsManifestResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliHlsManifestResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliHlsSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliHlsSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliDashManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<BilibiliDashManifestResponse, ApiError>> + Send + 'static,
    >,
>;
pub type BilibiliDashSegmentResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliDashSegmentResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliSubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliDanmakuFileResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliDanmakuFileResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliLiveDanmakuStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliLiveDanmakuEvent, ApiError>> + Send + 'static>,
>;

pub async fn get_bilibili_media_stream(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliMediaStreamRequest,
) -> Result<BilibiliMediaStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_bilibili_access(
        &deps,
        &req.version,
        format!("media-streams/{}/{}", req.mode_name, req.url_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
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
        chunk.map(|chunk| BilibiliMediaStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_hls_manifest(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliHlsManifestRequest,
) -> Result<BilibiliHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_bilibili_access(
        &deps,
        &req.version,
        format!("hls-manifests/{}/{}", req.mode_name, req.url_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
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
    let segment_base = playback_provider_route_base("bilibili", &req.version, "hls-segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, "hls-segments"),
        action,
        false,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| BilibiliHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_hls_segment(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliHlsSegmentRequest,
) -> Result<BilibiliHlsSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, claims) = verify_bilibili_access(
        &deps,
        &req.version,
        "hls-segments".to_string(),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        Some(&req.target_url),
    )
    .await?;
    let action = deps
        .playback_provider_service
        .hls_segment_action(
            &req.version,
            &req.target_url,
            req.range.as_deref(),
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let segment_base = playback_provider_route_base("bilibili", &req.version, "hls-segments");
    let stream = playback_transport_action_to_chunk_stream(
        deps.chunk_deps_with_hls(&segment_base, &claims, "hls-segments"),
        action,
        head,
    )
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| BilibiliHlsSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_dash_manifest(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliDashManifestRequest,
) -> Result<BilibiliDashManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let mode = bilibili_dash_manifest_mode(req.mode)?;
    let mode_resource = bilibili_dash_manifest_mode_resource(mode);
    let (store, claims) = verify_bilibili_access(
        &deps,
        &req.version,
        format!("dash-manifests/{}/{}", req.mode_name, mode_resource),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
    )
    .await?;
    let dash_segment_base = playback_provider_route_base("bilibili", &req.version, "dash-segments");
    let mode_name = req.mode_name.clone();
    let signing_key = deps.proxy_signing_key;
    let mut proxy_url_for =
        (mode == synctv_core::provider::bilibili::BilibiliDashManifestMode::Proxy).then(|| {
            Box::new(move |index: usize, _target_url: &str| {
                let resource = format!("dash-segments/{mode_name}/{index}");
                let mut segment_claims = claims.clone();
                segment_claims.resource = resource.clone();
                segment_claims.target_url = None;
                let signed_query = signing_key.build_signed_query(&segment_claims);
                format!(
                    "{}/{}/{}?{}",
                    dash_segment_base,
                    url::form_urlencoded::byte_serialize(mode_name.as_bytes()).collect::<String>(),
                    index,
                    signed_query
                )
            }) as Box<synctv_core::provider::bilibili::BilibiliDashProxyUrlMapper<'_>>
        });
    let action = deps
        .playback_provider_service
        .dash_manifest_action(
            &req.version,
            &req.mode_name,
            mode,
            store,
            proxy_url_for.as_mut().map(|mapper| mapper.as_mut()),
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| BilibiliDashManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_dash_segment(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliDashSegmentRequest,
) -> Result<BilibiliDashSegmentResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_bilibili_access(
        &deps,
        &req.version,
        format!("dash-segments/{}/{}", req.mode_name, req.url_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
    )
    .await?;
    let action = deps
        .playback_provider_service
        .dash_segment_action(
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
        chunk.map(|chunk| BilibiliDashSegmentResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_subtitle(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliSubtitleRequest,
) -> Result<BilibiliSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_bilibili_access(
        &deps,
        &req.version,
        format!("subtitles/{}/{}", req.mode_name, req.subtitle_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
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
        chunk.map(|chunk| BilibiliSubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_danmaku_file(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliDanmakuFileRequest,
) -> Result<BilibiliDanmakuFileResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_bilibili_access(
        &deps,
        &req.version,
        format!("danmaku-files/{}", req.danmaku_index),
        &req.sig,
        &req.uid,
        &req.rid,
        req.exp,
        None,
    )
    .await?;
    let action = deps
        .playback_provider_service
        .danmaku_file_action(
            &req.version,
            req.danmaku_index as usize,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| BilibiliDanmakuFileResponse { chunk: Some(chunk) })
    })))
}

pub async fn watch_bilibili_live_danmaku(
    deps: BilibiliLiveDanmakuDeps<'_>,
    req: WatchBilibiliLiveDanmakuRequest,
) -> Result<BilibiliLiveDanmakuStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let stream = deps
        .playback_provider_service
        .watch_live_danmaku(BilibiliLiveDanmakuRequest {
            media_id: &req.media_id,
            actor_user_id: deps.actor_user_id,
            request_control: deps.request_control,
        })
        .await
        .map_err(ApiError::from)?
        .map(|event| {
            event
                .map(live_danmaku_event_to_proto)
                .map_err(ApiError::from)
        });
    Ok(Box::pin(stream))
}

fn bilibili_dash_manifest_mode(
    value: i32,
) -> Result<synctv_core::provider::bilibili::BilibiliDashManifestMode, ApiError> {
    match BilibiliDashManifestMode::try_from(value).map_err(|_| {
        ApiError::InvalidInput("Unsupported Bilibili DASH manifest mode".to_string())
    })? {
        BilibiliDashManifestMode::Unspecified | BilibiliDashManifestMode::Direct => {
            Ok(synctv_core::provider::bilibili::BilibiliDashManifestMode::Direct)
        }
        BilibiliDashManifestMode::Proxy => {
            Ok(synctv_core::provider::bilibili::BilibiliDashManifestMode::Proxy)
        }
    }
}

fn bilibili_dash_manifest_mode_resource(
    mode: synctv_core::provider::bilibili::BilibiliDashManifestMode,
) -> &'static str {
    match mode {
        synctv_core::provider::bilibili::BilibiliDashManifestMode::Direct => "direct",
        synctv_core::provider::bilibili::BilibiliDashManifestMode::Proxy => "proxy",
    }
}

#[allow(clippy::too_many_arguments)]
async fn verify_bilibili_access(
    deps: &BilibiliPlaybackProviderDeps<'_>,
    version: &str,
    resource: String,
    signature: &str,
    user_id: &str,
    room_id: &str,
    expires_at: i64,
    target_url: Option<&str>,
) -> Result<
    (
        std::sync::Arc<dyn synctv_core::provider::store::ProviderStore>,
        synctv_core::proxy_signature::ProxyUrlClaims,
    ),
    ApiError,
> {
    verify_playback_provider_http_access(
        deps.proxy_signing_key,
        deps.public_id_codec,
        deps.provider_stores,
        deps.user_service,
        deps.playback_transport_services,
        PROVIDER,
        version,
        resource,
        signature,
        user_id,
        room_id,
        expires_at,
        target_url,
    )
    .await
}

pub fn live_danmaku_event_to_proto(
    event: synctv_core::provider::BilibiliLiveDanmakuEvent,
) -> BilibiliLiveDanmakuEvent {
    let r#type = match event.kind {
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Unspecified => {
            BilibiliLiveDanmakuEventType::Unspecified
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Chat => {
            BilibiliLiveDanmakuEventType::Chat
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::UserEnter => {
            BilibiliLiveDanmakuEventType::UserEnter
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Gift => {
            BilibiliLiveDanmakuEventType::Gift
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Heartbeat => {
            BilibiliLiveDanmakuEventType::Heartbeat
        }
        synctv_core::provider::BilibiliLiveDanmakuEventKind::Unknown => {
            BilibiliLiveDanmakuEventType::Unknown
        }
    };
    BilibiliLiveDanmakuEvent {
        format: event.format,
        event_type: event.event_type,
        user: event.user,
        message: event.message,
        timestamp: event.timestamp,
        gift_name: event.gift_name,
        gift_count: event.gift_count,
        online_count: event.online_count,
        r#type: r#type as i32,
    }
}

fn playback_provider_route_base(route_provider: &str, version: &str, resource: &str) -> String {
    let encoded_version: String =
        url::form_urlencoded::byte_serialize(version.as_bytes()).collect();
    format!("/api/playback-providers/{route_provider}/{encoded_version}/{resource}")
}

impl<'a> BilibiliPlaybackProviderDeps<'a> {
    fn chunk_deps(&self) -> PlaybackTransportExecutorDeps<'a> {
        PlaybackTransportExecutorDeps {
            proxy_signing_key: self.proxy_signing_key,
            proxy_http_client: self.proxy_http_client,
            ssrf_guard: self.ssrf_guard,
            proxy_slice_cache: self.proxy_slice_cache,
            request_control: self.request_control,
            hls_rewrite: None,
        }
    }

    fn chunk_deps_with_hls(
        &self,
        segment_base: &'a str,
        claims: &'a synctv_core::proxy_signature::ProxyUrlClaims,
        resource: &'static str,
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
