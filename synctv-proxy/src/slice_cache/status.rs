//! Cache status enum modeled after nginx's cache status defines.
//!
//! Maps to `ngx_http_cache.h` constants:
//! `NGX_HTTP_CACHE_MISS=1` through `NGX_HTTP_CACHE_HIT=7`.

use std::fmt;

/// Cache status modeled after nginx's cache status defines
/// (`ngx_http_cache.h`: `NGX_HTTP_CACHE_MISS=1` through `NGX_HTTP_CACHE_HIT=7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// The requested data was found in cache and served directly.
    Hit,
    /// The requested data was not found in cache and had to be fetched.
    Miss,
    /// The cached entry existed but its TTL had elapsed.
    Expired,
    /// The entry is expired but still within the stale-serve window.
    Stale,
    /// Another request is currently revalidating the entry; the stale
    /// version is being served in the meantime.
    Updating,
    /// The entry was successfully revalidated against the origin.
    Revalidated,
    /// Caching was bypassed entirely (e.g., cache disabled, too large).
    Bypass,
}

impl CacheStatus {
    /// Return the canonical upper-case string representation used in
    /// the `X-Cache-Status` HTTP header.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Hit => "HIT",
            Self::Miss => "MISS",
            Self::Expired => "EXPIRED",
            Self::Stale => "STALE",
            Self::Updating => "UPDATING",
            Self::Revalidated => "REVALIDATED",
            Self::Bypass => "BYPASS",
        }
    }
}

impl fmt::Display for CacheStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_returns_uppercase_header_values() {
        assert_eq!(CacheStatus::Hit.as_str(), "HIT");
        assert_eq!(CacheStatus::Miss.as_str(), "MISS");
        assert_eq!(CacheStatus::Expired.as_str(), "EXPIRED");
        assert_eq!(CacheStatus::Stale.as_str(), "STALE");
        assert_eq!(CacheStatus::Updating.as_str(), "UPDATING");
        assert_eq!(CacheStatus::Revalidated.as_str(), "REVALIDATED");
        assert_eq!(CacheStatus::Bypass.as_str(), "BYPASS");
    }

    #[test]
    fn display_matches_as_str() {
        let variants = [
            CacheStatus::Hit,
            CacheStatus::Miss,
            CacheStatus::Expired,
            CacheStatus::Stale,
            CacheStatus::Updating,
            CacheStatus::Revalidated,
            CacheStatus::Bypass,
        ];
        for v in &variants {
            assert_eq!(format!("{v}"), v.as_str());
        }
    }

    #[test]
    fn debug_includes_variant_name() {
        // Debug should produce something like "Hit", "Miss", etc.
        let dbg = format!("{:?}", CacheStatus::Hit);
        assert!(dbg.contains("Hit"), "Debug output was: {dbg}");
    }

    #[test]
    fn clone_and_copy_produce_equal_values() {
        let original = CacheStatus::Stale;
        let copied: CacheStatus = original;
        let copied2: CacheStatus = original;
        assert_eq!(original, copied);
        assert_eq!(original, copied2);
    }

    #[test]
    fn equality_works_across_variants() {
        assert_eq!(CacheStatus::Hit, CacheStatus::Hit);
        assert_ne!(CacheStatus::Hit, CacheStatus::Miss);
        assert_ne!(CacheStatus::Expired, CacheStatus::Stale);
        assert_ne!(CacheStatus::Updating, CacheStatus::Revalidated);
        assert_ne!(CacheStatus::Miss, CacheStatus::Bypass);
    }

    #[test]
    fn all_seven_variants_have_distinct_strings() {
        let strs: Vec<&str> = [
            CacheStatus::Hit,
            CacheStatus::Miss,
            CacheStatus::Expired,
            CacheStatus::Stale,
            CacheStatus::Updating,
            CacheStatus::Revalidated,
            CacheStatus::Bypass,
        ]
        .iter()
        .map(super::CacheStatus::as_str)
        .collect();

        // Check uniqueness.
        let mut deduped = strs.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            strs.len(),
            deduped.len(),
            "All status strings must be unique"
        );
    }

    #[test]
    fn display_can_be_used_in_header_context() {
        // Simulate inserting into an HTTP header value.
        let status = CacheStatus::Revalidated;
        let header_value = format!("X-Cache-Status: {status}");
        assert_eq!(header_value, "X-Cache-Status: REVALIDATED");
    }
}
