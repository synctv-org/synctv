use anyhow::Result;
use std::sync::Arc;
use tracing::info;

use synctv_core::{Config, RedisConnectionRuntime, SharedStateMode, SharedStateProfile};

use crate::rtmp_auth;
use crate::server;

struct LivestreamRuntimeBindings {
    publisher_registry: Arc<dyn synctv_livestream::relay::StreamRegistryTrait>,
    user_stream_index: Arc<dyn rtmp_auth::UserStreamIndex>,
    #[cfg(test)]
    publisher_registry_shared: bool,
    #[cfg(test)]
    user_stream_index_shared: bool,
}

struct CoreRegistryConnectionRuntime {
    runtime: Arc<dyn RedisConnectionRuntime>,
}

#[async_trait::async_trait]
impl synctv_livestream::relay::RegistryConnectionRuntime for CoreRegistryConnectionRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        self.runtime.snapshot().await
    }
}

fn publisher_registry_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<(Arc<dyn synctv_livestream::relay::StreamRegistryTrait>, bool)> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => Ok((
            synctv_livestream::relay::shared_stream_registry(
                Arc::new(CoreRegistryConnectionRuntime {
                    runtime: profile.require_shared_runtime("livestream publisher registry")?,
                }),
                profile.key_prefix().to_string(),
            ),
            true,
        )),
        SharedStateMode::SharedBestEffort | SharedStateMode::LocalOnly => {
            Ok((synctv_livestream::relay::local_stream_registry(), false))
        }
    }
}

fn build_livestream_runtime_bindings(
    profile: &SharedStateProfile,
) -> Result<LivestreamRuntimeBindings> {
    let (publisher_registry, publisher_registry_shared) =
        publisher_registry_from_shared_state_profile(profile)?;
    let user_stream_index = rtmp_auth::user_stream_index_from_shared_state_profile(profile)?;
    let user_stream_index_shared = user_stream_index.supports_cross_node_lookup();

    info!(
        publisher_registry_shared,
        user_stream_index_shared,
        state_mode = ?profile.state_mode(),
        "Livestream runtime initialized"
    );

    Ok(LivestreamRuntimeBindings {
        publisher_registry,
        user_stream_index,
        #[cfg(test)]
        publisher_registry_shared,
        #[cfg(test)]
        user_stream_index_shared,
    })
}

/// Initialize livestream components (RTMP server and live streaming infrastructure).
///
/// Returns the `LivestreamState` handle (for graceful shutdown) and the shared
/// `LiveStreamingInfrastructure` (passed to gRPC/HTTP servers).
///
pub async fn init_livestream(
    config: &Config,
    synctv_services: &synctv_core::bootstrap::services::Services,
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    node_id: &str,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(
    Option<server::LivestreamState>,
    Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    Vec<tokio::task::JoinHandle<()>>,
)> {
    info!("Initializing livestream infrastructure...");

    let shared_state_profile = SharedStateProfile::from_runtime(
        shared_runtime,
        &config.redis.key_prefix,
        config.cluster_runtime_enabled(),
    );
    let runtime = build_livestream_runtime_bindings(&shared_state_profile)?;
    let publisher_registry = runtime.publisher_registry.clone();
    let user_stream_index = runtime.user_stream_index.clone();

    // Shared tracker for user->stream mapping (kick-on-ban)
    let user_stream_tracker = Arc::new(synctv_livestream::api::StreamTracker::new());

    // Stream lifecycle event channel (app-level logging)
    let (stream_lifecycle_tx, mut stream_lifecycle_rx) =
        tokio::sync::broadcast::channel::<rtmp_auth::StreamLifecycleEvent>(64);

    let mut background_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Start periodic cleanup of stale stream tracker entries.
    // When a publisher crashes without a clean on_unpublish, secondary indexes
    // can retain orphaned references. This background task cleans them up.
    let tracker_cleanup_handle =
        user_stream_tracker.start_periodic_cleanup(std::time::Duration::from_mins(1), cancel);
    background_handles.push(tracker_cleanup_handle);

    let lifecycle_handle = tokio::spawn(async move {
        while let Ok(event) = stream_lifecycle_rx.recv().await {
            match event {
                rtmp_auth::StreamLifecycleEvent::Started {
                    room_id,
                    media_id,
                    user_id,
                } => {
                    info!(
                        room_id = %room_id,
                        media_id = %media_id,
                        user_id = %user_id,
                        "Stream started"
                    );
                }
                rtmp_auth::StreamLifecycleEvent::Stopped {
                    room_id,
                    media_id,
                    user_id,
                } => {
                    info!(
                        room_id = %room_id,
                        media_id = %media_id,
                        user_id = %user_id,
                        "Stream stopped"
                    );
                }
            }
        }
    });
    background_handles.push(lifecycle_handle);

    // Pre-bind RTMP listener to catch port-in-use errors before deep initialization.
    // This follows the same pattern as gRPC/HTTP server pre-binding.
    let rtmp_listen_addr = format!("{}:{}", config.server.host, config.livestream.rtmp_port);
    let rtmp_socket_addr: std::net::SocketAddr = rtmp_listen_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid RTMP address '{rtmp_listen_addr}': {e}"))?;
    let rtmp_listener = tokio::net::TcpListener::bind(rtmp_socket_addr)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind RTMP address {rtmp_socket_addr}: {e}"))?;
    info!("RTMP server pre-bound on {}", rtmp_socket_addr);

    let publisher_registry_for_auth = publisher_registry.clone();
    let user_stream_tracker_for_auth = user_stream_tracker.clone();

    // Build the LivestreamServer first so RTMP auth can share the restart flag
    // used by the StreamHub restart loop.
    let livestream_server = synctv_livestream::LivestreamServer::new(
        synctv_livestream::LivestreamConfig {
            rtmp_address: rtmp_listen_addr,
            gop_cache_size: config.livestream.gop_cache_size as usize,
            node_id: node_id.to_string(),
            cleanup_check_interval_seconds: config.livestream.cleanup_check_interval_seconds,
            stream_timeout_seconds: config.livestream.stream_timeout_seconds,
            cluster_enabled: config.cluster_runtime_enabled(),
            cluster_secret: if config.cluster_runtime_enabled()
                && !config.server.cluster_secret.is_empty()
            {
                Some(config.server.cluster_secret.clone())
            } else {
                None
            },
            gop_cache_max_memory_mb: config.livestream.gop_cache_max_memory_mb,
            api_address: config.advertise_api_address(),
            hls_memory_max_mb: config.livestream.hls_memory_max_mb,
            hls_shared_storage: config.livestream.hls_shared_storage,
            hls_storage_path: config.livestream.hls_storage_path.clone(),
        },
        publisher_registry,
        user_stream_tracker,
    );

    let rtmp_auth_impl = rtmp_auth::SyncTvRtmpAuth::new(
        synctv_services.room_service.clone(),
        synctv_services.user_service.clone(),
        synctv_services.publish_key_service.clone(),
        user_stream_tracker_for_auth,
        publisher_registry_for_auth,
        node_id.to_string(),
        config.advertise_api_address(),
        Arc::new(
            synctv_core::PublicIdCodec::from_config(&config.external_ids)
                .expect("external_ids config must be validated before building RTMP auth"),
        ),
        Some(stream_lifecycle_tx),
    )
    .with_user_stream_index(user_stream_index)
    .with_restarting_flag(livestream_server.restarting_flag());
    let rtmp_auth: Arc<dyn synctv_livestream::AuthCallback> = Arc::new(rtmp_auth_impl);

    // One-shot facade: start all xiu components
    let handle = livestream_server
        .with_auth(rtmp_auth)
        .with_rtmp_listener(rtmp_listener)
        .start()
        .map_err(|e| anyhow::anyhow!("Failed to start livestream: {e}"))?;

    let live_infra = handle.infrastructure.clone();
    let state = Some(server::LivestreamState { handle });

    Ok((state, Some(live_infra), background_handles))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_livestream_runtime_bindings_without_runtime_uses_local_backends() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);

        let runtime = build_livestream_runtime_bindings(&profile)
            .expect("local-only profile should build local livestream runtime");

        assert!(!runtime.publisher_registry_shared);
        assert!(!runtime.user_stream_index_shared);
    }

    struct MockRedisRuntime;

    #[async_trait::async_trait]
    impl RedisConnectionRuntime for MockRedisRuntime {
        async fn snapshot(&self) -> redis::aio::ConnectionManager {
            panic!("mock redis runtime snapshot should not be called in factory tests");
        }
    }

    #[test]
    fn test_build_livestream_runtime_bindings_keeps_standalone_mode_local_even_with_runtime() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedBestEffort,
            Some(Arc::new(MockRedisRuntime)),
            "test:",
        );

        let runtime = build_livestream_runtime_bindings(&profile)
            .expect("standalone profile should keep livestream runtime local");

        assert!(!runtime.publisher_registry_shared);
        assert!(!runtime.user_stream_index_shared);
    }

    #[test]
    fn test_build_livestream_runtime_bindings_requires_shared_runtime_in_cluster_mode() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);

        let Err(error) = build_livestream_runtime_bindings(&profile) else {
            panic!("cluster profile without runtime must be rejected");
        };

        assert!(
            error
                .to_string()
                .contains("cluster runtime requires shared livestream publisher registry"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_build_livestream_runtime_bindings_uses_shared_backends_in_cluster_mode() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedRequired,
            Some(Arc::new(MockRedisRuntime)),
            "test:",
        );

        let runtime = build_livestream_runtime_bindings(&profile)
            .expect("cluster profile with runtime should build shared livestream runtime");

        assert!(runtime.publisher_registry_shared);
        assert!(runtime.user_stream_index_shared);
    }

    #[test]
    fn test_init_livestream_signature_uses_runtime_abstraction() {
        fn assert_signature(
            config: &Config,
            services: &synctv_core::bootstrap::services::Services,
            runtime: Option<Arc<dyn RedisConnectionRuntime>>,
            node_id: &str,
            cancel: tokio_util::sync::CancellationToken,
        ) {
            std::mem::drop(init_livestream(config, services, runtime, node_id, cancel));
        }

        let _ = assert_signature;
    }
}
