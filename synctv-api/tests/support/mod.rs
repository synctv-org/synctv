use std::sync::Arc;

use synctv_api::{
    AdminApiRuntime, AdminReadServices, ClientApiRuntime, ProxySigningKey, RequestExecutor,
};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, JwtValidator, RateLimiter,
        SecurityPipeline, UserService,
    },
};

pub fn local_request_executor() -> RequestExecutor {
    let jwt_service = JwtService::new("test-request-executor-secret-minimum-32-chars")
        .expect("test JWT service should build");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://synctv:synctv@127.0.0.1:5432/synctv_test")
        .expect("test pool should build lazily");
    let user_service = Arc::new(UserService::new_for_tests(
        &pool,
        jwt_service.clone(),
        UsernameCache::local_only("test:request-executor:user:".to_string(), 100, 60),
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400)),
        KeyBuilder::new("test:request-executor"),
        BruteForceProtection::in_memory("test:request-executor:brute:".to_string()),
    ));
    RequestExecutor::new(
        Arc::new(synctv_api::ApiRuntimeSettings::default()),
        Arc::new(JwtValidator::new(Arc::new(jwt_service))),
        Arc::new(SecurityPipeline::new(&user_service)),
        Arc::new(RateLimiter::local_only(
            "test:request-executor:rate:".to_string(),
        )),
    )
}

pub fn proxy_signing_key(seed: &'static [u8]) -> Arc<ProxySigningKey> {
    Arc::new(ProxySigningKey::try_derive_from(seed).expect("test signing key should derive"))
}

pub fn media_swarm_signing_key(seed: &'static [u8]) -> Arc<synctv_api::MediaSwarmSigningKey> {
    Arc::new(
        synctv_api::MediaSwarmSigningKey::try_derive_from(seed)
            .expect("test media swarm signing key should derive"),
    )
}

pub fn client_api_runtime() -> ClientApiRuntime {
    ClientApiRuntime::local_disabled(
        Arc::new(local_request_executor()),
        proxy_signing_key(b"test-client-api-runtime-signing-key-32-bytes"),
        media_swarm_signing_key(b"test-client-api-media-swarm-signing-key-32-bytes"),
    )
}

#[allow(dead_code)]
pub fn admin_api_runtime() -> AdminApiRuntime {
    AdminApiRuntime::local_disabled(
        Arc::new(local_request_executor()),
        proxy_signing_key(b"test-admin-api-runtime-signing-key-32-bytes!"),
        media_swarm_signing_key(b"test-admin-api-media-swarm-signing-key-32-bytes!"),
        Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
            "test:admin:",
        )),
    )
}

#[allow(dead_code)]
pub fn admin_read_services(user_service: &UserService) -> AdminReadServices {
    let write_pool = user_service.pool().clone();
    let read_pool = user_service.eventually_consistent_pool().clone();
    AdminReadServices {
        system_stats_service: Arc::new(synctv_core::service::SystemStatsService::new(
            read_pool.clone(),
        )),
        review_service: Arc::new(synctv_core::service::ReviewService::new_with_read_pool(
            write_pool.clone(),
            read_pool.clone(),
        )),
        ban_record_service: Arc::new(synctv_core::service::BanRecordService::new_with_read_pool(
            write_pool.clone(),
            read_pool.clone(),
        )),
        content_report_service: Arc::new(
            synctv_core::service::ContentReportService::new_with_read_pool(write_pool, read_pool),
        ),
    }
}
