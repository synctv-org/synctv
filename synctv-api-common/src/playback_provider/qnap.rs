use futures::StreamExt;
use synctv_core::provider::{ExecutionControl, QnapHlsResourceRequest};
use synctv_core::service::QnapPlaybackProviderService;
use synctv_proto::playback_provider::qnap::{
    GetQnapHlsManifestRequest, GetQnapHlsResourceRequest, GetQnapResourceRequest,
    GetQnapSubtitleRequest, GetQnapThumbnailRequest, GetQnapThumbnailResourceRequest,
    QnapHlsManifestResponse, QnapHlsResourceKind, QnapHlsResourceResponse, QnapResourceResponse,
    QnapSubtitleResponse, QnapThumbnailResourceResponse, QnapThumbnailResponse,
};

use super::common::{
    decode_playback_resource_owner, playback_provider_route_base,
    playback_transport_action_to_chunk_stream, verify_playback_provider_access_with_deps,
    verify_playback_resource_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::QnapProvider::NAME;

pub struct QnapPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a QnapPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type QnapResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<QnapResourceResponse, ApiError>> + Send + 'static>,
>;
pub type QnapHlsManifestResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<QnapHlsManifestResponse, ApiError>> + Send + 'static>,
>;
pub type QnapHlsResourceResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<QnapHlsResourceResponse, ApiError>> + Send + 'static>,
>;
pub type QnapSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<QnapSubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type QnapThumbnailResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<QnapThumbnailResponse, ApiError>> + Send + 'static>,
>;
pub type QnapThumbnailResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<QnapThumbnailResourceResponse, ApiError>>
            + Send
            + 'static,
    >,
>;

pub async fn get_qnap_resource(
    deps: QnapPlaybackProviderDeps<'_>,
    req: GetQnapResourceRequest,
) -> Result<QnapResourceResponseStream, ApiError> {
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
        chunk.map(|chunk| QnapResourceResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_qnap_hls_manifest(
    deps: QnapPlaybackProviderDeps<'_>,
    req: GetQnapHlsManifestRequest,
) -> Result<QnapHlsManifestResponseStream, ApiError> {
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
        playback_provider_route_base(&req.rid, PROVIDER, &req.version, "hls-resources"),
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
        chunk.map(|chunk| QnapHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_qnap_hls_resource(
    deps: QnapPlaybackProviderDeps<'_>,
    req: GetQnapHlsResourceRequest,
) -> Result<QnapHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = qnap_hls_resource_kind(req.resource_kind)?;
    let kind_name = qnap_hls_resource_kind_name(kind);
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
    let is_manifest = kind == QnapHlsResourceKind::Manifest;
    let action = deps
        .playback_provider_service
        .hls_resource_action(
            QnapHlsResourceRequest {
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
            playback_provider_route_base(&req.rid, PROVIDER, &req.version, "hls-resources"),
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
        chunk.map(|chunk| QnapHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn qnap_hls_resource_kind(value: i32) -> Result<QnapHlsResourceKind, ApiError> {
    let kind = QnapHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid QNAP HLS resource kind".to_string()))?;
    match kind {
        QnapHlsResourceKind::Media | QnapHlsResourceKind::Manifest => Ok(kind),
        QnapHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "QNAP HLS resource kind is required".to_string(),
        )),
    }
}

const fn qnap_hls_resource_kind_name(kind: QnapHlsResourceKind) -> &'static str {
    match kind {
        QnapHlsResourceKind::Media => "media",
        QnapHlsResourceKind::Manifest => "manifest",
        QnapHlsResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_qnap_subtitle(
    deps: QnapPlaybackProviderDeps<'_>,
    req: GetQnapSubtitleRequest,
) -> Result<QnapSubtitleResponseStream, ApiError> {
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
        chunk.map(|chunk| QnapSubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_qnap_thumbnail(
    deps: QnapPlaybackProviderDeps<'_>,
    req: GetQnapThumbnailRequest,
) -> Result<QnapThumbnailResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_playback_provider_access_with_deps(
        &deps.access_deps(),
        PROVIDER,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: "thumbnail".to_string(),
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
        .thumbnail_action(&req.version, store, deps.request_control)
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| QnapThumbnailResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_qnap_thumbnail_resource(
    deps: QnapPlaybackProviderDeps<'_>,
    req: GetQnapThumbnailResourceRequest,
) -> Result<QnapThumbnailResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let scope = crate::qnap_thumbnail_urls::QnapThumbnailScope {
        server_id: &req.server_id,
        credential_owner_id: &req.credential_owner_id,
        path: &req.path,
        size: req.size,
    };
    let version = crate::qnap_thumbnail_urls::signature_version(scope);
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
        .thumbnail_resource_action(credential_owner_id, &req.server_id, &req.path, req.size)
        .await
        .map_err(ApiError::from)?;
    let stream =
        playback_transport_action_to_chunk_stream(deps.chunk_deps(), action, false).await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| QnapThumbnailResourceResponse { chunk: Some(chunk) })
    })))
}

crate::impl_has_playback_provider_access_fields!(QnapPlaybackProviderDeps<'a>);

impl<'a> QnapPlaybackProviderDeps<'a> {
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
