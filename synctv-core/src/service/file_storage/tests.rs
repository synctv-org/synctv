use std::{collections::HashMap, sync::Arc};

use futures::TryStreamExt;
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    models::{
        CompleteFileUploadPart, CompleteFileUploadSession, FileByteRange, FileRangeRequest,
        FileUploadPolicy, FileUploadRange, FileUploadSession, GetFileObject, StoreFileUpload,
        StoreFileUploadResult,
    },
    repository::{FileStorageRepository, UpsertFileObject},
    service::file_upload_policies::{chat_attachment_upload_policy, user_avatar_upload_policy},
};

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

fn payload_size(payload: &[u8]) -> i64 {
    ok(i64::try_from(payload.len()), "payload length should fit")
}

fn single_manifest_part(
    size_bytes: i64,
    checksum_sha256: impl Into<String>,
) -> Vec<FileUploadManifestPart> {
    vec![FileUploadManifestPart {
        part_number: 1,
        offset_bytes: 0,
        size_bytes,
        checksum_sha256: checksum_sha256.into(),
    }]
}

fn manifest_parts_from_payload(payload: &[u8], max_size_bytes: i64) -> Vec<FileUploadManifestPart> {
    let size_bytes = payload_size(payload);
    let part_size_bytes = upload_session_part_size(max_size_bytes);
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

fn content_manifest_sha256_from_parts(
    size_bytes: i64,
    max_size_bytes: i64,
    parts: &[FileUploadManifestPart],
) -> String {
    ok(
        file_part_manifest_digest(
            size_bytes,
            upload_session_part_size(max_size_bytes),
            parts.iter().map(|part| {
                (
                    part.part_number,
                    part.size_bytes,
                    part.checksum_sha256.as_str(),
                )
            }),
        ),
        "content manifest digest should build",
    )
}

fn manifest_for_payload(
    payload: &[u8],
    max_size_bytes: i64,
) -> (Vec<FileUploadManifestPart>, String) {
    let parts = manifest_parts_from_payload(payload, max_size_bytes);
    let digest = content_manifest_sha256_from_parts(payload_size(payload), max_size_bytes, &parts);
    (parts, digest)
}

fn single_part_manifest_for_payload(
    payload: &[u8],
    max_size_bytes: i64,
) -> (Vec<FileUploadManifestPart>, String) {
    manifest_for_payload(payload, max_size_bytes)
}

struct UploadRequestInput<'a> {
    user_id: UserId,
    storage_scope: &'a str,
    client_file_id: &'a str,
    filename: Option<&'a str>,
    mime_type: &'a str,
    payload: &'a [u8],
    width: Option<i32>,
    height: Option<i32>,
    metadata: serde_json::Value,
    policy: FileUploadPolicy,
}

fn upload_request(input: UploadRequestInput<'_>) -> (CreateFileUploadSession, String) {
    let parts = manifest_parts_from_payload(input.payload, input.policy.max_size_bytes);
    let content_manifest_sha256 = content_manifest_sha256_from_parts(
        payload_size(input.payload),
        input.policy.max_size_bytes,
        &parts,
    );
    (
        CreateFileUploadSession {
            user_id: input.user_id,
            storage_scope: input.storage_scope.to_string(),
            client_file_id: Some(input.client_file_id.to_string()),
            filename: input.filename.map(str::to_string),
            mime_type: input.mime_type.to_string(),
            size_bytes: payload_size(input.payload),
            width: input.width,
            height: input.height,
            parts,
            metadata: input.metadata,
            policy: input.policy,
        },
        content_manifest_sha256,
    )
}

fn simple_upload_request(
    user_id: UserId,
    storage_scope: &str,
    client_file_id: &str,
    mime_type: &str,
    payload: &[u8],
    policy: FileUploadPolicy,
) -> (CreateFileUploadSession, String) {
    upload_request(UploadRequestInput {
        user_id,
        storage_scope,
        client_file_id,
        filename: None,
        mime_type,
        payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    })
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

fn oversized_test_upload_policy() -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "test_file".to_string(),
        max_size_bytes: i64::try_from(MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES)
            .expect("part cap should fit")
            + 1,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec!["application/pdf".to_string()],
        storage_namespace: "test/files".to_string(),
        database_object_route_prefix: "/api/test/file-objects".to_string(),
    }
}

fn large_test_upload_policy(max_size_bytes: i64, mime_type: &str) -> FileUploadPolicy {
    FileUploadPolicy {
        kind: "test_file".to_string(),
        max_size_bytes,
        allowed_mime_prefixes: Vec::new(),
        allowed_mime_types: vec![mime_type.to_string()],
        storage_namespace: "test/files".to_string(),
        database_object_route_prefix: "/api/test/file-objects".to_string(),
    }
}

async fn upsert_uncompressed_blob(
    repository: &FileStorageRepository,
    storage_backend: &str,
    object_key: &str,
    mime_type: &str,
    payload: &[u8],
) {
    let part_checksum_sha256 = hex::encode(Sha256::digest(payload));
    let content_manifest_sha256 = content_manifest_sha256_from_parts(
        payload_size(payload),
        payload_size(payload),
        &single_manifest_part(payload_size(payload), part_checksum_sha256.clone()),
    );
    let metadata = serde_json::Value::Object(Default::default());
    ok(
        repository
            .upsert_object(UpsertFileObject {
                storage_backend,
                object_key,
                mime_type,
                size_bytes: payload_size(payload),
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &metadata,
            })
            .await,
        "object should be inserted",
    );
    ok(
        repository
            .upsert_blob_part(crate::repository::UpsertFileBlobPart {
                storage_backend,
                object_key,
                part_index: 0,
                offset_bytes: 0,
                size_bytes: payload_size(payload),
                checksum_sha256: &part_checksum_sha256,
                compression: FileBlobCompression::None,
                data: payload.to_vec(),
            })
            .await,
        "blob should be inserted",
    );
}

fn object_url_parts(upload_url: &str) -> (String, String) {
    let parsed = ok(
        url::Url::parse(&format!("http://localhost{upload_url}")),
        "upload url should parse",
    );
    let encoded_object_key = some(
        parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(str::to_string),
        "encoded object key should exist",
    );
    let read_token = some(
        parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.into_owned())),
        "read token should exist",
    );
    (encoded_object_key, read_token)
}

fn database_object_read_url_parts(
    storage: &DatabaseFileStorageService,
    storage_backend: &str,
    object_key: &str,
) -> (String, String) {
    let object_url = ok(
        storage.object_url(storage_backend, object_key, "/api/test/objects"),
        "database object url should build",
    );
    object_url_parts(some(
        object_url.as_deref(),
        "database object url should exist",
    ))
}

fn backend_object_read_url_parts(
    storage_backend: &str,
    object_key: &str,
    secret: &str,
) -> (String, String) {
    let object_url = ok(
        database_file_object_url("/api/test/objects", storage_backend, object_key, secret),
        "backend object url should build",
    );
    object_url_parts(&object_url)
}

#[test]
fn versioned_hmac_token_split_rejects_malformed_tokens() {
    assert_eq!(
        ok(
            split_versioned_hmac_token("v1.payload.signature", "v1", "invalid"),
            "versioned HMAC token should split",
        ),
        ("v1", "payload", "signature")
    );

    for token in [
        "",
        "v1",
        "v1.payload",
        "v1..signature",
        "v1.payload.",
        ".payload.signature",
        "v1.payload.signature.extra",
        "v2.payload.signature",
    ] {
        assert!(
            matches!(
                split_versioned_hmac_token(token, "v1", "invalid"),
                Err(Error::InvalidInput(message)) if message == "invalid"
            ),
            "token should be rejected: {token:?}"
        );
    }
}

#[test]
fn presigned_upload_headers_normalizes_and_rejects_invalid_values() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "Content-Type",
        ok("image/png".parse(), "content type header should parse"),
    );
    headers.insert(
        "host",
        ok("storage.example.com".parse(), "host header should parse"),
    );

    let upload_headers = ok(
        presigned_upload_headers(&headers),
        "presigned upload headers should normalize",
    );
    assert_eq!(
        upload_headers.get("content-type").map(String::as_str),
        Some("image/png")
    );
    assert!(!upload_headers.contains_key("host"));

    let mut invalid_headers = http::HeaderMap::new();
    invalid_headers.insert(
        "x-amz-meta-name",
        ok(
            http::HeaderValue::from_bytes(&[0xff]),
            "invalid-byte header fixture should build",
        ),
    );

    assert!(matches!(
        presigned_upload_headers(&invalid_headers),
        Err(Error::Internal(message)) if message.contains("x-amz-meta-name")
    ));
}

#[test]
fn upload_media_type_extracts_base_type_and_rejects_empty_values() {
    assert_eq!(
        ok(upload_media_type("image/png"), "media type should parse"),
        "image/png"
    );
    assert_eq!(
        ok(
            upload_media_type(" image/png ; charset=utf-8"),
            "parameterized media type should parse"
        ),
        "image/png"
    );

    for value in ["", "   ", "; charset=utf-8"] {
        assert!(
            matches!(
                upload_media_type(value),
                Err(Error::InvalidInput(message)) if message.contains("media type is empty")
            ),
            "content-type should be rejected: {value:?}"
        );
    }
}

fn valid_new_stored_file() -> NewStoredFile {
    NewStoredFile {
        filename: None,
        id: "file-1".to_string(),
        storage_backend: "database".to_string(),
        object_key: "objects/file-1".to_string(),
        url: None,
        mime_type: Some("image/png".to_string()),
        size_bytes: Some(7),
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
    }
}

#[test]
fn validate_stored_files_requires_mime_type_and_size() {
    let valid = valid_new_stored_file();
    ok(
        validate_stored_files(std::slice::from_ref(&valid)),
        "valid file should pass",
    );

    let mut missing_mime = valid.clone();
    missing_mime.mime_type = None;
    assert!(matches!(
        validate_stored_files(&[missing_mime]),
        Err(Error::InvalidInput(message)) if message.contains("mime_type is required")
    ));

    let mut empty_mime = valid.clone();
    empty_mime.mime_type = Some("   ".to_string());
    assert!(matches!(
        validate_stored_files(&[empty_mime]),
        Err(Error::InvalidInput(message)) if message.contains("valid media type")
    ));

    let mut bad_mime = valid.clone();
    bad_mime.mime_type = Some("image".to_string());
    assert!(matches!(
        validate_stored_files(&[bad_mime]),
        Err(Error::InvalidInput(message)) if message.contains("valid media type")
    ));

    let mut missing_size = valid;
    missing_size.size_bytes = None;
    assert!(matches!(
        validate_stored_files(&[missing_size]),
        Err(Error::InvalidInput(message)) if message.contains("size_bytes is required")
    ));
}

#[tokio::test]
async fn routed_storage_accepts_empty_submitted_file_references_without_repository() {
    let mut backends: HashMap<String, Arc<dyn FileStorageService>> = HashMap::new();
    backends.insert("disabled".to_string(), Arc::new(DisabledFileStorageService));
    let routed = ok(
        FileStorageBackendRegistry::new(backends).routed("disabled"),
        "disabled backend should route",
    );

    let prepared = ok(
        routed
            .prepare_submitted_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "rooms/1/chat/attachments",
                    database_object_route_prefix: "/api/test/objects",
                    client_request_id: Some("empty-attachments"),
                },
                Vec::new(),
            )
            .await,
        "empty submitted file references should prepare",
    );

    assert!(prepared.is_empty());
}

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
    backends.insert("database".to_string(), database.clone());
    backends.insert("disabled".to_string(), Arc::new(DisabledFileStorageService));
    let routed = ok(
        FileStorageBackendRegistry::new(backends).routed("database"),
        "database backend should route",
    );

    let payload = b"avatar";
    let policy = user_avatar_upload_policy();
    let (parts, content_manifest_sha256) =
        single_part_manifest_for_payload(payload, policy.max_size_bytes);
    let session = ok(
        routed
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let object_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    assert!(object_url.starts_with("/api/user/avatar-objects/"));

    let (encoded_upload_session_key, _) = object_url_parts(object_url);
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );

    ok(
        routed
            .store_upload_object(
                &encoded_upload_session_key,
                upload_token,
                Some("image/png"),
                payload.to_vec(),
            )
            .await,
        "object should store",
    );

    let (encoded_object_key, read_token) =
        database_object_read_url_parts(database.as_ref(), "database", &session.file.object_key);
    let loaded = ok(
        routed
            .get_object(GetFileObject {
                encoded_object_key,
                read_token,
                range: None,
            })
            .await,
        "routed storage should read by token backend",
    );
    assert_eq!(loaded.storage_backend, "database");
    assert_eq!(loaded.mime_type, "image/png");
    assert_eq!(loaded.content_manifest_sha256, content_manifest_sha256);
    assert_eq!(loaded.data, payload);
}

#[tokio::test]
async fn database_storage_default_zstd_compresses_blob_and_returns_original_payload() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool.clone()));
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = vec![b'a'; 4096];
    let policy = user_avatar_upload_policy();
    let (parts, content_manifest_sha256) =
        single_part_manifest_for_payload(&payload, policy.max_size_bytes);
    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(&payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let session = expect_upload_session(session, "upload session should be created");
    let upload_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    let (encoded_object_key, _) = object_url_parts(upload_url);
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );

    let stored = ok(
        storage
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.clone(),
            )
            .await,
        "object should store",
    );
    assert_eq!(stored.compression, FileBlobCompression::None);
    assert_eq!(stored.size_bytes, payload_size(&payload));
    assert_eq!(stored.content_manifest_sha256, content_manifest_sha256);
    assert_eq!(stored.data, payload);

    let row = ok(
        sqlx::query!(
            r#"
            SELECT compression, octet_length(data)::BIGINT AS stored_size_bytes
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            "#,
            "database",
            &stored.object_key,
        )
        .fetch_one(&pool)
        .await,
        "stored blob row should load",
    );
    let compression = row.compression;
    let stored_size_bytes = row.stored_size_bytes;
    assert_eq!(compression, i16::from(FileBlobCompression::Zstd));
    assert!(
        some(stored_size_bytes, "stored size should exist") < payload_size(&payload),
        "compressed payload should be smaller than original payload"
    );

    let (read_encoded_object_key, read_token) =
        database_object_read_url_parts(&storage, "database", &stored.object_key);
    let loaded = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: read_encoded_object_key,
                read_token,
                range: None,
            })
            .await,
        "object should read",
    );
    assert_eq!(loaded.compression, FileBlobCompression::None);
    assert_eq!(loaded.data, payload);
}

#[tokio::test]
async fn database_storage_rejects_parts_above_database_part_cap() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage =
        DatabaseFileStorageService::new("database", repository, "test-file-storage-secret");
    let payload = vec![b'x'; MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES + 1];
    let policy = oversized_test_upload_policy();
    let (parts, _) = manifest_for_payload(&payload, policy.max_size_bytes);

    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "rooms/1/chat/attachments".to_string(),
                client_file_id: Some("chat-attachment-1".to_string()),
                filename: Some("large.bin".to_string()),
                mime_type: "application/pdf".to_string(),
                size_bytes: payload_size(&payload),
                width: None,
                height: None,
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "database storage should create large multipart session",
    );
    let upload_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    let (encoded_object_key, _) = object_url_parts(upload_url);
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );

    let error = err(
        storage
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("application/pdf"),
                payload,
            )
            .await,
        "database storage should reject oversized single-part upload",
    );

    assert!(matches!(
        error,
        Error::InvalidInput(message) if message.contains(&MAX_DATABASE_FILE_UPLOAD_PART_SIZE_BYTES.to_string())
    ));
}

#[tokio::test]
async fn database_storage_lz4_compresses_blob_and_returns_original_payload() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool.clone()));
    let storage = DatabaseFileStorageService::new_with_compression(
        "database",
        repository.clone(),
        "test-file-storage-secret",
        FileBlobCompression::Lz4,
    );
    let payload = vec![b'b'; 4096];
    let policy = user_avatar_upload_policy();
    let (parts, content_manifest_sha256) =
        single_part_manifest_for_payload(&payload, policy.max_size_bytes);
    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(&payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let upload_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    let (encoded_object_key, _) = object_url_parts(upload_url);
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );

    let stored = ok(
        storage
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.clone(),
            )
            .await,
        "object should store",
    );
    assert_eq!(stored.compression, FileBlobCompression::None);
    assert_eq!(stored.content_manifest_sha256, content_manifest_sha256);
    assert_eq!(stored.data, payload);

    let row = ok(
        sqlx::query!(
            r#"
            SELECT compression, octet_length(data)::BIGINT AS stored_size_bytes
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            "#,
            "database",
            &stored.object_key,
        )
        .fetch_one(&pool)
        .await,
        "stored blob row should load",
    );
    let compression = row.compression;
    let stored_size_bytes = row.stored_size_bytes;
    assert_eq!(compression, i16::from(FileBlobCompression::Lz4));
    assert!(
        some(stored_size_bytes, "stored size should exist") < payload_size(&payload),
        "compressed payload should be smaller than original payload"
    );

    let (read_encoded_object_key, read_token) =
        database_object_read_url_parts(&storage, "database", &stored.object_key);
    let loaded = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: read_encoded_object_key,
                read_token,
                range: None,
            })
            .await,
        "object should read",
    );
    assert_eq!(loaded.compression, FileBlobCompression::None);
    assert_eq!(loaded.data, payload);
}

#[tokio::test]
async fn database_storage_rejects_checksum_reuse_when_existing_mime_violates_policy() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"animated-gif";
    upsert_uncompressed_blob(
        repository.as_ref(),
        "database",
        "database/chat/attachments/animated.gif",
        "image/gif",
        payload,
    )
    .await;

    let (request, _) = simple_upload_request(
        UserId::expect_positive(1),
        "users/1/avatars",
        "avatar-1",
        "image/png",
        payload,
        user_avatar_upload_policy(),
    );
    let err = err(
        storage.create_upload_session(request).await,
        "avatar policy should reject existing GIF reuse",
    );

    assert!(matches!(
        err,
        Error::InvalidInput(message) if message == "user_avatar mime_type is not allowed"
    ));
}

#[tokio::test]
async fn database_storage_allows_checksum_reuse_when_existing_mime_matches_policy() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"animated-gif";
    let (request, _) = simple_upload_request(
        UserId::expect_positive(1),
        "rooms/1/chat/attachments",
        "chat-image-1",
        "image/gif",
        payload,
        chat_attachment_upload_policy(),
    );
    upsert_uncompressed_blob(
        repository.as_ref(),
        "database",
        "database/chat/attachments/animated.gif",
        "image/gif",
        payload,
    )
    .await;

    let session = ok(
        storage.create_upload_session(request).await,
        "chat policy should allow GIF reuse",
    );

    assert!(!session.upload_required);
    assert_eq!(session.file.mime_type.as_deref(), Some("image/gif"));
}

#[tokio::test]
async fn database_storage_strips_upload_token_from_prepared_files() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"avatar";
    let policy = user_avatar_upload_policy();
    let (parts, _) = single_part_manifest_for_payload(payload, policy.max_size_bytes);
    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let session = expect_upload_session(session, "upload session should be created");
    assert!(session.file.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_some());

    let upload_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    let (encoded_object_key, _) = object_url_parts(upload_url);
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    ok(
        storage
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.to_vec(),
            )
            .await,
        "object should store",
    );

    let prepared = ok(
        storage
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    database_object_route_prefix: "/api/test/objects",
                    client_request_id: None,
                },
                vec![session.file],
            )
            .await,
        "file should prepare",
    );
    let metadata = &prepared[0].metadata;
    assert!(prepared[0].url.is_some());
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
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"avatar";
    let policy = user_avatar_upload_policy();
    let (parts, content_manifest_sha256) =
        single_part_manifest_for_payload(payload, policy.max_size_bytes);
    upsert_uncompressed_blob(
        repository.as_ref(),
        "database",
        "database/users/avatars/avatar.png",
        "image/png",
        payload,
    )
    .await;

    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let mut session = expect_upload_session(session, "upload session should be created");
    assert!(!session.upload_required);
    assert!(session.file.url.is_none());
    assert!(session.file.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_some());
    let nonce = some(
        session.ownership_proof_nonce.as_deref(),
        "ownership proof nonce should exist",
    );
    let chunks = ok(
        ownership_proof_chunks_from_bytes(payload, &session.ownership_proof_ranges),
        "proof chunks should build",
    );
    let proof = file_ownership_proof_digest(
        nonce,
        &session.ownership_proof_ranges,
        &content_manifest_sha256,
        payload_size(payload),
        chunks.iter().map(Vec::as_slice),
    );
    some(
        session.file.metadata.as_object_mut(),
        "metadata should be object",
    )
    .insert(
        FILE_OWNERSHIP_PROOF_KEY.to_string(),
        serde_json::Value::String(proof),
    );

    let prepared = ok(
        storage
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    database_object_route_prefix: "/api/test/objects",
                    client_request_id: None,
                },
                vec![session.file],
            )
            .await,
        "file should prepare",
    );
    let metadata = &prepared[0].metadata;
    assert!(prepared[0].url.is_some());
    assert!(metadata.get(FILE_UPLOAD_TOKEN_KEY).is_none());
    assert!(metadata.get(FILE_OWNERSHIP_PROOF_KEY).is_none());
    assert_eq!(
        metadata.get("blurhash").and_then(serde_json::Value::as_str),
        Some("abc")
    );
}

#[tokio::test]
async fn database_instant_upload_proof_state_is_scoped_to_reference() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"avatar";
    let policy = user_avatar_upload_policy();
    let (parts, content_manifest_sha256) =
        single_part_manifest_for_payload(payload, policy.max_size_bytes);
    upsert_uncompressed_blob(
        repository.as_ref(),
        "database",
        "database/users/avatars/shared.png",
        "image/png",
        payload,
    )
    .await;

    let first = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-proof-1".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                parts: parts.clone(),
                metadata: serde_json::Value::Object(Default::default()),
                policy: policy.clone(),
            })
            .await,
        "first instant upload session should be created",
    );
    let second = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-proof-2".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "second instant upload session should be created",
    );
    assert_eq!(first.file.object_key, second.file.object_key);
    assert_ne!(first.file.id, second.file.id);

    let first_chunks = ok(
        ownership_proof_chunks_from_bytes(payload, &first.ownership_proof_ranges),
        "first proof chunks should build",
    );
    let first_proof = file_ownership_proof_digest(
        some(
            first.ownership_proof_nonce.as_deref(),
            "first nonce should exist",
        ),
        &first.ownership_proof_ranges,
        &content_manifest_sha256,
        payload_size(payload),
        first_chunks.iter().map(Vec::as_slice),
    );
    let first_upload_token = some(
        first
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "first upload token should exist",
    );
    ok(
        storage
            .complete_upload_session(CompleteFileUploadSession {
                file_id: Some(first.file.id.clone()),
                encoded_object_key: encode_database_file_object_key(&first.file.object_key),
                upload_token: first_upload_token,
                upload_id: None,
                ownership_proof: Some(first_proof),
                parts: Vec::new(),
            })
            .await,
        "first proof should complete",
    );

    let first_prepared = ok(
        storage
            .prepare_submitted_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    database_object_route_prefix: "/api/test/objects",
                    client_request_id: None,
                },
                vec![submitted_file_reference_from_session_file(&first.file)
                    .expect("first reference should build")],
            )
            .await,
        "first verified reference should prepare",
    );
    assert_eq!(first_prepared[0].id, first.file.id);
    let second_prepare_error = err(
        storage
            .prepare_submitted_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    database_object_route_prefix: "/api/test/objects",
                    client_request_id: None,
                },
                vec![submitted_file_reference_from_session_file(&second.file)
                    .expect("second reference should build")],
            )
            .await,
        "second unverified reference should fail",
    );
    assert!(matches!(
        second_prepare_error,
        Error::InvalidInput(message) if message.contains("ownership proof")
    ));

    let second_chunks = ok(
        ownership_proof_chunks_from_bytes(payload, &second.ownership_proof_ranges),
        "second proof chunks should build",
    );
    let second_proof = file_ownership_proof_digest(
        some(
            second.ownership_proof_nonce.as_deref(),
            "second nonce should exist",
        ),
        &second.ownership_proof_ranges,
        &content_manifest_sha256,
        payload_size(payload),
        second_chunks.iter().map(Vec::as_slice),
    );
    let second_upload_token = some(
        second
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "second upload token should exist",
    );
    ok(
        storage
            .complete_upload_session(CompleteFileUploadSession {
                file_id: Some(second.file.id.clone()),
                encoded_object_key: encode_database_file_object_key(&second.file.object_key),
                upload_token: second_upload_token,
                upload_id: None,
                ownership_proof: Some(second_proof),
                parts: Vec::new(),
            })
            .await,
        "second proof should complete independently",
    );
    let second_prepared = ok(
        storage
            .prepare_submitted_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(1),
                    storage_scope: "users/1/avatars",
                    database_object_route_prefix: "/api/test/objects",
                    client_request_id: None,
                },
                vec![submitted_file_reference_from_session_file(&second.file)
                    .expect("second reference should build")],
            )
            .await,
        "second verified reference should prepare",
    );
    assert_eq!(second_prepared[0].id, second.file.id);
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
    upsert_uncompressed_blob(
        repository.as_ref(),
        "primary_db",
        "database/users/avatars/file.webp",
        "image/webp",
        b"avatar",
    )
    .await;
    let mut tx = ok(pool.begin().await, "transaction should begin");
    ok(
        FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            "primary_db",
            "database/users/avatars/file.webp",
            "user_avatar",
            "user:1",
            None,
            &serde_json::Value::Object(Default::default()),
        )
        .await,
        "reference should insert",
    );
    ok(tx.commit().await, "transaction should commit");

    ok(
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
            .await,
        "delete should use configured database backend name",
    );

    assert!(!ok(
        repository
            .blob_exists("primary_db", "database/users/avatars/file.webp")
            .await,
        "blob lookup should succeed"
    ));
    assert!(!ok(
        repository
            .object_exists("primary_db", "database/users/avatars/file.webp")
            .await,
        "object lookup should succeed"
    ));
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
    upsert_uncompressed_blob(
        repository.as_ref(),
        "primary_db",
        "database/chat/attachments/orphan.webp",
        "image/webp",
        b"orphan",
    )
    .await;

    ok(
        storage
            .delete_files(
                FileStorageCleanupOrigin::UnreferencedObject,
                &[FileReferenceTarget {
                    storage_backend: "primary_db".to_string(),
                    object_key: "database/chat/attachments/orphan.webp".to_string(),
                    reference_kind: "unreferenced_file".to_string(),
                    reference_id: "database/chat/attachments/orphan.webp".to_string(),
                }],
            )
            .await,
        "unreferenced object delete should not require a reference row",
    );

    assert!(!ok(
        repository
            .object_exists("primary_db", "database/chat/attachments/orphan.webp")
            .await,
        "object lookup should succeed"
    ));
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
        ok(
            optional_file_storage_public_url(&config, "covers/one.png"),
            "optional public URL should evaluate"
        ),
        None
    );
    assert!(matches!(
        file_storage_public_url(&config, "covers/one.png"),
        Err(Error::InvalidInput(_))
    ));

    config.public_base_url = Some("https://cdn.example.com/assets".to_string());
    let url = some(
        ok(
            optional_file_storage_public_url(&config, "covers/one.png"),
            "public URL should build",
        ),
        "configured public URL should be returned",
    );
    assert_eq!(
        url,
        "https://cdn.example.com/assets/synctv-files/covers/one.png"
    );
}

#[tokio::test]
async fn pending_file_object_is_not_reused_but_can_be_cleaned_up() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let content_manifest_sha256 = hex::encode(Sha256::digest(b"pending-manifest"));
    ok(
        repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: "s3_public",
                object_key: "files/sha256/pending.webp",
                mime_type: "image/webp",
                size_bytes: 7,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &serde_json::Value::Object(Default::default()),
            })
            .await,
        "pending object should be inserted",
    );

    let reusable = ok(
        repository
            .get_object_by_manifest("s3_public", &content_manifest_sha256, 7)
            .await,
        "manifest lookup should succeed",
    );
    assert!(reusable.is_none());

    let storage = DatabaseFileStorageService::new(
        "s3_public",
        repository.clone(),
        "test-file-storage-secret",
    );
    ok(
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
            .await,
        "pending object should be cleanup-addressable",
    );

    assert!(!ok(
        repository
            .object_exists("s3_public", "files/sha256/pending.webp")
            .await,
        "object lookup should succeed"
    ));
}

#[tokio::test]
async fn pending_file_object_becomes_reusable_after_validation() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let content_manifest_sha256 = hex::encode(Sha256::digest(b"validated-manifest"));
    ok(
        repository
            .upsert_pending_object(UpsertFileObject {
                storage_backend: "s3_public",
                object_key: "files/sha256/validated.webp",
                mime_type: "image/webp",
                size_bytes: 9,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &serde_json::Value::Object(Default::default()),
            })
            .await,
        "pending object should be inserted",
    );
    assert!(!ok(
        repository
            .object_validated("s3_public", "files/sha256/validated.webp")
            .await,
        "validated lookup should succeed"
    ));

    ok(
        repository
            .upsert_object(UpsertFileObject {
                storage_backend: "s3_public",
                object_key: "files/sha256/validated.webp",
                mime_type: "image/webp",
                size_bytes: 9,
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &serde_json::Value::Object(Default::default()),
            })
            .await,
        "object should validate",
    );

    let reusable = some(
        ok(
            repository
                .get_object_by_manifest("s3_public", &content_manifest_sha256, 9)
                .await,
            "manifest lookup should succeed",
        ),
        "validated object should be reusable",
    );
    assert_eq!(reusable.object_key, "files/sha256/validated.webp");
    assert!(reusable.validated_at.is_some());
}

#[tokio::test]
async fn database_storage_skips_compression_below_threshold() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool.clone()));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository,
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::Zstd,
            min_size_bytes: 8192,
            min_savings_percent: 10,
        },
    );
    let payload = vec![b'a'; 4096];
    let policy = user_avatar_upload_policy();
    let (parts, _) = single_part_manifest_for_payload(&payload, policy.max_size_bytes);
    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-small".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(&payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let (encoded_object_key, _) = object_url_parts(some(
        session.upload_url.as_deref(),
        "upload url should be returned",
    ));
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    let stored = ok(
        storage
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.clone(),
            )
            .await,
        "object should store",
    );

    let row = ok(
        sqlx::query!(
            r#"
            SELECT compression, octet_length(data)::BIGINT AS stored_size_bytes
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            "#,
            "database",
            &stored.object_key,
        )
        .fetch_one(&pool)
        .await,
        "stored blob should load",
    );
    let compression = row.compression;
    let stored_size_bytes = row.stored_size_bytes;
    assert_eq!(compression, i16::from(FileBlobCompression::None));
    assert_eq!(
        some(stored_size_bytes, "stored size should exist"),
        payload_size(&payload)
    );
}

#[tokio::test]
async fn database_storage_skips_low_savings_compression() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool.clone()));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository,
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::Zstd,
            min_size_bytes: 0,
            min_savings_percent: 100,
        },
    );
    let payload = vec![b'a'; 4096];
    let policy = user_avatar_upload_policy();
    let (parts, _) = single_part_manifest_for_payload(&payload, policy.max_size_bytes);
    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-low-savings".to_string()),
                filename: None,
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(&payload),
                width: Some(16),
                height: Some(16),
                parts,
                metadata: serde_json::Value::Object(Default::default()),
                policy,
            })
            .await,
        "upload session should be created",
    );
    let (encoded_object_key, _) = object_url_parts(some(
        session.upload_url.as_deref(),
        "upload url should be returned",
    ));
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    let stored = ok(
        storage
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.clone(),
            )
            .await,
        "object should store",
    );

    let row = ok(
        sqlx::query!(
            r#"
            SELECT compression, octet_length(data)::BIGINT AS stored_size_bytes
            FROM file_blob_parts
            WHERE storage_backend = $1 AND object_key = $2
            "#,
            "database",
            &stored.object_key,
        )
        .fetch_one(&pool)
        .await,
        "stored blob should load",
    );
    let compression = row.compression;
    let stored_size_bytes = row.stored_size_bytes;
    assert_eq!(compression, i16::from(FileBlobCompression::None));
    assert_eq!(
        some(stored_size_bytes, "stored size should exist"),
        payload_size(&payload)
    );
}

#[tokio::test]
async fn database_storage_resumable_upload_completes_after_all_parts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository.clone(),
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::None,
            ..Default::default()
        },
    );
    let payload = vec![b'x'; 12 * 1024 * 1024];
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (request, content_manifest_sha256) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-resumable",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy: policy.clone(),
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    assert!(session.resumable);
    let (encoded_object_key, _) = object_url_parts(some(
        session.upload_url.as_deref(),
        "upload url should be returned",
    ));
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    let part_size = usize::try_from(session.part_size_bytes).expect("part size should fit");
    let first = payload[..part_size].to_vec();
    let second = payload[part_size..].to_vec();
    let first_result = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: encoded_object_key.clone(),
                upload_token: upload_token.to_string(),
                content_type: Some("image/png".to_string()),
                range: Some(FileUploadRange {
                    start: 0,
                    end_inclusive: session.part_size_bytes - 1,
                    total_size: payload_size(&payload),
                }),
                data: first,
            })
            .await,
        "first upload part should store",
    );
    let StoreFileUploadResult::PartAccepted {
        uploaded_size_bytes,
        uploaded_parts,
    } = first_result
    else {
        panic!("first upload part should be accepted");
    };
    assert_eq!(uploaded_size_bytes, session.part_size_bytes);
    assert_eq!(uploaded_parts, vec![1]);

    let (resumed_request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-resumable-retry",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let resumed = ok(
        storage.create_upload_session(resumed_request).await,
        "resumed upload session should be created",
    );
    assert_eq!(resumed.file.object_key, session.file.object_key);
    assert_eq!(resumed.uploaded_size_bytes, session.part_size_bytes);
    assert_eq!(resumed.uploaded_parts, vec![1]);

    let completed = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: encoded_object_key.clone(),
                upload_token: upload_token.to_string(),
                content_type: Some("image/png".to_string()),
                range: Some(FileUploadRange {
                    start: session.part_size_bytes,
                    end_inclusive: payload_size(&payload) - 1,
                    total_size: payload_size(&payload),
                }),
                data: second,
            })
            .await,
        "second upload part should complete",
    );
    let StoreFileUploadResult::Complete(blob) = completed else {
        panic!("second upload part should complete object");
    };
    assert_eq!(blob.content_manifest_sha256, content_manifest_sha256);
    assert!(blob.data.is_empty());
    let upload_session_key =
        decode_database_file_object_key(&encoded_object_key).expect("session key should decode");
    let completed_parts = ok(
        repository
            .list_upload_session_parts("database", &upload_session_key)
            .await,
        "upload parts should load",
    );
    assert_eq!(
        completed_parts
            .iter()
            .map(|part| part.part_number)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let temporary_blob_parts = ok(
        repository
            .list_blob_parts("database", &upload_session_key)
            .await,
        "temporary blob parts should load",
    );
    assert!(temporary_blob_parts.is_empty());
    let temporary_object_exists = ok(
        repository
            .object_exists("database", &upload_session_key)
            .await,
        "temporary object should load",
    );
    assert!(!temporary_object_exists);
    let permanent_blob_parts = ok(
        repository
            .list_blob_parts("database", &blob.object_key)
            .await,
        "permanent blob parts should load",
    );
    assert_eq!(permanent_blob_parts.len(), 2);
    assert_eq!(
        permanent_blob_parts
            .iter()
            .map(|part| part.part_index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let (read_encoded_object_key, read_token) =
        database_object_read_url_parts(&storage, "database", &blob.object_key);
    let loaded = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: read_encoded_object_key.clone(),
                read_token: read_token.clone(),
                range: None,
            })
            .await,
        "completed object should read",
    );
    assert_eq!(loaded.data, payload);
    let requested = FileByteRange {
        start: 3,
        end_inclusive: 8,
    };
    let ranged = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: read_encoded_object_key.clone(),
                read_token: read_token.clone(),
                range: Some(FileRangeRequest::Exact(requested)),
            })
            .await,
        "database completed object range should read",
    );
    assert_eq!(ranged.range, Some(requested));
    assert_eq!(ranged.total_size_bytes, payload_size(&payload));
    assert_eq!(ranged.data, payload[3..9]);
}

#[tokio::test]
async fn database_storage_streams_completed_blob_parts_without_single_buffer() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository,
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::None,
            min_size_bytes: 1,
            min_savings_percent: 0,
        },
    );
    let mut payload = Vec::with_capacity(12 * 1024 * 1024);
    payload.extend(vec![b'a'; 8 * 1024 * 1024]);
    payload.extend(vec![b'b'; 4 * 1024 * 1024]);
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-stream",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    let (encoded_object_key, _) = object_url_parts(some(
        session.upload_url.as_deref(),
        "upload url should be returned",
    ));
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    let split = usize::try_from(session.part_size_bytes).expect("part size should fit");
    let mut completed = None;
    for (index, chunk) in payload.chunks(split).enumerate() {
        let start = i64::try_from(index * split).expect("start should fit");
        let result = ok(
            storage
                .store_upload(StoreFileUpload {
                    encoded_object_key: encoded_object_key.clone(),
                    upload_token: upload_token.to_string(),
                    content_type: Some("image/png".to_string()),
                    range: Some(FileUploadRange {
                        start,
                        end_inclusive: start + payload_size(chunk) - 1,
                        total_size: payload_size(&payload),
                    }),
                    data: chunk.to_vec(),
                })
                .await,
            "upload part should store",
        );
        if let StoreFileUploadResult::Complete(blob) = result {
            completed = Some(blob);
        }
    }
    let blob = some(completed, "multipart upload should complete");
    let (read_encoded_object_key, read_token) =
        database_object_read_url_parts(&storage, "database", &blob.object_key);
    let download = ok(
        storage
            .get_object_stream(GetFileObject {
                encoded_object_key: read_encoded_object_key,
                read_token,
                range: None,
            })
            .await,
        "completed object should stream",
    );
    assert_eq!(download.metadata.size_bytes, payload_size(&payload));
    let chunks = ok(
        download.stream.try_collect::<Vec<_>>().await,
        "stream chunks should read",
    );
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 8 * 1024 * 1024);
    assert_eq!(chunks[1].len(), 4 * 1024 * 1024);
    let collected = chunks
        .into_iter()
        .flat_map(std::iter::IntoIterator::into_iter)
        .collect::<Vec<_>>();
    assert_eq!(collected, payload);
}

#[tokio::test]
async fn database_storage_resumable_fingerprint_is_scoped_to_uploader_and_scope() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository,
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::None,
            ..Default::default()
        },
    );
    let payload = vec![b'f'; 12 * 1024 * 1024];
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let make_request = |user_id, storage_scope, client_file_id| {
        upload_request(UploadRequestInput {
            user_id,
            storage_scope,
            client_file_id,
            filename: None,
            mime_type: "image/png",
            payload: &payload,
            width: Some(16),
            height: Some(16),
            metadata: serde_json::Value::Object(Default::default()),
            policy: policy.clone(),
        })
        .0
    };
    let first = ok(
        storage
            .create_upload_session(make_request(
                UserId::expect_positive(1),
                "users/1/avatars",
                "avatar-resumable-1",
            ))
            .await,
        "first upload session should be created",
    );
    let second_user = ok(
        storage
            .create_upload_session(make_request(
                UserId::expect_positive(2),
                "users/2/avatars",
                "avatar-resumable-2",
            ))
            .await,
        "second user upload session should be created",
    );
    let second_scope = ok(
        storage
            .create_upload_session(make_request(
                UserId::expect_positive(1),
                "users/1/banner",
                "banner-resumable-1",
            ))
            .await,
        "second scope upload session should be created",
    );
    let resumed = ok(
        storage
            .create_upload_session(make_request(
                UserId::expect_positive(1),
                "users/1/avatars",
                "avatar-resumable-retry",
            ))
            .await,
        "same uploader and scope should resume",
    );

    assert_eq!(resumed.file.object_key, first.file.object_key);
    assert_eq!(second_user.file.object_key, first.file.object_key);
    assert_eq!(second_scope.file.object_key, first.file.object_key);
    assert_eq!(resumed.upload_url, first.upload_url);
    assert_ne!(second_user.upload_url, first.upload_url);
    assert_ne!(second_scope.upload_url, first.upload_url);
}

#[tokio::test]
async fn database_storage_multipart_stores_manifest_identity() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool.clone()));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository,
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::None,
            ..Default::default()
        },
    );
    let mut payload = Vec::with_capacity(12 * 1024 * 1024);
    payload.extend(vec![b'a'; 8 * 1024 * 1024]);
    payload.extend(vec![b'b'; 4 * 1024 * 1024]);
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (request, content_manifest_sha256) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-multipart-checksum",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    let (encoded_object_key, _) = object_url_parts(some(
        session.upload_url.as_deref(),
        "upload url should be returned",
    ));
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    let split = usize::try_from(session.part_size_bytes).expect("part size should fit");
    let mut completed = None;
    for (index, chunk) in payload.chunks(split).enumerate() {
        let start = i64::try_from(index * split).expect("start should fit");
        let end_inclusive = start + payload_size(chunk) - 1;
        let result = ok(
            storage
                .store_upload(StoreFileUpload {
                    encoded_object_key: encoded_object_key.clone(),
                    upload_token: upload_token.to_string(),
                    content_type: Some("image/png".to_string()),
                    range: Some(FileUploadRange {
                        start,
                        end_inclusive,
                        total_size: payload_size(&payload),
                    }),
                    data: chunk.to_vec(),
                })
                .await,
            "upload part should store",
        );
        if let StoreFileUploadResult::Complete(blob) = result {
            completed = Some(blob);
        }
    }

    let blob = some(completed, "multipart upload should complete");
    assert_eq!(blob.content_manifest_sha256, content_manifest_sha256);
    assert!(blob.data.is_empty());
    let (read_encoded_object_key, read_token) =
        database_object_read_url_parts(&storage, "database", &blob.object_key);
    let loaded = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: read_encoded_object_key,
                read_token,
                range: None,
            })
            .await,
        "completed object should read",
    );
    assert_eq!(loaded.data, payload);
    let object_content_manifest_sha256 = ok(
        sqlx::query_scalar!(
            r#"
            SELECT content_manifest_sha256
            FROM file_objects
            WHERE storage_backend = $1 AND object_key = $2
            "#,
            "database",
            &blob.object_key,
        )
        .fetch_one(&pool)
        .await,
        "stored file object should load",
    );
    assert_eq!(object_content_manifest_sha256, content_manifest_sha256);
}

#[tokio::test]
async fn database_storage_range_reads_from_permanent_blob_parts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage = DatabaseFileStorageService::new_with_compression_config(
        "database",
        repository,
        "test-file-storage-secret",
        DatabaseFileStorageCompressionConfig {
            algorithm: FileBlobCompression::Zstd,
            min_size_bytes: 1,
            min_savings_percent: 0,
        },
    );
    let payload = vec![b'r'; 12 * 1024 * 1024];
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-range",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    let (encoded_object_key, _) = object_url_parts(some(
        session.upload_url.as_deref(),
        "upload url should be returned",
    ));
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "upload token should exist",
    );
    let split = usize::try_from(session.part_size_bytes).expect("part size should fit");
    let mut completed = None;
    for (index, chunk) in payload.chunks(split).enumerate() {
        let start = i64::try_from(index * split).expect("start should fit");
        let end_inclusive = start + payload_size(chunk) - 1;
        let result = ok(
            storage
                .store_upload(StoreFileUpload {
                    encoded_object_key: encoded_object_key.clone(),
                    upload_token: upload_token.to_string(),
                    content_type: Some("image/png".to_string()),
                    range: Some(FileUploadRange {
                        start,
                        end_inclusive,
                        total_size: payload_size(&payload),
                    }),
                    data: chunk.to_vec(),
                })
                .await,
            "upload part should store",
        );
        if let StoreFileUploadResult::Complete(blob) = result {
            completed = Some(blob);
        }
    }
    let blob = some(completed, "multipart upload should complete");
    let (read_encoded_object_key, read_token) =
        database_object_read_url_parts(&storage, "database", &blob.object_key);

    let requested = FileByteRange {
        start: 1024,
        end_inclusive: 4095,
    };
    let loaded = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: read_encoded_object_key,
                read_token,
                range: Some(FileRangeRequest::Exact(requested)),
            })
            .await,
        "range read should load",
    );
    assert_eq!(loaded.range, Some(requested));
    assert_eq!(loaded.total_size_bytes, payload_size(&payload));
    assert_eq!(loaded.size_bytes, requested.size_bytes());
    assert_eq!(loaded.data, payload[1024..4096]);
}

#[tokio::test]
async fn s3_storage_multipart_session_returns_native_part_urls() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator.clone())
    .with_test_multipart_upload_id("test-upload-id");
    let payload = vec![b's'; 5 * 1024 * 1024];
    let policy = user_avatar_upload_policy();
    let (request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-resumable",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    assert!(session.upload_url.as_deref().is_some_and(|url| {
        url.starts_with("/api/user/avatar-objects/") && url.contains("?token=")
    }));
    assert_eq!(session.upload_method.as_deref(), Some("PUT"));
    assert_eq!(
        session
            .upload_headers
            .get("content-type")
            .map(String::as_str),
        Some("image/png")
    );
    assert!(session
        .upload_headers
        .contains_key(FILE_UPLOAD_TOKEN_HEADER));
    let upload_id = some(session.upload_id.clone(), "S3 upload_id should be returned");
    assert_eq!(session.part_urls.len(), 1);
    assert_eq!(session.part_urls[0].part_number, 1);
    assert_eq!(session.part_urls[0].offset_bytes, 0);
    assert_eq!(session.part_urls[0].size_bytes, payload_size(&payload));
    assert_eq!(session.part_urls[0].upload_method, "PUT");
    assert!(session.part_urls[0].upload_url.contains("X-Amz-Signature="));
    assert_eq!(upload_id, "test-upload-id");
}

#[tokio::test]
async fn s3_storage_streams_range_from_backend_proxy_path() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator.clone())
    .with_test_force_stat_error();
    let object_key = "files/users/1/avatars/avatar-s3-stream.png";
    let payload = b"0123456789abcdef".to_vec();
    ok(
        operator
            .write_with(object_key, payload.clone())
            .content_type("image/png")
            .await,
        "S3 object should be written",
    );
    let content_manifest_sha256 = hex::encode(Sha256::digest(&payload));
    ok(
        repository
            .upsert_object(UpsertFileObject {
                storage_backend: "s3_public",
                object_key,
                mime_type: "image/png",
                size_bytes: payload_size(&payload),
                content_manifest_sha256: &content_manifest_sha256,
                metadata: &serde_json::Value::Object(Default::default()),
            })
            .await,
        "S3 object metadata should be inserted",
    );
    let (encoded_object_key, read_token) =
        backend_object_read_url_parts("s3_public", object_key, "test-file-storage-secret");
    let requested = FileByteRange {
        start: 4,
        end_inclusive: 11,
    };
    let download = ok(
        storage
            .get_object_stream(GetFileObject {
                encoded_object_key,
                read_token,
                range: Some(FileRangeRequest::Exact(requested)),
            })
            .await,
        "S3 object range should stream",
    );
    assert_eq!(download.metadata.range, Some(requested));
    assert_eq!(download.metadata.size_bytes, requested.size_bytes());
    assert_eq!(download.metadata.total_size_bytes, payload_size(&payload));
    assert_eq!(download.metadata.mime_type, "image/png");
    let chunks = ok(
        download.stream.try_collect::<Vec<_>>().await,
        "S3 stream should read",
    );
    let collected = chunks
        .into_iter()
        .flat_map(std::iter::IntoIterator::into_iter)
        .collect::<Vec<_>>();
    assert_eq!(collected, payload[4..12]);
}

#[tokio::test]
async fn s3_storage_multipart_completion_uses_part_manifest_digest() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator.clone())
    .with_test_multipart_upload_id("test-upload-id");
    let payload = b"s3-completed-object".to_vec();
    let policy = user_avatar_upload_policy();
    let (request, content_manifest_sha256) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-complete",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let part_checksum_sha256 = request.parts[0].checksum_sha256.clone();
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    ok(
        operator
            .write_with(&session.file.object_key, payload.clone())
            .content_type("image/png")
            .await,
        "completed S3 object should be written",
    );
    let upload_token = some(
        session
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "upload token should exist",
    );
    let complete = ok(
        storage
            .complete_upload_session(CompleteFileUploadSession {
                file_id: Some(session.file.id.clone()),
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token,
                upload_id: session.upload_id.clone(),
                ownership_proof: None,
                parts: vec![CompleteFileUploadPart {
                    part_number: 1,
                    etag: "\"etag-1\"".to_string(),
                    size_bytes: payload_size(&payload),
                    checksum_sha256: Some(part_checksum_sha256),
                }],
            })
            .await,
        "S3 multipart upload should complete",
    );
    let blob = some(complete.object, "completed object should be returned");
    assert_eq!(blob.content_manifest_sha256, content_manifest_sha256);
    assert_eq!(complete.uploaded_size_bytes, payload_size(&payload));
    assert_eq!(complete.uploaded_parts, vec![1]);
    assert!(ok(
        repository
            .object_validated("s3_public", &session.file.object_key)
            .await,
        "object validation should load"
    ));
    let read_url = ok(
        database_file_object_url(
            "/api/user/avatar-objects",
            "s3_public",
            &session.file.object_key,
            "test-file-storage-secret",
        ),
        "read object url should be generated",
    );
    let (encoded_object_key, read_token) = object_url_parts(&read_url);
    let loaded = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key: encoded_object_key.clone(),
                read_token: read_token.clone(),
                range: None,
            })
            .await,
        "completed object should read",
    );
    assert_eq!(loaded.data, payload);
    let requested = FileByteRange {
        start: 3,
        end_inclusive: 8,
    };
    let ranged = ok(
        storage
            .get_object(GetFileObject {
                encoded_object_key,
                read_token,
                range: Some(FileRangeRequest::Exact(requested)),
            })
            .await,
        "S3 completed object range should read",
    );
    assert_eq!(ranged.range, Some(requested));
    assert_eq!(ranged.total_size_bytes, payload_size(&payload));
    assert_eq!(ranged.data, payload[3..9]);
}

#[tokio::test]
async fn s3_storage_store_upload_accepts_server_mediated_parts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator.clone())
    .with_test_multipart_upload_id("test-upload-id");
    let mut payload = Vec::with_capacity(12 * 1024 * 1024);
    payload.extend(vec![b'a'; 8 * 1024 * 1024]);
    payload.extend(vec![b'b'; 4 * 1024 * 1024]);
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (request, content_manifest_sha256) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-server-mediated",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    let upload_token = some(
        session
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "upload token should exist",
    );
    let part_size = usize::try_from(session.part_size_bytes).expect("part size should fit");
    let first_part = payload[..part_size].to_vec();
    let first = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token: upload_token.clone(),
                content_type: Some("image/png".to_string()),
                range: Some(FileUploadRange {
                    start: 0,
                    end_inclusive: session.part_size_bytes - 1,
                    total_size: payload_size(&payload),
                }),
                data: first_part,
            })
            .await,
        "first S3 server-mediated part should upload",
    );
    let StoreFileUploadResult::PartAccepted {
        uploaded_size_bytes,
        uploaded_parts,
    } = first
    else {
        panic!("first part should be accepted without completing");
    };
    assert_eq!(uploaded_size_bytes, session.part_size_bytes);
    assert_eq!(uploaded_parts, vec![1]);

    let second_part = payload[part_size..].to_vec();
    let completed = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token,
                content_type: Some("image/png".to_string()),
                range: Some(FileUploadRange {
                    start: session.part_size_bytes,
                    end_inclusive: payload_size(&payload) - 1,
                    total_size: payload_size(&payload),
                }),
                data: second_part,
            })
            .await,
        "second S3 server-mediated part should complete",
    );
    let StoreFileUploadResult::Complete(blob) = completed else {
        panic!("all S3 server-mediated parts should complete object");
    };
    assert_eq!(blob.content_manifest_sha256, content_manifest_sha256);
    assert_eq!(blob.size_bytes, payload_size(&payload));
    assert!(ok(
        repository
            .object_validated("s3_public", &session.file.object_key)
            .await,
        "object validation should load"
    ));
    let loaded = ok(
        operator.read(&session.file.object_key).await,
        "completed S3 object should be stored",
    );
    assert_eq!(loaded.to_vec(), payload);
}

#[tokio::test]
async fn s3_storage_server_mediated_upload_is_bound_to_session_key() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator)
    .with_test_multipart_upload_id("shared-test-upload-id");
    let mut payload = Vec::with_capacity(12 * 1024 * 1024);
    payload.extend(vec![b'a'; 8 * 1024 * 1024]);
    payload.extend(vec![b'b'; 4 * 1024 * 1024]);
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (first_request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-session-a",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy: policy.clone(),
    });
    let first_session = ok(
        storage.create_upload_session(first_request).await,
        "first upload session should be created",
    );
    let (second_request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(2),
        storage_scope: "users/2/avatars",
        client_file_id: "avatar-s3-session-b",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let second_session = ok(
        storage.create_upload_session(second_request).await,
        "second upload session should be created",
    );
    assert_eq!(
        first_session.file.object_key,
        second_session.file.object_key
    );
    assert_ne!(
        first_session.encoded_object_key,
        second_session.encoded_object_key
    );
    let (url_encoded_session_key, _) = object_url_parts(some(
        first_session.upload_url.as_deref(),
        "first upload url should exist",
    ));
    assert_eq!(url_encoded_session_key, first_session.encoded_object_key);
    assert_ne!(url_encoded_session_key, second_session.encoded_object_key);

    let upload_token = some(
        first_session
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "first upload token should exist",
    );
    let part_size = usize::try_from(first_session.part_size_bytes).expect("part size should fit");
    let first_part = payload[..part_size].to_vec();
    let result = ok(
        storage
            .store_upload(StoreFileUpload {
                encoded_object_key: first_session.encoded_object_key.clone(),
                upload_token,
                content_type: Some("image/png".to_string()),
                range: Some(FileUploadRange {
                    start: 0,
                    end_inclusive: first_session.part_size_bytes - 1,
                    total_size: payload_size(&payload),
                }),
                data: first_part,
            })
            .await,
        "first session server-mediated part should upload",
    );
    let StoreFileUploadResult::PartAccepted { uploaded_parts, .. } = result else {
        panic!("first part should be accepted without completing");
    };
    assert_eq!(uploaded_parts, vec![1]);
    let first_session_key = decode_database_file_object_key(&first_session.encoded_object_key)
        .expect("first session key should decode");
    let second_session_key = decode_database_file_object_key(&second_session.encoded_object_key)
        .expect("second session key should decode");
    assert_eq!(
        ok(
            repository
                .list_upload_session_parts("s3_public", &first_session_key)
                .await,
            "first session parts should load",
        )
        .len(),
        1
    );
    assert!(ok(
        repository
            .list_upload_session_parts("s3_public", &second_session_key)
            .await,
        "second session parts should load",
    )
    .is_empty());
}

#[tokio::test]
async fn s3_storage_multipart_completion_rejects_manifest_mismatch() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator.clone())
    .with_test_multipart_upload_id("test-upload-id");
    let expected_payload = b"declared-s3-object".to_vec();
    let actual_payload = b"tampered-s3-object".to_vec();
    assert_eq!(expected_payload.len(), actual_payload.len());
    let policy = user_avatar_upload_policy();
    let (request, expected_content_manifest_sha256) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-checksum-mismatch",
        filename: None,
        mime_type: "image/png",
        payload: &expected_payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let actual_part_checksum = hex::encode(Sha256::digest(&actual_payload));
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    ok(
        operator
            .write_with(&session.file.object_key, actual_payload)
            .content_type("image/png")
            .await,
        "tampered S3 object should be written",
    );
    let upload_token = some(
        session
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "upload token should exist",
    );

    let error = err(
        storage
            .complete_upload_session(CompleteFileUploadSession {
                file_id: Some(session.file.id.clone()),
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token,
                upload_id: session.upload_id.clone(),
                ownership_proof: None,
                parts: vec![CompleteFileUploadPart {
                    part_number: 1,
                    etag: "\"etag-1\"".to_string(),
                    size_bytes: payload_size(&expected_payload),
                    checksum_sha256: Some(actual_part_checksum),
                }],
            })
            .await,
        "S3 multipart checksum mismatch should fail",
    );

    assert!(matches!(error, Error::InvalidInput(message) if message.contains("manifest")));
    assert!(ok(
        repository
            .get_object_by_manifest(
                "s3_public",
                &expected_content_manifest_sha256,
                payload_size(&expected_payload),
            )
            .await,
        "object by manifest should load"
    )
    .is_none());
    assert!(!ok(
        repository
            .object_validated("s3_public", &session.file.object_key)
            .await,
        "object validation should load"
    ));
}

#[tokio::test]
async fn s3_public_constructor_requires_repository_for_upload_session() {
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new(S3FileStorageConfig {
            endpoint: "http://s3.invalid".to_string(),
            access_key_id: "test-access-key".to_string(),
            secret_access_key: "test-secret-key".to_string(),
            bucket: "synctv-test".to_string(),
            region: "us-east-1".to_string(),
            base_path: "files".to_string(),
            public_base_url: Some("https://cdn.example.test".to_string()),
            upload_expires_seconds: 900,
            storage_backend: "s3_public".to_string(),
            upload_token_secret: "test-file-storage-secret".to_string(),
        }),
        "s3 storage should build",
    )
    .with_operator(operator.clone());
    let payload = b"direct-s3-upload".to_vec();
    let policy = user_avatar_upload_policy();
    let (request, _) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-direct",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let error = err(
        storage.create_upload_session(request).await,
        "repository-backed S3 upload session should be required",
    );
    assert!(matches!(
        error,
        Error::Internal(message) if message.contains("file storage repository")
    ));
}

#[tokio::test]
async fn s3_multipart_completion_uses_all_recorded_parts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool.clone()));
    let operator = ok(
        opendal::Operator::new(opendal::services::Memory::default())
            .map(opendal::OperatorBuilder::finish),
        "memory operator should build",
    );
    let storage = ok(
        S3CompatibleFileStorageService::new_with_repository(
            S3FileStorageConfig {
                endpoint: "http://s3.invalid".to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret-key".to_string(),
                bucket: "synctv-test".to_string(),
                region: "us-east-1".to_string(),
                base_path: "files".to_string(),
                public_base_url: Some("https://cdn.example.test".to_string()),
                upload_expires_seconds: 900,
                storage_backend: "s3_public".to_string(),
                upload_token_secret: "test-file-storage-secret".to_string(),
            },
            Some(repository.clone()),
        ),
        "s3 storage should build",
    )
    .with_operator(operator.clone())
    .with_test_multipart_upload_id("test-upload-id");
    let mut payload = Vec::with_capacity(12 * 1024 * 1024);
    payload.extend(vec![b'a'; 8 * 1024 * 1024]);
    payload.extend(vec![b'b'; 4 * 1024 * 1024]);
    let policy = large_test_upload_policy(payload_size(&payload), "image/png");
    let (request, content_manifest_sha256) = upload_request(UploadRequestInput {
        user_id: UserId::expect_positive(1),
        storage_scope: "users/1/avatars",
        client_file_id: "avatar-s3-staged-complete",
        filename: None,
        mime_type: "image/png",
        payload: &payload,
        width: Some(16),
        height: Some(16),
        metadata: serde_json::Value::Object(Default::default()),
        policy,
    });
    let session = ok(
        storage.create_upload_session(request).await,
        "upload session should be created",
    );
    ok(
        operator
            .write_with(&session.file.object_key, payload.clone())
            .content_type("image/png")
            .await,
        "completed S3 object should be written",
    );
    let upload_token = some(
        session
            .file
            .metadata
            .get(FILE_UPLOAD_TOKEN_KEY)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        "upload token should exist",
    );
    let part_size = session.part_size_bytes;
    let first_part_checksum = hex::encode(Sha256::digest(
        &payload[..usize::try_from(part_size).expect("part size should fit")],
    ));
    let first = ok(
        storage
            .complete_upload_session(CompleteFileUploadSession {
                file_id: Some(session.file.id.clone()),
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token: upload_token.clone(),
                upload_id: session.upload_id.clone(),
                ownership_proof: None,
                parts: vec![CompleteFileUploadPart {
                    part_number: 1,
                    etag: "\"etag-1\"".to_string(),
                    size_bytes: part_size,
                    checksum_sha256: Some(first_part_checksum),
                }],
            })
            .await,
        "first S3 completion call should record progress",
    );
    assert!(first.object.is_none());
    assert_eq!(first.uploaded_size_bytes, part_size);
    assert_eq!(first.uploaded_parts, vec![1]);

    let second_part = &payload[usize::try_from(part_size).expect("part size should fit")..];
    let second = ok(
        storage
            .complete_upload_session(CompleteFileUploadSession {
                file_id: Some(session.file.id.clone()),
                encoded_object_key: session.encoded_object_key.clone(),
                upload_token,
                upload_id: session.upload_id.clone(),
                ownership_proof: None,
                parts: vec![CompleteFileUploadPart {
                    part_number: 2,
                    etag: "\"etag-2\"".to_string(),
                    size_bytes: payload_size(second_part),
                    checksum_sha256: Some(hex::encode(Sha256::digest(second_part))),
                }],
            })
            .await,
        "second S3 completion call should complete object",
    );
    let blob = some(second.object, "completed object should be returned");
    assert_eq!(blob.content_manifest_sha256, content_manifest_sha256);
    assert_eq!(second.uploaded_size_bytes, payload_size(&payload));
    assert_eq!(second.uploaded_parts, vec![1, 2]);
    let object_content_manifest_sha256 = ok(
        sqlx::query_scalar!(
            r#"
            SELECT content_manifest_sha256
            FROM file_objects
            WHERE storage_backend = $1 AND object_key = $2
            "#,
            "s3_public",
            &blob.object_key,
        )
        .fetch_one(&pool)
        .await,
        "stored S3 file object should load",
    );
    assert_eq!(object_content_manifest_sha256, content_manifest_sha256);
}
