use std::sync::Arc;

use sha2::{Digest, Sha256};
use synctv_core::{
    models::{
        CreateFileUploadSession, FileReferenceTarget, FileUploadManifestPart, FileUploadRange,
        FileUploadSession, FileUploadSessionCreateResult, StoreFileUpload, StoreFileUploadResult,
        UserId,
    },
    repository::FileStorageRepository,
    service::{
        FileStorageCleanupOrigin, FileStorageService, S3CompatibleFileStorageService,
        S3FileStorageConfig,
    },
};
use synctv_core_testing::{ok, some, start_rustfs, test_rustfs_base_path};

const STORAGE_BACKEND: &str = "rustfs_s3";
const UPLOAD_TOKEN_SECRET: &str = "rustfs-e2e-file-storage-secret";
const PART_SIZE_BYTES: i64 = 8 * 1024 * 1024;

fn payload_size(payload: &[u8]) -> i64 {
    ok(i64::try_from(payload.len()), "payload length should fit")
}

fn manifest_parts_from_payload(
    payload: &[u8],
    part_size_bytes: i64,
) -> Vec<FileUploadManifestPart> {
    let size_bytes = payload_size(payload);
    let part_count = (size_bytes + part_size_bytes - 1) / part_size_bytes;
    (1..=part_count)
        .map(|part_number| {
            let offset_bytes = (part_number - 1) * part_size_bytes;
            let size_bytes = (size_bytes - offset_bytes).min(part_size_bytes);
            let start = usize::try_from(offset_bytes).expect("part offset should fit");
            let end = start + usize::try_from(size_bytes).expect("part size should fit");
            FileUploadManifestPart {
                part_number: i32::try_from(part_number).expect("part number should fit"),
                offset_bytes,
                size_bytes,
                checksum_sha256: hex::encode(Sha256::digest(&payload[start..end])),
            }
        })
        .collect()
}

fn pdf_upload_policy(max_size_bytes: i64) -> synctv_core::models::FileUploadPolicy {
    synctv_core::models::FileUploadPolicy {
        kind: "rustfs_e2e_file".to_string(),
        object_kind: synctv_core::models::FileObjectKind::Generic,
        max_size_bytes,
        max_width: None,
        max_height: None,
        require_image_dimensions: false,
        max_audio_duration_seconds: None,
        max_audio_bitrate_bps: None,
        require_audio_metadata: false,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec!["application/pdf".to_string()],
        storage_namespace: "e2e/files".to_string(),
    }
}

fn multipart_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(10 * 1024 * 1024);
    payload.extend(vec![
        b'a';
        usize::try_from(PART_SIZE_BYTES)
            .expect("part size should fit")
    ]);
    payload.extend(vec![b'b'; 2 * 1024 * 1024]);
    payload
}

fn expect_upload_session(
    result: FileUploadSessionCreateResult,
    context: &str,
) -> FileUploadSession {
    result.into_session().unwrap_or_else(|| {
        std::panic::panic_any(format!(
            "{context}: expected upload session, got upload plan"
        ))
    })
}

async fn rustfs_storage(
    label: &str,
) -> (
    synctv_core_testing::RustfsContainer,
    synctv_core_testing::TestContainer,
    Arc<FileStorageRepository>,
    S3CompatibleFileStorageService,
) {
    let ((rustfs, rustfs_config), (postgres, pool)) =
        tokio::join!(start_rustfs(), synctv_core_testing::create_test_pool());
    assert!(rustfs_config.bucket.starts_with("synctv-"));
    assert_eq!(rustfs.bucket(), rustfs_config.bucket);
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: rustfs_config.endpoint.clone(),
                access_key_id: rustfs_config.access_key_id,
                secret_access_key: rustfs_config.secret_access_key,
                bucket: rustfs_config.bucket,
                region: rustfs_config.region,
                base_path: test_rustfs_base_path(label),
                public_base_url: Some(rustfs_config.endpoint),
                upload_expires_seconds: 900,
                storage_backend: STORAGE_BACKEND.to_string(),
                upload_token_secret: UPLOAD_TOKEN_SECRET.to_string(),
            },
            Some(repository.clone()),
        ),
        "RustFS S3 storage should build",
    );
    (rustfs, postgres, repository, storage)
}

fn upload_request(
    payload: &[u8],
    part_size_bytes: i64,
    client_file_id: &str,
) -> CreateFileUploadSession {
    CreateFileUploadSession {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/files".to_string(),
        client_file_id: Some(client_file_id.to_string()),
        filename: Some("rustfs-e2e.pdf".to_string()),
        mime_type: "application/pdf".to_string(),
        size_bytes: payload_size(payload),
        width: None,
        height: None,
        duration_seconds: None,
        bitrate_bps: None,
        parts: manifest_parts_from_payload(payload, part_size_bytes),
        metadata: synctv_core::models::FileMetadata::default(),
        policy: pdf_upload_policy(payload_size(payload)),
    }
}

#[tokio::test]
#[ignore = "requires Docker and starts shared PostgreSQL/RustFS testcontainers"]
async fn rustfs_s3_server_mediated_multipart_upload_reads_and_deletes_object() {
    let (_rustfs, postgres, repository, storage) = rustfs_storage("multipart-roundtrip").await;
    let payload = multipart_payload();
    let request = upload_request(&payload, PART_SIZE_BYTES, "rustfs-roundtrip");
    let session = expect_upload_session(
        ok(
            storage.create_upload_session(request).await,
            "RustFS upload session should be created",
        ),
        "RustFS upload session",
    );
    assert_eq!(session.part_size_bytes, PART_SIZE_BYTES);
    assert_eq!(session.part_urls.len(), 2);
    let upload_token = some(
        session
            .upload_headers
            .get("x-synctv-file-upload-token")
            .cloned(),
        "upload token header should exist",
    );

    let first_part =
        payload[..usize::try_from(PART_SIZE_BYTES).expect("part size should fit")].to_vec();
    let first = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token: upload_token.clone(),
                content_type: Some("application/pdf".to_string()),
                range: Some(FileUploadRange {
                    start: 0,
                    end_inclusive: PART_SIZE_BYTES - 1,
                    total_size: payload_size(&payload),
                }),
                data: first_part,
            })
            .await,
        "first RustFS S3 multipart part should upload",
    );
    let StoreFileUploadResult::PartAccepted {
        uploaded_size_bytes,
        uploaded_parts,
    } = first
    else {
        panic!("first part should be accepted");
    };
    assert_eq!(uploaded_size_bytes, PART_SIZE_BYTES);
    assert_eq!(uploaded_parts, vec![1]);

    let second_part =
        payload[usize::try_from(PART_SIZE_BYTES).expect("part size should fit")..].to_vec();
    let completed = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: session.encoded_object_key,
                upload_token,
                content_type: Some("application/pdf".to_string()),
                range: Some(FileUploadRange {
                    start: PART_SIZE_BYTES,
                    end_inclusive: payload_size(&payload) - 1,
                    total_size: payload_size(&payload),
                }),
                data: second_part,
            })
            .await,
        "second RustFS S3 multipart part should complete object",
    );
    let StoreFileUploadResult::Complete(blob) = completed else {
        panic!("second part should complete object");
    };
    assert_eq!(blob.storage_backend, STORAGE_BACKEND);
    assert_eq!(blob.size_bytes, payload_size(&payload));
    assert!(ok(
        repository
            .object_validated(STORAGE_BACKEND, &blob.object_key)
            .await,
        "object validation state should load"
    ));

    let loaded = ok(
        storage
            .get_object_by_key(STORAGE_BACKEND, &blob.object_key)
            .await,
        "RustFS object should read through storage service",
    );
    assert_eq!(loaded.data, payload);
    assert_eq!(loaded.total_size_bytes, payload_size(&payload));

    ok(
        storage
            .delete_files(
                FileStorageCleanupOrigin::UnreferencedObject,
                &[FileReferenceTarget {
                    storage_backend: STORAGE_BACKEND.to_string(),
                    object_key: blob.object_key.clone(),
                    reference_kind: "rustfs_e2e".to_string(),
                    reference_id: "roundtrip".to_string(),
                }],
            )
            .await,
        "RustFS object should delete",
    );
    assert!(
        !ok(
            repository
                .object_exists(STORAGE_BACKEND, &blob.object_key)
                .await,
            "object existence should load"
        ),
        "deleted object DB row should be removed"
    );
    assert!(
        storage
            .get_object_by_key(STORAGE_BACKEND, &blob.object_key)
            .await
            .is_err(),
        "deleted RustFS object should not read"
    );

    postgres.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker and starts shared PostgreSQL/RustFS testcontainers"]
async fn rustfs_s3_rejects_part_outside_declared_manifest() {
    let (_rustfs, postgres, repository, storage) = rustfs_storage("manifest-bounds").await;
    let payload = multipart_payload();
    let request = upload_request(&payload, PART_SIZE_BYTES, "rustfs-manifest-bounds");
    let session = expect_upload_session(
        ok(
            storage.create_upload_session(request).await,
            "RustFS upload session should be created",
        ),
        "RustFS upload session",
    );
    let upload_token = some(
        session
            .upload_headers
            .get("x-synctv-file-upload-token")
            .cloned(),
        "upload token header should exist",
    );
    let upload_session_key = some(
        ok(
            repository
                .get_pending_upload_session_by_object_key(STORAGE_BACKEND, &session.file.object_key)
                .await,
            "upload session should load by object key",
        ),
        "pending upload session should exist",
    )
    .upload_session_key;
    let mut metadata = ok(
        repository
            .get_upload_session(STORAGE_BACKEND, &upload_session_key)
            .await,
        "upload session metadata should load",
    )
    .expect("upload session should exist")
    .metadata;
    assert_eq!(metadata.manifest_parts.len(), 2);
    metadata.manifest_parts.pop();
    ok(
        repository
            .update_upload_session_metadata(STORAGE_BACKEND, &upload_session_key, &metadata)
            .await,
        "upload session manifest should update",
    );
    let second_part =
        payload[usize::try_from(PART_SIZE_BYTES).expect("part size should fit")..].to_vec();
    let error = storage
        .store_upload(StoreFileUpload {
            encoded_object_key: session.encoded_object_key,
            upload_token,
            content_type: Some("application/pdf".to_string()),
            range: Some(FileUploadRange {
                start: PART_SIZE_BYTES,
                end_inclusive: payload_size(&payload) - 1,
                total_size: payload_size(&payload),
            }),
            data: second_part,
        })
        .await
        .expect_err("out-of-manifest part should fail");
    assert!(
        error
            .to_string()
            .contains("file upload part is not in manifest"),
        "unexpected error: {error}"
    );
    assert!(
        !ok(
            repository
                .object_validated(STORAGE_BACKEND, &session.file.object_key)
                .await,
            "object validation state should load"
        ),
        "invalid upload should not validate object"
    );

    postgres.cleanup().await;
}
