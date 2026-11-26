//! Lifetime rules for issuing and authenticating RTMP publish keys.
#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(verus_only),
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[cfg_attr(verus_only, verus_verify)]
pub enum PublishKeyType {
    SingleUse,
    Expiring,
    Permanent,
}

#[derive(Clone, Copy)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Phase {
    Issue,
    Authenticate,
}

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum LifetimeError {
    RequiresExpiration,
    UnexpectedExpiration,
    Expired,
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_ok() == match exp {
        None => kind is Permanent,
        Some(end) => !(kind is Permanent) && (now < end || (phase is Authenticate && now == end)),
    },
    (result matches Err(LifetimeError::RequiresExpiration)) == (exp.is_none() && !(kind is Permanent)),
    (result matches Err(LifetimeError::UnexpectedExpiration)) == (exp.is_some() && kind is Permanent),
    (result matches Err(LifetimeError::Expired)) == (!(kind is Permanent) && (exp matches Some(end) &&
        (now > end || (phase is Issue && now == end)))),
))]
pub(super) fn validate(
    kind: PublishKeyType,
    exp: Option<i64>,
    now: i64,
    phase: Phase,
) -> Result<(), LifetimeError> {
    match (kind, exp) {
        (PublishKeyType::Permanent, None) => Ok(()),
        (PublishKeyType::Permanent, Some(_)) => Err(LifetimeError::UnexpectedExpiration),
        (_, None) => Err(LifetimeError::RequiresExpiration),
        (_, Some(end)) if now > end || (now == end && matches!(phase, Phase::Issue)) => {
            Err(LifetimeError::Expired)
        }
        _ => Ok(()),
    }
}

#[cfg_attr(verus_only, verus_spec(ttl => ensures
    ttl == vstd::math::min(u64::MAX as int, vstd::math::max(0, end as int - now) + 300),
))]
pub(super) fn deduplication_ttl(end: i64, now: i64) -> u64 {
    let remaining = i128::from(end) - i128::from(now);
    u64::try_from(remaining).unwrap_or(0).saturating_add(300)
}
