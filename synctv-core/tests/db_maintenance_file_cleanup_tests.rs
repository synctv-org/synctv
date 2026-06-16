//! `DatabaseMaintenanceService` file storage cleanup tests.

use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use synctv_core::{
    models::{
        CreateFileUploadSession, FileReferenceTarget, FileUploadSessionCreateResult, NewStoredFile,
        Room, RoomId, RoomStatus, SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::{FileStorageRepository, RoomRepository, UpsertFileObject, UserRepository},
    service::{
        db_maintenance::DatabaseMaintenanceOptions, AlwaysLeader, DatabaseMaintenanceService,
        FileStorageCleanupOrigin, FileStorageContext, FileStorageService,
    },
    Error,
};
use synctv_core_testing::{create_test_pool, ok};

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
    ) -> synctv_core::Result<FileUploadSessionCreateResult> {
        Err(Error::Internal("not used".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> synctv_core::Result<Vec<NewStoredFile>> {
        Ok(files)
    }

    async fn delete_files(
        &self,
        origin: FileStorageCleanupOrigin,
        files: &[FileReferenceTarget],
    ) -> synctv_core::Result<()> {
        let mut deleted = ok(
            self.deleted_object_keys.lock(),
            "deleted object key recorder lock should be acquired",
        );
        deleted.extend(files.iter().map(|file| file.object_key.clone()));
        let mut origins = ok(
            self.deleted_origins.lock(),
            "deleted origin recorder lock should be acquired",
        );
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
    ) -> synctv_core::Result<FileUploadSessionCreateResult> {
        Err(Error::Internal("not used".to_string()))
    }

    async fn prepare_files(
        &self,
        _context: FileStorageContext<'_>,
        files: Vec<NewStoredFile>,
    ) -> synctv_core::Result<Vec<NewStoredFile>> {
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

fn maintenance_with_storage(
    pool: sqlx::PgPool,
    storage: Arc<dyn FileStorageService>,
) -> DatabaseMaintenanceService {
    DatabaseMaintenanceService::new_with_options(
        pool,
        Arc::new(AlwaysLeader),
        DatabaseMaintenanceOptions {
            file_storage_service: Some(storage),
            ..DatabaseMaintenanceOptions::default()
        },
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn old_chat_message_cleanup_deletes_image_objects() {
    let (_container, pool) = create_test_pool().await;
    let storage = Arc::new(RecordingFileStorageService::default());

    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id);
    let room = ok(
        RoomRepository::new(pool.clone()).create(&room).await,
        "room should be created",
    );

    let old_at = Utc::now() - Duration::days(120);
    ok(
        insert_old_chat_message_with_image(
            &pool,
            room.id,
            user.id,
            9_001,
            "cleanup-image-1",
            "normalized/raw/cleanup-image.webp",
            old_at,
        )
        .await,
        "chat message should be inserted",
    );

    let service = maintenance_with_storage(pool.clone(), storage.clone());

    ok(
        service.run_cleanup_old_chat_messages().await,
        "old chat cleanup should succeed",
    );

    let deleted_object_keys = ok(
        storage.deleted_object_keys.lock(),
        "deleted object key recorder lock should be acquired",
    )
    .clone();
    assert_eq!(
        deleted_object_keys,
        vec!["normalized/raw/cleanup-image.webp".to_string()]
    );
    let deleted_origins = ok(
        storage.deleted_origins.lock(),
        "deleted origin recorder lock should be acquired",
    )
    .clone();
    assert_eq!(deleted_origins, vec!["retention_expired".to_string()]);

    let message_exists: bool = ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id = $1)")
            .bind(9_001_i64)
            .fetch_one(&pool)
            .await,
        "message existence query should succeed",
    );
    assert!(!message_exists);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn failed_old_file_cleanup_is_persisted_and_retried() {
    let (_container, pool) = create_test_pool().await;
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id);
    let room = ok(
        RoomRepository::new(pool.clone()).create(&room).await,
        "room should be created",
    );
    let old_at = Utc::now() - Duration::days(120);
    ok(
        insert_old_chat_message_with_image(
            &pool,
            room.id,
            user.id,
            9_002,
            "retry-image-1",
            "normalized/raw/retry-image.webp",
            old_at,
        )
        .await,
        "chat message should be inserted",
    );

    let failing_service =
        maintenance_with_storage(pool.clone(), Arc::new(FailingFileStorageService));
    ok(
        failing_service.run_cleanup_old_chat_messages().await,
        "old chat cleanup should succeed even when object cleanup fails",
    );

    let queued: i64 = ok(
        sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT FROM file_cleanup_jobs WHERE object_key = $1 AND completed_at IS NULL",
        )
        .bind("normalized/raw/retry-image.webp")
        .fetch_one(&pool)
        .await,
        "queued cleanup job should be queryable",
    );
    assert_eq!(queued, 1);
    let due = ok(
        FileStorageRepository::new(pool.clone())
            .count_due_cleanup_jobs()
            .await,
        "due cleanup job count should be queryable",
    );
    assert_eq!(due, 1);

    let recording_storage = Arc::new(RecordingFileStorageService::default());
    let retry_service = maintenance_with_storage(pool.clone(), recording_storage.clone());
    ok(
        retry_service.run_retry_file_cleanup_jobs().await,
        "cleanup retry should succeed",
    );

    let deleted_object_keys = ok(
        recording_storage.deleted_object_keys.lock(),
        "deleted object key recorder lock should be acquired",
    )
    .clone();
    assert_eq!(
        deleted_object_keys,
        vec!["normalized/raw/retry-image.webp".to_string()]
    );
    let deleted_origins = ok(
        recording_storage.deleted_origins.lock(),
        "deleted origin recorder lock should be acquired",
    )
    .clone();
    assert_eq!(deleted_origins, vec!["cleanup_retry".to_string()]);

    let completed: bool = ok(
        sqlx::query_scalar(
            "SELECT completed_at IS NOT NULL FROM file_cleanup_jobs WHERE object_key = $1",
        )
        .bind("normalized/raw/retry-image.webp")
        .fetch_one(&pool)
        .await,
        "completed cleanup job should be queryable",
    );
    assert!(completed);
    let due_after_retry = ok(
        FileStorageRepository::new(pool.clone())
            .count_due_cleanup_jobs()
            .await,
        "due cleanup job count should be queryable after retry",
    );
    assert_eq!(due_after_retry, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn expired_file_reference_cleanup_releases_reference() {
    let (_container, pool) = create_test_pool().await;
    let storage = Arc::new(RecordingFileStorageService::default());
    let repository = FileStorageRepository::new(pool.clone());
    ok(
        repository
            .upsert_object(UpsertFileObject {
                storage_backend: "database",
                object_key: "database/files/expired.webp",
                mime_type: "image/webp",
                size_bytes: 7,
                content_manifest_sha256: &"a".repeat(64),
                metadata: &serde_json::Value::Object(Default::default()),
            })
            .await,
        "object should be registered",
    );
    let mut tx = ok(pool.begin().await, "transaction should begin");
    ok(
        FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            "database",
            "database/files/expired.webp",
            "temporary_file",
            "temp:1",
            Some(Utc::now() - Duration::minutes(5)),
            &serde_json::Value::Object(Default::default()),
        )
        .await,
        "expired reference should insert",
    );
    ok(tx.commit().await, "transaction should commit");

    let service = maintenance_with_storage(pool.clone(), storage.clone());
    let released = ok(
        service.run_cleanup_expired_file_references().await,
        "expired reference cleanup should succeed",
    );

    assert_eq!(released, 1);
    assert_eq!(
        ok(
            storage.deleted_object_keys.lock(),
            "deleted object key recorder lock should be acquired"
        )
        .clone(),
        vec!["database/files/expired.webp".to_string()]
    );
    assert_eq!(
        ok(
            storage.deleted_origins.lock(),
            "deleted origin recorder lock should be acquired"
        )
        .clone(),
        vec!["reference_expired".to_string()]
    );
}

async fn create_test_user(pool: &sqlx::PgPool) -> User {
    let now = Utc::now();
    let user = User {
        id: UserId::new(),
        username: format!("db_maintenance_user_{}", synctv_common::snanoid!(8)),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    ok(
        UserRepository::new(pool.clone()).create(&user).await,
        "user should be created",
    )
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
    .bind("attachment message")
    .bind(4_i16)
    .bind(1_i16)
    .bind(1_i64)
    .bind(serde_json::Value::Object(Default::default()))
    .bind(created_at)
    .execute(pool)
    .await?;

    sqlx::query(
        r"
        INSERT INTO chat_message_attachments (
            id, kind, room_id, message_id, message_created_at, filename, storage_backend, object_key, url,
            mime_type, size_bytes, width, height, metadata, created_at
        ) VALUES (
            $1, 2, $2, $3, $4, NULL, $5, $6, NULL, NULL, NULL, NULL, NULL, $7, $8
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
