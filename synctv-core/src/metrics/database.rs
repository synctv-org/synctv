use super::*;

pub static DB_CONNECTIONS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "db_connections_active",
        "Current number of active database connections",
    )
});

pub static DB_POOL_UTILIZATION: std::sync::LazyLock<GaugeVec> = std::sync::LazyLock::new(|| {
    gauge_vec(
        "db_pool_utilization_ratio",
        "Database connection pool utilization ratio (active/max)",
        &["pool"],
    )
});

pub static DB_POOL_SIZE_MAX: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "db_pool_size_max",
        "Maximum number of connections in the pool",
    )
});

pub static DB_CONNECTIONS_IDLE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "db_connections_idle",
        "Number of idle connections in the pool",
    )
});
