#[cfg(verus_only)]
use vstd::prelude::*;

/// Four magic bytes followed by a four-byte header length.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (8 + header as int + body <= u64::MAX),
    out matches Some(n) ==> n == 8 + header as int + body,
))]
pub(super) fn file_size(header: u64, body: u64) -> Option<u64> {
    8u64.checked_add(header)?.checked_add(body)
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out == vstd::math::min(u64::MAX as int, inserted as int + ttl as int * 1000 + grace_millis),
))]
pub(crate) const fn cache_entry_deadline_millis(inserted: u64, ttl: u64, grace_millis: u64) -> u64 {
    inserted
        .saturating_add(ttl.saturating_mul(1000))
        .saturating_add(grace_millis)
}
