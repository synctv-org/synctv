use super::S3CompatibleFileStorageService;
use crate::{
    models::NewStoredFile,
    service::file_storage::{
        attach_prepared_file_object_access, attach_prepared_file_urls,
        strip_internal_file_metadata, validate_file_upload_token_context, validate_stored_files,
        FileStorageContext,
    },
    Error, Result,
};

impl S3CompatibleFileStorageService {
    pub(super) async fn prepare_s3_files(
        &self,
        context: FileStorageContext<'_>,
        mut files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        for file in &files {
            if file.storage_backend != self.config.storage_backend {
                return Err(Error::InvalidInput(format!(
                    "file storage_backend must be {}",
                    self.config.storage_backend
                )));
            }
        }
        for file in &files {
            if let Some(token) = file
                .metadata
                .upload_token
                .as_deref()
                .map(str::trim)
                .filter(|token| !token.is_empty())
            {
                let payload = validate_file_upload_token_context(
                    token,
                    crate::SystemClock.now(),
                    &self.config.upload_token_secret,
                )?;
                if payload.user_id != context.user_id.as_i64()
                    || payload.storage_scope != context.storage_scope
                {
                    return Err(Error::InvalidInput(
                        "file upload token does not belong to this request".to_string(),
                    ));
                }
            }
            if let Some(repository) = self.repository.as_ref() {
                if repository
                    .object_validated(&self.config.storage_backend, &file.object_key)
                    .await?
                {
                    continue;
                }
                return Err(Error::InvalidInput(
                    "file upload session has not been completed".to_string(),
                ));
            }
        }
        strip_internal_file_metadata(&mut files);
        validate_stored_files(&files)?;
        attach_prepared_file_urls(self, &mut files)?;
        attach_prepared_file_object_access(self, &mut files, context.object_kind)?;
        if let Some(repository) = self.repository.as_ref() {
            super::super::media_processing::attach_variants_to_files(
                self,
                repository.as_ref(),
                &mut files,
                context.object_kind,
            )
            .await?;
        }
        Ok(files)
    }
}
