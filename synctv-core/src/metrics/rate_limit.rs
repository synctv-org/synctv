use super::*;

pub static RATE_LIMIT_REDIS_FALLBACKS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "rate_limit_redis_fallbacks_total",
            "Total Redis errors that triggered in-memory rate limit fallback",
            &["category"],
        )
    });
