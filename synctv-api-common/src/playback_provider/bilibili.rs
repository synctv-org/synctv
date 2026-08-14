use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{BilibiliLiveDanmakuRequest, BilibiliPlaybackProviderService};
use synctv_proto::playback_provider::bilibili::{
    BilibiliDanmakuFileResponse, BilibiliDashManifestMode, BilibiliDashManifestResponse,
    BilibiliDashResourceKind, BilibiliDashResourceResponse, BilibiliHlsManifestResponse,
    BilibiliHlsResourceKind, BilibiliHlsResourceResponse, BilibiliLiveDanmakuEvent,
    BilibiliLiveDanmakuEventType, BilibiliMediaStreamResponse, BilibiliSubtitleResponse,
    GetBilibiliDanmakuFileRequest, GetBilibiliDashManifestRequest, GetBilibiliDashResourceRequest,
    GetBilibiliHlsManifestRequest, GetBilibiliHlsResourceRequest, GetBilibiliMediaStreamRequest,
    GetBilibiliSubtitleRequest, WatchBilibiliLiveDanmakuRequest,
};

use super::common::{
    dash_transport_action_to_chunk_stream, playback_provider_route_base,
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    DashRewriteSigning, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackProviderIdentityRuntime,
    PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::BilibiliProvider::NAME;

pub struct BilibiliPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a BilibiliPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub struct BilibiliLiveDanmakuDeps<'a> {
    pub playback_provider_service: &'a BilibiliPlaybackProviderService,
    pub identity_runtime: PlaybackProviderIdentityRuntime<'a>,
    pub actor_user_id: synctv_core::models::UserId,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type BilibiliMediaStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliMediaStreamResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliHlsManifestResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliHlsManifestResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<BilibiliHlsResourceResponse, ApiError>> + Send + 'static>,
>;
pub type BilibiliDashManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<BilibiliDashManifestResponse, ApiError>> + Send + 'static,
    >,
>;
pub type BilibiliDashResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<BilibiliDashResourceResponse, ApiError>> + Send + 'static,
    >,
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
        playback_provider_route_base("bilibili", &req.version, "hls-resources"),
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
        chunk.map(|chunk| BilibiliHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_hls_resource(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliHlsResourceRequest,
) -> Result<BilibiliHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = bilibili_hls_resource_kind(req.resource_kind)?;
    let kind_name = bilibili_hls_resource_kind_name(kind);
    let head = req.head;
    let (store, claims) = verify_bilibili_access(
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
            synctv_core::provider::BilibiliHlsResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
                media_index: req.media_index as usize,
                target_url: &req.target_url,
                is_manifest: kind == BilibiliHlsResourceKind::Manifest,
                range_header: req.range.as_deref(),
            },
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if kind == BilibiliHlsResourceKind::Manifest {
        let segment_base = format!(
            "{}/{}/{}",
            playback_provider_route_base("bilibili", &req.version, "hls-resources"),
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
        chunk.map(|chunk| BilibiliHlsResourceResponse { chunk: Some(chunk) })
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
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("dash-manifests/{}/{}", req.mode_name, mode_resource),
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
            mode,
            store,
            deps.request_control,
        )
        .await
        .map_err(ApiError::from)?;
    let stream = if mode == synctv_core::provider::BilibiliDashManifestMode::Proxy {
        let resource_base = format!(
            "{}/{}",
            playback_provider_route_base("bilibili", &req.version, "dash-resources"),
            urlencoding::encode(&req.mode_name)
        );
        let resource_prefix = format!("dash-resources/{}", req.mode_name);
        dash_transport_action_to_chunk_stream(
            deps.chunk_deps(),
            action,
            DashRewriteSigning {
                resource_base: &resource_base,
                resource_prefix: &resource_prefix,
                claims: &claims,
            },
            false,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| BilibiliDashManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_bilibili_dash_resource(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliDashResourceRequest,
) -> Result<BilibiliDashResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = bilibili_dash_resource_kind(req.resource_kind)?;
    let kind_name = bilibili_dash_resource_kind_name(kind);
    let head = req.head;
    let (store, claims) = verify_bilibili_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("dash-resources/{}/{kind_name}", req.mode_name),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: Some(&req.scope_url),
        },
    )
    .await?;
    let is_manifest = kind == BilibiliDashResourceKind::Manifest;
    let action = deps
        .playback_provider_service
        .dash_resource_action(
            synctv_core::provider::BilibiliDashResourceRequest {
                version: &req.version,
                mode_name: &req.mode_name,
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
            "{}/{}",
            playback_provider_route_base("bilibili", &req.version, "dash-resources"),
            urlencoding::encode(&req.mode_name)
        );
        let resource_prefix = format!("dash-resources/{}", req.mode_name);
        dash_transport_action_to_chunk_stream(
            deps.chunk_deps(),
            action,
            DashRewriteSigning {
                resource_base: &resource_base,
                resource_prefix: &resource_prefix,
                claims: &claims,
            },
            head,
        )
        .await?
    } else {
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, head).await?
    };
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| BilibiliDashResourceResponse { chunk: Some(chunk) })
    })))
}

fn bilibili_hls_resource_kind(value: i32) -> Result<BilibiliHlsResourceKind, ApiError> {
    let kind = BilibiliHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Bilibili HLS resource kind".to_string()))?;
    match kind {
        BilibiliHlsResourceKind::Media | BilibiliHlsResourceKind::Manifest => Ok(kind),
        BilibiliHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Bilibili HLS resource kind is required".to_string(),
        )),
    }
}

const fn bilibili_hls_resource_kind_name(kind: BilibiliHlsResourceKind) -> &'static str {
    match kind {
        BilibiliHlsResourceKind::Media => "media",
        BilibiliHlsResourceKind::Manifest => "manifest",
        BilibiliHlsResourceKind::Unspecified => "unspecified",
    }
}

fn bilibili_dash_resource_kind(value: i32) -> Result<BilibiliDashResourceKind, ApiError> {
    let kind = BilibiliDashResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Bilibili DASH resource kind".to_string()))?;
    match kind {
        BilibiliDashResourceKind::Media | BilibiliDashResourceKind::Manifest => Ok(kind),
        BilibiliDashResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Bilibili DASH resource kind is required".to_string(),
        )),
    }
}

const fn bilibili_dash_resource_kind_name(kind: BilibiliDashResourceKind) -> &'static str {
    match kind {
        BilibiliDashResourceKind::Media => "media",
        BilibiliDashResourceKind::Manifest => "manifest",
        BilibiliDashResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_bilibili_subtitle(
    deps: BilibiliPlaybackProviderDeps<'_>,
    req: GetBilibiliSubtitleRequest,
) -> Result<BilibiliSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_bilibili_access(
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
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("danmaku-files/{}", req.danmaku_index),
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
    let media_id = crate::impls::proto_validated_media_id(
        &req.media_id,
        deps.identity_runtime.public_id_codec,
    )?;
    let stream = deps
        .playback_provider_service
        .watch_live_danmaku(BilibiliLiveDanmakuRequest {
            media_id,
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
) -> Result<synctv_core::provider::BilibiliDashManifestMode, ApiError> {
    match BilibiliDashManifestMode::try_from(value).map_err(|_| {
        ApiError::InvalidInput("Unsupported Bilibili DASH manifest mode".to_string())
    })? {
        BilibiliDashManifestMode::Unspecified | BilibiliDashManifestMode::Direct => {
            Ok(synctv_core::provider::BilibiliDashManifestMode::Direct)
        }
        BilibiliDashManifestMode::Proxy => {
            Ok(synctv_core::provider::BilibiliDashManifestMode::Proxy)
        }
    }
}

fn bilibili_dash_manifest_mode_resource(
    mode: synctv_core::provider::BilibiliDashManifestMode,
) -> &'static str {
    match mode {
        synctv_core::provider::BilibiliDashManifestMode::Direct => "direct",
        synctv_core::provider::BilibiliDashManifestMode::Proxy => "proxy",
    }
}

async fn verify_bilibili_access(
    deps: &BilibiliPlaybackProviderDeps<'_>,
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

crate::impl_has_playback_provider_access_fields!(BilibiliPlaybackProviderDeps<'a>);

impl<'a> BilibiliPlaybackProviderDeps<'a> {
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
