#![allow(clippy::unwrap_used)]
//! Shared test helpers for synctv-core tests
//!
//! This crate provides common test utilities to reduce code duplication
//! across integration tests.

pub mod assertions;
pub mod constants;
pub mod fixtures;
pub mod postgres;
pub mod services;

// Re-export commonly used items
pub use fixtures::{TestRoom, TestUser};
pub use postgres::{create_test_pool, create_test_pool_with_db, TestContainer};
pub use services::{
    create_test_attempt_tracker, create_test_brute_force_protection, create_test_jwt_service,
    create_test_jwt_service_with_secret, create_test_token_blacklist_store,
};
