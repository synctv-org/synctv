#![recursion_limit = "256"]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
pub fn install_process_crypto_provider() {
    if rustls::crypto::CryptoProvider::install_default(default_crypto_provider()).is_err() {
        tracing::debug!("Process rustls crypto provider was already installed");
    }
    if default_jwt_crypto_provider().install_default().is_err() {
        tracing::debug!("Process JWT crypto provider was already installed");
    }
}

#[cfg(not(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
)))]
pub const fn install_process_crypto_provider() {}

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "tls-aws-lc")]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }

    #[cfg(all(
        not(feature = "tls-aws-lc"),
        any(
            feature = "tls-ring",
            feature = "tls-webpki-roots",
            feature = "tls-native-roots"
        )
    ))]
    {
        rustls::crypto::ring::default_provider()
    }
}

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
fn default_jwt_crypto_provider() -> &'static jsonwebtoken::crypto::CryptoProvider {
    #[cfg(feature = "tls-aws-lc")]
    {
        &jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER
    }

    #[cfg(all(
        not(feature = "tls-aws-lc"),
        any(
            feature = "tls-ring",
            feature = "tls-webpki-roots",
            feature = "tls-native-roots"
        )
    ))]
    {
        &jsonwebtoken::crypto::rust_crypto::DEFAULT_PROVIDER
    }
}

pub mod cache;
pub mod clock;
pub mod credential_encryption;
pub mod error;
pub mod logging;
pub mod metrics;
pub mod models;
pub mod oauth2;
pub mod provider;
pub mod redis_runtime;
pub mod repository;
pub mod resilience;
pub mod service;
pub mod shared_state;
pub mod spawn;
pub mod validation;

#[cfg(test)]
pub(crate) mod test_helpers;

pub use cache::KeyBuilder;
pub use clock::{
    Clock, ClockSyncOptions, ClockSyncProvider, ClockSyncSntpProviderOptions, SyncedClock,
    SyncedClockStatus, SystemClock, TimeOptions,
};
pub use error::{Error, InternalExt, Result};
pub use redis_runtime::{
    coordination_runtime_from_client,
    coordination_runtime_from_client_with_connection_options_and_operation_timeout, direct_runtime,
    redis_connection_manager_options, redis_runtime_snapshot, shared_runtime,
    shared_runtime_from_conn, DirectRedisConnectionRuntime, ManagedRedisRuntime,
    OnDemandRedisRuntime, RedisConnectionRuntime, RedisCoordinationRuntime, RedisDeploymentMode,
    SharedRedisConnectionRuntime,
};
pub use shared_state::{SharedStateMode, SharedStateProfile};
