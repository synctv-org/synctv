use sha2::{Digest, Sha256};

use super::S3CompatibleFileStorageService;
use crate::{
    models::{StoreFileUpload, StoreFileUploadResult},
    repository::UpsertFileUploadSessionPart,
    service::file_storage::{
        constant_time_eq, decode_file_object_key, file_part_manifest_digest, payload_len_i64,
        upload_manifest_parts_from_metadata, upload_media_type, upload_session_is_multipart,
        upload_session_is_single_object, upload_session_parts_progress, validate_file_upload_token,
        validate_upload_range,
    },
    Error, Result,
};

impl S3CompatibleFileStorageService {
    pub(super) async fn store_s3_upload(
        &self,
        upload: StoreFileUpload,
    ) -> Result<StoreFileUploadResult> {
        let StoreFileUpload {
            encoded_object_key,
            upload_token,
            content_type,
            range,
            data,
        } = upload;
        if data.is_empty() {
            return Err(Error::InvalidInput(
                "file upload part must be non-empty".to_string(),
            ));
        }
        let upload_session_key = decode_file_object_key(&encoded_object_key)?;
        let payload = validate_file_upload_token(
            &self.config.storage_backend,
            &upload_token,
            &upload_session_key,
            crate::SystemClock.now(),
            &self.config.upload_token_secret,
        )?;
        let repository = self.repository()?;
        let session = repository
            .get_upload_session(&self.config.storage_backend, &upload_session_key)
            .await?
            .ok_or_else(|| Error::InvalidInput("file upload session was not found".to_string()))?;
        if session.completed_at.is_some() || session.expires_at <= crate::SystemClock.now() {
            return Err(Error::InvalidInput(
                "file upload session is not active".to_string(),
            ));
        }
        let mime_type = payload
            .mime_type
            .as_deref()
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if mime_type != session.mime_type {
            return Err(Error::InvalidInput(
                "file upload token does not match upload session".to_string(),
            ));
        }
        if let Some(content_type) = content_type.as_deref() {
            if upload_media_type(content_type)? != mime_type {
                return Err(Error::InvalidInput(
                    "file content-type does not match upload session".to_string(),
                ));
            }
        }
        let expected_size = payload
            .size_bytes
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if expected_size != session.size_bytes {
            return Err(Error::InvalidInput(
                "file upload token does not match upload session".to_string(),
            ));
        }
        let actual_part_checksum = hex::encode(Sha256::digest(&data));
        let range = range.unwrap_or(crate::models::FileUploadRange {
            start: 0,
            end_inclusive: payload_len_i64(data.len())? - 1,
            total_size: expected_size,
        });
        // Validate against the persisted session plan. S3 and database
        // multipart sessions share the same resume/idempotency contract: the
        // per-session manifest defines accepted offsets, sizes, and checksums.
        let part_index =
            validate_upload_range(range, data.len(), expected_size, session.part_size_bytes)?;
        let part_number = part_index.checked_add(1).ok_or_else(|| {
            Error::InvalidInput("file upload part number is too large".to_string())
        })?;
        let manifest_parts = upload_manifest_parts_from_metadata(&session.metadata)?;
        let expected_part = manifest_parts
            .iter()
            .find(|part| part.part_number == part_number)
            .ok_or_else(|| {
                Error::InvalidInput("file upload part is not in manifest".to_string())
            })?;
        let part_size = payload_len_i64(data.len())?;
        if expected_part.offset_bytes != range.start
            || expected_part.size_bytes != part_size
            || expected_part.checksum_sha256 != actual_part_checksum
        {
            return Err(Error::InvalidInput(
                "file upload part does not match manifest".to_string(),
            ));
        }
        if upload_session_is_single_object(session.session_kind) {
            if range.start != 0 || part_size != expected_size {
                return Err(Error::InvalidInput(
                    "file upload object does not match manifest".to_string(),
                ));
            }
            let actual_content_manifest_sha256 = file_part_manifest_digest(
                expected_size,
                session.part_size_bytes,
                [(
                    expected_part.part_number,
                    part_size,
                    actual_part_checksum.as_str(),
                )],
            )?;
            if !constant_time_eq(
                actual_content_manifest_sha256.as_bytes(),
                session.content_manifest_sha256.as_bytes(),
            ) {
                return Err(Error::InvalidInput(
                    "file manifest does not match uploaded object".to_string(),
                ));
            }
            self.operator
                .write_with(&session.object_key, data)
                .content_type(&session.mime_type)
                .await
                .map_err(|error| {
                    Error::Internal(format!("failed to write S3 file object: {error}"))
                })?;
            let blob = self.complete_single_upload_session(&session).await?;
            return Ok(StoreFileUploadResult::Complete(blob));
        }
        if !upload_session_is_multipart(session.session_kind) {
            return Err(Error::InvalidInput(
                "file upload session kind is invalid".to_string(),
            ));
        }
        let existing_parts = repository
            .list_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        if let Some(existing) = existing_parts
            .iter()
            .find(|part| part.part_index == part_index)
        {
            if existing.offset_bytes != range.start
                || existing.size_bytes != part_size
                || existing.checksum_sha256.as_deref() != Some(actual_part_checksum.as_str())
            {
                return Err(Error::InvalidInput(
                    "file upload part conflicts with an existing part".to_string(),
                ));
            }
            let (uploaded_size_bytes, uploaded_parts) =
                upload_session_parts_progress(&existing_parts)?;
            return Ok(StoreFileUploadResult::PartAccepted {
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let upload_id = session
            .upload_id
            .as_deref()
            .ok_or_else(|| Error::InvalidInput("S3 upload_id is required".to_string()))?;
        let etag = self
            .upload_s3_multipart_part(
                &session.object_key,
                upload_id,
                part_number,
                &actual_part_checksum,
                range.start,
                data,
            )
            .await?;
        repository
            .upsert_upload_session_part(UpsertFileUploadSessionPart {
                storage_backend: &self.config.storage_backend,
                upload_session_key: &session.upload_session_key,
                part_index,
                part_number,
                offset_bytes: range.start,
                size_bytes: part_size,
                checksum_sha256: Some(&actual_part_checksum),
                etag: Some(&etag),
            })
            .await?;
        let session_parts = repository
            .list_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        let result = self
            .finalize_multipart_upload_session(&session, &session_parts, upload_id)
            .await?;
        Ok(match result.object {
            Some(blob) => StoreFileUploadResult::Complete(blob),
            None => StoreFileUploadResult::PartAccepted {
                uploaded_size_bytes: result.uploaded_size_bytes,
                uploaded_parts: result.uploaded_parts,
            },
        })
    }
}
