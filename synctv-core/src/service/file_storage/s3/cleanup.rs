use super::S3CompatibleFileStorageService;
use crate::{
    models::{FileReferenceTarget, FileUploadSessionRecord},
    service::file_storage::FileStorageCleanupOrigin,
    Error, Result,
};

impl S3CompatibleFileStorageService {
    pub(super) async fn delete_s3_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> Result<()> {
        let origin_label = origin.as_str();
        let mut failed_count = 0_u64;
        let mut last_error = None;
        for file in files {
            if file.storage_backend != self.config.storage_backend {
                continue;
            }
            crate::metrics::file_storage::FILE_OBJECT_DELETE_ATTEMPTS
                .with_label_values(&[origin_label, &file.storage_backend])
                .inc();
            if let Some(repository) = self.repository.as_ref() {
                let delete_claimed = repository
                    .claim_object_for_delete(
                        &file.storage_backend,
                        &file.object_key,
                        &file.reference_kind,
                        &file.reference_id,
                        origin == FileStorageCleanupOrigin::UnreferencedObject,
                    )
                    .await?;
                if !delete_claimed {
                    continue;
                }
            }
            let mut objects = Vec::new();
            if let Some(repository) = self.repository.as_ref() {
                let derived_variants = repository
                    .list_derived_object_variants(&file.storage_backend, &file.object_key)
                    .await?;
                objects.extend(
                    derived_variants
                        .into_iter()
                        .map(|variant| (variant.storage_backend, variant.object_key)),
                );
            }
            objects.push((file.storage_backend.clone(), file.object_key.clone()));
            let mut file_delete_failed = false;
            for (_, object_key) in &objects {
                match self.operator.delete(object_key).await {
                    Ok(()) => {}
                    Err(error) => {
                        file_delete_failed = true;
                        failed_count += 1;
                        last_error = Some(error.to_string());
                        crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                            .with_label_values(&[origin_label, &file.storage_backend])
                            .inc();
                        tracing::warn!(
                            error = %error,
                            object_key,
                            "failed to delete file object"
                        );
                    }
                }
            }
            if file_delete_failed {
                continue;
            }
            if let Some(repository) = self.repository.as_ref() {
                for (storage_backend, object_key) in objects
                    .iter()
                    .filter(|(_, object_key)| object_key != &file.object_key)
                {
                    repository
                        .delete_object(storage_backend, object_key)
                        .await?;
                }
                repository
                    .delete_object(&file.storage_backend, &file.object_key)
                    .await?;
            }
        }
        if failed_count > 0 {
            return Err(Error::Internal(format!(
                "failed to delete {failed_count} file object(s): {}",
                last_error.unwrap_or_else(|| "unknown error".to_string())
            )));
        }
        Ok(())
    }

    pub(super) async fn cleanup_expired_s3_upload_session(
        &self,
        session: FileUploadSessionRecord,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        if session.storage_backend != self.config.storage_backend {
            return Ok(false);
        }
        if session.completed_at.is_some() || session.expires_at > now {
            return Ok(false);
        }
        let repository = self.repository()?;
        if let Some(upload_id) = session.upload_id.as_deref() {
            if let Err(error) = self
                .abort_s3_multipart_upload(&session.object_key, upload_id)
                .await
            {
                tracing::warn!(
                    error = %error,
                    object_key = %session.object_key,
                    upload_id,
                    "failed to abort expired S3 multipart upload"
                );
            }
        }
        if let Err(error) = self.operator.delete(&session.object_key).await {
            tracing::debug!(
                error = %error,
                object_key = %session.object_key,
                "expired S3 upload object delete skipped or failed"
            );
        }
        repository
            .delete_upload_session_parts(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        let (_, reference_id) =
            super::super::upload_session_reference_target(session.metadata.file_id.as_str());
        if !reference_id.is_empty() {
            repository
                .release_reference(
                    super::super::FILE_UPLOAD_SESSION_REFERENCE_KIND,
                    &reference_id,
                    &self.config.storage_backend,
                    &session.object_key,
                )
                .await?;
        }
        let session_deleted = repository
            .delete_upload_session(&self.config.storage_backend, &session.upload_session_key)
            .await?;
        if !repository
            .object_validated(&self.config.storage_backend, &session.object_key)
            .await?
            && repository
                .object_reference_count_excluding_kind(
                    &self.config.storage_backend,
                    &session.object_key,
                    super::super::FILE_UPLOAD_SESSION_REFERENCE_KIND,
                )
                .await?
                == 0
        {
            repository
                .delete_object(&self.config.storage_backend, &session.object_key)
                .await?;
        }
        Ok(session_deleted)
    }
}
