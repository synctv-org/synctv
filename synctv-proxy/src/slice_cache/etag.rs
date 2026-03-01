//! ETag consistency validation for cached resources.
//!
//! Mirrors nginx's slice filter ETag checking: the first slice's ETag
//! establishes the expected value; subsequent slices must match or the
//! entire resource is invalidated.

/// Per-resource metadata stored alongside slice data to enable ETag
/// consistency checking across slices.
#[derive(Clone, Debug)]
pub struct CachedResourceMeta {
    /// ETag returned by the upstream for this resource.
    pub etag: Option<String>,
    /// Total size of the resource as reported by upstream.
    pub total_size: Option<u64>,
    /// Content-Type of the resource.
    pub content_type: Option<String>,
}
