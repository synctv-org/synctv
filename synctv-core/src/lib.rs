#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive — use only one");

pub mod bench_support;
pub mod bootstrap;
pub mod cache;
pub mod config;
pub mod error;
pub mod logging;
pub mod metrics;
pub mod models;
pub mod oauth2;
pub mod provider;
pub mod redis_runtime;
pub mod repository;
pub mod resilience;
pub mod secrets;
pub mod service;
pub mod shared_state;
pub mod spawn;
pub mod time {
    pub use synctv_common::time::*;
}
pub mod transaction;
pub mod validation;

#[cfg(test)]
pub mod test_helpers;

pub use cache::KeyBuilder;
pub use config::{Config, GrpcRateLimitConfig, HttpRateLimitConfig};
pub use error::{Error, InternalExt, Result};
pub use redis_runtime::{
    coordination_runtime_from_client, direct_runtime, direct_runtime_from_conn, shared_runtime,
    shared_runtime_from_conn, DirectRedisConnectionRuntime, ManagedRedisRuntime,
    OnDemandRedisRuntime, RedisConnectionRuntime, RedisCoordinationRuntime,
    SharedRedisConnectionRuntime,
};
pub use shared_state::{SharedStateMode, SharedStateProfile};
pub use transaction::{with_transaction, UnitOfWork};
