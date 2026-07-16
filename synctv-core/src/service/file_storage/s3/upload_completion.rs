use super::{
    multipart::{completed_upload_part_manifest_digest, completion_parts_from_session_parts},
    S3CompatibleFileStorageService,
};
use crate::{
    models::{CompleteFileUploadSessionResult, FileBlob, FileBlobCompression},
    repository::UpsertFileObject,
    service::file_storage::{
        constant_time_eq, upload_session_object_metadata, upload_session_parts_progress,
        upload_session_policy,
    },
    Error, Result,
};

impl S3CompatibleFileStorageService {
    pub(super) async fn finalize_multipart_upload_session(
        &self,
        session: &crate::models::FileUploadSessionRecord,
        session_parts: &[crate::models::FileUploadSessionPart],
        upload_id: &str,
    ) -> Result<CompleteFileUploadSessionResult> {
        let (uploaded_size_bytes, uploaded_parts) = upload_session_parts_progress(session_parts)?;
        if uploaded_size_bytes != session.size_bytes {
            return Ok(CompleteFileUploadSessionResult {
                object: None,
                uploaded_size_bytes,
                uploaded_parts,
            });
        }
        let completed_parts = completion_parts_from_session_parts(session_parts)?;
        self.complete_s3_multipart_upload(&session.object_key, upload_id, &completed_parts)
            .await?;
        self.validate_completed_s3_object_size(&session.object_key, session.size_bytes)
            .await?;
        let content_manifest_sha256 = completed_upload_part_manifest_digest(
            session_parts,
            session.size_bytes,
            session.part_size_bytes,
        )?;
        if !constant_time_eq(
            content_manifest_sha256.as_bytes(),
            session.content_manifest_sha256.as_bytes(),
        ) {
            self.delete_invalid_upload_object(&session.object_key, "manifest_mismatch")
                .await;
            return Err(Error::InvalidInput(
                "file manifest does not match uploaded parts".to_string(),
            ));
        }
        let repository = self.repository()?;
        let upload_policy = upload_session_policy(&session.metadata);
        let metadata = upload_session_object_metadata(&session.metadata);
        repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: &self.config.storage_backend,
                object_key: &session.object_key,
                mime_type: &session.mime_type,
                size_bytes: session.size_bytes,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &metadata,
            })
            .await?;
        let mut blob = FileBlob {
            storage_backend: self.config.storage_backend.clone(),
            object_key: session.object_key.clone(),
            mime_type: session.mime_type.clone(),
            size_bytes: session.size_bytes,
            total_size_bytes: session.size_bytes,
            content_manifest_sha256: content_manifest_sha256.clone(),
            compression: FileBlobCompression::None,
            range: None,
            data: bytes::Bytes::new(),
            metadata,
            created_at: crate::SystemClock.now(),
        };
        if let Err(error) =
            super::super::complete_uploaded_file_object(self, repository, &mut blob, &upload_policy)
                .await
        {
            self.delete_invalid_upload_object(&session.object_key, "media_validation_failed")
                .await;
            repository
                .delete_upload_session_parts(
                    &self.config.storage_backend,
                    &session.upload_session_key,
                )
                .await?;
            repository
                .delete_object(&self.config.storage_backend, &session.object_key)
                .await?;
            return Err(error);
        }
        repository
            .mark_object_validated(&self.config.storage_backend, &session.object_key)
            .await?;
        repository
            .complete_upload_session(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        Ok(CompleteFileUploadSessionResult {
            object: Some(blob),
            uploaded_size_bytes: session.size_bytes,
            uploaded_parts,
        })
    }

    pub(super) async fn complete_single_upload_session(
        &self,
        session: &crate::models::FileUploadSessionRecord,
    ) -> Result<FileBlob> {
        self.validate_completed_s3_object_size(&session.object_key, session.size_bytes)
            .await?;
        let repository = self.repository()?;
        let upload_policy = upload_session_policy(&session.metadata);
        let metadata = upload_session_object_metadata(&session.metadata);
        let mut blob =
            super::super::session_record_blob(session, bytes::Bytes::new(), metadata.clone());
        if let Err(error) =
            super::super::complete_uploaded_file_object(self, repository, &mut blob, &upload_policy)
                .await
        {
            self.delete_invalid_upload_object(&session.object_key, "media_validation_failed")
                .await;
            repository
                .delete_object(&self.config.storage_backend, &session.object_key)
                .await?;
            return Err(error);
        }
        repository
            .mark_object_validated(&self.config.storage_backend, &session.object_key)
            .await?;
        repository
            .update_object_metadata(
                &self.config.storage_backend,
                &session.object_key,
                &blob.metadata,
            )
            .await?;
        repository
            .complete_upload_session(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        Ok(blob)
    }
}
