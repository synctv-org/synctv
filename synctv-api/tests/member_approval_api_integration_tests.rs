#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use synctv_api::impls::{AdminApiImpl, ClientApiImpl};
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::RequireApproval, MemberStatus, RoomSettings, SignupMethod, User, UserId,
        UserRole, UserStatus,
    },
    repository::{
        ProviderInstanceRepository, RoomMemberRepository, SettingsRepository, UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        AuditService, EmailService, InMemoryTokenBlacklistStore, PublishKeyService,
        RemoteProviderManager, RoomService, SettingsRegistry, SettingsService, UserService,
    },
    Config,
};

fn make_user(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache =
        UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let mut service = UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    );
    service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    service
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    ClientApiImpl::new(
        user_service,
        room_service,
        connection_manager,
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
}

async fn make_admin_api(pool: sqlx::PgPool) -> AdminApiImpl {
    let user_service = Arc::new(make_user_service(pool.clone()));
    let mut room_service = RoomService::new(pool.clone(), (*user_service).clone());
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service
        .initialize()
        .await
        .expect("settings initialized");
    let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
    room_service.set_settings_registry(settings_registry.clone());
    let email_service = Arc::new(EmailService::new(None).expect("email service"));
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let publish_key_service = Arc::new(PublishKeyService::new(
        JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").unwrap(),
        24,
    ));

    AdminApiImpl::new(
        Arc::new(room_service),
        user_service,
        settings_service,
        Some(settings_registry),
        email_service,
        connection_manager,
        provider_instance_manager,
        None,
        Some(publish_key_service),
        Arc::new(Config::default()),
        None,
        Arc::new(AuditService::new_unbuffered(pool)),
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_client_member_approval_api_contracts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    let added = client_api
        .add_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::AddMemberRequest {
                user_id: add_target.id.as_str().to_string(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
            },
        )
        .await
        .unwrap()
        .member
        .expect("add_member response member");
    assert_eq!(added.user_id, add_target.id.as_str());
    assert_eq!(
        added.role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );
    assert_eq!(
        added.status,
        synctv_proto::common::MemberStatus::Active as i32
    );

    room_service
        .join_room(room.id.clone(), approve_target.id.clone(), None)
        .await
        .unwrap();
    let approved = client_api
        .approve_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::ApproveMemberRequest {
                user_id: approve_target.id.as_str().to_string(),
            },
        )
        .await
        .unwrap()
        .member
        .expect("approve_member response member");
    assert_eq!(approved.user_id, approve_target.id.as_str());
    assert_eq!(
        approved.status,
        synctv_proto::common::MemberStatus::Active as i32
    );

    room_service
        .join_room(room.id.clone(), reject_target.id.clone(), None)
        .await
        .unwrap();
    let rejected = client_api
        .reject_member(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_proto::client::RejectMemberRequest {
                user_id: reject_target.id.as_str().to_string(),
                reason: "duplicate request".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(rejected.success);

    let rejected_member = member_repo
        .get_any(&room.id, &reject_target.id)
        .await
        .unwrap()
        .expect("rejected member persisted");
    assert_eq!(rejected_member.status, MemberStatus::Rejected);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_member_approval_api_contracts() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

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
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    let added = admin_api
        .add_member(
            synctv_proto::admin::AddMemberRequest {
                room_id: room.id.as_str().to_string(),
                user_id: add_target.id.as_str().to_string(),
                role: synctv_proto::common::RoomMemberRole::Member as i32,
                notify: false,
            },
            &root_admin.id,
            &synctv_api::impls::admin::RequestContext::default(),
        )
        .await
        .unwrap()
        .member
        .expect("admin add_member response member");
    assert_eq!(added.user_id, add_target.id.as_str());
    assert_eq!(
        added.role,
        synctv_proto::common::RoomMemberRole::Member as i32
    );
    assert_eq!(
        added.status,
        synctv_proto::common::MemberStatus::Active as i32
    );

    room_service
        .join_room(room.id.clone(), approve_target.id.clone(), None)
        .await
        .unwrap();
    let approved = admin_api
        .approve_member(
            synctv_proto::admin::ApproveMemberRequest {
                room_id: room.id.as_str().to_string(),
                user_id: approve_target.id.as_str().to_string(),
            },
            &root_admin.id,
            &synctv_api::impls::admin::RequestContext::default(),
        )
        .await
        .unwrap()
        .member
        .expect("admin approve_member response member");
    assert_eq!(approved.user_id, approve_target.id.as_str());
    assert_eq!(
        approved.status,
        synctv_proto::common::MemberStatus::Active as i32
    );

    room_service
        .join_room(room.id.clone(), reject_target.id.clone(), None)
        .await
        .unwrap();
    let rejected = admin_api
        .reject_member(
            synctv_proto::admin::RejectMemberRequest {
                room_id: room.id.as_str().to_string(),
                user_id: reject_target.id.as_str().to_string(),
                reason: "policy violation".to_string(),
            },
            &root_admin.id,
            &synctv_api::impls::admin::RequestContext::default(),
        )
        .await
        .unwrap();
    assert!(rejected.success);

    let rejected_member = member_repo
        .get_any(&room.id, &reject_target.id)
        .await
        .unwrap()
        .expect("rejected member persisted");
    assert_eq!(rejected_member.status, MemberStatus::Rejected);
}
