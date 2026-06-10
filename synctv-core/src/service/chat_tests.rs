use std::sync::{Arc, Mutex};

use super::*;
use crate::service::file_storage::{
    file_content_object_key, file_ownership_proof_digest, file_storage_object_base_path,
    file_storage_public_url, ownership_proof_chunks_from_bytes,
    validate_create_file_upload_session, DatabaseFileStorageService,
    S3CompatibleFileStorageService, S3FileStorageConfig, FILE_OWNERSHIP_PROOF_KEY,
    FILE_UPLOAD_TOKEN_HEADER, FILE_UPLOAD_TOKEN_KEY,
};
use crate::{
    cache::{KeyBuilder, UsernameCache},
    models::{ChatReadState, CreateFileUploadSession, NewStoredFile, SignupMethod, User},
    repository::{
        FileStorageRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository,
        UserRepository,
    },
    service::{
        auth::JwtService, chat_image_upload_policy, BruteForceProtection,
        DisabledFileStorageService, InMemoryTokenBlacklistStore, RateLimiter, RoomService,
    },
};
use opendal::Operator;
use sha2::{Digest, Sha256};
use tokio::sync::Barrier;

const TEST_FILE_STORAGE_SCOPE: &str = "rooms/1/users/1";

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

fn joined<T>(result: std::result::Result<T, tokio::task::JoinError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn object_url_parts(upload_url: &str) -> (String, String) {
    let parsed = ok(
        url::Url::parse(&format!("http://localhost{upload_url}")),
        "relative database object URL should parse with base",
    );
    let encoded_object_key = some(
        parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(str::to_string),
        "encoded object key path segment should exist",
    );
    let read_token = some(
        parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.into_owned())),
        "read token should be present",
    );

    (encoded_object_key, read_token)
}

#[derive(Debug)]
struct PrefixingFileStorageService;

#[async_trait::async_trait]
impl FileStorageService for PrefixingFileStorageService {
    fn backend_name(&self) -> &'static str {
        "test-storage"
    }

    async fn create_upload_session(
        &self,
        mut request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        validate_create_file_upload_session(&request)?;
        let id = request
            .client_file_id
            .take()
            .unwrap_or_else(|| "custom-image".to_string());
        Ok(FileUploadSession {
            file: NewStoredFile {
                id: id.clone(),
                storage_backend: "test-storage".to_string(),
                object_key: format!("normalized/uploads/{id}"),
                url: Some(format!("https://cdn.invalid/uploads/{id}")),
                mime_type: Some(request.mime_type),
                size_bytes: Some(request.size_bytes),
                width: request.width,
                height: request.height,
                metadata: request.metadata,
            },
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            ownership_proof_metadata_key: None,
            upload_url: Some(format!("https://upload.invalid/{id}")),
            upload_method: Some("PUT".to_string()),
            upload_headers: Default::default(),
            expires_at: Some(Utc::now()),
            max_size_bytes: MAX_CHAT_IMAGE_SIZE_BYTES,
        })
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        images: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        assert!(context.user_id.as_i64() > 0);
        assert!(!context.storage_scope.is_empty());
        validate_chat_images(&images)?;
        Ok(images
            .into_iter()
            .map(|mut image| {
                image.storage_backend = "test-storage".to_string();
                image.object_key = format!("normalized/{}", image.object_key);
                image.url = Some(format!("https://cdn.invalid/{}", image.id));
                image
            })
            .collect())
    }
}

#[derive(Debug, Default)]
struct RecordingFileStorageService {
    deleted_object_keys: Mutex<Vec<String>>,
    deleted_origins: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl FileStorageService for RecordingFileStorageService {
    fn backend_name(&self) -> &'static str {
        "test-storage"
    }

    async fn create_upload_session(
        &self,
        mut request: CreateFileUploadSession,
    ) -> Result<FileUploadSession> {
        validate_create_file_upload_session(&request)?;
        let id = request
            .client_file_id
            .take()
            .unwrap_or_else(|| "custom-image".to_string());
        Ok(FileUploadSession {
            file: NewStoredFile {
                id: id.clone(),
                storage_backend: "test-storage".to_string(),
                object_key: format!("normalized/uploads/{id}"),
                url: Some(format!("https://cdn.invalid/uploads/{id}")),
                mime_type: Some(request.mime_type),
                size_bytes: Some(request.size_bytes),
                width: request.width,
                height: request.height,
                metadata: request.metadata,
            },
            upload_required: true,
            ownership_proof_required: false,
            ownership_proof_nonce: None,
            ownership_proof_ranges: Vec::new(),
            ownership_proof_metadata_key: None,
            upload_url: Some(format!("https://upload.invalid/{id}")),
            upload_method: Some("PUT".to_string()),
            upload_headers: Default::default(),
            expires_at: Some(Utc::now()),
            max_size_bytes: MAX_CHAT_IMAGE_SIZE_BYTES,
        })
    }

    async fn prepare_files(
        &self,
        context: FileStorageContext<'_>,
        images: Vec<NewStoredFile>,
    ) -> Result<Vec<NewStoredFile>> {
        assert!(context.user_id.as_i64() > 0);
        assert!(!context.storage_scope.is_empty());
        validate_chat_images(&images)?;
        Ok(images
            .into_iter()
            .map(|mut image| {
                image.storage_backend = "test-storage".to_string();
                image.object_key = format!("normalized/{}", image.object_key);
                image.url = Some(format!("https://cdn.invalid/{}", image.id));
                image
            })
            .collect())
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[crate::models::FileReferenceTarget],
    ) -> Result<()> {
        let mut deleted = ok(self.deleted_object_keys.lock(), "deleted object keys lock");
        deleted.extend(files.iter().map(|file| file.object_key.clone()));
        let mut origins = ok(self.deleted_origins.lock(), "deleted origins lock");
        origins.extend(files.iter().map(|_| origin.as_str().to_string()));
        Ok(())
    }
}

fn test_user_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> Arc<UserService> {
    Arc::new(UserService::new_for_tests(
        pool,
        ok(
            JwtService::new("test-secret-key-for-chat-service-tests-32-chars"),
            "jwt",
        ),
        username_cache,
        Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test".to_string()),
    ))
}

#[test]
fn validate_chat_metadata_rejects_non_object_values() {
    let error = err(
        validate_chat_metadata(&serde_json::json!(["tag"])),
        "chat metadata should be an object",
    );

    assert!(
        matches!(error, Error::InvalidInput(message) if message == "chat metadata must be a JSON object")
    );
}

#[test]
fn validate_chat_images_rejects_non_object_metadata() {
    let image = NewStoredFile {
        id: "img-test".to_string(),
        storage_backend: "database".to_string(),
        object_key: "rooms/1/chat/img-test".to_string(),
        url: None,
        mime_type: Some("image/png".to_string()),
        size_bytes: Some(1024),
        width: Some(32),
        height: Some(32),
        metadata: serde_json::json!(["tag"]),
    };

    let error = err(
        validate_chat_images(&[image]),
        "image metadata should be object",
    );
    assert!(
        matches!(error, Error::InvalidInput(message) if message == "chat metadata must be a JSON object")
    );
}

fn playback_query_with_context() -> ChatPlaybackMessagesQuery {
    ChatPlaybackMessagesQuery {
        room_id: RoomId::expect_positive(1),
        media_id: Some(crate::models::MediaId::expect_positive(2)),
        playlist_id: None,
        target: None,
        position_seconds: 10.0,
        before_seconds: 1.0,
        after_seconds: 2.0,
        limit: 100,
        include_deleted: false,
    }
}

#[test]
fn chat_playback_query_normalize_treats_empty_target_as_absent() {
    let query = ChatPlaybackMessagesQuery {
        media_id: None,
        target: Some(Vec::new()),
        ..playback_query_with_context()
    }
    .normalize();

    assert!(query.target.is_none());
}

#[test]
fn validate_chat_playback_query_rejects_empty_context_after_normalize() {
    let query = ChatPlaybackMessagesQuery {
        media_id: None,
        target: Some(Vec::new()),
        ..playback_query_with_context()
    };

    assert!(matches!(
        validate_chat_playback_query(query),
        Err(Error::InvalidInput(message)) if message.contains("requires")
    ));
}

#[test]
fn validate_chat_playback_query_rejects_invalid_limit() {
    let zero = ChatPlaybackMessagesQuery {
        limit: 0,
        ..playback_query_with_context()
    };
    let too_large = ChatPlaybackMessagesQuery {
        limit: 501,
        ..playback_query_with_context()
    };

    assert!(matches!(
        validate_chat_playback_query(zero),
        Err(Error::InvalidInput(message)) if message.contains("limit")
    ));
    assert!(matches!(
        validate_chat_playback_query(too_large),
        Err(Error::InvalidInput(message)) if message.contains("limit")
    ));
}

fn test_chat_service(pool: &sqlx::PgPool, username_cache: UsernameCache) -> ChatService {
    test_chat_service_with_file_storage(pool, username_cache, Arc::new(DisabledFileStorageService))
}

fn test_chat_service_with_file_storage(
    pool: &sqlx::PgPool,
    username_cache: UsernameCache,
    file_storage_service: Arc<dyn FileStorageService>,
) -> ChatService {
    let permission_service = ok(
        PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            crate::service::permission::PermissionServiceRuntime {
                room_settings_repo: Some(RoomSettingsRepository::new(pool.clone())),
                ..crate::service::permission::PermissionServiceRuntime::local_only()
            },
        ),
        "permission service should build",
    );
    let room_settings_service = RoomSettingsService::new(
        RoomSettingsRepository::new(pool.clone()),
        None,
        Arc::new(NotificationService::default()),
        None,
        None,
    );

    ChatService::new(
        Arc::new(ChatRepository::new(pool.clone())),
        ChatRuntime {
            rate_limiter: Arc::new(RateLimiter::local_only("test:chat:".to_string())),
            rate_limit_config: RateLimitConfig::default(),
            content_filter: ContentFilter::new(),
        },
        ChatDependencies {
            permission_service,
            room_settings_service,
            user_service: test_user_service(pool, username_cache),
            file_storage_service,
            audit_service: None,
            notification_service: NotificationService::default(),
        },
    )
}

fn test_chat_message(id: i64, created_at: chrono::DateTime<Utc>) -> ChatMessage {
    ChatMessage {
        id,
        room_id: RoomId::expect_positive(1),
        user_id: Some(UserId::expect_positive(2)),
        client_message_id: None,
        content: "hello".to_string(),
        message_type: ChatMessageType::Text,
        status: ChatMessageStatus::Active,
        version: 1,
        reply_to_message_id: None,
        reply_to_message_created_at: None,
        metadata: serde_json::Value::Object(Default::default()),
        edited_at: None,
        deleted_at: None,
        deleted_by: None,
        delete_reason: None,
        created_at,
    }
}

#[test]
fn read_state_covers_newer_message_cursor() {
    let created_at = Utc::now();
    let message = test_chat_message(10, created_at);
    let state = ChatReadState {
        room_id: message.room_id,
        user_id: UserId::expect_positive(2),
        last_read_message_id: Some(11),
        last_read_message_created_at: Some(created_at),
        last_read_event_id: None,
        last_read_event_sequence: None,
        updated_at: Utc::now(),
    };

    assert!(read_state_covers_message(Some(&state), &message, None));
}

#[test]
fn read_state_allows_forward_message_cursor() {
    let created_at = Utc::now();
    let message = test_chat_message(10, created_at);
    let state = ChatReadState {
        room_id: message.room_id,
        user_id: UserId::expect_positive(2),
        last_read_message_id: Some(9),
        last_read_message_created_at: Some(created_at),
        last_read_event_id: None,
        last_read_event_sequence: None,
        updated_at: Utc::now(),
    };

    assert!(!read_state_covers_message(Some(&state), &message, None));
}

#[test]
fn read_state_allows_forward_event_on_same_message() {
    let created_at = Utc::now();
    let message = test_chat_message(10, created_at);
    let event = ChatMessageEventLog {
        sequence: 12,
        event: ChatMessageEvent {
            event_id: "event-12".to_string(),
            sequence: 12,
            room_id: message.room_id,
            actor_user_id: UserId::expect_positive(2),
            kind: crate::models::ChatEventKind::Edited,
            message: ChatMessageWithImages {
                message: message.clone(),
                images: Vec::new(),
                reactions: Vec::new(),
            },
            occurred_at: Utc::now(),
        },
    };
    let state = ChatReadState {
        room_id: message.room_id,
        user_id: UserId::expect_positive(2),
        last_read_message_id: Some(message.id),
        last_read_message_created_at: Some(message.created_at),
        last_read_event_id: Some("event-11".to_string()),
        last_read_event_sequence: Some(11),
        updated_at: Utc::now(),
    };

    assert!(!read_state_covers_message(
        Some(&state),
        &message,
        Some(&event)
    ));
}

#[tokio::test]
async fn username_lookup_falls_back_to_database_and_populates_cache() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_cache_miss_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    let username_cache = UsernameCache::local_only("test:chat:username:".to_string(), 100, 60);
    let service = test_chat_service_with_file_storage(
        &pool,
        username_cache.clone(),
        Arc::new(ok(
            S3CompatibleFileStorageService::new(test_s3_file_storage_config()),
            "S3 file storage config should be valid",
        )),
    );

    assert_eq!(
        ok(
            username_cache.get(&user.id).await,
            "cache read should succeed"
        ),
        None
    );

    let username = ok(
        service.username_for_user(&user.id).await,
        "database fallback should resolve username",
    );

    assert_eq!(username, user.username);
    assert_eq!(
        ok(
            username_cache.get(&user.id).await,
            "cache read should succeed"
        ),
        Some(user.username)
    );
}

#[tokio::test]
async fn disabled_file_storage_rejects_images() {
    let service = DisabledFileStorageService;
    let result = service
        .prepare_files(
            FileStorageContext {
                user_id: UserId::expect_positive(1),
                storage_scope: TEST_FILE_STORAGE_SCOPE,
                client_request_id: Some("client-1"),
            },
            vec![NewStoredFile {
                id: "image-1".to_string(),
                storage_backend: "database".to_string(),
                object_key: "image.webp".to_string(),
                url: None,
                mime_type: Some("image/webp".to_string()),
                size_bytes: Some(-1),
                width: Some(640),
                height: Some(480),
                metadata: serde_json::Value::Object(Default::default()),
            }],
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[test]
fn validate_chat_images_rejects_duplicates_in_one_message() {
    let image = NewStoredFile {
        id: "image-1".to_string(),
        storage_backend: "database".to_string(),
        object_key: "image-1.webp".to_string(),
        url: None,
        mime_type: Some("image/webp".to_string()),
        size_bytes: Some(1024),
        width: Some(640),
        height: Some(480),
        metadata: serde_json::Value::Object(Default::default()),
    };

    let duplicate_id = validate_chat_images(&[
        image.clone(),
        NewStoredFile {
            id: image.id.clone(),
            object_key: "image-2.webp".to_string(),
            ..image.clone()
        },
    ]);
    assert!(matches!(duplicate_id, Err(Error::InvalidInput(_))));

    let duplicate_key = validate_chat_images(&[
        image.clone(),
        NewStoredFile {
            id: "image-2".to_string(),
            object_key: image.object_key.clone(),
            ..image
        },
    ]);
    assert!(matches!(duplicate_key, Err(Error::InvalidInput(_))));
}

#[test]
fn validate_chat_images_rejects_zero_size() {
    let result = validate_chat_images(&[NewStoredFile {
        id: "image-1".to_string(),
        storage_backend: "database".to_string(),
        object_key: "image-1.webp".to_string(),
        url: None,
        mime_type: Some("image/webp".to_string()),
        size_bytes: Some(0),
        width: Some(640),
        height: Some(480),
        metadata: serde_json::Value::Object(Default::default()),
    }]);

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn disabled_file_storage_rejects_upload_session() {
    let service = DisabledFileStorageService;
    let result = service
        .create_upload_session(CreateFileUploadSession {
            storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
            user_id: UserId::expect_positive(2),
            client_file_id: Some("client-image-1".to_string()),
            mime_type: "image/webp".to_string(),
            size_bytes: 1024,
            width: Some(640),
            height: Some(480),
            checksum_sha256: Some("a".repeat(64)),
            metadata: serde_json::json!({"blurhash": "abc"}),
            policy: chat_image_upload_policy(),
        })
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn disabled_file_storage_rejects_prepared_images() {
    let service = DisabledFileStorageService;

    let result = service
        .prepare_files(
            FileStorageContext {
                user_id: UserId::expect_positive(2),
                storage_scope: TEST_FILE_STORAGE_SCOPE,
                client_request_id: Some("client-1"),
            },
            vec![NewStoredFile {
                id: "image-1".to_string(),
                storage_backend: "database".to_string(),
                object_key: "rooms/1/chat/2/image-1".to_string(),
                url: None,
                mime_type: Some("image/webp".to_string()),
                size_bytes: Some(1024),
                width: Some(640),
                height: Some(480),
                metadata: serde_json::Value::Object(Default::default()),
            }],
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn database_file_storage_roundtrips_uploaded_object() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_database_image_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:database-image:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (_room, _) = ok(
        room_service
            .create_room(
                "Database Image Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    let service = DatabaseFileStorageService::new(
        "database",
        Arc::new(FileStorageRepository::new(pool.clone())),
        "database-image-secret",
    );
    let expected_checksum = hex::encode(Sha256::digest(b"data"));
    let session = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: user.id,
                client_file_id: Some("database-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(expected_checksum.clone()),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "database upload session should be created",
    );
    assert_eq!(session.file.storage_backend, "database");
    assert_eq!(session.upload_method.as_deref(), Some("PUT"));
    let upload_url = some(
        session.upload_url.as_deref(),
        "database upload url should be returned",
    );
    let (encoded_object_key, read_token) = object_url_parts(upload_url);
    let upload_token = some(
        session
            .upload_headers
            .get(FILE_UPLOAD_TOKEN_HEADER)
            .map(String::as_str),
        "database upload token header should be returned",
    );
    let stored = ok(
        service
            .store_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/webp"),
                b"data".to_vec(),
            )
            .await,
        "database image object should store",
    );
    assert_eq!(stored.object_key, session.file.object_key);
    assert_eq!(stored.data, b"data");
    assert_eq!(stored.checksum_sha256, expected_checksum);
    let loaded = ok(
        service.get_object(&encoded_object_key, &read_token).await,
        "database image object should load",
    );
    assert_eq!(loaded.data, b"data");
    let prepared = ok(
        service
            .prepare_files(
                FileStorageContext {
                    user_id: user.id,
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("database-image-message"),
                },
                vec![session.file],
            )
            .await,
        "uploaded database image should prepare",
    );
    assert_eq!(prepared.len(), 1);

    let mut reuse_session = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: user.id,
                client_file_id: Some("database-image-2".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(expected_checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "database upload session should reuse existing object",
    );
    assert!(!reuse_session.upload_required);
    assert!(reuse_session.ownership_proof_required);
    assert_eq!(reuse_session.file.object_key, stored.object_key);
    let nonce = some(
        reuse_session.ownership_proof_nonce.as_deref(),
        "reuse session should return proof nonce",
    );
    let chunks = ok(
        ownership_proof_chunks_from_bytes(b"data", &reuse_session.ownership_proof_ranges),
        "proof chunks should be readable",
    );
    let proof = file_ownership_proof_digest(
        nonce,
        &reuse_session.ownership_proof_ranges,
        chunks.iter().map(Vec::as_slice),
    );
    some(
        reuse_session.file.metadata.as_object_mut(),
        "metadata should be object",
    )
    .insert(
        FILE_OWNERSHIP_PROOF_KEY.to_string(),
        serde_json::Value::String(proof),
    );
    ok(
        service
            .prepare_files(
                FileStorageContext {
                    user_id: user.id,
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("database-image-reuse-message"),
                },
                vec![reuse_session.file],
            )
            .await,
        "reused database image should prepare with ownership proof",
    );
}

#[tokio::test]
async fn database_file_storage_rejects_checksum_mismatch() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_database_image_checksum_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only(
                    "test:chat:database-image-checksum:".to_string(),
                    100,
                    60,
                ),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (_room, _) = ok(
        room_service
            .create_room(
                "Database Image Checksum Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    let service = DatabaseFileStorageService::new(
        "database",
        Arc::new(FileStorageRepository::new(pool.clone())),
        "database-image-checksum-secret",
    );
    let session = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: user.id,
                client_file_id: Some("database-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(hex::encode(Sha256::digest(b"data"))),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "database upload session should be created",
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
        "database upload token header should be returned",
    );

    let result = service
        .store_upload_object(
            &encoded_object_key,
            upload_token,
            Some("image/webp"),
            b"fail".to_vec(),
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn image_upload_session_requires_checksum() {
    let service = ok(
        S3CompatibleFileStorageService::new(test_s3_file_storage_config()),
        "S3 file storage config should be valid",
    );

    let result = service
        .create_upload_session(CreateFileUploadSession {
            storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
            user_id: UserId::expect_positive(9),
            client_file_id: Some("client-image-1".to_string()),
            mime_type: "image/png".to_string(),
            size_bytes: 2048,
            width: Some(800),
            height: Some(600),
            checksum_sha256: None,
            metadata: serde_json::Value::Object(Default::default()),
            policy: chat_image_upload_policy(),
        })
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn s3_file_storage_rejects_tampered_upload_session_image() {
    let service = ok(
        S3CompatibleFileStorageService::new(test_s3_file_storage_config()),
        "S3 file storage config should be valid",
    );
    let session = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(2),
                client_file_id: Some("client-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 1024,
                width: Some(640),
                height: Some(480),
                checksum_sha256: Some("b".repeat(64)),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "upload session should be created",
    );
    let mut tampered = session.file;
    tampered.object_key = "files/rooms/1/chat/2/other-image.webp".to_string();

    let result = service
        .prepare_files(
            FileStorageContext {
                user_id: UserId::expect_positive(2),
                storage_scope: TEST_FILE_STORAGE_SCOPE,
                client_request_id: Some("client-1"),
            },
            vec![tampered],
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

fn test_s3_file_storage_config() -> S3FileStorageConfig {
    S3FileStorageConfig {
        endpoint: "https://s3.example.com".to_string(),
        access_key_id: "test-access-key".to_string(),
        secret_access_key: "test-secret-key".to_string(),
        bucket: "synctv-files".to_string(),
        region: "auto".to_string(),
        base_path: "files/".to_string(),
        public_base_url: Some("https://cdn.example.com/files".to_string()),
        upload_expires_seconds: 600,
        storage_backend: "s3".to_string(),
        upload_token_secret: "file-upload-token-secret".to_string(),
    }
}

#[tokio::test]
async fn s3_file_storage_creates_presigned_upload_session() {
    let service = ok(
        S3CompatibleFileStorageService::new(test_s3_file_storage_config()),
        "S3 file storage config should be valid",
    );
    let session = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("client-image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: Some("a".repeat(64)),
                metadata: serde_json::json!({"blurhash": "abc"}),
                policy: chat_image_upload_policy(),
            })
            .await,
        "S3 upload session should be created",
    );

    assert!(session.upload_required);
    assert_eq!(session.upload_method.as_deref(), Some("PUT"));
    assert_eq!(
        session
            .upload_headers
            .get("content-type")
            .map(String::as_str),
        Some("image/png")
    );
    assert_eq!(session.file.id, "client-image-1");
    assert_eq!(session.file.storage_backend, "s3");
    assert!(session
        .file
        .metadata
        .get(FILE_UPLOAD_TOKEN_KEY)
        .and_then(serde_json::Value::as_str)
        .is_some());
    assert!(session
        .file
        .object_key
        .starts_with("files/chat/images/sha256/aa/aa/"));
    let expected_public_url = format!(
        "https://cdn.example.com/files/synctv-files/{}",
        session.file.object_key
    );
    assert_eq!(
        session.file.url.as_deref(),
        Some(expected_public_url.as_str())
    );
    let upload_url = some(session.upload_url, "upload URL should be returned");
    assert!(upload_url.starts_with(&format!(
        "https://s3.example.com/synctv-files/{}?",
        session.file.object_key
    )));
    assert!(upload_url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
    assert!(upload_url.contains("X-Amz-Credential=test-access-key%2F"));
    assert!(upload_url.contains("X-Amz-SignedHeaders="));
    assert!(upload_url.contains("X-Amz-Signature="));
    assert!(session.expires_at.is_some());
    assert_eq!(session.max_size_bytes, MAX_CHAT_IMAGE_SIZE_BYTES);
}

#[tokio::test]
async fn image_upload_sessions_reuse_content_object_key_for_reused_client_ids() {
    let service = ok(
        S3CompatibleFileStorageService::new(test_s3_file_storage_config()),
        "S3 file storage config should be valid",
    );
    let first = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: Some("c".repeat(64)),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "first upload session should be created",
    );
    let second = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("image-1".to_string()),
                mime_type: "image/png".to_string(),
                size_bytes: 2048,
                width: Some(800),
                height: Some(600),
                checksum_sha256: Some("c".repeat(64)),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "second upload session should be created",
    );

    assert_eq!(first.file.id, "image-1");
    assert_eq!(second.file.id, "image-1");
    assert_eq!(first.file.object_key, second.file.object_key);
}

#[tokio::test]
async fn s3_file_storage_reuses_registered_object_with_ownership_proof() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let repository = Arc::new(FileStorageRepository::new(pool));
    let config = test_s3_file_storage_config();
    let policy = chat_image_upload_policy();
    let checksum = hex::encode(Sha256::digest(b"data"));
    let object_key = file_content_object_key(
        &file_storage_object_base_path(&config.base_path, &policy.storage_namespace),
        &checksum,
    );
    let operator = ok(
        Operator::new(opendal::services::Memory::default()),
        "memory operator should build",
    )
    .finish();
    ok(
        operator.write(&object_key, b"data".to_vec()).await,
        "object should be written",
    );
    ok(
        repository
            .upsert_object(
                "s3",
                &object_key,
                "image/webp",
                4,
                &checksum,
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "object registry should be written",
    );
    let service = ok(
        S3CompatibleFileStorageService::new_with_repository(config, Some(repository)),
        "S3 file storage config should be valid",
    )
    .with_operator(operator);

    let mut session = ok(
        service
            .create_upload_session(CreateFileUploadSession {
                storage_scope: TEST_FILE_STORAGE_SCOPE.to_string(),
                user_id: UserId::expect_positive(9),
                client_file_id: Some("image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 4,
                width: Some(16),
                height: Some(16),
                checksum_sha256: Some(checksum),
                metadata: serde_json::Value::Object(Default::default()),
                policy: chat_image_upload_policy(),
            })
            .await,
        "registered S3 object should create reuse session",
    );
    assert!(!session.upload_required);
    assert!(session.ownership_proof_required);
    assert_eq!(session.file.object_key, object_key);
    let nonce = some(
        session.ownership_proof_nonce.as_deref(),
        "reuse session should return proof nonce",
    );
    let chunks = ok(
        ownership_proof_chunks_from_bytes(b"data", &session.ownership_proof_ranges),
        "proof chunks should be readable",
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

    ok(
        service
            .prepare_files(
                FileStorageContext {
                    user_id: UserId::expect_positive(9),
                    storage_scope: TEST_FILE_STORAGE_SCOPE,
                    client_request_id: Some("s3-reuse"),
                },
                vec![session.file],
            )
            .await,
        "S3 reuse should prepare with ownership proof",
    );
}

#[test]
fn s3_public_url_uses_url_path_segment_encoding() {
    let mut config = test_s3_file_storage_config();
    config.bucket = "bucket with spaces".to_string();
    let url = ok(
        file_storage_public_url(&config, "chat images/with # and ?.png"),
        "public URL should be built",
    );

    assert_eq!(
        url,
        "https://cdn.example.com/files/bucket%20with%20spaces/chat%20images/with%20%23%20and%20%3F.png"
    );
}

#[test]
fn s3_file_storage_rejects_invalid_config() {
    let mut config = test_s3_file_storage_config();
    config.endpoint.clear();

    let result = S3CompatibleFileStorageService::new(config);

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn s3_file_storage_rejects_unexpected_backend_on_send() {
    let service = ok(
        S3CompatibleFileStorageService::new(test_s3_file_storage_config()),
        "S3 file storage config should be valid",
    );
    let result = service
        .prepare_files(
            FileStorageContext {
                user_id: UserId::expect_positive(1),
                storage_scope: TEST_FILE_STORAGE_SCOPE,
                client_request_id: Some("client-1"),
            },
            vec![NewStoredFile {
                id: "image-1".to_string(),
                storage_backend: "database".to_string(),
                object_key: "image.webp".to_string(),
                url: Some("https://cdn.example.com/image.webp".to_string()),
                mime_type: Some("image/webp".to_string()),
                size_bytes: Some(1024),
                width: Some(640),
                height: Some(480),
                metadata: serde_json::Value::Object(Default::default()),
            }],
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
async fn metadata_only_image_token_is_stripped_before_persistence() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache =
        UsernameCache::local_only("test:chat:image-token-strip:".to_string(), 100, 60);
    let service = test_chat_service_with_file_storage(
        &pool,
        username_cache.clone(),
        Arc::new(DatabaseFileStorageService::new(
            "database",
            Arc::new(FileStorageRepository::new(pool.clone())),
            "test-file-storage-secret",
        )),
    );
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_image_token_strip_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:image-token-strip:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Image Token Strip Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let payload = vec![b'd'; 1024];
    let session = ok(
        service
            .create_image_upload_session(CreateChatImageUploadSession {
                room_id: room.id,
                user_id: user.id,
                client_image_id: Some("strip-image-1".to_string()),
                mime_type: "image/webp".to_string(),
                size_bytes: 1024,
                width: Some(640),
                height: Some(480),
                checksum_sha256: Some(hex::encode(Sha256::digest(&payload))),
                metadata: serde_json::json!({"blurhash": "abc"}),
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
        "database upload token header should be returned",
    );
    ok(
        service
            .store_image_upload_object(
                &encoded_object_key,
                upload_token,
                Some("image/webp"),
                payload,
            )
            .await,
        "database image object should store",
    );

    let event = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("strip-image-message-1".to_string()),
                content: String::new(),
                message_type: ChatMessageType::Image,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: vec![session.file],
            })
            .await,
        "image message should be stored",
    );

    let image = some(event.message.images.first(), "image should be present");
    assert!(image.metadata.get(FILE_UPLOAD_TOKEN_KEY).is_none());
    assert_eq!(
        image.metadata.get("blurhash").and_then(|v| v.as_str()),
        Some("abc")
    );
}

#[tokio::test]
async fn custom_file_storage_can_normalize_image_metadata() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache = UsernameCache::local_only("test:chat:image:".to_string(), 100, 60);
    let service = test_chat_service_with_file_storage(
        &pool,
        username_cache.clone(),
        Arc::new(PrefixingFileStorageService),
    );
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_file_storage_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:image:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Image Storage Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        FileStorageRepository::new(pool.clone())
            .upsert_object(
                "test-storage",
                "normalized/raw/image.webp",
                "image/webp",
                123,
                &hex::encode(Sha256::digest(b"raw/image.webp")),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "normalized image object should be registered",
    );

    let event = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("image-storage-client-id".to_string()),
                content: String::new(),
                message_type: ChatMessageType::Image,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: vec![NewStoredFile {
                    id: "image-storage-1".to_string(),
                    storage_backend: "client".to_string(),
                    object_key: "raw/image.webp".to_string(),
                    url: None,
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(123),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            })
            .await,
        "image message should be stored",
    );

    let image = some(event.message.images.first(), "image should be present");
    assert_eq!(image.storage_backend, "test-storage");
    assert_eq!(image.object_key, "normalized/raw/image.webp");
    assert_eq!(
        image.url.as_deref(),
        Some("https://cdn.invalid/image-storage-1")
    );
}

#[tokio::test]
async fn deleting_image_message_releases_image_objects() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache = UsernameCache::local_only("test:chat:delete-image:".to_string(), 100, 60);
    let storage = Arc::new(RecordingFileStorageService::default());
    let service =
        test_chat_service_with_file_storage(&pool, username_cache.clone(), storage.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_delete_image_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:delete-image:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Delete Image Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        FileStorageRepository::new(pool.clone())
            .upsert_object(
                "test-storage",
                "normalized/raw/delete-image.webp",
                "image/webp",
                123,
                &hex::encode(Sha256::digest(b"raw/delete-image.webp")),
                &serde_json::Value::Object(Default::default()),
            )
            .await,
        "normalized image object should be registered",
    );

    let created = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("delete-image-client-id".to_string()),
                content: String::new(),
                message_type: ChatMessageType::Image,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: vec![NewStoredFile {
                    id: "delete-image-1".to_string(),
                    storage_backend: "client".to_string(),
                    object_key: "raw/delete-image.webp".to_string(),
                    url: None,
                    mime_type: Some("image/webp".to_string()),
                    size_bytes: Some(123),
                    width: Some(640),
                    height: Some(480),
                    metadata: serde_json::Value::Object(Default::default()),
                }],
            })
            .await,
        "image message should be stored",
    );

    ok(
        service
            .delete_message_event(DeleteChatMessage {
                room_id: room.id,
                message_id: created.message.message.id,
                user_id: user.id,
                client_operation_id: None,
                reason: None,
                expected_version: Some(created.message.message.version),
            })
            .await,
        "image message should delete cleanly",
    );

    let deleted_object_keys = ok(
        storage.deleted_object_keys.lock(),
        "deleted object keys lock",
    )
    .clone();
    assert_eq!(
        deleted_object_keys,
        vec!["normalized/raw/delete-image.webp".to_string()]
    );
    let deleted_origins = ok(storage.deleted_origins.lock(), "deleted origins lock").clone();
    assert_eq!(deleted_origins, vec!["reference_released".to_string()]);
}

#[tokio::test]
async fn concurrent_idempotent_send_returns_existing_created_event() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache =
        UsernameCache::local_only("test:chat:idempotent-send:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_idempotent_send_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:idempotent-send:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Idempotent Send Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let request = SendChatMessage {
        room_id: room.id,
        user_id: user.id,
        client_message_id: Some("same-client-message-id".to_string()),
        content: "same payload".to_string(),
        message_type: ChatMessageType::Text,
        reply_to_message_id: None,
        metadata: serde_json::Value::Object(Default::default()),
        images: Vec::new(),
    };
    let worker_count = 6;
    let barrier = Arc::new(Barrier::new(worker_count));
    let mut handles = Vec::new();
    for _ in 0..worker_count {
        let service = service.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            service.send_message_event_outcome(request).await
        }));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(ok(
            joined(handle.await, "send task should finish"),
            "idempotent send should succeed",
        ));
    }
    let first = &some(outcomes.first(), "event should be returned").event;
    for outcome in &outcomes {
        let event = &outcome.event;
        assert_eq!(event.event_id, first.event_id);
        assert_eq!(event.message.message.id, first.message.message.id);
        assert_eq!(event.kind, ChatEventKind::Created);
    }
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.inserted).count(),
        1
    );

    let message_count = ok(
        sqlx::query_scalar::<_, i64>(
            r"
        SELECT COUNT(*) AS count
        FROM chat_messages
        WHERE room_id = $1 AND user_id = $2 AND client_message_id = $3
        ",
        )
        .bind(room.id.as_i64())
        .bind(user.id.as_i64())
        .bind("same-client-message-id")
        .fetch_one(&pool)
        .await,
        "message count should load",
    );
    assert_eq!(message_count, 1);

    let event_count = ok(
        sqlx::query_scalar::<_, i64>(
            r"
        SELECT COUNT(*) AS count
        FROM chat_message_events
        WHERE room_id = $1
          AND message_id = $2
          AND event_type = $3
        ",
        )
        .bind(room.id.as_i64())
        .bind(first.message.message.id)
        .bind("chat_message_created")
        .fetch_one(&pool)
        .await,
        "event count should load",
    );
    assert_eq!(event_count, 1);
}

#[tokio::test]
async fn concurrent_same_edit_returns_existing_edit_event() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache =
        UsernameCache::local_only("test:chat:concurrent-edit:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_concurrent_edit_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:concurrent-edit:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Concurrent Edit Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let created = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("concurrent-edit-created".to_string()),
                content: "before edit".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "message should be stored",
    );
    let request = EditChatMessage {
        room_id: room.id,
        message_id: created.message.message.id,
        user_id: user.id,
        client_operation_id: None,
        content: "after edit".to_string(),
        metadata: serde_json::json!({"edited": true}),
        expected_version: Some(created.message.message.version),
    };
    let worker_count = 6;
    let barrier = Arc::new(Barrier::new(worker_count));
    let mut handles = Vec::new();
    for _ in 0..worker_count {
        let service = service.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            service.edit_message_outcome(request).await
        }));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(ok(
            joined(handle.await, "edit task should finish"),
            "same edit should converge",
        ));
    }
    let first = &some(outcomes.first(), "event should be returned").event;
    for outcome in &outcomes {
        let event = &outcome.event;
        assert_eq!(event.event_id, first.event_id);
        assert_eq!(event.kind, ChatEventKind::Edited);
        assert_eq!(
            event.message.message.version,
            created.message.message.version + 1
        );
    }
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.inserted).count(),
        1
    );

    let edit_event_count = ok(
        sqlx::query_scalar::<_, i64>(
            r"
        SELECT COUNT(*) AS count
        FROM chat_message_events
        WHERE room_id = $1
          AND message_id = $2
          AND event_type = $3
        ",
        )
        .bind(room.id.as_i64())
        .bind(created.message.message.id)
        .bind("chat_message_edited")
        .fetch_one(&pool)
        .await,
        "edit event count should load",
    );
    assert_eq!(edit_event_count, 1);
}

#[tokio::test]
async fn concurrent_same_delete_returns_existing_delete_event() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache =
        UsernameCache::local_only("test:chat:concurrent-delete:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_concurrent_delete_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:concurrent-delete:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Concurrent Delete Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let created = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("concurrent-delete-created".to_string()),
                content: "delete me".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "message should be stored",
    );
    let request = DeleteChatMessage {
        room_id: room.id,
        message_id: created.message.message.id,
        user_id: user.id,
        client_operation_id: None,
        reason: Some("cleanup".to_string()),
        expected_version: Some(created.message.message.version),
    };
    let worker_count = 6;
    let barrier = Arc::new(Barrier::new(worker_count));
    let mut handles = Vec::new();
    for _ in 0..worker_count {
        let service = service.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            service.delete_message_event_outcome(request).await
        }));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(ok(
            joined(handle.await, "delete task should finish"),
            "same delete should converge",
        ));
    }
    let first = &some(outcomes.first(), "event should be returned").event;
    for outcome in &outcomes {
        let event = &outcome.event;
        assert_eq!(event.event_id, first.event_id);
        assert_eq!(event.kind, ChatEventKind::Deleted);
        assert_eq!(
            event.message.message.version,
            created.message.message.version + 1
        );
    }
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.inserted).count(),
        1
    );

    let delete_event_count = ok(
        sqlx::query_scalar::<_, i64>(
            r"
        SELECT COUNT(*) AS count
        FROM chat_message_events
        WHERE room_id = $1
          AND message_id = $2
          AND event_type = $3
        ",
        )
        .bind(room.id.as_i64())
        .bind(created.message.message.id)
        .bind("chat_message_deleted")
        .fetch_one(&pool)
        .await,
        "delete event count should load",
    );
    assert_eq!(delete_event_count, 1);
}

#[tokio::test]
async fn chat_reactions_update_history_and_emit_reaction_events() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache = UsernameCache::local_only("test:chat:reactions:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let owner = ok(
        user_repository
            .create(&User::new(
                "chat_reaction_owner".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "owner should be created",
    );
    let member = ok(
        user_repository
            .create(&User::new(
                "chat_reaction_member".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "member should be created",
    );
    ok(
        username_cache.set(&owner.id, &owner.username).await,
        "owner username cache should write",
    );
    ok(
        username_cache.set(&member.id, &member.username).await,
        "member username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:reactions:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Reaction Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        room_service.join_room(room.id, member.id, None).await,
        "member should join room",
    );
    let message = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("reaction-message".to_string()),
                content: "react to this".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "message should be stored",
    );

    let owner_reaction = ok(
        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: owner.id,
                reaction_key: "like".to_string(),
                enabled: true,
            })
            .await,
        "owner reaction should be stored",
    );
    assert_eq!(owner_reaction.event.kind, ChatEventKind::ReactionsChanged);
    assert_eq!(owner_reaction.event.message.reactions.len(), 1);
    assert_eq!(owner_reaction.event.message.reactions[0].key, "like");
    assert_eq!(owner_reaction.event.message.reactions[0].count, 1);
    assert!(owner_reaction.event.message.reactions[0].reacted_by_me);

    ok(
        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: member.id,
                reaction_key: "like".to_string(),
                enabled: true,
            })
            .await,
        "member reaction should be stored",
    );

    let (owner_history, _) = ok(
        service
            .get_history_with_images_for_viewer(&room.id, None, 10, false, Some(&owner.id))
            .await,
        "owner history should load",
    );
    let owner_summary = some(
        owner_history[0]
            .reactions
            .iter()
            .find(|reaction| reaction.key == "like"),
        "like summary should exist",
    );
    assert_eq!(owner_summary.count, 2);
    assert!(owner_summary.reacted_by_me);

    ok(
        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: owner.id,
                reaction_key: "like".to_string(),
                enabled: false,
            })
            .await,
        "owner reaction should clear",
    );

    let (owner_history, _) = ok(
        service
            .get_history_with_images_for_viewer(&room.id, None, 10, false, Some(&owner.id))
            .await,
        "owner history should reload",
    );
    let owner_summary = some(
        owner_history[0]
            .reactions
            .iter()
            .find(|reaction| reaction.key == "like"),
        "like summary should remain",
    );
    assert_eq!(owner_summary.count, 1);
    assert!(!owner_summary.reacted_by_me);

    let (member_history, _) = ok(
        service
            .get_history_with_images_for_viewer(&room.id, None, 10, false, Some(&member.id))
            .await,
        "member history should load",
    );
    let member_summary = some(
        member_history[0]
            .reactions
            .iter()
            .find(|reaction| reaction.key == "like"),
        "like summary should exist",
    );
    assert_eq!(member_summary.count, 1);
    assert!(member_summary.reacted_by_me);

    let page = ok(
        service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                None,
                10,
            )
            .await,
        "reaction users should load",
    );
    assert_eq!(page.total, 1);
    assert_eq!(page.users.len(), 1);
    assert_eq!(page.users[0].user_id, member.id);
    assert!(page.next_cursor.is_none());

    ok(
        service
            .set_reaction_event_outcome(SetChatReaction {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: owner.id,
                reaction_key: "like".to_string(),
                enabled: true,
            })
            .await,
        "owner reaction should be restored",
    );
    let refreshed_page = ok(
        service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                None,
                10,
            )
            .await,
        "reaction users should reload after cache invalidation",
    );
    assert_eq!(refreshed_page.total, 2);
    assert_eq!(refreshed_page.users.len(), 2);
    let page = ok(
        service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                None,
                1,
            )
            .await,
        "first reaction user page should load",
    );
    assert_eq!(page.total, 2);
    assert_eq!(page.users.len(), 1);
    let cursor = some(page.next_cursor, "next cursor should be present");
    let next = ok(
        service
            .list_reaction_users(
                &room.id,
                message.message.message.id,
                &member.id,
                "like",
                Some(cursor),
                1,
            )
            .await,
        "second reaction user page should load",
    );
    assert_eq!(next.total, 2);
    assert_eq!(next.users.len(), 1);
    assert_ne!(page.users[0].user_id, next.users[0].user_id);
}

#[tokio::test]
async fn read_state_tracks_unread_count_and_stays_monotonic() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache = UsernameCache::local_only("test:chat:read-state:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_read_state_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let reader = ok(
        user_repository
            .create(&User::new(
                "chat_read_state_reader".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "reader should be created",
    );
    ok(
        username_cache.set(&reader.id, &reader.username).await,
        "reader username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:read-state:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Read State Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        room_service.join_room(room.id, reader.id, None).await,
        "reader should join room",
    );

    let first = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("read-state-1".to_string()),
                content: "first".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "first message should be stored",
    );
    let second = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("read-state-2".to_string()),
                content: "second".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "second message should be stored",
    );
    ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: reader.id,
                client_message_id: Some("read-state-reader-own".to_string()),
                content: "reader own message".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "reader own message should be stored",
    );
    ok(
        service
            .edit_message(EditChatMessage {
                room_id: room.id,
                message_id: first.message.message.id,
                user_id: user.id,
                client_operation_id: None,
                content: "first edited after second".to_string(),
                metadata: serde_json::Value::Object(Default::default()),
                expected_version: Some(first.message.message.version),
            })
            .await,
        "older message should be editable after newer message",
    );

    let initial = ok(
        service.get_read_state(&room.id, &reader.id).await,
        "read state should load",
    );
    assert_eq!(initial.unread_count, 2);
    assert_eq!(initial.state.last_read_message_id, None);

    let after_first = ok(
        service
            .mark_read(MarkChatRead {
                room_id: room.id,
                user_id: reader.id,
                message_id: first.message.message.id,
            })
            .await,
        "first message should be marked read",
    );
    assert_eq!(after_first.unread_count, 1);
    assert_eq!(
        after_first.state.last_read_message_id,
        Some(first.message.message.id)
    );
    ok(
        sqlx::query(
            r"
        UPDATE chat_read_states
        SET last_read_message_id = NULL, last_read_message_created_at = NULL
        WHERE room_id = $1 AND user_id = $2
        ",
        )
        .bind(room.id.as_i64())
        .bind(reader.id.as_i64())
        .execute(&pool)
        .await,
        "read state message cursor should be cleared",
    );
    let event_sequence_fallback = ok(
        service.get_read_state(&room.id, &reader.id).await,
        "read state should use event sequence fallback",
    );
    assert_eq!(event_sequence_fallback.unread_count, 1);
    assert_eq!(
        event_sequence_fallback.state.last_read_event_sequence,
        after_first.state.last_read_event_sequence
    );

    let after_second = ok(
        service
            .mark_read(MarkChatRead {
                room_id: room.id,
                user_id: reader.id,
                message_id: second.message.message.id,
            })
            .await,
        "second message should be marked read",
    );
    assert_eq!(after_second.unread_count, 0);
    assert_eq!(
        after_second.state.last_read_message_id,
        Some(second.message.message.id)
    );

    let stale = ok(
        service
            .mark_read(MarkChatRead {
                room_id: room.id,
                user_id: reader.id,
                message_id: first.message.message.id,
            })
            .await,
        "stale read cursor should be ignored",
    );
    assert_eq!(stale.unread_count, 0);
    assert_eq!(
        stale.state.last_read_message_id,
        Some(second.message.message.id)
    );
}

#[tokio::test]
async fn message_context_returns_messages_around_anchor_in_chronological_order() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache = UsernameCache::local_only("test:chat:context:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_context_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:context:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Context Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let mut messages = Vec::new();
    for index in 1..=5 {
        let event = ok(
            service
                .send_message_event(SendChatMessage {
                    room_id: room.id,
                    user_id: user.id,
                    client_message_id: Some(format!("context-{index}")),
                    content: format!("message {index}"),
                    message_type: ChatMessageType::Text,
                    reply_to_message_id: None,
                    metadata: serde_json::Value::Object(Default::default()),
                    images: Vec::new(),
                })
                .await,
            "message should be stored",
        );
        messages.push(event.message.message);
    }

    let context = ok(
        service
            .get_message_context(&room.id, messages[2].id, 2, 2, false)
            .await,
        "context should load",
    );

    assert_eq!(
        context
            .before
            .iter()
            .map(|message| message.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["message 1", "message 2"]
    );
    assert_eq!(context.anchor.message.content, "message 3");
    assert_eq!(
        context
            .after
            .iter()
            .map(|message| message.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["message 4", "message 5"]
    );
}

#[tokio::test]
async fn chat_text_validation_rejects_whitespace_send_and_edit() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let username_cache =
        UsernameCache::local_only("test:chat:text-validation:".to_string(), 100, 60);
    let service = test_chat_service(&pool, username_cache.clone());
    let user_repository = Arc::new(UserRepository::new(pool.clone()));
    let user = ok(
        user_repository
            .create(&User::new(
                "chat_text_validation_user".to_string(),
                SignupMethod::Password,
            ))
            .await,
        "user should be created",
    );
    ok(
        username_cache.set(&user.id, &user.username).await,
        "username cache should write",
    );
    let room_service = ok(
        RoomService::new_for_tests(
            pool.clone(),
            (*test_user_service(
                &pool,
                UsernameCache::local_only("test:chat:text-validation:room:".to_string(), 100, 60),
            ))
            .clone(),
        ),
        "room service should build",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Text Validation Room".to_string(),
                String::new(),
                user.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let whitespace_send = err(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("whitespace-send".to_string()),
                content: "   \n\t ".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "whitespace-only chat should be rejected",
    );
    assert!(matches!(whitespace_send, Error::InvalidInput(_)));

    let message = ok(
        service
            .send_message_event(SendChatMessage {
                room_id: room.id,
                user_id: user.id,
                client_message_id: Some("valid-before-edit".to_string()),
                content: "valid".to_string(),
                message_type: ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await,
        "valid message should be stored",
    );

    let whitespace_edit = err(
        service
            .edit_message(EditChatMessage {
                room_id: room.id,
                message_id: message.message.message.id,
                user_id: user.id,
                client_operation_id: None,
                content: " \n ".to_string(),
                metadata: serde_json::Value::Object(Default::default()),
                expected_version: Some(message.message.message.version),
            })
            .await,
        "whitespace-only edit should be rejected",
    );
    assert!(matches!(whitespace_edit, Error::InvalidInput(_)));
}
