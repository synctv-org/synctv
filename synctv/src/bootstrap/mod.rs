pub mod cluster;
mod database;
pub mod livestream;
pub mod node_id;
mod redis;
mod services;
mod user;
pub mod webrtc;

pub(crate) use synctv_core::RedisDeploymentMode;

pub(crate) use database::{
    DatabaseInitOptions, DatabasePoolOptions, DatabasePools, init_database,
    init_database_with_read_pool_and_cancel,
};
pub(crate) use redis::{RedisConnectionOptions, RedisInitOptions, init_redis};
pub(crate) use services::{
    CacheOptions, CoreServicesOptions, FileStorageBackendOptions,
    FileStorageDatabaseCompressionOption, FileStorageDatabaseOptions, FileStorageOptions,
    FileStorageS3Options, InitServicesOptions, JwtOptions, MessagingRateLimitOptions,
    SecurityOptions, Services, SsrfOptions, init_services_with_options,
};
pub(crate) use user::{RootUserBootstrapOptions, bootstrap_root_user, has_any_admin_users};
