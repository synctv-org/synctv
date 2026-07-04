//! Bootstrap module for initializing the `SyncTV` server
//!
//! This module handles:
//! - Database initialization
//! - Configuration loading
//! - Service initialization and dependency injection
//! - User bootstrap (root user creation)

mod config;
mod database;
mod redis;
mod services;
mod user;

pub use config::{load_config, load_config_with_options, load_dotenv, LoadConfigOptions};
pub use database::{
    init_database, init_database_with_read_pool_and_cancel, DatabaseInit, DatabasePools,
};
pub use redis::{init_redis, RedisInit};
pub use services::{init_services, init_services_with_options, InitServicesOptions, Services};
pub use user::{bootstrap_root_user, has_any_admin_users, has_any_users};
