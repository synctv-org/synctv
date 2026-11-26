//! Admission, single-flight filling and bounded buffering for complete HTTP responses.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use synctv_common::ExecutionControl;

use super::SliceCache;
use crate::slice_cache::backend::SliceCacheBackend;
use crate::slice_cache::etag::{CachedResourceMeta, StoredEntry};
use crate::slice_cache::keys::full_response_keys;
use crate::slice_cache::status::CacheStatus;
use crate::slice_cache::types::{CachedFullResponse, FullResponseFetchResult};
use crate::{
    ProviderHeaders, apply_provider_headers, run_with_proxy_cancellation,
    send_with_redirect_validation_with_control_and_timeout,
};

/// Largest complete response eligible for the non-slice cache.
///
/// Media streams stay streaming regardless of size. This cap only bounds a
/// response that is fully buffered before being placed in the shared cache.
const MAX_FULL_RESPONSE_CACHE_ENTRY_SIZE: usize = 16 * 1024 * 1024;

impl SliceCache {
    /// Fetch a non-range response as one cacheable object.
    ///
    /// Every upstream response updates admission metadata. Requests without
    /// metadata use the per-resource lock as a single-flight guard and check
    /// metadata again after acquiring it. Resources already known to be
    /// ineligible bypass the lock and stream directly.
    pub(in crate::slice_cache) async fn get_or_fetch_full_response_with_control(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<FullResponseFetchResult, anyhow::Error> {
        let (key, meta_key) = full_response_keys(url, provider_headers);

        let stored_metadata = self.meta.get(&meta_key).map(|metadata| metadata.clone());

        if let Some(metadata) = stored_metadata {
            if !Self::full_response_metadata_allows_cache(&metadata) {
                let response = self
                    .fetch_full_response(
                        url,
                        provider_headers,
                        request_control,
                        upstream_header_timeout,
                    )
                    .await?;
                let metadata = Self::full_response_resource_metadata(&response);
                self.store_full_response_metadata(&key, &meta_key, metadata)
                    .await;
                return Ok(FullResponseFetchResult::Bypass(response));
            }
            if let Some(cached) = self.get_cached_full_response(&key, &metadata).await {
                return Ok(FullResponseFetchResult::Cached(cached));
            }
        }

        self.complete_full_response_cache_fill(
            &key,
            &meta_key,
            url,
            provider_headers,
            request_control,
            upstream_header_timeout,
        )
        .await
    }

    async fn fetch_full_response(
        &self,
        url: &str,
        provider_headers: &ProviderHeaders,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<reqwest::Response, anyhow::Error> {
        let request = apply_provider_headers(self.client.get(url), url, provider_headers)?;
        send_with_redirect_validation_with_control_and_timeout(
            &self.client,
            request,
            &self.ssrf_guard,
            request_control,
            upstream_header_timeout,
        )
        .await
        .context("Full response cache fetch failed")
        .map(|proxy_response| proxy_response.response)
    }

    fn full_response_resource_metadata(response: &reqwest::Response) -> CachedResourceMeta {
        let content_length = response.content_length();
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(ToString::to_string)
        };

        CachedResourceMeta {
            status: Some(response.status().as_u16()),
            etag: header("etag"),
            last_modified: header("last-modified"),
            total_size: content_length,
            supports_ranges: false,
            content_type: header("content-type"),
            content_encoding: header("content-encoding"),
            validated_at: std::time::SystemTime::now(),
            last_accessed: std::time::SystemTime::now(),
        }
    }

    fn full_response_metadata_allows_cache(metadata: &CachedResourceMeta) -> bool {
        metadata.status == Some(reqwest::StatusCode::OK.as_u16())
            && metadata
                .total_size
                .and_then(|length| usize::try_from(length).ok())
                .is_some_and(|length| length <= MAX_FULL_RESPONSE_CACHE_ENTRY_SIZE)
    }

    async fn store_full_response_metadata(
        &self,
        key: &str,
        meta_key: &str,
        metadata: CachedResourceMeta,
    ) {
        if !Self::full_response_metadata_allows_cache(&metadata) {
            self.backend.remove(key).await;
        }
        self.put_resource_meta_by_key(meta_key.to_string(), metadata);
    }

    async fn get_cached_full_response(
        &self,
        key: &str,
        metadata: &CachedResourceMeta,
    ) -> Option<CachedFullResponse> {
        let entry = self.backend.get(key).await?;
        if entry.is_expired() {
            return None;
        }

        Some(CachedFullResponse {
            data: entry.data,
            content_type: metadata.content_type.clone(),
            content_encoding: metadata.content_encoding.clone(),
            etag: metadata.etag.clone(),
            last_modified: metadata.last_modified.clone(),
            status: CacheStatus::Hit,
        })
    }

    async fn complete_full_response_cache_fill(
        &self,
        key: &str,
        meta_key: &str,
        url: &str,
        provider_headers: &ProviderHeaders,
        request_control: Option<&ExecutionControl>,
        upstream_header_timeout: Option<Duration>,
    ) -> Result<FullResponseFetchResult, anyhow::Error> {
        let guard = self
            .lock_slice_key(
                key,
                "Request cancelled while waiting for full response cache lock",
                request_control,
            )
            .await?;

        if let Some(metadata) = self.meta.get(meta_key).map(|metadata| metadata.clone()) {
            if Self::full_response_metadata_allows_cache(&metadata) {
                if let Some(cached) = self.get_cached_full_response(key, &metadata).await {
                    return Ok(FullResponseFetchResult::Cached(cached));
                }
            } else {
                drop(guard);
                self.locks
                    .remove_if(key, |_, lock| Arc::strong_count(lock) == 1);

                let response = self
                    .fetch_full_response(
                        url,
                        provider_headers,
                        request_control,
                        upstream_header_timeout,
                    )
                    .await?;
                let metadata = Self::full_response_resource_metadata(&response);
                self.store_full_response_metadata(key, meta_key, metadata)
                    .await;
                return Ok(FullResponseFetchResult::Bypass(response));
            }
        }

        let response = self
            .fetch_full_response(
                url,
                provider_headers,
                request_control,
                upstream_header_timeout,
            )
            .await?;
        let metadata = Self::full_response_resource_metadata(&response);
        if !Self::full_response_metadata_allows_cache(&metadata) {
            self.put_resource_meta_by_key(meta_key.to_string(), metadata);
            self.backend.remove(key).await;
            drop(guard);
            self.locks
                .remove_if(key, |_, lock| Arc::strong_count(lock) == 1);
            return Ok(FullResponseFetchResult::Bypass(response));
        }
        // File-backed bodies survive restarts while metadata does not. Remove
        // any independently restored body before publishing the new metadata.
        self.backend.remove(key).await;
        self.store_full_response_metadata(key, meta_key, metadata.clone())
            .await;

        self.cache_full_response(key, metadata, response, request_control)
            .await
    }

    async fn cache_full_response(
        &self,
        key: &str,
        metadata: CachedResourceMeta,
        mut response: reqwest::Response,
        request_control: Option<&ExecutionControl>,
    ) -> Result<FullResponseFetchResult, anyhow::Error> {
        let content_length =
            usize::try_from(metadata.total_size.ok_or_else(|| {
                anyhow::anyhow!("Cacheable full response omitted Content-Length")
            })?)
            .map_err(|_| anyhow::anyhow!("Cacheable full response Content-Length exceeds usize"))?;
        let mut body = Vec::with_capacity(content_length);
        while let Some(chunk) = run_with_proxy_cancellation(
            "full response cache body read",
            request_control,
            response.chunk(),
        )
        .await?
        .map_err(|error| anyhow::anyhow!("Failed to read full response cache body: {error}"))?
        {
            let next_length = body
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| anyhow::anyhow!("Full response cache body length overflow"))?;
            if next_length > content_length {
                return Err(anyhow::anyhow!(
                    "Full response cache body exceeded declared Content-Length: got at least {next_length}, expected {content_length}"
                ));
            }
            body.extend_from_slice(&chunk);
        }

        if body.len() != content_length {
            return Err(anyhow::anyhow!(
                "Full response cache body length mismatch: got {}, expected {content_length}",
                body.len()
            ));
        }

        let data = Bytes::from(body);
        self.backend
            .put(key, StoredEntry::new(data.clone(), self.config.segment_ttl))
            .await
            .map_err(|error| {
                anyhow::anyhow!("Failed to store full response cache entry: {error}")
            })?;

        Ok(FullResponseFetchResult::Cached(CachedFullResponse {
            data,
            content_type: metadata.content_type,
            content_encoding: metadata.content_encoding,
            etag: metadata.etag,
            last_modified: metadata.last_modified,
            status: CacheStatus::Miss,
        }))
    }
}
