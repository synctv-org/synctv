#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_spec(valid => ensures valid == (now as int <= created as int + ttl)))]
pub(super) const fn within_ttl(created: u64, now: u64, ttl: u64) -> bool {
    now.saturating_sub(created) <= ttl
}

#[cfg_attr(verus_only, verus_spec(claim => ensures claim == (matches_snapshot && now as int <= created as int + ttl)))]
pub(super) const fn may_consume(matches_snapshot: bool, created: u64, now: u64, ttl: u64) -> bool {
    matches_snapshot && within_ttl(created, now, ttl)
}
