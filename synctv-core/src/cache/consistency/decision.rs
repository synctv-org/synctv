//! Decisions over a fence snapshot taken under the store's lock.
#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub enum FenceRepairDecision {
    KeepPending,
    FinalizePending,
    ExpirePending,
    AdvanceCommitted,
    ResetCommitted,
    Noop,
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    (result is FinalizePending) == (pending matches Some(v) && v <= db),
    (result is ExpirePending) == (pending matches Some(v) && v > db && expired),
    (result is KeepPending) == (pending matches Some(v) && v > db && !expired),
    (result is AdvanceCommitted) == (pending.is_none() && committed < db),
    (result is ResetCommitted) == (pending.is_none() && committed > db),
    (result is Noop) == (pending.is_none() && committed == db),
))]
pub(super) fn repair_decision(
    committed: i64,
    pending: Option<i64>,
    db: i64,
    expired: bool,
) -> FenceRepairDecision {
    if let Some(version) = pending {
        if version <= db {
            return FenceRepairDecision::FinalizePending;
        }
        if expired {
            return FenceRepairDecision::ExpirePending;
        }
        return FenceRepairDecision::KeepPending;
    }
    match committed.cmp(&db) {
        std::cmp::Ordering::Less => FenceRepairDecision::AdvanceCommitted,
        std::cmp::Ordering::Greater => FenceRepairDecision::ResetCommitted,
        std::cmp::Ordering::Equal => FenceRepairDecision::Noop,
    }
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result == (started matches Some(start) && now as int - start >= lease_ms as int),
))]
pub(super) fn lease_expired(started: Option<i64>, now: i64, lease_ms: u128) -> bool {
    let Some(start) = started else { return false };
    let elapsed = i128::from(now) - i128::from(start);
    #[allow(clippy::cast_sign_loss)] // The sign check precedes the conversion.
    {
        elapsed >= 0 && elapsed as u128 >= lease_ms
    }
}

#[derive(Debug)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum ReservationError {
    Conflict,
    Exhausted,
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_ok() == (current <= observed && (pending.is_none() || pending == Some(observed))
        && observed < i64::MAX),
    result matches Ok(next) ==> (next == observed + 1 && next > current),
    result matches Err(ReservationError::Conflict) ==>
        (current > observed || (pending.is_some() && pending != Some(observed))),
    result matches Err(ReservationError::Exhausted) ==> observed == i64::MAX,
))]
pub(super) fn next_version(
    current: i64,
    pending: Option<i64>,
    observed: i64,
) -> Result<i64, ReservationError> {
    if current > observed || matches!(pending, Some(version) if version != observed) {
        return Err(ReservationError::Conflict);
    }
    observed.checked_add(1).ok_or(ReservationError::Exhausted)
}
