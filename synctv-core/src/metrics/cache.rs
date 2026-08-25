use super::*;

pub static CACHE_HITS: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    int_counter_vec(
        "cache_hits_total",
        "Total number of cache hits",
        &["cache_type", "level"],
    )
});

pub static CACHE_MISSES: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    int_counter_vec(
        "cache_misses_total",
        "Total number of cache misses",
        &["cache_type", "level"],
    )
});

pub static CACHE_EVICTIONS: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    int_counter_vec(
        "cache_evictions_total",
        "Total number of cache evictions",
        &["cache_type"],
    )
});

pub static CACHE_ERRORS: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    int_counter_vec(
        "cache_errors_total",
        "Total number of cache operation errors",
        &["cache_type", "operation"],
    )
});

pub static CACHE_INVALIDATIONS: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "cache_invalidations_total",
            "Total number of cache invalidations",
            &["cache_type"],
        )
    });

pub static CACHE_OPERATION_DURATION: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        histogram_vec(
            HistogramOpts::new(
                "cache_operation_duration_seconds",
                "Duration of cache operations in seconds",
            ),
            &["operation"],
        )
    });

pub static CACHE_LAG_FLUSH_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "cache_lag_flush_total",
            "Total L1 cache flushes triggered by broadcast channel lag",
            &["component"],
        )
    });

pub static CACHE_FENCE_OPERATIONS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "cache_fence_operations_total",
            "Total number of cache version-fence operations",
            &["domain", "operation", "result"],
        )
    });

pub static CACHE_DB_FALLBACK_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "cache_db_fallback_total",
            "Total number of strong cache reads that fell back to PostgreSQL",
            &["domain", "reason"],
        )
    });

pub static CACHE_STALE_WRITE_REJECT_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "cache_stale_write_reject_total",
            "Total number of stale version-aware cache writes rejected",
            &["cache_type", "level"],
        )
    });

pub static CACHE_FENCE_PENDING: std::sync::LazyLock<GaugeVec> = std::sync::LazyLock::new(|| {
    gauge_vec(
        "cache_fence_pending",
        "Whether a cache version fence domain currently has a pending write",
        &["domain"],
    )
});

pub static CACHE_FENCE_REPAIR_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "cache_fence_repair_total",
            "Total number of read-time cache fence repair outcomes",
            &["domain", "result"],
        )
    });

pub static CACHE_FENCE_DB_COMPARE: std::sync::LazyLock<GaugeVec> = std::sync::LazyLock::new(|| {
    gauge_vec(
        "cache_fence_db_compare",
        "Latest cache fence patrol comparison with PostgreSQL version (1 when observed)",
        &["domain", "relation"],
    )
});
