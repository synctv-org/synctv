use crate::{
    models::{CompleteFileUploadPart, FileUploadSessionPart},
    service::file_storage::file_part_manifest_digest,
    Error, Result,
};

pub(super) fn completion_parts_from_session_parts(
    parts: &[FileUploadSessionPart],
) -> Result<Vec<CompleteFileUploadPart>> {
    let mut complete_parts = Vec::with_capacity(parts.len());
    for part in parts {
        let etag = part.etag.as_deref().map(str::trim).ok_or_else(|| {
            Error::InvalidInput(
                "S3 multipart completion requires every recorded part ETag".to_string(),
            )
        })?;
        if etag.is_empty() {
            return Err(Error::InvalidInput(
                "S3 multipart completion requires every recorded part ETag".to_string(),
            ));
        }
        complete_parts.push(CompleteFileUploadPart {
            part_number: part.part_number,
            etag: etag.to_string(),
            size_bytes: part.size_bytes,
            checksum_sha256: part.checksum_sha256.clone(),
        });
    }
    complete_parts.sort_by_key(|part| part.part_number);
    Ok(complete_parts)
}

pub(super) fn completed_upload_part_manifest_digest(
    parts: &[FileUploadSessionPart],
    size_bytes: i64,
    part_size_bytes: i64,
) -> Result<String> {
    let manifest_parts = parts
        .iter()
        .map(|part| {
            let checksum = part.checksum_sha256.as_deref().ok_or_else(|| {
                Error::InvalidInput(
                    "S3 multipart completion requires every part checksum_sha256".to_string(),
                )
            })?;
            Ok((part.part_number, part.size_bytes, checksum))
        })
        .collect::<Result<Vec<_>>>()?;
    file_part_manifest_digest(size_bytes, part_size_bytes, manifest_parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;

    fn session_part(
        part_number: i32,
        etag: Option<&str>,
        checksum_sha256: Option<&str>,
    ) -> FileUploadSessionPart {
        FileUploadSessionPart {
            storage_backend: "s3".to_string(),
            upload_session_key: "session".to_string(),
            part_index: part_number - 1,
            part_number,
            offset_bytes: i64::from(part_number - 1) * 4,
            size_bytes: 4,
            checksum_sha256: checksum_sha256.map(ToString::to_string),
            etag: etag.map(ToString::to_string),
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
        }
    }

    #[test]
    fn completion_parts_are_sorted_and_trim_etags() {
        let parts = [
            session_part(2, Some(" \"etag-2\" "), Some("checksum-2")),
            session_part(1, Some("\"etag-1\""), Some("checksum-1")),
        ];

        let completed =
            completion_parts_from_session_parts(&parts).checked("completion parts should build");

        assert_eq!(completed[0].part_number, 1);
        assert_eq!(completed[0].etag, "\"etag-1\"");
        assert_eq!(completed[1].part_number, 2);
        assert_eq!(completed[1].etag, "\"etag-2\"");
    }

    #[test]
    fn completion_parts_require_recorded_etags() {
        let error = completion_parts_from_session_parts(&[session_part(1, None, Some("checksum"))])
            .failed("missing etag should fail");

        assert!(matches!(
            error,
            Error::InvalidInput(message)
                if message == "S3 multipart completion requires every recorded part ETag"
        ));
    }

    #[test]
    fn completed_manifest_digest_requires_checksums() {
        let error =
            completed_upload_part_manifest_digest(&[session_part(1, Some("\"etag\""), None)], 4, 4)
                .failed("missing checksum should fail");

        assert!(matches!(
            error,
            Error::InvalidInput(message)
                if message == "S3 multipart completion requires every part checksum_sha256"
        ));
    }
}
