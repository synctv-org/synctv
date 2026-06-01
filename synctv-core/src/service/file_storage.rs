use std::time::Duration as StdDuration;
use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use opendal::{services::S3, Operator};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    models::{
        CreateFileUploadSession, FileBlob, FileOwnershipProofRange, FileReferenceTarget,
        FileUploadPolicy, FileUploadSession, NewStoredFile, UserId,
    },
    repository::FileStorageRepository,
    Error, Result,
};

const FILE_UPLOAD_EXPIRES_SECONDS: i64 = 900;
pub(crate) const FILE_UPLOAD_TOKEN_KEY: &str = "_synctv_upload_token";
pub(crate) const FILE_OWNERSHIP_PROOF_KEY: &str = "_synctv_ownership_proof";
const FILE_OWNERSHIP_PROOF_ALGORITHM: &str = "synctv-file-ownership-proof-v1";
const FILE_OWNERSHIP_PROOF_RANGE_COUNT: usize = 3;
const FILE_OWNERSHIP_PROOF_RANGE_BYTES: i32 = 1024;
const FILE_UPLOAD_TOKEN_VERSION: &str = "v1";
pub const FILE_UPLOAD_TOKEN_HEADER: &str = "x-synctv-file-upload-token";
const DATABASE_FILE_READ_TOKEN_VERSION: &str = "v1";
const MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES: usize = 20 * 1024 * 1024;

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

#[derive(Clone)]
pub struct FileStorageBackendRegistry {
    backends: Arc<HashMap<String, Arc<dyn FileStorageService>>>,
}

#[derive(Clone)]
pub struct RoutedFileStorageService {
    registry: FileStorageBackendRegistry,
    write_backend: String,
}

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
}

impl DatabaseFileStorageService {
    #[must_use]
    pub fn new(
        storage_backend: impl Into<String>,
        repository: Arc<FileStorageRepository>,
        upload_token_secret: impl Into<String>,
    ) -> Self {
        Self {
            storage_backend: storage_backend.into(),
            repository,
            upload_token_secret: upload_token_secret.into(),
        }
    }
}

impl FileStorageBackendRegistry {
    #[must_use]
    pub fn new(backends: HashMap<String, Arc<dyn FileStorageService>>) -> Self {
        Self {
            backends: Arc::new(backends),
        }
    }

    pub fn routed(&self, write_backend: impl Into<String>) -> Result<RoutedFileStorageService> {
        let write_backend = write_backend.into();
        if !self.backends.contains_key(&write_backend) {
            return Err(Error::InvalidInput(format!(
                "file storage backend '{write_backend}' is not configured"
            )));
        }
        Ok(RoutedFileStorageService {
            registry: self.clone(),
            write_backend,
        })
    }

    fn backend(&self, name: &str) -> Result<Arc<dyn FileStorageService>> {
        self.backends.get(name).cloned().ok_or_else(|| {
            Error::InvalidInput(format!("file storage backend '{name}' is not configured"))
        })
    }
}

#[async_trait::async_trait]
impl FileStorageService for RoutedFileStorageService {
    fn backend_name(&self) -> &str {
        &self.write_backend
    }

    fn object_url(
        &self,
        storage_backend: &str,
        object_key: &str,
        database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        self.registry.backend(storage_backend)?.object_url(
            storage_backend,
            object_key,
            database_object_route_prefix,
        )
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        self.registry
            .backend(&self.write_backend)?
            .create_upload_session(request)
            .await
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        validate_stored_files(&files)?;
        let mut prepared = Vec::with_capacity(files.len());
        let mut by_backend: HashMap<String, Vec<NewStoredFile>> = HashMap::new();
        for file in files {
            by_backend
                .entry(file.storage_backend.clone())
                .or_default()
                .push(file);
        }
        for (backend_name, backend_files) in by_backend {
            let mut backend_prepared = self
                .registry
                .backend(&backend_name)?
                .prepare_files(context, backend_files)
                .await?;
            prepared.append(&mut backend_prepared);
        }
        Ok(prepared)
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> Result<()> {
        let mut by_backend: HashMap<&str, Vec<FileReferenceTarget>> = HashMap::new();
        for file in files {
            by_backend
                .entry(file.storage_backend.as_str())
                .or_default()
                .push(file.clone());
        }
        for (backend_name, backend_files) in by_backend {
            self.registry
                .backend(backend_name)?
                .delete_files(origin, &backend_files)
                .await?;
        }
        Ok(())
    }

    async fn store_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        let backend_name = file_upload_token_storage_backend(upload_token)?;
        self.registry
            .backend(&backend_name)?
            .store_upload_object(encoded_object_key, upload_token, content_type, data)
            .await
    }

    async fn get_object(&self, encoded_object_key: &str, read_token: &str) -> Result<FileBlob> {
        let backend_name = database_file_read_token_storage_backend(read_token)?;
        self.registry
            .backend(&backend_name)?
            .get_object(encoded_object_key, read_token)
            .await
    }
}

impl S3CompatibleFileStorageService {
    pub fn new(config: S3FileStorageConfig) -> Result<Self> {
        validate_s3_file_storage_config(&config)?;
        let operator = s3_operator_from_config(&config)?;
        Ok(Self {
            config,
            operator,
            repository: None,
        })
    }

    #[must_use]
    pub fn with_repository(mut self, repository: Arc<FileStorageRepository>) -> Self {
        self.repository = Some(repository);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_operator(mut self, operator: Operator) -> Self {
        self.operator = operator;
        self
    }

    async fn read_object_range(
        &self,
        object_key: &str,
        range: &FileOwnershipProofRange,
    ) -> Result<Vec<u8>> {
        if range.offset < 0 || range.length <= 0 {
            return Err(Error::InvalidInput(
                "invalid file ownership proof range".to_string(),
            ));
        }
        let start = u64::try_from(range.offset)
            .map_err(|_| Error::InvalidInput("invalid file ownership proof range".to_string()))?;
        let length = u64::try_from(range.length)
            .map_err(|_| Error::InvalidInput("invalid file ownership proof range".to_string()))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| Error::InvalidInput("invalid file ownership proof range".to_string()))?;
        let bytes = self
            .operator
            .read_with(object_key)
            .range(start..end)
            .await
            .map_err(|error| {
                Error::InvalidInput(format!("file object range is not readable: {error}"))
            })?;
        Ok(bytes.to_vec())
    }
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

#[async_trait::async_trait]
impl FileStorageService for DatabaseFileStorageService {
    fn backend_name(&self) -> &str {
        &self.storage_backend
    }

    fn object_url(
        &self,
        storage_backend: &str,
        object_key: &str,
        database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        if storage_backend != self.storage_backend {
            return Ok(None);
        }
        database_file_object_url(
            database_object_route_prefix,
            storage_backend,
            object_key,
            &self.upload_token_secret,
        )
        .map(Some)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        validate_create_file_upload_session(&request)?;
        let checksum_sha256 = normalized_checksum_sha256(request.checksum_sha256.as_deref());
        let file_id = file_id_from_request(request.client_file_id.as_deref());
        let expires_at = Utc::now() + chrono::Duration::seconds(FILE_UPLOAD_EXPIRES_SECONDS);
        if let Some(checksum) = checksum_sha256.as_deref() {
            if let Some(existing) = self
                .repository
                .get_object_by_checksum(&self.storage_backend, checksum, request.size_bytes)
                .await?
            {
                validate_file_mime_type(&request.policy, &existing.mime_type)?;
                if !self
                    .repository
                    .blob_exists(&self.storage_backend, &existing.object_key)
                    .await?
                {
                    return Err(Error::Internal(
                        "file object registry points to a missing database blob".to_string(),
                    ));
                }
                let mut file = NewStoredFile {
                    id: file_id,
                    storage_backend: self.storage_backend.clone(),
                    object_key: existing.object_key.clone(),
                    url: Some(database_file_object_url(
                        &request.policy.database_object_route_prefix,
                        &self.storage_backend,
                        &existing.object_key,
                        &self.upload_token_secret,
                    )?),
                    mime_type: Some(existing.mime_type),
                    size_bytes: Some(existing.size_bytes),
                    width: request.width,
                    height: request.height,
                    metadata: request.metadata,
                };
                let (nonce, ranges) = attach_file_ownership_proof_token(
                    &mut file,
                    request.user_id,
                    &request.storage_scope,
                    expires_at,
                    &self.upload_token_secret,
                    checksum,
                    request.size_bytes,
                )?;
                validate_stored_files(std::slice::from_ref(&file))?;
                return Ok(FileUploadSession {
                    file,
                    upload_required: false,
                    ownership_proof_required: true,
                    ownership_proof_nonce: Some(nonce),
                    ownership_proof_ranges: ranges,
                    ownership_proof_metadata_key: Some(FILE_OWNERSHIP_PROOF_KEY.to_string()),
                    upload_url: None,
                    upload_method: None,
                    upload_headers: Default::default(),
                    expires_at: Some(expires_at),
                    max_size_bytes: request.policy.max_size_bytes,
                });
            }
        }
        let object_key = if let Some(checksum) = checksum_sha256.as_deref() {
            match self
                .repository
                .get_any_object_by_checksum(&self.storage_backend, checksum, request.size_bytes)
                .await?
            {
                Some(existing) => existing.object_key,
                None => file_content_object_key(
                    &database_file_namespace_base_path(&request.policy.storage_namespace),
                    checksum,
                ),
            }
        } else {
            file_object_key(
                &database_file_namespace_base_path(&request.policy.storage_namespace),
                &request.storage_scope,
                &server_file_object_id(),
                &request.mime_type,
            )
        };
        let mut file = NewStoredFile {
            id: file_id,
            storage_backend: self.storage_backend.clone(),
            object_key,
            url: None,
            mime_type: Some(request.mime_type),
            size_bytes: Some(request.size_bytes),
            width: request.width,
            height: request.height,
            metadata: request.metadata,
        };
        attach_file_upload_token(
            &mut file,
            request.user_id,
            &request.storage_scope,
            expires_at,
            &self.upload_token_secret,
            checksum_sha256.as_deref(),
            None,
        )?;
        file.url = Some(database_file_object_url(
            &request.policy.database_object_route_prefix,
            &self.storage_backend,
            &file.object_key,
            &self.upload_token_secret,
        )?);
        validate_stored_files(std::slice::from_ref(&file))?;
        let mut upload_headers = std::collections::BTreeMap::new();
        upload_headers.insert(
            "content-type".to_string(),
            file.mime_type.clone().unwrap_or_default(),
        );
        if let Some(token) = file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
        {
            upload_headers.insert(FILE_UPLOAD_TOKEN_HEADER.to_string(), token.to_string());
        }
        let object_url = database_file_object_url(
            &request.policy.database_object_route_prefix,
            &self.storage_backend,
            &file.object_key,
            &self.upload_token_secret,
        )?;
        Ok(FileUploadSession {
            file,
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            ownership_proof_metadata_key: None,
            upload_url: Some(object_url),
            upload_method: Some("PUT".to_string()),
            upload_headers,
            expires_at: Some(Utc::now() + chrono::Duration::seconds(FILE_UPLOAD_EXPIRES_SECONDS)),
            max_size_bytes: request.policy.max_size_bytes,
        })
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        mut files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        validate_stored_files(&files)?;
        for file in &files {
            if file.storage_backend != self.storage_backend {
                return Err(Error::InvalidInput(format!(
                    "file storage_backend must be {}",
                    self.storage_backend
                )));
            }
            if !self
                .repository
                .blob_exists(&self.storage_backend, &file.object_key)
                .await?
            {
                return Err(Error::InvalidInput(
                    "file object has not been uploaded".to_string(),
                ));
            }
        }
        validate_file_upload_tokens(context, &files, &self.upload_token_secret)?;
        for file in &files {
            let payload =
                file_upload_token_payload_from_file(file, context.user_id, context.storage_scope)?;
            if payload
                .get("ownership_proof_required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                let proof = file
                    .metadata
                    .get(FILE_OWNERSHIP_PROOF_KEY)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        Error::InvalidInput("file ownership proof is required".to_string())
                    })?;
                let nonce = payload
                    .get("ownership_proof_nonce")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
                let ranges = ownership_proof_ranges_from_payload(&payload)?;
                let blob = self
                    .repository
                    .get_blob(&self.storage_backend, &file.object_key)
                    .await?
                    .ok_or_else(|| {
                        Error::InvalidInput("file object has not been uploaded".to_string())
                    })?;
                let chunks = ownership_proof_chunks_from_bytes(&blob.data, &ranges)?;
                let expected =
                    file_ownership_proof_digest(nonce, &ranges, chunks.iter().map(Vec::as_slice));
                if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                    return Err(Error::InvalidInput(
                        "file ownership proof does not match object".to_string(),
                    ));
                }
            }
        }
        strip_internal_file_metadata(&mut files);
        Ok(files)
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> Result<()> {
        let origin_label = origin.as_str();
        for file in files {
            if file.storage_backend != self.storage_backend {
                continue;
            }
            crate::metrics::file_storage::FILE_OBJECT_DELETE_ATTEMPTS
                .with_label_values(&[origin_label, &file.storage_backend])
                .inc();
            self.repository
                .release_reference(
                    &file.reference_kind,
                    &file.reference_id,
                    &file.storage_backend,
                    &file.object_key,
                )
                .await?;
            if self
                .repository
                .object_reference_count(&file.storage_backend, &file.object_key)
                .await?
                > 0
            {
                continue;
            }
            if let Err(error) = self
                .repository
                .delete_blob(&self.storage_backend, &file.object_key)
                .await
            {
                crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                    .with_label_values(&[origin_label, &file.storage_backend])
                    .inc();
                return Err(error);
            }
            self.repository
                .delete_object(&file.storage_backend, &file.object_key)
                .await?;
        }
        Ok(())
    }

    async fn store_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        data: Vec<u8>,
    ) -> Result<FileBlob> {
        if data.is_empty() || data.len() > MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES {
            return Err(Error::InvalidInput(format!(
                "file payload must be between 1 and {MAX_DATABASE_FILE_UPLOAD_SIZE_BYTES} bytes"
            )));
        }
        let object_key = decode_database_file_object_key(encoded_object_key)?;
        let payload = validate_database_file_upload_token(
            &self.storage_backend,
            upload_token,
            &object_key,
            Utc::now(),
            &self.upload_token_secret,
        )?;
        let mime_type = payload
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if let Some(content_type) = content_type {
            if content_type.split(';').next().unwrap_or_default().trim() != mime_type {
                return Err(Error::InvalidInput(
                    "file content-type does not match upload session".to_string(),
                ));
            }
        }
        let expected_size = payload
            .get("size_bytes")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
        if expected_size != i64::try_from(data.len()).unwrap_or(i64::MAX) {
            return Err(Error::InvalidInput(
                "file payload size does not match upload session".to_string(),
            ));
        }
        let actual_checksum = hex::encode(Sha256::digest(&data));
        if let Some(expected_checksum) = payload
            .get("checksum_sha256")
            .and_then(serde_json::Value::as_str)
        {
            if !constant_time_eq(
                actual_checksum.as_bytes(),
                expected_checksum.to_ascii_lowercase().as_bytes(),
            ) {
                return Err(Error::InvalidInput(
                    "file payload checksum does not match upload session".to_string(),
                ));
            }
        }
        let blob = self
            .repository
            .upsert_blob(
                &self.storage_backend,
                &object_key,
                mime_type,
                data,
                &serde_json::Value::Object(Default::default()),
            )
            .await?;
        self.repository
            .upsert_object(
                &self.storage_backend,
                &object_key,
                mime_type,
                expected_size,
                &actual_checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await?;
        Ok(blob)
    }

    async fn get_object(&self, encoded_object_key: &str, read_token: &str) -> Result<FileBlob> {
        let object_key = decode_database_file_object_key(encoded_object_key)?;
        validate_database_file_read_token(
            &self.storage_backend,
            &object_key,
            read_token,
            &self.upload_token_secret,
        )?;
        self.repository
            .get_blob(&self.storage_backend, &object_key)
            .await?
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))
    }
}

#[async_trait::async_trait]
impl FileStorageService for S3CompatibleFileStorageService {
    fn backend_name(&self) -> &str {
        &self.config.storage_backend
    }

    fn object_url(
        &self,
        storage_backend: &str,
        object_key: &str,
        _database_object_route_prefix: &str,
    ) -> Result<Option<String>> {
        if storage_backend != self.config.storage_backend {
            return Ok(None);
        }
        optional_file_storage_public_url(&self.config, object_key)
    }

    async fn create_upload_session(
        &self,
        request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        validate_create_file_upload_session(&request)?;
        let checksum_sha256 = normalized_checksum_sha256(request.checksum_sha256.as_deref());
        let file_id = file_id_from_request(request.client_file_id.as_deref());
        let now = Utc::now();
        let expires = self
            .config
            .upload_expires_seconds
            .clamp(60, FILE_UPLOAD_EXPIRES_SECONDS);
        let expires_at = now + chrono::Duration::seconds(expires);
        if let (Some(repository), Some(checksum)) =
            (self.repository.as_ref(), checksum_sha256.as_deref())
        {
            if let Some(existing) = repository
                .get_object_by_checksum(&self.config.storage_backend, checksum, request.size_bytes)
                .await?
            {
                if self.operator.stat(&existing.object_key).await.is_ok() {
                    validate_file_mime_type(&request.policy, &existing.mime_type)?;
                    let mut file = NewStoredFile {
                        id: file_id,
                        storage_backend: self.config.storage_backend.clone(),
                        object_key: existing.object_key.clone(),
                        url: optional_file_storage_public_url(&self.config, &existing.object_key)?,
                        mime_type: Some(existing.mime_type),
                        size_bytes: Some(existing.size_bytes),
                        width: request.width,
                        height: request.height,
                        metadata: request.metadata,
                    };
                    let (nonce, ranges) = attach_file_ownership_proof_token(
                        &mut file,
                        request.user_id,
                        &request.storage_scope,
                        expires_at,
                        &self.config.upload_token_secret,
                        checksum,
                        request.size_bytes,
                    )?;
                    validate_stored_files(std::slice::from_ref(&file))?;
                    return Ok(FileUploadSession {
                        file,
                        upload_required: false,
                        ownership_proof_required: true,
                        ownership_proof_nonce: Some(nonce),
                        ownership_proof_ranges: ranges,
                        ownership_proof_metadata_key: Some(FILE_OWNERSHIP_PROOF_KEY.to_string()),
                        upload_url: None,
                        upload_method: None,
                        upload_headers: Default::default(),
                        expires_at: Some(expires_at),
                        max_size_bytes: request.policy.max_size_bytes,
                    });
                }
            }
        }
        let object_base_path = file_storage_object_base_path(
            &self.config.base_path,
            &request.policy.storage_namespace,
        );
        let object_key = if let (Some(repository), Some(checksum)) =
            (self.repository.as_ref(), checksum_sha256.as_deref())
        {
            match repository
                .get_any_object_by_checksum(
                    &self.config.storage_backend,
                    checksum,
                    request.size_bytes,
                )
                .await?
            {
                Some(existing) => existing.object_key,
                None => file_content_object_key(&object_base_path, checksum),
            }
        } else {
            checksum_sha256.as_deref().map_or_else(
                || {
                    file_object_key(
                        &object_base_path,
                        &request.storage_scope,
                        &server_file_object_id(),
                        &request.mime_type,
                    )
                },
                |checksum| file_content_object_key(&object_base_path, checksum),
            )
        };
        if let (Some(repository), Some(checksum)) =
            (self.repository.as_ref(), checksum_sha256.as_deref())
        {
            repository
                .upsert_pending_object(
                    &self.config.storage_backend,
                    &object_key,
                    &request.mime_type,
                    request.size_bytes,
                    checksum,
                    &serde_json::Value::Object(Default::default()),
                )
                .await?;
        }
        let presigned = self
            .operator
            .presign_write_with(&object_key, StdDuration::from_secs(expires.cast_unsigned()))
            .content_type(&request.mime_type)
            .await
            .map_err(|error| Error::Internal(format!("failed to presign S3 upload: {error}")))?;
        let upload_url = presigned.uri().to_string();
        let public_url = optional_file_storage_public_url(&self.config, &object_key)?;
        let mut file = NewStoredFile {
            id: file_id,
            storage_backend: self.config.storage_backend.clone(),
            object_key,
            url: public_url,
            mime_type: Some(request.mime_type),
            size_bytes: Some(request.size_bytes),
            width: request.width,
            height: request.height,
            metadata: request.metadata,
        };
        attach_file_upload_token(
            &mut file,
            request.user_id,
            &request.storage_scope,
            expires_at,
            &self.config.upload_token_secret,
            checksum_sha256.as_deref(),
            None,
        )?;
        validate_stored_files(std::slice::from_ref(&file))?;
        let upload_headers = presigned
            .header()
            .iter()
            .filter(|(name, _)| name.as_str() != "host")
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        Ok(FileUploadSession {
            file,
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            ownership_proof_metadata_key: None,
            upload_url: Some(upload_url),
            upload_method: Some(presigned.method().to_string()),
            upload_headers,
            expires_at: Some(expires_at),
            max_size_bytes: request.policy.max_size_bytes,
        })
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        mut files: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        validate_stored_files(&files)?;
        for file in &files {
            if file.storage_backend != self.config.storage_backend {
                return Err(Error::InvalidInput(format!(
                    "file storage_backend must be {}",
                    self.config.storage_backend
                )));
            }
        }
        validate_file_upload_tokens(context, &files, &self.config.upload_token_secret)?;
        for file in &files {
            let payload =
                file_upload_token_payload_from_file(file, context.user_id, context.storage_scope)?;
            let checksum = payload
                .get("checksum_sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase);
            if payload
                .get("ownership_proof_required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                let proof = file
                    .metadata
                    .get(FILE_OWNERSHIP_PROOF_KEY)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        Error::InvalidInput("file ownership proof is required".to_string())
                    })?;
                let nonce = payload
                    .get("ownership_proof_nonce")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
                let ranges = ownership_proof_ranges_from_payload(&payload)?;
                let mut chunks = Vec::with_capacity(ranges.len());
                for range in &ranges {
                    chunks.push(self.read_object_range(&file.object_key, range).await?);
                }
                let expected =
                    file_ownership_proof_digest(nonce, &ranges, chunks.iter().map(Vec::as_slice));
                if !constant_time_eq(proof.as_bytes(), expected.as_bytes()) {
                    return Err(Error::InvalidInput(
                        "file ownership proof does not match object".to_string(),
                    ));
                }
                continue;
            }

            let Some(checksum) = checksum else {
                continue;
            };
            let Some(repository) = self.repository.as_ref() else {
                continue;
            };
            if repository
                .object_validated(&self.config.storage_backend, &file.object_key)
                .await?
            {
                continue;
            }
            let bytes = self
                .operator
                .read(&file.object_key)
                .await
                .map_err(|error| {
                    Error::InvalidInput(format!("file object is not readable: {error}"))
                })?;
            let data = bytes.to_vec();
            let expected_size = payload
                .get("size_bytes")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))?;
            if expected_size != i64::try_from(data.len()).unwrap_or(i64::MAX) {
                let _ = self.operator.delete(&file.object_key).await;
                return Err(Error::InvalidInput(
                    "file payload size does not match upload session".to_string(),
                ));
            }
            let actual_checksum = hex::encode(Sha256::digest(&data));
            if !constant_time_eq(actual_checksum.as_bytes(), checksum.as_bytes()) {
                let _ = self.operator.delete(&file.object_key).await;
                return Err(Error::InvalidInput(
                    "file payload checksum does not match upload session".to_string(),
                ));
            }
            let mime_type = file
                .mime_type
                .as_deref()
                .ok_or_else(|| Error::InvalidInput("file mime_type is required".to_string()))?;
            repository
                .upsert_object(
                    &self.config.storage_backend,
                    &file.object_key,
                    mime_type,
                    expected_size,
                    &actual_checksum,
                    &serde_json::Value::Object(Default::default()),
                )
                .await?;
        }
        strip_internal_file_metadata(&mut files);
        Ok(files)
    }

    async fn delete_files(
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
                repository
                    .release_reference(
                        &file.reference_kind,
                        &file.reference_id,
                        &file.storage_backend,
                        &file.object_key,
                    )
                    .await?;
                if repository
                    .object_reference_count(&file.storage_backend, &file.object_key)
                    .await?
                    > 0
                {
                    continue;
                }
            }
            match self.operator.delete(&file.object_key).await {
                Ok(()) => {}
                Err(error) => {
                    failed_count += 1;
                    last_error = Some(error.to_string());
                    crate::metrics::file_storage::FILE_OBJECT_DELETE_FAILURES
                        .with_label_values(&[origin_label, &file.storage_backend])
                        .inc();
                    tracing::warn!(
                        error = %error,
                        object_key = %file.object_key,
                        "failed to delete file object"
                    );
                }
            }
            if let Some(repository) = self.repository.as_ref() {
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
}

fn attach_file_ownership_proof_token(
    file: &mut NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
    expires_at: DateTime<Utc>,
    secret: &str,
    checksum_sha256: &str,
    size_bytes: i64,
) -> Result<(String, Vec<FileOwnershipProofRange>)> {
    let nonce = synctv_common::snanoid!(32);
    let ranges = file_ownership_proof_ranges(checksum_sha256, &nonce, size_bytes);
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

fn attach_file_upload_token(
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

fn validate_file_upload_tokens(
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
    ));
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
    let mut parts = token.split('.');
    let version = parts.next().unwrap_or_default();
    let encoded_payload = parts.next().unwrap_or_default();
    let signature = parts.next().unwrap_or_default();
    if version != FILE_UPLOAD_TOKEN_VERSION || parts.next().is_some() {
        return Err(Error::InvalidInput(
            "invalid file upload session token".to_string(),
        ));
    }
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput("invalid file upload session token".to_string()))?;
    let expected_signature = hex::encode(hmac_sha256(
        file_upload_token_key(user_id, storage_scope, secret).as_bytes(),
        &payload_bytes,
    ));
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

fn file_upload_token_payload_from_file(
    file: &NewStoredFile,
    user_id: UserId,
    storage_scope: &str,
) -> Result<serde_json::Value> {
    let token = file
        .metadata
        .get(FILE_UPLOAD_TOKEN_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::InvalidInput("file upload session token is required".to_string()))?;
    let encoded_payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| Error::InvalidInput("invalid file upload session token".to_string()))?;
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
    let ownership_proof = if payload
        .get("ownership_proof_required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
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

fn file_upload_token_key(user_id: UserId, storage_scope: &str, secret: &str) -> String {
    format!(
        "synctv:file-upload:{}:{}:{}",
        user_id.as_i64(),
        storage_scope,
        secret
    )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn validate_file_metadata(metadata: &serde_json::Value) -> Result<()> {
    if !metadata.is_object() {
        return Err(Error::InvalidInput(
            "file metadata must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

fn strip_internal_file_metadata(files: &mut [NewStoredFile]) {
    for file in files {
        if let Some(metadata) = file.metadata.as_object_mut() {
            metadata.remove(FILE_UPLOAD_TOKEN_KEY);
            metadata.remove(FILE_OWNERSHIP_PROOF_KEY);
        }
    }
}

fn validate_stored_files(files: &[NewStoredFile]) -> Result<()> {
    let mut file_ids = std::collections::HashSet::with_capacity(files.len());
    let mut object_keys = std::collections::HashSet::with_capacity(files.len());
    for file in files {
        if file.id.trim().is_empty() || file.id.chars().count() > 128 {
            return Err(Error::InvalidInput(
                "file id must be between 1 and 128 characters".to_string(),
            ));
        }
        if file.storage_backend.trim().is_empty() || file.object_key.trim().is_empty() {
            return Err(Error::InvalidInput(
                "file storage_backend and object_key are required".to_string(),
            ));
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
        if file.size_bytes.is_some_and(|size| size <= 0)
            || file.width.is_some_and(|width| width <= 0)
            || file.height.is_some_and(|height| height <= 0)
        {
            return Err(Error::InvalidInput(
                "file size and dimensions must be positive".to_string(),
            ));
        }
        validate_file_metadata(&file.metadata)?;
    }
    Ok(())
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
    let Some(checksum) = &request.checksum_sha256 else {
        return Err(Error::InvalidInput(
            "checksum_sha256 is required for file uploads".to_string(),
        ));
    };
    let valid = checksum.len() == 64 && checksum.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err(Error::InvalidInput(
            "checksum_sha256 must be a 64-character hex string".to_string(),
        ));
    }
    if !request.metadata.is_object() {
        return Err(Error::InvalidInput(
            "file metadata must be a JSON object".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_upload_policy(policy: &FileUploadPolicy) -> Result<()> {
    if policy.kind.trim().is_empty()
        || policy.max_size_bytes <= 0
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

fn validate_file_mime_type(policy: &FileUploadPolicy, mime_type: &str) -> Result<()> {
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

fn validate_s3_file_storage_config(config: &S3FileStorageConfig) -> Result<()> {
    if config.endpoint.trim().is_empty()
        || config.access_key_id.trim().is_empty()
        || config.secret_access_key.trim().is_empty()
        || config.bucket.trim().is_empty()
        || config.region.trim().is_empty()
        || config.upload_token_secret.trim().is_empty()
    {
        return Err(Error::InvalidInput(
            "S3 file storage requires endpoint, bucket, region, access_key_id, secret_access_key, and upload_token_secret"
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
    if let Some(public_base_url) = config
        .public_base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        url::Url::parse(public_base_url.trim())
            .map_err(|error| Error::InvalidInput(format!("Invalid S3 public_base_url: {error}")))?;
    }
    Ok(())
}

fn file_id_from_request(client_file_id: Option<&str>) -> String {
    client_file_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || format!("img_{}", synctv_common::snanoid!(16)),
            ToOwned::to_owned,
        )
}

fn server_file_object_id() -> String {
    format!("obj_{}", synctv_common::snanoid!(24))
}

fn normalized_checksum_sha256(checksum: Option<&str>) -> Option<String> {
    checksum.map(|value| value.trim().to_ascii_lowercase())
}

fn file_ownership_proof_ranges(
    checksum_sha256: &str,
    nonce: &str,
    size_bytes: i64,
) -> Vec<FileOwnershipProofRange> {
    if size_bytes <= 0 {
        return Vec::new();
    }
    let range_len = FILE_OWNERSHIP_PROOF_RANGE_BYTES
        .min(i32::try_from(size_bytes).unwrap_or(FILE_OWNERSHIP_PROOF_RANGE_BYTES));
    if size_bytes <= i64::from(range_len) {
        return vec![FileOwnershipProofRange {
            offset: 0,
            length: range_len,
        }];
    }

    let seed = Sha256::digest(format!("{checksum_sha256}:{nonce}").as_bytes());
    let max_start = size_bytes - i64::from(range_len);
    let mut ranges = Vec::with_capacity(FILE_OWNERSHIP_PROOF_RANGE_COUNT);
    for index in 0..FILE_OWNERSHIP_PROOF_RANGE_COUNT {
        let start = index * 8;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&seed[start..start + 8]);
        let offset = (u64::from_be_bytes(bytes) % (max_start.cast_unsigned() + 1)).cast_signed();
        ranges.push(FileOwnershipProofRange {
            offset,
            length: range_len,
        });
    }
    ranges.sort_by_key(|range| range.offset);
    ranges.dedup_by_key(|range| range.offset);
    ranges
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

fn ownership_proof_ranges_from_payload(
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

fn database_file_namespace_base_path(storage_namespace: &str) -> String {
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

fn file_object_key(base_path: &str, storage_scope: &str, file_id: &str, mime_type: &str) -> String {
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

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn encode_database_file_object_key(object_key: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        object_key.as_bytes(),
    )
}

fn decode_database_file_object_key(encoded: &str) -> Result<String> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, encoded)
        .map_err(|_| Error::InvalidInput("invalid file object key".to_string()))?;
    String::from_utf8(bytes).map_err(|_| Error::InvalidInput("invalid file object key".to_string()))
}

fn database_file_object_url(
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
    ));
    let encoded_payload = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload_bytes,
    );
    Ok(format!(
        "{DATABASE_FILE_READ_TOKEN_VERSION}.{encoded_payload}.{signature}"
    ))
}

fn database_file_read_token_storage_backend(token: &str) -> Result<String> {
    decode_versioned_hmac_token_payload(token, DATABASE_FILE_READ_TOKEN_VERSION)?
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid file read token".to_string()))
}

fn validate_database_file_read_token(
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

fn file_upload_token_storage_backend(token: &str) -> Result<String> {
    decode_versioned_hmac_token_payload(token, FILE_UPLOAD_TOKEN_VERSION)?
        .get("storage_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidInput("invalid file upload token".to_string()))
}

fn validate_database_file_upload_token(
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
    let mut parts = token.split('.');
    let version = parts.next().unwrap_or_default();
    let encoded_payload = parts.next().unwrap_or_default();
    let _signature = parts.next().unwrap_or_default();
    if version != expected_version || parts.next().is_some() {
        return Err(Error::InvalidInput("invalid token".to_string()));
    }
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
    let mut parts = token.split('.');
    let version = parts.next().unwrap_or_default();
    let encoded_payload = parts.next().unwrap_or_default();
    let signature = parts.next().unwrap_or_default();
    if version != expected_version || parts.next().is_some() {
        return Err(Error::InvalidInput(error_message.to_string()));
    }
    let payload_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        encoded_payload,
    )
    .map_err(|_| Error::InvalidInput(error_message.to_string()))?;
    let expected_signature = hex::encode(hmac_sha256(key, &payload_bytes));
    if !constant_time_eq(signature.as_bytes(), expected_signature.as_bytes()) {
        return Err(Error::InvalidInput(error_message.to_string()));
    }
    serde_json::from_slice(&payload_bytes)
        .map_err(|_| Error::InvalidInput(error_message.to_string()))
}

fn s3_operator_from_config(config: &S3FileStorageConfig) -> Result<Operator> {
    let mut builder = S3::default()
        .endpoint(config.endpoint.trim())
        .access_key_id(config.access_key_id.trim())
        .secret_access_key(config.secret_access_key.trim())
        .bucket(config.bucket.trim())
        .disable_config_load()
        .disable_ec2_metadata();

    let region = config.region.trim();
    if !region.is_empty() {
        builder = builder.region(region);
    }

    Operator::new(builder)
        .map(opendal::OperatorBuilder::finish)
        .map_err(|error| Error::Internal(format!("failed to initialize S3 file storage: {error}")))
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
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        repository::FileStorageRepository,
        service::file_upload_policies::{chat_image_upload_policy, user_avatar_upload_policy},
    };

    #[tokio::test]
    async fn routed_database_storage_reads_objects_from_token_backend() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let database = Arc::new(DatabaseFileStorageService::new(
            "database",
            repository,
            "test-file-storage-secret",
        ));
        let mut backends: HashMap<String, Arc<dyn FileStorageService>> = HashMap::new();
        backends.insert("database".to_string(), database);
        backends.insert("disabled".to_string(), Arc::new(DisabledFileStorageService));
        let routed = FileStorageBackendRegistry::new(backends)
            .routed("database")
            .expect("database backend should route");

        let payload = b"avatar";
        let checksum = hex::encode(Sha256::digest(payload));
        let session = routed
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: i64::try_from(payload.len()).expect("payload length should fit"),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum.clone()),
                metadata: serde_json::Value::Object(Default::default()),
                policy: user_avatar_upload_policy(),
            })
            .await
            .expect("upload session should be created");
        let object_url = session
            .upload_url
            .as_deref()
            .expect("database upload url should be returned");
        assert!(object_url.starts_with("/api/user/avatar-objects/"));

        let parsed =
            url::Url::parse(&format!("http://localhost{object_url}")).expect("url should parse");
        let encoded_object_key = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .expect("encoded object key should exist");
        let read_token = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.into_owned()))
            .expect("read token should exist");
        let upload_token = session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .expect("upload token should exist");

        routed
            .store_upload_object(
                encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.to_vec(),
            )
            .await
            .expect("object should store");

        let loaded = routed
            .get_object(encoded_object_key, &read_token)
            .await
            .expect("routed storage should read by token backend");
        assert_eq!(loaded.storage_backend, "database");
        assert_eq!(loaded.mime_type, "image/png");
        assert_eq!(loaded.checksum_sha256, checksum);
        assert_eq!(loaded.data, payload);
    }

    #[tokio::test]
    async fn database_storage_rejects_checksum_reuse_when_existing_mime_violates_policy() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let storage = DatabaseFileStorageService::new(
            "database",
            repository.clone(),
            "test-file-storage-secret",
        );
        let payload = b"animated-gif";
        let checksum = hex::encode(Sha256::digest(payload));
        repository
            .upsert_blob(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                payload.to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("blob should be inserted");
        repository
            .upsert_object(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                i64::try_from(payload.len()).expect("payload length should fit"),
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("chat image object should be inserted");

        let err = storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: i64::try_from(payload.len()).expect("payload length should fit"),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: user_avatar_upload_policy(),
            })
            .await
            .expect_err("avatar policy should reject existing GIF reuse");

        assert!(matches!(
            err,
            Error::InvalidInput(message) if message == "user_avatar mime_type is not allowed"
        ));
    }

    #[tokio::test]
    async fn database_storage_allows_checksum_reuse_when_existing_mime_matches_policy() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let storage = DatabaseFileStorageService::new(
            "database",
            repository.clone(),
            "test-file-storage-secret",
        );
        let payload = b"animated-gif";
        let checksum = hex::encode(Sha256::digest(payload));
        repository
            .upsert_blob(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                payload.to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("blob should be inserted");
        repository
            .upsert_object(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                i64::try_from(payload.len()).expect("payload length should fit"),
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("chat image object should be inserted");

        let session = storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "rooms/1/chat/images".to_string(),
                client_file_id: Some("chat-image-1".to_string()),
                mime_type: "image/gif".to_string(),
                size_bytes: i64::try_from(payload.len()).expect("payload length should fit"),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await
            .expect("chat policy should allow GIF reuse");

        assert!(!session.upload_required);
        assert_eq!(session.file.mime_type.as_deref(), Some("image/gif"));
    }

    #[tokio::test]
    async fn database_storage_strips_upload_token_from_prepared_files() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let storage = DatabaseFileStorageService::new(
            "database",
            repository.clone(),
            "test-file-storage-secret",
        );
        let payload = b"avatar";
        let checksum = hex::encode(Sha256::digest(payload));
        let session = storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: i64::try_from(payload.len()).expect("payload length should fit"),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: user_avatar_upload_policy(),
            })
            .await
            .expect("upload session should be created");
        assert!(session.file.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_some());

        let upload_url = session
            .upload_url
            .as_deref()
            .expect("database upload url should be returned");
        let parsed = url::Url::parse(&format!("http://localhost{upload_url}"))
            .expect("upload url should parse");
        let encoded_object_key = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .expect("encoded object key should exist");
        let upload_token = session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .expect("upload token should exist");
        storage
            .store_upload_object(
                encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.to_vec(),
            )
            .await
            .expect("object should store");

        let prepared = storage
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    client_request_id: None,
                },
                vec![session.file],
            )
            .await
            .expect("file should prepare");
        let metadata = &prepared[0].metadata;
        assert!(metadata.get(FILE_UPLOAD_TOKEN_KEY).is_none());
        assert!(metadata.get(FILE_OWNERSHIP_PROOF_KEY).is_none());
        assert_eq!(
            metadata.get("blurhash").and_then(serde_json::Value::as_str),
            Some("abc")
        );
    }

    #[tokio::test]
    async fn database_storage_strips_ownership_proof_from_prepared_files() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let storage = DatabaseFileStorageService::new(
            "database",
            repository.clone(),
            "test-file-storage-secret",
        );
        let payload = b"avatar";
        let checksum = hex::encode(Sha256::digest(payload));
        repository
            .upsert_blob(
                "database",
                "database/users/avatars/avatar.png",
                "image/png",
                payload.to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("blob should be inserted");
        repository
            .upsert_object(
                "database",
                "database/users/avatars/avatar.png",
                "image/png",
                i64::try_from(payload.len()).expect("payload length should fit"),
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("object should be inserted");

        let mut session = storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: i64::try_from(payload.len()).expect("payload length should fit"),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: user_avatar_upload_policy(),
            })
            .await
            .expect("upload session should be created");
        assert!(!session.upload_required);
        assert!(session.file.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_some());
        let nonce = session
            .ownership_proof_nonce
            .as_deref()
            .expect("ownership proof nonce should exist");
        let chunks = ownership_proof_chunks_from_bytes(payload, &session.ownership_proof_ranges)
            .expect("proof chunks should build");
        let proof = file_ownership_proof_digest(
            nonce,
            &session.ownership_proof_ranges,
            chunks.iter().map(Vec::as_slice),
        );
        session
            .file
            .metadata
            .as_object_mut()
            .expect("metadata should be object")
            .insert(
                FILE_OWNERSHIP_PROOF_KEY.to_string(),
                serde_json::Value::String(proof),
            );

        let prepared = storage
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    client_request_id: None,
                },
                vec![session.file],
            )
            .await
            .expect("file should prepare");
        let metadata = &prepared[0].metadata;
        assert!(metadata.get(FILE_UPLOAD_TOKEN_KEY).is_none());
        assert!(metadata.get(FILE_OWNERSHIP_PROOF_KEY).is_none());
        assert_eq!(
            metadata.get("blurhash").and_then(serde_json::Value::as_str),
            Some("abc")
        );
    }

    #[tokio::test]
    async fn database_storage_delete_uses_configured_backend_name() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool.clone()));
        let storage = DatabaseFileStorageService::new(
            "primary_db",
            repository.clone(),
            "test-file-storage-secret",
        );
        repository
            .upsert_blob(
                "primary_db",
                "database/users/avatars/file.webp",
                "image/webp",
                b"avatar".to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("blob should be inserted");
        repository
            .upsert_object(
                "primary_db",
                "database/users/avatars/file.webp",
                "image/webp",
                6,
                &hex::encode(Sha256::digest(b"avatar")),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("object should be inserted");
        let mut tx = pool.begin().await.expect("transaction should begin");
        FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            "primary_db",
            "database/users/avatars/file.webp",
            "user_avatar",
            "user:1",
            None,
            &serde_json::Value::Object(Default::default()),
        )
        .await
        .expect("reference should insert");
        tx.commit().await.expect("transaction should commit");

        storage
            .delete_files(
                FileStorageCleanupOrigin::ReferenceReleased,
                &[FileReferenceTarget {
                    storage_backend: "primary_db".to_string(),
                    object_key: "database/users/avatars/file.webp".to_string(),
                    reference_kind: "user_avatar".to_string(),
                    reference_id: "user:1".to_string(),
                }],
            )
            .await
            .expect("delete should use configured database backend name");

        assert!(!repository
            .blob_exists("primary_db", "database/users/avatars/file.webp")
            .await
            .expect("blob lookup should succeed"));
        assert!(!repository
            .object_exists("primary_db", "database/users/avatars/file.webp")
            .await
            .expect("object lookup should succeed"));
    }

    #[tokio::test]
    async fn database_storage_deletes_unreferenced_object_without_reference_row() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let storage = DatabaseFileStorageService::new(
            "primary_db",
            repository.clone(),
            "test-file-storage-secret",
        );
        repository
            .upsert_blob(
                "primary_db",
                "database/chat/images/orphan.webp",
                "image/webp",
                b"orphan".to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("blob should be inserted");
        repository
            .upsert_object(
                "primary_db",
                "database/chat/images/orphan.webp",
                "image/webp",
                6,
                &hex::encode(Sha256::digest(b"orphan")),
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("object should be inserted");

        storage
            .delete_files(
                FileStorageCleanupOrigin::UnreferencedObject,
                &[FileReferenceTarget {
                    storage_backend: "primary_db".to_string(),
                    object_key: "database/chat/images/orphan.webp".to_string(),
                    reference_kind: "unreferenced_file".to_string(),
                    reference_id: "database/chat/images/orphan.webp".to_string(),
                }],
            )
            .await
            .expect("unreferenced object delete should not require a reference row");

        assert!(!repository
            .object_exists("primary_db", "database/chat/images/orphan.webp")
            .await
            .expect("object lookup should succeed"));
    }

    #[test]
    fn s3_public_url_requires_public_base_url() {
        let mut config = S3FileStorageConfig {
            endpoint: "https://s3.internal.example.com".to_string(),
            access_key_id: "access".to_string(),
            secret_access_key: "secret".to_string(),
            bucket: "synctv-files".to_string(),
            region: "auto".to_string(),
            base_path: "files".to_string(),
            public_base_url: None,
            upload_expires_seconds: 900,
            storage_backend: "s3_private".to_string(),
            upload_token_secret: "secret".to_string(),
        };

        assert_eq!(
            optional_file_storage_public_url(&config, "covers/one.png")
                .expect("optional public URL should evaluate"),
            None
        );
        assert!(matches!(
            file_storage_public_url(&config, "covers/one.png"),
            Err(Error::InvalidInput(_))
        ));

        config.public_base_url = Some("https://cdn.example.com/assets".to_string());
        let url = optional_file_storage_public_url(&config, "covers/one.png")
            .expect("public URL should build")
            .expect("configured public URL should be returned");
        assert_eq!(
            url,
            "https://cdn.example.com/assets/synctv-files/covers/one.png"
        );
    }

    #[tokio::test]
    async fn pending_file_object_is_not_reused_but_can_be_cleaned_up() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let checksum = hex::encode(Sha256::digest(b"pending"));
        repository
            .upsert_pending_object(
                "s3_public",
                "files/sha256/pending.webp",
                "image/webp",
                7,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("pending object should be inserted");

        let reusable = repository
            .get_object_by_checksum("s3_public", &checksum, 7)
            .await
            .expect("checksum lookup should succeed");
        assert!(reusable.is_none());

        let storage = DatabaseFileStorageService::new(
            "s3_public",
            repository.clone(),
            "test-file-storage-secret",
        );
        storage
            .delete_files(
                FileStorageCleanupOrigin::UnreferencedObject,
                &[FileReferenceTarget {
                    storage_backend: "s3_public".to_string(),
                    object_key: "files/sha256/pending.webp".to_string(),
                    reference_kind: "unreferenced_file".to_string(),
                    reference_id: "files/sha256/pending.webp".to_string(),
                }],
            )
            .await
            .expect("pending object should be cleanup-addressable");

        assert!(!repository
            .object_exists("s3_public", "files/sha256/pending.webp")
            .await
            .expect("object lookup should succeed"));
    }

    #[tokio::test]
    async fn pending_file_object_becomes_reusable_after_validation() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let repository = Arc::new(FileStorageRepository::new(pool));
        let checksum = hex::encode(Sha256::digest(b"validated"));
        repository
            .upsert_pending_object(
                "s3_public",
                "files/sha256/validated.webp",
                "image/webp",
                9,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("pending object should be inserted");
        assert!(!repository
            .object_validated("s3_public", "files/sha256/validated.webp")
            .await
            .expect("validated lookup should succeed"));

        repository
            .upsert_object(
                "s3_public",
                "files/sha256/validated.webp",
                "image/webp",
                9,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await
            .expect("object should validate");

        let reusable = repository
            .get_object_by_checksum("s3_public", &checksum, 9)
            .await
            .expect("checksum lookup should succeed")
            .expect("validated object should be reusable");
        assert_eq!(reusable.object_key, "files/sha256/validated.webp");
        assert!(reusable.validated_at.is_some());
    }
}
