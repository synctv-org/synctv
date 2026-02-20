//! Bootstrap module for initializing the `SyncTV` server
//!
//! This module handles:
//! - Database initialization
//! - Configuration loading
//! - Service initialization and dependency injection
//! - User bootstrap (root user creation)

pub mod database;
pub mod config;
pub mod redis;
pub mod services;
pub mod user;

pub use database::init_database;
pub use config::load_config;
pub use redis::{RedisHandles, init_redis};
pub use services::init_services;
pub use user::{bootstrap_root_user, has_any_users};
