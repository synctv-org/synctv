use futures::StreamExt;
use sha2::{Digest, Sha256};

use super::S3CompatibleFileStorageService;
use crate::{
    models::{
        FileBlob, FileBlobCompression, FileByteRange, FileMetadata, FileObjectDownload,
        FileObjectMetadata, FileOwnershipProofRange, GetFileObject,
    },
    repository::UpsertFileObject,
    service::file_storage::{
        decode_file_object_key, encode_file_object_key, payload_len_i64,
        validate_file_object_read_token, FileObjectReader,
    },
    Error, Result,
};

impl S3CompatibleFileStorageService {
    pub(super) async fn read_object_range(
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

    pub(super) async fn delete_invalid_upload_object(
        &self,
        object_key: &str,
        reason: &'static str,
    ) {
        if let Err(error) = self.operator.delete(object_key).await {
            tracing::warn!(
                storage_backend = %self.config.storage_backend,
                object_key,
                reason,
                error = %error,
                "Failed to delete invalid uploaded file object"
            );
        }
    }

    pub(super) async fn validate_completed_s3_object_size(
        &self,
        object_key: &str,
        expected_size: i64,
    ) -> Result<()> {
        let metadata = self.operator.stat(object_key).await.map_err(|error| {
            Error::InvalidInput(format!("file object is not readable: {error}"))
        })?;
        let size = i64::try_from(metadata.content_length())
            .map_err(|_| Error::Internal("file object size exceeds i64::MAX".to_string()))?;
        if size != expected_size {
            self.delete_invalid_upload_object(object_key, "size_mismatch")
                .await;
            return Err(Error::InvalidInput(
                "file payload size does not match upload session".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) async fn stat_object(&self, object_key: &str) -> Option<opendal::Metadata> {
        #[cfg(test)]
        if self.test_force_stat_error {
            return None;
        }
        match self.operator.stat(object_key).await {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                tracing::debug!(
                    storage_backend = %self.config.storage_backend,
                    object_key,
                    error = %error,
                    "S3 object stat failed; falling back to object read path"
                );
                None
            }
        }
    }

    pub(super) async fn object_download(
        &self,
        request: GetFileObject,
    ) -> Result<FileObjectDownload> {
        let object_key = decode_file_object_key(&request.encoded_object_key)?;
        validate_file_object_read_token(
            &self.config.storage_backend,
            &object_key,
            &request.read_token,
            &self.config.upload_token_secret,
        )?;
        let object = if let Some(repository) = self.repository.as_ref() {
            repository
                .get_object(&self.config.storage_backend, &object_key)
                .await?
        } else {
            None
        };
        let stat = if object.is_some() {
            None
        } else {
            self.stat_object(&object_key).await
        };
        let total_size_bytes = match object.as_ref() {
            Some(object) => Some(
                u64::try_from(object.size_bytes)
                    .map_err(|_| Error::Internal("file object size is invalid".to_string()))?,
            ),
            None => stat.as_ref().map(opendal::Metadata::content_length),
        };
        let Some(total_size_bytes) = total_size_bytes else {
            if request.range.is_some() {
                return Err(Error::InvalidInput(
                    "file range requires known object size".to_string(),
                ));
            }
            let data = self
                .operator
                .read(&object_key)
                .await
                .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
            let size_bytes = payload_len_i64(data.len())?;
            let mime_type = object
                .as_ref()
                .map(|object| object.mime_type.clone())
                .or_else(|| {
                    stat.as_ref()
                        .and_then(|stat| stat.content_type().map(ToString::to_string))
                })
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let content_manifest_sha256 = object
                .as_ref()
                .map(|object| object.content_manifest_sha256.clone())
                .unwrap_or_default();
            let metadata = object
                .as_ref()
                .map_or_else(Default::default, |object| object.metadata.clone());
            let created_at = object
                .as_ref()
                .map_or_else(|| crate::SystemClock.now(), |object| object.created_at);
            return Ok(FileObjectDownload {
                metadata: FileObjectMetadata {
                    storage_backend: self.config.storage_backend.clone(),
                    object_key,
                    mime_type,
                    size_bytes,
                    total_size_bytes: size_bytes,
                    content_manifest_sha256,
                    compression: FileBlobCompression::None,
                    range: None,
                    metadata,
                    created_at,
                },
                stream: futures::stream::once(async move { Ok(data.to_bytes()) }).boxed(),
            });
        };
        let requested_range = request.range;
        let range = super::super::resolve_file_range(requested_range, total_size_bytes)?;
        let storage_range = match requested_range {
            None => opendal::BytesRange::default(),
            Some(crate::models::FileRangeRequest::Exact(_)) => {
                let resolved = range
                    .ok_or_else(|| Error::Internal("resolved file range is missing".to_string()))?;
                opendal::BytesRange::new(resolved.start, Some(resolved.size_bytes()))
            }
            Some(crate::models::FileRangeRequest::From { start }) => {
                opendal::BytesRange::new(start, None)
            }
            Some(crate::models::FileRangeRequest::Suffix { length }) => {
                opendal::BytesRange::suffix(length)
            }
        };
        let mime_type = object
            .as_ref()
            .map(|object| object.mime_type.clone())
            .or_else(|| {
                stat.as_ref()
                    .and_then(|stat| stat.content_type().map(ToString::to_string))
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let content_manifest_sha256 = object
            .as_ref()
            .map(|object| object.content_manifest_sha256.clone())
            .unwrap_or_default();
        let metadata = object
            .as_ref()
            .map_or_else(Default::default, |object| object.metadata.clone());
        let created_at = object
            .as_ref()
            .map_or_else(|| crate::SystemClock.now(), |object| object.created_at);
        let reader = self
            .operator
            .reader(&object_key)
            .await
            .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
        let bytes_stream = reader
            .into_bytes_stream(storage_range)
            .await
            .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
        let stream = bytes_stream
            .map(|chunk| {
                chunk.map_err(|error| {
                    Error::Internal(format!("failed to read S3 file object stream: {error}"))
                })
            })
            .boxed();
        Ok(FileObjectDownload {
            metadata: FileObjectMetadata {
                storage_backend: self.config.storage_backend.clone(),
                object_key,
                mime_type,
                size_bytes: i64::try_from(
                    range.map_or(total_size_bytes, FileByteRange::size_bytes),
                )
                .map_err(|_| {
                    Error::Internal("file range size exceeds storage limits".to_string())
                })?,
                total_size_bytes: i64::try_from(total_size_bytes).map_err(|_| {
                    Error::Internal("file object size exceeds storage limits".to_string())
                })?,
                content_manifest_sha256,
                compression: FileBlobCompression::None,
                range,
                metadata,
                created_at,
            },
            stream,
        })
    }

    pub(super) async fn object_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<FileBlob> {
        self.validate_storage_backend(storage_backend)?;
        let read_token = super::super::file_object_read_token(
            &self.config.storage_backend,
            object_key,
            &self.config.upload_token_secret,
        )?;
        let download = self
            .object_download(GetFileObject {
                encoded_object_key: encode_file_object_key(object_key),
                read_token,
                range: None,
            })
            .await?;
        super::super::collect_file_object_download(download).await
    }

    pub(super) async fn object_reader_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
    ) -> Result<FileObjectReader> {
        self.validate_storage_backend(storage_backend)?;
        let size_bytes = if let Some(repository) = self.repository.as_ref() {
            repository
                .get_object(&self.config.storage_backend, object_key)
                .await?
                .map(|object| object.size_bytes)
        } else {
            None
        };
        let size_bytes = if let Some(size_bytes) = size_bytes {
            size_bytes
        } else {
            let stat = self
                .operator
                .stat(object_key)
                .await
                .map_err(|error| Error::NotFound(format!("File object not found: {error}")))?;
            i64::try_from(stat.content_length())
                .map_err(|_| Error::Internal("file object size exceeds i64::MAX".to_string()))?
        };
        let operator = self.operator.clone();
        let object_key = object_key.to_string();
        let reader = super::super::read_seek::RangeSeekReader::new(
            size_bytes,
            usize::try_from(size_bytes.min(1024 * 1024)).unwrap_or(1024 * 1024),
            move |offset, length| {
                let operator = operator.clone();
                let object_key = object_key.clone();
                Box::pin(async move {
                    let start = u64::try_from(offset)
                        .map_err(|_| Error::InvalidInput("file range is invalid".to_string()))?;
                    let end = start
                        .checked_add(u64::try_from(length).map_err(|_| {
                            Error::InvalidInput("file range is invalid".to_string())
                        })?)
                        .ok_or_else(|| Error::InvalidInput("file range is invalid".to_string()))?;
                    let bytes = operator
                        .read_with(&object_key)
                        .range(start..end)
                        .await
                        .map_err(|error| {
                            Error::Internal(format!("failed to read S3 file object range: {error}"))
                        })?;
                    Ok(bytes.to_bytes())
                })
            },
        )?;
        Ok(Box::new(reader))
    }

    pub(super) async fn write_object_by_key(
        &self,
        storage_backend: &str,
        object_key: &str,
        mime_type: &str,
        data: Vec<u8>,
        metadata: FileMetadata,
    ) -> Result<FileBlob> {
        self.validate_storage_backend(storage_backend)?;
        if data.is_empty() {
            return Err(Error::InvalidInput(
                "file object payload must be non-empty".to_string(),
            ));
        }
        let size_bytes = payload_len_i64(data.len())?;
        let checksum = hex::encode(Sha256::digest(&data));
        self.operator
            .write(object_key, data.clone())
            .await
            .map_err(|error| {
                Error::Internal(format!("failed to write S3 file object variant: {error}"))
            })?;
        self.repository()?
            .upsert_object(UpsertFileObject {
                storage_backend: &self.config.storage_backend,
                object_key,
                mime_type,
                size_bytes,
                content_manifest_sha256: &checksum,
                metadata: &metadata,
            })
            .await?;
        Ok(FileBlob {
            storage_backend: self.config.storage_backend.clone(),
            object_key: object_key.to_string(),
            mime_type: mime_type.to_string(),
            size_bytes,
            total_size_bytes: size_bytes,
            content_manifest_sha256: checksum,
            compression: FileBlobCompression::None,
            range: None,
            data,
            metadata,
            created_at: crate::SystemClock.now(),
        })
    }

    fn validate_storage_backend(&self, storage_backend: &str) -> Result<()> {
        if storage_backend != self.config.storage_backend {
            return Err(Error::InvalidInput(format!(
                "file storage_backend must be {}",
                self.config.storage_backend
            )));
        }
        Ok(())
    }
}
