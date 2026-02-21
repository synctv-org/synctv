//! Testcontainers-based infrastructure for integration tests.
//!
//! Provides `TestInfra` which automatically starts Postgres and Redis containers,
//! runs migrations, and provides ready-to-use connections.

use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;

/// Default PostgreSQL version for test containers
const POSTGRES_VERSION: &str = "16-alpine";
/// Default Redis version for test containers
const REDIS_VERSION: &str = "7-alpine";

/// Test infrastructure that manages Postgres and Redis containers.
///
/// Containers are automatically stopped when this struct is dropped.
///
/// # Example
///
/// ```text
/// let infra = TestInfra::new().await;
/// let pool = &infra.pool;
/// // use pool for database operations...
/// ```
pub struct TestInfra {
    pub pool: PgPool,
    pub redis_client: redis::Client,
    pub redis_url: String,
    // Keep containers alive for the lifetime of the test
    _postgres: ContainerAsync<Postgres>,
    _redis: ContainerAsync<Redis>,
}

impl TestInfra {
    /// Start Postgres and Redis containers, run migrations, and return connections.
    pub async fn new() -> Self {
        // Start containers in parallel
        // Use PostgreSQL 16-alpine which has gen_random_uuid() built-in and supports
        // BEFORE ROW triggers on partitioned tables
        // Use Redis 7-alpine for modern features and performance
        let (pg_container, redis_container) = tokio::join!(
            Postgres::default()
                .with_db_name("synctv_test")
                .with_user("synctv")
                .with_password("synctv_test")
                .with_tag(POSTGRES_VERSION)
                .start(),
            Redis::default()
                .with_tag(REDIS_VERSION)
                .start(),
        );

        let pg_container = pg_container.expect("Failed to start Postgres container");
        let redis_container = redis_container.expect("Failed to start Redis container");

        // Get mapped ports
        let pg_host = pg_container.get_host().await.expect("Failed to get Postgres host");
        let pg_port = pg_container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get Postgres port");

        let redis_host = redis_container.get_host().await.expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        // Build connection URLs
        let database_url = format!(
            "postgresql://synctv:synctv_test@{}:{}/synctv_test",
            pg_host, pg_port
        );
        let redis_url = format!("redis://{}:{}", redis_host, redis_port);

        // Connect to Postgres
        let pool = PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to Postgres container");

        // Run migrations
        sqlx::migrate!("../migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        // Create Redis client
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        // Verify Redis connectivity
        let _: () = redis::cmd("PING")
            .query_async(
                &mut redis_client
                    .get_multiplexed_async_connection()
                    .await
                    .expect("Failed to connect to Redis container"),
            )
            .await
            .expect("Redis PING failed");

        Self {
            pool,
            redis_client,
            redis_url,
            _postgres: pg_container,
            _redis: redis_container,
        }
    }

    /// Create a Redis `ConnectionManager` for use with services that require one.
    pub async fn redis_connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }

    /// Start only Postgres (no Redis). Useful for DB-only tests.
    pub async fn postgres_only() -> TestPostgres {
        // Use PostgreSQL 16-alpine which has gen_random_uuid() built-in and supports
        // BEFORE ROW triggers on partitioned tables
        let pg_container = Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag(POSTGRES_VERSION)
            .start()
            .await
            .expect("Failed to start Postgres container");

        let pg_host = pg_container.get_host().await.expect("Failed to get Postgres host");
        let pg_port = pg_container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get Postgres port");

        let database_url = format!(
            "postgresql://synctv:synctv_test@{}:{}/synctv_test",
            pg_host, pg_port
        );

        let pool = PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to Postgres container");

        sqlx::migrate!("../migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        TestPostgres {
            pool,
            _postgres: pg_container,
        }
    }

    /// Start only Redis (no Postgres). Useful for Redis-only tests.
    pub async fn redis_only() -> TestRedis {
        // Use Redis 7-alpine for modern features and performance
        let redis_container = Redis::default()
            .with_tag(REDIS_VERSION)
            .start()
            .await
            .expect("Failed to start Redis container");

        let redis_host = redis_container.get_host().await.expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        let redis_url = format!("redis://{}:{}", redis_host, redis_port);

        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        TestRedis {
            redis_client,
            redis_url,
            _redis: redis_container,
        }
    }
}

/// Postgres-only test infrastructure.
pub struct TestPostgres {
    pub pool: PgPool,
    _postgres: ContainerAsync<Postgres>,
}

/// Redis-only test infrastructure.
pub struct TestRedis {
    pub redis_client: redis::Client,
    pub redis_url: String,
    _redis: ContainerAsync<Redis>,
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
}
