use std::{collections::HashMap, sync::Mutex};

use super::*;

pub static LOGGING_DROPPED_LINES_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "logging_dropped_lines_total",
            "Total log lines dropped by a full non-blocking queue",
            &["component"],
        )
    });

static LAST_OBSERVED: std::sync::LazyLock<Mutex<HashMap<String, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn sync_dropped_lines(samples: &[(String, usize)]) {
    let mut observed = LAST_OBSERVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (component, current) in samples {
        let previous = observed.entry(component.clone()).or_default();
        let delta = current.saturating_sub(*previous);
        let counter = LOGGING_DROPPED_LINES_TOTAL.with_label_values(&[component]);
        if delta > 0 {
            counter.inc_by(u64::try_from(delta).unwrap_or(u64::MAX));
        }
        *previous = *current;
    }
}
