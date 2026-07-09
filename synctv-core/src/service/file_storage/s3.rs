use std::sync::Arc;

use opendal::Operator;

use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, CreateFileUploadSession,
        FileBlob, FileObjectAccess, FileObjectDownload, FileObjectKind, FileReferenceTarget,
        FileUploadSessionCreateResult, GetFileObject, NewStoredFile, StoreFileUpload,
        StoreFileUploadResult,
    },
    repository::FileStorageRepository,
    service::file_storage::{
        collect_file_object_download, file_object_access, file_reuse_grant,
        optional_file_storage_public_url, validate_file_reuse_grant,
        validate_s3_file_storage_config, CreateFileReuseGrant, FileObjectReader, FileReuseGrant,
        FileStorageCleanupOrigin, FileStorageContext, FileStorageService, ValidatedFileReuseGrant,
    },
    Error, Result,
};

mod cleanup;
mod complete_upload;
mod create_upload;
mod multipart;
mod multipart_presign;
mod multipart_runtime;
mod multipart_transport;
mod object_io;
mod prepare;
mod protocol;
mod setup;
mod store_upload;
#[cfg(feature = "test-support")]
mod test_bucket;
mod upload_completion;
mod url;

#[cfg(test)]
pub(super) use multipart_presign::presigned_upload_headers;
use setup::{s3_http_client, s3_operator_from_config};

#[derive(Debug, Clone)]
pub struct S3FileStorageConfig {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub region: String,
    pub base_path: String,
    pub public_base_url: Option<String>,
    pub upload_expires_seconds: i64,
    pub storage_backend: String,
    pub upload_token_secret: String,
}

pub struct S3CompatibleFileStorageService {
    pub(crate) config: S3FileStorageConfig,
    pub(crate) operator: Operator,
    pub(crate) repository: Option<Arc<FileStorageRepository>>,
    http_client: reqwest::Client,
    #[cfg(test)]
    test_multipart_upload_id: Option<String>,
    #[cfg(test)]
    test_force_stat_error: bool,
}

impl S3CompatibleFileStorageService {
    pub fn new(config: S3FileStorageConfig) -> Result<Self> {
        Self::new_with_repository(config, None)
    }

    pub fn new_with_repository(
        config: S3FileStorageConfig,
        repository: Option<Arc<FileStorageRepository>>,
    ) -> Result<Self> {
        validate_s3_file_storage_config(&config)?;
        let operator = s3_operator_from_config(&config)?;
        Ok(Self {
            config,
            operator,
            repository,
            http_client: s3_http_client(),
            #[cfg(test)]
            test_multipart_upload_id: None,
            #[cfg(test)]
            test_force_stat_error: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_operator(mut self, operator: Operator) -> Self {
        self.operator = operator;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_multipart_upload_id(mut self, upload_id: impl Into<String>) -> Self {
        self.test_multipart_upload_id = Some(upload_id.into());
        self
    }

    #[cfg(test)]
    pub(crate) const fn with_test_force_stat_error(mut self) -> Self {
        self.test_force_stat_error = true;
        self
    }

    fn repository(&self) -> Result<&Arc<FileStorageRepository>> {
        self.repository.as_ref().ok_or_else(|| {
            Error::Internal(
                "S3 multipart file uploads require a file storage repository".to_string(),
            )
        })
    }
}

#[async_trait::async_trait]
impl FileStorageService for S3CompatibleFileStorageService {
    fn backend_name(&self) -> &str {
        &self.config.storage_backend
    }

    fn supports_reuse_grants(&self) -> bool {
        true
    }

    fn repository(&self) -> Option<Arc<FileStorageRepository>> {
        self.repository.clone()
    }

    fn public_object_url(&self, storage_backend: &str, object_key: &str) -> Result<Option<String>> {
        if storage_backend != self.config.storage_backend {
            return Ok(None);
        }
        optional_file_storage_public_url(&self.config, object_key)
    }

    fn file_object_access(
        &self,
        storage_backend: &str,
        object_key: &str,
        object_kind: FileObjectKind,
    ) -> Result<Option<FileObjectAccess>> {
        if storage_backend != self.config.storage_backend {
            return Ok(None);
        }
        file_object_access(
            object_kind,
            &self.config.storage_backend,
            object_key,
            &self.config.upload_token_secret,
        )
        .map(Some)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        self.create_s3_upload_session(request).await
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        self.prepare_s3_files(context, files).await
    }

    fn create_reuse_grant(&self, request: CreateFileReuseGrant<'_>) -> Result<FileReuseGrant> {
        file_reuse_grant(&request, &self.config.upload_token_secret)
    }

    async fn validate_reuse_grant(
        &self,
        token: &str,
        context: FileStorageContext<'_>,
    ) -> Result<ValidatedFileReuseGrant> {
        validate_file_reuse_grant(
            token,
            context,
            crate::SystemClock.now(),
            &self.config.upload_token_secret,
        )
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> Result<()> {
        self.delete_s3_files(origin, files).await
    }

    async fn cleanup_expired_upload_session(
        &self,
        session: crate::models::FileUploadSessionRecord,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        self.cleanup_expired_s3_upload_session(session, now).await
    }

    async fn store_upload(&self, upload: StoreFileUpload) -> Result<StoreFileUploadResult> {
        self.store_s3_upload(upload).await
    }

    async fn complete_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        self.complete_s3_upload_session(request).await
    }

    async fn get_object(&self, request: GetFileObject) -> Result<FileBlob> {
        collect_file_object_download(self.object_download(request).await?).await
    }

    async fn get_object_stream(&self, request: GetFileObject) -> Result<FileObjectDownload> {
        self.object_download(request).await
    }

    async fn get_object_by_key(&self, storage_backend: &str, object_key: &str) -> Result<FileBlob> {
        self.object_by_key(storage_backend, object_key).await
    }

    async fn get_object_reader_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<FileObjectReader> {
        self.object_reader_by_key(storage_backend, object_key).await
    }

    async fn put_object_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
        mime_type: &str,
        data: Vec<u8>,
        metadata: crate::models::FileMetadata,
    ) -> Result<FileBlob> {
        self.write_object_by_key(storage_backend, object_key, mime_type, data, metadata)
            .await
    }

    async fn process_object_variants(
        &self,
        storage_backend: &str,
        object_key: &str,
        object_kind: FileObjectKind,
        upload_policy: &crate::models::FileUploadPolicy,
    ) -> Result<Vec<crate::models::FileObjectVariant>> {
        if storage_backend != self.config.storage_backend {
            return Ok(Vec::new());
        }
        let repository = self.repository()?;
        super::process_file_variants_for_object(
            self,
            repository.clone(),
            storage_backend,
            object_key,
            object_kind,
            upload_policy,
        )
        .await
        .map(|result| result.variants)
    }
}
