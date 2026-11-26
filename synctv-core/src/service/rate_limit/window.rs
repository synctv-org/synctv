#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_spec(result => ensures
    oldest_score_millis == 0 ==> result == 1,
    oldest_score_millis != 0 ==> result == vstd::math::max(1,
        (vstd::math::max(0, vstd::math::min(window_seconds * 1000, u64::MAX as int)
            - vstd::math::max(0, now_millis as int - oldest_score_millis)) + 999) / 1000),
))]
pub(super) fn retry_after_seconds_from_oldest(
    now_millis: u64,
    oldest_score_millis: u64,
    window_seconds: u64,
) -> u64 {
    if oldest_score_millis == 0 {
        return 1;
    }

    let time_since_oldest = now_millis.saturating_sub(oldest_score_millis);
    let window_ms = window_seconds.saturating_mul(1000);
    let remaining_window = window_ms.saturating_sub(time_since_oldest);
    if remaining_window == 0 {
        1
    } else {
        (remaining_window - 1) / 1000 + 1
    }
}
