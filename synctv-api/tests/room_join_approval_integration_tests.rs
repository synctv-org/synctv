#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room_settings::RequireApproval, RoomSettings, SignupMethod, User, UserId, UserRole,
        UserStatus,
    },
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};
use synctv_proto::common::MemberStatus;

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
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
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> synctv_api::impls::ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    synctv_api::impls::ClientApiImpl::new(
        user_service,
        room_service,
        connection_manager,
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
        Arc::new(synctv_api::PublicIdCodec::default_for_tests()),
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_response_exposes_pending_membership_contract() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let client_api = make_client_api(user_service, room_service.clone());

    let owner = user_repo
        .create(&make_user("approval_contract_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("approval_contract_joiner"))
        .await
        .unwrap();

    let settings = RoomSettings {
        require_approval: RequireApproval(true),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Approval Contract Room".to_string(),
            String::new(),
            owner.id,
            None,
            Some(settings),
        )
        .await
        .unwrap();

    let public_id_codec = synctv_api::PublicIdCodec::default_for_tests();
    let room_id = public_id_codec.encode_room_id(room.id).unwrap();
    let response = client_api
        .join_room(
            &joiner.id,
            &room_id,
            synctv_proto::client::JoinRoomRequest {
                room_id: room_id.clone(),
                password: String::new(),
            },
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        response.membership_status,
        MemberStatus::Active as i32,
        "join response returns the synthetic requester member separately from review status"
    );
    assert!(
        response.requires_approval,
        "join response must explicitly tell the client that approval is required"
    );
    assert!(
        response.members.is_empty(),
        "pending join should not leak the room member list before approval"
    );
}
