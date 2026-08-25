use super::*;

pub static FILE_OBJECT_DELETE_ATTEMPTS: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_file_object_delete_attempts_total",
            "Total file object delete attempts",
            &["origin", "backend"],
        )
    });

pub static FILE_OBJECT_DELETE_FAILURES: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_file_object_delete_failures_total",
            "Total file object delete failures",
            &["origin", "backend"],
        )
    });

pub static FILE_CLEANUP_JOBS_DUE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "synctv_file_cleanup_jobs_due",
        "File cleanup jobs due for retry",
    )
});

pub static FILE_CLEANUP_JOBS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_file_cleanup_jobs_total",
            "Total file cleanup retry job actions",
            &["action", "origin", "backend"],
        )
    });
