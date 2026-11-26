//! Inclusive HTTP ranges and half-open offsets into a cached slice.
#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub enum ClientRangePlan {
    Explicit { start: u64, end: u64 },
    OpenEnded { start: u64 },
    Suffix { suffix_len: u64 },
    MultiRange,
}

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum BoundsError {
    BeyondTotal,
    EmptySuffix,
    Reversed,
    MultiRange,
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_ok() == match plan {
        ClientRangePlan::Explicit { start, end } => start <= end && start < total,
        ClientRangePlan::OpenEnded { start } => start < total,
        ClientRangePlan::Suffix { suffix_len } => total > 0 && suffix_len > 0,
        ClientRangePlan::MultiRange => false,
    },
    result matches Ok((first, last)) ==> (first <= last < total && match plan {
        ClientRangePlan::Explicit { start, end } => first == start && last == if end < total { end as int } else { total - 1 },
        ClientRangePlan::OpenEnded { start } => first == start && last + 1 == total,
        ClientRangePlan::Suffix { suffix_len } => last + 1 == total && total - first == if suffix_len < total { suffix_len } else { total },
        ClientRangePlan::MultiRange => false,
    }),
))]
pub(super) fn resolve(plan: ClientRangePlan, total: u64) -> Result<(u64, u64), BoundsError> {
    match plan {
        ClientRangePlan::MultiRange => Err(BoundsError::MultiRange),
        _ if total == 0 => Err(BoundsError::BeyondTotal),
        ClientRangePlan::Explicit { start, end } => {
            if start >= total {
                Err(BoundsError::BeyondTotal)
            } else if start > end {
                Err(BoundsError::Reversed)
            } else {
                Ok((start, end.min(total - 1)))
            }
        }
        ClientRangePlan::OpenEnded { start } => {
            if start >= total {
                Err(BoundsError::BeyondTotal)
            } else {
                Ok((start, total - 1))
            }
        }
        ClientRangePlan::Suffix { suffix_len } => {
            if suffix_len == 0 {
                Err(BoundsError::EmptySuffix)
            } else {
                Ok((total.saturating_sub(suffix_len), total - 1))
            }
        }
    }
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_some() == (first <= last && first < base + len && base <= last && len > 0),
    result matches Some((lo, hi)) ==> (
        lo < hi <= len &&
        base + lo == if first > base { first as int } else { base as int } &&
        base + hi == if last + 1 < base + len { last + 1 } else { base + len }),
))]
pub(super) fn crop_offsets(base: u64, len: usize, first: u64, last: u64) -> Option<(usize, usize)> {
    if first > last || last < base || len == 0 {
        return None;
    }
    let lo = first.saturating_sub(base);
    if lo >= len as u64 {
        return None;
    }
    let last_offset = (last - base).min((len - 1) as u64);
    Some((
        usize::try_from(lo).ok()?,
        usize::try_from(last_offset).ok()? + 1,
    ))
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_some() == (start <= last && last < u64::MAX && (total.is_none() || last < total.unwrap())),
    result matches Some(end) ==> (end == last + 1 && start < end),
))]
pub(super) fn exclusive_end(start: u64, last: u64, total: Option<u64>) -> Option<u64> {
    if start > last || matches!(total, Some(size) if last >= size) {
        return None;
    }
    last.checked_add(1)
}

#[cfg(verus_only)]
verus! {
    // Scan the maximal ASCII decimal prefix with an unbounded mathematical accumulator.
    pub(super) closed spec fn decimal(bytes: Seq<u8>, i: int, value: int) -> Option<(u64, usize)>
        decreases bytes.len() - i
    {
        if value > u64::MAX { None }
        else if i >= bytes.len() || !(48 <= bytes[i] <= 57) {
            if i == 0 { None } else { Some((value as u64, i as usize)) }
        } else { decimal(bytes, i + 1, value * 10 + bytes[i] - 48) }
    }
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result == decimal(bytes@, 0, 0),
    result matches Some((value, n)) ==> (0 < n <= bytes.len() &&
        (forall|k: int| 0 <= k < n ==> 48 <= #[trigger] bytes@[k] <= 57)),
))]
pub(super) fn decimal_prefix(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut i = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            i <= bytes.len(),
            decimal(bytes@, 0, 0) == decimal(bytes@, i as int, value as int),
            forall|k: int| 0 <= k < i ==> 48 <= #[trigger] bytes@[k] <= 57,
        decreases bytes.len() - i,
    ))]
    #[allow(
        clippy::manual_is_ascii_check,
        reason = "Verus models the range predicate used by the decimal invariant"
    )]
    while i < bytes.len() && (b'0'..=b'9').contains(&bytes[i]) {
        #[cfg(verus_only)]
        proof! { reveal_with_fuel(decimal, 2); }
        let byte = bytes[i];
        let digit = u64::from(byte - b'0');
        value = value.checked_mul(10)?.checked_add(digit)?;
        i += 1;
    }
    if i == 0 { None } else { Some((value, i)) }
}
