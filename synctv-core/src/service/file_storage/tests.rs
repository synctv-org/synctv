use std::{collections::HashMap, sync::Arc};

use sha2::{Digest, Sha256};

use super::*;
use crate::{
    repository::FileStorageRepository,
    service::file_upload_policies::{chat_image_upload_policy, user_avatar_upload_policy},
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
    let routed = ok(
        FileStorageBackendRegistry::new(backends).routed("database"),
        "database backend should route",
    );

    let payload = b"avatar";
    let checksum = hex::encode(Sha256::digest(payload));
    let session = ok(
        routed
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum.clone()),
                metadata: serde_json::Value::Object(Default::default()),
                policy: user_avatar_upload_policy(),
            })
            .await,
        "upload session should be created",
    );
    let object_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    assert!(object_url.starts_with("/api/user/avatar-objects/"));

    let (encoded_object_key, read_token) = object_url_parts(object_url);
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
                &encoded_object_key,
                upload_token,
                Some("image/png"),
                payload.to_vec(),
            )
            .await,
        "object should store",
    );

    let loaded = ok(
        routed.get_object(&encoded_object_key, &read_token).await,
        "routed storage should read by token backend",
    );
    assert_eq!(loaded.storage_backend, "database");
    assert_eq!(loaded.mime_type, "image/png");
    assert_eq!(loaded.checksum_sha256, checksum);
    assert_eq!(loaded.data, payload);
}

#[tokio::test]
async fn database_storage_rejects_checksum_reuse_when_existing_mime_violates_policy() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"animated-gif";
    let checksum = hex::encode(Sha256::digest(payload));
    ok(
        repository
            .upsert_blob(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                payload.to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "blob should be inserted",
    );
    ok(
        repository
            .upsert_object(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                payload_size(payload),
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "chat image object should be inserted",
    );

    let err = err(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: user_avatar_upload_policy(),
            })
            .await,
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
    let checksum = hex::encode(Sha256::digest(payload));
    ok(
        repository
            .upsert_blob(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                payload.to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "blob should be inserted",
    );
    ok(
        repository
            .upsert_object(
                "database",
                "database/chat/images/animated.gif",
                "image/gif",
                payload_size(payload),
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "chat image object should be inserted",
    );

    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "rooms/1/chat/images".to_string(),
                client_file_id: Some("chat-image-1".to_string()),
                mime_type: "image/gif".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
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
    let checksum = hex::encode(Sha256::digest(payload));
    let session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: user_avatar_upload_policy(),
            })
            .await,
        "upload session should be created",
    );
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
                    client_request_id: None,
                },
                vec![session.file],
            )
            .await,
        "file should prepare",
    );
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
    let storage =
        DatabaseFileStorageService::new("database", repository.clone(), "test-file-storage-secret");
    let payload = b"avatar";
    let checksum = hex::encode(Sha256::digest(payload));
    ok(
        repository
            .upsert_blob(
                "database",
                "database/users/avatars/avatar.png",
                "image/png",
                payload.to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "blob should be inserted",
    );
    ok(
        repository
            .upsert_object(
                "database",
                "database/users/avatars/avatar.png",
                "image/png",
                payload_size(payload),
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "object should be inserted",
    );

    let mut session = ok(
        storage
            .create_upload_session(CreateFileUploadSession {
                user_id: UserId::expect_positive(1),
                storage_scope: "users/1/avatars".to_string(),
                client_file_id: Some("avatar-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: payload_size(payload),
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: user_avatar_upload_policy(),
            })
            .await,
        "upload session should be created",
    );
    assert!(!session.upload_required);
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
                    client_request_id: None,
                },
                vec![session.file],
            )
            .await,
        "file should prepare",
    );
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
    ok(
        repository
            .upsert_blob(
                "primary_db",
                "database/users/avatars/file.webp",
                "image/webp",
                b"avatar".to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "blob should be inserted",
    );
    ok(
        repository
            .upsert_object(
                "primary_db",
                "database/users/avatars/file.webp",
                "image/webp",
                6,
                &hex::encode(Sha256::digest(b"avatar")),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "object should be inserted",
    );
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
    ok(
        repository
            .upsert_blob(
                "primary_db",
                "database/chat/images/orphan.webp",
                "image/webp",
                b"orphan".to_vec(),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "blob should be inserted",
    );
    ok(
        repository
            .upsert_object(
                "primary_db",
                "database/chat/images/orphan.webp",
                "image/webp",
                6,
                &hex::encode(Sha256::digest(b"orphan")),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "object should be inserted",
    );

    ok(
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
            .await,
        "unreferenced object delete should not require a reference row",
    );

    assert!(!ok(
        repository
            .object_exists("primary_db", "database/chat/images/orphan.webp")
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
    let checksum = hex::encode(Sha256::digest(b"pending"));
    ok(
        repository
            .upsert_pending_object(
                "s3_public",
                "files/sha256/pending.webp",
                "image/webp",
                7,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "pending object should be inserted",
    );

    let reusable = ok(
        repository
            .get_object_by_checksum("s3_public", &checksum, 7)
            .await,
        "checksum lookup should succeed",
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
    let checksum = hex::encode(Sha256::digest(b"validated"));
    ok(
        repository
            .upsert_pending_object(
                "s3_public",
                "files/sha256/validated.webp",
                "image/webp",
                9,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
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
            .upsert_object(
                "s3_public",
                "files/sha256/validated.webp",
                "image/webp",
                9,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "object should validate",
    );

    let reusable = some(
        ok(
            repository
                .get_object_by_checksum("s3_public", &checksum, 9)
                .await,
            "checksum lookup should succeed",
        ),
        "validated object should be reusable",
    );
    assert_eq!(reusable.object_key, "files/sha256/validated.webp");
    assert!(reusable.validated_at.is_some());
}
