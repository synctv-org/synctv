use std::sync::Arc;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CreateFileUploadSession, FileBlob, FileOwnershipProofRange, FileReferenceTarget,
        FileUploadSession, NewStoredFile, UserId,
    },
    repository::FileStorageRepository,
    Error, Result,
};

mod database;
mod routing;
mod s3;
mod validation;
pub use routing::{FileStorageBackendRegistry, RoutedFileStorageService};
#[cfg(test)]
use s3::presigned_upload_headers;
pub use s3::{S3CompatibleFileStorageService, S3FileStorageConfig};
pub(crate) use validation::validate_create_file_upload_session;
use validation::{
    strip_internal_file_metadata, validate_file_mime_type, validate_s3_file_storage_config,
    validate_stored_files,
};

pub(super) const FILE_UPLOAD_EXPIRES_SECONDS: i64 = 900;
pub(crate) const FILE_UPLOAD_TOKEN_KEY: &str = "_synctv_upload_token";
pub(crate) const FILE_OWNERSHIP_PROOF_KEY: &str = "_synctv_ownership_proof";
const FILE_OWNERSHIP_PROOF_ALGORITHM: &str = "synctv-file-ownership-proof-v1";
const FILE_OWNERSHIP_PROOF_RANGE_COUNT: usize = 3;
const FILE_OWNERSHIP_PROOF_RANGE_BYTES: i32 = 1024;
const FILE_UPLOAD_TOKEN_VERSION: &str = "v1";
pub const FILE_UPLOAD_TOKEN_HEADER: &str = "x-synctv-file-upload-token";
const DATABASE_FILE_READ_TOKEN_VERSION: &str = "v1";
pub(super) const MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES: usize = 20 * 1024 * 1024;

pub(super) fn payload_len_i64(len: usize) -> Result<i64> {
    i64::try_from(len)
        .map_err(|_| Error::InvalidInput("file payload size exceeds i64::MAX".to_string()))
}

#[derive(Debug, Clone, Copy)]
pub struct FileStorageContext<'a> {
    pub user_id: UserId,
    pub storage_scope: &'a str,
    pub client_request_id: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStorageCleanupOrigin {
    ReferenceReleased,
    ReferenceExpired,
    RetentionExpired,
    ReferenceCapExceeded,
    CleanupRetry,
    UnreferencedObject,
}

impl FileStorageCleanupOrigin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReferenceReleased => "reference_released",
            Self::ReferenceExpired => "reference_expired",
            Self::RetentionExpired => "retention_expired",
            Self::ReferenceCapExceeded => "reference_cap_exceeded",
            Self::CleanupRetry => "cleanup_retry",
            Self::UnreferencedObject => "unreferenced_object",
        }
    }
}

#[async_trait::async_trait]
pub trait FileStorageService: Send + Sync {
    fn backend_name(&self) -> &str;

    fn object_url(
        &self,
        _storage_backend: &str,
        _object_key: &str,
        _database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSession>;

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>>;

    async fn delete_files(
        &self,
        _origin: FileStorageCleanupOrigin,
        _files: &[FileReferenceTarget],
    ) -> Result<()> {
        Ok(())
    }

    async fn store_upload_object(
        &self,
        _encoded_object_key: &str,
        _upload_token: &str,
        _content_type: Option<&str>,
        _data: Vec<u8>,
    ) -> Result<FileBlob> {
        Err(Error::InvalidInput(
            "file object upload is not supported by this storage backend".to_string(),
        ))
    }

    async fn get_object(&self, _encoded_object_key: &str, _read_token: &str) -> Result<FileBlob> {
        Err(Error::NotFound("File object not found".to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct DisabledFileStorageService;

#[derive(Clone)]
pub struct DatabaseFileStorageService {
    pub(crate) storage_backend: String,
    pub(crate) repository: Arc<FileStorageRepository>,
    pub(crate) upload_token_secret: String,
}

#[async_trait::async_trait]
impl FileStorageService for DisabledFileStorageService {
    fn backend_name(&self) -> &'static str {
        "disabled"
    }

    async fn create_upload_session(
        &self,
        _request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        Err(Error::InvalidInput("file storage is disabled".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        if files.is_empty() {
            Ok(files)
        } else {
            Err(Error::InvalidInput("file storage is disabled".to_string()))
        }
    }
}

pub(super) fn attach_file_ownership_proof_token(
    file: &mut NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    checksum_sha256: &str,
    size_bytes: i64,
) -> Result<(String, Vec<FileOwnershipProofRange>)> {
    let nonce = synctv_common::snanoid!(32);
    let ranges = file_ownership_proof_ranges(checksum_sha256, &nonce, size_bytes)?;
    attach_file_upload_token(
        file,
        user_id,
        storage_scope,
        expires_at,
        secret,
        Some(checksum_sha256),
        Some((&nonce, &ranges)),
    )?;
    Ok((nonce, ranges))
}

pub(super) fn attach_file_upload_token(
    file: &mut NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    checksum_sha256: Option<&str>,
    ownership_proof: Option<(&str, &[FileOwnershipProofRange])>,
) -> Result<()> {
    let token = file_upload_token(
        file,
        user_id,
        storage_scope,
        expires_at,
        secret,
        checksum_sha256,
        ownership_proof,
    )?;
    let Some(metadata) = file.metadata.as_object_mut() else {
        return Err(Error::InvalidInput(
            "file metadata must be a JSON object".to_string(),
        ));
    };
    metadata.insert(
        FILE_UPLOAD_TOKEN_KEY.to_string(),
        serde_json::Value::String(token),
    );
    Ok(())
}

pub(super) fn validate_file_upload_tokens(
    context: FileStorageContext<'_>,
    files: &[NewStoredFile],
    secret: &str,
) -> Result<()> {
    let now = Utc::now();
    for file in files {
        let token = file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::InvalidInput("file upload session token is required".to_string())
            })?;
        validate_file_upload_token(
            file,
            context.user_id,
            context.storage_scope,
            token,
            now,
            secret,
        )?;
    }
    Ok(())
}

fn file_upload_token(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    checksum_sha256: Option<&str>,
    ownership_proof: Option<(&str, &[FileOwnershipProofRange])>,
) -> Result<String> {
    let payload = file_upload_token_payload(
        file,
        user_id,
        storage_scope,
        expires_at,
        checksum_sha256,
        ownership_proof,
    );
    let payload_bytes = serde_json::to_vec(&payload)?;
    let signature = hex::encode(hmac_sha256(
        file_upload_token_key(user_id, storage_scope, secret).as_bytes(),
        &payload_bytes,
    )?);
    let encoded_payload = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload_bytes,
    );
    Ok(format!(
        "{FILE_UPLOAD_TOKEN_VERSION}.{encoded_payload}.{signature}"
    ))
}

fn validate_file_upload_token(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    token: &str,
    now: DateTime<Utc>,
    secret: &str,
) -> Result<()> {
    let (_version, encoded_payload, signature) = split_versioned_hmac_token(
        token,
        FILE_UPLOAD_TOKEN_VERSION,
        "invalid file upload session token",
    )?;
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput("invalid file upload session token".to_string()))?;
    let expected_signature = hex::encode(hmac_sha256(
        file_upload_token_key(user_id, storage_scope, secret).as_bytes(),
        &payload_bytes,
    )?);
    if !constant_time_eq(signature.as_bytes(), expected_signature.as_bytes()) {
        return Err(Error::InvalidInput(
            "invalid file upload session token".to_string(),
        ));
    }
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput("invalid file upload session token".to_string()))?;
    if payload != file_upload_token_payload_from_file(file, user_id, storage_scope)? {
        return Err(Error::InvalidInput(
            "file upload session token does not match file metadata".to_string(),
        ));
    }
    let expires_at = payload
        .get("expires_at")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file upload session token".to_string()))?;
    if expires_at <= now.timestamp() {
        return Err(Error::InvalidInput(
            "file upload session has expired".to_string(),
        ));
    }
    Ok(())
}

fn file_upload_token_payload(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    checksum_sha256: Option<&str>,
    ownership_proof: Option<(&str, &[FileOwnershipProofRange])>,
) -> serde_json::Value {
    let mut payload = json!({
        "user_id": user_id.as_i64(),
        "storage_scope": storage_scope,
        "file_id": file.id,
        "storage_backend": file.storage_backend,
        "object_key": file.object_key,
        "mime_type": file.mime_type,
        "size_bytes": file.size_bytes,
        "width": file.width,
        "height": file.height,
        "expires_at": expires_at.timestamp(),
    });
    if let Some(checksum_sha256) = checksum_sha256 {
        payload["checksum_sha256"] =
            serde_json::Value::String(checksum_sha256.to_ascii_lowercase());
    }
    if let Some((nonce, ranges)) = ownership_proof {
        payload["ownership_proof_required"] = serde_json::Value::Bool(true);
        payload["ownership_proof_algorithm"] =
            serde_json::Value::String(FILE_OWNERSHIP_PROOF_ALGORITHM.to_string());
        payload["ownership_proof_nonce"] = serde_json::Value::String(nonce.to_string());
        payload["ownership_proof_ranges"] = ownership_proof_ranges_to_json(ranges);
    }
    payload
}

pub(super) fn file_upload_token_payload_from_file(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
) -> Result<serde_json::Value> {
    let token = file
        .metadata
        .get(FILE_UPLOAD_TOKEN_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::InvalidInput("file upload session token is required".to_string()))?;
    let (_version, encoded_payload, _signature) = split_versioned_hmac_token(
        token,
        FILE_UPLOAD_TOKEN_VERSION,
        "invalid file upload session token",
    )?;
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput("invalid file upload session token".to_string()))?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput("invalid file upload session token".to_string()))?;
    let expires_at = payload
        .get("expires_at")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file upload session token".to_string()))?;
    let checksum_sha256 = payload
        .get("checksum_sha256")
        .and_then(serde_json::Value::as_str);
    let ownership_proof = if optional_payload_bool(
        &payload,
        "ownership_proof_required",
        "file upload session token",
    )? {
        let nonce = payload
            .get("ownership_proof_nonce")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidInput("invalid file upload session token".to_string()))?;
        let ranges = ownership_proof_ranges_from_payload(&payload)?;
        Some((nonce, ranges))
    } else {
        None
    };
    Ok(file_upload_token_payload(
        file,
        user_id,
        storage_scope,
        DateTime::from_timestamp(expires_at, 0)
            .ok_or_else(|| Error::InvalidInput("invalid file upload session token".to_string()))?,
        checksum_sha256,
        ownership_proof
            .as_ref()
            .map(|(nonce, ranges)| (*nonce, ranges.as_slice())),
    ))
}

pub(super) fn optional_payload_bool(
    payload: &serde_json::Value,
    key: &'static str,
    token_name: &'static str,
) -> Result<bool> {
    match payload.get(key) {
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Error::InvalidInput(format!(
            "invalid {token_name}: {key} must be a boolean"
        ))),
        None => Ok(false),
    }
}

fn file_upload_token_key(user_id: UserId, storage_scope: &str, secret: &str) -> String {
    format!(
        "synctv:file-upload:{}:{}:{}",
        user_id.as_i64(),
        storage_scope,
        secret
    )
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

pub(super) fn file_id_from_request(client_file_id: Option<&str>) -> String {
    client_file_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || format!("img_{}", synctv_common::snanoid!(16)),
            ToOwned::to_owned,
        )
}

pub(super) fn server_file_object_id() -> String {
    format!("obj_{}", synctv_common::snanoid!(24))
}

pub(super) fn normalized_checksum_sha256(checksum: Option<&str>) -> Option<String> {
    checksum.map(|value| value.trim().to_ascii_lowercase())
}

fn file_ownership_proof_ranges(
    checksum_sha256: &str,
    nonce: &str,
    size_bytes: i64,
) -> Result<Vec<FileOwnershipProofRange>> {
    if size_bytes <= 0 {
        return Ok(Vec::new());
    }
    let range_len = FILE_OWNERSHIP_PROOF_RANGE_BYTES
        .min(i32::try_from(size_bytes).unwrap_or(FILE_OWNERSHIP_PROOF_RANGE_BYTES));
    if size_bytes <= i64::from(range_len) {
        return Ok(vec![FileOwnershipProofRange {
            offset: 0,
            length: range_len,
        }]);
    }

    let seed = Sha256::digest(format!("{checksum_sha256}:{nonce}").as_bytes());
    let max_start = size_bytes - i64::from(range_len);
    let max_start = u64::try_from(max_start)
        .map_err(|_| Error::Internal("ownership proof max offset is negative".to_string()))?;
    let modulo = max_start
        .checked_add(1)
        .ok_or_else(|| Error::Internal("ownership proof offset range overflow".to_string()))?;
    let mut ranges = Vec::with_capacity(FILE_OWNERSHIP_PROOF_RANGE_COUNT);
    for index in 0..FILE_OWNERSHIP_PROOF_RANGE_COUNT {
        let start = index * 8;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&seed[start..start + 8]);
        let offset = i64::try_from(u64::from_be_bytes(bytes) % modulo)
            .map_err(|_| Error::Internal("ownership proof offset exceeds i64::MAX".to_string()))?;
        ranges.push(FileOwnershipProofRange {
            offset,
            length: range_len,
        });
    }
    ranges.sort_by_key(|range| range.offset);
    ranges.dedup_by_key(|range| range.offset);
    Ok(ranges)
}

fn ownership_proof_ranges_to_json(ranges: &[FileOwnershipProofRange]) -> serde_json::Value {
    serde_json::Value::Array(
        ranges
            .iter()
            .map(|range| {
                json!({
                    "offset": range.offset,
                    "length": range.length,
                })
            })
            .collect(),
    )
}

pub(super) fn ownership_proof_ranges_from_payload(
    payload: &serde_json::Value,
) -> Result<Vec<FileOwnershipProofRange>> {
    let ranges = payload
        .get("ownership_proof_ranges")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    ranges
        .iter()
        .map(|range| {
            let offset = range
                .get("offset")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
            let length = range
                .get("length")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
            let length = i32::try_from(length)
                .map_err(|_| Error::InvalidInput("invalid file upload token".to_string()))?;
            Ok(FileOwnershipProofRange { offset, length })
        })
        .collect()
}

pub(crate) fn file_ownership_proof_digest<'a, I>(
    nonce: &str,
    ranges: &[FileOwnershipProofRange],
    chunks: I,
) -> String
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut hasher = Sha256::new();
    hasher.update(FILE_OWNERSHIP_PROOF_ALGORITHM.as_bytes());
    hasher.update([0]);
    hasher.update(nonce.as_bytes());
    for (range, chunk) in ranges.iter().zip(chunks) {
        hasher.update(range.offset.to_be_bytes());
        hasher.update(range.length.to_be_bytes());
        hasher.update(chunk);
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn ownership_proof_chunks_from_bytes(
    data: &[u8],
    ranges: &[FileOwnershipProofRange],
) -> Result<Vec<Vec<u8>>> {
    ranges
        .iter()
        .map(|range| {
            if range.offset < 0 || range.length <= 0 {
                return Err(Error::InvalidInput(
                    "invalid file ownership proof range".to_string(),
                ));
            }
            let start = usize::try_from(range.offset).map_err(|_| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            let len = usize::try_from(range.length).map_err(|_| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            let end = start.checked_add(len).ok_or_else(|| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            let chunk = data.get(start..end).ok_or_else(|| {
                Error::InvalidInput("invalid file ownership proof range".to_string())
            })?;
            Ok(chunk.to_vec())
        })
        .collect()
}

pub(crate) fn file_content_object_key(base_path: &str, checksum_sha256: &str) -> String {
    let checksum = checksum_sha256.trim().to_ascii_lowercase();
    let prefix = base_path.trim().trim_matches('/');
    let key = format!(
        "sha256/{}/{}/{}",
        &checksum[0..2],
        &checksum[2..4],
        checksum
    );
    if prefix.is_empty() {
        key
    } else {
        format!("{prefix}/{key}")
    }
}

pub(super) fn database_file_namespace_base_path(storage_namespace: &str) -> String {
    let namespace = storage_namespace.trim().trim_matches('/');
    if namespace.is_empty() {
        "database".to_string()
    } else {
        format!("database/{namespace}")
    }
}

pub(crate) fn file_storage_object_base_path(config_base_path: &str, policy_prefix: &str) -> String {
    let config_base_path = config_base_path.trim().trim_matches('/');
    let policy_prefix = policy_prefix.trim().trim_matches('/');
    match (config_base_path.is_empty(), policy_prefix.is_empty()) {
        (true, true) => String::new(),
        (true, false) => policy_prefix.to_string(),
        (false, true) => config_base_path.to_string(),
        (false, false) => format!("{config_base_path}/{policy_prefix}"),
    }
}

pub(super) fn file_object_key(
    base_path: &str,
    storage_scope: &str,
    file_id: &str,
    mime_type: &str,
) -> String {
    let extension = match mime_type.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/avif" => "avif",
        "image/webp" => "webp",
        _ => "bin",
    };
    let prefix = base_path.trim().trim_matches('/');
    let scope = storage_scope.trim().trim_matches('/');
    let key = if scope.is_empty() {
        format!("{file_id}.{extension}")
    } else {
        format!("{scope}/{file_id}.{extension}")
    };
    if prefix.is_empty() {
        key
    } else {
        format!("{prefix}/{key}")
    }
}

pub(crate) fn file_storage_public_url(
    config: &S3FileStorageConfig,
    object_key: &str,
) -> Result<String> {
    let base = config
        .public_base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| Error::InvalidInput("S3 public_base_url is not configured".to_string()))?;
    s3_path_style_url(base, &config.bucket, object_key)
}

pub(crate) fn optional_file_storage_public_url(
    config: &S3FileStorageConfig,
    object_key: &str,
) -> Result<Option<String>> {
    if config
        .public_base_url
        .as_deref()
        .is_none_or(|url| url.trim().is_empty())
    {
        return Ok(None);
    }
    file_storage_public_url(config, object_key).map(Some)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|error| Error::Internal(format!("failed to initialize HMAC-SHA256: {error}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn encode_database_file_object_key(object_key: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        object_key.as_bytes(),
    )
}

pub(super) fn decode_database_file_object_key(encoded: &str) -> Result<String> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, encoded)
        .map_err(|_| Error::InvalidInput("invalid file object key".to_string()))?;
    String::from_utf8(bytes).map_err(|_| Error::InvalidInput("invalid file object key".to_string()))
}

pub(super) fn database_file_object_url(
    route_prefix: &str,
    storage_backend: &str,
    object_key: &str,
    secret: &str,
) -> Result<String> {
    let encoded_key = encode_database_file_object_key(object_key);
    let read_token = database_file_read_token(storage_backend, object_key, secret)?;
    let route_prefix = route_prefix.trim().trim_end_matches('/');
    if !route_prefix.starts_with('/') {
        return Err(Error::InvalidInput(
            "database object route prefix must be absolute".to_string(),
        ));
    }
    Ok(format!("{route_prefix}/{encoded_key}?token={read_token}"))
}

fn database_file_read_token(
    storage_backend: &str,
    object_key: &str,
    secret: &str,
) -> Result<String> {
    let payload = json!({ "storage_backend": storage_backend, "object_key": object_key });
    let payload_bytes = serde_json::to_vec(&payload)?;
    let signature = hex::encode(hmac_sha256(
        format!("synctv:file-read:{secret}").as_bytes(),
        &payload_bytes,
    )?);
    let encoded_payload = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload_bytes,
    );
    Ok(format!(
        "{DATABASE_FILE_READ_TOKEN_VERSION}.{encoded_payload}.{signature}"
    ))
}

pub(super) fn database_file_read_token_storage_backend(token: &str) -> Result<String> {
    decode_versioned_hmac_token_payload(token, DATABASE_FILE_READ_TOKEN_VERSION)?
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid file read token".to_string()))
}

pub(super) fn validate_database_file_read_token(
    storage_backend: &str,
    object_key: &str,
    token: &str,
    secret: &str,
) -> Result<()> {
    let payload = validate_versioned_hmac_token(
        token,
        DATABASE_FILE_READ_TOKEN_VERSION,
        format!("synctv:file-read:{secret}").as_bytes(),
        "invalid file read token",
    )?;
    if payload
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        != Some(storage_backend)
    {
        return Err(Error::InvalidInput("invalid file read token".to_string()));
    }
    if payload
        .get("object_key")
        .and_then(serde_json::Value::as_str)
        != Some(object_key)
    {
        return Err(Error::InvalidInput("invalid file read token".to_string()));
    }
    Ok(())
}

pub(super) fn file_upload_token_storage_backend(token: &str) -> Result<String> {
    decode_versioned_hmac_token_payload(token, FILE_UPLOAD_TOKEN_VERSION)?
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))
}

pub(super) fn validate_database_file_upload_token(
    storage_backend: &str,
    token: &str,
    object_key: &str,
    now: DateTime<Utc>,
    secret: &str,
) -> Result<serde_json::Value> {
    let payload = decode_versioned_hmac_token_payload(token, FILE_UPLOAD_TOKEN_VERSION)?;
    let user_id = payload
        .get("user_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    let storage_scope = payload
        .get("storage_scope")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    let key = file_upload_token_key(
        UserId::try_from(user_id)
            .map_err(|_| Error::InvalidInput("invalid file upload token".to_string()))?,
        storage_scope,
        secret,
    );
    let payload = validate_versioned_hmac_token(
        token,
        FILE_UPLOAD_TOKEN_VERSION,
        key.as_bytes(),
        "invalid file upload token",
    )?;
    if payload
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        != Some(storage_backend)
        || payload
            .get("object_key")
            .and_then(serde_json::Value::as_str)
            != Some(object_key)
    {
        return Err(Error::InvalidInput("invalid file upload token".to_string()));
    }
    let expires_at = payload
        .get("expires_at")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
    if expires_at <= now.timestamp() {
        return Err(Error::InvalidInput(
            "file upload session has expired".to_string(),
        ));
    }
    Ok(payload)
}

fn decode_versioned_hmac_token_payload(
    token: &str,
    expected_version: &str,
) -> Result<serde_json::Value> {
    let (_version, encoded_payload, _signature) =
        split_versioned_hmac_token(token, expected_version, "invalid token")?;
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput("invalid token".to_string()))?;
    serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput("invalid token".to_string()))
}

fn validate_versioned_hmac_token(
    token: &str,
    expected_version: &str,
    key: &[u8],
    error_message: &str,
) -> Result<serde_json::Value> {
    let (_version, encoded_payload, signature) =
        split_versioned_hmac_token(token, expected_version, error_message)?;
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput(error_message.to_string()))?;
    let expected_signature = hex::encode(hmac_sha256(key, &payload_bytes)?);
    if !constant_time_eq(signature.as_bytes(), expected_signature.as_bytes()) {
        return Err(Error::InvalidInput(error_message.to_string()));
    }
    serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput(error_message.to_string()))
}

fn split_versioned_hmac_token<'a>(
    token: &'a str,
    expected_version: &str,
    error_message: &str,
) -> Result<(&'a str, &'a str, &'a str)> {
    let mut parts = token.split('.');
    let Some(version) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(Error::InvalidInput(error_message.to_string()));
    };
    let Some(encoded_payload) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(Error::InvalidInput(error_message.to_string()));
    };
    let Some(signature) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(Error::InvalidInput(error_message.to_string()));
    };
    if version != expected_version || parts.next().is_some() {
        return Err(Error::InvalidInput(error_message.to_string()));
    }
    Ok((version, encoded_payload, signature))
}

pub(super) fn upload_media_type(content_type: &str) -> Result<&str> {
    let media_type = content_type
        .split_once(';')
        .map_or(content_type, |(media_type, _)| media_type)
        .trim();
    if media_type.is_empty() {
        return Err(Error::InvalidInput(
            "file content-type media type is empty".to_string(),
        ));
    }
    Ok(media_type)
}

fn s3_path_style_url(base: &str, bucket: &str, object_key: &str) -> Result<String> {
    let mut url = url::Url::parse(base.trim())
        .map_err(|error| Error::InvalidInput(format!("Invalid S3 public URL base: {error}")))?;
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            Error::InvalidInput("S3 public URL base must be hierarchical".to_string())
        })?;
        segments.push(bucket.trim());
        for segment in object_key.split('/').filter(|segment| !segment.is_empty()) {
            segments.push(segment);
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests;
