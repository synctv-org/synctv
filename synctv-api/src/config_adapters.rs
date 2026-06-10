//! Configuration adapters shared by API composition paths.

use std::path::PathBuf;
use std::time::Duration;

use synctv_core::Config;
use synctv_proxy::slice_cache::{CacheBackendConfig, SliceCacheConfig};

#[must_use]
pub fn proxy_slice_cache_config_from_app_config(config: &Config) -> SliceCacheConfig {
    let backend = if config.proxy_slice_cache.file_backend_enabled {
        CacheBackendConfig::File {
            cache_dir: PathBuf::from(&config.proxy_slice_cache.file_cache_dir),
            dir_levels: (2, 2),
        }
    } else {
        CacheBackendConfig::Memory
    };

    SliceCacheConfig {
        enabled: config.proxy_slice_cache.enabled,
        slice_size: config.proxy_slice_cache.slice_size_bytes,
        max_cache_size: config.proxy_slice_cache.max_cache_size_bytes,
        segment_ttl: Duration::from_secs(config.proxy_slice_cache.segment_ttl_seconds),
        stale_max_age: Duration::from_secs(config.proxy_slice_cache.stale_max_age_seconds),
        stale_while_revalidate: config.proxy_slice_cache.stale_while_revalidate,
        backend,
        eviction_interval: Duration::from_secs(config.proxy_slice_cache.eviction_interval_seconds),
        watermark_ratio: config.proxy_slice_cache.watermark_ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn proxy_slice_cache_config_controls_startup_enablement() {
        let mut app_config = Config::default();

        let enabled_config = proxy_slice_cache_config_from_app_config(&app_config);
        assert!(
            enabled_config.enabled,
            "proxy slice cache should be enabled by default at startup"
        );

        app_config.proxy_slice_cache.enabled = false;
        let disabled_config = proxy_slice_cache_config_from_app_config(&app_config);
        assert!(
            !disabled_config.enabled,
            "startup config must be able to disable proxy slice cache"
        );
    }

    #[test]
    fn proxy_slice_cache_config_uses_file_backend_when_enabled() -> TestResult {
        let mut config = Config::default();
        config.proxy_slice_cache.file_backend_enabled = true;
        config.proxy_slice_cache.file_cache_dir = "/tmp/synctv-proxy-cache".to_string();

        let slice_cache_config = proxy_slice_cache_config_from_app_config(&config);

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

    #[test]
    fn proxy_slice_cache_config_maps_runtime_tuning_fields() {
        let mut config = Config::default();
        config.proxy_slice_cache.slice_size_bytes = 4 * 1024 * 1024;
        config.proxy_slice_cache.max_cache_size_bytes = 1024 * 1024 * 1024;
        config.proxy_slice_cache.segment_ttl_seconds = 600;
        config.proxy_slice_cache.stale_max_age_seconds = 120;
        config.proxy_slice_cache.stale_while_revalidate = false;
        config.proxy_slice_cache.eviction_interval_seconds = 30;
        config.proxy_slice_cache.watermark_ratio = 0.75;

        let slice_cache_config = proxy_slice_cache_config_from_app_config(&config);

        assert_eq!(slice_cache_config.slice_size, 4 * 1024 * 1024);
        assert_eq!(slice_cache_config.max_cache_size, 1024 * 1024 * 1024);
        assert_eq!(slice_cache_config.segment_ttl, Duration::from_mins(10));
        assert_eq!(slice_cache_config.stale_max_age, Duration::from_mins(2));
        assert!(!slice_cache_config.stale_while_revalidate);
        assert_eq!(
            slice_cache_config.eviction_interval,
            Duration::from_secs(30)
        );
        assert!((slice_cache_config.watermark_ratio - 0.75).abs() < f64::EPSILON);
    }
}
