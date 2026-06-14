use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncReadExt;

fn cache_header_bincode_config() -> impl bincode::config::Config + Send + Sync + 'static {
    bincode::config::standard()
        .with_little_endian()
        .with_variable_int_encoding()
}

pub(super) fn encode_header(header: &FileEntryHeader) -> anyhow::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(header, cache_header_bincode_config())
        .map_err(|e| anyhow::anyhow!("bincode encode: {e}"))
}

fn decode_header(bytes: &[u8]) -> anyhow::Result<FileEntryHeader> {
    bincode::serde::decode_from_slice(bytes, cache_header_bincode_config())
        .map(|(header, _consumed)| header)
        .map_err(|e| anyhow::anyhow!("bincode decode: {e}"))
}

/// Magic bytes identifying a valid `SyncTV` cache file (version 1).
pub(super) const CACHE_FILE_MAGIC: &[u8; 4] = b"STV\x01";

/// Minimum size of a valid cache file: 4 (magic) + 4 (`header_len`) = 8.
const MIN_FILE_SIZE: u64 = 8;

/// Safety limit: reject cache files with attacker-sized headers.
const MAX_HEADER_LEN: usize = 64 * 1024;
const MAX_HEADER_FUTURE_SKEW_MILLIS: u64 = 24 * 60 * 60 * 1_000;

/// On-disk header written at the start of every cache file.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(super) struct FileEntryHeader {
    pub key: String,
    pub inserted_at_millis: u64,
    pub ttl_secs: u64,
    pub last_accessed_millis: u64,
    pub data_size: u64,
}

/// Read a cache file and return the deserialized header + data body.
pub(super) async fn read_cache_file(path: &PathBuf) -> anyhow::Result<(FileEntryHeader, Bytes)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to open cache file {}: {e}", path.display()))?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read magic from {}: {e}", path.display()))?;
    if &magic != CACHE_FILE_MAGIC {
        return Err(anyhow::anyhow!(
            "Invalid magic in {}: expected {:?}, got {:?}",
            path.display(),
            CACHE_FILE_MAGIC,
            magic
        ));
    }

    let header_len = read_header_len(&mut file).await?;
    let header = read_header(&mut file, header_len).await.map_err(|e| {
        anyhow::anyhow!("Failed to deserialize header from {}: {e}", path.display())
    })?;
    validate_cache_header(path, &header)?;

    let mut data_buf = Vec::new();
    file.read_to_end(&mut data_buf).await?;

    if data_buf.len() as u64 != header.data_size {
        return Err(anyhow::anyhow!(
            "Data size mismatch in {}: header says {} but file has {} bytes",
            path.display(),
            header.data_size,
            data_buf.len()
        ));
    }

    Ok((header, Bytes::from(data_buf)))
}

/// Read only the header from a cache file.
pub(super) async fn read_cache_file_header(path: &PathBuf) -> anyhow::Result<FileEntryHeader> {
    let metadata = fs::metadata(path).await?;
    if metadata.len() < MIN_FILE_SIZE {
        return Err(anyhow::anyhow!(
            "File {} is too small ({} bytes) to be a valid cache file",
            path.display(),
            metadata.len()
        ));
    }

    let mut file = tokio::fs::File::open(path).await?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).await?;
    if &magic != CACHE_FILE_MAGIC {
        return Err(anyhow::anyhow!(
            "Invalid magic in {}: expected {:?}, got {:?}",
            path.display(),
            CACHE_FILE_MAGIC,
            magic
        ));
    }

    let header_len = read_header_len(&mut file).await?;
    let header = read_header(&mut file, header_len).await?;
    validate_cache_header(path, &header)?;
    validate_cache_file_size(path, metadata.len(), header_len, header.data_size)?;
    Ok(header)
}

/// Update the `last_accessed_millis` field in an existing cache file's header.
pub(super) async fn update_file_last_accessed(
    path: &Path,
    last_accessed_millis: u64,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .await?;

    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).await?;
    if &magic != CACHE_FILE_MAGIC {
        return Err(anyhow::anyhow!("Invalid magic"));
    }

    let header_len = read_header_len(&mut file).await?;
    let mut header = read_header(&mut file, header_len).await?;
    header.last_accessed_millis = last_accessed_millis;

    let new_header_buf = encode_header(&header)?;
    if new_header_buf.len() != header_len {
        return Ok(());
    }

    file.seek(std::io::SeekFrom::Start(8)).await?;
    file.write_all(&new_header_buf).await?;
    file.flush().await?;

    Ok(())
}

async fn read_header_len(file: &mut tokio::fs::File) -> anyhow::Result<usize> {
    let mut header_len_buf = [0u8; 4];
    file.read_exact(&mut header_len_buf).await?;
    let header_len = u32::from_le_bytes(header_len_buf) as usize;
    if header_len > MAX_HEADER_LEN {
        return Err(anyhow::anyhow!(
            "Header too large: {header_len} bytes (max {MAX_HEADER_LEN})"
        ));
    }
    Ok(header_len)
}

async fn read_header(
    file: &mut tokio::fs::File,
    header_len: usize,
) -> anyhow::Result<FileEntryHeader> {
    let mut header_buf = vec![0u8; header_len];
    file.read_exact(&mut header_buf).await?;
    decode_header(&header_buf)
}

fn validate_cache_file_size(
    path: &Path,
    file_size: u64,
    header_len: usize,
    data_size: u64,
) -> anyhow::Result<()> {
    let header_len = u64::try_from(header_len)
        .map_err(|_| anyhow::anyhow!("Header length overflow in {}", path.display()))?;
    let expected_size = MIN_FILE_SIZE
        .checked_add(header_len)
        .and_then(|prefix_size| prefix_size.checked_add(data_size))
        .ok_or_else(|| anyhow::anyhow!("Cache file size overflow in {}", path.display()))?;

    anyhow::ensure!(
        file_size == expected_size,
        "Cache file size mismatch in {}: header expects {} bytes but file has {} bytes",
        path.display(),
        expected_size,
        file_size
    );

    Ok(())
}

fn validate_cache_header(path: &Path, header: &FileEntryHeader) -> anyhow::Result<()> {
    let max_allowed_timestamp = millis_since_epoch().saturating_add(MAX_HEADER_FUTURE_SKEW_MILLIS);
    validate_cache_timestamp(
        path,
        "inserted_at_millis",
        header.inserted_at_millis,
        max_allowed_timestamp,
    )?;
    validate_cache_timestamp(
        path,
        "last_accessed_millis",
        header.last_accessed_millis,
        max_allowed_timestamp,
    )?;
    Ok(())
}

fn validate_cache_timestamp(
    path: &Path,
    field: &str,
    value: u64,
    max_allowed_timestamp: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        system_time_from_millis(value).is_some() && value <= max_allowed_timestamp,
        "Invalid {field} in {}: {value}",
        path.display()
    );
    Ok(())
}

/// Return the current time as milliseconds since the Unix epoch.
pub(super) fn millis_since_epoch() -> u64 {
    system_time_to_millis(SystemTime::now())
}

/// Convert a [`SystemTime`] to milliseconds since the Unix epoch.
pub(super) fn system_time_to_millis(t: SystemTime) -> u64 {
    let millis = match t.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis(),
        Err(error) => {
            tracing::warn!(%error, "system time is before Unix epoch");
            0
        }
    };
    millis.try_into().unwrap_or(u64::MAX)
}

pub(super) const fn cache_entry_deadline_millis(inserted_at_millis: u64, ttl_secs: u64) -> u64 {
    inserted_at_millis.saturating_add(ttl_secs.saturating_mul(1_000))
}

pub(super) fn system_time_from_millis(millis: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}
