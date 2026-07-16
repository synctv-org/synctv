use super::S3CompatibleFileStorageService;
use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, FileBlob, FileBlobCompression,
    },
    repository::UpsertFileUploadSessionPart,
    service::file_storage::{
        constant_time_eq, decode_file_object_key, file_ownership_proof_digest,
        mark_upload_session_ownership_proof_verified, upload_session_file_id,
        upload_session_is_multipart, upload_session_is_single_object,
        upload_session_object_metadata, validate_file_upload_token,
    },
    Error, Result,
};
use futures::{stream, StreamExt as _, TryStreamExt as _};

impl S3CompatibleFileStorageService {
    pub(super) async fn complete_s3_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        let repository = self.repository()?;
        let object_key = decode_file_object_key(&request.encoded_object_key)?;
        let _payload = validate_file_upload_token(
            &self.config.storage_backend,
            &request.upload_token,
            &object_key,
            crate::SystemClock.now(),
            &self.config.upload_token_secret,
        )?;
        let Some(file_id) = request
            .file_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return Err(Error::InvalidInput("file_id is required".to_string()));
        };
        let (reference_kind, reference_id) =
            crate::service::file_storage::upload_session_reference_target(file_id);
        let mut reference_session = repository
            .get_upload_session_by_reference(reference_kind, &reference_id)
            .await?;
        let reference_file = if let Some(session) = reference_session.as_ref() {
            if session.storage_backend != self.config.storage_backend
                || (session.object_key != object_key && session.upload_session_key != object_key)
            {
                return Err(Error::InvalidInput(
                    "file reference does not match upload session".to_string(),
                ));
            }
            if let Some(ownership_proof) = session.metadata.ownership_proof.as_ref() {
                if session.expires_at <= crate::SystemClock.now() {
                    return Err(Error::InvalidInput(
                        "file upload session is not active".to_string(),
                    ));
                }
                Some((
                    session.object_key.clone(),
                    session.mime_type.clone(),
                    session.size_bytes,
                    session.content_manifest_sha256.clone(),
                    session.metadata.clone(),
                    ownership_proof.clone(),
                ))
            } else {
                if upload_session_is_single_object(session.session_kind)
                    && session.completed_at.is_some()
                {
                    return Ok(CompleteFileUploadSessionResult {
                        object: Some(super::super::session_record_blob(
                            session,
                            bytes::Bytes::new(),
                            upload_session_object_metadata(&session.metadata),
                        )),
                        uploaded_size_bytes: session.size_bytes,
                        uploaded_parts: vec![1],
                    });
                }
                if session.completed_at.is_some() || session.expires_at <= crate::SystemClock.now()
                {
                    return Err(Error::InvalidInput(
                        "file upload session is not active".to_string(),
                    ));
                }
                if upload_session_is_single_object(session.session_kind) {
                    return Ok(CompleteFileUploadSessionResult {
                        object: None,
                        uploaded_size_bytes: 0,
                        uploaded_parts: Vec::new(),
                    });
                }
                None
            }
        } else {
            let reference = repository
                .get_active_reference_metadata_by_target(reference_kind, &reference_id)
                .await?
                .ok_or_else(|| Error::InvalidInput("file reference was not found".to_string()))?;
            if reference.storage_backend != self.config.storage_backend {
                return Err(Error::InvalidInput(
                    "file reference does not match upload session".to_string(),
                ));
            }
            if reference.object_key == object_key {
                let crate::models::FileReferenceMetadata::UploadSession(metadata) =
                    reference.metadata
                else {
                    return Err(Error::InvalidInput(
                        "file reference was not found".to_string(),
                    ));
                };
                let ownership_proof = metadata.ownership_proof.clone().ok_or_else(|| {
                    Error::InvalidInput("file reference was not found".to_string())
                })?;
                Some((
                    reference.object_key,
                    reference.mime_type,
                    reference.size_bytes,
                    reference.content_manifest_sha256,
                    metadata,
                    ownership_proof,
                ))
            } else {
                let session = repository
                    .get_upload_session(&self.config.storage_backend, &object_key)
                    .await?
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "file reference does not match upload session".to_string(),
                        )
                    })?;
                if session.object_key != reference.object_key
                    || upload_session_file_id(&session.metadata)? != reference_id
                {
                    return Err(Error::InvalidInput(
                        "file reference does not match upload session".to_string(),
                    ));
                }
                reference_session = Some(session);
                None
            }
        };
        if let Some((
            reference_object_key,
            mime_type,
            size_bytes,
            content_manifest_sha256,
            metadata,
            ownership_proof,
        )) = reference_file
        {
            if metadata.ownership_proof_verified {
                return Err(Error::InvalidInput(
                    "file upload session is not active".to_string(),
                ));
            }
            let proof = request
                .ownership_proof
                .as_deref()
                .map(str::trim)
                .filter(|proof| !proof.is_empty())
                .ok_or_else(|| {
                    Error::InvalidInput("file ownership proof is required".to_string())
                })?;
            let nonce = ownership_proof.nonce.as_str();
            let ranges = &ownership_proof.ranges;
            let chunks = stream::iter(0..ranges.len())
                .map(|index| self.read_object_range(&reference_object_key, &ranges[index]))
                .buffered(3)
                .try_collect::<Vec<_>>()
                .await?;
            let expected = file_ownership_proof_digest(
                nonce,
                ranges,
                &content_manifest_sha256,
                size_bytes,
                chunks.iter().map(Vec::as_slice),
            );
            if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                return Err(Error::InvalidInput(
                    "file ownership proof does not match object".to_string(),
                ));
            }
            let metadata = mark_upload_session_ownership_proof_verified(&metadata);
            repository
                .update_reference_metadata(
                    reference_kind,
                    &reference_id,
                    &self.config.storage_backend,
                    &reference_object_key,
                    &crate::models::FileReferenceMetadata::UploadSession(metadata),
                )
                .await?;
            return Ok(CompleteFileUploadSessionResult {
                object: Some(FileBlob {
                    storage_backend: self.config.storage_backend.clone(),
                    object_key: reference_object_key,
                    mime_type,
                    size_bytes,
                    total_size_bytes: size_bytes,
                    content_manifest_sha256,
                    compression: FileBlobCompression::None,
                    range: None,
                    data: bytes::Bytes::new(),
                    metadata: Default::default(),
                    created_at: crate::SystemClock.now(),
                }),
                uploaded_size_bytes: size_bytes,
                uploaded_parts: Vec::new(),
            });
        }
        let session = reference_session
            .take()
            .ok_or_else(|| Error::InvalidInput("file reference was not found".to_string()))?;
        if upload_session_is_single_object(session.session_kind) {
            let blob = self.complete_single_upload_session(&session).await?;
            return Ok(CompleteFileUploadSessionResult {
                object: Some(blob),
                uploaded_size_bytes: session.size_bytes,
                uploaded_parts: vec![1],
            });
        }
        if !upload_session_is_multipart(session.session_kind) {
            return Err(Error::InvalidInput(
                "file upload session kind is invalid".to_string(),
            ));
        }
        let upload_id = request
            .upload_id
            .as_deref()
            .or(session.upload_id.as_deref())
            .ok_or_else(|| Error::InvalidInput("S3 upload_id is required".to_string()))?;
        let mut uploaded_parts = Vec::with_capacity(request.parts.len());
        for part in &request.parts {
            if part.part_number <= 0 || part.size_bytes <= 0 {
                return Err(Error::InvalidInput(
                    "S3 multipart completion requires positive part number and size".to_string(),
                ));
            }
            let checksum = part.checksum_sha256.as_deref().ok_or_else(|| {
                Error::InvalidInput(
                    "S3 multipart completion requires every part checksum_sha256".to_string(),
                )
            })?;
            let offset = i64::from(part.part_number - 1)
                .checked_mul(session.part_size_bytes)
                .ok_or_else(|| {
                    Error::InvalidInput("file upload part offset overflow".to_string())
                })?;
            let expected_part_size = (session.size_bytes - offset).min(session.part_size_bytes);
            if offset < 0 || expected_part_size <= 0 || part.size_bytes != expected_part_size {
                return Err(Error::InvalidInput(
                    "S3 multipart completion part size does not match upload session".to_string(),
                ));
            }
            repository
                .upsert_upload_session_part(UpsertFileUploadSessionPart {
                    storage_backend: &self.config.storage_backend,
                    upload_session_key: &session.upload_session_key,
                    part_index: part.part_number - 1,
                    part_number: part.part_number,
                    offset_bytes: offset,
                    size_bytes: part.size_bytes,
                    checksum_sha256: Some(checksum),
                    etag: Some(&part.etag),
                })
                .await?;
            uploaded_parts.push(part.part_number);
        }
        let session_parts = repository
            .list_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        self.finalize_multipart_upload_session(&session, &session_parts, upload_id)
            .await
    }
}
