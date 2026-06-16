#![allow(clippy::unwrap_used)]

mod support;

use chrono::Utc;
use std::sync::Arc;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};
use synctv_proto::client::{ListMyRoomsRequest, MyRoomRelation};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
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

fn make_client_api(
    user_service: Arc<UserService>,
    room_service: Arc<RoomService>,
) -> synctv_api::impls::ClientApiImpl {
    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));

    synctv_api::impls::ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: connection_manager,
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_my_rooms_relation_filter_and_response_relation_are_consistent() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));
    let room_service = Arc::new(
        RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build"),
    );
    let client_api = make_client_api(user_service, room_service.clone());

    let actor = user_repo
        .create(&make_user("my_rooms_actor"))
        .await
        .unwrap();
    let external_owner = user_repo
        .create(&make_user("my_rooms_external_owner"))
        .await
        .unwrap();

    let (created_room, _) = room_service
        .create_room(
            "Actor Created Room".to_string(),
            String::new(),
            actor.id,
            None,
            None,
        )
        .await
        .unwrap();

    let (participating_room, _) = room_service
        .create_room(
            "External Created Room".to_string(),
            String::new(),
            external_owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(participating_room.id, actor.id, None)
        .await
        .unwrap();

    let created_only = client_api
        .list_my_rooms(
            &actor.id,
            ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                status: 0,
                is_banned: None,
                relation: MyRoomRelation::Created as i32,
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap();

    assert_eq!(created_only.total, 1);
    assert_eq!(created_only.rooms.len(), 1);
    assert_eq!(
        created_only.rooms[0].room.as_ref().unwrap().id,
        synctv_core::PublicIdCodec::plain()
            .encode_room_id(created_room.id)
            .unwrap()
    );
    assert_eq!(
        created_only.rooms[0].relation,
        MyRoomRelation::Created as i32
    );

    let participating_only = client_api
        .list_my_rooms(
            &actor.id,
            ListMyRoomsRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                status: 0,
                is_banned: None,
                relation: MyRoomRelation::Participating as i32,
                sort_by: 0,
                sort_direction: 0,
            },
        )
        .await
        .unwrap();

    assert_eq!(participating_only.total, 1);
    assert_eq!(participating_only.rooms.len(), 1);
    assert_eq!(
        participating_only.rooms[0].room.as_ref().unwrap().id,
        synctv_core::PublicIdCodec::plain()
            .encode_room_id(participating_room.id)
            .unwrap()
    );
    assert_eq!(
        participating_only.rooms[0].relation,
        MyRoomRelation::Participating as i32
    );
}
