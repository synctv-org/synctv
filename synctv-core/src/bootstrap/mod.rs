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

pub use config::{load_config, load_config_with_options, load_dotenv, LoadConfigOptions};
pub use database::{
    acquire_unbounded_ddl_connection, init_database, init_database_with_read_pool_and_cancel,
    DatabaseInit, DatabasePools,
};
pub use redis::{init_redis, RedisInit};
pub use services::init_services;
pub use user::{bootstrap_root_user, has_any_admin_users, has_any_users};
