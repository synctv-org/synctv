use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use tokio::fs;

use super::file_format::{cache_entry_deadline_millis, millis_since_epoch, read_cache_file_header};
use super::file_index::{FileIndex, FileIndexEntry, LoadResult};

pub(super) async fn load_index(
    cache_dir: &Path,
    dir_levels: (usize, usize),
    index: &FileIndex,
    stale_max_age: Duration,
) -> anyhow::Result<LoadResult> {
    let mut result = LoadResult::default();
    let now = millis_since_epoch();
    let stale_max_millis = u64::try_from(stale_max_age.as_millis()).unwrap_or(u64::MAX);

    walk_and_load(
        cache_dir,
        cache_dir,
        dir_levels,
        index,
        now,
        stale_max_millis,
        &mut result,
    )
    .await?;

    Ok(result)
}

fn walk_and_load<'a>(
    cache_dir: &'a Path,
    dir: &'a Path,
    dir_levels: (usize, usize),
    index: &'a FileIndex,
    now: u64,
    stale_max_millis: u64,
    result: &'a mut LoadResult,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut read_dir = match fs::read_dir(dir).await {
            Ok(read_dir) => read_dir,
            Err(e) => {
                tracing::warn!(
                    dir = %dir.display(),
                    "Failed to read cache directory: {e}"
                );
                return Ok(());
            }
        };

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        "Failed to read metadata: {e}"
                    );
                    result.errors += 1;
                    continue;
                }
            };

            if metadata.is_dir() {
                if path.file_name().is_some_and(|name| name == ".tmp") {
                    continue;
                }
                walk_and_load(
                    cache_dir,
                    &path,
                    dir_levels,
                    index,
                    now,
                    stale_max_millis,
                    result,
                )
                .await?;
                continue;
            }

            if metadata.is_file() {
                load_cache_file(
                    cache_dir,
                    dir_levels,
                    path,
                    index,
                    now,
                    stale_max_millis,
                    result,
                )
                .await;
            }
        }

        Ok(())
    })
}

async fn load_cache_file(
    cache_dir: &Path,
    dir_levels: (usize, usize),
    path: PathBuf,
    index: &FileIndex,
    now: u64,
    stale_max_millis: u64,
    result: &mut LoadResult,
) {
    match read_cache_file_header(&path).await {
        Ok(header) => {
            let expected_path = cache_path(cache_dir, dir_levels, &header.key);
            if path != expected_path {
                tracing::warn!(
                    path = %path.display(),
                    expected_path = %expected_path.display(),
                    key = %header.key,
                    "Cache file path does not match header key, deleting"
                );
                if let Err(error) = fs::remove_file(&path).await {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "Failed to delete cache file with mismatched key path"
                    );
                }
                result.errors += 1;
                return;
            }

            let deadline_millis =
                cache_entry_deadline_millis(header.inserted_at_millis, header.ttl_secs);
            let stale_deadline = deadline_millis.saturating_add(stale_max_millis);

            if now > stale_deadline {
                if let Err(e) = fs::remove_file(&path).await {
                    tracing::warn!(
                        path = %path.display(),
                        "Failed to delete stale cache file: {e}"
                    );
                }
                result.deleted += 1;
            } else {
                index.insert(
                    header.key.clone(),
                    FileIndexEntry {
                        path,
                        data_size: header.data_size,
                        inserted_at_millis: header.inserted_at_millis,
                        ttl_secs: header.ttl_secs,
                        last_accessed: AtomicU64::new(header.last_accessed_millis),
                    },
                );
                result.loaded += 1;
                result.total_bytes = result.total_bytes.saturating_add(header.data_size);
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                "Corrupted cache file during index load, deleting: {e}"
            );
            if let Err(error) = fs::remove_file(&path).await {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "Failed to delete corrupted cache file during index load"
                );
            }
            result.errors += 1;
        }
    }
}

fn cache_path(cache_dir: &Path, dir_levels: (usize, usize), key: &str) -> PathBuf {
    let (level1_len, level2_len) = dir_levels;
    let level1 = &key[..level1_len.min(key.len())];
    let level2 = &key[level1_len..((level1_len + level2_len).min(key.len()))];
    cache_dir.join(level1).join(level2).join(key)
}
