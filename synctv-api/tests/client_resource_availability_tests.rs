#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use chrono::Utc;
use synctv_api::impls::ClientApiImpl;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        Media, MediaId, Playlist, PlaylistId, RoomId, SignupMethod, User, UserId, UserRole,
        UserStatus,
    },
    repository::{MediaRepository, PlaylistRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};
use synctv_core_testing::{create_test_pool, TestContainer};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

fn public_id_codec() -> synctv_core::PublicIdCodec {
    synctv_core::PublicIdCodec::plain()
}

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

fn make_playlist(room_id: &RoomId, creator_id: &UserId, name: &str, position: i32) -> Playlist {
    let now = Utc::now();
    Playlist {
        id: PlaylistId::new(),
        room_id: *room_id,
        creator_id: Some(*creator_id),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: f64::from(position),
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: now,
        updated_at: now,
        version: 0,
    }
}

fn make_media(room_id: &RoomId, creator_id: &UserId, name: &str, position: i32) -> Media {
    let now = Utc::now();
    Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: *room_id,
        creator_id: Some(*creator_id),
        name: name.to_string(),
        description: String::new(),
        position: f64::from(position),
        source_provider: synctv_core::models::SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(format!(
            "https://example.com/{name}.mp4"
        )),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: now,
        updated_at: now,
        version: 0,
    }
}

struct ClientApiFixture {
    postgres: TestContainer,
    pool: sqlx::PgPool,
    client_api: ClientApiImpl,
    user_repo: UserRepository,
    playlist_repo: PlaylistRepository,
    media_repo: MediaRepository,
    owner: User,
    creator: User,
    room: synctv_core::models::Room,
}

impl ClientApiFixture {
    async fn cleanup(self) {
        let Self {
            postgres,
            pool,
            client_api,
            user_repo,
            playlist_repo,
            media_repo,
            owner,
            creator,
            room,
        } = self;

        drop(client_api);
        drop(user_repo);
        drop(playlist_repo);
        drop(media_repo);
        drop(owner);
        drop(creator);
        drop(room);

        pool.close().await;
        postgres.cleanup().await;
    }
}

async fn create_client_api_fixture() -> ClientApiFixture {
    let (postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let room_service = RoomService::new_for_tests(pool.clone(), (*user_service).clone())
        .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("availability_owner"))
        .await
        .unwrap();
    let creator = user_repo
        .create(&make_user("availability_creator"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Availability Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let connection_service = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let mut runtime = support::client_api_runtime();
    runtime.presence_service = connection_service.presence_service();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service,
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
        runtime,
    );

    ClientApiFixture {
        postgres,
        pool,
        client_api,
        user_repo,
        playlist_repo,
        media_repo,
        owner,
        creator,
        room,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn list_playlist_items_root_includes_unavailable_resources_and_marks_availability() {
    let fixture = create_client_api_fixture().await;
    let ClientApiFixture {
        client_api,
        user_repo,
        playlist_repo,
        media_repo,
        owner,
        creator,
        room,
        ..
    } = &fixture;

    let available_playlist = playlist_repo
        .create(&make_playlist(&room.id, &owner.id, "available-folder", 1))
        .await
        .unwrap();
    let unavailable_playlist = playlist_repo
        .create(&make_playlist(
            &room.id,
            &creator.id,
            "unavailable-folder",
            2,
        ))
        .await
        .unwrap();
    let available_media = media_repo
        .create(&make_media(&room.id, &owner.id, "available-media", 1))
        .await
        .unwrap();
    let unavailable_media = media_repo
        .create(&make_media(&room.id, &creator.id, "unavailable-media", 2))
        .await
        .unwrap();
    user_repo.ban(&creator.id, None, None).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let available_playlist_id = codec.encode_playlist_id(available_playlist.id).unwrap();
    let unavailable_playlist_id = codec.encode_playlist_id(unavailable_playlist.id).unwrap();
    let available_media_id = codec.encode_media_id(available_media.id).unwrap();
    let unavailable_media_id = codec.encode_media_id(unavailable_media.id).unwrap();

    let response = client_api
        .list_playlist_items(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.total, 4);
    assert_eq!(response.folder_count, 2);
    assert_eq!(response.file_count, 2);
    assert_eq!(response.playlists.len(), 2);
    assert_eq!(response.media.len(), 2);

    let available_folder = response
        .playlists
        .iter()
        .find(|item| item.id == available_playlist_id)
        .unwrap();
    assert_eq!(
        available_folder.availability,
        synctv_proto::client::ResourceAvailability::Available as i32
    );

    let unavailable_folder = response
        .playlists
        .iter()
        .find(|item| item.id == unavailable_playlist_id)
        .unwrap();
    assert_eq!(
        unavailable_folder.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );

    let available_file = response
        .media
        .iter()
        .find(|item| item.id == available_media_id)
        .unwrap();
    assert_eq!(
        available_file.availability,
        synctv_proto::client::ResourceAvailability::Available as i32
    );

    let unavailable_file = response
        .media
        .iter()
        .find(|item| item.id == unavailable_media_id)
        .unwrap();
    assert_eq!(
        unavailable_file.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn list_playlist_items_root_availability_filter_updates_counts_and_pagination() {
    let fixture = create_client_api_fixture().await;
    let ClientApiFixture {
        client_api,
        user_repo,
        playlist_repo,
        media_repo,
        owner,
        creator,
        room,
        ..
    } = &fixture;

    playlist_repo
        .create(&make_playlist(&room.id, &owner.id, "available-folder", 1))
        .await
        .unwrap();
    let unavailable_playlist = playlist_repo
        .create(&make_playlist(
            &room.id,
            &creator.id,
            "unavailable-folder",
            0,
        ))
        .await
        .unwrap();
    media_repo
        .create(&make_media(&room.id, &owner.id, "available-media", 1))
        .await
        .unwrap();
    let unavailable_media = media_repo
        .create(&make_media(&room.id, &creator.id, "unavailable-media", 0))
        .await
        .unwrap();
    user_repo.ban(&creator.id, None, None).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let unavailable_playlist_id = codec.encode_playlist_id(unavailable_playlist.id).unwrap();
    let unavailable_media_id = codec.encode_media_id(unavailable_media.id).unwrap();

    let page_one = client_api
        .list_playlist_items(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 1,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32,
                refresh: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(page_one.total, 2);
    assert_eq!(page_one.folder_count, 1);
    assert_eq!(page_one.file_count, 1);
    assert_eq!(page_one.playlists.len(), 1);
    assert!(page_one.media.is_empty());
    assert_eq!(page_one.playlists[0].id, unavailable_playlist_id);

    let page_two = client_api
        .list_playlist_items(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 2,
                page_size: 1,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32,
                refresh: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(page_two.total, 2);
    assert_eq!(page_two.folder_count, 1);
    assert_eq!(page_two.file_count, 1);
    assert!(page_two.playlists.is_empty());
    assert_eq!(page_two.media.len(), 1);
    assert_eq!(page_two.media[0].id, unavailable_media_id);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn list_playlist_items_root_returns_stable_version_until_contents_change() {
    let fixture = create_client_api_fixture().await;
    let ClientApiFixture {
        client_api,
        media_repo,
        owner,
        room,
        ..
    } = &fixture;

    media_repo
        .create(&make_media(&room.id, &owner.id, "media-one", 1))
        .await
        .unwrap();

    let request = synctv_proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 20,
        search: String::new(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();

    let first = client_api
        .list_playlist_items(&owner.id, &room_public_id, request.clone())
        .await
        .unwrap();
    let second = client_api
        .list_playlist_items(&owner.id, &room_public_id, request.clone())
        .await
        .unwrap();

    assert!(!first.version.is_empty());
    assert_eq!(first.version, second.version);

    media_repo
        .create(&make_media(&room.id, &owner.id, "media-two", 2))
        .await
        .unwrap();

    let third = client_api
        .list_playlist_items(&owner.id, &room_public_id, request)
        .await
        .unwrap();

    assert_ne!(first.version, third.version);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn list_playlists_availability_filter_updates_total_and_response_items() {
    let fixture = create_client_api_fixture().await;
    let ClientApiFixture {
        client_api,
        user_repo,
        playlist_repo,
        owner,
        creator,
        room,
        ..
    } = &fixture;

    playlist_repo
        .create(&make_playlist(&room.id, &owner.id, "available-folder", 1))
        .await
        .unwrap();
    let unavailable_playlist = playlist_repo
        .create(&make_playlist(
            &room.id,
            &creator.id,
            "unavailable-folder",
            0,
        ))
        .await
        .unwrap();
    user_repo.ban(&creator.id, None, None).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let unavailable_playlist_id = codec.encode_playlist_id(unavailable_playlist.id).unwrap();

    let response = client_api
        .list_playlists(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistsRequest {
                parent_id: String::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                dynamic_only: None,
                sort_by: synctv_proto::client::PlaylistListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.playlists.len(), 1);
    assert_eq!(response.playlists[0].id, unavailable_playlist_id);
    assert_eq!(
        response.playlists[0].availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn public_room_discovery_marks_room_unavailable_when_creator_is_banned() {
    let fixture = create_client_api_fixture().await;
    let ClientApiFixture {
        client_api,
        user_repo,
        room,
        owner,
        ..
    } = &fixture;

    user_repo.ban(&owner.id, None, None).await.unwrap();
    let connection_id = format!("conn_{}", uuid::Uuid::new_v4().simple());
    client_api
        .connection_service
        .register(connection_id.clone(), owner.id)
        .await
        .unwrap();
    client_api
        .connection_service
        .join_room(&connection_id, room.id)
        .await
        .unwrap();

    let list_response = client_api
        .list_rooms(synctv_proto::client::ListRoomsRequest {
            page: 1,
            page_size: 20,
            search: String::new(),
            sort_by: synctv_proto::client::RoomListSortBy::CreatedAt as i32,
            sort_direction: synctv_proto::client::SortDirection::Desc as i32,
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .await
        .unwrap();

    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();
    let listed_room = list_response
        .rooms
        .iter()
        .find(|candidate| candidate.id == room_public_id)
        .expect("public list should still surface the room");
    assert_eq!(
        listed_room.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );

    let check_response = client_api
        .check_room(synctv_proto::client::CheckRoomRequest {
            room_id: room_public_id.clone(),
        })
        .await
        .unwrap();

    assert!(check_response.exists, "room should still exist");
    assert_eq!(
        check_response.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );
    assert!(
        check_response.name.is_empty(),
        "public check_room must not leak room name"
    );

    let hot_response = client_api
        .get_hot_rooms(synctv_proto::client::GetHotRoomsRequest { limit: 10 })
        .await
        .unwrap();

    let hot_room = hot_response
        .rooms
        .iter()
        .find_map(|entry| {
            entry
                .room
                .as_ref()
                .filter(|candidate| candidate.id == room_public_id)
        })
        .expect("hot rooms should still surface the room");
    assert_eq!(
        hot_room.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );

    client_api
        .connection_service
        .unregister(&connection_id)
        .await;

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn hot_rooms_considers_online_rooms_outside_newest_page() {
    let fixture = create_client_api_fixture().await;
    let ClientApiFixture {
        client_api,
        room,
        owner,
        ..
    } = &fixture;

    let connection_id = format!("conn_{}", uuid::Uuid::new_v4().simple());
    client_api
        .connection_service
        .register(connection_id.clone(), owner.id)
        .await
        .unwrap();
    client_api
        .connection_service
        .join_room(&connection_id, room.id)
        .await
        .unwrap();

    for index in 0..5 {
        client_api
            .room_service
            .create_room(
                format!("newer-empty-room-{index}"),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .unwrap();
    }

    let response = client_api
        .get_hot_rooms(synctv_proto::client::GetHotRoomsRequest { limit: 1 })
        .await
        .unwrap();

    let top_room = response
        .rooms
        .first()
        .and_then(|entry| entry.room.as_ref())
        .expect("hot rooms should return one room");
    assert_eq!(
        top_room.id,
        public_id_codec().encode_room_id(room.id).unwrap()
    );
    assert_eq!(response.rooms[0].online_count, 1);

    client_api
        .connection_service
        .unregister(&connection_id)
        .await;
    fixture.cleanup().await;
}
