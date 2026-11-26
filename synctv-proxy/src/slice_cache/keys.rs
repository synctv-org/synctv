use sha2::{Digest, Sha256};

use crate::ProviderHeaders;

fn hash_resource(url: &str, provider_headers: &ProviderHeaders) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hasher.update(b"\0");

    let mut sorted: Vec<(&String, &String)> = provider_headers.iter().collect();
    sorted.sort_by_key(|(key, _)| *key);
    for (key, value) in sorted {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }

    hasher
}

/// Group a resource's slices so validator changes only visit resident entries.
/// Legacy opaque slice keys age out through the normal eviction path.
pub(super) fn slice_cache_key(resource: &str, slice_index: u64) -> String {
    format!("{resource}-{slice_index:x}")
}

pub(super) fn resource_meta_key(url: &str, provider_headers: &ProviderHeaders) -> String {
    let mut hasher = hash_resource(url, provider_headers);
    hasher.update(b"\0meta");
    hex::encode(hasher.finalize())
}

pub(super) fn full_response_keys(
    url: &str,
    provider_headers: &ProviderHeaders,
) -> (String, String) {
    let mut body = hash_resource(url, provider_headers);
    let mut metadata = body.clone();
    body.update(b"\0full-response");
    metadata.update(b"\0full-response-meta");
    (
        hex::encode(body.finalize()),
        hex::encode(metadata.finalize()),
    )
}
