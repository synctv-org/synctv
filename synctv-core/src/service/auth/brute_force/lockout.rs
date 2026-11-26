//! Threshold selection and retry timing shared by login lockout checks.
#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_spec(result => ensures
    result == if attempts >= high.0 { Some(high.1) }
        else if attempts >= middle.0 { Some(middle.1) }
        else if attempts >= low.0 { Some(low.1) } else { None },
))]
pub(super) const fn duration(
    attempts: u64,
    low: (u64, u64),
    middle: (u64, u64),
    high: (u64, u64),
) -> Option<u64> {
    if attempts >= high.0 {
        Some(high.1)
    } else if attempts >= middle.0 {
        Some(middle.1)
    } else if attempts >= low.0 {
        Some(low.1)
    } else {
        None
    }
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result == (if now <= last { duration as int }
        else if now as int - last >= duration { 0int }
        else { duration as int - (now as int - last) }),
))]
pub(super) fn remaining(now: i64, last: i64, duration: u64) -> u64 {
    if now <= last {
        return duration;
    }
    let elapsed = i128::from(now) - i128::from(last);
    // The difference of two ordered i64 values fits in u64.
    duration.saturating_sub(u64::try_from(elapsed).unwrap_or(u64::MAX))
}

#[cfg_attr(verus_only, verus_spec(next => ensures
    next >= previous,
    previous < u64::MAX ==> next == previous + 1,
))]
pub(super) fn next_count(previous: u64) -> u64 {
    previous.saturating_add(1)
}
