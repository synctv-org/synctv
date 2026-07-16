use super::{multipart_transport, S3CompatibleFileStorageService};
use crate::{models::CompleteFileUploadPart, Error, Result};

#[cfg(test)]
use sha2::{Digest, Sha256};

impl S3CompatibleFileStorageService {
    pub(super) async fn create_multipart_upload(
        &self,
        object_key: &str,
        mime_type: &str,
    ) -> Result<String> {
        #[cfg(test)]
        if let Some(upload_id) = &self.test_multipart_upload_id {
            let _ = (object_key, mime_type);
            return Ok(upload_id.clone());
        }

        multipart_transport::create_s3_multipart_upload(
            &self.config,
            &self.http_client,
            object_key,
            mime_type,
        )
        .await
    }

    pub(super) async fn complete_s3_multipart_upload(
        &self,
        object_key: &str,
        upload_id: &str,
        parts: &[CompleteFileUploadPart],
    ) -> Result<()> {
        #[cfg(test)]
        if self.test_multipart_upload_id.is_some() {
            let _ = (object_key, upload_id);
            return Ok(());
        }

        multipart_transport::complete_s3_multipart_upload(
            &self.config,
            &self.http_client,
            object_key,
            upload_id,
            parts,
        )
        .await
    }

    pub(super) async fn abort_s3_multipart_upload(
        &self,
        object_key: &str,
        upload_id: &str,
    ) -> Result<()> {
        #[cfg(test)]
        if self.test_multipart_upload_id.is_some() {
            let _ = (object_key, upload_id);
            return Ok(());
        }

        multipart_transport::abort_s3_multipart_upload(
            &self.config,
            &self.http_client,
            object_key,
            upload_id,
        )
        .await
    }

    pub(super) async fn upload_s3_multipart_part(
        &self,
        object_key: &str,
        upload_id: &str,
        part_number: i32,
        checksum_sha256: &str,
        #[cfg(test)] offset_bytes: i64,
        #[cfg(not(test))] _offset_bytes: i64,
        data: bytes::Bytes,
    ) -> Result<String> {
        if part_number <= 0 {
            return Err(Error::InvalidInput(
                "S3 multipart upload requires a positive part number".to_string(),
            ));
        }

        #[cfg(test)]
        if self.test_multipart_upload_id.is_some() {
            let _ = upload_id;
            let offset = usize::try_from(offset_bytes).map_err(|_| {
                Error::InvalidInput("S3 multipart upload part offset is invalid".to_string())
            })?;
            let mut object = self
                .operator
                .read(object_key)
                .await
                .map(|bytes| bytes.to_vec())
                .unwrap_or_default();
            if object.len() < offset {
                object.resize(offset, 0);
            }
            let end = offset.checked_add(data.len()).ok_or_else(|| {
                Error::Internal("S3 multipart test object size overflow".to_string())
            })?;
            if object.len() < end {
                object.resize(end, 0);
            }
            object[offset..end].copy_from_slice(&data);
            self.operator
                .write_with(object_key, object)
                .await
                .map_err(|error| {
                    Error::Internal(format!("failed to write test S3 upload part: {error}"))
                })?;
            return Ok(format!("\"{}\"", hex::encode(Sha256::digest(&data))));
        }

        multipart_transport::upload_s3_multipart_part(
            &self.config,
            &self.http_client,
            object_key,
            upload_id,
            part_number,
            checksum_sha256,
            data,
        )
        .await
    }
}
