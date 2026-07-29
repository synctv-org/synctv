use super::{
    apply_global_layers, build_app_state, build_cors_layer, optional_header_str,
    register_all_routes, required_header_str, start_proxy_cache_lifecycle, RouterOptions,
};
use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::{routing::get, Router};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use synctv_api_common::proxy_signature::ProxySigningKey;
use synctv_core::cache::{KeyBuilder, UsernameCache};
use synctv_core::provider::ProviderSet;
use synctv_core::service::{
    AuditService, ContentFilter, InMemoryTokenBlacklistStore, ProvidersManager, RateLimitConfig,
    RateLimiter, RoomService, UserService,
};
use synctv_proxy::slice_cache::{SliceCache, SliceCacheBackend, SliceCacheConfig, StoredEntry};
use tower::ServiceExt;

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn app_ok<T>(result: Result<T, super::AppError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn app_err<T>(result: Result<T, super::AppError>) -> TestResult<super::AppError> {
    match result {
        Ok(_) => Err(test_error("expected HTTP app error")),
        Err(error) => Ok(error),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn test_provider_access_service(
    credential_repo: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    providers: &ProviderSet,
    provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
) -> Arc<dyn synctv_core::provider::ProviderAccessService> {
    Arc::new(
        synctv_core::provider::CachedProviderAccessService::new(
            credential_repo,
            providers.alist.clone(),
        )
        .with_store(provider_stores.load("credentials")),
    )
}

struct TestPlaybackProviderServices {
    playback_transport_services: Arc<synctv_core::provider::PlaybackTransportServices>,
    alist: Arc<synctv_core::service::AlistPlaybackProviderService>,
    bilibili: Arc<synctv_core::service::BilibiliPlaybackProviderService>,
    direct_url: Arc<synctv_core::service::DirectUrlPlaybackProviderService>,
    emby: Arc<synctv_core::service::EmbyPlaybackProviderService>,
    rtmp: Arc<synctv_core::service::RtmpPlaybackProviderService>,
    live_proxy: Arc<synctv_core::service::LiveProxyPlaybackProviderService>,
    twitch: Arc<synctv_core::service::TwitchPlaybackProviderService>,
    youtube: Arc<synctv_core::service::YoutubePlaybackProviderService>,
    douyin: Arc<synctv_core::service::DouyinPlaybackProviderService>,
    tiktok: Arc<synctv_core::service::TikTokPlaybackProviderService>,
    huya: Arc<synctv_core::service::HuyaPlaybackProviderService>,
    douyu: Arc<synctv_core::service::DouyuPlaybackProviderService>,
    acfun: Arc<synctv_core::service::AcFunPlaybackProviderService>,
    cctv: Arc<synctv_core::service::CctvPlaybackProviderService>,
    fnos: Arc<synctv_core::service::FnosPlaybackProviderService>,
    qnap: Arc<synctv_core::service::QnapPlaybackProviderService>,
    synology: Arc<synctv_core::service::SynologyPlaybackProviderService>,
    nextcloud: Arc<synctv_core::service::NextcloudPlaybackProviderService>,
    seafile: Arc<synctv_core::service::SeafilePlaybackProviderService>,
    truenas: Arc<synctv_core::service::TrueNasPlaybackProviderService>,
}

struct TestProviderApiImpls {
    provider_common: Arc<synctv_api_common::providers::ProviderCommonApiImpl>,
    bilibili: Arc<synctv_api_common::providers::BilibiliApiImpl>,
    alist: Arc<synctv_api_common::providers::AlistApiImpl>,
    emby: Arc<synctv_api_common::providers::EmbyApiImpl>,
    cloudreve: Arc<synctv_api_common::providers::CloudreveApiImpl>,
    twitch: Arc<synctv_api_common::providers::TwitchApiImpl>,
    youtube: Arc<synctv_api_common::providers::YoutubeApiImpl>,
    douyin: Arc<synctv_api_common::providers::DouyinApiImpl>,
    tiktok: Arc<synctv_api_common::providers::TikTokApiImpl>,
    fnos: Arc<synctv_api_common::providers::FnosApiImpl>,
    qnap: Arc<synctv_api_common::providers::QnapApiImpl>,
    synology: Arc<synctv_api_common::providers::SynologyApiImpl>,
    nextcloud: Arc<synctv_api_common::providers::NextcloudApiImpl>,
    seafile: Arc<synctv_api_common::providers::SeafileApiImpl>,
    truenas: Arc<synctv_api_common::providers::TrueNasApiImpl>,
}

struct TestCoreApiImpls {
    client: Arc<synctv_api_common::impls::ClientApiImpl>,
    admin: Option<Arc<synctv_api_common::impls::AdminApiImpl>>,
    email: Option<Arc<synctv_api_common::impls::EmailApiImpl>>,
    notification: Option<Arc<synctv_api_common::impls::NotificationApiImpl>>,
    oauth2: Option<Arc<synctv_api_common::impls::OAuth2ApiImpl>>,
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn test_core_api_impls(
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    user_service: Arc<UserService>,
    read_pool: Option<sqlx::PgPool>,
    room_service: Arc<RoomService>,
    connection_service: Arc<dyn synctv_realtime::sync::ConnectionRuntime>,
    presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    jwt_service: synctv_core::service::JwtService,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    request_executor: Arc<synctv_api_common::impls::RequestExecutor>,
    jwt_validator: Arc<synctv_core::service::JwtValidator>,
    provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    signing_key: Arc<ProxySigningKey>,
    media_swarm_signing_key: Arc<synctv_api_common::proxy_signature::MediaSwarmSigningKey>,
    rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService>,
    settings_service: Option<Arc<synctv_core::service::SettingsService>>,
    runtime_settings_store: Option<Arc<synctv_core::service::RuntimeSettingsStore>>,
    email_service: Option<Arc<synctv_core::service::EmailService>>,
    email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    audit_service: Arc<AuditService>,
    provider_instance_manager: Arc<synctv_core::service::RemoteProviderManager>,
) -> TestResult<TestCoreApiImpls> {
    let email = match (email_service.clone(), email_token_service) {
        (Some(_email_service), Some(email_token_service)) => {
            let email_outbox_service = Arc::new(synctv_core::service::EmailOutboxService::new(
                user_service.pool().clone(),
                "5252525252525252525252525252525252525252525252525252525252525252",
            )?);
            Some(Arc::new(synctv_api_common::impls::EmailApiImpl::new(
                user_service.clone(),
                email_token_service,
                email_outbox_service,
                rate_limiter.clone(),
                public_id_codec.clone(),
            )))
        }
        _ => None,
    };
    let realtime_event_service =
        Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new());
    let client = Arc::new(synctv_api_common::impls::ClientApiImpl::new_with_runtime(
        synctv_api_common::impls::ClientApiOptions {
            user_service: user_service.clone(),
            read_pool: read_pool.clone(),
            room_service: room_service.clone(),
            connection_service: connection_service.clone(),
            runtime_settings: runtime_settings.clone(),
            publish_key_service: None,
            jwt_service,
            live_streaming_infrastructure: None,
            runtime_settings_store: runtime_settings_store.clone(),
            public_id_codec: public_id_codec.clone(),
            chat_service: None,
            provider_stores: provider_stores.clone(),
            email_api: email.clone(),
            passkey_service: None,
        },
        synctv_api_common::impls::ClientApiRuntime::new_with_services(
            synctv_api_common::impls::ClientApiRuntimeServices {
                clock: Arc::new(synctv_core::SystemClock),
                realtime_fanout:
                    synctv_api_common::realtime_fanout::disabled_realtime_fanout_service(),
                realtime_event_service: realtime_event_service.clone(),
                redis_runtime: None,
                builtin_stun_url: None,
                webrtc_status:
                    synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
                provider_access_service: provider_access_service.clone(),
                signing_key: signing_key.clone(),
                media_swarm_signing_key: media_swarm_signing_key.clone(),
                presence_service: presence_service.clone(),
                jwt_validator,
                request_executor: request_executor.clone(),
                ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(
                    None,
                )),
                playback_duration_probe: None,
            },
        ),
    ));
    let admin = if let Some(settings_service) = settings_service {
        let email_service = email_service
            .ok_or_else(|| test_error("email service is required to build admin API"))?;
        Some(Arc::new(
            synctv_api_common::impls::AdminApiImpl::new_with_runtime(
                synctv_api_common::impls::AdminApiOptions {
                    room_service,
                    user_service: user_service.clone(),
                    read_services: synctv_api_common::test_support::admin_read_services(
                        user_service.as_ref(),
                    ),
                    settings_service,
                    runtime_settings_store,
                    email_service,
                    connection_service,
                    provider_instance_manager,
                    live_streaming_infrastructure: None,
                    publish_key_service: None,
                    runtime_settings,
                    audit_service,
                    public_id_codec: public_id_codec.clone(),
                },
                synctv_api_common::impls::AdminApiRuntime {
                    clock: Arc::new(synctv_core::SystemClock),
                    realtime_fanout:
                        synctv_api_common::realtime_fanout::disabled_realtime_fanout_service(),
                    realtime_event_service,
                    provider_stores,
                    provider_access_service,
                    signing_key,
                    media_swarm_signing_key,
                    presence_service,
                    request_executor,
                },
            ),
        ))
    } else {
        None
    };

    Ok(TestCoreApiImpls {
        client,
        admin,
        email,
        notification: None,
        oauth2: None,
    })
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn test_provider_api_impls(
    providers: &ProviderSet,
    provider_instance_manager: Arc<synctv_core::service::RemoteProviderManager>,
    user_service: Arc<UserService>,
    audit_service: Arc<AuditService>,
    providers_manager: Arc<synctv_core::service::ProvidersManager>,
    request_executor: Arc<synctv_api_common::impls::RequestExecutor>,
    credential_repo: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
) -> TestResult<TestProviderApiImpls> {
    let provider_common = Arc::new(
        synctv_api_common::providers::ProviderCommonApiImpl::new_with_runtime(
            provider_instance_manager,
            user_service,
            audit_service,
            synctv_api_common::providers::ProviderCommonApiRuntime {
                providers_manager,
                request_executor,
            },
        ),
    );
    let event_service = Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new());
    let runtime = synctv_api_common::providers::ProviderApiRuntime {
        access_service: provider_access_service,
        event_service: event_service.clone(),
    };
    let credential_backed_providers = providers.with_credential_repo(credential_repo);
    let bilibili = Arc::new(
        synctv_api_common::providers::BilibiliApiImpl::new_with_runtime(
            credential_backed_providers.bilibili.clone(),
            b"test-secret-key-for-http-router-tests-minimum-32-chars",
            runtime.clone(),
        )
        .map_err(|error| test_error(error.to_string()))?,
    );
    let alist = Arc::new(
        synctv_api_common::providers::AlistApiImpl::new_with_runtime(
            credential_backed_providers.alist.clone(),
            runtime.clone(),
        ),
    );
    let emby = Arc::new(synctv_api_common::providers::EmbyApiImpl::new_with_runtime(
        credential_backed_providers.emby.clone(),
        runtime,
    ));
    let cloudreve = Arc::new(synctv_api_common::providers::CloudreveApiImpl::new(
        credential_backed_providers.cloudreve.clone(),
        event_service.clone(),
    ));
    let twitch = Arc::new(synctv_api_common::providers::TwitchApiImpl::new(
        credential_backed_providers.twitch.clone(),
        event_service.clone(),
    ));
    let youtube = Arc::new(synctv_api_common::providers::YoutubeApiImpl::new(
        credential_backed_providers.youtube.clone(),
        event_service.clone(),
    ));
    let douyin = Arc::new(synctv_api_common::providers::DouyinApiImpl::new(
        credential_backed_providers.douyin.clone(),
        event_service.clone(),
    ));
    let tiktok = Arc::new(synctv_api_common::providers::TikTokApiImpl::new(
        credential_backed_providers.tiktok.clone(),
        event_service.clone(),
    ));
    let fnos = Arc::new(synctv_api_common::providers::FnosApiImpl::new(
        credential_backed_providers.fnos.clone(),
        event_service.clone(),
    ));
    let qnap = Arc::new(synctv_api_common::providers::QnapApiImpl::new(
        credential_backed_providers.qnap.clone(),
        event_service.clone(),
    ));
    let synology = Arc::new(synctv_api_common::providers::SynologyApiImpl::new(
        credential_backed_providers.synology.clone(),
        event_service.clone(),
    ));
    let nextcloud = Arc::new(synctv_api_common::providers::NextcloudApiImpl::new(
        credential_backed_providers.nextcloud.clone(),
        event_service.clone(),
    ));
    let seafile = Arc::new(synctv_api_common::providers::SeafileApiImpl::new(
        credential_backed_providers.seafile.clone(),
        event_service.clone(),
    ));
    let truenas = Arc::new(synctv_api_common::providers::TrueNasApiImpl::new(
        credential_backed_providers.truenas.clone(),
        event_service,
    ));

    Ok(TestProviderApiImpls {
        provider_common,
        bilibili,
        alist,
        emby,
        cloudreve,
        twitch,
        youtube,
        douyin,
        tiktok,
        fnos,
        qnap,
        synology,
        nextcloud,
        seafile,
        truenas,
    })
}

#[allow(clippy::needless_pass_by_value)]
fn test_playback_provider_services(
    providers: ProviderSet,
    provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    room_service: Arc<RoomService>,
    credential_repo: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
) -> TestPlaybackProviderServices {
    let playback_transport_services = Arc::new(synctv_core::provider::PlaybackTransportServices {
        room_service: room_service.clone(),
        permission_service: room_service.permission_service().clone(),
        credential_encryption: None,
        playback_session_repo: synctv_core::repository::ProviderPlaybackSessionRepository::new(
            credential_repo.pool().clone(),
        ),
        credential_repo,
        provider_access_service: provider_access_service.clone(),
    });
    let deps = synctv_core::service::PlaybackProviderServiceDeps {
        providers,
        provider_stores,
        playback_transport_services: playback_transport_services.clone(),
        provider_access_service,
    };
    TestPlaybackProviderServices {
        playback_transport_services,
        alist: Arc::new(synctv_core::service::AlistPlaybackProviderService::new(
            deps.clone(),
        )),
        bilibili: Arc::new(synctv_core::service::BilibiliPlaybackProviderService::new(
            deps.clone(),
        )),
        direct_url: Arc::new(synctv_core::service::DirectUrlPlaybackProviderService::new(
            deps.clone(),
        )),
        emby: Arc::new(synctv_core::service::EmbyPlaybackProviderService::new(
            deps.clone(),
        )),
        rtmp: Arc::new(synctv_core::service::RtmpPlaybackProviderService::new(
            deps.clone(),
        )),
        live_proxy: Arc::new(synctv_core::service::LiveProxyPlaybackProviderService::new(
            deps.clone(),
        )),
        twitch: Arc::new(synctv_core::service::TwitchPlaybackProviderService::new(
            deps.clone(),
        )),
        youtube: Arc::new(synctv_core::service::YoutubePlaybackProviderService::new(
            deps.clone(),
        )),
        douyin: Arc::new(synctv_core::service::DouyinPlaybackProviderService::new(
            deps.clone(),
        )),
        tiktok: Arc::new(synctv_core::service::TikTokPlaybackProviderService::new(
            deps.clone(),
        )),
        huya: Arc::new(synctv_core::service::HuyaPlaybackProviderService::new(
            deps.clone(),
        )),
        douyu: Arc::new(synctv_core::service::DouyuPlaybackProviderService::new(
            deps.clone(),
        )),
        acfun: Arc::new(synctv_core::service::AcFunPlaybackProviderService::new(
            deps.clone(),
        )),
        cctv: Arc::new(synctv_core::service::CctvPlaybackProviderService::new(
            deps.clone(),
        )),
        fnos: Arc::new(synctv_core::service::FnosPlaybackProviderService::new(
            deps.clone(),
        )),
        qnap: Arc::new(synctv_core::service::QnapPlaybackProviderService::new(
            deps.clone(),
        )),
        synology: Arc::new(synctv_core::service::SynologyPlaybackProviderService::new(
            deps.clone(),
        )),
        nextcloud: Arc::new(synctv_core::service::NextcloudPlaybackProviderService::new(
            deps.clone(),
        )),
        seafile: Arc::new(synctv_core::service::SeafilePlaybackProviderService::new(
            deps.clone(),
        )),
        truenas: Arc::new(synctv_core::service::TrueNasPlaybackProviderService::new(
            deps,
        )),
    }
}

struct TestApiExecutionRuntime {
    jwt_validator: Arc<synctv_core::service::JwtValidator>,
    security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    request_executor: Arc<synctv_api_common::impls::RequestExecutor>,
}

#[allow(clippy::needless_pass_by_value)]
fn test_api_execution_runtime(
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    user_service: Arc<UserService>,
    user_cache: Arc<synctv_core::cache::UserCache>,
    jwt_service: synctv_core::service::JwtService,
    rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService>,
) -> TestResult<TestApiExecutionRuntime> {
    let security_pipeline = Arc::new(synctv_core::service::SecurityPipeline::new_with_runtime(
        user_service.clone(),
        synctv_core::service::SecurityPipelineRuntime {
            user_cache: Some(user_cache),
            token_blacklist: user_service.token_blacklist_store(),
            key_builder: user_service.key_builder().clone(),
        },
    ));
    let jwt_validator = Arc::new(synctv_core::service::JwtValidator::new(Arc::new(
        jwt_service,
    )));
    let public_id_codec = Arc::new(
        synctv_adapter::PublicIdCodec::from_config(&synctv_adapter::PublicIdConfig::default())
            .map_err(|error| test_error(format!("invalid test public id config: {error}")))?,
    );
    let request_executor = Arc::new(synctv_api_common::impls::RequestExecutor::new(
        runtime_settings,
        jwt_validator.clone(),
        security_pipeline.clone(),
        rate_limiter,
    ));

    Ok(TestApiExecutionRuntime {
        jwt_validator,
        security_pipeline,
        public_id_codec,
        request_executor,
    })
}

fn test_request(result: Result<Request<Body>, axum::http::Error>) -> TestResult<Request<Body>> {
    result.map_err(|error| test_error(error.to_string()))
}

fn test_response<E>(result: Result<Response<Body>, E>) -> TestResult<Response<Body>>
where
    E: std::fmt::Display,
{
    result.map_err(|error| test_error(error.to_string()))
}

fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
    result.map_err(|error| test_error(error.to_string()))
}

fn api_ok<T>(result: Result<T, synctv_api_common::impls::ApiError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn test_fixture<T, E>(result: Result<T, E>) -> T
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("test fixture setup failed: {error}")),
    }
}

#[test]
fn optional_file_range_parses_standard_byte_ranges() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=10-19"));
    let range = app_ok(super::optional_file_range(&headers))?;
    assert!(matches!(
        range,
        Some(synctv_core::models::FileRangeRequest::Exact(
            synctv_core::models::FileByteRange {
                start: 10,
                end_inclusive: 19,
            },
        ))
    ));

    headers.insert(header::RANGE, HeaderValue::from_static("bytes=10-"));
    assert!(matches!(
        app_ok(super::optional_file_range(&headers))?,
        Some(synctv_core::models::FileRangeRequest::From { start: 10 })
    ));

    headers.insert(header::RANGE, HeaderValue::from_static("bytes=-20"));
    assert!(matches!(
        app_ok(super::optional_file_range(&headers))?,
        Some(synctv_core::models::FileRangeRequest::Suffix { length: 20 })
    ));

    headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1,2-3"));
    let error = app_err(super::optional_file_range(&headers))?;
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn file_blob_response_sets_partial_content_headers() -> TestResult {
    let download = synctv_core::models::FileObjectDownload {
        metadata: synctv_core::models::FileObjectMetadata {
            storage_backend: "database".to_string(),
            object_key: "object".to_string(),
            mime_type: "text/plain".to_string(),
            size_bytes: 4,
            total_size_bytes: 10,
            content_manifest_sha256: "a".repeat(64),
            compression: synctv_core::models::FileBlobCompression::None,
            range: Some(synctv_core::models::FileByteRange {
                start: 2,
                end_inclusive: 5,
            }),
            metadata: synctv_core::models::FileMetadata::default(),
            created_at: synctv_core::SystemClock.now(),
        },
        stream: futures::stream::once(async {
            Ok::<_, synctv_core::Error>(Bytes::from_static(b"cdef"))
        })
        .boxed(),
    };
    let response = app_ok(super::file_object_download_response(
        download,
        Some("private, max-age=1"),
    ))?;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers().get(header::CONTENT_RANGE),
        Some(&HeaderValue::from_static("bytes 2-5/10"))
    );
    assert_eq!(
        response.headers().get(header::ACCEPT_RANGES),
        Some(&HeaderValue::from_static("bytes"))
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static("private, max-age=1"))
    );
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|error| test_error(error.to_string()))?
        .to_bytes();
    assert_eq!(body, Bytes::from_static(b"cdef"));
    Ok(())
}

#[tokio::test]
async fn file_object_download_response_sets_streaming_headers() -> TestResult {
    let download = synctv_core::models::FileObjectDownload {
        metadata: synctv_core::models::FileObjectMetadata {
            storage_backend: "database".to_string(),
            object_key: "object".to_string(),
            mime_type: "text/plain".to_string(),
            size_bytes: 4,
            total_size_bytes: 4,
            content_manifest_sha256: "b".repeat(64),
            compression: synctv_core::models::FileBlobCompression::None,
            range: None,
            metadata: synctv_core::models::FileMetadata::default(),
            created_at: synctv_core::SystemClock.now(),
        },
        stream: futures::stream::iter([
            Ok::<_, synctv_core::Error>(Bytes::from_static(b"ab")),
            Ok(Bytes::from_static(b"cd")),
        ])
        .boxed(),
    };
    let response = app_ok(super::file_object_download_response(download, None))?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_LENGTH),
        Some(&HeaderValue::from_static("4"))
    );
    assert_eq!(
        response.headers().get(header::ACCEPT_RANGES),
        Some(&HeaderValue::from_static("bytes"))
    );
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|error| test_error(error.to_string()))?
        .to_bytes();
    assert_eq!(body, Bytes::from_static(b"abcd"));
    Ok(())
}

fn http_test_database() -> synctv_core_testing::TestDatabase {
    let handle = std::thread::spawn(|| {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                std::panic::panic_any(format!("test database runtime should build: {error}"))
            }
        };
        runtime.block_on(synctv_core_testing::create_test_database_with_db_and_label(
            "http_test",
            "http_test",
        ))
    });
    match handle.join() {
        Ok(database) => database,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[test]
fn required_header_str_rejects_missing_header() -> TestResult {
    let headers = HeaderMap::new();

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "missing token",
    ))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error.message(), "missing token");
    Ok(())
}

#[test]
fn required_header_str_rejects_blank_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("x-upload-token", HeaderValue::from_static("   "));

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "missing token",
    ))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error.message(), "missing token");
    Ok(())
}

#[test]
fn required_header_str_rejects_non_utf8_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("x-upload-token", HeaderValue::from_bytes(&[0xff])?);

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "missing token",
    ))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("x-upload-token"));
    Ok(())
}

#[test]
fn optional_header_str_rejects_non_utf8_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_bytes(&[0xff])?,
    );

    let error = app_err(optional_header_str(
        &headers,
        &axum::http::header::CONTENT_TYPE,
    ))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("content-type"));
    Ok(())
}

#[test]
fn required_header_str_rejects_duplicate_values() -> TestResult {
    let mut headers = axum::http::HeaderMap::new();
    headers.append(
        "x-upload-token",
        axum::http::HeaderValue::from_static("first"),
    );
    headers.append(
        "x-upload-token",
        axum::http::HeaderValue::from_static("second"),
    );

    let error = app_err(required_header_str(
        &headers,
        "x-upload-token",
        "Missing upload token",
    ))?;

    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert!(error.message().contains("Multiple x-upload-token headers"));
    Ok(())
}

#[test]
fn optional_header_str_rejects_duplicate_values() -> TestResult {
    let mut headers = axum::http::HeaderMap::new();
    headers.append(
        axum::http::header::CONTENT_RANGE,
        axum::http::HeaderValue::from_static("bytes 0-1/4"),
    );
    headers.append(
        axum::http::header::CONTENT_RANGE,
        axum::http::HeaderValue::from_static("bytes 2-3/4"),
    );

    let error = app_err(optional_header_str(
        &headers,
        &axum::http::header::CONTENT_RANGE,
    ))?;

    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert!(error.message().contains("Multiple content-range headers"));
    Ok(())
}

#[test]
fn forwarded_proto_is_https_accepts_trusted_proxy_https() -> TestResult {
    let mut server = synctv_api_common::ApiServerSettings::default();
    server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

    let result = app_ok(super::forwarded_proto_is_https(
        &server,
        &headers,
        Some("10.1.2.3".parse()?),
    ))?;

    assert!(result);
    Ok(())
}

#[test]
fn forwarded_proto_is_https_ignores_untrusted_peer() -> TestResult {
    let mut server = synctv_api_common::ApiServerSettings::default();
    server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

    let result = super::forwarded_proto_is_https(&server, &headers, Some("192.168.1.10".parse()?))?;

    assert!(!result);
    Ok(())
}

#[test]
fn forwarded_proto_is_https_rejects_non_utf8_from_trusted_proxy() -> TestResult {
    let mut server = synctv_api_common::ApiServerSettings::default();
    server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", HeaderValue::from_bytes(&[0xff])?);

    let error = app_err(super::forwarded_proto_is_https(
        &server,
        &headers,
        Some("10.1.2.3".parse()?),
    ))?;

    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(error.message().contains("x-forwarded-proto"));
    Ok(())
}

pub(crate) fn test_app_state() -> super::AppState {
    test_app_state_with_rate_limits(
        synctv_api_common::api_runtime::RequestRateLimitSettings::default(),
    )
}

fn test_app_state_with_rate_limits(
    request_rate_limits: synctv_api_common::api_runtime::RequestRateLimitSettings,
) -> super::AppState {
    let database = http_test_database();
    let pool = database.pool.clone();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
    let user_service = Arc::new(UserService::new_for_tests(
        &pool,
        test_fixture(synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )),
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
    ));
    let room_service = Arc::new(test_fixture(RoomService::new_for_tests(
        pool.clone(),
        (*user_service).clone(),
    )));
    let provider_instance_manager = synctv_core_testing::create_empty_provider_instance_manager();
    let providers_manager = Arc::new(test_fixture(ProvidersManager::new(
        provider_instance_manager.clone(),
    )));
    let providers = ProviderSet::new_with_ssrf_guard(
        provider_instance_manager.clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    );
    let providers = test_fixture(providers);
    let jwt_service = test_fixture(synctv_core::service::JwtService::new(
        "test-secret-key-for-http-router-tests-minimum-32-chars",
    ));
    let (audit_service, _audit_handle) = AuditService::new(pool.clone());
    let audit_service = Arc::new(audit_service);
    let config = synctv_api_common::ApiRuntimeSettings {
        request_rate_limits,
        ..synctv_api_common::ApiRuntimeSettings::default()
    };
    let config = Arc::new(config);
    let user_cache = Arc::new(synctv_core::cache::UserCache::local_only(
        128,
        60,
        300,
        "test:user:".to_string(),
    ));
    let rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService> =
        Arc::new(RateLimiter::local_only("test:".to_string()));
    let api_execution_runtime = test_fixture(test_api_execution_runtime(
        config.clone(),
        user_service.clone(),
        user_cache.clone(),
        jwt_service.clone(),
        rate_limiter.clone(),
    ));
    let credential_repo =
        Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool));
    let shared_provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
        synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:"),
    );
    let provider_access_service = test_provider_access_service(
        credential_repo.clone(),
        &providers,
        shared_provider_stores.clone(),
    );
    let playback_provider_services = test_playback_provider_services(
        providers.clone(),
        shared_provider_stores.clone(),
        room_service.clone(),
        credential_repo.clone(),
        provider_access_service.clone(),
    );
    let connection_manager = Arc::new(synctv_realtime::sync::ConnectionManager::new(
        synctv_realtime::sync::ConnectionLimits::default(),
    ));
    let presence_service = Arc::new(synctv_core::service::OnlinePresenceService::local());
    let shared_proxy_signing_key = Arc::new(
        synctv_api_common::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-proxy-signing-key-minimum-32-bytes!!",
        )
        .expect("test proxy signing key should derive"),
    );
    let media_swarm_signing_key = Arc::new(
        synctv_api_common::proxy_signature::MediaSwarmSigningKey::try_derive_from(
            b"test-media-swarm-signing-key-minimum-32-bytes",
        )
        .expect("test media swarm signing key should derive"),
    );
    let core_api_impls = test_fixture(test_core_api_impls(
        config.clone(),
        user_service.clone(),
        None,
        room_service.clone(),
        connection_manager.clone(),
        presence_service.clone(),
        jwt_service.clone(),
        api_execution_runtime.public_id_codec.clone(),
        api_execution_runtime.request_executor.clone(),
        api_execution_runtime.jwt_validator.clone(),
        shared_provider_stores.clone(),
        provider_access_service.clone(),
        shared_proxy_signing_key.clone(),
        media_swarm_signing_key.clone(),
        rate_limiter.clone(),
        None,
        None,
        None,
        None,
        audit_service.clone(),
        provider_instance_manager.clone(),
    ));
    let provider_api_impls = test_fixture(test_provider_api_impls(
        &providers,
        provider_instance_manager.clone(),
        user_service.clone(),
        audit_service.clone(),
        providers_manager.clone(),
        api_execution_runtime.request_executor.clone(),
        credential_repo.clone(),
        provider_access_service.clone(),
    ));
    let router_options = RouterOptions {
        runtime_settings: config,
        user_cache,
        user_service,
        read_pool: None,
        room_service,
        content_filter: ContentFilter::new(),
        provider_access_service,
        event_service: Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new()),
        connection_manager,
        presence_service,
        jwt_service,
        jwt_validator: api_execution_runtime.jwt_validator,
        security_pipeline: api_execution_runtime.security_pipeline,
        public_id_codec: api_execution_runtime.public_id_codec,
        request_executor: api_execution_runtime.request_executor,
        metrics_access_controller: Arc::new(
            synctv_api_common::metrics_auth::MetricsAccessController::new(),
        ),
        client_api: core_api_impls.client.clone(),
        admin_api: core_api_impls.admin.clone(),
        email_api: core_api_impls.email.clone(),
        notification_api: core_api_impls.notification.clone(),
        oauth2_api: core_api_impls.oauth2.clone(),
        realtime_fanout_service:
            synctv_api_common::realtime_fanout::disabled_realtime_fanout_service(),
        oauth2_service: None,
        passkey_service: None,
        settings_service: None,
        runtime_settings_store: None,
        email_service: None,
        email_token_service: None,
        publish_key_service: None,
        notification_service: None,
        chat_service: None,
        audit_service,
        live_streaming_infrastructure: None,
        cluster_client: None,
        rate_limiter,
        ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
        redis_runtime: None,
        shared_provider_stores,
        playback_transport_services: playback_provider_services
            .playback_transport_services
            .clone(),
        alist_playback_provider_service: playback_provider_services.alist.clone(),
        bilibili_playback_provider_service: playback_provider_services.bilibili.clone(),
        direct_url_playback_provider_service: playback_provider_services.direct_url.clone(),
        emby_playback_provider_service: playback_provider_services.emby.clone(),
        rtmp_playback_provider_service: playback_provider_services.rtmp.clone(),
        live_proxy_playback_provider_service: playback_provider_services.live_proxy.clone(),
        twitch_playback_provider_service: playback_provider_services.twitch.clone(),
        youtube_playback_provider_service: playback_provider_services.youtube.clone(),
        douyin_playback_provider_service: playback_provider_services.douyin.clone(),
        tiktok_playback_provider_service: playback_provider_services.tiktok.clone(),
        huya_playback_provider_service: playback_provider_services.huya.clone(),
        douyu_playback_provider_service: playback_provider_services.douyu.clone(),
        acfun_playback_provider_service: playback_provider_services.acfun.clone(),
        cctv_playback_provider_service: playback_provider_services.cctv.clone(),
        fnos_playback_provider_service: playback_provider_services.fnos.clone(),
        qnap_playback_provider_service: playback_provider_services.qnap.clone(),
        synology_playback_provider_service: playback_provider_services.synology.clone(),
        nextcloud_playback_provider_service: playback_provider_services.nextcloud.clone(),
        seafile_playback_provider_service: playback_provider_services.seafile.clone(),
        truenas_playback_provider_service: playback_provider_services.truenas.clone(),
        provider_common_api: provider_api_impls.provider_common.clone(),
        bilibili_api: provider_api_impls.bilibili.clone(),
        alist_api: provider_api_impls.alist.clone(),
        emby_api: provider_api_impls.emby.clone(),
        cloudreve_api: provider_api_impls.cloudreve.clone(),
        twitch_api: provider_api_impls.twitch.clone(),
        youtube_api: provider_api_impls.youtube.clone(),
        douyin_api: provider_api_impls.douyin.clone(),
        tiktok_api: provider_api_impls.tiktok.clone(),
        fnos_api: provider_api_impls.fnos.clone(),
        qnap_api: provider_api_impls.qnap.clone(),
        synology_api: provider_api_impls.synology.clone(),
        nextcloud_api: provider_api_impls.nextcloud.clone(),
        seafile_api: provider_api_impls.seafile.clone(),
        truenas_api: provider_api_impls.truenas.clone(),
        shared_proxy_signing_key,
        media_swarm_signing_key,
        builtin_stun_url: None,
        webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
        credential_encryption: None,
        ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
        proxy_slice_cache: Arc::new(test_fixture(SliceCache::new(SliceCacheConfig::default()))),
        proxy_http_client: test_fixture(synctv_proxy::build_proxy_http_client(
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )),
        messaging_rate_limit_config: RateLimitConfig::default(),
        heartbeat_schedule: synctv_api_common::impls::HeartbeatSchedule::production(),
        providers_manager,
        playback_duration_probe: None,
    };
    test_fixture(build_app_state(router_options)).with_test_database_leases(vec![database])
}

async fn test_app_state_with_websocket_runtime(
    request_rate_limits: synctv_api_common::api_runtime::RequestRateLimitSettings,
) -> super::AppState {
    let state = test_app_state_with_rate_limits(request_rate_limits);
    let mut router_options = state.router_options.as_ref().clone();
    let database = http_test_database();
    let pool = database.pool.clone();

    let room_settings_service = synctv_core::service::RoomSettingsService::new(
        synctv_core::repository::RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(synctv_core::service::NotificationService::default()),
        None,
        None,
    );
    let chat_service = synctv_core::service::ChatService::new(
        Arc::new(synctv_core::repository::ChatRepository::new(pool.clone())),
        synctv_core::service::ChatRuntime {
            clock: Arc::new(synctv_core::SystemClock),
            rate_limiter: router_options.rate_limiter.clone(),
            rate_limit_config: state
                .shared_api_runtime
                .messaging_rate_limit_config
                .as_ref()
                .clone(),
            content_filter: state.shared_api_runtime.content_filter.as_ref().clone(),
        },
        synctv_core::service::ChatDependencies {
            permission_service: router_options.room_service.permission_service().clone(),
            room_settings_service,
            user_service: router_options.user_service.clone(),
            file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
            audit_service: None,
            notification_service: synctv_core::service::NotificationService::default(),
            runtime_settings_store: None,
        },
    );
    router_options.chat_service = Some(Arc::new(chat_service));
    let realtime_manager =
        synctv_realtime::sync::RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-node".to_string(),
            dedup_window: Duration::from_secs(30),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await;
    let realtime_manager = Arc::new(test_fixture(realtime_manager));
    router_options.event_service = realtime_manager;
    test_fixture(build_app_state(router_options))
        .with_shared_test_database_leases(state.test_database_leases())
        .with_added_test_database_lease(database)
}

async fn test_app_state_with_real_chat_runtime(pool: sqlx::PgPool) -> super::AppState {
    let state = test_app_state_with_rate_limits(
        synctv_api_common::api_runtime::RequestRateLimitSettings::default(),
    );
    let mut router_options = state.router_options.as_ref().clone();

    let username_cache = UsernameCache::local_only("test:http-chat:username:".to_string(), 128, 60);
    let user_service = UserService::new_for_tests(
        &pool,
        router_options.jwt_service.clone(),
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test:http-chat"),
        synctv_core::service::BruteForceProtection::in_memory("test:http-chat:auth".to_string()),
    );
    let user_service = Arc::new(user_service);

    let room_service = test_fixture(RoomService::new_for_tests(
        pool.clone(),
        (*user_service).clone(),
    ));
    let room_service = Arc::new(room_service);

    let room_settings_repo = synctv_core::repository::RoomSettingsRepository::new(pool.clone());
    let permission_service = synctv_core::service::PermissionService::new_with_runtime(
        synctv_core::repository::RoomMemberRepository::new(pool.clone()),
        synctv_core::repository::RoomRepository::new(pool.clone()),
        synctv_core::service::PermissionServiceRuntime {
            room_settings_repo: Some(room_settings_repo.clone()),
            ..synctv_core::service::PermissionServiceRuntime::local_only()
        },
    );
    let permission_service = test_fixture(permission_service);
    let notification_service = synctv_core::service::NotificationService::default();
    let room_settings_service = synctv_core::service::RoomSettingsService::new(
        room_settings_repo,
        None,
        Arc::new(notification_service.clone()),
        None,
        None,
    );
    let chat_service = Arc::new(synctv_core::service::ChatService::new(
        Arc::new(synctv_core::repository::ChatRepository::new(pool.clone())),
        synctv_core::service::ChatRuntime {
            clock: Arc::new(synctv_core::SystemClock),
            rate_limiter: Arc::new(RateLimiter::local_only("test:http-chat:".to_string())),
            rate_limit_config: RateLimitConfig::default(),
            content_filter: ContentFilter::new(),
        },
        synctv_core::service::ChatDependencies {
            permission_service,
            room_settings_service,
            user_service: Arc::clone(&user_service),
            file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
            audit_service: None,
            notification_service,
            runtime_settings_store: None,
        },
    ));

    let realtime_manager =
        synctv_realtime::sync::RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "test-http-chat-node".to_string(),
            dedup_window: Duration::from_secs(30),
            critical_channel_capacity: 8,
            publish_channel_capacity: 8,
            key_prefix: "test:http-chat:".to_string(),
            catchup_window_secs: 60,
            stream_max_length: 100,
            event_handler: None,
            parent_cancel_token: None,
        })
        .await;
    let realtime_manager = Arc::new(test_fixture(realtime_manager));

    let user_cache = Arc::new(synctv_core::cache::UserCache::local_only(
        128,
        60,
        300,
        "test:http-chat:user:".to_string(),
    ));
    let api_execution_runtime = test_fixture(test_api_execution_runtime(
        router_options.runtime_settings.clone(),
        user_service.clone(),
        user_cache.clone(),
        router_options.jwt_service.clone(),
        router_options.rate_limiter.clone(),
    ));
    let client_api = Arc::new(synctv_api_common::impls::ClientApiImpl::new_with_runtime(
        synctv_api_common::impls::ClientApiOptions {
            user_service: user_service.clone(),
            read_pool: None,
            room_service: room_service.clone(),
            chat_service: Some(chat_service.clone()),
            connection_service: router_options.connection_manager.clone(),
            runtime_settings: router_options.runtime_settings.clone(),
            publish_key_service: None,
            jwt_service: router_options.jwt_service.clone(),
            live_streaming_infrastructure: None,
            runtime_settings_store: router_options.runtime_settings_store.clone(),
            provider_stores: router_options.shared_provider_stores.clone(),
            public_id_codec: api_execution_runtime.public_id_codec.clone(),
            email_api: router_options.email_api.clone(),
            passkey_service: router_options.passkey_service.clone(),
        },
        synctv_api_common::impls::ClientApiRuntime::new_with_services(
            synctv_api_common::impls::ClientApiRuntimeServices {
                clock: Arc::new(synctv_core::SystemClock),
                realtime_fanout: router_options.realtime_fanout_service.clone(),
                realtime_event_service: realtime_manager.clone(),
                redis_runtime: router_options.redis_runtime.clone(),
                builtin_stun_url: None,
                webrtc_status: router_options.webrtc_status.clone(),
                provider_access_service: router_options.provider_access_service.clone(),
                signing_key: router_options.shared_proxy_signing_key.clone(),
                media_swarm_signing_key: router_options.media_swarm_signing_key.clone(),
                presence_service: router_options.presence_service.clone(),
                jwt_validator: api_execution_runtime.jwt_validator.clone(),
                request_executor: api_execution_runtime.request_executor.clone(),
                ws_ticket_service: router_options.ws_ticket_service.clone(),
                playback_duration_probe: router_options.playback_duration_probe.clone(),
            },
        ),
    ));

    router_options.user_cache = user_cache;
    router_options.user_service = user_service;
    router_options.room_service = room_service;
    router_options.chat_service = Some(chat_service);
    router_options.event_service = realtime_manager;
    router_options.jwt_validator = api_execution_runtime.jwt_validator;
    router_options.security_pipeline = api_execution_runtime.security_pipeline;
    router_options.public_id_codec = api_execution_runtime.public_id_codec;
    router_options.request_executor = api_execution_runtime.request_executor;
    router_options.client_api = client_api;
    router_options.connection_manager = Arc::new(synctv_realtime::sync::ConnectionManager::new(
        synctv_realtime::sync::ConnectionLimits::default(),
    ));
    router_options.audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
    let credential_repo =
        Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool));
    let providers = test_fixture(ProviderSet::new_with_ssrf_guard(
        state.providers_manager.instance_manager().clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    ));
    router_options.provider_access_service = test_provider_access_service(
        credential_repo,
        &providers,
        router_options.shared_provider_stores.clone(),
    );

    test_fixture(build_app_state(router_options))
}

#[tokio::test]
async fn test_start_proxy_cache_lifecycle_evicts_expired_entries_and_stops_on_cancel() -> TestResult
{
    let cache = Arc::new(
        SliceCache::new(SliceCacheConfig {
            eviction_interval: Duration::from_millis(20),
            max_cache_size: 1024,
            ..SliceCacheConfig::default()
        })
        .map_err(|error| test_error(error.to_string()))?,
    );
    let key = "expired-slice".to_string();
    cache
        .backend()
        .put(
            &key,
            StoredEntry {
                data: Bytes::from_static(b"stale"),
                inserted_at: SystemTime::now() - Duration::from_secs(2),
                ttl: Duration::from_millis(5),
                last_accessed: SystemTime::now() - Duration::from_secs(2),
            },
        )
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let lifecycle = start_proxy_cache_lifecycle(&cache);

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if cache.backend().get(&key).await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    lifecycle.cancel.cancel();

    tokio::time::timeout(Duration::from_secs(1), lifecycle.handle)
        .await
        .map_err(|error| test_error(error.to_string()))?
        .map_err(|error| test_error(error.to_string()))?;
    Ok(())
}

#[tokio::test]
async fn test_start_proxy_cache_lifecycle_starts_even_when_runtime_toggle_is_off() -> TestResult {
    let cache = Arc::new(
        SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        })
        .map_err(|error| test_error(error.to_string()))?,
    );

    let lifecycle = start_proxy_cache_lifecycle(&cache);
    lifecycle.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), lifecycle.handle)
        .await
        .map_err(|error| test_error(error.to_string()))?
        .map_err(|error| test_error(error.to_string()))?;
    Ok(())
}

#[tokio::test]
async fn test_build_app_state_reuses_injected_proxy_cache() -> TestResult {
    let database = http_test_database();
    let pool = database.pool.clone();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
    let user_service = Arc::new(UserService::new_for_tests(
        &pool,
        synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .map_err(|error| test_error(error.to_string()))?,
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
    ));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .map_err(|error| test_error(error.to_string()))?,
    );
    let provider_instance_manager = synctv_core_testing::create_empty_provider_instance_manager();
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager.clone())
            .map_err(|error| test_error(error.to_string()))?,
    );
    let providers = ProviderSet::new_with_ssrf_guard(
        provider_instance_manager.clone(),
        synctv_common::ssrf::SsrfGuard::strict_policy(),
    )
    .map_err(|error| test_error(error.to_string()))?;
    let jwt_service = synctv_core::service::JwtService::new(
        "test-secret-key-for-http-router-tests-minimum-32-chars",
    )
    .map_err(|error| test_error(error.to_string()))?;
    let config = Arc::new(synctv_api_common::ApiRuntimeSettings::default());
    let user_cache = Arc::new(synctv_core::cache::UserCache::local_only(
        128,
        60,
        300,
        "test:user:".to_string(),
    ));
    let rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService> =
        Arc::new(RateLimiter::local_only("test:".to_string()));
    let api_execution_runtime = test_api_execution_runtime(
        config.clone(),
        user_service.clone(),
        user_cache.clone(),
        jwt_service.clone(),
        rate_limiter.clone(),
    )?;
    let (audit_service, _audit_handle) = AuditService::new(pool.clone());
    let audit_service = Arc::new(audit_service);
    let injected_cache = Arc::new(
        SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        })
        .map_err(|error| test_error(error.to_string()))?,
    );
    let injected_provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
        synctv_core::provider::ProviderStoreRegistry::local_only("shared:test:"),
    );
    let injected_credential_repo =
        Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()));
    let injected_provider_access_service = test_provider_access_service(
        injected_credential_repo.clone(),
        &providers,
        injected_provider_stores.clone(),
    );
    let injected_playback_provider_services = test_playback_provider_services(
        providers.clone(),
        injected_provider_stores.clone(),
        room_service.clone(),
        injected_credential_repo.clone(),
        injected_provider_access_service.clone(),
    );
    let injected_public_id_codec = api_execution_runtime.public_id_codec.clone();
    let injected_request_executor = api_execution_runtime.request_executor.clone();
    let injected_provider_api_impls = test_provider_api_impls(
        &providers,
        provider_instance_manager.clone(),
        user_service.clone(),
        audit_service.clone(),
        providers_manager.clone(),
        injected_request_executor.clone(),
        injected_credential_repo.clone(),
        injected_provider_access_service.clone(),
    )?;
    let injected_proxy_signing_key = Arc::new(
        ProxySigningKey::try_derive_from(b"test-secret-key-for-http-router-tests-minimum-32-chars")
            .map_err(|error| test_error(error.to_string()))?,
    );
    let injected_media_swarm_signing_key = Arc::new(
        synctv_api_common::proxy_signature::MediaSwarmSigningKey::try_derive_from(
            b"test-media-swarm-key-for-http-router-tests-minimum-32-chars",
        )
        .map_err(|error| test_error(error.to_string()))?,
    );
    let connection_manager = Arc::new(synctv_realtime::sync::ConnectionManager::new(
        synctv_realtime::sync::ConnectionLimits::default(),
    ));
    let presence_service = Arc::new(synctv_core::service::OnlinePresenceService::local());
    let injected_core_api_impls = test_core_api_impls(
        config.clone(),
        user_service.clone(),
        None,
        room_service.clone(),
        connection_manager.clone(),
        presence_service.clone(),
        jwt_service.clone(),
        injected_public_id_codec.clone(),
        injected_request_executor.clone(),
        api_execution_runtime.jwt_validator.clone(),
        injected_provider_stores.clone(),
        injected_provider_access_service.clone(),
        injected_proxy_signing_key.clone(),
        injected_media_swarm_signing_key.clone(),
        rate_limiter.clone(),
        None,
        None,
        None,
        None,
        audit_service.clone(),
        provider_instance_manager.clone(),
    )?;
    let injected_proxy_http_client =
        synctv_proxy::build_proxy_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
            .map_err(|error| test_error(error.to_string()))?;
    let injected_metrics_access_controller =
        Arc::new(synctv_api_common::metrics_auth::MetricsAccessController::new());

    let state = build_app_state(RouterOptions {
        runtime_settings: config,
        user_service,
        user_cache,
        room_service,
        read_pool: None,
        content_filter: ContentFilter::new(),
        provider_access_service: injected_provider_access_service.clone(),
        event_service: Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new()),
        connection_manager,
        presence_service,
        jwt_service,
        jwt_validator: api_execution_runtime.jwt_validator,
        security_pipeline: api_execution_runtime.security_pipeline,
        public_id_codec: injected_public_id_codec.clone(),
        request_executor: injected_request_executor.clone(),
        metrics_access_controller: injected_metrics_access_controller.clone(),
        client_api: injected_core_api_impls.client.clone(),
        admin_api: injected_core_api_impls.admin.clone(),
        email_api: injected_core_api_impls.email.clone(),
        notification_api: injected_core_api_impls.notification.clone(),
        oauth2_api: injected_core_api_impls.oauth2.clone(),
        realtime_fanout_service:
            synctv_api_common::realtime_fanout::disabled_realtime_fanout_service(),
        oauth2_service: None,
        passkey_service: None,
        settings_service: None,
        runtime_settings_store: None,
        email_service: None,
        email_token_service: None,
        publish_key_service: None,
        notification_service: None,
        chat_service: None,
        audit_service,
        live_streaming_infrastructure: None,
        cluster_client: None,
        rate_limiter,
        ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
        redis_runtime: None,
        shared_provider_stores: injected_provider_stores.clone(),
        playback_transport_services: injected_playback_provider_services
            .playback_transport_services
            .clone(),
        alist_playback_provider_service: injected_playback_provider_services.alist.clone(),
        bilibili_playback_provider_service: injected_playback_provider_services.bilibili.clone(),
        direct_url_playback_provider_service: injected_playback_provider_services
            .direct_url
            .clone(),
        emby_playback_provider_service: injected_playback_provider_services.emby.clone(),
        rtmp_playback_provider_service: injected_playback_provider_services.rtmp.clone(),
        live_proxy_playback_provider_service: injected_playback_provider_services
            .live_proxy
            .clone(),
        twitch_playback_provider_service: injected_playback_provider_services.twitch.clone(),
        youtube_playback_provider_service: injected_playback_provider_services.youtube.clone(),
        douyin_playback_provider_service: injected_playback_provider_services.douyin.clone(),
        tiktok_playback_provider_service: injected_playback_provider_services.tiktok.clone(),
        huya_playback_provider_service: injected_playback_provider_services.huya.clone(),
        douyu_playback_provider_service: injected_playback_provider_services.douyu.clone(),
        acfun_playback_provider_service: injected_playback_provider_services.acfun.clone(),
        cctv_playback_provider_service: injected_playback_provider_services.cctv.clone(),
        fnos_playback_provider_service: injected_playback_provider_services.fnos.clone(),
        qnap_playback_provider_service: injected_playback_provider_services.qnap.clone(),
        synology_playback_provider_service: injected_playback_provider_services.synology.clone(),
        nextcloud_playback_provider_service: injected_playback_provider_services.nextcloud.clone(),
        seafile_playback_provider_service: injected_playback_provider_services.seafile.clone(),
        truenas_playback_provider_service: injected_playback_provider_services.truenas.clone(),
        provider_common_api: injected_provider_api_impls.provider_common.clone(),
        bilibili_api: injected_provider_api_impls.bilibili.clone(),
        alist_api: injected_provider_api_impls.alist.clone(),
        emby_api: injected_provider_api_impls.emby.clone(),
        cloudreve_api: injected_provider_api_impls.cloudreve.clone(),
        twitch_api: injected_provider_api_impls.twitch.clone(),
        youtube_api: injected_provider_api_impls.youtube.clone(),
        douyin_api: injected_provider_api_impls.douyin.clone(),
        tiktok_api: injected_provider_api_impls.tiktok.clone(),
        fnos_api: injected_provider_api_impls.fnos.clone(),
        qnap_api: injected_provider_api_impls.qnap.clone(),
        synology_api: injected_provider_api_impls.synology.clone(),
        nextcloud_api: injected_provider_api_impls.nextcloud.clone(),
        seafile_api: injected_provider_api_impls.seafile.clone(),
        truenas_api: injected_provider_api_impls.truenas.clone(),
        shared_proxy_signing_key: injected_proxy_signing_key.clone(),
        media_swarm_signing_key: injected_media_swarm_signing_key.clone(),
        builtin_stun_url: None,
        webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
        credential_encryption: None,
        proxy_slice_cache: injected_cache.clone(),
        ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
        proxy_http_client: injected_proxy_http_client,
        messaging_rate_limit_config: RateLimitConfig::default(),
        heartbeat_schedule: synctv_api_common::impls::HeartbeatSchedule::production(),
        providers_manager,
        playback_duration_probe: None,
    })?
    .with_test_database_leases(vec![database]);

    assert!(
        Arc::ptr_eq(&state.proxy_slice_cache, &injected_cache),
        "AppState must reuse the injected proxy slice cache instead of creating a hidden default instance"
    );
    assert!(
        !state.proxy_slice_cache.config().enabled,
        "The injected cache configuration must be preserved"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.provider_stores,
            &injected_provider_stores
        ),
        "AppState must reuse the injected provider store registry"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.provider_access_service,
            &injected_provider_access_service
        ),
        "AppState must reuse the injected provider access service"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.public_id_codec,
            &injected_public_id_codec
        ),
        "AppState must reuse the injected public ID codec"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.request_executor,
            &injected_request_executor
        ),
        "AppState must reuse the injected request executor"
    );
    assert!(
        Arc::ptr_eq(
            &state.metrics_access_controller,
            &injected_metrics_access_controller
        ),
        "AppState must reuse the injected metrics access controller"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.client_api,
            &injected_core_api_impls.client
        ),
        "AppState must reuse the injected client API"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.provider_common_api,
            &injected_provider_api_impls.provider_common
        ),
        "AppState must reuse the injected provider common API"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.bilibili_api,
            &injected_provider_api_impls.bilibili
        ),
        "AppState must reuse the injected Bilibili API"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.alist_api,
            &injected_provider_api_impls.alist
        ),
        "AppState must reuse the injected Alist API"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.emby_api,
            &injected_provider_api_impls.emby
        ),
        "AppState must reuse the injected Emby API"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.proxy_signing_key,
            &injected_proxy_signing_key
        ),
        "AppState must reuse the injected proxy signing key"
    );
    assert!(
        Arc::ptr_eq(
            &state.shared_api_runtime.media_swarm_signing_key,
            &injected_media_swarm_signing_key
        ),
        "AppState must reuse the injected media swarm signing key"
    );
    assert!(
        state
            .proxy_http_client
            .get("https://example.com")
            .build()
            .is_ok(),
        "The injected proxy HTTP client must remain usable in AppState"
    );
    assert!(
        state.shared_api_runtime.security_pipeline.has_user_cache(),
        "AppState security pipeline should carry the shared user cache"
    );
    Ok(())
}

#[tokio::test]
async fn test_playback_patch_route_is_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room_123/playback")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"type":1}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "PATCH playback route must be registered in the project router"
    );
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "PATCH playback route must accept PATCH requests"
    );
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "request should reach the registered route and follow the normal auth path; playback PATCH is not gated by websocket-runtime-only middleware"
    );
    Ok(())
}

#[tokio::test]
async fn test_api_root_redirects_to_configured_project_url() -> TestResult {
    let mut state = test_app_state();
    Arc::make_mut(&mut Arc::make_mut(&mut state.router_options).runtime_settings)
        .server
        .project_url = "https://example.com/synctv/project?source=api".to_string();
    let app = super::create_router_from_shared_state(&state)?;
    let request = Request::builder().uri("/").body(Body::empty())?;

    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers().get(header::LOCATION),
        Some(&HeaderValue::from_static(
            "https://example.com/synctv/project?source=api"
        ))
    );
    Ok(())
}

#[tokio::test]
async fn test_native_passkey_association_documents_are_served_from_config() -> TestResult {
    let mut state = test_app_state();
    let server =
        &mut Arc::make_mut(&mut Arc::make_mut(&mut state.router_options).runtime_settings).server;
    server.apple_app_ids = vec!["85KBWFQ6F6.org.synctv.app".to_string()];
    server.android_apps = vec![synctv_api_common::AndroidAppAssociationSettings {
        package_name: "org.synctv.app".to_string(),
        sha256_cert_fingerprints: vec![
            "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899".to_string(),
        ],
    }];
    let app = super::create_router_from_shared_state(&state)?;

    let apple_response = test_response(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/.well-known/apple-app-site-association")
                    .body(Body::empty())?,
            )
            .await,
    )?;
    assert_eq!(apple_response.status(), StatusCode::OK);
    assert_eq!(
        apple_response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/json"))
    );
    assert_eq!(
        apple_response.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static("public, max-age=300"))
    );
    let apple_body = apple_response.into_body().collect().await?.to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&apple_body)?,
        serde_json::json!({
            "applinks": {
                "apps": [],
                "details": [{
                    "appID": "85KBWFQ6F6.org.synctv.app",
                    "paths": []
                }]
            },
            "webcredentials": {"apps": ["85KBWFQ6F6.org.synctv.app"]}
        })
    );

    let android_response = test_response(
        app.oneshot(
            Request::builder()
                .uri("/.well-known/assetlinks.json")
                .body(Body::empty())?,
        )
        .await,
    )?;
    assert_eq!(android_response.status(), StatusCode::OK);
    let android_body = android_response.into_body().collect().await?.to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&android_body)?,
        serde_json::json!([{
            "relation": [
                "delegate_permission/common.get_login_creds",
                "delegate_permission/common.handle_all_urls"
            ],
            "target": {
                "namespace": "android_app",
                "package_name": "org.synctv.app",
                "sha256_cert_fingerprints": [
                    "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99"
                ]
            }
        }])
    );
    Ok(())
}

#[tokio::test]
async fn test_playback_navigation_routes_are_reachable_via_project_router() -> TestResult {
    synctv_core::install_process_crypto_provider();
    for (method, uri, body) in [
        ("POST", "/api/rooms/room_123/playback/next", "{}"),
        ("POST", "/api/rooms/room_123/playback/previous", "{}"),
        ("GET", "/api/rooms/room_123/playback/history", ""),
        ("POST", "/api/rooms/room_123/playback/history/ph_1/play", ""),
    ] {
        let app = register_all_routes().with_state(test_app_state());
        let request = test_request(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body)),
        )?;
        let response = test_response(app.oneshot(request).await)?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "route: {uri}");
    }
    Ok(())
}

#[tokio::test]
async fn test_chat_message_patch_route_is_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room_123/chat/messages/msg_456")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"content":"edited","expectedVersion":"1"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "chat message PATCH route must be registered in the project router"
    );
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "chat message PATCH route must accept PATCH requests"
    );
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
        ),
        "request should reach the registered route and be handled by the normal request pipeline"
    );
    Ok(())
}

#[tokio::test]
async fn test_chat_message_delete_route_is_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/rooms/room_123/chat/messages/msg_456")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"expectedVersion":"1","reason":"cleanup"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "chat message DELETE route must be registered in the project router"
    );
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "chat message DELETE route must accept DELETE requests"
    );
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
        ),
        "request should reach the registered route and be handled by the normal request pipeline"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_events_sse_receives_live_send_event() -> TestResult {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
    let now = synctv_core::SystemClock.now();
    let owner = core_ok(
        synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_live_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await,
    )?;
    let access_token = core_ok(state.jwt_service.sign_access_token(&owner.id, 0))?;
    let (room, _) = core_ok(
        state
            .room_service
            .create_room(
                "HTTP SSE Chat Live Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
    )?;
    let public_room_id = state
        .shared_api_runtime
        .public_id_codec
        .encode_room_id(room.id)
        .map_err(test_error)?;
    let app = register_all_routes().with_state(state.clone());
    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/rooms/{public_room_id}/watch/chat-events?format=json"
            ))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    let status = response.status();
    if status != StatusCode::OK {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        panic!(
            "expected chat events SSE status 200, got {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    let mut body = response.into_body();
    let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await?
        .ok_or_else(|| test_error("SSE stream ended before initial frame"))?
        .map_err(|error| test_error(error.to_string()))?;
    let mut rendered = String::new();
    if let Some(data) = first_frame.data_ref() {
        rendered.push_str(std::str::from_utf8(data)?);
    }
    assert!(rendered.contains("event: observed\n"));

    let sent = api_ok(
        state
            .shared_api_runtime
            .client_api
            .send_chat_message_for_actor(
                &synctv_api_common::impls::client::RoomActor::User {
                    room_id: room.id,
                    user_id: owner.id,
                },
                synctv_proto::client::SendChatMessageRequest {
                    client_message_id: "http-sse-live-send-1".to_string(),
                    content: "live push event".to_string(),
                    metadata: None,
                    ..Default::default()
                },
            )
            .await,
    )?
    .event
    .ok_or_else(|| test_error("chat send should return event"))?;

    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await?
            .ok_or_else(|| test_error("SSE stream ended before expected frame"))?
            .map_err(|error| test_error(error.to_string()))?;
        if let Some(data) = frame.data_ref() {
            rendered.push_str(std::str::from_utf8(data)?);
        }
        if rendered.contains("live push event") {
            break;
        }
    }

    assert!(rendered.contains("event: changed\n"));
    assert!(rendered.contains(&format!("id: {}\n", sent.sequence)));
    assert!(rendered.contains("live push event"));
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_events_sse_replays_after_last_event_id_header() -> TestResult {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
    let now = synctv_core::SystemClock.now();
    let owner = core_ok(
        synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await,
    )?;
    let access_token = core_ok(state.jwt_service.sign_access_token(&owner.id, 0))?;
    let (room, _) = core_ok(
        state
            .room_service
            .create_room(
                "HTTP SSE Chat Replay Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
    )?;
    let chat_service = state
        .chat_service
        .as_ref()
        .ok_or_else(|| test_error("chat service should be present"))?
        .clone();

    let first = core_ok(
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-1".to_string()),
                content: "first replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::User,
                reply_to_message_id: None,
                metadata: None,
                attachments: Vec::new(),
                mentions: Vec::new(),
            })
            .await,
    )?;
    core_ok(
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-2".to_string()),
                content: "second replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::User,
                reply_to_message_id: None,
                metadata: None,
                attachments: Vec::new(),
                mentions: Vec::new(),
            })
            .await,
    )?;
    core_ok(
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-3".to_string()),
                content: "third replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::User,
                reply_to_message_id: None,
                metadata: None,
                attachments: Vec::new(),
                mentions: Vec::new(),
            })
            .await,
    )?;

    let public_room_id = state
        .shared_api_runtime
        .public_id_codec
        .encode_room_id(room.id)
        .map_err(test_error)?;
    let app = register_all_routes().with_state(state.clone());
    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/rooms/{public_room_id}/watch/chat-events?format=json"
            ))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header("last-event-id", first.sequence.to_string())
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    let status = response.status();
    if status != StatusCode::OK {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        panic!(
            "expected chat events replay SSE status 200, got {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    let mut body = response.into_body();
    let mut rendered = String::new();
    for _ in 0..8 {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await?
            .ok_or_else(|| test_error("SSE stream ended before expected frame"))?
            .map_err(|error| test_error(error.to_string()))?;
        if let Some(data) = frame.data_ref() {
            rendered.push_str(std::str::from_utf8(data)?);
        }
        if rendered.contains("second replay") && rendered.contains("third replay") {
            break;
        }
    }

    assert!(rendered.contains("event: observed\n"));
    assert!(rendered.contains("event: changed\n"));
    assert!(rendered.contains("id: "));
    assert!(rendered.contains("second replay"));
    assert!(rendered.contains("third replay"));
    assert!(
        !rendered.contains("first replay"),
        "Last-Event-ID should replay events strictly after the supplied sequence"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_chat_events_sse_unknown_last_event_id_returns_bad_request() -> TestResult {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
    let now = synctv_core::SystemClock.now();
    let owner = core_ok(
        synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_bad_cursor_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await,
    )?;
    let access_token = core_ok(state.jwt_service.sign_access_token(&owner.id, 0))?;
    let (room, _) = core_ok(
        state
            .room_service
            .create_room(
                "HTTP SSE Chat Bad Cursor Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
    )?;
    let public_room_id = state
        .shared_api_runtime
        .public_id_codec
        .encode_room_id(room.id)
        .map_err(test_error)?;
    let app = register_all_routes().with_state(state.clone());

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/rooms/{public_room_id}/watch/chat-events?format=json"
            ))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}"),
            )
            .header("last-event-id", "missing-chat-sequence")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn test_public_and_authenticated_room_discovery_routes_are_separate() -> TestResult {
    synctv_core::install_process_crypto_provider();
    let public_app = register_all_routes().with_state(test_app_state());
    let public_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/discover?page=1&pageSize=10")
            .body(Body::empty()),
    )?;
    let public_response = test_response(public_app.oneshot(public_request).await)?;
    assert_ne!(public_response.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(public_response.status(), StatusCode::NOT_FOUND);

    for uri in [
        "/api/user/rooms/discover?page=1&pageSize=10",
        "/api/user/rooms/room_123/discovery",
    ] {
        let user_app = register_all_routes().with_state(test_app_state());
        let user_request = test_request(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty()),
        )?;
        let user_response = test_response(user_app.oneshot(user_request).await)?;
        assert_eq!(user_response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    Ok(())
}

#[tokio::test]
async fn test_public_time_route_echoes_client_timestamp_without_auth() -> TestResult {
    let state = test_app_state();
    let clock = state.shared_api_runtime.client_api.clock.clone();
    let app = register_all_routes().with_state(state);
    let client_sent_at_nanos = 1_700_000_000_123_456_789_i64;
    let before = clock.now_nanos();

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/public/time?clientSentAtNanos={client_sent_at_nanos}"
            ))
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;
    let after = clock.now_nanos();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["clientSentAtNanos"], client_sent_at_nanos.to_string());
    let server_received_at_nanos = json["serverReceivedAtNanos"]
        .as_str()
        .ok_or_else(|| test_error("serverReceivedAtNanos should be a string"))?
        .parse::<i64>()?;
    let server_sent_at_nanos = json["serverSentAtNanos"]
        .as_str()
        .ok_or_else(|| test_error("serverSentAtNanos should be a string"))?
        .parse::<i64>()?;
    assert!(server_received_at_nanos >= before);
    assert!(server_received_at_nanos <= after);
    assert!(server_sent_at_nanos >= server_received_at_nanos);
    assert!(server_sent_at_nanos <= after);
    Ok(())
}

#[tokio::test]
async fn test_opaque_login_routes_are_registered() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for uri in [
        "/api/auth/opaque/login/start",
        "/api/auth/opaque/login/finish",
    ] {
        let request = test_request(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from("{")),
        )?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept POST"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_password_email_and_totp_auth_routes_are_registered() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for uri in [
        "/api/auth/login/start",
        "/api/auth/direct-password/register",
        "/api/auth/direct-password/login",
        "/api/auth/email/registration/request",
        "/api/auth/email/registration/confirm",
        "/api/auth/mfa/totp/verify",
        "/api/auth/mfa/recovery-code/verify",
    ] {
        let request = test_request(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from("{")),
        )?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept POST"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_passkey_login_routes_fail_closed_when_service_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let start_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkeys/login/start")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r"{}")),
    )?;
    let start_response = test_response(app.clone().oneshot(start_request).await)?;
    assert_eq!(start_response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let finish_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/passkeys/login/finish")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"sessionId":"session","credential":{"id":"cred","rawId":"cmF3","response":{"authenticatorData":"YXV0aA","clientDataJSON":"Y2xpZW50","signature":"c2ln"},"type":1}}"#,
            )),
    )?;
    let finish_response = test_response(app.oneshot(finish_request).await)?;
    assert_eq!(finish_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn test_account_security_routes_are_registered_and_require_authentication() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for (method, uri, body) in [
        ("GET", "/api/user/preferences", None),
        (
            "PATCH",
            "/api/user/preferences",
            Some(r#"{"notifications":{"roomEventInApp":true}}"#),
        ),
        (
            "PUT",
            "/api/user/two-factor",
            Some(r#"{"enabled":true,"verificationId":"verification-id"}"#),
        ),
        ("GET", "/api/user/passkeys", None),
        (
            "POST",
            "/api/user/passkeys/bind/start",
            Some(r#"{"name":"Laptop"}"#),
        ),
        (
            "POST",
            "/api/user/passkeys/bind/finish",
            Some(
                r#"{"sessionId":"session","credential":{"id":"cred","rawId":"cmF3","response":{"attestationObject":"YXR0","clientDataJSON":"Y2xpZW50"},"type":1},"verificationId":"verification-id"}"#,
            ),
        ),
        (
            "DELETE",
            "/api/user/passkeys/Y3JlZGVudGlhbA",
            Some(r#"{"verificationId":"verification-id"}"#),
        ),
        (
            "POST",
            "/api/user/totp/setup/start",
            Some(r#"{"verificationId":"verification-id"}"#),
        ),
        (
            "POST",
            "/api/user/totp/setup/finish",
            Some(r#"{"setupId":"setup-id","code":"123456"}"#),
        ),
        (
            "POST",
            "/api/user/totp/recovery-codes/regenerate",
            Some(r#"{"verificationId":"verification-id"}"#),
        ),
        (
            "DELETE",
            "/api/user/totp",
            Some(r#"{"verificationId":"verification-id"}"#),
        ),
        (
            "PUT",
            "/api/rooms/room_abc123/chat/messages/42/reactions/like",
            None,
        ),
        (
            "DELETE",
            "/api/rooms/room_abc123/chat/messages/42/reactions/like",
            None,
        ),
        (
            "GET",
            "/api/rooms/room_abc123/chat/messages/42/reactions/like/users",
            None,
        ),
    ] {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
        }
        let request = test_request(builder.body(body.map_or_else(Body::empty, Body::from)))?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept {method}"
        );
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} should follow the normal authenticated user route path"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_member_approval_routes_are_reachable_via_project_router() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    for (method, uri, body) in [
        (
            "POST",
            "/api/rooms/room1234_abx/members",
            Some(r#"{"userId":"usr_1","role":1,"notify":true}"#),
        ),
        ("GET", "/api/rooms/room1234_abx/reviews/joins", None),
        (
            "POST",
            "/api/rooms/room1234_abx/reviews/joins/AbC123xYz890/approve",
            None,
        ),
        (
            "POST",
            "/api/rooms/room1234_abx/reviews/joins/AbC123xYz890/reject",
            Some(r#"{"requestId":"usr_1","reason":"no longer eligible"}"#),
        ),
    ] {
        let builder = Request::builder().method(method).uri(uri);
        let builder = if body.is_some() {
            builder.header(axum::http::header::CONTENT_TYPE, "application/json")
        } else {
            builder
        };
        let request = test_request(builder.body(Body::from(
            body.map_or_else(String::new, ToString::to_string),
        )))?;
        let response = test_response(app.clone().oneshot(request).await)?;

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{uri} must be registered in the project router"
        );
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{uri} must accept its documented HTTP method"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_oauth2_unlink_route_is_reachable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/oauth2/type/github/unlink")
            .body(Body::empty()),
    )?;
    let new_route_response = test_response(app.clone().oneshot(request).await)?;
    assert_ne!(new_route_response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_oauth2_unlink_provider_path_rejects_non_canonical_case() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/oauth2/type/GitHub/unlink")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn test_delete_all_read_notifications_route_is_reachable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("DELETE")
            .uri("/api/notifications/read")
            .body(Body::empty()),
    )?;
    let new_route_response = test_response(app.clone().oneshot(request).await)?;
    assert_ne!(new_route_response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_main_router_does_not_expose_metrics_endpoint() -> TestResult {
    let mut state = test_app_state();
    Arc::make_mut(&mut state.router_options).runtime_settings = Arc::new({
        let mut config = (*state.runtime_settings).clone();
        config.metrics.enabled = true;
        config.metrics.auth.mode = synctv_api_common::api_runtime::MetricsAuthMode::BearerToken;
        config.metrics.auth.bearer_token = "metrics-secret".to_string();
        config
    });
    let app = register_all_routes().with_state(state);

    let request = test_request(Request::builder().uri("/metrics").body(Body::empty()))?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_provider_login_routes_reject_invalid_tokens_before_rate_limiting() -> TestResult {
    let state =
        test_app_state_with_rate_limits(synctv_api_common::api_runtime::RequestRateLimitSettings {
            auth_max_requests: 1,
            auth_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/alist/login")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
            )),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/alist/login")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
            )),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "provider login routes should consume the auth rate-limit bucket before invalid-token authentication fails"
    );
    Ok(())
}

#[tokio::test]
async fn test_auth_login_malformed_json_still_consumes_auth_rate_limit_bucket() -> TestResult {
    let state =
        test_app_state_with_rate_limits(synctv_api_common::api_runtime::RequestRateLimitSettings {
            auth_max_requests: 1,
            auth_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/email/confirm")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{invalid json")),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/auth/email/confirm")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{invalid json")),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "malformed auth payloads should still consume the auth rate-limit bucket"
    );
    Ok(())
}

#[tokio::test]
async fn test_bilibili_me_route_requires_post() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/bilibili/me")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "Bilibili /me must require POST so provider requests stay consistently structured"
    );
    Ok(())
}

#[tokio::test]
async fn test_ticket_route_uses_write_rate_limit_tier() -> TestResult {
    let state = test_app_state_with_websocket_runtime(
        synctv_api_common::api_runtime::RequestRateLimitSettings {
            write_max_requests: 1,
            write_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        },
    )
    .await;
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"roomId":"room_123"}"#)),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(
        first.status(),
        StatusCode::UNAUTHORIZED,
        "first unauthenticated ticket request should reach auth before exhausting the write bucket"
    );

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"roomId":"room_123"}"#)),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "ticket issuance should consume the write rate-limit bucket before unauthenticated requests reach impl authentication"
    );
    Ok(())
}

#[tokio::test]
async fn test_provider_proxy_routes_use_streaming_rate_limit_tier() -> TestResult {
    let state =
        test_app_state_with_rate_limits(synctv_api_common::api_runtime::RequestRateLimitSettings {
            streaming_max_requests: 1,
            streaming_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/playback-providers/bilibili/v1/hls-resources/direct/0/media?targetUrl=https%3A%2F%2Fcdn.example.com%2Fseg.ts&sig=s&uid=u&rid=r&exp=1")
            .body(Body::empty()),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let second_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/playback-providers/bilibili/v1/hls-resources/direct/0/media?targetUrl=https%3A%2F%2Fcdn.example.com%2Fseg.ts&sig=s&uid=u&rid=r&exp=1")
            .body(Body::empty()),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "playback provider endpoints must use the streaming rate-limit bucket"
    );
    Ok(())
}

#[tokio::test]
async fn test_direct_url_manifest_and_resource_routes_are_reachable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);
    let scope = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        "https://cdn.example.com/dash/video/",
    );
    let routes = [
        "/api/playback-providers/direct-url/v1/hls-resources/direct/0/media?targetUrl=https%3A%2F%2Fcdn.example.com%2Fsegment.ts&sig=s&uid=u&rid=r&exp=1".to_string(),
        "/api/playback-providers/direct-url/v1/dash-manifests/direct/0?sig=s&uid=u&rid=r&exp=1".to_string(),
        format!(
            "/api/playback-providers/direct-url/v1/dash-resources/direct/0/media/{scope}/u/r/1/s"
        ),
        format!(
            "/api/playback-providers/direct-url/v1/dash-resources/direct/0/media/{scope}/u/r/1/s/video/segment-1.m4s?token=x"
        ),
    ];

    for route in routes {
        let request = test_request(
            Request::builder()
                .method("GET")
                .uri(&route)
                .body(Body::empty()),
        )?;
        let response = test_response(app.clone().oneshot(request).await)?;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "route should reach playback access validation: {route}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_transport_layers_preserve_shared_http_metadata_without_global_timeout() -> TestResult
{
    let state = test_app_state();
    let app = apply_global_layers(
        Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "completed"
            }),
        ),
        &state,
    )?;

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/slow")
            .header("x-request-id", "transport-no-timeout-123")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "transport layers should no longer enforce a path-selected unary timeout"
    );
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("transport-no-timeout-123"),
        "request IDs must still be propagated without transport timeout wrapping"
    );
    assert_eq!(
        response
            .headers()
            .get("X-Frame-Options")
            .ok_or_else(|| test_error("missing X-Frame-Options header"))?,
        "DENY",
        "shared security headers must still be applied after removing transport timeout routing"
    );
    Ok(())
}

#[tokio::test]
async fn test_streaming_proxy_routes_preserve_options_preflight() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let rtmp_request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/api/playback-providers/rtmp/ver1/hls-playlist")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let rtmp_preflight = test_response(app.clone().oneshot(rtmp_request).await)?;
    assert_ne!(
        rtmp_preflight.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "RTMP playback-provider route must handle browser preflight"
    );

    let live_proxy_request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/api/playback-providers/live-proxy/ver1/hls-playlist")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let live_proxy_preflight = test_response(app.oneshot(live_proxy_request).await)?;
    assert_ne!(
        live_proxy_preflight.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "live_proxy playback-provider route must handle browser preflight"
    );
    Ok(())
}

#[tokio::test]
async fn test_cors_preflight_does_not_advertise_credentials() -> TestResult {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert!(
        response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .is_none(),
        "native-client-oriented CORS policy should not advertise credentialed browser requests by default"
    );
    Ok(())
}

#[tokio::test]
async fn test_cors_preflight_allows_request_correlation_headers() -> TestResult {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "x-request-id, traceparent, tracestate",
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);

    let allowed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS)
        .ok_or_else(|| test_error("preflight should advertise allowed headers"))?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(allowed_headers.contains("x-request-id"));
    assert!(allowed_headers.contains("traceparent"));
    assert!(allowed_headers.contains("tracestate"));
    Ok(())
}

#[tokio::test]
async fn test_cors_preflight_allows_upload_and_range_headers() -> TestResult {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("OPTIONS")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "PUT")
            .header(
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "content-range, range, x-synctv-file-upload-token",
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let allowed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS)
        .ok_or_else(|| test_error("preflight should advertise upload headers"))?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(allowed_headers.contains("content-range"));
    assert!(allowed_headers.contains("range"));
    assert!(allowed_headers.contains("x-synctv-file-upload-token"));
    Ok(())
}

#[tokio::test]
async fn test_cors_actual_response_exposes_request_id_header() -> TestResult {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let exposed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .ok_or_else(|| {
            test_error("CORS response should expose request correlation response headers")
        })?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(exposed_headers.contains("x-request-id"));
    Ok(())
}

#[tokio::test]
async fn test_cors_actual_response_exposes_upload_and_range_headers() -> TestResult {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(build_cors_layer(&config)?);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/test")
            .header(axum::http::header::ORIGIN, "https://example.com")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    let exposed_headers = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .ok_or_else(|| test_error("CORS response should expose upload and range headers"))?
        .to_str()
        .map_err(|error| test_error(error.to_string()))?
        .to_ascii_lowercase();
    assert!(exposed_headers.contains("x-synctv-upload-complete"));
    assert!(exposed_headers.contains("x-synctv-uploaded-size-bytes"));
    assert!(exposed_headers.contains("x-synctv-uploaded-parts"));
    assert!(exposed_headers.contains("content-range"));
    assert!(exposed_headers.contains("accept-ranges"));
    assert!(exposed_headers.contains("x-synctv-content-manifest-sha256"));
    Ok(())
}

#[cfg(feature = "openapi")]
#[tokio::test]
async fn test_openapi_json_route_is_available() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .uri("/api-docs/openapi.json")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["openapi"], "3.1.0");
    assert!(json["paths"]["/api/auth/email/confirm"].is_object());
    assert!(json["paths"]["/api/auth/direct-password/register"].is_object());
    assert!(json["paths"]["/api/auth/direct-password/login"].is_object());
    assert!(json["paths"]["/api/auth/login/start"].is_object());
    assert!(json["paths"]["/api/auth/email/registration/request"].is_object());
    assert!(json["paths"]["/api/auth/email/registration/confirm"].is_object());
    assert!(json["paths"]["/api/auth/mfa/totp/verify"].is_object());
    assert!(json["paths"]["/api/auth/mfa/recovery-code/verify"].is_object());
    assert!(json["paths"]["/api/tickets"].is_object());
    assert!(json["paths"]["/api/user"].is_object());
    assert!(json["paths"]["/api/user/totp/setup/start"].is_object());
    assert!(json["paths"]["/api/user/totp/setup/finish"].is_object());
    assert!(json["paths"]["/api/user/totp/recovery-codes/regenerate"].is_object());
    assert!(json["paths"]["/api/user/totp"].is_object());
    assert!(json["paths"]["/api/rooms/{roomId}/media"].is_object());
    assert!(json["paths"]["/api/admin/users"].is_object());
    assert!(json["paths"]["/api/rooms/{roomId}/webrtc/ice-servers"].is_object());
    assert!(json["paths"]["/api/oauth2/exchange"].is_object());
    assert!(json["paths"]["/api/oauth2/providers"].is_object());
    assert!(json["paths"]["/api/oauth2/{provider}/authorize"].is_object());
    assert!(json["paths"]["/api/notifications"].is_object());
    assert!(json["paths"]["/api/providers/bilibili/parse"].is_object());
    assert!(json["paths"]["/api/providers/youtube/resolve"].is_object());
    assert!(json["paths"]["/api/providers/alist/login"].is_object());
    assert!(json["paths"]["/api/providers/instances"].is_object());
    assert!(json["paths"]["/api/rooms/{roomId}/streams"].is_object());
    assert!(json["paths"]["/api/providers/rtmp/rooms/{roomId}/publish-key/{mediaId}"].is_object());
    assert!(json["paths"]["/api/providers/rtmp/rooms/{roomId}/info/{mediaId}"].is_object());
    assert_eq!(
        json["paths"]["/api/providers/alist/login"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_alist_LoginResponse"
    );
    assert_eq!(
        json["paths"]["/api/providers/emby/list"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_emby_ListResponse"
    );
    assert_eq!(
        json["paths"]["/api/providers/bilibili/login/qr/check"]["post"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_bilibili_QRStatusResponse"
    );
    assert_eq!(
        json["paths"]["/api/providers/bilibili/parse"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_provider_bilibili_ParseResponse"
    );
    assert!(
        json["components"]["schemas"]["synctv_provider_bilibili_ParseRequest"]["properties"]
            ["shared"]
            .is_null(),
        "Bilibili parsing should return credential-neutral source configs"
    );
    for request_schema in [
        "synctv_provider_twitch_ResolveRequest",
        "synctv_provider_twitch_ListChannelItemsRequest",
        "synctv_provider_douyin_ResolveRequest",
        "synctv_provider_douyin_ListUserPostsRequest",
        "synctv_provider_tiktok_ResolveRequest",
        "synctv_provider_tiktok_GetUserRequest",
        "synctv_provider_tiktok_ListUserPostsRequest",
        "synctv_provider_youtube_ResolveRequest",
    ] {
        assert!(
            json["components"]["schemas"][request_schema]["properties"]["shared"].is_null(),
            "{request_schema} should return credential-neutral source configs"
        );
    }
    let bilibili_parse_candidate =
        &json["components"]["schemas"]["synctv_provider_bilibili_ParseCandidate"];
    let source_config_variants = json["components"]["schemas"]
        ["synctv_provider_bilibili_ParseCandidate_source_config"]["oneOf"]
        .as_array()
        .ok_or_else(|| test_error("Bilibili ParseCandidate source config variants"))?;
    assert!(
        bilibili_parse_candidate["allOf"].is_array(),
        "Bilibili parse candidates should flatten their source config"
    );
    assert!(
        source_config_variants
            .iter()
            .any(|variant| variant["properties"]["media"].is_object()),
        "Bilibili parse candidates should expose typed media source config"
    );
    assert!(
        source_config_variants
            .iter()
            .any(|variant| variant["properties"]["playlist"].is_object()),
        "Bilibili parse candidates should expose typed playlist source config"
    );
    assert_eq!(
        json["paths"]["/api/user"]["patch"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/synctv_client_User"
    );
    assert_eq!(
        json["paths"]["/api/tickets"]["post"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/synctv_client_CreateWebSocketTicketResponse"
    );
    assert_eq!(
        json["paths"]["/api/rooms/{roomId}/webrtc/ice-servers"]["get"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/synctv_client_GetIceServersResponse"
    );

    let alist_login_ref = json["paths"]["/api/providers/alist/login"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .ok_or_else(|| test_error("alist login schema ref"))?;
    let auth_login_ref = json["paths"]["/api/auth/email/confirm"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .ok_or_else(|| test_error("auth login schema ref"))?;
    let emby_login_ref = json["paths"]["/api/providers/emby/login"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .ok_or_else(|| test_error("emby login schema ref"))?;
    assert_eq!(
        auth_login_ref,
        "#/components/schemas/synctv_client_LoginResponse"
    );
    assert_ne!(
        auth_login_ref, alist_login_ref,
        "client login and provider login must use distinct OpenAPI components"
    );
    assert_ne!(
        alist_login_ref, emby_login_ref,
        "distinct provider response types must not collapse onto the same OpenAPI component"
    );

    let alist_login_schema_name = alist_login_ref
        .rsplit('/')
        .next()
        .ok_or_else(|| test_error("alist login schema name"))?;
    let emby_login_schema_name = emby_login_ref
        .rsplit('/')
        .next()
        .ok_or_else(|| test_error("emby login schema name"))?;

    let alist_login_properties =
        &json["components"]["schemas"][alist_login_schema_name]["properties"];
    assert!(
        alist_login_properties["token"].is_object(),
        "alist login schema should expose token"
    );
    assert!(
        alist_login_properties["serverId"].is_object(),
        "alist login schema should expose serverId"
    );
    assert!(
        alist_login_properties["userId"].is_null(),
        "alist login schema must not be overwritten by emby login response"
    );

    let emby_login_properties =
        &json["components"]["schemas"][emby_login_schema_name]["properties"];
    assert!(
        emby_login_properties["userId"].is_object(),
        "emby login schema should expose user_id"
    );
    assert!(
        emby_login_properties["username"].is_object(),
        "emby login schema should expose username"
    );
    assert!(
        emby_login_properties["isAdmin"].is_object(),
        "emby login schema should expose isAdmin"
    );
    assert!(
        emby_login_properties["token"].is_null(),
        "emby login schema must not be overwritten by alist login response"
    );
    Ok(())
}

#[cfg(feature = "openapi")]
#[tokio::test]
async fn test_swagger_ui_route_is_available() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(Request::builder().uri("/swagger-ui/").body(Body::empty()))?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[test]
fn test_build_cors_layer_rejects_invalid_configured_origin() {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec![
        "https://example.com".to_string(),
        "not a valid origin".to_string(),
    ];

    let result = build_cors_layer(&config);

    assert!(
        result.is_err(),
        "invalid configured CORS origins must fail fast instead of being silently ignored"
    );
}

#[test]
fn test_build_cors_layer_rejects_configured_origin_with_path() {
    let mut config = synctv_api_common::ApiRuntimeSettings::default();
    config.server.cors_allowed_origins = vec!["https://example.com/app".to_string()];

    let result = build_cors_layer(&config);

    assert!(
        result.is_err(),
        "configured CORS origins with paths must fail fast during router construction"
    );
}

#[tokio::test]
async fn test_provider_common_routes_rate_limit_invalid_tokens_before_authentication() -> TestResult
{
    let state =
        test_app_state_with_rate_limits(synctv_api_common::api_runtime::RequestRateLimitSettings {
            admin_max_requests: 1,
            admin_window_seconds: 60,
            auth_max_requests: 100,
            auth_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        });
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/instances")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .body(Body::empty()),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(
        first.status(),
        StatusCode::UNAUTHORIZED,
        "first provider common request should still reach authentication while the admin bucket has capacity"
    );

    let second_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/instances")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .body(Body::empty()),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "provider common routes should consume the admin rate-limit bucket before invalid-token authentication fails"
    );
    Ok(())
}

#[tokio::test]
async fn test_provider_management_routes_do_not_consume_outer_read_bucket() -> TestResult {
    let state =
        test_app_state_with_rate_limits(synctv_api_common::api_runtime::RequestRateLimitSettings {
            read_max_requests: 1,
            read_window_seconds: 60,
            auth_max_requests: 100,
            auth_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        });
    let app = register_all_routes().with_state(state);

    let management_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/alist/login")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
            )),
    )?;
    let management = test_response(app.clone().oneshot(management_request).await)?;
    assert_eq!(
        management.status(),
        StatusCode::UNAUTHORIZED,
        "provider auth routes should hit their own auth limiter without consuming the outer read bucket"
    );

    let common_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/instances")
            .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
            .body(Body::empty()),
    )?;
    let common = test_response(app.oneshot(common_request).await)?;
    assert_eq!(
        common.status(),
        StatusCode::UNAUTHORIZED,
        "provider management traffic must not drain the provider-common read bucket"
    );
    Ok(())
}

#[tokio::test]
async fn test_ticket_routes_use_write_rate_limit_tier() -> TestResult {
    let state = test_app_state_with_websocket_runtime(
        synctv_api_common::api_runtime::RequestRateLimitSettings {
            write_max_requests: 1,
            write_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_api_common::api_runtime::RequestRateLimitSettings::default()
        },
    )
    .await;
    let app = register_all_routes().with_state(state);

    let first_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"roomId":"room_123"}"#)),
    )?;
    let first = test_response(app.clone().oneshot(first_request).await)?;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let second_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"roomId":"room_123"}"#)),
    )?;
    let second = test_response(app.oneshot(second_request).await)?;
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "ticket creation should consume the write rate-limit bucket before unauthenticated requests reach impl authentication"
    );
    Ok(())
}

#[tokio::test]
async fn test_ticket_route_fails_closed_when_websocket_runtime_is_unavailable() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"roomId":"room1234_abx"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "ticket issuance must fail closed with service unavailable when websocket runtime dependencies are unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn test_websocket_route_fails_closed_when_runtime_is_unavailable_before_upgrade_checks(
) -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/ws/rooms/AbC123xYz890")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "websocket runtime checks must fail closed before WebSocketUpgrade extraction would otherwise return 400"
    );
    Ok(())
}

#[tokio::test]
async fn test_websocket_ticket_runtime_gate_does_not_leak_to_other_write_routes() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("PATCH")
            .uri("/api/user")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"newUsername":"patched-name"}"#)),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "write routes unrelated to ticket issuance must keep their normal auth path when websocket runtime dependencies are unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn test_rtmp_publish_key_routes_are_reachable_under_api() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let api_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/providers/rtmp/rooms/room_AbC123xYz890/publish-key/med_ZyX098wVu765")
            .body(Body::empty()),
    )?;
    let api_response = test_response(app.clone().oneshot(api_request).await)?;
    assert_eq!(api_response.status(), StatusCode::UNAUTHORIZED);

    let info_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/providers/rtmp/rooms/room_AbC123xYz890/info/med_ZyX098wVu765")
            .body(Body::empty()),
    )?;
    let info_api_response = test_response(app.clone().oneshot(info_request).await)?;
    assert_eq!(info_api_response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_oauth2_routes_fail_closed_when_service_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/oauth2/providers")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn test_optional_user_execution_rejects_invalid_authorization_header() -> TestResult {
    let state = test_app_state();
    let request_meta = synctv_api_common::impls::RequestMetadata::new(
        synctv_api_common::impls::TransportProtocol::Http,
    )
    .with_authorization(Some("Bearer malformed-token".to_string()))
    .with_client_ip(Some("127.0.0.1".parse()?));

    let err =
        match state
            .shared_api_runtime
            .request_executor
            .execute_optional_user_with_control(
                &request_meta,
                synctv_api_common::impls::EndpointRateLimitCategory::Auth,
                |_control, _authenticated| async move {
                    Ok::<_, synctv_api_common::impls::ApiError>(())
                },
            )
            .await
        {
            Ok(()) => return Err(test_error("invalid bearer token must be rejected")),
            Err(error) => error,
        };

    assert!(
        matches!(err.classify(), synctv_api_common::impls::ErrorKind::Unauthenticated),
        "strict optional-auth execution must reject invalid bearer headers instead of downgrading to anonymous",
    );
    Ok(())
}

#[tokio::test]
async fn test_http_request_metadata_rejects_non_utf8_authorization_header() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/discover")
            .header(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_bytes(&[0xff])?,
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_http_request_metadata_rejects_non_utf8_user_agent_header() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/discover")
            .header(
                axum::http::header::USER_AGENT,
                axum::http::HeaderValue::from_bytes(&[0xff])?,
            )
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn test_notification_routes_fail_closed_when_service_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let read_request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/notifications")
            .body(Body::empty()),
    )?;
    let read_response = test_response(app.clone().oneshot(read_request).await)?;
    assert_eq!(read_response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let write_request = test_request(
        Request::builder()
            .method("POST")
            .uri("/api/notifications/read-all")
            .body(Body::empty()),
    )?;
    let write_response = test_response(app.oneshot(write_request).await)?;
    assert_eq!(write_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn test_live_provider_routes_remain_registered_when_infrastructure_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/api/rooms/room_abc123/streams")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_websocket_routes_fail_closed_when_dependencies_missing() -> TestResult {
    let state = test_app_state();
    let app = register_all_routes().with_state(state);

    let request = test_request(
        Request::builder()
            .method("GET")
            .uri("/ws/rooms/room1234_abx")
            .body(Body::empty()),
    )?;
    let response = test_response(app.oneshot(request).await)?;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "websocket route must fail closed before auth/query validation when runtime dependencies are unavailable"
    );
    Ok(())
}
