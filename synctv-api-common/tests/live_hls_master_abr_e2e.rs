//! End-to-end coverage for the live HLS master contract: the master
//! playlist carries the quality ladder as variants, warms the best
//! eligible transcode rung on first fetch, and signs each variant playlist
//! URL for exactly its rung. Requires a host `ffmpeg` binary.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use synctv_api_common::api_runtime::LivestreamRuntimeSettings;
use synctv_api_common::playback_provider::common::{
    get_live_hls_master_chunks, LiveHlsMasterChunksRequest, LivePlaybackDeps,
};
use synctv_api_common::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt};
use synctv_common::ssrf::SsrfGuard;
use synctv_livestream::{
    local_stream_registry, FfmpegWorkerConfig, HlsS3Options, HlsStorageBackend, HlsStreamingApi,
    LivestreamConfig, LivestreamServer, StreamRegistryTrait, StreamTracker, TranscodeConfig,
    TranscodePreset, WorkerConfig, WorkerEngine,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};
use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite, RtmpStreamMode};

const RUNG: &str = "720p";

struct RegistryAuth {
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
}

#[async_trait]
impl AuthCallback for RegistryAuth {
    async fn on_publish(
        &self,
        generation_id: synctv_xiu::streamhub::utils::Uuid,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        let registered = self
            .registry
            .try_activate_generation(
                app_name,
                stream_name,
                &self.node_id,
                "abr-e2e-user",
                "127.0.0.1:0",
                &generation_id.to_string(),
            )
            .await?;
        if !registered {
            return Err(anyhow::anyhow!("test publisher registration already exists").into());
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
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

fn config(rtmp_address: std::net::SocketAddr) -> LivestreamConfig {
    LivestreamConfig {
        rtmp_address: rtmp_address.to_string(),
        gop_cache_size: 1024 * 1024,
        node_id: "abr-e2e-node".to_string(),
        cleanup_check_interval_seconds: 1,
        stream_timeout_seconds: 30,
        distributed_enabled: false,
        cluster_secret: None,
        grpc_max_message_size_bytes: 16 * 1024 * 1024,
        grpc_compression_enabled: true,
        gop_cache_max_memory_mb: 8,
        max_flv_tag_size_bytes: 10 * 1024 * 1024,
        cluster_address: "127.0.0.1:0".to_string(),
        hls_memory_max_mb: 64,
        hls_storage_backend: HlsStorageBackend::Memory,
        hls_storage_path: String::new(),
        hls_s3: HlsS3Options::default(),
        ssrf_guard: SsrfGuard::strict_policy(),
        webrtc_session: synctv_livestream::WebRtcSessionConfig::default(),
        transcode: TranscodeConfig {
            enabled: true,
            workers: vec![WorkerConfig {
                name: None,
                priority: 100,
                engine: WorkerEngine::Ffmpeg(FfmpegWorkerConfig {
                    max_concurrent: 1,
                    binary: "ffmpeg".to_string(),
                }),
            }],
            presets: vec![TranscodePreset {
                name: RUNG.to_string(),
                height: 720,
                video_kbps: 2500,
                audio_kbps: 96,
                max_fps: 30,
            }],
        },
    }
}

#[tokio::test]
async fn hls_master_warms_and_lists_transcode_rung_variants() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
        "this test requires a host ffmpeg binary"
    );

    let room_id = synctv_core::models::RoomId::new();
    let media_id = synctv_core::models::MediaId::new();
    let room = room_id.to_string();
    let media = media_id.to_string();
    let registry = local_stream_registry();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let rtmp_address = listener.local_addr()?;
    let auth: Arc<dyn AuthCallback> = Arc::new(RegistryAuth {
        registry: Arc::clone(&registry),
        node_id: "abr-e2e-node".to_string(),
    });
    let mut server = LivestreamServer::new(
        config(rtmp_address),
        registry,
        Arc::new(StreamTracker::new()),
    )
    .with_auth(auth)
    .with_rtmp_listener(listener)
    .start()?;
    let infrastructure = Arc::clone(&server.infrastructure);

    // A 1080p source leaves room below it for the 720p preset.
    let mut publisher = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-profile:v",
            "baseline",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-f",
            "flv",
            &format!("rtmp://{rtmp_address}/{room}/{media}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if infrastructure
            .find_publisher(&room, &media)
            .await?
            .is_some()
        {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "source publisher did not register"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let signing_key =
        ProxySigningKey::try_derive_from(b"abr-e2e-secret").expect("derive signing key");
    let connection_manager = ConnectionManager::new(ConnectionLimits::default());
    let livestream_config = LivestreamRuntimeSettings {
        rtmp_port: rtmp_address.port(),
        public_rtmp_host: String::new(),
        public_webrtc_base_url: String::new(),
        flv_max_connection_duration_seconds: 86400,
        flv_write_timeout_seconds: 30,
    };
    let deps = LivePlaybackDeps {
        proxy_signing_key: &signing_key,
        live_streaming_infrastructure: Some(&infrastructure),
        connection_runtime: &connection_manager,
        livestream_config: &livestream_config,
        runtime_settings_store: None,
    };
    let expires_at = synctv_core::SystemClock.now().timestamp() + 1800;
    let request = LiveHlsMasterChunksRequest {
        provider_name: synctv_core::provider::RtmpProvider::NAME.to_string(),
        room_id,
        media_id,
        version: "live-v1".to_string(),
        signature_user_id: "user-1".to_string(),
        signature_room_id: "room-1".to_string(),
        signature_expires_at: expires_at,
        route_provider: "live".to_string(),
        external_source: None,
    };

    // The first master fetch itself warms the rung and answers with the
    // full ladder: the source variant plus the live transcode rung.
    let master_playlist = tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = get_live_hls_master_chunks(deps, request)
            .await
            .map_err(|error| anyhow::anyhow!("master request failed: {error:?}"))?;
        let chunk = stream
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("master playlist stream ended without a chunk"))
            .and_then(|chunk| chunk.map_err(|error| anyhow::anyhow!("master chunk: {error:?}")))?;
        Ok::<_, anyhow::Error>(String::from_utf8(chunk.data.to_vec())?)
    })
    .await??;

    let stream_infs = master_playlist
        .lines()
        .filter(|line| line.starts_with("#EXT-X-STREAM-INF"))
        .collect::<Vec<_>>();
    assert_eq!(
        stream_infs.len(),
        2,
        "master must list the source and the {RUNG} rung, got:\n{master_playlist}"
    );
    assert!(
        stream_infs[0].starts_with("#EXT-X-STREAM-INF:BANDWIDTH=8000000"),
        "the source variant leads the ladder: {}",
        stream_infs[0]
    );
    assert_eq!(
        stream_infs[1],
        format!("#EXT-X-STREAM-INF:BANDWIDTH=2596000,NAME=\"{RUNG}\""),
        "the rung variant carries the preset bandwidth and name"
    );

    let urls = master_playlist
        .lines()
        .filter(|line| line.starts_with("/api/playback-providers/"))
        .collect::<Vec<_>>();
    assert_eq!(urls.len(), 2);
    let rung_url = urls[1];
    let (rung_path, rung_query) = rung_url.split_once('?').unwrap();
    assert!(
        rung_path.ends_with(&format!("/{RUNG}/index.m3u8")),
        "rung variant must point at the rung playlist: {rung_url}"
    );
    let generation_id = rung_path
        .trim_end_matches(&format!("/{RUNG}/index.m3u8"))
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    // The variant URL is signed for exactly the rung playlist resource:
    // neither the source playlist nor another rung verifies with it.
    let rung_resource = format!("hls/{generation_id}/{RUNG}/index.m3u8");
    signing_key
        .parse_and_verify_playback_query(
            rung_query,
            synctv_core::provider::RtmpProvider::NAME,
            "live-v1",
            &rung_resource,
            "room-1",
        )
        .expect("rung variant URL must carry a valid rung-scoped signature");
    assert!(
        signing_key
            .parse_and_verify_playback_query(
                rung_query,
                synctv_core::provider::RtmpProvider::NAME,
                "live-v1",
                &format!("hls/{generation_id}/index.m3u8"),
                "room-1",
            )
            .is_err(),
        "the source playlist resource must not verify a rung signature"
    );

    // The advertised rung actually plays: its media playlist produces
    // segments under the rung stream key.
    let rung_media = synctv_livestream::rung_stream_name(&media, RUNG);
    let playlist_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let playlist = loop {
        let playlist = HlsStreamingApi::generate_playlist_simple(
            &infrastructure,
            &room,
            &rung_media,
            &generation_id,
            "/segments/",
        )
        .await?;
        if let Some(playlist) = playlist {
            if playlist.contains("#EXTINF:") {
                break playlist;
            }
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < playlist_deadline,
            "rung media playlist did not gain segments"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(playlist.contains("#EXTINF:"));

    let _ = publisher.start_kill();
    let _ = publisher.wait().await;
    let absent_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let source_gone = infrastructure
            .find_publisher(&room, &media)
            .await?
            .is_none();
        let rung_gone = infrastructure
            .find_publisher(&room, &rung_media)
            .await?
            .is_none();
        if source_gone && rung_gone {
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < absent_deadline,
            "source and rung must leave the registry after the publisher ends"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(server.shutdown_graceful(3).await);
    Ok(())
}
