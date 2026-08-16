use anyhow::Result;
use std::sync::Arc;
use tracing::info;

use synctv_adapter::PublicIdCodec;
use synctv_core::{RedisConnectionRuntime, SharedStateMode, SharedStateProfile};
use synctv_realtime::fanout::RealtimeEventService;

use crate::app_config::AppConfig as Config;
use crate::resource_options::{hls_s3_options, hls_storage_backend};

use crate::rtmp_auth;
use crate::server;

struct LivestreamRuntimeBindings {
    publisher_registry: Arc<dyn synctv_livestream::StreamRegistryTrait>,
    user_stream_index: Arc<dyn rtmp_auth::UserStreamIndex>,
    #[cfg(test)]
    publisher_registry_shared: bool,
    #[cfg(test)]
    user_stream_index_shared: bool,
}

pub struct LivestreamInitOptions {
    pub hls_cleanup_leader: Arc<dyn synctv_core::service::LeaderCheck>,
    pub rtmp_listener: Option<tokio::net::TcpListener>,
}

struct CoreRegistryConnectionRuntime {
    runtime: Arc<dyn RedisConnectionRuntime>,
}

#[async_trait::async_trait]
impl synctv_livestream::RegistryConnectionRuntime for CoreRegistryConnectionRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        self.runtime.snapshot().await
    }
}

fn publisher_registry_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<(Arc<dyn synctv_livestream::StreamRegistryTrait>, bool)> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => Ok((
            synctv_livestream::shared_stream_registry(
                Arc::new(CoreRegistryConnectionRuntime {
                    runtime: profile.require_shared_runtime("livestream publisher registry")?,
                }),
                profile.key_prefix().to_string(),
            ),
            true,
        )),
        SharedStateMode::SharedBestEffort | SharedStateMode::LocalOnly => {
            Ok((synctv_livestream::local_stream_registry(), false))
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
    public_id_codec: Arc<PublicIdCodec>,
    synctv_services: &crate::bootstrap::Services,
    realtime_event_service: Arc<dyn RealtimeEventService>,
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    init_options: LivestreamInitOptions,
    node_id: &str,
) -> Result<(
    Option<server::LivestreamState>,
    Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    Vec<tokio::task::JoinHandle<()>>,
)> {
    info!("Initializing livestream infrastructure...");

    let LivestreamInitOptions {
        hls_cleanup_leader,
        rtmp_listener,
    } = init_options;

    let shared_state_profile = SharedStateProfile::for_cluster_runtime(
        shared_runtime,
        &config.redis.key_prefix,
        config.cluster_runtime_enabled(),
    );
    let runtime = build_livestream_runtime_bindings(&shared_state_profile)?;
    let publisher_registry = runtime.publisher_registry.clone();
    let user_stream_index = runtime.user_stream_index.clone();

    // Shared tracker for user->stream mapping (kick-on-user-ban)
    let user_stream_tracker = Arc::new(synctv_livestream::StreamTracker::new());

    let (stream_lifecycle_tx, mut stream_lifecycle_rx) =
        tokio::sync::mpsc::channel::<synctv_livestream::StreamLifecycleEvent>(64);

    // Pre-bind RTMP listener to catch port-in-use errors before deep initialization.
    // This follows the same pattern as gRPC/HTTP server pre-binding.
    let rtmp_listen_addr = config.livestream_address();
    let rtmp_socket_addr: std::net::SocketAddr = rtmp_listen_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid RTMP address '{rtmp_listen_addr}': {e}"))?;
    let rtmp_listener = match rtmp_listener {
        Some(listener) => listener,
        None => tokio::net::TcpListener::bind(rtmp_socket_addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind RTMP address {rtmp_socket_addr}: {e}"))?,
    };
    let rtmp_bound_addr = rtmp_listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("Failed to read RTMP listener address: {e}"))?;
    info!("RTMP server pre-bound on {}", rtmp_bound_addr);

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
            distributed_enabled: config.cluster_runtime_enabled(),
            cluster_secret: if config.cluster_runtime_enabled() && !config.cluster.secret.is_empty()
            {
                Some(config.cluster.secret.clone())
            } else {
                None
            },
            grpc_max_message_size_bytes: config.server.grpc_max_message_size_bytes,
            grpc_compression_enabled: config.server.grpc_compression_enabled,
            gop_cache_max_memory_mb: config.livestream.gop_cache_max_memory_mb,
            max_flv_tag_size_bytes: config.livestream.max_flv_tag_size_bytes,
            cluster_address: config.advertise_cluster_address(),
            hls_memory_max_mb: config.livestream.hls_storage.memory_max_mb(),
            hls_storage_backend: hls_storage_backend(config.livestream.hls_storage.backend()),
            hls_storage_path: config.livestream.hls_storage.path().to_string(),
            hls_s3: hls_s3_options(&config.livestream.hls_storage),
            ssrf_guard: config.security.ssrf_guard(),
        },
        publisher_registry,
        user_stream_tracker.clone(),
    )
    .with_lifecycle_sender(stream_lifecycle_tx);
    let publisher_control = livestream_server.publisher_control_handle();

    let rtmp_auth_impl = rtmp_auth::SyncTvRtmpAuth::new(rtmp_auth::SyncTvRtmpAuthConfig {
        room_service: synctv_services.room_service.clone(),
        user_service: synctv_services.user_service.clone(),
        publish_key_service: synctv_services.publish_key_service.clone(),
        user_stream_tracker: user_stream_tracker_for_auth,
        registry: publisher_registry_for_auth,
        node_id: node_id.to_string(),
        cluster_address: config.advertise_cluster_address(),
        public_id_codec,
        is_restarting: Some(livestream_server.restarting_flag()),
        user_stream_index,
        publisher_control,
    });
    let rtmp_auth: Arc<dyn synctv_xiu::rtmp::auth::AuthCallback> = Arc::new(rtmp_auth_impl);

    // One-shot facade: start all xiu components
    let handle = livestream_server
        .with_auth(rtmp_auth)
        .with_hls_cleanup_leader(hls_cleanup_leader)
        .with_rtmp_listener(rtmp_listener)
        .start()
        .map_err(|e| anyhow::anyhow!("Failed to start livestream: {e}"))?;

    let live_infra = handle.infrastructure.clone();
    let state = Some(server::LivestreamState {
        handle,
        infrastructure: live_infra.clone(),
    });

    let mut background_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    let lifecycle_handle = tokio::spawn(async move {
        while let Some(event) = stream_lifecycle_rx.recv().await {
            match event {
                synctv_livestream::StreamLifecycleEvent::Started {
                    room_id,
                    media_id,
                    user_id,
                    generation_id,
                } => {
                    info!(
                        room_id = %room_id,
                        media_id = %media_id,
                        user_id = %user_id,
                        generation_id = %generation_id,
                        "Stream started"
                    );
                    let event = parse_lifecycle_realtime_event(
                        &room_id,
                        &media_id,
                        &user_id,
                        &generation_id,
                        true,
                    );
                    if let Some(event) = event {
                        log_lifecycle_delivery(
                            realtime_event_service.broadcast_outcome(event),
                            &room_id,
                            &media_id,
                        );
                    }
                }
                synctv_livestream::StreamLifecycleEvent::Stopped {
                    room_id,
                    media_id,
                    user_id,
                    generation_id,
                } => {
                    info!(
                        room_id = %room_id,
                        media_id = %media_id,
                        user_id = %user_id,
                        generation_id = %generation_id,
                        "Stream stopped"
                    );
                    let event = parse_lifecycle_realtime_event(
                        &room_id,
                        &media_id,
                        &user_id,
                        &generation_id,
                        false,
                    );
                    if let Some(event) = event {
                        log_lifecycle_delivery(
                            realtime_event_service.broadcast_outcome(event),
                            &room_id,
                            &media_id,
                        );
                    }
                }
            }
        }
    });
    background_handles.push(lifecycle_handle);

    Ok((state, Some(live_infra), background_handles))
}

fn log_lifecycle_delivery(
    outcome: synctv_realtime::fanout::RealtimeDeliveryOutcome,
    room_id: &str,
    media_id: &str,
) {
    if outcome.distributed_delivery_missed() {
        tracing::warn!(
            room_id,
            media_id,
            local_delivered = outcome.local_delivered(),
            "Livestream lifecycle refresh missed distributed realtime delivery"
        );
    }
}

fn parse_lifecycle_realtime_event(
    room_id: &str,
    media_id: &str,
    user_id: &str,
    generation_id: &str,
    is_live: bool,
) -> Option<synctv_realtime::sync::RealtimeEvent> {
    let parsed = (|| {
        Some(synctv_realtime::sync::RealtimeEvent::LiveStreamChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.parse().ok()?,
            media_id: media_id.parse().ok()?,
            user_id: user_id.parse().ok()?,
            generation_id: generation_id.to_string(),
            is_live,
            timestamp: synctv_core::SystemClock.now(),
        })
    })();
    if parsed.is_none() {
        tracing::error!(
            room_id,
            media_id,
            user_id,
            "Discarding livestream lifecycle event with invalid identifiers"
        );
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_livestream_runtime_bindings_without_runtime_uses_local_backends() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);

        let runtime = build_livestream_runtime_bindings(&profile)
            .expect("local-only profile should build local livestream runtime");

        assert!(!runtime.publisher_registry_shared);
        assert!(!runtime.user_stream_index_shared);
    }

    struct TestRedisRuntime;

    #[async_trait::async_trait]
    impl RedisConnectionRuntime for TestRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            panic!("test redis runtime snapshot should not be called in factory tests");
        }
    }

    #[test]
    fn test_build_livestream_runtime_bindings_keeps_standalone_mode_local_even_with_runtime() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedBestEffort,
            Some(Arc::new(TestRedisRuntime)),
            "test:",
        );

        let runtime = build_livestream_runtime_bindings(&profile)
            .expect("standalone profile should keep livestream runtime local");

        assert!(!runtime.publisher_registry_shared);
        assert!(!runtime.user_stream_index_shared);
    }

    #[test]
    fn test_build_livestream_runtime_bindings_requires_shared_runtime_in_cluster_mode() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);

        let Err(error) = build_livestream_runtime_bindings(&profile) else {
            panic!("cluster profile without runtime must be rejected");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared livestream publisher registry"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_build_livestream_runtime_bindings_uses_shared_backends_in_cluster_mode() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedRequired,
            Some(Arc::new(TestRedisRuntime)),
            "test:",
        );

        let runtime = build_livestream_runtime_bindings(&profile)
            .expect("cluster profile with runtime should build shared livestream runtime");

        assert!(runtime.publisher_registry_shared);
        assert!(runtime.user_stream_index_shared);
    }
}
