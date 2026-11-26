//! Timers for retrying leadership acquisition.
#[cfg(verus_only)]
use vstd::prelude::*;

/// Grace period multiplier after a prolonged Redis outage.
#[cfg_attr(verus_only, verus_verify)]
pub(super) const OUTAGE_RECOVERY_GRACE_MULTIPLIER: u64 = 3;

#[cfg_attr(verus_only, verus_spec(grace => ensures
    grace == if prolonged {
        vstd::math::min(interval * OUTAGE_RECOVERY_GRACE_MULTIPLIER, u64::MAX as int)
    } else { interval as int },
))]
pub(super) fn redis_grace(interval: u64, prolonged: bool) -> u64 {
    if prolonged {
        interval.saturating_mul(OUTAGE_RECOVERY_GRACE_MULTIPLIER)
    } else {
        interval
    }
}

/// Allow for clock skew between nodes when checking the lease.
#[cfg(any(feature = "k8s", verus_only))]
#[cfg_attr(verus_only, verus_verify)]
pub(super) const CLOCK_DRIFT_TOLERANCE_SECS: i64 = 5;

#[cfg(any(feature = "k8s", verus_only))]
#[cfg_attr(verus_only, verus_spec(expired => ensures expired == match renewed {
    None => true,
    Some(start) => now as int > start as int + duration + CLOCK_DRIFT_TOLERANCE_SECS,
}))]
pub(super) fn lease_expired(renewed: Option<i64>, now: i64, duration: i32) -> bool {
    let Some(start) = renewed else { return true };
    i128::from(now) - i128::from(start)
        > i128::from(duration) + i128::from(CLOCK_DRIFT_TOLERANCE_SECS)
}

#[cfg(any(feature = "k8s", verus_only))]
#[cfg_attr(verus_only, verus_spec(grace => ensures
    grace <= cap,
    grace == vstd::math::min(cap as int, base * (1u64 << (if losses <= 1 { 0u64 }
        else if losses >= 7 { 6u64 } else { (losses - 1) as u64 }))),
))]
pub(super) fn loss_backoff(base: u64, cap: u64, losses: u64) -> u64 {
    let exponent = losses.saturating_sub(1).min(6);
    base.saturating_mul(1u64 << exponent).min(cap)
}
