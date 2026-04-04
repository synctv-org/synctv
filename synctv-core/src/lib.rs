#![cfg_attr(test, allow(clippy::unwrap_used))]

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
pub mod repository;
pub mod resilience;
pub mod secrets;
pub mod service;
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
pub use transaction::{with_transaction, UnitOfWork};
