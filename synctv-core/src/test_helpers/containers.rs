//! Thin compatibility wrappers over `synctv_core_testing`.
//!
//! `synctv-core` unit tests historically used a local testcontainers helper
//! implementation while integration tests used the dedicated
//! `synctv_core_testing` crate. Keeping two independent container stacks caused
//! drift in startup parallelism, cleanup strategy, timeouts and naming.
//!
//! This module preserves the existing `crate::test_helpers::containers::*`
//! call sites used by in-crate tests, but delegates all real container work to
//! the shared `synctv_core_testing` helpers so there is a single source of
//! truth for Docker-backed test infrastructure.

use sqlx::PgPool;

/// Test infrastructure that manages Postgres and Redis containers.
///
/// Containers are automatically stopped when this struct is dropped.
pub struct TestInfra {
    pub pool: PgPool,
    pub redis_client: redis::Client,
    pub redis_url: String,
    postgres: synctv_core_testing::TestContainer,
    redis: synctv_core_testing::RedisContainer,
}

impl TestInfra {
    /// Start Postgres and Redis containers, run migrations, and return connections.
    pub async fn new() -> Self {
        let (postgres, pool) =
            synctv_core_testing::create_test_pool_with_db_and_label("synctv_test", "infra").await;
        let (redis, redis_url) = synctv_core_testing::start_redis_url_with_label("infra").await;
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        Self {
            pool,
            redis_client,
            redis_url,
            postgres,
            redis,
        }
    }

    /// Create a Redis `ConnectionManager` for use with services that require one.
    pub async fn redis_connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }

    pub async fn cleanup(self) {
        self.pool.close().await;
        self.postgres.cleanup().await;
        self.redis.cleanup().await;
    }

    /// Start only Postgres (no Redis). Useful for DB-only tests.
    pub async fn postgres_only() -> TestPostgres {
        let (postgres, pool) =
            synctv_core_testing::create_test_pool_with_db_and_label("synctv_test", "postgres-only")
                .await;

        TestPostgres { pool, postgres }
    }

    /// Start only Redis (no Postgres). Useful for Redis-only tests.
    pub async fn redis_only() -> TestRedis {
        let (redis, redis_url) =
            synctv_core_testing::start_redis_url_with_label("redis-only").await;
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        TestRedis {
            redis_client,
            redis_url,
            redis,
        }
    }
}

/// Postgres-only test infrastructure.
pub struct TestPostgres {
    pub pool: PgPool,
    postgres: synctv_core_testing::TestContainer,
}

impl TestPostgres {
    pub async fn cleanup(self) {
        self.pool.close().await;
        self.postgres.cleanup().await;
    }
}

/// Redis-only test infrastructure.
pub struct TestRedis {
    pub redis_client: redis::Client,
    pub redis_url: String,
    redis: synctv_core_testing::RedisContainer,
}

impl TestRedis {
    /// Start only Redis (no Postgres). Alias for `TestInfra::redis_only()`.
    pub async fn new() -> Self {
        TestInfra::redis_only().await
    }

    /// Create a Redis `ConnectionManager` for use with services that require one.
    pub async fn connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }

    pub async fn cleanup(self) {
        self.redis.cleanup().await;
    }
}
