use super::*;

pub static TASK_PANICS_TOTAL: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    int_counter_vec(
        "spawned_task_panics_total",
        "Total number of spawned task panics caught by spawn_monitored",
        &["task_name"],
    )
});
