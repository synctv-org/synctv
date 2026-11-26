//! Overflow-safe arithmetic for resumable upload plans.

#[cfg(verus_only)]
use vstd::prelude::*;

/// Return the number of fixed-size parts needed to cover a positive payload.
#[cfg_attr(verus_only, verus_spec(result => requires size_bytes > 0, part_size_bytes > 0, ensures
    result == (size_bytes as int - 1) / part_size_bytes as int + 1,
    1 <= result <= size_bytes,
    (result as int - 1) * part_size_bytes < size_bytes <= result as int * part_size_bytes,
))]
pub(super) fn upload_part_count(size_bytes: i64, part_size_bytes: i64) -> i64 {
    #[cfg(verus_only)]
    proof! {
        let n = size_bytes as int - 1;
        let d = part_size_bytes as int;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(n, d);
        assert((n / d + 1) * d == (n / d) * d + d) by (nonlinear_arith);
    }
    (size_bytes - 1) / part_size_bytes + 1
}

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum RangeError {
    Bounds,
    Payload,
    Alignment,
    Width,
    Index,
}

/// Validate against the part width captured when the upload session was opened.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (0 <= start <= end < total && total == expected && width > 0 &&
        end as int - start + 1 == payload && start as int % width as int == 0 &&
        end as int + 1 == vstd::math::min(start as int + width, total as int) &&
        start as int / width as int <= i32::MAX),
    out matches Ok(index) ==> index as int == start as int / width as int,
))]
pub(super) fn upload_index(
    start: i64,
    end: i64,
    total: i64,
    expected: i64,
    width: i64,
    payload: usize,
) -> Result<i32, RangeError> {
    if start < 0 || end < start || total != expected || end >= total || width <= 0 {
        return Err(RangeError::Bounds);
    }
    let length = end - start + 1;
    if usize::try_from(length).ok() != Some(payload) {
        return Err(RangeError::Payload);
    }
    if start % width != 0 {
        return Err(RangeError::Alignment);
    }
    if length != width.min(total - start) {
        return Err(RangeError::Width);
    }
    match i32::try_from(start / width) {
        Ok(index) => Ok(index),
        Err(_) => Err(RangeError::Index),
    }
}
