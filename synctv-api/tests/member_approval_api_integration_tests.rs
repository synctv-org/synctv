#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use chrono::Utc;
use synctv_api::{AdminApiConfig, AdminApiImpl, ApiError, ClientApiImpl};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::RequireApproval, ReviewRequestId, ReviewStatus, RoomSettings, SignupMethod,
        User, UserId, UserRole, UserStatus,
    },
    repository::{ProviderInstanceRepository, SettingsRepository, UserRepository},
    service::{
        AuditService, BruteForceProtection, EmailConfig, EmailConfigProvider, EmailService,
        InMemoryTokenBlacklistStore, JwtService, PublishKeyService, RemoteProviderManager,
        RoomService, RoomServiceOptions, RuntimeSettingsStore, SettingsService, UserService,
    },
    Config,
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

struct DisabledEmailConfigProvider;

impl EmailConfigProvider for DisabledEmailConfigProvider {
    fn current_config(&self) -> synctv_core::Result<Option<EmailConfig>> {
        Ok(None)
    }
}

fn make_user(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

fn public_id_codec() -> synctv_api::PublicIdCodec {
    synctv_api::PublicIdCodec::plain()
}

fn review_request_public_id(id: i64) -> String {
    public_id_codec()
        .encode_review_request_id(ReviewRequestId::expect_positive(id))
        .unwrap()
}

async fn pending_join_request_id(
    pool: &sqlx::PgPool,
    room_id: synctv_core::models::RoomId,
    user_id: UserId,
) -> i64 {
    sqlx::query_scalar!(
        r#"
        SELECT id AS "id!"
        FROM room_join_requests
        WHERE room_id = $1 AND user_id = $2 AND reviewed_at IS NULL
        "#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn join_request_status(pool: &sqlx::PgPool, request_id: i64) -> i16 {
    sqlx::query_scalar!(
        r#"SELECT status AS "status!" FROM room_join_requests WHERE id = $1"#,
        request_id
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn room_member_exists(
    pool: &sqlx::PgPool,
    room_id: synctv_core::models::RoomId,
    user_id: UserId,
) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2) AS "exists!""#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn join_request_review_state(pool: &sqlx::PgPool, request_id: i64) -> (i16, bool) {
    let row = sqlx::query!(
        r#"
        SELECT status AS "status!",
               reviewed_at IS NOT NULL AS "reviewed!"
        FROM room_join_requests
        WHERE id = $1
        "#,
        request_id
    )
    .fetch_one(pool)
    .await
    .unwrap();
    (row.status, row.reviewed)
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));

    ClientApiImpl::new_with_runtime(
        synctv_api::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: connection_manager,
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(public_id_codec()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    )
}

async fn make_admin_api(pool: sqlx::PgPool) -> AdminApiImpl {
    let user_service = Arc::new(make_user_service(&pool));
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service.clone()));
    let room_service = RoomService::new_with_options(
        pool.clone(),
        (*user_service).clone(),
        RoomServiceOptions {
            runtime_settings_store: Some(runtime_settings_store.clone()),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .expect("room service should build");
    let email_service =
        Arc::new(EmailService::new(Arc::new(DisabledEmailConfigProvider)).expect("email service"));
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let publish_key_service = Arc::new(
        PublishKeyService::new(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").unwrap(),
            Arc::new(synctv_core::SystemClock),
            24,
        )
        .expect("publish key service should build"),
    );

    AdminApiImpl::new_with_runtime(
        AdminApiConfig {
            room_service: Arc::new(room_service),
            read_services: support::admin_read_services(user_service.as_ref()),
            user_service,
            settings_service,
            runtime_settings_store: Some(runtime_settings_store),
            email_service,
            connection_service: connection_manager,
            provider_instance_manager,
            live_streaming_infrastructure: None,
            publish_key_service: Some(publish_key_service),
            config: Arc::new(Config::default()),
            audit_service: Arc::new(AuditService::new_unbuffered(pool)),
            public_id_codec: Arc::new(public_id_codec()),
        },
        support::admin_api_runtime(),
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_client_member_approval_api_contracts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("client_member_owner", UserRole::User))
        .await
        .unwrap();
    let add_target = user_repo
        .create(&make_user("client_member_added", UserRole::User))
        .await
        .unwrap();
    let approve_target = user_repo
        .create(&make_user("client_member_approve", UserRole::User))
        .await
        .unwrap();
    let reject_target = user_repo
        .create(&make_user("client_member_reject", UserRole::User))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Client Approval API Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let add_target_public_id = codec.encode_user_id(add_target.id).unwrap();
    let approve_target_public_id = codec.encode_user_id(approve_target.id).unwrap();

    let added = client_api
        .add_member(
            &owner.id,
            &room_public_id,
            synctv_proto::client::AddMemberRequest {
                user_id: add_target_public_id.clone(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
                remark_name: String::new(),
                display_tag: String::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(added.user_id, add_target_public_id);
    assert_eq!(
        added.role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );

    room_service
        .join_room(room.id, approve_target.id, None)
        .await
        .unwrap();
    let pending_reviews = client_api
        .list_room_join_reviews(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListRoomJoinReviewsRequest {
                page: 1,
                page_size: 10,
                status: synctv_proto::common::ReviewStatus::Pending as i32,
                user_id: approve_target_public_id.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(pending_reviews.total, 1);
    assert_eq!(pending_reviews.reviews.len(), 1);
    assert_eq!(pending_reviews.reviews[0].user_id, approve_target_public_id);
    let approve_request_id = pending_reviews.reviews[0].id.clone();
    let approved = client_api
        .approve_room_join_review(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ApproveRoomJoinReviewRequest {
                request_id: approve_request_id,
            },
        )
        .await
        .unwrap()
        .member
        .expect("approve_room_join_review response member");
    assert_eq!(approved.user_id, approve_target_public_id);

    room_service
        .join_room(room.id, reject_target.id, None)
        .await
        .unwrap();
    let reject_request_id = pending_join_request_id(&pool, room.id, reject_target.id).await;
    let rejected = client_api
        .reject_room_join_review(
            &owner.id,
            &room_public_id,
            synctv_proto::client::RejectRoomJoinReviewRequest {
                request_id: review_request_public_id(reject_request_id),
                reason: "duplicate request".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.id, review_request_public_id(reject_request_id));
    assert_eq!(
        rejected.status,
        synctv_proto::common::ReviewStatus::Rejected as i32
    );

    let rejected_member_exists = room_member_exists(&pool, room.id, reject_target.id).await;
    assert!(
        !rejected_member_exists,
        "rejected room join reviews must not create member rows"
    );
    let rejected_status = join_request_status(&pool, reject_request_id).await;
    assert_eq!(rejected_status, i16::from(ReviewStatus::Rejected));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_member_approval_api_contracts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_api = make_admin_api(pool.clone()).await;
    let room_service = admin_api.room_service.clone();

    let root_admin = user_repo
        .create(&make_user("admin_member_root", UserRole::Root))
        .await
        .unwrap();
    let owner = user_repo
        .create(&make_user("admin_member_owner", UserRole::User))
        .await
        .unwrap();
    let add_target = user_repo
        .create(&make_user("admin_member_added", UserRole::User))
        .await
        .unwrap();
    let approve_target = user_repo
        .create(&make_user("admin_member_approve", UserRole::User))
        .await
        .unwrap();
    let reject_target = user_repo
        .create(&make_user("admin_member_reject", UserRole::User))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Admin Approval API Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    let added = admin_api
        .add_member(
            synctv_proto::admin::AddMemberRequest {
                room_id: public_id_codec().encode_room_id(room.id).unwrap(),
                user_id: public_id_codec().encode_user_id(add_target.id).unwrap(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
                remark_name: String::new(),
                display_tag: String::new(),
            },
            &root_admin.id,
            &synctv_api::AdminRequestContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        added.user_id,
        public_id_codec().encode_user_id(add_target.id).unwrap()
    );
    assert_eq!(
        added.role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );

    room_service
        .join_room(room.id, approve_target.id, None)
        .await
        .unwrap();
    let approve_request_id = pending_join_request_id(&pool, room.id, approve_target.id).await;
    let approved = admin_api
        .approve_room_join_review(
            synctv_proto::admin::ApproveRoomJoinReviewRequest {
                request_id: review_request_public_id(approve_request_id),
            },
            &root_admin.id,
            &synctv_api::AdminRequestContext::default(),
        )
        .await
        .unwrap()
        .member
        .expect("admin approve review response member");
    assert_eq!(
        approved.user_id,
        public_id_codec().encode_user_id(approve_target.id).unwrap()
    );

    room_service
        .join_room(room.id, reject_target.id, None)
        .await
        .unwrap();
    let reject_request_id = pending_join_request_id(&pool, room.id, reject_target.id).await;
    let rejected = admin_api
        .reject_room_join_review(
            synctv_proto::admin::RejectRoomJoinReviewRequest {
                request_id: review_request_public_id(reject_request_id),
                reason: "policy violation".to_string(),
            },
            &root_admin.id,
            &synctv_api::AdminRequestContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.id, review_request_public_id(reject_request_id));
    assert_eq!(
        rejected.status,
        synctv_proto::common::ReviewStatus::Rejected as i32
    );

    let rejected_member_exists = room_member_exists(&pool, room.id, reject_target.id).await;
    assert!(
        !rejected_member_exists,
        "rejected room join reviews must not create member rows"
    );
    let rejected_status = join_request_status(&pool, reject_request_id).await;
    assert_eq!(rejected_status, i16::from(ReviewStatus::Rejected));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_client_room_join_review_uses_request_id_not_user_id() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("stale_review_owner", UserRole::User))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("stale_review_target", UserRole::User))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Stale Request-ID Review Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();
    let old_request_id = pending_join_request_id(&pool, room.id, target.id).await;
    client_api
        .reject_room_join_review(
            &owner.id,
            &room_public_id,
            synctv_proto::client::RejectRoomJoinReviewRequest {
                request_id: review_request_public_id(old_request_id),
                reason: "first request rejected".to_string(),
            },
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();
    let new_request_id = pending_join_request_id(&pool, room.id, target.id).await;
    assert_ne!(old_request_id, new_request_id);

    let stale_approve_error = client_api
        .approve_room_join_review(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ApproveRoomJoinReviewRequest {
                request_id: review_request_public_id(old_request_id),
            },
        )
        .await
        .expect_err("reviewing a non-pending historical request must fail");
    assert!(
        matches!(stale_approve_error, ApiError::NotFound(ref message) if message.contains("Pending join request")),
        "stale request-id review must not fall back to user-id approval, got: {stale_approve_error:?}"
    );

    let new_status = join_request_status(&pool, new_request_id).await;
    assert_eq!(new_status, i16::from(ReviewStatus::Pending));

    let member_exists = room_member_exists(&pool, room.id, target.id).await;
    assert!(!member_exists);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_room_join_review_uses_request_id_not_user_id() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let admin_api = make_admin_api(pool.clone()).await;
    let room_service = admin_api.room_service.clone();

    let root_admin = user_repo
        .create(&make_user("admin_stale_review_root", UserRole::Root))
        .await
        .unwrap();
    let owner = user_repo
        .create(&make_user("admin_stale_review_owner", UserRole::User))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("admin_stale_review_target", UserRole::User))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Admin Stale Request-ID Review Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();
    let old_request_id = pending_join_request_id(&pool, room.id, target.id).await;
    admin_api
        .reject_room_join_review(
            synctv_proto::admin::RejectRoomJoinReviewRequest {
                request_id: review_request_public_id(old_request_id),
                reason: "first request rejected".to_string(),
            },
            &root_admin.id,
            &synctv_api::AdminRequestContext::default(),
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();
    let new_request_id = pending_join_request_id(&pool, room.id, target.id).await;
    assert_ne!(old_request_id, new_request_id);

    let stale_approve_error = admin_api
        .approve_room_join_review(
            synctv_proto::admin::ApproveRoomJoinReviewRequest {
                request_id: review_request_public_id(old_request_id),
            },
            &root_admin.id,
            &synctv_api::AdminRequestContext::default(),
        )
        .await
        .expect_err("reviewing a non-pending historical request must fail");
    assert!(
        matches!(stale_approve_error, ApiError::NotFound(ref message) if message.contains("Pending join request")),
        "admin stale request-id review must not fall back to user-id approval, got: {stale_approve_error:?}"
    );

    let new_status = join_request_status(&pool, new_request_id).await;
    assert_eq!(new_status, i16::from(ReviewStatus::Pending));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_join_review_approval_rejects_globally_banned_target() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("banned_review_owner", UserRole::User))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("banned_review_target", UserRole::User))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Banned Target Review Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();
    let request_id = pending_join_request_id(&pool, room.id, target.id).await;

    user_repo
        .ban(&target.id, None, Some("test ban".to_string()))
        .await
        .unwrap();

    let approve_error = client_api
        .approve_room_join_review(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ApproveRoomJoinReviewRequest {
                request_id: review_request_public_id(request_id),
            },
        )
        .await
        .expect_err("banned users must not be approved into rooms");
    assert!(
        matches!(approve_error, ApiError::Authorization(ref message) if message.contains("banned")),
        "approval should fail with a ban-related authorization error, got: {approve_error:?}"
    );

    let review_state = join_request_review_state(&pool, request_id).await;
    assert_eq!(review_state.0, i16::from(ReviewStatus::Pending));
    assert!(!review_state.1);

    let member_exists = room_member_exists(&pool, room.id, target.id).await;
    assert!(!member_exists);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_member_resolves_existing_room_join_review() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("add_resolves_review_owner", UserRole::User))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("add_resolves_review_target", UserRole::User))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };
    let (room, _) = room_service
        .create_room(
            "Add Resolves Review Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, target.id, None)
        .await
        .unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let target_public_id = codec.encode_user_id(target.id).unwrap();
    let request_id = pending_join_request_id(&pool, room.id, target.id).await;

    client_api
        .add_member(
            &owner.id,
            &room_public_id,
            synctv_proto::client::AddMemberRequest {
                user_id: target_public_id.clone(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
                remark_name: String::new(),
                display_tag: String::new(),
            },
        )
        .await
        .unwrap();

    let review_state = join_request_review_state(&pool, request_id).await;
    assert_eq!(review_state.0, i16::from(ReviewStatus::Approved));
    assert!(review_state.1);

    let pending_reviews = client_api
        .list_room_join_reviews(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListRoomJoinReviewsRequest {
                page: 1,
                page_size: 10,
                status: synctv_proto::common::ReviewStatus::Pending as i32,
                user_id: target_public_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(pending_reviews.total, 0);
}
