//! Runtime-setting adapters shared by API composition paths.

use std::path::PathBuf;
use std::time::Duration;

use synctv_proxy::slice_cache::{CacheBackendConfig, SliceCacheConfig};

#[must_use]
pub fn proxy_slice_cache_options_from_runtime_settings(
    runtime_settings: &crate::ApiRuntimeSettings,
) -> SliceCacheConfig {
    let backend = if runtime_settings.proxy_slice_cache.file_backend_enabled {
        CacheBackendConfig::File {
            cache_dir: PathBuf::from(&runtime_settings.proxy_slice_cache.file_cache_dir),
            dir_levels: (2, 2),
        }
    } else {
        CacheBackendConfig::Memory
    };

    SliceCacheConfig {
        enabled: runtime_settings.proxy_slice_cache.enabled,
        slice_size: runtime_settings.proxy_slice_cache.slice_size_bytes,
        max_cache_size: runtime_settings.proxy_slice_cache.max_cache_size_bytes,
        segment_ttl: Duration::from_secs(runtime_settings.proxy_slice_cache.segment_ttl_seconds),
        stale_max_age: Duration::from_secs(
            runtime_settings.proxy_slice_cache.stale_max_age_seconds,
        ),
        stale_while_revalidate: runtime_settings.proxy_slice_cache.stale_while_revalidate,
        backend,
        eviction_interval: Duration::from_secs(
            runtime_settings.proxy_slice_cache.eviction_interval_seconds,
        ),
        watermark_ratio: runtime_settings.proxy_slice_cache.watermark_ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ApiRuntimeSettings;

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn proxy_slice_cache_config_uses_file_backend_when_enabled() -> TestResult {
        let mut config = ApiRuntimeSettings::default();
        config.proxy_slice_cache.file_backend_enabled = true;
        config.proxy_slice_cache.file_cache_dir = "/tmp/synctv-proxy-cache".to_string();

        let slice_cache_config = proxy_slice_cache_options_from_runtime_settings(&config);

        match slice_cache_config.backend {
            CacheBackendConfig::File {
                cache_dir,
                dir_levels,
            } => {
                assert_eq!(cache_dir, PathBuf::from("/tmp/synctv-proxy-cache"));
                assert_eq!(dir_levels, (2, 2));
            }
            CacheBackendConfig::Memory => return Err(anyhow::anyhow!("expected file backend")),
        }
        Ok(())
    }
}
