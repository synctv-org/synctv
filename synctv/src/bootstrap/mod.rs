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
    init_database, init_database_with_read_pool_and_cancel, DatabaseInitOptions,
    DatabasePoolOptions, DatabasePools,
};
pub(crate) use redis::{init_redis, RedisConnectionOptions, RedisInitOptions};
pub(crate) use services::{
    init_services_with_options, CacheOptions, CoreServicesOptions, FileStorageBackendOptions,
    FileStorageDatabaseCompressionOption, FileStorageDatabaseOptions, FileStorageOptions,
    FileStorageS3Options, InitServicesOptions, JwtOptions, MessagingRateLimitOptions,
    SecurityOptions, Services, SsrfOptions,
};
pub(crate) use user::{bootstrap_root_user, has_any_admin_users, RootUserBootstrapOptions};
