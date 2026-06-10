use std::sync::Arc;

use synctv_api::impls::{AdminApiRuntime, ClientApiRuntime, RequestExecutor};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    proxy_signature::ProxySigningKey,
    service::{
        auth::{BruteForceProtection, JwtService, JwtValidator, SecurityPipeline},
        InMemoryTokenBlacklistStore, RateLimiter, UserService,
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
        Arc::new(synctv_core::Config::default()),
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

pub fn client_api_runtime() -> ClientApiRuntime {
    ClientApiRuntime::local_disabled(
        Arc::new(local_request_executor()),
        proxy_signing_key(b"test-client-api-runtime-signing-key-32-bytes"),
    )
}

#[allow(dead_code)]
pub fn admin_api_runtime() -> AdminApiRuntime {
    AdminApiRuntime::local_disabled(
        Arc::new(local_request_executor()),
        proxy_signing_key(b"test-admin-api-runtime-signing-key-32-bytes!"),
        Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
            "test:admin:",
        )),
    )
}
