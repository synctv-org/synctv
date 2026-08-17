use futures::StreamExt;
use synctv_core::provider::ExecutionControl;
use synctv_core::service::AlistPlaybackProviderService;
use synctv_proto::playback_provider::alist::{
    AlistFileStreamResponse, AlistHlsResourceKind, AlistSubtitleResponse, AlistThumbnailResponse,
    AlistTranscodedHlsManifestResponse, AlistTranscodedHlsResourceResponse,
    GetAlistFileStreamRequest, GetAlistSubtitleRequest, GetAlistThumbnailRequest,
    GetAlistTranscodedHlsManifestRequest, GetAlistTranscodedHlsResourceRequest,
};

use super::common::{
    playback_provider_route_base, playback_transport_action_to_chunk_stream,
    verify_playback_provider_access_with_deps, HasPlaybackProviderAccessFields, HlsRewriteSigning,
    PlaybackProviderAccessRequest, PlaybackProviderApiRuntime, PlaybackTransportExecutorDeps,
};
use crate::impls::ApiError;

const PROVIDER: &str = synctv_core::provider::AlistProvider::NAME;

pub struct AlistPlaybackProviderDeps<'a> {
    pub playback_provider_service: &'a AlistPlaybackProviderService,
    pub runtime: PlaybackProviderApiRuntime<'a>,
    pub request_control: Option<&'a ExecutionControl>,
}

pub type AlistFileStreamResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AlistFileStreamResponse, ApiError>> + Send + 'static>,
>;
pub type AlistTranscodedHlsManifestResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<AlistTranscodedHlsManifestResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub type AlistTranscodedHlsResourceResponseStream = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<AlistTranscodedHlsResourceResponse, ApiError>>
            + Send
            + 'static,
    >,
>;
pub type AlistSubtitleResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AlistSubtitleResponse, ApiError>> + Send + 'static>,
>;
pub type AlistThumbnailResponseStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AlistThumbnailResponse, ApiError>> + Send + 'static>,
>;

async fn execute_alist_action_with_auth_retry<F, Fut>(
    deps: &AlistPlaybackProviderDeps<'_>,
    store: std::sync::Arc<dyn synctv_core::provider::ProviderStore>,
    version: &str,
    executor_deps: PlaybackTransportExecutorDeps<'_>,
    head: bool,
    mut action: F,
) -> Result<super::common::PlaybackProviderChunkStream, ApiError>
where
    F: FnMut(std::sync::Arc<dyn synctv_core::provider::ProviderStore>) -> Fut,
    Fut: std::future::Future<
        Output = Result<synctv_core::provider::PlaybackTransportAction, ApiError>,
    >,
{
    retry_alist_upstream_auth(
        || {
            let action = action(store.clone());
            async move {
                let action = action.await?;
                playback_transport_action_to_chunk_stream(executor_deps, action, head).await
            }
        },
        || {
            let store = store.clone();
            async move {
                deps.playback_provider_service
                    .invalidate_playback_access(version, store, deps.request_control)
                    .await
                    .map_err(ApiError::from)
            }
        },
    )
    .await
}

async fn retry_alist_upstream_auth<E, EFut, I, IFut>(
    mut execute: E,
    mut invalidate: I,
) -> Result<super::common::PlaybackProviderChunkStream, ApiError>
where
    E: FnMut() -> EFut,
    EFut:
        std::future::Future<Output = Result<super::common::PlaybackProviderChunkStream, ApiError>>,
    I: FnMut() -> IFut,
    IFut: std::future::Future<Output = Result<(), ApiError>>,
{
    for attempt in 0..2 {
        match execute().await {
            Ok(mut stream) => {
                let first = stream.next().await;
                let upstream_auth_failure = matches!(
                    first,
                    Some(Ok(ref chunk)) if matches!(chunk.status, 401 | 403)
                );
                if upstream_auth_failure && attempt == 0 {
                    invalidate().await?;
                    continue;
                }
                return Ok(match first {
                    Some(first) => {
                        Box::pin(futures::stream::once(async move { first }).chain(stream))
                    }
                    None => stream,
                });
            }
            Err(ApiError::Authentication(_)) if attempt == 0 => {
                invalidate().await?;
            }
            Err(error) => return Err(error),
        }
    }
    Err(ApiError::Authentication(
        "Alist rejected the refreshed playback resource".to_string(),
    ))
}

pub async fn get_alist_file_stream(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistFileStreamRequest,
) -> Result<AlistFileStreamResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let head = req.head;
    let (store, _) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!("files/{}/{}", req.mode_name, req.url_index),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let stream = Box::pin(execute_alist_action_with_auth_retry(
        &deps,
        store,
        &req.version,
        deps.chunk_deps(),
        head,
        |store| async {
            deps.playback_provider_service
                .file_stream_action(
                    &req.version,
                    &req.mode_name,
                    req.url_index as usize,
                    req.range.as_deref(),
                    store,
                    deps.request_control,
                )
                .await
                .map_err(ApiError::from)
        },
    ))
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistFileStreamResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_transcoded_hls_manifest(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistTranscodedHlsManifestRequest,
) -> Result<AlistTranscodedHlsManifestResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, claims) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "transcoded-hls-manifests/{}/{}",
                req.mode_name, req.url_index
            ),
            signature: &req.sig,
            user_id: &req.uid,
            room_id: &req.rid,
            expires_at: req.exp,
            target_url: None,
        },
    )
    .await?;
    let segment_base = format!(
        "{}/{}/{}",
        playback_provider_route_base(&req.rid, "alist", &req.version, "transcoded-hls-resources"),
        urlencoding::encode(&req.mode_name),
        req.url_index
    );
    let resource = format!(
        "transcoded-hls-resources/{}/{}/*",
        req.mode_name, req.url_index
    );
    let stream = Box::pin(execute_alist_action_with_auth_retry(
        &deps,
        store,
        &req.version,
        deps.chunk_deps_with_hls(&segment_base, &claims, &resource),
        false,
        |store| async {
            deps.playback_provider_service
                .transcoded_hls_manifest_action(
                    &req.version,
                    &req.mode_name,
                    req.url_index as usize,
                    store,
                    deps.request_control,
                )
                .await
                .map_err(ApiError::from)
        },
    ))
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistTranscodedHlsManifestResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_transcoded_hls_resource(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistTranscodedHlsResourceRequest,
) -> Result<AlistTranscodedHlsResourceResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let kind = alist_hls_resource_kind(req.resource_kind)?;
    let kind_name = alist_hls_resource_kind_name(kind);
    let head = req.head;
    let (store, claims) = verify_alist_access(
        &deps,
        PlaybackProviderAccessRequest {
            version: &req.version,
            resource: format!(
                "transcoded-hls-resources/{}/{}/{kind_name}",
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
    let segment_base = (kind == AlistHlsResourceKind::Manifest).then(|| {
        format!(
            "{}/{}/{}",
            playback_provider_route_base(
                &req.rid,
                "alist",
                &req.version,
                "transcoded-hls-resources"
            ),
            urlencoding::encode(&req.mode_name),
            req.media_index
        )
    });
    let resource = (kind == AlistHlsResourceKind::Manifest).then(|| {
        format!(
            "transcoded-hls-resources/{}/{}/*",
            req.mode_name, req.media_index
        )
    });
    let executor_deps = match (&segment_base, &resource) {
        (Some(segment_base), Some(resource)) => {
            deps.chunk_deps_with_hls(segment_base, &claims, resource)
        }
        _ => deps.chunk_deps(),
    };
    let stream = Box::pin(execute_alist_action_with_auth_retry(
        &deps,
        store,
        &req.version,
        executor_deps,
        head,
        |store| async {
            deps.playback_provider_service
                .transcoded_hls_resource_action(
                    synctv_core::provider::AlistHlsResourceRequest {
                        version: &req.version,
                        mode_name: &req.mode_name,
                        media_index: req.media_index as usize,
                        target_url: &req.target_url,
                        is_manifest: kind == AlistHlsResourceKind::Manifest,
                        range_header: req.range.as_deref(),
                    },
                    store,
                    deps.request_control,
                )
                .await
                .map_err(ApiError::from)
        },
    ))
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistTranscodedHlsResourceResponse { chunk: Some(chunk) })
    })))
}

fn alist_hls_resource_kind(value: i32) -> Result<AlistHlsResourceKind, ApiError> {
    let kind = AlistHlsResourceKind::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Invalid Alist HLS resource kind".to_string()))?;
    match kind {
        AlistHlsResourceKind::Media | AlistHlsResourceKind::Manifest => Ok(kind),
        AlistHlsResourceKind::Unspecified => Err(ApiError::InvalidInput(
            "Alist HLS resource kind is required".to_string(),
        )),
    }
}

const fn alist_hls_resource_kind_name(kind: AlistHlsResourceKind) -> &'static str {
    match kind {
        AlistHlsResourceKind::Media => "media",
        AlistHlsResourceKind::Manifest => "manifest",
        AlistHlsResourceKind::Unspecified => "unspecified",
    }
}

pub async fn get_alist_subtitle(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistSubtitleRequest,
) -> Result<AlistSubtitleResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_alist_access(
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
    let stream = Box::pin(execute_alist_action_with_auth_retry(
        &deps,
        store,
        &req.version,
        deps.chunk_deps(),
        false,
        |store| async {
            deps.playback_provider_service
                .subtitle_action(
                    &req.version,
                    &req.mode_name,
                    req.subtitle_index as usize,
                    store,
                    deps.request_control,
                )
                .await
                .map_err(ApiError::from)
        },
    ))
    .await?;
    Ok(Box::pin(stream.map(|chunk| {
        chunk.map(|chunk| AlistSubtitleResponse { chunk: Some(chunk) })
    })))
}

pub async fn get_alist_thumbnail(
    deps: AlistPlaybackProviderDeps<'_>,
    req: GetAlistThumbnailRequest,
) -> Result<AlistThumbnailResponseStream, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let (store, _) = verify_alist_access(
        &deps,
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
        chunk.map(|chunk| AlistThumbnailResponse { chunk: Some(chunk) })
    })))
}

async fn verify_alist_access(
    deps: &AlistPlaybackProviderDeps<'_>,
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

crate::impl_has_playback_provider_access_fields!(AlistPlaybackProviderDeps<'a>);

impl<'a> AlistPlaybackProviderDeps<'a> {
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
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use synctv_proto::playback_provider::common::StreamChunk;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn status_stream(status: u32) -> super::super::common::PlaybackProviderChunkStream {
        Box::pin(futures::stream::once(async move {
            Ok(StreamChunk {
                status,
                ..Default::default()
            })
        }))
    }

    #[derive(Clone)]
    struct AuthThenSuccess {
        requests: Arc<AtomicUsize>,
    }

    impl Respond for AuthThenSuccess {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(403)
            } else {
                ResponseTemplate::new(200).insert_header("Content-Length", "0")
            }
        }
    }

    #[tokio::test]
    async fn upstream_auth_status_invalidates_and_retries_once() -> anyhow::Result<()> {
        let executions = AtomicUsize::new(0);
        let invalidations = AtomicUsize::new(0);
        let mut stream = retry_alist_upstream_auth(
            || async {
                let attempt = executions.fetch_add(1, Ordering::SeqCst);
                Ok(status_stream(if attempt == 0 { 403 } else { 200 }))
            },
            || async {
                invalidations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;

        let first = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("retried stream should contain metadata"))?
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        assert_eq!(first.status, 200);
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn upstream_manifest_auth_error_invalidates_and_retries_once() -> anyhow::Result<()> {
        let executions = AtomicUsize::new(0);
        let invalidations = AtomicUsize::new(0);
        let mut stream = retry_alist_upstream_auth(
            || async {
                let attempt = executions.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Err(ApiError::Authentication(
                        "Remote M3U8 returned status 401 Unauthorized".to_string(),
                    ))
                } else {
                    Ok(status_stream(200))
                }
            },
            || async {
                invalidations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;

        assert_eq!(
            stream
                .next()
                .await
                .ok_or_else(|| anyhow::anyhow!("retried stream should contain metadata"))?
                .map_err(|error| anyhow::anyhow!("{error:?}"))?
                .status,
            200
        );
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn head_transport_keeps_head_semantics_across_auth_retry() -> anyhow::Result<()> {
        let mock_server = MockServer::start().await;
        let requests = Arc::new(AtomicUsize::new(0));
        Mock::given(method("HEAD"))
            .and(path("/movie.mp4"))
            .respond_with(AuthThenSuccess {
                requests: requests.clone(),
            })
            .expect(2)
            .mount(&mock_server)
            .await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *mock_server.address())
            .build()?;
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::builder()
            .extra_allowed_host("cdn.example.com".to_string())
            .build();
        let slice_cache = synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
            synctv_proxy::slice_cache::SliceCacheConfig::default(),
            client.clone(),
            ssrf_guard.clone(),
        )?;
        let signing_key = crate::proxy_signature::ProxySigningKey::try_derive_from(
            b"alist-head-auth-retry-test-key",
        )?;
        let executor_deps = PlaybackTransportExecutorDeps {
            proxy_signing_key: &signing_key,
            proxy_http_client: &client,
            ssrf_guard: &ssrf_guard,
            proxy_slice_cache: &slice_cache,
            request_control: None,
            hls_rewrite: None,
        };
        let invalidations = AtomicUsize::new(0);
        let url = format!(
            "http://cdn.example.com:{}/movie.mp4",
            mock_server.address().port()
        );
        let mut stream = retry_alist_upstream_auth(
            || {
                let action = synctv_core::provider::PlaybackTransportAction::FetchAndForward {
                    url: url.clone(),
                    headers: HashMap::new(),
                    range_header: None,
                    proxy_strategy:
                        synctv_core::provider::PlaybackResourceProxyStrategy::SliceCache,
                };
                async move {
                    playback_transport_action_to_chunk_stream(executor_deps, action, true).await
                }
            },
            || async {
                invalidations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;

        assert_eq!(
            stream
                .next()
                .await
                .ok_or_else(|| anyhow::anyhow!("HEAD response should contain metadata"))?
                .map_err(|error| anyhow::anyhow!("{error:?}"))?
                .status,
            200
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
