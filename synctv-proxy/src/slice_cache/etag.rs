//! ETag consistency validation for cached resources, plus the `StoredEntry`
//! type used by cache backends.
//!
//! Mirrors nginx's slice filter ETag checking: the first slice's ETag
//! establishes the expected value; subsequent slices must match or the
//! entire resource is invalidated.

use std::time::{Duration, SystemTime};

use bytes::Bytes;

/// Per-resource metadata stored alongside slice data to enable ETag
/// consistency checking across slices.
#[derive(Clone, Debug)]
pub struct CachedResourceMeta {
    /// ETag returned by the upstream for this resource.
    pub etag: Option<String>,
    /// Last-Modified header returned by the upstream.
    pub last_modified: Option<String>,
    /// Total size of the resource as reported by upstream.
    pub total_size: Option<u64>,
    /// Content-Type of the resource.
    pub content_type: Option<String>,
    /// When this metadata was last accessed. Used by `cleanup_stale_meta`
    /// to evict least-recently-accessed entries first.
    pub last_accessed: SystemTime,
}

/// Data stored in any cache backend. Uses `SystemTime` (not `Instant`) so that
/// entries can be serialized for file-based persistence.
#[derive(Clone, Debug)]
pub struct StoredEntry {
    /// The cached bytes.
    pub data: Bytes,
    /// When this entry was inserted into the cache.
    pub inserted_at: SystemTime,
    /// How long this entry is considered fresh.
    pub ttl: Duration,
    /// When this entry was last accessed (read).
    pub last_accessed: SystemTime,
}

impl StoredEntry {
    /// Create a new entry with the given data and TTL.
    /// Both `inserted_at` and `last_accessed` are set to `SystemTime::now()`.
    #[must_use]
    pub fn new(data: Bytes, ttl: Duration) -> Self {
        let now = SystemTime::now();
        Self {
            data,
            inserted_at: now,
            ttl,
            last_accessed: now,
        }
    }

    /// Returns `true` if the entry's TTL has elapsed.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        SystemTime::now()
            .duration_since(self.inserted_at)
            .unwrap_or(Duration::ZERO)
            > self.ttl
    }

    /// Returns `true` if the entry is expired but still within the stale
    /// serving window (i.e., `elapsed > ttl && elapsed <= ttl + stale_max_age`).
    #[must_use]
    pub fn is_stale(&self, stale_max_age: Duration) -> bool {
        let elapsed = SystemTime::now()
            .duration_since(self.inserted_at)
            .unwrap_or(Duration::ZERO);
        elapsed > self.ttl && elapsed <= self.ttl + stale_max_age
    }

    /// Update `last_accessed` to the current time.
    pub fn touch(&mut self) {
        self.last_accessed = SystemTime::now();
    }

    /// Returns the size of the stored data in bytes.
    #[must_use]
    pub const fn data_size(&self) -> u64 {
        self.data.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // CachedResourceMeta tests
    // ---------------------------------------------------------------

    #[test]
    fn cached_resource_meta_has_last_modified_field() {
        let meta = CachedResourceMeta {
            etag: Some("\"abc\"".to_string()),
            last_modified: Some("Tue, 01 Jan 2030 00:00:00 GMT".to_string()),
            total_size: Some(1024),
            content_type: Some("video/mp4".to_string()),
            last_accessed: SystemTime::now(),
        };
        assert_eq!(
            meta.last_modified.as_deref(),
            Some("Tue, 01 Jan 2030 00:00:00 GMT")
        );
    }

    // ---------------------------------------------------------------
    // StoredEntry tests
    // ---------------------------------------------------------------

    #[test]
    fn new_entry_is_not_expired() {
        let entry = StoredEntry::new(Bytes::from("hello"), Duration::from_mins(1));
        assert!(!entry.is_expired());
    }

    #[test]
    fn expired_entry_is_detected() {
        let entry = StoredEntry {
            data: Bytes::from("data"),
            inserted_at: SystemTime::now() - Duration::from_mins(2),
            ttl: Duration::from_mins(1),
            last_accessed: SystemTime::now(),
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn zero_ttl_entry_is_immediately_expired() {
        // A zero-TTL entry should be expired as soon as any time passes.
        let entry = StoredEntry {
            data: Bytes::from("x"),
            inserted_at: SystemTime::now() - Duration::from_nanos(1),
            ttl: Duration::ZERO,
            last_accessed: SystemTime::now(),
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn stale_within_window() {
        // Entry expired 30s ago, stale window is 60s -> stale.
        let entry = StoredEntry {
            data: Bytes::from("data"),
            inserted_at: SystemTime::now() - Duration::from_secs(90),
            ttl: Duration::from_mins(1),
            last_accessed: SystemTime::now(),
        };
        assert!(entry.is_expired());
        assert!(entry.is_stale(Duration::from_mins(1)));
    }

    #[test]
    fn stale_beyond_window() {
        // Entry expired 120s ago, stale window is 60s -> NOT stale (too old).
        let entry = StoredEntry {
            data: Bytes::from("data"),
            inserted_at: SystemTime::now() - Duration::from_mins(3),
            ttl: Duration::from_mins(1),
            last_accessed: SystemTime::now(),
        };
        assert!(entry.is_expired());
        assert!(!entry.is_stale(Duration::from_mins(1)));
    }

    #[test]
    fn fresh_entry_is_not_stale() {
        let entry = StoredEntry::new(Bytes::from("fresh"), Duration::from_mins(5));
        assert!(!entry.is_expired());
        assert!(!entry.is_stale(Duration::from_mins(1)));
    }

    #[test]
    fn touch_updates_last_accessed() {
        let mut entry = StoredEntry {
            data: Bytes::from("data"),
            inserted_at: SystemTime::now() - Duration::from_secs(100),
            ttl: Duration::from_mins(5),
            last_accessed: SystemTime::now() - Duration::from_secs(100),
        };
        let before_touch = entry.last_accessed;
        // Small sleep to ensure time difference (SystemTime granularity).
        std::thread::sleep(Duration::from_millis(2));
        entry.touch();
        assert!(entry.last_accessed > before_touch);
    }

    #[test]
    fn data_size_matches_bytes_len() {
        let data = Bytes::from(vec![0u8; 1024]);
        let entry = StoredEntry::new(data, Duration::from_mins(1));
        assert_eq!(entry.data_size(), 1024);
    }

    #[test]
    fn data_size_empty() {
        let entry = StoredEntry::new(Bytes::new(), Duration::from_mins(1));
        assert_eq!(entry.data_size(), 0);
    }

    #[test]
    fn inserted_at_and_last_accessed_start_equal() {
        let entry = StoredEntry::new(Bytes::from("test"), Duration::from_secs(10));
        assert_eq!(entry.inserted_at, entry.last_accessed);
    }
}
