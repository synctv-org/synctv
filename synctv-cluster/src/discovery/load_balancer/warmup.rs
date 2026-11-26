//! Fallback score for a node that has not reported its connection count.

#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub(super) const WARMUP_PENALTY: usize = 1000;
#[cfg_attr(verus_only, verus_verify)]
const WARMUP_SECS: usize = 60;

#[cfg(verus_only)]
verus! {
    pub(super) closed spec fn score(age: int) -> int {
        if age <= 0 { 1000 } else if age >= 60 { 0 } else { 1000 * (60 - age) / 60 }
    }

    proof fn penalty_only_decreases(first: int, later: int)
        requires first <= later,
        ensures score(first) >= score(later),
    {}
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result == score(now as int - registered as int),
))]
pub(super) fn warmup_score(registered: i64, now: i64) -> usize {
    let age = i128::from(now) - i128::from(registered);
    if age <= 0 {
        WARMUP_PENALTY
    } else if age >= WARMUP_SECS as i128 {
        0
    } else {
        WARMUP_PENALTY * (WARMUP_SECS - usize::try_from(age).unwrap_or(WARMUP_SECS)) / WARMUP_SECS
    }
}
