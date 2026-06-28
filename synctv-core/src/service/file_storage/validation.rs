use std::collections::HashSet;

use crate::{
    models::{
        CreateFileUploadSession, FileMetadata, FileUploadPolicy, NewStoredFile, FILE_ID_MAX_CHARS,
        FILE_OBJECT_KEY_MAX_CHARS, FILE_SHA256_HEX_CHARS, FILE_STORAGE_BACKEND_MAX_CHARS,
    },
    service::file_storage::S3FileStorageConfig,
    Error, Result,
};

pub(crate) fn validate_file_metadata(metadata: &FileMetadata) -> Result<()> {
    if metadata.upload_token.is_some() || metadata.ownership_proof.is_some() {
        return Err(Error::InvalidInput(
            "file metadata includes internal fields".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn strip_internal_file_metadata(files: &mut [NewStoredFile]) {
    for file in files {
        file.metadata = file.metadata.public();
    }
}

pub(super) fn validate_stored_files(files: &[NewStoredFile]) -> Result<()> {
    let mut file_ids = HashSet::with_capacity(files.len());
    let mut object_keys = HashSet::with_capacity(files.len());
    for file in files {
        if file.id.trim().is_empty() || file.id.chars().count() > FILE_ID_MAX_CHARS {
            return Err(Error::InvalidInput(format!(
                "file id must be between 1 and {FILE_ID_MAX_CHARS} characters"
            )));
        }
        if file.storage_backend.trim().is_empty()
            || file.storage_backend.chars().count() > FILE_STORAGE_BACKEND_MAX_CHARS
            || file.object_key.trim().is_empty()
            || file.object_key.chars().count() > FILE_OBJECT_KEY_MAX_CHARS
        {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be 1-{FILE_STORAGE_BACKEND_MAX_CHARS} characters and object_key must be 1-{FILE_OBJECT_KEY_MAX_CHARS} characters"
            )));
        }
        if !file_ids.insert(file.id.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate file id in one request".to_string(),
            ));
        }
        if !object_keys.insert(file.object_key.as_str()) {
            return Err(Error::InvalidInput(
                "duplicate file object_key in one request".to_string(),
            ));
        }
        required_file_mime_type(file)?;
        if !matches!(file.size_bytes, Some(size) if size > 0) {
            return Err(Error::InvalidInput(
                "file size_bytes is required and must be positive".to_string(),
            ));
        }
        if file.width.is_some_and(|width| width <= 0)
            || file.height.is_some_and(|height| height <= 0)
        {
            return Err(Error::InvalidInput(
                "file dimensions must be positive".to_string(),
            ));
        }
        validate_file_metadata(&file.metadata)?;
    }
    Ok(())
}

pub(super) fn required_file_mime_type(file: &NewStoredFile) -> Result<&str> {
    let mime_type = file
        .mime_type
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?;
    if mime_type.is_empty() || !mime_type.contains('/') {
        return Err(Error::InvalidInput(
            "file mime_type must be a valid media type".to_string(),
        ));
    }
    Ok(mime_type)
}

pub(crate) fn validate_create_file_upload_session(request: &CreateFileUploadSession) -> Result<()> {
    if let Some(id) = &request.client_file_id {
        let len = id.chars().count();
        if !(1..=128).contains(&len) {
            return Err(Error::InvalidInput(
                "client_file_id must be between 1 and 128 characters".to_string(),
            ));
        }
    }
    if let Some(filename) = &request.filename {
        let len = filename.chars().count();
        if filename.trim().is_empty() || len > 255 || filename.chars().any(char::is_control) {
            return Err(Error::InvalidInput(
                "filename must be between 1 and 255 characters without control characters"
                    .to_string(),
            ));
        }
    }
    validate_file_upload_policy(&request.policy)?;
    validate_file_mime_type(&request.policy, &request.mime_type)?;
    if request.size_bytes <= 0 || request.size_bytes > request.policy.max_size_bytes {
        return Err(Error::InvalidInput(format!(
            "file size must be between 1 and {} bytes",
            request.policy.max_size_bytes
        )));
    }
    if request.width.is_some_and(|width| width <= 0)
        || request.height.is_some_and(|height| height <= 0)
    {
        return Err(Error::InvalidInput(
            "file dimensions must be positive".to_string(),
        ));
    }
    validate_file_dimensions(
        &request.policy,
        &request.mime_type,
        request.width,
        request.height,
    )?;
    validate_file_audio_metadata(
        &request.policy,
        &request.mime_type,
        request.duration_seconds,
        request.bitrate_bps,
    )?;
    for part in &request.parts {
        let valid = part.checksum_sha256.len() == FILE_SHA256_HEX_CHARS
            && part.checksum_sha256.chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Err(Error::InvalidInput(
                "file upload part checksum_sha256 must be a 64-character hex string".to_string(),
            ));
        }
        if part.part_number <= 0 || part.offset_bytes < 0 || part.size_bytes <= 0 {
            return Err(Error::InvalidInput(
                "file upload manifest part number, offset, and size are invalid".to_string(),
            ));
        }
    }
    validate_file_metadata(&request.metadata)?;
    Ok(())
}

pub(super) fn validate_file_upload_policy(policy: &FileUploadPolicy) -> Result<()> {
    if policy.kind.trim().is_empty()
        || policy.max_size_bytes <= 0
        || policy.max_width.is_some_and(|width| width <= 0)
        || policy.max_height.is_some_and(|height| height <= 0)
        || policy
            .max_audio_duration_seconds
            .is_some_and(|duration| duration <= 0)
        || policy
            .max_audio_bitrate_bps
            .is_some_and(|bitrate| bitrate <= 0)
        || policy.storage_namespace.trim().is_empty()
        || policy.database_object_route_prefix.trim().is_empty()
    {
        return Err(Error::InvalidInput(
            "invalid file upload policy".to_string(),
        ));
    }
    if policy.allowed_mime_prefixes.is_empty() && policy.allowed_mime_types.is_empty() {
        return Err(Error::InvalidInput(
            "file upload policy must allow at least one MIME type".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_file_mime_type(policy: &FileUploadPolicy, mime_type: &str) -> Result<()> {
    let normalized = mime_type.trim().to_ascii_lowercase();
    let allowed_exact = policy
        .allowed_mime_types
        .iter()
        .any(|allowed| normalized == allowed.trim().to_ascii_lowercase());
    let allowed_prefix = policy
        .allowed_mime_prefixes
        .iter()
        .any(|prefix| normalized.starts_with(&prefix.trim().to_ascii_lowercase()));
    if allowed_exact || allowed_prefix {
        return Ok(());
    }
    Err(Error::InvalidInput(format!(
        "{} mime_type is not allowed",
        policy.kind
    )))
}

pub(crate) fn validate_file_dimensions(
    policy: &FileUploadPolicy,
    mime_type: &str,
    width: Option<i32>,
    height: Option<i32>,
) -> Result<()> {
    let is_image = mime_type.trim().to_ascii_lowercase().starts_with("image/");
    if !is_image {
        return Ok(());
    }
    if policy.require_image_dimensions && (width.is_none() || height.is_none()) {
        return Err(Error::InvalidInput(format!(
            "{} image dimensions are required",
            policy.kind
        )));
    }
    if let (Some(max_width), Some(width)) = (policy.max_width, width) {
        if width > max_width {
            return Err(Error::InvalidInput(format!(
                "{} image width must be at most {max_width}px",
                policy.kind
            )));
        }
    }
    if let (Some(max_height), Some(height)) = (policy.max_height, height) {
        if height > max_height {
            return Err(Error::InvalidInput(format!(
                "{} image height must be at most {max_height}px",
                policy.kind
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_file_audio_metadata(
    policy: &FileUploadPolicy,
    mime_type: &str,
    duration_seconds: Option<i32>,
    bitrate_bps: Option<i32>,
) -> Result<()> {
    let is_audio = mime_type.trim().to_ascii_lowercase().starts_with("audio/");
    if !is_audio {
        return Ok(());
    }
    if duration_seconds.is_some_and(|duration| duration <= 0)
        || bitrate_bps.is_some_and(|bitrate| bitrate <= 0)
    {
        return Err(Error::InvalidInput(
            "file audio duration and bitrate must be positive".to_string(),
        ));
    }
    if policy.require_audio_metadata && (duration_seconds.is_none() || bitrate_bps.is_none()) {
        return Err(Error::InvalidInput(format!(
            "{} audio metadata is required",
            policy.kind
        )));
    }
    if let (Some(max_duration), Some(duration)) =
        (policy.max_audio_duration_seconds, duration_seconds)
    {
        if duration > max_duration {
            return Err(Error::InvalidInput(format!(
                "{} audio duration must be at most {max_duration} seconds",
                policy.kind
            )));
        }
    }
    if let (Some(max_bitrate), Some(bitrate)) = (policy.max_audio_bitrate_bps, bitrate_bps) {
        if bitrate > max_bitrate {
            return Err(Error::InvalidInput(format!(
                "{} audio bitrate must be at most {max_bitrate} bps",
                policy.kind
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_s3_file_storage_config(config: &S3FileStorageConfig) -> Result<()> {
    if config.endpoint.trim().is_empty()
        || config.access_key_id.trim().is_empty()
        || config.secret_access_key.trim().is_empty()
        || config.bucket.trim().is_empty()
        || config.region.trim().is_empty()
        || config
            .public_base_url
            .as_deref()
            .is_none_or(|url| url.trim().is_empty())
        || config.upload_token_secret.trim().is_empty()
    {
        return Err(Error::InvalidInput(
            "S3 file storage requires endpoint, bucket, region, access_key_id, secret_access_key, public_base_url, and upload_token_secret"
                .to_string(),
        ));
    }
    if config.upload_expires_seconds <= 0 {
        return Err(Error::InvalidInput(
            "S3 file upload_expires_seconds must be positive".to_string(),
        ));
    }
    url::Url::parse(config.endpoint.trim())
        .map_err(|error| Error::InvalidInput(format!("Invalid S3 endpoint: {error}")))?;
    if let Some(public_base_url) = config.public_base_url.as_deref() {
        url::Url::parse(public_base_url.trim())
            .map_err(|error| Error::InvalidInput(format!("Invalid S3 public_base_url: {error}")))?;
    }
    Ok(())
}
