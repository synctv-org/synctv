//! Admission before an RTMP chunk read or payload allocation.
#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub(super) const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum BudgetError {
    InvalidLength,
    TooLarge(usize),
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_ok() == (
        declared <= MAX_MESSAGE_SIZE && buffered <= declared && (chunk > 0 || buffered == declared)
        && other + buffered + vstd::math::min(declared - buffered, chunk as int) <= MAX_MESSAGE_SIZE),
    result matches Ok(read) ==> (
        read == vstd::math::min(declared - buffered, chunk as int)
        && buffered + read <= declared && other + buffered + read <= MAX_MESSAGE_SIZE
        && (read == 0) == (buffered == declared)),
))]
pub(super) fn next_read(
    declared: usize,
    buffered: usize,
    chunk: usize,
    other: usize,
) -> Result<usize, BudgetError> {
    if declared > MAX_MESSAGE_SIZE {
        return Err(BudgetError::TooLarge(declared));
    }
    if buffered > declared || (chunk == 0 && buffered < declared) {
        return Err(BudgetError::InvalidLength);
    }
    let read = (declared - buffered).min(chunk);
    let total = other.saturating_add(buffered + read);
    if total > MAX_MESSAGE_SIZE {
        Err(BudgetError::TooLarge(total))
    } else {
        Ok(read)
    }
}
