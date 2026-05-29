//! `PostgreSQL` test container helpers

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgConnection, PgPoolOptions, PgSslMode};
use sqlx::Connection as _;
use sqlx::PgPool;
use testcontainers::core::wait::LogWaitStrategy;
use testcontainers::core::{ImageExt, ReuseDirective, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;
use url::Url;

use crate::docker::{
    acquire_run_lock, candidate_endpoints_for_host, cleanup_orphaned_run_lock_files,
    cleanup_orphaned_testcontainers, current_test_run_id as docker_current_test_run_id,
    current_test_run_id_from as docker_current_test_run_id_from,
    docker_named_container_belongs_to_current_run, docker_port_candidates, host_address_family,
    sanitize_container_name, startup_error_is_named_container_conflict, startup_error_is_retriable,
    ProcessLock, TEST_RUN_LABEL,
};

/// Default `PostgreSQL` version for test containers
pub const POSTGRES_VERSION: &str = "18";
const DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 300;
const MIN_DOCKER_STARTUP_TIMEOUT_SECS: u64 = 30;
const DOCKER_STARTUP_TIMEOUT_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS";
const DEFAULT_DOCKER_STARTUP_PARALLELISM: usize = 8;
const MIN_DOCKER_STARTUP_PARALLELISM: usize = 1;
const DOCKER_STARTUP_PARALLELISM_ENV: &str = "SYNCTV_TEST_DOCKER_STARTUP_PARALLELISM";
const DEFAULT_SHARED_ADMIN_POOL_MAX_CONNECTIONS: u32 = 64;
const MIN_SHARED_ADMIN_POOL_MAX_CONNECTIONS: u32 = 2;
const SHARED_ADMIN_POOL_MAX_CONNECTIONS_ENV: &str = "SYNCTV_TEST_PG_ADMIN_POOL_MAX_CONNECTIONS";
const DEFAULT_TEMPLATE_CLONE_PARALLELISM: usize = 4;
const MIN_TEMPLATE_CLONE_PARALLELISM: usize = 1;
const TEMPLATE_CLONE_PARALLELISM_ENV: &str = "SYNCTV_TEST_PG_TEMPLATE_CLONE_PARALLELISM";
const DEFAULT_TEST_POOL_MAX_CONNECTIONS: u32 = 32;
const MIN_TEST_POOL_MAX_CONNECTIONS: u32 = 1;
const TEST_POOL_MAX_CONNECTIONS_ENV: &str = "SYNCTV_TEST_PG_POOL_MAX_CONNECTIONS";
const ADMIN_DATABASE: &str = "postgres";
const TEMPLATE_DATABASE_PREFIX: &str = "synctv_template";
const TEST_DATABASE_PREFIX: &str = "synctv_test";
const MAX_DATABASE_IDENTIFIER_LEN: usize = 63;
const DATABASE_NAME_RANDOM_LEN: usize = 10;
const POSTGRES_EPHEMERAL_TUNING_ARGS: &[&str] = &[
    "-c",
    "fsync=off",
    "-c",
    "synchronous_commit=off",
    "-c",
    "full_page_writes=off",
    "-c",
    "wal_level=minimal",
    "-c",
    "max_wal_senders=0",
    "-c",
    "checkpoint_timeout=1h",
    "-c",
    "max_wal_size=8GB",
    "-c",
    "autovacuum=off",
    "-c",
    "max_worker_processes=4",
    "-c",
    "max_connections=1024",
    "-c",
    "superuser_reserved_connections=0",
    "-c",
    "shared_buffers=512MB",
    "-c",
    "temp_buffers=1MB",
    "-c",
    "work_mem=1MB",
    "-c",
    "maintenance_work_mem=256MB",
    "-c",
    "effective_cache_size=1GB",
    "-c",
    "jit=off",
    "-c",
    "max_parallel_workers_per_gather=0",
    "-c",
    "max_parallel_workers=0",
    "-c",
    "random_page_cost=1.0",
    "-c",
    "log_statement=none",
    "-c",
    "log_duration=off",
    "-c",
    "log_min_duration_statement=-1",
    "-c",
    "log_connections=off",
    "-c",
    "log_disconnections=off",
    "-c",
    "log_lock_waits=off",
];

fn postgres_ephemeral_tuning_args() -> impl Iterator<Item = &'static str> {
    POSTGRES_EPHEMERAL_TUNING_ARGS.iter().copied()
}

static SHARED_POSTGRES: OnceCell<Arc<SharedPostgresServer>> = OnceCell::const_new();
static TEST_DATABASE_COUNTER: AtomicU64 = AtomicU64::new(1);
static TEMPLATE_CLONE_SEMAPHORE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

struct SharedPostgresServer {
    // Intentionally held but never dropped: the shared container survives
    // until the next test run's orphan cleanup removes it.  Using
    // ManuallyDrop prevents the Drop impl from calling `docker rm` when
    // any single nextest worker process exits while others are still running.
    //
    // Workers after the first one may attach to the already-created named
    // Docker container via `docker port`, so they do not have a testcontainers
    // handle.
    _container: Option<std::mem::ManuallyDrop<ContainerAsync<Postgres>>>,
    // Dedicated runtime for the admin pool.  Individual `#[tokio::test]`
    // runtimes are created and destroyed per-test; if the pool is created
    // on one of those, its IO driver dies when the test finishes, causing
    // subsequent tests to see "A Tokio 1.x context was found, but it is
    // being shutdown."  Keeping a dedicated runtime alive prevents this.
    _pool_runtime: tokio::runtime::Runtime,
    host: String,
    port: u16,
    admin_pool: PgPool,
    template_database: String,
    _run_lock: ProcessLock,
}

/// Database lease backed by a shared PostgreSQL test container.
///
/// The underlying container is started once per test process and kept alive for
/// the duration of that process. Each lease gets its own isolated database
/// cloned from a pre-migrated template database.
pub struct TestContainer {
    shared: Arc<SharedPostgresServer>,
    database_name: String,
    cleaned_up: bool,
}

/// Isolated PostgreSQL test database with an open pool.
///
/// The `container` field owns cleanup for the leased database. Keep the
/// fixture alive for at least as long as code uses `pool`.
pub struct TestDatabase {
    pub container: TestContainer,
    pub pool: PgPool,
}

impl TestDatabase {
    pub async fn cleanup(self) {
        self.pool.close().await;
        self.container.cleanup().await;
    }
}

impl SharedPostgresServer {
    fn connect_options(&self, database_name: &str) -> PgConnectOptions {
        PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .username("synctv")
            .password("synctv_test")
            .database(database_name)
            .ssl_mode(PgSslMode::Disable)
    }

    async fn drop_database(&self, database_name: &str) {
        if database_name == self.template_database || database_name == ADMIN_DATABASE {
            return;
        }

        let sql = format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            quote_identifier(database_name)
        );

        if let Err(err) = sqlx::query(&sql).execute(&self.admin_pool).await {
            eprintln!("warning: failed to drop postgres test database {database_name}: {err}");
        }
    }
}

impl TestContainer {
    const fn new(shared: Arc<SharedPostgresServer>, database_name: String) -> Self {
        Self {
            shared,
            database_name,
            cleaned_up: false,
        }
    }

    #[must_use]
    pub fn database_name(&self) -> &str {
        &self.database_name
    }

    pub async fn cleanup(mut self) {
        if !self.cleaned_up {
            self.shared.drop_database(&self.database_name).await;
            self.cleaned_up = true;
        }
    }

    pub async fn recreate_as_empty_database(&self) {
        if self.database_name == self.shared.template_database
            || self.database_name == ADMIN_DATABASE
        {
            return;
        }

        let database = quote_identifier(&self.database_name);
        let drop_sql = format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)");
        sqlx::query(&drop_sql)
            .execute(&self.shared.admin_pool)
            .await
            .expect("test database should be dropped before empty recreation");

        let create_sql = format!("CREATE DATABASE {database}");
        sqlx::query(&create_sql)
            .execute(&self.shared.admin_pool)
            .await
            .expect("test database should be recreated empty");
    }

    pub fn host(&self) -> String {
        self.shared.host.clone()
    }

    pub fn port_ipv4(&self, _internal_port: u16) -> u16 {
        self.shared.port
    }

    pub fn host_port(&self, _internal_port: u16) -> (String, u16) {
        (self.shared.host.clone(), self.shared.port)
    }
}

impl Drop for TestContainer {
    fn drop(&mut self) {
        if self.cleaned_up {
            return;
        }

        self.cleaned_up = true;
        let shared = Arc::clone(&self.shared);
        let database_name = self.database_name.clone();
        spawn_best_effort_database_cleanup(shared, database_name);
    }
}

/// Returns the timeout budget used for Docker-backed integration tests.
///
/// The default is intentionally higher than 30 seconds because workspace-scale
/// `cargo nextest -j20` runs can cold-pull images or contend on Docker daemon
/// resources, making a 30s cap spuriously fail healthy tests.
#[must_use]
pub fn docker_startup_timeout() -> Duration {
    docker_startup_timeout_from(std::env::var(DOCKER_STARTUP_TIMEOUT_ENV).ok().as_deref())
}

#[must_use]
pub fn docker_startup_parallelism() -> usize {
    docker_startup_parallelism_from(
        std::env::var(DOCKER_STARTUP_PARALLELISM_ENV)
            .ok()
            .as_deref(),
    )
}

#[must_use]
fn docker_startup_timeout_from(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(MIN_DOCKER_STARTUP_TIMEOUT_SECS))
        .map_or_else(
            || Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

#[must_use]
fn docker_startup_parallelism_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_DOCKER_STARTUP_PARALLELISM, |slots| {
            slots.max(MIN_DOCKER_STARTUP_PARALLELISM)
        })
}

fn shared_admin_pool_max_connections() -> u32 {
    shared_admin_pool_max_connections_from(
        std::env::var(SHARED_ADMIN_POOL_MAX_CONNECTIONS_ENV)
            .ok()
            .as_deref(),
    )
}

fn shared_admin_pool_max_connections_from(value: Option<&str>) -> u32 {
    value
        .and_then(|value| value.parse::<u32>().ok())
        .map_or(DEFAULT_SHARED_ADMIN_POOL_MAX_CONNECTIONS, |connections| {
            connections.max(MIN_SHARED_ADMIN_POOL_MAX_CONNECTIONS)
        })
}

fn template_clone_parallelism() -> usize {
    template_clone_parallelism_from(
        std::env::var(TEMPLATE_CLONE_PARALLELISM_ENV)
            .ok()
            .as_deref(),
    )
}

fn template_clone_parallelism_from(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map_or(DEFAULT_TEMPLATE_CLONE_PARALLELISM, |slots| {
            slots.max(MIN_TEMPLATE_CLONE_PARALLELISM)
        })
}

fn template_clone_semaphore() -> Arc<tokio::sync::Semaphore> {
    Arc::clone(
        TEMPLATE_CLONE_SEMAPHORE
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(template_clone_parallelism()))),
    )
}

fn default_test_pool_max_connections() -> u32 {
    default_test_pool_max_connections_from(
        std::env::var(TEST_POOL_MAX_CONNECTIONS_ENV).ok().as_deref(),
    )
}

fn default_test_pool_max_connections_from(value: Option<&str>) -> u32 {
    value
        .and_then(|value| value.parse::<u32>().ok())
        .map_or(DEFAULT_TEST_POOL_MAX_CONNECTIONS, |connections| {
            connections.max(MIN_TEST_POOL_MAX_CONNECTIONS)
        })
}

fn sanitize_database_component(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    while name.ends_with('_') {
        name.pop();
    }

    if name.is_empty() {
        "db".to_string()
    } else {
        name
    }
}

fn current_test_label() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .map_or_else(
            || "unknown-test".to_string(),
            |value| sanitize_container_name(&value, "postgres-test"),
        )
}

fn current_test_run_id() -> String {
    docker_current_test_run_id("postgres-test")
}

fn current_test_run_id_from(run_id: Option<&str>) -> String {
    docker_current_test_run_id_from(run_id, "postgres-test")
}

fn postgres_ready_conditions() -> Vec<WaitFor> {
    // The official postgres image emits "database system is ready to accept
    // connections" twice on first boot: once for a transient init server and
    // once after the final post-init restart. Waiting for the second occurrence
    // avoids racing the final server startup without the overhead of Docker
    // healthchecks on every container.
    vec![WaitFor::log(
        LogWaitStrategy::stdout_or_stderr("database system is ready to accept connections")
            .with_times(2),
    )]
}

fn named_postgres_request(
    db_name: &str,
    container_name: &str,
) -> testcontainers::ContainerRequest<Postgres> {
    let run_id = current_test_run_id();
    Postgres::default()
        .with_db_name(db_name)
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .with_container_name(container_name.to_string())
        .with_label(TEST_RUN_LABEL, run_id)
        .with_reuse(ReuseDirective::Always)
        .with_cmd(postgres_ephemeral_tuning_args())
        .with_ready_conditions(postgres_ready_conditions())
}

pub fn postgres_connection_url_with_credentials(
    host: &str,
    port: u16,
    db_name: &str,
    username: &str,
    password: &str,
) -> String {
    let mut url = Url::parse(&format!("postgresql://{}:{port}", format_socket_host(host)))
        .expect("postgres connection URL base should be valid");
    url.set_username(username)
        .expect("postgres username should be encoded into URL");
    url.set_password(Some(password))
        .expect("postgres password should be encoded into URL");
    url.set_path(&format!("/{db_name}"));
    url.to_string()
}

fn postgres_connection_url(host: &str, port: u16, db_name: &str) -> String {
    postgres_connection_url_with_credentials(host, port, db_name, "synctv", "synctv_test")
}

fn format_socket_host(host: &str) -> String {
    if matches!(host_address_family(host), Some(IpAddr::V6(_))) && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

async fn resolve_host_port(
    container: &ContainerAsync<Postgres>,
    internal_port: u16,
    db_name: &str,
) -> (String, u16) {
    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut retries = 0u32;
    let mut last_error: Option<String> = None;

    while std::time::Instant::now() < deadline {
        let host = match container.get_host().await {
            Ok(host) => host.to_string(),
            Err(err) => {
                last_error = Some(format!("failed to get Postgres host: {err}"));
                retries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
        };

        let ports = match container.ports().await {
            Ok(ports) => ports,
            Err(err) => {
                last_error = Some(format!("failed to inspect Postgres port mappings: {err}"));
                retries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
        };

        let endpoints = candidate_endpoints_for_host(
            &host,
            ports.map_to_host_port_ipv4(internal_port),
            ports.map_to_host_port_ipv6(internal_port),
        );

        if endpoints.is_empty() {
            last_error = Some(format!(
                "failed to resolve Postgres endpoint for host {host}: ipv4={:?}, ipv6={:?}",
                ports.map_to_host_port_ipv4(internal_port),
                ports.map_to_host_port_ipv6(internal_port)
            ));
            retries += 1;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            continue;
        }

        for (candidate_host, candidate_port) in &endpoints {
            let connect_options = PgConnectOptions::new()
                .host(candidate_host)
                .port(*candidate_port)
                .username("synctv")
                .password("synctv_test")
                .database(db_name)
                .ssl_mode(PgSslMode::Disable);

            match sqlx::postgres::PgConnection::connect_with(&connect_options).await {
                Ok(mut conn) => {
                    sqlx::query_scalar!("SELECT 1 AS \"one!\"")
                        .fetch_one(&mut conn)
                        .await
                        .expect("PostgreSQL readiness probe should succeed once connected");
                    drop(conn);
                    return (candidate_host.clone(), *candidate_port);
                }
                Err(err) => last_error = Some(err.to_string()),
            }
        }

        retries += 1;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    panic!(
        "PostgreSQL not ready within {:?} after {retries} retries across endpoints {:?}: {}",
        docker_startup_timeout(),
        "dynamic",
        last_error.unwrap_or_else(|| "connection attempts did not yield an error".to_string())
    );
}

async fn resolve_existing_named_postgres_endpoint(
    container_name: &str,
    db_name: &str,
) -> Option<(String, u16)> {
    if !docker_named_container_belongs_to_current_run(container_name, &current_test_run_id()) {
        return None;
    }

    let deadline = std::time::Instant::now() + docker_startup_timeout();
    let mut last_error = String::from("docker port has not returned a Postgres endpoint yet");

    while std::time::Instant::now() < deadline {
        if let Some(candidates) = docker_port_candidates(container_name, 5432) {
            for (host, port) in &candidates {
                let connect_options = PgConnectOptions::new()
                    .host(host)
                    .port(*port)
                    .username("synctv")
                    .password("synctv_test")
                    .database(db_name)
                    .ssl_mode(PgSslMode::Disable);

                match sqlx::postgres::PgConnection::connect_with(&connect_options).await {
                    Ok(mut conn) => {
                        let probe = sqlx::query_scalar!("SELECT 1 AS \"one!\"")
                            .fetch_one(&mut conn)
                            .await;
                        if probe.is_ok() {
                            return Some((host.clone(), *port));
                        }
                        last_error = format!("readiness probe failed: {probe:?}");
                    }
                    Err(err) => {
                        last_error = format!("connect failed for {host}:{port}: {err}");
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    panic!(
        "Existing Postgres container {container_name} did not become reachable within {:?}: {last_error}",
        docker_startup_timeout()
    );
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn shared_container_name() -> String {
    shared_container_name_from(std::env::var("NEXTEST_RUN_ID").ok().as_deref())
}

fn shared_container_name_from(run_id: Option<&str>) -> String {
    format!("synctv-pg-shared-{}", current_test_run_id_from(run_id))
}

fn template_database_name() -> String {
    template_database_name_from(std::env::var("NEXTEST_RUN_ID").ok().as_deref())
}

fn template_database_name_from(run_id: Option<&str>) -> String {
    let raw = format!(
        "{TEMPLATE_DATABASE_PREFIX}_{}",
        current_test_run_id_from(run_id)
    );
    truncate_database_identifier(&sanitize_database_component(&raw))
}

fn truncate_database_identifier(value: &str) -> String {
    value.chars().take(MAX_DATABASE_IDENTIFIER_LEN).collect()
}

fn build_test_database_name(requested_db_name: &str, label: &str) -> String {
    let base = sanitize_database_component(requested_db_name);
    let label = sanitize_database_component(label);
    let test_label = sanitize_database_component(&current_test_label());
    let counter = TEST_DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let suffix = format!(
        "{}_{}",
        counter,
        synctv_common::snanoid!(DATABASE_NAME_RANDOM_LEN).to_lowercase()
    );
    let prefix = format!(
        "{}_{}_{}_",
        sanitize_database_component(TEST_DATABASE_PREFIX),
        base,
        label
    );

    let available = MAX_DATABASE_IDENTIFIER_LEN.saturating_sub(suffix.len() + 1);
    let mut stem = format!("{prefix}{test_label}");
    if stem.len() > available {
        stem.truncate(available);
    }
    while stem.ends_with('_') {
        stem.pop();
    }
    if stem.is_empty() {
        stem.push_str(TEST_DATABASE_PREFIX);
    }

    format!("{stem}_{suffix}")
}

#[cfg(test)]
async fn database_exists(admin_pool: &PgPool, database_name: &str) -> bool {
    sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1) AS \"exists!\"",
        database_name,
    )
    .fetch_one(admin_pool)
    .await
    .expect("database existence query should succeed")
}

async fn recreate_template_database(
    admin_pool: &PgPool,
    template_database: &str,
    connect_options: PgConnectOptions,
) {
    // Fast path: if the template database already exists and is marked as a
    // template, skip the expensive DROP + CREATE + migrate cycle.  The first
    // nextest worker process creates the template; all subsequent workers in
    // the same run reuse it (they share the same NEXTEST_RUN_ID).
    let already_exists: bool = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1 AND datistemplate = true) AS \"exists!\"",
        template_database,
    )
    .fetch_one(admin_pool)
    .await
    .expect("template database existence check should succeed");

    if already_exists {
        return;
    }

    let untemplate_sql = format!(
        "ALTER DATABASE {} WITH IS_TEMPLATE false",
        quote_identifier(template_database)
    );
    let _ = sqlx::query(&untemplate_sql).execute(admin_pool).await;

    let drop_sql = format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        quote_identifier(template_database)
    );
    sqlx::query(&drop_sql)
        .execute(admin_pool)
        .await
        .expect("template database cleanup should succeed");

    let create_sql = format!("CREATE DATABASE {}", quote_identifier(template_database));
    sqlx::query(&create_sql)
        .execute(admin_pool)
        .await
        .expect("template database creation should succeed");

    let template_pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(30))
        .max_connections(2)
        .connect_with(connect_options.clone().database(template_database))
        .await
        .expect("template database pool creation should succeed");

    sqlx::migrate!("../../migrations")
        .run(&template_pool)
        .await
        .expect("template database migrations should succeed");

    template_pool.close().await;

    let no_connections_sql = format!(
        "ALTER DATABASE {} WITH ALLOW_CONNECTIONS false",
        quote_identifier(template_database)
    );
    sqlx::query(&no_connections_sql)
        .execute(admin_pool)
        .await
        .expect("template database should disallow new connections");

    let terminate_sql = format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{template_database}' AND pid <> pg_backend_pid()"
    );
    sqlx::query(&terminate_sql)
        .execute(admin_pool)
        .await
        .expect("template database should terminate lingering sessions");

    let mark_template_sql = format!(
        "ALTER DATABASE {} WITH IS_TEMPLATE true",
        quote_identifier(template_database)
    );
    sqlx::query(&mark_template_sql)
        .execute(admin_pool)
        .await
        .expect("template database should be marked reusable");
}

async fn init_shared_postgres_server() -> SharedPostgresServer {
    let run_id = current_test_run_id();
    let run_lock = acquire_run_lock("postgres", &run_id);
    cleanup_orphaned_testcontainers("synctv-pg-", "postgres", &run_id);
    cleanup_orphaned_run_lock_files("synctv-postgres-run-");
    cleanup_orphaned_run_lock_files("synctv-postgres-startup-");

    // Serialize first creation per nextest run so concurrent worker processes
    // observe the same reusable container instead of each creating their own.
    let lock_name = format!("postgres-startup-{run_id}");
    let _startup_lock = tokio::task::spawn_blocking(move || loop {
        if let Some(lock) = ProcessLock::try_acquire(&lock_name) {
            return lock;
        }
        std::thread::sleep(Duration::from_millis(100));
    })
    .await
    .expect("postgres startup lock task should not panic");

    let container_name = shared_container_name();
    let (postgres, host, port) = if let Some((host, port)) =
        resolve_existing_named_postgres_endpoint(&container_name, ADMIN_DATABASE).await
    {
        (None, host, port)
    } else {
        let start_deadline = std::time::Instant::now() + docker_startup_timeout();
        loop {
            match tokio::time::timeout(
                docker_startup_timeout(),
                named_postgres_request(ADMIN_DATABASE, &container_name).start(),
            )
            .await
            {
                Ok(Ok(container)) => {
                    let (host, port) = resolve_host_port(&container, 5432, ADMIN_DATABASE).await;
                    break (Some(container), host, port);
                }
                Ok(Err(err)) => {
                    let err_string = err.to_string();
                    if startup_error_is_named_container_conflict(&err_string) {
                        if let Some((host, port)) = resolve_existing_named_postgres_endpoint(
                            &container_name,
                            ADMIN_DATABASE,
                        )
                        .await
                        {
                            break (None, host, port);
                        }
                    }
                    if startup_error_is_retriable(&err_string)
                        && std::time::Instant::now() < start_deadline
                    {
                        eprintln!(
                            "warning: transient Postgres container startup error for {container_name}, retrying: {err_string}"
                        );
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                    panic!("Failed to start Postgres container: {err}");
                }
                Err(elapsed) => {
                    panic!(
                        "Docker container startup timed out after {:?}: {elapsed} (is Docker running?)",
                        docker_startup_timeout(),
                    );
                }
            }
        }
    };
    let admin_connect_options = PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username("synctv")
        .password("synctv_test")
        .database(ADMIN_DATABASE)
        .ssl_mode(PgSslMode::Disable);

    // Create the admin pool on a dedicated runtime that outlives any
    // individual test's tokio runtime.  `spawn_blocking` runs on the
    // tokio blocking thread pool so we don't block the async runtime.
    let template_database = template_database_name();
    let template_db = template_database.clone();
    let (admin_pool, pool_runtime) = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("dedicated pool runtime should build");
        let admin_pool_max_connections = shared_admin_pool_max_connections();

        let pool = rt.block_on(async {
            PgPoolOptions::new()
                .acquire_timeout(Duration::from_secs(30))
                .max_connections(admin_pool_max_connections)
                .connect_with(admin_connect_options.clone())
                .await
                .expect("shared postgres admin pool creation should succeed")
        });

        rt.block_on(async {
            recreate_template_database(&pool, &template_db, admin_connect_options).await;
        });

        (pool, rt)
    })
    .await
    .expect("pool initialization thread should not panic");

    SharedPostgresServer {
        _container: postgres.map(std::mem::ManuallyDrop::new),
        _pool_runtime: pool_runtime,
        host,
        port,
        admin_pool,
        template_database,
        _run_lock: run_lock,
    }
}

async fn shared_postgres_server() -> Arc<SharedPostgresServer> {
    Arc::clone(
        SHARED_POSTGRES
            .get_or_init(|| async { Arc::new(init_shared_postgres_server().await) })
            .await,
    )
}

async fn provision_test_database(requested_db_name: &str, label: &str) -> TestContainer {
    let shared = shared_postgres_server().await;
    let database_name = build_test_database_name(requested_db_name, label);
    let create_sql = format!(
        "CREATE DATABASE {} TEMPLATE {}",
        quote_identifier(&database_name),
        quote_identifier(&shared.template_database)
    );

    // PostgreSQL serializes template cloning more than a normal query path,
    // so an unconstrained burst of full-stack tests can exhaust the shared
    // admin pool while waiting on CREATE DATABASE ... TEMPLATE locks.
    let clone_permit = template_clone_semaphore()
        .acquire_owned()
        .await
        .expect("template clone semaphore should stay open");

    let mut admin_connection = PgConnection::connect_with(&shared.connect_options(ADMIN_DATABASE))
        .await
        .expect("direct postgres admin connection for template clone should succeed");

    sqlx::query(&create_sql)
        .execute(&mut admin_connection)
        .await
        .expect("test database creation from template should succeed");
    drop(clone_permit);

    TestContainer::new(shared, database_name)
}

fn spawn_best_effort_database_cleanup(shared: Arc<SharedPostgresServer>, database_name: String) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            shared.drop_database(&database_name).await;
        });
        return;
    }

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("drop-time postgres cleanup runtime should build");
        runtime.block_on(async move {
            shared.drop_database(&database_name).await;
        });
    });
}

/// Creates a `PostgreSQL` test container and connection pool
///
/// This function:
/// 1. Starts a shared `PostgreSQL` Docker container once per test process
/// 2. Creates an isolated database cloned from a migrated template
/// 3. Creates a connection pool to that per-test database
///
/// # Returns
///
/// A tuple of (database lease, pool). The lease keeps cleanup ownership for
/// the isolated database while the shared container stays alive for other
/// tests in the same process.
pub async fn create_test_pool() -> (TestContainer, PgPool) {
    create_test_pool_with_db_and_label("synctv_test", "pool").await
}

pub async fn create_test_pool_with_options_and_label(
    db_name: &str,
    label: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> (TestContainer, PgPool) {
    let container = provision_test_database(db_name, label).await;
    let connect_options = container.shared.connect_options(container.database_name());

    let pool_result = PgPoolOptions::new()
        .acquire_timeout(acquire_timeout)
        .max_connections(max_connections)
        .connect_with(connect_options)
        .await;

    let pool = match pool_result {
        Ok(pool) => pool,
        Err(err) => {
            container.cleanup().await;
            panic!("PostgreSQL pool creation should succeed after template clone: {err}");
        }
    };

    (container, pool)
}

pub async fn create_test_database_with_options_and_label(
    db_name: &str,
    label: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> TestDatabase {
    let (container, pool) =
        create_test_pool_with_options_and_label(db_name, label, max_connections, acquire_timeout)
            .await;
    TestDatabase { container, pool }
}

pub async fn create_test_pool_with_db_and_label(
    db_name: &str,
    label: &str,
) -> (TestContainer, PgPool) {
    create_test_pool_with_options_and_label(
        db_name,
        label,
        default_test_pool_max_connections(),
        Duration::from_secs(30),
    )
    .await
}

/// Creates a `PostgreSQL` test pool with a custom database name
pub async fn create_test_pool_with_db(db_name: &str) -> (TestContainer, PgPool) {
    create_test_pool_with_db_and_label(db_name, db_name).await
}

pub async fn create_test_database_with_db_and_label(db_name: &str, label: &str) -> TestDatabase {
    create_test_database_with_options_and_label(
        db_name,
        label,
        default_test_pool_max_connections(),
        Duration::from_secs(30),
    )
    .await
}

pub async fn create_test_database() -> TestDatabase {
    create_test_database_with_db_and_label("synctv_test", "database").await
}

pub async fn connect_test_pool_url(database_url: &str) -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(30))
        .max_connections(default_test_pool_max_connections())
        .connect(database_url)
        .await
        .expect("test should connect to PostgreSQL database URL")
}

/// Starts a shared `PostgreSQL` test container and returns a connection URL
/// for a per-test cloned database without creating a pool.
pub async fn create_test_database_url_with_label(
    db_name: &str,
    label: &str,
) -> (TestContainer, String) {
    let container = provision_test_database(db_name, label).await;
    let database_url = postgres_connection_url(
        &container.shared.host,
        container.shared.port,
        container.database_name(),
    );

    (container, database_url)
}

#[cfg(test)]
mod tests {
    use crate::docker::{
        docker_port_line_candidates, docker_rm_force_with_program, lock_file_path,
        run_has_active_lock,
    };

    use super::*;

    #[test]
    fn named_postgres_request_waits_for_second_ready_log() {
        let request = named_postgres_request("postgres", "synctv-pg-test");
        let ready_conditions = request.ready_conditions();

        assert_eq!(
            ready_conditions.len(),
            1,
            "postgres test container should have a single explicit readiness condition"
        );
        assert!(
            matches!(ready_conditions.as_slice(), [WaitFor::Log(_)]),
            "postgres test container should wait for the second ready log instead of the first init-server ready log"
        );
    }

    #[test]
    fn named_postgres_request_uses_ephemeral_pg18_tuning() {
        let request = named_postgres_request("postgres", "synctv-pg-test");
        let cmd: Vec<_> = request.cmd().map(std::borrow::Cow::into_owned).collect();

        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "wal_level=minimal"]),
            "ephemeral test postgres should minimize WAL volume: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "max_wal_senders=0"]),
            "wal_level=minimal requires wal senders to be disabled: {cmd:?}"
        );
        assert!(
            cmd.windows(2).any(|pair| pair == ["-c", "autovacuum=off"]),
            "ephemeral test postgres should disable autovacuum background churn: {cmd:?}"
        );
        assert!(
            cmd.windows(2).any(|pair| pair == ["-c", "jit=off"]),
            "jit compilation overhead should stay disabled for short-lived test workloads: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "max_parallel_workers_per_gather=0"]),
            "parallel query workers add avoidable overhead for small test workloads: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "max_connections=1024"]),
            "test postgres should not over-allocate connection slots: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "superuser_reserved_connections=0"]),
            "ephemeral test postgres should expose all connection slots to test clients: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "log_statement=none"]),
            "test postgres should disable statement logging overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "log_duration=off"]),
            "test postgres should disable query duration logging overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "log_min_duration_statement=-1"]),
            "test postgres should disable slow query logging overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "log_connections=off"]),
            "test postgres should disable connection logging overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "log_disconnections=off"]),
            "test postgres should disable disconnection logging overhead: {cmd:?}"
        );
        assert!(
            cmd.windows(2)
                .any(|pair| pair == ["-c", "log_lock_waits=off"]),
            "test postgres should disable lock-wait logging overhead: {cmd:?}"
        );
    }

    #[test]
    fn pool_defaults_match_high_concurrency_nextest_profile() {
        assert_eq!(shared_admin_pool_max_connections_from(None), 64);
        assert_eq!(default_test_pool_max_connections_from(None), 32);
    }

    #[test]
    fn test_docker_startup_timeout_honors_valid_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("180")),
            Duration::from_mins(3)
        );
    }

    #[test]
    fn test_docker_startup_timeout_rejects_too_small_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("5")),
            Duration::from_secs(MIN_DOCKER_STARTUP_TIMEOUT_SECS)
        );
    }

    #[test]
    fn test_docker_startup_timeout_ignores_invalid_override() {
        assert_eq!(
            docker_startup_timeout_from(Some("not-a-number")),
            Duration::from_secs(DEFAULT_DOCKER_STARTUP_TIMEOUT_SECS)
        );
    }

    #[test]
    fn test_docker_startup_parallelism_honors_valid_override() {
        assert_eq!(docker_startup_parallelism_from(Some("6")), 6);
    }

    #[test]
    fn test_docker_startup_parallelism_rejects_zero_override() {
        assert_eq!(
            docker_startup_parallelism_from(Some("0")),
            MIN_DOCKER_STARTUP_PARALLELISM
        );
    }

    #[test]
    fn test_shared_admin_pool_max_connections_honors_valid_override() {
        assert_eq!(shared_admin_pool_max_connections_from(Some("24")), 24);
    }

    #[test]
    fn test_shared_admin_pool_max_connections_rejects_too_small_override() {
        assert_eq!(
            shared_admin_pool_max_connections_from(Some("1")),
            MIN_SHARED_ADMIN_POOL_MAX_CONNECTIONS
        );
    }

    #[test]
    fn test_template_clone_parallelism_honors_valid_override() {
        assert_eq!(template_clone_parallelism_from(Some("6")), 6);
    }

    #[test]
    fn test_template_clone_parallelism_rejects_zero_override() {
        assert_eq!(
            template_clone_parallelism_from(Some("0")),
            MIN_TEMPLATE_CLONE_PARALLELISM
        );
    }

    #[test]
    fn test_default_test_pool_max_connections_honors_valid_override() {
        assert_eq!(default_test_pool_max_connections_from(Some("12")), 12);
    }

    #[test]
    fn test_default_test_pool_max_connections_rejects_zero_override() {
        assert_eq!(
            default_test_pool_max_connections_from(Some("0")),
            MIN_TEST_POOL_MAX_CONNECTIONS
        );
    }

    #[test]
    fn build_test_database_name_produces_unique_names() {
        let left = build_test_database_name("synctv_test", "pool");
        let right = build_test_database_name("synctv_test", "pool");

        assert_ne!(left, right);
    }

    #[test]
    fn build_test_database_name_preserves_requested_prefix() {
        let name = build_test_database_name("auth_service", "rate-limiter");

        assert!(
            name.starts_with("synctv_test_auth_service_rate_limiter"),
            "database name should preserve requested base and label for debugging: {name}"
        );
    }

    #[test]
    fn build_test_database_name_stays_within_postgres_identifier_limit() {
        let name = build_test_database_name(
            "this-is-a-very-long-database-name-that-would-otherwise-overflow",
            "this-is-a-very-long-label-that-should-also-be-truncated",
        );

        assert!(
            name.len() <= MAX_DATABASE_IDENTIFIER_LEN,
            "database name should fit postgres identifier limit: {name}"
        );
    }

    #[test]
    fn shared_container_name_uses_nextest_run_id_when_present() {
        let name = shared_container_name_from(Some("Run.Id/42"));

        assert_eq!(name, "synctv-pg-shared-run-id-42");
    }

    #[test]
    fn template_database_name_uses_nextest_run_id_when_present() {
        let name = template_database_name_from(Some("Run.Id/42"));

        assert!(
            name.starts_with("synctv_template_run_id_42"),
            "template database should be namespaced by nextest run id: {name}"
        );
    }

    #[test]
    fn run_has_active_lock_detects_live_run_lock() {
        let run_id = format!("test-run-{}", synctv_common::snanoid!(8).to_lowercase());
        let lock = acquire_run_lock("postgres", &run_id);

        assert!(
            run_has_active_lock("postgres", &run_id),
            "active run lock must prevent orphan cleanup from treating the run as dead"
        );

        drop(lock);

        assert!(
            !run_has_active_lock("postgres", &run_id),
            "released run lock must no longer mark the run as active"
        );
    }

    #[test]
    fn lock_file_path_uses_platform_temp_directory() {
        let path = lock_file_path("postgres-startup-test");

        assert!(
            path.starts_with(crate::test_temp_dir()),
            "postgres lock files should live under the platform temp directory: {path:?}"
        );
    }

    #[test]
    fn open_lock_file_creates_missing_parent_directories() {
        let nested_dir = crate::test_temp_dir().join(format!(
            "synctv-postgres-lock-test-{}",
            synctv_common::snanoid!(8).to_lowercase()
        ));
        let path = nested_dir.join("nested").join("postgres.lock");

        let file = ProcessLock::open_lock_file(&path);
        drop(file);

        assert!(
            path.exists(),
            "opening a postgres lock file should create missing parent directories: {path:?}"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(nested_dir);
    }

    #[test]
    fn docker_rm_force_reports_command_failure() {
        let err = docker_rm_force_with_program("false", "synctv-pg-test")
            .expect_err("failed command must surface as an error");

        assert!(
            err.contains("command `false rm -v -f synctv-pg-test` exited with status"),
            "error should include the failing command line: {err}"
        );
    }

    #[test]
    fn docker_rm_force_reports_spawn_failure() {
        let err =
            docker_rm_force_with_program("synctv-command-that-should-not-exist", "synctv-pg-test")
                .expect_err("spawn failure must surface as an error");

        assert!(
            err.contains("failed to spawn `synctv-command-that-should-not-exist`"),
            "error should include the missing program: {err}"
        );
    }

    #[test]
    fn startup_error_is_retriable_for_known_container_lifecycle_races() {
        assert!(startup_error_is_retriable("No such container: abc123"));
        assert!(startup_error_is_retriable(
            "DockerResponseServerError { status_code: 404, message: \"No such container\" }"
        ));
        assert!(startup_error_is_retriable(
            "container is marked for removal"
        ));
        assert!(startup_error_is_retriable(
            "DockerResponseServerError { status_code: 409, message: \"container is marked for removal\" }"
        ));
        assert!(!startup_error_is_retriable(
            "404 gateway from registry mirror"
        ));
        assert!(!startup_error_is_retriable("409 conflict during start"));
        assert!(!startup_error_is_retriable("authentication failed"));
    }

    #[test]
    fn startup_error_detects_named_container_conflicts_only() {
        assert!(startup_error_is_named_container_conflict(
            "Docker responded with status code 409: Conflict. The container name \"/synctv-pg-shared-run\" is already in use by container \"abc123\""
        ));
        assert!(startup_error_is_named_container_conflict(
            "Conflict. The container name is already in use"
        ));
        assert!(!startup_error_is_named_container_conflict(
            "409 conflict during start"
        ));
        assert!(!startup_error_is_named_container_conflict(
            "authentication failed"
        ));
    }

    #[test]
    fn docker_port_line_candidates_handles_wildcard_bindings() {
        let ipv4_candidates = docker_port_line_candidates("0.0.0.0:33811");
        assert!(
            ipv4_candidates.contains(&("127.0.0.1".to_string(), 33811)),
            "wildcard IPv4 binding should include loopback: {ipv4_candidates:?}"
        );
        let ipv6_candidates = docker_port_line_candidates("[::]:33811");
        assert!(
            ipv6_candidates.contains(&("::1".to_string(), 33811))
                && ipv6_candidates.contains(&("127.0.0.1".to_string(), 33811)),
            "wildcard IPv6 binding should include IPv6 and IPv4 loopback: {ipv6_candidates:?}"
        );
        assert_eq!(docker_port_line_candidates("invalid"), Vec::new());
    }

    #[test]
    fn postgres_connection_url_uses_resolved_host() {
        let url = postgres_connection_url("docker.internal", 5432, "synctv_test");

        assert_eq!(
            url,
            "postgresql://synctv:synctv_test@docker.internal:5432/synctv_test"
        );
        assert!(
            !url.contains("@127.0.0.1:"),
            "connection URL must not hardcode localhost"
        );
    }

    #[test]
    fn postgres_connection_url_brackets_ipv6_literals() {
        let url = postgres_connection_url("::1", 5432, "synctv_test");

        assert_eq!(
            url,
            "postgresql://synctv:synctv_test@[::1]:5432/synctv_test"
        );
    }

    #[test]
    fn postgres_connection_url_with_credentials_brackets_ipv6_literals() {
        let url = postgres_connection_url_with_credentials(
            "::1",
            5432,
            "synctv_test",
            "readonly",
            "secret",
        );

        assert_eq!(url, "postgresql://readonly:secret@[::1]:5432/synctv_test");
    }

    #[test]
    fn postgres_connection_url_with_credentials_percent_encodes_reserved_characters() {
        let url = postgres_connection_url_with_credentials(
            "127.0.0.1",
            5432,
            "synctv_test",
            "read@only",
            "sec/re?t#value",
        );
        let parsed = Url::parse(&url).expect("connection URL should remain parseable");

        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(5432));
        assert_eq!(parsed.path(), "/synctv_test");
        assert!(
            url.contains("read%40only") && url.contains("sec%2Fre%3Ft%23value"),
            "reserved credentials should be percent-encoded in URL: {url}"
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv4_port_for_domain_hosts() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", Some(5432), Some(15433)),
            vec![
                ("docker.internal".to_string(), 5432),
                ("docker.internal".to_string(), 15433)
            ]
        );
    }

    #[test]
    fn resolve_host_port_keeps_reported_host_for_ipv4_domain_mappings() {
        assert_eq!(
            candidate_endpoints_for_host("10.0.0.8", Some(5432), None),
            vec![("10.0.0.8".to_string(), 5432)]
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_port_for_ipv6_hosts() {
        let candidates = candidate_endpoints_for_host("[::1]", Some(5432), Some(15433));

        assert!(
            candidates.contains(&("[::1]".to_string(), 15433)),
            "IPv6 host candidates should preserve IPv6 endpoint: {candidates:?}"
        );
        assert!(
            candidates.contains(&("127.0.0.1".to_string(), 5432)),
            "IPv6 host candidates should include IPv4 loopback fallback: {candidates:?}"
        );
    }

    #[test]
    fn resolve_host_port_uses_ipv6_when_domain_only_has_ipv6_mapping() {
        assert_eq!(
            candidate_endpoints_for_host("docker.internal", None, Some(15433)),
            vec![("docker.internal".to_string(), 15433)]
        );
    }

    #[test]
    fn resolve_host_port_rewrites_localhost_to_ipv6_literal_when_needed() {
        let candidates = candidate_endpoints_for_host("localhost", Some(5432), Some(15433));

        assert!(
            candidates.contains(&("::1".to_string(), 15433))
                && candidates.contains(&("127.0.0.1".to_string(), 5432)),
            "localhost candidates should include IPv6 and IPv4 loopback: {candidates:?}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn create_test_database_url_reuses_shared_container_and_isolates_databases() {
        let (db_one, url_one) =
            create_test_database_url_with_label("synctv_test", "shared-a").await;
        let (db_two, url_two) =
            create_test_database_url_with_label("synctv_test", "shared-b").await;

        let shared = shared_postgres_server().await;
        assert_eq!(
            db_one.host_port(5432),
            db_two.host_port(5432),
            "leases in the same process should reuse a single postgres container"
        );
        assert_ne!(
            db_one.database_name(),
            db_two.database_name(),
            "leases should point at isolated cloned databases"
        );
        assert!(
            database_exists(&shared.admin_pool, db_one.database_name()).await,
            "first cloned database should exist"
        );
        assert!(
            database_exists(&shared.admin_pool, db_two.database_name()).await,
            "second cloned database should exist"
        );
        assert!(
            url_one.contains(db_one.database_name()),
            "database URL should target the cloned database: {url_one}"
        );
        assert!(
            url_two.contains(db_two.database_name()),
            "database URL should target the cloned database: {url_two}"
        );

        db_one.cleanup().await;
        db_two.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn cleanup_drops_cloned_database() {
        let (db, _url) = create_test_database_url_with_label("synctv_test", "cleanup").await;
        let shared = shared_postgres_server().await;
        let database_name = db.database_name().to_string();

        assert!(
            database_exists(&shared.admin_pool, &database_name).await,
            "cloned database should exist before cleanup"
        );

        db.cleanup().await;

        assert!(
            !database_exists(&shared.admin_pool, &database_name).await,
            "cleanup should drop the cloned database"
        );
    }
}
