#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use synctv_api::impls::ClientApiImpl;
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        Media, MediaId, Playlist, PlaylistId, RoomId, SignupMethod, User, UserId, UserRole,
        UserStatus,
    },
    repository::{MediaRepository, PlaylistRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};
use synctv_core_testing::{create_test_pool, TestContainer};

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
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
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let mut svc = UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn make_playlist(room_id: &RoomId, creator_id: &UserId, name: &str, position: i32) -> Playlist {
    let now = Utc::now();
    Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: Some(creator_id.clone()),
        name: name.to_string(),
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
        room_id: room_id.clone(),
        creator_id: Some(creator_id.clone()),
        name: name.to_string(),
        position: f64::from(position),
        source_provider: "direct_url".to_string(),
        source_config: json!({ "url": format!("https://example.com/{name}.mp4") }),
        provider_instance_name: Some("direct_url".to_string()),
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
    let user_service = Arc::new(make_user_service(pool.clone()));

    let mut room_service = RoomService::new(pool.clone(), (*user_service).clone());
    room_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
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
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let client_api = ClientApiImpl::new(
        user_service,
        room_service,
        Arc::new(ConnectionManager::new(ConnectionLimits::default())),
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
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
    user_repo
        .update_status(&creator.id, UserStatus::Banned)
        .await
        .unwrap();

    let response = client_api
        .list_playlist_items(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::All as i32,
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
        .find(|item| item.id == available_playlist.id.as_str())
        .unwrap();
    assert_eq!(
        available_folder.availability,
        synctv_api::proto::client::ResourceAvailability::Available as i32
    );

    let unavailable_folder = response
        .playlists
        .iter()
        .find(|item| item.id == unavailable_playlist.id.as_str())
        .unwrap();
    assert_eq!(
        unavailable_folder.availability,
        synctv_api::proto::client::ResourceAvailability::CreatorInactive as i32
    );

    let available_file = response
        .media
        .iter()
        .find(|item| item.id == available_media.id.as_str())
        .unwrap();
    assert_eq!(
        available_file.availability,
        synctv_api::proto::client::ResourceAvailability::Available as i32
    );

    let unavailable_file = response
        .media
        .iter()
        .find(|item| item.id == unavailable_media.id.as_str())
        .unwrap();
    assert_eq!(
        unavailable_file.availability,
        synctv_api::proto::client::ResourceAvailability::CreatorInactive as i32
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
    user_repo
        .update_status(&creator.id, UserStatus::Banned)
        .await
        .unwrap();

    let page_one = client_api
        .list_playlist_items(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 1,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::Unavailable
                    as i32,
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
    assert_eq!(page_one.playlists[0].id, unavailable_playlist.id.as_str());

    let page_two = client_api
        .list_playlist_items(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 2,
                page_size: 1,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::Unavailable
                    as i32,
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
    assert_eq!(page_two.media[0].id, unavailable_media.id.as_str());

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

    let request = synctv_api::proto::client::ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 20,
        search: String::new(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
        availability: synctv_api::proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    };

    let first = client_api
        .list_playlist_items(owner.id.as_str(), room.id.as_str(), request.clone())
        .await
        .unwrap();
    let second = client_api
        .list_playlist_items(owner.id.as_str(), room.id.as_str(), request.clone())
        .await
        .unwrap();

    assert!(!first.version.is_empty());
    assert_eq!(first.version, second.version);

    media_repo
        .create(&make_media(&room.id, &owner.id, "media-two", 2))
        .await
        .unwrap();

    let third = client_api
        .list_playlist_items(owner.id.as_str(), room.id.as_str(), request)
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
    user_repo
        .update_status(&creator.id, UserStatus::Banned)
        .await
        .unwrap();

    let response = client_api
        .list_playlists(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistsRequest {
                parent_id: String::new(),
                page: 1,
                page_size: 20,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                dynamic_only: None,
                sort_by: synctv_api::proto::client::PlaylistListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::Unavailable
                    as i32,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.playlists.len(), 1);
    assert_eq!(response.playlists[0].id, unavailable_playlist.id.as_str());
    assert_eq!(
        response.playlists[0].availability,
        synctv_api::proto::client::ResourceAvailability::CreatorInactive as i32
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

    user_repo
        .update_status(&owner.id, UserStatus::Banned)
        .await
        .unwrap();

    let list_response = client_api
        .list_rooms(synctv_api::proto::client::ListRoomsRequest {
            page: 1,
            page_size: 20,
            search: String::new(),
            sort_by: synctv_api::proto::client::RoomListSortBy::CreatedAt as i32,
            sort_direction: synctv_api::proto::client::SortDirection::Desc as i32,
        })
        .await
        .unwrap();

    let listed_room = list_response
        .rooms
        .iter()
        .find(|candidate| candidate.id == room.id.as_str())
        .expect("public list should still surface the room");
    assert_eq!(
        listed_room.availability,
        synctv_api::proto::client::ResourceAvailability::CreatorInactive as i32
    );

    let check_response = client_api
        .check_room(synctv_api::proto::client::CheckRoomRequest {
            room_id: room.id.as_str().to_string(),
        })
        .await
        .unwrap();

    assert!(check_response.exists, "room should still exist");
    assert_eq!(
        check_response.availability,
        synctv_api::proto::client::ResourceAvailability::CreatorInactive as i32
    );
    assert!(
        check_response.name.is_empty(),
        "public check_room must not leak room name"
    );

    let hot_response = client_api
        .get_hot_rooms(synctv_api::proto::client::GetHotRoomsRequest { limit: 10 })
        .await
        .unwrap();

    let hot_room = hot_response
        .rooms
        .iter()
        .find_map(|entry| {
            entry
                .room
                .as_ref()
                .filter(|candidate| candidate.id == room.id.as_str())
        })
        .expect("hot rooms should still surface the room");
    assert_eq!(
        hot_room.availability,
        synctv_api::proto::client::ResourceAvailability::CreatorInactive as i32
    );

    fixture.cleanup().await;
}
