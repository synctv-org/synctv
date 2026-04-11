use anyhow::Result;
use std::sync::Arc;
use tracing::info;

use synctv_core::bootstrap::RedisHandles;
use synctv_core::Config;

use crate::rtmp_auth;
use crate::server;

/// Initialize livestream components (RTMP server and live streaming infrastructure).
///
/// Returns the `LivestreamState` handle (for graceful shutdown) and the shared
/// `LiveStreamingInfrastructure` (passed to gRPC/HTTP servers).
///
/// When `redis_handles` is `None`, uses an in-memory stream registry.
pub async fn init_livestream(
    config: &Config,
    synctv_services: &synctv_core::bootstrap::services::Services,
    redis_handles: Option<&RedisHandles>,
    node_id: &str,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(
    Option<server::LivestreamState>,
    Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    Vec<tokio::task::JoinHandle<()>>,
)> {
    info!("Initializing livestream infrastructure...");

    // Publisher registry: Redis-backed when available, in-memory otherwise
    let (publisher_registry, redis_conn_for_auth): (
        Arc<dyn synctv_livestream::relay::StreamRegistryTrait>,
        _,
    ) = if let Some(rh) = redis_handles {
        let redis_conn = rh.conn.clone();
        let redis_conn_for_auth = redis_conn.clone();
        let registry = Arc::new(
            synctv_livestream::relay::StreamRegistry::with_shared_conn_and_key_prefix(
                redis_conn,
                config.redis.key_prefix.clone(),
            ),
        ) as Arc<dyn synctv_livestream::relay::StreamRegistryTrait>;
        info!("Livestream publisher registry: Redis-backed");
        (registry, Some(redis_conn_for_auth))
    } else {
        let registry = Arc::new(synctv_livestream::relay::InMemoryStreamRegistry::new())
            as Arc<dyn synctv_livestream::relay::StreamRegistryTrait>;
        info!("Livestream publisher registry: in-memory");
        (registry, None)
    };

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

    let mut rtmp_auth_impl = rtmp_auth::SyncTvRtmpAuth::new(
        synctv_services.room_service.clone(),
        synctv_services.user_service.clone(),
        synctv_services.publish_key_service.clone(),
        user_stream_tracker_for_auth,
        publisher_registry_for_auth,
        node_id.to_string(),
        config.advertise_api_address(),
        Some(stream_lifecycle_tx),
        config.redis.key_prefix.clone(),
    )
    .with_restarting_flag(livestream_server.restarting_flag());

    if let Some(conn) = redis_conn_for_auth {
        rtmp_auth_impl = rtmp_auth_impl.with_redis(conn);
    }
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
