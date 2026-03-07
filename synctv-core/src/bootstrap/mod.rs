//! Bootstrap module for initializing the `SyncTV` server
//!
//! This module handles:
//! - Database initialization
//! - Configuration loading
//! - Service initialization and dependency injection
//! - User bootstrap (root user creation)

pub mod config;
pub mod database;
pub mod redis;
pub mod services;
pub mod user;

pub use config::load_config;
pub use database::init_database;
pub use redis::{init_redis, RedisHandles};
pub use services::init_services;
pub use user::{bootstrap_root_user, has_any_admin_users, has_any_users};
