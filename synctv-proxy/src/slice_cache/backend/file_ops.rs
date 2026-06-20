use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use tokio::fs;

use super::file_format::{
    cache_entry_deadline_millis, encode_header, millis_since_epoch, system_time_to_millis,
    FileEntryHeader, CACHE_FILE_MAGIC,
};
use super::file_index::FileIndex;
use crate::slice_cache::etag::StoredEntry;

pub(super) struct WrittenFileEntry {
    pub path: PathBuf,
    pub inserted_at_millis: u64,
    pub last_accessed_millis: u64,
    pub ttl_secs: u64,
    pub data_size: u64,
}

pub(super) struct EvictionCandidate {
    pub key: String,
    pub data_size: u64,
}

pub(super) async fn write_entry(
    cache_dir: &Path,
    dir_levels: (usize, usize),
    temp_counter: &AtomicU64,
    key: &str,
    entry: &StoredEntry,
) -> anyhow::Result<WrittenFileEntry> {
    let path = cache_path(cache_dir, dir_levels, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let inserted_at_millis = system_time_to_millis(entry.inserted_at);
    let last_accessed_millis = system_time_to_millis(entry.last_accessed);
    let ttl_secs = entry.ttl.as_secs();
    let data_size = entry.data.len() as u64;

    let header = FileEntryHeader {
        key: key.to_string(),
        inserted_at_millis,
        ttl_secs,
        last_accessed_millis,
        data_size,
    };

    write_atomic(cache_dir, temp_counter, &path, &header, &entry.data).await?;

    Ok(WrittenFileEntry {
        path,
        inserted_at_millis,
        last_accessed_millis,
        ttl_secs,
        data_size,
    })
}

pub(super) async fn cleanup_temp_files(cache_dir: &Path) {
    let tmp_dir = tmp_dir(cache_dir);
    let Ok(mut read_dir) = fs::read_dir(&tmp_dir).await else {
        return;
    };

    let cutoff = SystemTime::now() - Duration::from_mins(5);

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match entry.metadata().await {
            Ok(metadata) => {
                let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
                if modified < cutoff {
                    tracing::debug!(
                        path = %path.display(),
                        "Removing orphaned temp file"
                    );
                    if let Err(error) = fs::remove_file(&path).await {
                        tracing::debug!(
                            path = %path.display(),
                            %error,
                            "failed to remove orphaned slice cache temp file"
                        );
                    }
                }
            }
            Err(_) => {
                if let Err(error) = fs::remove_file(&path).await {
                    tracing::debug!(
                        path = %path.display(),
                        %error,
                        "failed to remove unreadable slice cache temp file"
                    );
                }
            }
        }
    }
}

pub(super) fn lru_candidates(index: &FileIndex) -> Vec<EvictionCandidate> {
    let mut candidates: Vec<(String, u64, u64)> = index
        .entries
        .iter()
        .map(|entry| {
            (
                entry.key().clone(),
                entry.last_accessed.load(Ordering::Relaxed),
                entry.data_size,
            )
        })
        .collect();

    candidates.par_sort_by_key(|(_key, last_accessed, _data_size)| *last_accessed);
    candidates
        .into_iter()
        .map(|(key, _last_accessed, data_size)| EvictionCandidate { key, data_size })
        .collect()
}

pub(super) fn expired_keys(index: &FileIndex) -> Vec<String> {
    let now = millis_since_epoch();
    index
        .entries
        .iter()
        .filter_map(|entry| {
            let deadline_millis =
                cache_entry_deadline_millis(entry.inserted_at_millis, entry.ttl_secs);
            (now > deadline_millis).then(|| entry.key().clone())
        })
        .collect()
}

pub(super) fn cache_path(cache_dir: &Path, dir_levels: (usize, usize), key: &str) -> PathBuf {
    let (level1_len, level2_len) = dir_levels;
    let level1 = &key[..level1_len.min(key.len())];
    let level2 = &key[level1_len..((level1_len + level2_len).min(key.len()))];
    cache_dir.join(level1).join(level2).join(key)
}

fn tmp_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join(".tmp")
}

fn next_temp_name(temp_counter: &AtomicU64) -> String {
    let counter = temp_counter.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("tmp_{pid}_{counter:012}")
}

async fn write_atomic(
    cache_dir: &Path,
    temp_counter: &AtomicU64,
    path: &Path,
    header: &FileEntryHeader,
    data: &[u8],
) -> anyhow::Result<()> {
    let header_bytes = encode_header(header)?;
    let header_len = u32::try_from(header_bytes.len())
        .map_err(|_| anyhow::anyhow!("cache header too large: {}", header_bytes.len()))?;

    let mut file_content = Vec::with_capacity(4 + 4 + header_bytes.len() + data.len());
    file_content.extend_from_slice(CACHE_FILE_MAGIC);
    file_content.extend_from_slice(&header_len.to_le_bytes());
    file_content.extend_from_slice(&header_bytes);
    file_content.extend_from_slice(data);

    let tmp_dir = tmp_dir(cache_dir);
    fs::create_dir_all(&tmp_dir).await?;
    let tmp_path = tmp_dir.join(next_temp_name(temp_counter));

    fs::write(&tmp_path, &file_content)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to write temp file {}: {e}", tmp_path.display()))?;

    if let Err(e) = fs::rename(&tmp_path, path).await {
        if let Err(cleanup_error) = fs::remove_file(&tmp_path).await {
            tracing::warn!(
                path = %tmp_path.display(),
                %cleanup_error,
                "failed to remove slice cache temp file after rename failure"
            );
        }
        return Err(anyhow::anyhow!(
            "Failed to rename {} -> {}: {e}",
            tmp_path.display(),
            path.display()
        ));
    }

    Ok(())
}
