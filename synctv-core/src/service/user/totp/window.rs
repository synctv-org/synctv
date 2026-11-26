#[cfg(verus_only)]
use vstd::prelude::*;

/// Try the current 30-second counter first, then allow one step of clock skew in either direction.
#[cfg_attr(verus_only, verus_spec(steps => ensures
    steps[0] as int == (if timestamp < 0 { 0 } else { timestamp as int / 30 }),
    steps[1] as int == steps[0] as int - 1, steps[2] as int == steps[0] as int + 1,
))]
pub(super) fn candidates(timestamp: i64) -> [i64; 3] {
    let current = timestamp.max(0) / 30;
    [current, current - 1, current + 1]
}

#[cfg_attr(verus_only, verus_spec(ok => ensures ok == (step >= 0 &&
    match last_used { None => true, Some(last) => step > last })))]
pub(super) fn unused(step: i64, last_used: Option<i64>) -> bool {
    step >= 0
        && match last_used {
            None => true,
            Some(last) => step > last,
        }
}
