#![cfg(feature = "s3")]
#![allow(clippy::unwrap_used)]
#![cfg_attr(
    not(any(feature = "tls-aws-lc", feature = "tls-ring")),
    allow(dead_code, unused_imports)
)]

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context as _, Result};
use bytes::Bytes;
use synctv_core_testing::{start_rustfs, test_rustfs_base_path};
use synctv_xiu::hls::segment_manager::{CleanupAuthority, CleanupConfig, SegmentManager};
use synctv_xiu::storage::{HlsStorage, S3Config, S3Storage};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

struct TransientFailureProxy {
    endpoint: String,
    request_count: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl TransientFailureProxy {
    async fn start(upstream_endpoint: &str, failures: usize) -> Result<Self> {
        let upstream = url::Url::parse(upstream_endpoint)?;
        let upstream_host = upstream
            .host_str()
            .context("RustFS endpoint must contain a host")?
            .to_string();
        let upstream_port = upstream
            .port_or_known_default()
            .context("RustFS endpoint must contain a port")?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let request_count = Arc::new(AtomicUsize::new(0));
        let proxy_count = Arc::clone(&request_count);
        let task = tokio::spawn(async move {
            loop {
                let Ok((downstream, _)) = listener.accept().await else {
                    break;
                };
                let attempt = proxy_count.fetch_add(1, Ordering::AcqRel) + 1;
                let upstream_host = upstream_host.clone();
                tokio::spawn(async move {
                    if attempt <= failures {
                        respond_unavailable(downstream).await;
                        return;
                    }

                    let Ok(upstream) =
                        TcpStream::connect((upstream_host.as_str(), upstream_port)).await
                    else {
                        return;
                    };
                    proxy_connection(downstream, upstream).await;
                });
            }
        });

        Ok(Self {
            endpoint: format!("http://{address}"),
            request_count,
            task,
        })
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::Acquire)
    }
}

impl Drop for TransientFailureProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond_unavailable(mut stream: TcpStream) {
    let mut request = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    while request.len() < 64 * 1024 && !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let Ok(read) = stream.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let _ = stream
        .write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
}

async fn proxy_connection(mut downstream: TcpStream, mut upstream: TcpStream) {
    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
}

struct TestCleanupAuthority(AtomicBool);

impl TestCleanupAuthority {
    const fn new(enabled: bool) -> Self {
        Self(AtomicBool::new(enabled))
    }

    fn set(&self, enabled: bool) {
        self.0.store(enabled, Ordering::Release);
    }
}

impl CleanupAuthority for TestCleanupAuthority {
    fn should_cleanup(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn storage(s3: &synctv_core_testing::RustfsS3Config, base_path: &str) -> Result<S3Storage> {
    Ok(S3Storage::new(S3Config {
        endpoint: s3.endpoint.clone(),
        access_key_id: s3.access_key_id.clone(),
        secret_access_key: s3.secret_access_key.clone(),
        bucket: s3.bucket.clone(),
        region: Some(s3.region.clone()),
        base_path: base_path.to_string(),
        public_url_prefix: String::new(),
        presign_expires_in: 60,
    })?)
}

fn current_segment(suffix: &str) -> String {
    format!("{}_{}", chrono::Utc::now().timestamp() / 60, suffix)
}

fn expired_segment(suffix: &str) -> String {
    format!(
        "{}_{}",
        (chrono::Utc::now() - chrono::Duration::minutes(10)).timestamp() / 60,
        suffix
    )
}

#[cfg(not(any(feature = "tls-aws-lc", feature = "tls-ring")))]
#[test]
fn s3_reports_missing_crypto_provider_before_starting_transport() {
    let result = S3Storage::new(S3Config {
        endpoint: "https://s3.example.invalid".to_string(),
        access_key_id: "test".to_string(),
        secret_access_key: "test".to_string(),
        bucket: "test".to_string(),
        region: Some("us-east-1".to_string()),
        base_path: "hls".to_string(),
        public_url_prefix: String::new(),
        presign_expires_in: 60,
    });
    assert!(result.is_err());
}

fn wrong_credentials_storage(
    s3: &synctv_core_testing::RustfsS3Config,
    base_path: &str,
) -> Result<S3Storage> {
    Ok(S3Storage::new(S3Config {
        endpoint: s3.endpoint.clone(),
        access_key_id: "invalid-access-key".to_string(),
        secret_access_key: "invalid-secret-key".to_string(),
        bucket: s3.bucket.clone(),
        region: Some(s3.region.clone()),
        base_path: base_path.to_string(),
        public_url_prefix: String::new(),
        presign_expires_in: 60,
    })?)
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_implements_hls_storage_contract_and_preserves_prefix_isolation() -> Result<()> {
    let (_rustfs, s3) = start_rustfs().await;
    let primary = storage(&s3, &test_rustfs_base_path("xiu-s3-primary"))?;
    let neighbor = storage(&s3, &test_rustfs_base_path("xiu-s3-neighbor"))?;
    let app = "room";
    let stream = "media";

    let old_name = current_segment("z_old");
    let new_name = current_segment("a_new");
    primary
        .write(app, stream, &old_name, Bytes::from_static(b"old"))
        .await?;
    // RustFS exposes S3 Last-Modified with second-level interoperability.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    primary
        .write(app, stream, &new_name, Bytes::from_static(b"new"))
        .await?;
    neighbor
        .write(app, stream, &new_name, Bytes::from_static(b"neighbor"))
        .await?;

    let wrong_credentials =
        wrong_credentials_storage(&s3, &test_rustfs_base_path("xiu-s3-primary"))?;
    let auth_error = wrong_credentials
        .read(app, stream, &new_name)
        .await
        .expect_err("invalid S3 credentials must remain an observable backend error");
    assert_ne!(
        auth_error.kind(),
        std::io::ErrorKind::NotFound,
        "authentication failures must not be reported as missing segments"
    );

    assert_eq!(primary.read(app, stream, &new_name).await?, b"new"[..]);
    assert!(primary.exists(app, stream, &old_name).await?);
    assert_eq!(primary.count_stream_segments(app, stream).await?, 2);
    assert_eq!(
        primary.list_streams().await?,
        vec![(app.into(), stream.into())]
    );

    let deleted = primary
        .delete_oldest_stream_segments(app, stream, 1)
        .await?;
    assert_eq!(deleted, 1);
    assert!(!primary.exists(app, stream, &old_name).await?);
    assert!(primary.exists(app, stream, &new_name).await?);
    assert_eq!(
        neighbor.read(app, stream, &new_name).await?,
        b"neighbor"[..]
    );

    let missing = primary
        .read(app, stream, &old_name)
        .await
        .expect_err("deleted object should be reported as missing");
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    let expired_name = expired_segment("expired");
    primary
        .write(app, stream, &expired_name, Bytes::from_static(b"expired"))
        .await?;
    assert_eq!(primary.cleanup(Duration::from_mins(3)).await?, 1);
    assert!(!primary.exists(app, stream, &expired_name).await?);
    assert!(primary.exists(app, stream, &new_name).await?);

    let public_url = primary
        .get_public_url(app, stream, &new_name)
        .await?
        .context("RustFS should support S3 presigned reads")?;
    let response = reqwest::get(public_url).await?.error_for_status()?;
    assert_eq!(response.bytes().await?, b"new"[..]);

    assert_eq!(primary.delete_app_stream(app, stream).await?, 1);
    assert!(!primary.exists(app, stream, &new_name).await?);
    assert_eq!(
        neighbor.read(app, stream, &new_name).await?,
        b"neighbor"[..]
    );
    Ok(())
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_shared_cleanup_runs_only_after_leader_takeover() -> Result<()> {
    let (_rustfs, s3) = start_rustfs().await;
    let shared = Arc::new(storage(
        &s3,
        &test_rustfs_base_path("xiu-s3-shared-cleanup"),
    )?);
    let neighbor = storage(
        &s3,
        &test_rustfs_base_path("xiu-s3-shared-cleanup-neighbor"),
    )?;
    let expired = expired_segment("leader-owned");
    let active = current_segment("active");

    shared
        .write("room", "media", &expired, Bytes::from_static(b"expired"))
        .await?;
    shared
        .write("room", "media", &active, Bytes::from_static(b"active"))
        .await?;
    shared
        .write(
            "room",
            "neighbor-media",
            &active,
            Bytes::from_static(b"same-prefix-neighbor"),
        )
        .await?;
    neighbor
        .write(
            "room",
            "media",
            &expired,
            Bytes::from_static(b"other-prefix"),
        )
        .await?;

    let authority = Arc::new(TestCleanupAuthority::new(false));
    let manager = SegmentManager::new(
        shared.clone(),
        CleanupConfig {
            interval: Duration::from_mins(1),
            retention: Duration::from_mins(3),
            final_playlist_grace: Duration::from_mins(1),
            ended_segment_grace: Duration::from_secs(90),
            max_segments_per_stream: 0,
        },
    )
    .with_cleanup_authority(authority.clone());

    assert_eq!(manager.cleanup_expired().await?, 0);
    assert!(shared.exists("room", "media", &expired).await?);

    authority.set(true);
    assert_eq!(manager.cleanup_expired().await?, 1);
    assert!(!shared.exists("room", "media", &expired).await?);
    assert!(shared.exists("room", "media", &active).await?);
    assert!(shared.exists("room", "neighbor-media", &active).await?);
    assert_eq!(
        neighbor.read("room", "media", &expired).await?,
        b"other-prefix"[..]
    );
    Ok(())
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_lists_streams_in_stable_order_and_trims_only_the_target_stream() -> Result<()> {
    let (_rustfs, s3) = start_rustfs().await;
    let storage = storage(&s3, &test_rustfs_base_path("xiu-s3-list-trim"))?;

    let target_old_bucket = expired_segment("z_old_bucket");
    let target_old_name = current_segment("z_old_name");
    let target_new_name = current_segment("a_new_name");
    storage
        .write(
            "alpha",
            "main",
            &target_old_bucket,
            Bytes::from_static(b"old-bucket"),
        )
        .await?;
    storage
        .write(
            "alpha",
            "main",
            &target_old_name,
            Bytes::from_static(b"old-name"),
        )
        .await?;
    // RustFS reports S3 Last-Modified at second precision. The lexically first
    // name is deliberately newest so trimming must use metadata time.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    storage
        .write(
            "alpha",
            "main",
            &target_new_name,
            Bytes::from_static(b"new-name"),
        )
        .await?;
    storage
        .write(
            "alpha",
            "side",
            &target_new_name,
            Bytes::from_static(b"alpha-side"),
        )
        .await?;
    storage
        .write(
            "zeta",
            "main",
            &target_new_name,
            Bytes::from_static(b"zeta-main"),
        )
        .await?;

    let expected = vec![
        ("alpha".to_string(), "main".to_string()),
        ("alpha".to_string(), "side".to_string()),
        ("zeta".to_string(), "main".to_string()),
    ];
    assert_eq!(storage.list_streams().await?, expected);
    assert_eq!(storage.list_streams().await?, expected);
    assert_eq!(storage.count_stream_segments("alpha", "main").await?, 3);

    assert_eq!(
        storage
            .delete_oldest_stream_segments("alpha", "main", 1)
            .await?,
        2
    );
    assert_eq!(storage.count_stream_segments("alpha", "main").await?, 1);
    assert!(storage.exists("alpha", "main", &target_new_name).await?);
    assert!(storage.exists("alpha", "side", &target_new_name).await?);
    assert!(storage.exists("zeta", "main", &target_new_name).await?);
    Ok(())
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_delete_scopes_exact_app_stream_and_base_path() -> Result<()> {
    let (_rustfs, s3) = start_rustfs().await;
    let primary = storage(&s3, &test_rustfs_base_path("xiu-s3-delete-scope"))?;
    let neighbor = storage(&s3, &test_rustfs_base_path("xiu-s3-delete-scope-neighbor"))?;
    let current = current_segment("current");
    let expired = expired_segment("expired");

    for name in [&current, &expired] {
        primary
            .write("room", "media", name, Bytes::from_static(b"target"))
            .await?;
    }
    primary
        .write(
            "room",
            "media-extra",
            &current,
            Bytes::from_static(b"stream-neighbor"),
        )
        .await?;
    primary
        .write(
            "room-extra",
            "media",
            &current,
            Bytes::from_static(b"app-neighbor"),
        )
        .await?;
    neighbor
        .write(
            "room",
            "media",
            &current,
            Bytes::from_static(b"base-path-neighbor"),
        )
        .await?;

    assert_eq!(primary.delete_app_stream("room", "media").await?, 2);
    assert!(primary.exists("room", "media-extra", &current).await?);
    assert!(primary.exists("room-extra", "media", &current).await?);
    assert!(neighbor.exists("room", "media", &current).await?);

    assert_eq!(primary.delete_app("room").await?, 1);
    assert!(primary.exists("room-extra", "media", &current).await?);
    assert!(neighbor.exists("room", "media", &current).await?);
    assert!(primary
        .list_streams()
        .await?
        .contains(&("room-extra".to_string(), "media".to_string())));
    Ok(())
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_cleanup_failure_is_reported_and_preserves_objects() -> Result<()> {
    let (_rustfs, s3) = start_rustfs().await;
    let base_path = test_rustfs_base_path("xiu-s3-cleanup-failure");
    let valid = storage(&s3, &base_path)?;
    let invalid = wrong_credentials_storage(&s3, &base_path)?;
    let expired = expired_segment("preserved-after-failure");
    valid
        .write("room", "media", &expired, Bytes::from_static(b"preserved"))
        .await?;

    invalid
        .cleanup(Duration::from_mins(3))
        .await
        .expect_err("cleanup authentication failure must reach the caller");
    assert_eq!(
        valid.read("room", "media", &expired).await?,
        b"preserved"[..]
    );

    assert_eq!(valid.cleanup(Duration::from_mins(3)).await?, 1);
    assert!(!valid.exists("room", "media", &expired).await?);
    Ok(())
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_overwrite_exposes_only_complete_object_versions() -> Result<()> {
    const OLD_SIZE: usize = 1024 * 1024;
    const NEW_SIZE: usize = 8 * 1024 * 1024;

    let (_rustfs, s3) = start_rustfs().await;
    let storage = Arc::new(storage(
        &s3,
        &test_rustfs_base_path("xiu-s3-atomic-overwrite"),
    )?);
    let name = current_segment("atomic-overwrite");
    storage
        .write("room", "media", &name, Bytes::from(vec![0x11; OLD_SIZE]))
        .await?;

    let writer_storage = Arc::clone(&storage);
    let writer_name = name.clone();
    let writer = tokio::spawn(async move {
        writer_storage
            .write(
                "room",
                "media",
                &writer_name,
                Bytes::from(vec![0x22; NEW_SIZE]),
            )
            .await
    });

    for _ in 0..64 {
        let observed = storage.read("room", "media", &name).await?;
        match observed.len() {
            OLD_SIZE => assert!(observed.iter().all(|byte| *byte == 0x11)),
            NEW_SIZE => assert!(observed.iter().all(|byte| *byte == 0x22)),
            size => panic!("RustFS exposed a partial object version of {size} bytes"),
        }
        if writer.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    writer.await??;

    let final_version = storage.read("room", "media", &name).await?;
    assert_eq!(final_version.len(), NEW_SIZE);
    assert!(final_version.iter().all(|byte| *byte == 0x22));
    Ok(())
}

#[cfg(any(feature = "tls-aws-lc", feature = "tls-ring"))]
#[tokio::test]
#[ignore = "requires Docker and starts the shared RustFS testcontainer"]
async fn rustfs_write_recovers_after_transient_service_unavailable_responses() -> Result<()> {
    let (_rustfs, s3) = start_rustfs().await;
    let proxy = TransientFailureProxy::start(&s3.endpoint, 2).await?;
    let base_path = test_rustfs_base_path("xiu-s3-real-retry");
    let proxied = S3Storage::new(S3Config {
        endpoint: proxy.endpoint.clone(),
        access_key_id: s3.access_key_id.clone(),
        secret_access_key: s3.secret_access_key.clone(),
        bucket: s3.bucket.clone(),
        region: Some(s3.region.clone()),
        base_path: base_path.clone(),
        public_url_prefix: String::new(),
        presign_expires_in: 60,
    })?;
    let direct = storage(&s3, &base_path)?;
    let name = current_segment("retried-write");

    proxied
        .write(
            "room",
            "media",
            &name,
            Bytes::from_static(b"written-after-retry"),
        )
        .await?;

    assert!(proxy.request_count() >= 3);
    assert_eq!(
        direct.read("room", "media", &name).await?,
        b"written-after-retry"[..]
    );
    Ok(())
}
