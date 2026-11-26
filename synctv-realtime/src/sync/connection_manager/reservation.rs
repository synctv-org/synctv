//! Admission of one pending connection against a snapshot of registered slots.
#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_ok() == (registered + pending < limit),
    result matches Ok(next) ==> (next == pending + 1 && registered + next <= limit),
    result matches Err(used) ==> (used == if registered + pending <= usize::MAX {
        registered + pending
    } else { usize::MAX as int }),
))]
pub(super) fn reserve(registered: usize, pending: usize, limit: usize) -> Result<usize, usize> {
    let used = registered.saturating_add(pending);
    if used >= limit {
        Err(used)
    } else {
        Ok(pending + 1)
    }
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_some() == (pending > 0),
    result matches Some(next) ==> next + 1 == pending,
))]
pub(super) fn release(pending: usize) -> Option<usize> {
    pending.checked_sub(1)
}
