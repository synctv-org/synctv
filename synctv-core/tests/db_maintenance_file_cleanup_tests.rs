//! `DatabaseMaintenanceService` file storage cleanup tests.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use synctv_core::{
    models::{
        CreateFileUploadSession, FileReferenceTarget, FileUploadSession, NewChatImage, Room,
        RoomId, RoomStatus, SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::{FileStorageRepository, RoomRepository, UserRepository},
    service::{
        AlwaysLeader, DatabaseMaintenanceService, FileStorageCleanupOrigin, FileStorageContext,
        FileStorageService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;

#[derive(Default)]
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
        _request: CreateFileUploadSession,
    ) -> synctv_core::Result<FileUploadSession> {
        Err(Error::Internal("not used".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewChatImage>,
    ) -> synctv_core::Result<Vec<NewChatImage>> {
        Ok(files)
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> synctv_core::Result<()> {
        let mut deleted = self.deleted_object_keys.lock().unwrap();
        deleted.extend(files.iter().map(|file| file.object_key.clone()));
        let mut origins = self.deleted_origins.lock().unwrap();
        origins.extend(files.iter().map(|_| origin.as_str().to_string()));
        Ok(())
    }
}

struct FailingFileStorageService;

#[async_trait::async_trait]
impl FileStorageService for FailingFileStorageService {
    fn backend_name(&self) -> &'static str {
        "test-storage"
    }

    async fn create_upload_session(
        &self,
        _request: CreateFileUploadSession,
    ) -> synctv_core::Result<FileUploadSession> {
        Err(Error::Internal("not used".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewChatImage>,
    ) -> synctv_core::Result<Vec<NewChatImage>> {
        Ok(files)
    }

    async fn delete_files(
        &self,
        _origin: FileStorageCleanupOrigin,
        _files: &[FileReferenceTarget],
    ) -> synctv_core::Result<()> {
        Err(Error::Internal("delete failed".to_string()))
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn old_chat_message_cleanup_deletes_image_objects() {
    let (_container, pool) = create_test_pool().await;
    let storage = Arc::new(RecordingFileStorageService::default());

    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id);
    let room = RoomRepository::new(pool.clone())
        .create(&room)
        .await
        .expect("room should be created");

    let old_at = Utc::now() - Duration::days(120);
    insert_old_chat_message_with_image(
        &pool,
        room.id,
        user.id,
        9_001,
        "cleanup-image-1",
        "normalized/raw/cleanup-image.webp",
        old_at,
    )
    .await
    .expect("chat message should be inserted");

    let service = DatabaseMaintenanceService::new(pool.clone(), Arc::new(AlwaysLeader))
        .with_file_storage_service(storage.clone());

    service
        .run_cleanup_old_chat_messages()
        .await
        .expect("old chat cleanup should succeed");

    let deleted_object_keys = storage.deleted_object_keys.lock().unwrap().clone();
    assert_eq!(
        deleted_object_keys,
        vec!["normalized/raw/cleanup-image.webp".to_string()]
    );
    let deleted_origins = storage.deleted_origins.lock().unwrap().clone();
    assert_eq!(deleted_origins, vec!["retention_expired".to_string()]);

    let message_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id = $1)")
            .bind(9_001_i64)
            .fetch_one(&pool)
            .await
            .expect("message existence query should succeed");
    assert!(!message_exists);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn failed_old_file_cleanup_is_persisted_and_retried() {
    let (_container, pool) = create_test_pool().await;
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id);
    let room = RoomRepository::new(pool.clone())
        .create(&room)
        .await
        .expect("room should be created");
    let old_at = Utc::now() - Duration::days(120);
    insert_old_chat_message_with_image(
        &pool,
        room.id,
        user.id,
        9_002,
        "retry-image-1",
        "normalized/raw/retry-image.webp",
        old_at,
    )
    .await
    .expect("chat message should be inserted");

    let failing_service = DatabaseMaintenanceService::new(pool.clone(), Arc::new(AlwaysLeader))
        .with_file_storage_service(Arc::new(FailingFileStorageService));
    failing_service
        .run_cleanup_old_chat_messages()
        .await
        .expect("old chat cleanup should succeed even when object cleanup fails");

    let queued: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM file_cleanup_jobs WHERE object_key = $1 AND completed_at IS NULL",
    )
    .bind("normalized/raw/retry-image.webp")
    .fetch_one(&pool)
    .await
    .expect("queued cleanup job should be queryable");
    assert_eq!(queued, 1);
    let due = FileStorageRepository::new(pool.clone())
        .count_due_cleanup_jobs()
        .await
        .expect("due cleanup job count should be queryable");
    assert_eq!(due, 1);

    let recording_storage = Arc::new(RecordingFileStorageService::default());
    let retry_service = DatabaseMaintenanceService::new(pool.clone(), Arc::new(AlwaysLeader))
        .with_file_storage_service(recording_storage.clone());
    retry_service
        .run_retry_file_cleanup_jobs()
        .await
        .expect("cleanup retry should succeed");

    let deleted_object_keys = recording_storage
        .deleted_object_keys
        .lock()
        .unwrap()
        .clone();
    assert_eq!(
        deleted_object_keys,
        vec!["normalized/raw/retry-image.webp".to_string()]
    );
    let deleted_origins = recording_storage.deleted_origins.lock().unwrap().clone();
    assert_eq!(deleted_origins, vec!["cleanup_retry".to_string()]);

    let completed: bool = sqlx::query_scalar(
        "SELECT completed_at IS NOT NULL FROM file_cleanup_jobs WHERE object_key = $1",
    )
    .bind("normalized/raw/retry-image.webp")
    .fetch_one(&pool)
    .await
    .expect("completed cleanup job should be queryable");
    assert!(completed);
    let due_after_retry = FileStorageRepository::new(pool.clone())
        .count_due_cleanup_jobs()
        .await
        .expect("due cleanup job count should be queryable after retry");
    assert_eq!(due_after_retry, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn expired_file_reference_cleanup_releases_reference() {
    let (_container, pool) = create_test_pool().await;
    let storage = Arc::new(RecordingFileStorageService::default());
    let repository = FileStorageRepository::new(pool.clone());
    repository
        .upsert_object(
            "database",
            "database/files/expired.webp",
            "image/webp",
            7,
            &"a".repeat(64),
            &serde_json::Value::Object(Default::default()),
        )
        .await
        .expect("object should be registered");
    let mut tx = pool.begin().await.expect("transaction should begin");
    FileStorageRepository::insert_reference_in_tx(
        &mut tx,
        "database",
        "database/files/expired.webp",
        "temporary_file",
        "temp:1",
        Some(Utc::now() - Duration::minutes(5)),
        &serde_json::Value::Object(Default::default()),
    )
    .await
    .expect("expired reference should insert");
    tx.commit().await.expect("transaction should commit");

    let service = DatabaseMaintenanceService::new(pool.clone(), Arc::new(AlwaysLeader))
        .with_file_storage_service(storage.clone());
    let released = service
        .run_cleanup_expired_file_references()
        .await
        .expect("expired reference cleanup should succeed");

    assert_eq!(released, 1);
    assert_eq!(
        storage.deleted_object_keys.lock().unwrap().clone(),
        vec!["database/files/expired.webp".to_string()]
    );
    assert_eq!(
        storage.deleted_origins.lock().unwrap().clone(),
        vec!["reference_expired".to_string()]
    );
}

async fn create_test_user(pool: &sqlx::PgPool) -> User {
    let now = Utc::now();
    let user = User {
        id: UserId::new(),
        username: format!("db_maintenance_user_{}", synctv_common::snanoid!(8)),
        email: Some(format!(
            "db_maintenance_{}@example.com",
            synctv_common::snanoid!(8)
        )),
        password_hash: "test_hash".to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    UserRepository::new(pool.clone())
        .create(&user)
        .await
        .expect("user should be created")
}

fn create_test_room(created_by: UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: "DB Maintenance Room".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        created_by,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

async fn insert_old_chat_message_with_image(
    pool: &sqlx::PgPool,
    room_id: RoomId,
    user_id: UserId,
    message_id: i64,
    image_id: &str,
    object_key: &str,
    created_at: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        INSERT INTO chat_messages (
            id, room_id, user_id, client_message_id, content, message_type, status, version,
            reply_to_message_id, metadata, edited_at, deleted_at, deleted_by, delete_reason,
            created_at
        ) VALUES (
            $1, $2, $3, NULL, $4, $5, $6, $7,
            NULL, $8, NULL, NULL, NULL, NULL,
            $9
        )
        ",
    )
    .bind(message_id)
    .bind(room_id)
    .bind(user_id)
    .bind("image message")
    .bind(4_i16)
    .bind(1_i16)
    .bind(1_i64)
    .bind(serde_json::Value::Object(Default::default()))
    .bind(created_at)
    .execute(pool)
    .await?;

    sqlx::query(
        r"
        INSERT INTO chat_message_images (
            id, room_id, message_id, message_created_at, storage_backend, object_key, url,
            mime_type, size_bytes, width, height, metadata, created_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, NULL, NULL, NULL, NULL, NULL, $7, $8
        )
        ",
    )
    .bind(image_id)
    .bind(room_id)
    .bind(message_id)
    .bind(created_at)
    .bind("test-storage")
    .bind(object_key)
    .bind(serde_json::Value::Object(Default::default()))
    .bind(created_at)
    .execute(pool)
    .await?;

    Ok(())
}
