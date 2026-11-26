#[cfg(verus_only)]
use vstd::prelude::*;

/// Advance over one database part clipped to the requested half-open interval.
#[cfg_attr(verus_only, verus_spec(next => ensures next.is_some() == (
    0 <= offset && length > 0 && start <= cursor < end &&
    vstd::math::max(start as int, offset as int) == cursor &&
    cursor < vstd::math::min(end as int, offset as int + length)
), next matches Some(n) ==> (cursor < n <= end &&
    n as int == vstd::math::min(end as int, offset as int + length))))]
pub(super) fn advance_part(
    start: u64,
    end: u64,
    cursor: u64,
    offset: i64,
    length: i64,
) -> Option<u64> {
    let offset = u64::try_from(offset).ok()?;
    let length = u64::try_from(length).ok()?;
    let clipped_end = end.min(offset + length);
    if start.max(offset) != cursor || clipped_end <= cursor {
        return None;
    }
    Some(clipped_end)
}

/// A nonempty read must fit the request, the object and the caller's output buffer.
#[cfg_attr(verus_only, verus_spec(out => ensures out.is_some() == (0 <= position <= size &&
    0 < buffered <= max_chunk && position as int + buffered <= size),
    out matches Some((take, next)) ==> (take as int == vstd::math::min(capacity as int, buffered as int) &&
        next as int == position as int + take && position <= next <= size),
))]
pub(super) fn consume(
    position: i64,
    size: i64,
    buffered: usize,
    max_chunk: usize,
    capacity: usize,
) -> Option<(usize, i64)> {
    if position < 0
        || position > size
        || buffered == 0
        || buffered > max_chunk
        || i64::try_from(buffered).ok()? > size - position
    {
        return None;
    }
    let take = buffered.min(capacity);
    Some((take, position + i64::try_from(take).ok()?))
}
