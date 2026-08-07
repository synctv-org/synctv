#![allow(clippy::unwrap_used)]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use bytes::Bytes;
use redis::aio::ConnectionManager;
use synctv_common::ssrf::SsrfGuard;
use synctv_core_testing::RtmpPublisher;
use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite, RtmpStreamMode};
use synctv_xiu::storage::{FileStorage, HlsStorage, S3Config, S3Storage};
use tokio::{
    net::TcpListener,
    sync::{mpsc, RwLock},
};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{codec::CompressionEncoding, transport::Server};

use crate::{
    shared_stream_registry, FlvStreamingApi, HlsS3Options, HlsStorageBackend, HlsStreamingApi,
    LivestreamConfig, LivestreamServer, RegistryConnectionRuntime, StreamRegistryTrait,
    StreamRelayServiceServer, StreamTracker,
};

const ROOM: &str = "cluster-room";
const MEDIA: &str = "cluster-media";
const CLUSTER_SECRET: &str = "cluster-test-secret";

struct RedisRuntime {
    manager: RwLock<ConnectionManager>,
}

#[async_trait]
impl RegistryConnectionRuntime for RedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<ConnectionManager> {
        Ok(self.manager.read().await.clone())
    }
}

fn shared_registry(manager: ConnectionManager, key_prefix: &str) -> Arc<dyn StreamRegistryTrait> {
    shared_stream_registry(
        Arc::new(RedisRuntime {
            manager: RwLock::new(manager),
        }),
        key_prefix.to_string(),
    )
}

struct RegistryAuth {
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    cluster_address: String,
}

#[async_trait]
impl AuthCallback for RegistryAuth {
    async fn on_publish(
        &self,
        generation_id: synctv_xiu::streamhub::utils::Uuid,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> std::result::Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>>
    {
        if app_name != ROOM {
            return Err(anyhow::anyhow!("unexpected RTMP app").into());
        }
        if stream_name != MEDIA {
            return Err(anyhow::anyhow!("unexpected RTMP stream").into());
        }
        let registered = self
            .registry
            .try_activate_generation(
                app_name,
                stream_name,
                &self.node_id,
                "cluster-user",
                &self.cluster_address,
                &generation_id.to_string(),
            )
            .await?;
        if !registered {
            return Err(anyhow::anyhow!("publisher key is already registered").into());
        }
        Ok(Some(AuthPublishRewrite {
            app_name: app_name.to_string(),
            stream_name: stream_name.to_string(),
            media_mode: RtmpStreamMode::Default,
        }))
    }

    async fn on_play(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn on_publish_rollback(
        &self,
        generation_id: synctv_xiu::streamhub::utils::Uuid,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        let Ok(Some(publisher)) = self
            .registry
            .get_active_generation(app_name, stream_name)
            .await
        else {
            return;
        };
        if publisher.generation_id == generation_id.to_string() {
            let _ = self
                .registry
                .deactivate_generation_if_lease_matches(
                    app_name,
                    stream_name,
                    &publisher.generation_id,
                    publisher.lease_epoch,
                )
                .await;
        }
    }
}

fn config(
    node_id: &str,
    rtmp_address: std::net::SocketAddr,
    cluster_address: std::net::SocketAddr,
) -> LivestreamConfig {
    LivestreamConfig {
        rtmp_address: rtmp_address.to_string(),
        gop_cache_size: 1024 * 1024,
        node_id: node_id.to_string(),
        cleanup_check_interval_seconds: 1,
        stream_timeout_seconds: 30,
        distributed_enabled: true,
        cluster_secret: Some(CLUSTER_SECRET.to_string()),
        grpc_max_message_size_bytes: 16 * 1024 * 1024,
        grpc_compression_enabled: true,
        gop_cache_max_memory_mb: 8,
        max_flv_tag_size_bytes: 10 * 1024 * 1024,
        cluster_address: cluster_address.to_string(),
        hls_memory_max_mb: 64,
        hls_storage_backend: HlsStorageBackend::Memory,
        hls_storage_path: String::new(),
        hls_s3: HlsS3Options::default(),
        ssrf_guard: SsrfGuard::strict_policy(),
    }
}

async fn expect_flv_av(
    receiver: &mut mpsc::Receiver<std::result::Result<Bytes, std::io::Error>>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut header = false;
    let mut audio = false;
    let mut video = false;
    while Instant::now() < deadline && !(header && audio && video) {
        let chunk = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            receiver.recv(),
        )
        .await
        .context("timed out waiting for cross-node FLV")?
        .context("cross-node FLV response closed")??;
        header |= chunk.starts_with(b"FLV");
        if matches!(chunk.first(), Some(8 | 9)) {
            anyhow::ensure!(chunk.len() >= 15, "cross-node FLV tag is truncated");
            let previous_tag_size = u32::from_be_bytes(
                chunk[chunk.len() - 4..]
                    .try_into()
                    .expect("FLV previous-tag-size has four bytes"),
            );
            assert_eq!(
                usize::try_from(previous_tag_size)?,
                chunk.len() - 4,
                "FLV previous-tag-size must cover tag header and payload"
            );
            audio |= chunk.first() == Some(&8);
            video |= chunk.first() == Some(&9);
        }
    }
    anyhow::ensure!(
        header && audio && video,
        "cross-node FLV lacked header or A/V tags"
    );
    Ok(())
}

async fn expect_flv_timestamp(
    receiver: &mut mpsc::Receiver<std::result::Result<Bytes, std::io::Error>>,
    minimum_timestamp: u32,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut audio = false;
    let mut video = false;
    while Instant::now() < deadline && !(audio && video) {
        let chunk = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            receiver.recv(),
        )
        .await
        .context("timed out waiting for live cross-node FLV frame")?
        .context("cross-node FLV response closed during live relay")??;
        if chunk.len() < 8 || !matches!(chunk.first(), Some(8 | 9)) {
            continue;
        }
        let timestamp = u32::from(chunk[4]) << 16
            | u32::from(chunk[5]) << 8
            | u32::from(chunk[6])
            | u32::from(chunk[7]) << 24;
        if timestamp >= minimum_timestamp {
            audio |= chunk.first() == Some(&8);
            video |= chunk.first() == Some(&9);
        }
    }
    anyhow::ensure!(audio && video, "both live A/V tags must cross the relay");
    Ok(())
}

async fn wait_for_publisher(
    infrastructure: &crate::LiveStreamingInfrastructure,
) -> Result<crate::StreamGeneration> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(publisher) = infrastructure.find_publisher(ROOM, MEDIA).await? {
            return Ok(publisher);
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "publisher did not reach Redis registry"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_remote_playlist_until<F>(
    infrastructure: &crate::LiveStreamingInfrastructure,
    generation_id: &str,
    ready: F,
    timeout_message: &str,
) -> Result<String>
where
    F: Fn(&str) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    let segment_url_base = format!("/cluster-hls/{generation_id}/");
    loop {
        if let Some(playlist) = HlsStreamingApi::generate_playlist_simple(
            infrastructure,
            ROOM,
            MEDIA,
            generation_id,
            &segment_url_base,
        )
        .await?
        {
            if ready(&playlist) {
                return Ok(playlist);
            }
        }
        anyhow::ensure!(Instant::now() < deadline, "{timeout_message}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_remote_playlist(
    infrastructure: &crate::LiveStreamingInfrastructure,
    generation_id: &str,
) -> Result<String> {
    wait_for_remote_playlist_until(
        infrastructure,
        generation_id,
        |playlist| playlist.contains("#EXTINF:"),
        "remote HLS playlist was not generated",
    )
    .await
}

async fn wait_for_remote_endlist(
    infrastructure: &crate::LiveStreamingInfrastructure,
    generation_id: &str,
) -> Result<String> {
    wait_for_remote_playlist_until(
        infrastructure,
        generation_id,
        |playlist| playlist.contains("#EXT-X-ENDLIST"),
        "remote HLS playlist did not expose EXT-X-ENDLIST after unregister",
    )
    .await
}

async fn wait_for_registry_absent(
    infrastructure: &crate::LiveStreamingInfrastructure,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if infrastructure.find_publisher(ROOM, MEDIA).await?.is_none() {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "publisher ownership remained in Redis after disconnect"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_local_endlist(
    infrastructure: &crate::LiveStreamingInfrastructure,
    generation_id: &str,
) -> Result<()> {
    let registry = infrastructure
        .hls_stream_registry
        .as_ref()
        .context("node A HLS registry missing")?;
    let stream_key = synctv_xiu::hls::generation_registry_key(ROOM, MEDIA, generation_id);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if registry.get(&stream_key).is_some_and(|state| {
            state
                .read()
                .generate_m3u8(|name| format!("/cluster-hls/{name}.ts"))
                .contains("#EXT-X-ENDLIST")
        }) {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "publisher disconnect did not finish the HLS playlist"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn two_real_livestream_nodes_relay_rtmp_to_flv_and_proxy_memory_hls() -> Result<()> {
    let (_redis, _client, manager) =
        synctv_core_testing::start_redis_client_manager_with_label("livestream-cluster-e2e").await;
    let prefix = synctv_core_testing::test_redis_key_prefix("livestream-cluster-e2e");
    let registry_a = shared_registry(manager.clone(), &prefix);
    let registry_b = shared_registry(manager, &prefix);

    let rtmp_listener_a = TcpListener::bind("127.0.0.1:0").await?;
    let rtmp_address_a = rtmp_listener_a.local_addr()?;
    let rtmp_listener_b = TcpListener::bind("127.0.0.1:0").await?;
    let rtmp_address_b = rtmp_listener_b.local_addr()?;
    let relay_listener = TcpListener::bind("127.0.0.1:0").await?;
    let relay_address = relay_listener.local_addr()?;

    let auth: Arc<dyn AuthCallback> = Arc::new(RegistryAuth {
        registry: Arc::clone(&registry_a),
        node_id: "node-a".to_string(),
        cluster_address: relay_address.to_string(),
    });
    let mut node_a = LivestreamServer::new(
        config("node-a", rtmp_address_a, relay_address),
        registry_a,
        Arc::new(StreamTracker::new()),
    )
    .with_auth(auth)
    .with_rtmp_listener(rtmp_listener_a)
    .start()?;
    let auth_b: Arc<dyn AuthCallback> = Arc::new(RegistryAuth {
        registry: Arc::clone(&registry_b),
        node_id: "node-b".to_string(),
        cluster_address: rtmp_address_b.to_string(),
    });
    let mut node_b = LivestreamServer::new(
        config("node-b", rtmp_address_b, rtmp_address_b),
        registry_b,
        Arc::new(StreamTracker::new()),
    )
    .with_auth(auth_b)
    .with_rtmp_listener(rtmp_listener_b)
    .start()?;

    let relay_cancel = CancellationToken::new();
    let relay_service = StreamRelayServiceServer::new(node_a.infrastructure.relay_service(
        "node-a".to_string(),
        CLUSTER_SECRET.to_string(),
        relay_cancel.clone(),
    ))
    .accept_compressed(CompressionEncoding::Gzip)
    .send_compressed(CompressionEncoding::Gzip);
    let relay_cancel_for_server = relay_cancel.clone();
    let relay_task = tokio::spawn(async move {
        Server::builder()
            .add_service(relay_service)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(relay_listener),
                relay_cancel_for_server.cancelled_owned(),
            )
            .await
    });

    let mut publisher = RtmpPublisher::connect(rtmp_address_a, ROOM, MEDIA).await?;
    let publisher_info = wait_for_publisher(&node_b.infrastructure).await?;
    assert_eq!(publisher_info.node_id, "node-a");
    assert_eq!(publisher_info.cluster_address, relay_address.to_string());
    let first_master_generation = HlsStreamingApi::resolve_active_generation_with_pull(
        &node_b.infrastructure,
        ROOM,
        MEDIA,
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("HLS master should resolve the first active generation"))?;
    assert_eq!(
        first_master_generation.generation_id,
        publisher_info.generation_id
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;
    publisher.send_audio(1).await?;
    publisher.send_video(5_001, false).await?;
    publisher.send_audio(5_001).await?;
    publisher.send_video(10_001, true).await?;
    publisher.send_audio(10_001).await?;
    publisher.send_video(15_001, false).await?;
    publisher.send_audio(15_001).await?;
    publisher.send_video(20_001, true).await?;
    publisher.send_audio(20_001).await?;

    let (first_result, second_result) = tokio::join!(
        FlvStreamingApi::create_session_with_pull(&node_b.infrastructure, ROOM, MEDIA, None),
        FlvStreamingApi::create_session_with_pull(&node_b.infrastructure, ROOM, MEDIA, None),
    );
    let (mut first_flv, first_guard) = first_result?;
    let (mut second_flv, second_guard) = second_result?;
    expect_flv_av(&mut first_flv).await?;
    expect_flv_av(&mut second_flv).await?;

    publisher.send_video(25_001, false).await?;
    publisher.send_audio(25_001).await?;
    expect_flv_timestamp(&mut first_flv, 25_001).await?;
    expect_flv_timestamp(&mut second_flv, 25_001).await?;

    let playlist =
        wait_for_remote_playlist(&node_b.infrastructure, &publisher_info.generation_id).await?;
    let segment_names = playlist
        .lines()
        .filter_map(|line| {
            (!line.starts_with('#'))
                .then(|| line.rsplit('/').next())
                .flatten()
                .and_then(|name| name.strip_suffix(".ts"))
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        segment_names.len() >= 2,
        "proxied HLS playlist must expose two segments for uncached old-generation routing"
    );
    let segment = HlsStreamingApi::get_segment(
        &node_b.infrastructure,
        ROOM,
        MEDIA,
        &publisher_info.generation_id,
        &segment_names[0],
    )
    .await?;
    assert_eq!(segment.len() % 188, 0);
    assert!(
        segment
            .as_chunks::<188>()
            .0
            .iter()
            .all(|packet| packet[0] == 0x47),
        "every proxied MPEG-TS packet must start with the sync byte"
    );

    drop(first_guard);
    drop(second_guard);
    drop(first_flv);
    drop(second_flv);
    publisher.close();
    wait_for_local_endlist(&node_a.infrastructure, &publisher_info.generation_id).await?;
    wait_for_registry_absent(&node_b.infrastructure).await?;
    let ended_playlist =
        wait_for_remote_endlist(&node_b.infrastructure, &publisher_info.generation_id).await?;
    assert!(ended_playlist.contains("#EXT-X-ENDLIST"));

    let mut replacement = RtmpPublisher::connect(rtmp_address_b, ROOM, MEDIA).await?;
    let replacement_info = wait_for_publisher(&node_b.infrastructure).await?;
    assert!(
        replacement_info.lease_epoch > publisher_info.lease_epoch,
        "same-key replacement must receive a newer fencing lease_epoch"
    );
    assert_eq!(replacement_info.node_id, "node-b");
    let replacement_master_generation = HlsStreamingApi::resolve_active_generation_with_pull(
        &node_b.infrastructure,
        ROOM,
        MEDIA,
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("HLS master should resolve the replacement generation"))?;
    assert_eq!(
        replacement_master_generation.generation_id,
        replacement_info.generation_id
    );
    assert_ne!(
        replacement_master_generation.generation_id,
        publisher_info.generation_id
    );
    replacement.send_video(0, true).await?;
    replacement.send_audio(0).await?;
    replacement.send_video(1, true).await?;
    replacement.send_audio(1).await?;
    replacement.send_video(5_001, false).await?;
    replacement.send_audio(5_001).await?;
    replacement.send_video(10_001, true).await?;
    replacement.send_audio(10_001).await?;

    let replacement_playlist =
        wait_for_remote_playlist(&node_b.infrastructure, &replacement_info.generation_id).await?;
    assert!(replacement_playlist.contains(&replacement_info.generation_id));
    assert!(!replacement_playlist.contains(&publisher_info.generation_id));

    let old_uncached_segment = HlsStreamingApi::get_segment(
        &node_b.infrastructure,
        ROOM,
        MEDIA,
        &publisher_info.generation_id,
        &segment_names[1],
    )
    .await?;
    assert_eq!(old_uncached_segment.len() % 188, 0);
    assert!(
        old_uncached_segment
            .as_chunks::<188>()
            .0
            .iter()
            .all(|packet| packet[0] == 0x47),
        "old generation segment must still come from node A after node B takes ownership"
    );

    replacement.close();
    wait_for_registry_absent(&node_b.infrastructure).await?;

    assert!(node_b.shutdown_graceful(3).await);
    relay_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(3), relay_task)
        .await
        .context("relay server did not shut down")???;
    assert!(node_a.shutdown_graceful(3).await);
    Ok(())
}

#[derive(Clone)]
struct SharedStorageConfig {
    backend: HlsStorageBackend,
    path: String,
    s3: HlsS3Options,
}

impl SharedStorageConfig {
    fn apply(&self, config: &mut LivestreamConfig) {
        config.hls_storage_backend = self.backend;
        config.hls_storage_path.clone_from(&self.path);
        config.hls_s3 = self.s3.clone();
    }
}

fn playlist_segment_name(playlist: &str) -> Result<&str> {
    playlist
        .lines()
        .find_map(|line| {
            (!line.starts_with('#'))
                .then(|| line.rsplit('/').next())
                .flatten()
                .and_then(|name| name.strip_suffix(".ts"))
        })
        .context("proxied HLS playlist lacked a TS segment")
}

fn assert_mpeg_ts(segment: &Bytes) {
    assert_eq!(segment.len() % 188, 0);
    assert!(
        segment
            .as_chunks::<188>()
            .0
            .iter()
            .all(|packet| packet[0] == 0x47),
        "every shared MPEG-TS packet must start with the sync byte"
    );
}

async fn exercise_shared_storage_cluster(
    label: &str,
    storage_config: SharedStorageConfig,
    neighbor_storage: Arc<dyn HlsStorage>,
) -> Result<()> {
    let (_redis, _client, manager) =
        synctv_core_testing::start_redis_client_manager_with_label(label).await;
    let prefix = synctv_core_testing::test_redis_key_prefix(label);
    let registry_a = shared_registry(manager.clone(), &prefix);
    let registry_b = shared_registry(manager, &prefix);

    let rtmp_listener_a = TcpListener::bind("127.0.0.1:0").await?;
    let rtmp_address_a = rtmp_listener_a.local_addr()?;
    let rtmp_listener_b = TcpListener::bind("127.0.0.1:0").await?;
    let rtmp_address_b = rtmp_listener_b.local_addr()?;
    let relay_listener_a = TcpListener::bind("127.0.0.1:0").await?;
    let relay_address_a = relay_listener_a.local_addr()?;
    let relay_listener_b = TcpListener::bind("127.0.0.1:0").await?;
    let relay_address_b = relay_listener_b.local_addr()?;

    let auth: Arc<dyn AuthCallback> = Arc::new(RegistryAuth {
        registry: Arc::clone(&registry_a),
        node_id: "node-a".to_string(),
        cluster_address: relay_address_a.to_string(),
    });
    let mut config_a = config("node-a", rtmp_address_a, relay_address_a);
    storage_config.apply(&mut config_a);
    let mut node_a = LivestreamServer::new(config_a, registry_a, Arc::new(StreamTracker::new()))
        .with_auth(auth)
        .with_rtmp_listener(rtmp_listener_a)
        .start()?;

    let mut config_b = config("node-b", rtmp_address_b, relay_address_b);
    storage_config.apply(&mut config_b);
    let mut node_b = LivestreamServer::new(config_b, registry_b, Arc::new(StreamTracker::new()))
        .with_rtmp_listener(rtmp_listener_b)
        .start()?;

    let relay_cancel = CancellationToken::new();
    let relay_service = StreamRelayServiceServer::new(node_a.infrastructure.relay_service(
        "node-a".to_string(),
        CLUSTER_SECRET.to_string(),
        relay_cancel.clone(),
    ))
    .accept_compressed(CompressionEncoding::Gzip)
    .send_compressed(CompressionEncoding::Gzip);
    let relay_cancel_for_server = relay_cancel.clone();
    let relay_task = tokio::spawn(async move {
        Server::builder()
            .add_service(relay_service)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(relay_listener_a),
                relay_cancel_for_server.cancelled_owned(),
            )
            .await
    });

    let mut publisher = RtmpPublisher::connect(rtmp_address_a, ROOM, MEDIA).await?;
    let publisher_info = wait_for_publisher(&node_b.infrastructure).await?;
    assert_eq!(publisher_info.node_id, "node-a");

    tokio::time::sleep(Duration::from_millis(100)).await;
    publisher.send_video(0, true).await?;
    publisher.send_audio(0).await?;
    publisher.send_video(1, true).await?;
    publisher.send_audio(1).await?;
    publisher.send_video(5_001, false).await?;
    publisher.send_audio(5_001).await?;
    publisher.send_video(10_001, true).await?;
    publisher.send_audio(10_001).await?;
    publisher.send_video(15_001, false).await?;
    publisher.send_audio(15_001).await?;
    publisher.send_video(20_001, true).await?;
    publisher.send_audio(20_001).await?;

    let playlist =
        wait_for_remote_playlist(&node_b.infrastructure, &publisher_info.generation_id).await?;
    assert!(playlist.contains("#EXTINF:"));
    assert!(node_b
        .infrastructure
        .hls_stream_registry
        .as_ref()
        .is_none_or(|registry| registry.is_empty()));
    let segment_name = playlist_segment_name(&playlist)?.to_string();

    neighbor_storage
        .write(
            ROOM,
            MEDIA,
            &segment_name,
            Bytes::from_static(b"neighbor-prefix"),
        )
        .await?;

    relay_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(3), relay_task)
        .await
        .context("publisher relay did not shut down")???;

    let segment = HlsStreamingApi::get_segment(
        &node_b.infrastructure,
        ROOM,
        MEDIA,
        &publisher_info.generation_id,
        &segment_name,
    )
    .await?;
    assert_mpeg_ts(&segment);
    assert_eq!(
        neighbor_storage.read(ROOM, MEDIA, &segment_name).await?,
        Bytes::from_static(b"neighbor-prefix")
    );

    publisher.close();
    wait_for_local_endlist(&node_a.infrastructure, &publisher_info.generation_id).await?;
    wait_for_registry_absent(&node_b.infrastructure).await?;

    drop(relay_listener_b);
    assert!(node_b.shutdown_graceful(3).await);
    assert!(node_a.shutdown_graceful(3).await);
    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker Redis (testcontainers)"]
async fn two_nodes_proxy_playlist_and_read_shared_file_segment_directly() -> Result<()> {
    let shared = tempfile::tempdir()?;
    let neighbor = tempfile::tempdir()?;
    let neighbor_storage: Arc<dyn HlsStorage> = Arc::new(FileStorage::new(neighbor.path()));
    exercise_shared_storage_cluster(
        "livestream-cluster-shared-file-e2e",
        SharedStorageConfig {
            backend: HlsStorageBackend::SharedFile,
            path: shared.path().to_string_lossy().into_owned(),
            s3: HlsS3Options::default(),
        },
        neighbor_storage,
    )
    .await
}

#[tokio::test]
#[ignore = "Requires Docker Redis and RustFS (testcontainers)"]
async fn two_nodes_proxy_playlist_and_read_rustfs_segment_directly() -> Result<()> {
    let (_rustfs, s3) = synctv_core_testing::start_rustfs().await;
    let base_path = synctv_core_testing::test_rustfs_base_path("livestream-cluster-s3-e2e");
    let neighbor_base_path =
        synctv_core_testing::test_rustfs_base_path("livestream-cluster-s3-neighbor");
    let neighbor_storage: Arc<dyn HlsStorage> = Arc::new(S3Storage::new(S3Config {
        endpoint: s3.endpoint.clone(),
        access_key_id: s3.access_key_id.clone(),
        secret_access_key: s3.secret_access_key.clone(),
        bucket: s3.bucket.clone(),
        region: Some(s3.region.clone()),
        base_path: neighbor_base_path,
        public_url_prefix: String::new(),
        presign_expires_in: 60,
    })?);

    exercise_shared_storage_cluster(
        "livestream-cluster-s3-e2e",
        SharedStorageConfig {
            backend: HlsStorageBackend::S3,
            path: String::new(),
            s3: HlsS3Options {
                endpoint: s3.endpoint,
                access_key_id: s3.access_key_id,
                secret_access_key: s3.secret_access_key,
                bucket: s3.bucket,
                region: Some(s3.region),
                base_path,
            },
        },
        neighbor_storage,
    )
    .await
}
