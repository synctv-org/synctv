use std::{collections::HashMap, sync::Arc};

use crate::{
    models::{
        CompleteFileUploadSession, CompleteFileUploadSessionResult, CreateFileUploadSession,
        FileBlob, FileReferenceTarget, FileUploadSessionCreateResult, GetFileObject, NewStoredFile,
        StoreFileUpload, StoreFileUploadResult, SubmittedFileReference, SubmittedFileReferenceKind,
    },
    service::file_storage::{
        database_file_read_token_storage_backend, file_upload_token_storage_backend,
        upload_session_reference_target, validation::validate_stored_files, CreateFileReuseGrant,
        FileReuseGrant, FileStorageCleanupOrigin, FileStorageContext, FileStorageService,
        ValidatedFileReuseGrant,
    },
    Error, Result,
};

#[derive(Clone)]
pub struct FileStorageBackendRegistry {
    backends: Arc<HashMap<String, Arc<dyn FileStorageService>>>,
}

#[derive(Clone)]
pub struct RoutedFileStorageService {
    registry: FileStorageBackendRegistry,
    write_backend: String,
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
    ) -> Result<FileUploadSessionCreateResult> {
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

    async fn prepare_submitted_files(
        &self,
        context: FileStorageContext<'_>,
        files: Vec<SubmittedFileReference>,
    ) -> Result<Vec<NewStoredFile>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }

        let Some(repository) = self
            .registry
            .backends
            .values()
            .find_map(|backend| backend.repository())
        else {
            return Err(Error::InvalidInput(
                "file upload references are not supported by this storage".to_string(),
            ));
        };
        let mut prepared = Vec::with_capacity(files.len());
        let mut by_backend: HashMap<String, Vec<SubmittedFileReference>> = HashMap::new();
        for file in files {
            let backend_name = match file.kind {
                SubmittedFileReferenceKind::Upload => {
                    let id = file.id.trim();
                    if id.is_empty() {
                        return Err(Error::InvalidInput(
                            "file reference id is required".to_string(),
                        ));
                    }
                    let (reference_kind, reference_id) = upload_session_reference_target(id);
                    let session = repository
                        .get_upload_session_by_reference(reference_kind, &reference_id)
                        .await?
                        .ok_or_else(|| {
                            Error::InvalidInput("file reference was not found".to_string())
                        })?;
                    session.storage_backend
                }
                SubmittedFileReferenceKind::Reuse => self.write_backend.clone(),
            };
            by_backend.entry(backend_name).or_default().push(file);
        }
        for (backend_name, backend_files) in by_backend {
            let mut backend_prepared = self
                .registry
                .backend(&backend_name)?
                .prepare_submitted_files(context, backend_files)
                .await?;
            prepared.append(&mut backend_prepared);
        }
        Ok(prepared)
    }

    fn create_reuse_grant(&self, request: CreateFileReuseGrant<'_>) -> Result<FileReuseGrant> {
        self.registry
            .backend(&self.write_backend)?
            .create_reuse_grant(request)
    }

    async fn validate_reuse_grant(
        &self,
        token: &str,
        context: FileStorageContext<'_>,
    ) -> Result<ValidatedFileReuseGrant> {
        self.registry
            .backend(&self.write_backend)?
            .validate_reuse_grant(token, context)
            .await
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

    async fn store_upload(&self, upload: StoreFileUpload) -> Result<StoreFileUploadResult> {
        let backend_name = file_upload_token_storage_backend(&upload.upload_token)?;
        self.registry
            .backend(&backend_name)?
            .store_upload(upload)
            .await
    }

    async fn complete_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        let backend_name = file_upload_token_storage_backend(&request.upload_token)?;
        self.registry
            .backend(&backend_name)?
            .complete_upload_session(request)
            .await
    }

    async fn get_object(&self, request: GetFileObject) -> Result<FileBlob> {
        let backend_name = database_file_read_token_storage_backend(&request.read_token)?;
        self.registry
            .backend(&backend_name)?
            .get_object(request)
            .await
    }
}
