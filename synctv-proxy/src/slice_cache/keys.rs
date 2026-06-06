use std::collections::HashMap;

use sha2::{Digest, Sha256};

fn hash_resource(url: &str, provider_headers: &HashMap<String, String>) -> Sha256 {
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

pub(super) fn slice_cache_key(
    url: &str,
    provider_headers: &HashMap<String, String>,
    slice_index: u64,
) -> String {
    let mut hasher = hash_resource(url, provider_headers);
    hasher.update(b"\0");
    hasher.update(slice_index.to_le_bytes());
    hex::encode(hasher.finalize())
}

pub(super) fn resource_meta_key(url: &str, provider_headers: &HashMap<String, String>) -> String {
    let mut hasher = hash_resource(url, provider_headers);
    hasher.update(b"\0meta");
    hex::encode(hasher.finalize())
}
