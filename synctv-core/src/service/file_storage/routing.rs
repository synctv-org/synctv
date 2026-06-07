use std::{collections::HashMap, sync::Arc};

use crate::{
    models::{
        CreateFileUploadSession, FileBlob, FileReferenceTarget, FileUploadSession, NewStoredFile,
    },
    service::file_storage::{
        database_file_read_token_storage_backend, file_upload_token_storage_backend,
        validation::validate_stored_files, FileStorageCleanupOrigin, FileStorageContext,
        FileStorageService,
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
