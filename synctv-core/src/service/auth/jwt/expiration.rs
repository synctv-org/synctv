//! Expiration policy on authenticated NumericDate values.
#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum ExpirationPolicy {
    Required,
    Optional,
}

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum ExpirationStatus {
    Valid,
    Expired,
    Missing,
}

#[cfg_attr(verus_only, verus_spec(status => ensures
    (status is Valid) == match exp {
        Some(end) => now as int <= end as int + leeway,
        None => policy is Optional,
    },
    (status is Expired) == (exp matches Some(end) && now as int > end as int + leeway),
    (status is Missing) == (exp.is_none() && policy is Required),
))]
pub(super) fn check(
    now: i64,
    exp: Option<i64>,
    leeway: u64,
    policy: ExpirationPolicy,
) -> ExpirationStatus {
    match exp {
        Some(end) if i128::from(now) > i128::from(end) + i128::from(leeway) => {
            ExpirationStatus::Expired
        }
        None if matches!(policy, ExpirationPolicy::Required) => ExpirationStatus::Missing,
        _ => ExpirationStatus::Valid,
    }
}

/// Store revocations through the last accepted integer second, including clock leeway.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (exp as int + leeway + 1 - now <= u64::MAX),
    out matches Some(ttl) ==> ttl == vstd::math::max(0, exp as int + leeway + 1 - now),
))]
pub(super) fn retention(exp: i64, now: i64, leeway: u64) -> Option<u64> {
    let remaining = i128::from(exp) + i128::from(leeway) + 1 - i128::from(now);
    u64::try_from(remaining.max(0)).ok()
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out == match revoked { Some(t) => issued <= t, None => false },
))]
pub(super) fn revoked_before(issued: i64, revoked: Option<i64>) -> bool {
    matches!(revoked, Some(t) if issued <= t)
}

/// Keep the existing one-hour margin and cover longer configured clock leeway.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (duration as int + vstd::math::max(3600, leeway as int + 1) <= u64::MAX),
    out matches Some(n) ==> n == duration as int + vstd::math::max(3600, leeway as int + 1),
))]
pub(super) fn family_retention(duration: u64, leeway: u64) -> Option<u64> {
    let margin = leeway.checked_add(1)?;
    duration.checked_add(margin.max(3600))
}
