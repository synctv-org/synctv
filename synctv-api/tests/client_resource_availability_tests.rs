#![allow(clippy::unwrap_used)]

mod support;

use synctv_api::ApiRuntimeSettings as Config;

use std::sync::Arc;

use chrono::Utc;
use synctv_api::ClientApiImpl;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room_settings::AllowGuestJoin, Media, MediaId, Playlist, PlaylistId, RoomId, SignupMethod,
        User, UserId, UserRole, UserStatus,
    },
    repository::{MediaRepository, PlaylistRepository, SettingsRepository, UserRepository},
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService,
        RoomServiceOptions, RuntimeSettingsStore, SettingsService, UserService,
    },
};
use synctv_core_testing::{create_test_pool, create_test_pool_with_db_and_label, TestContainer};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

fn public_id_codec() -> synctv_api::PublicIdCodec {
    synctv_api::PublicIdCodec::plain()
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
        browse_access_mode: synctv_core::models::PlaylistBrowseAccessMode::Default,
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
        thumbnail_file_reference_id: None,
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
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    settings_service.initialize().await.unwrap();
    let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service));
    let room_service = RoomService::new_with_options(
        pool.clone(),
        (*user_service).clone(),
        RoomServiceOptions {
            runtime_settings_store: Some(runtime_settings_store.clone()),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service,
            connection_service,
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: Some(runtime_settings_store),
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
async fn single_room_discovery_uses_primary_while_feed_uses_read_pool() {
    let (primary_postgres, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "discovery-primary").await;
    let (read_postgres, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "discovery-read").await;
    let user_repo = UserRepository::new(primary_pool.clone());
    let user_service = Arc::new(make_user_service(&primary_pool));
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(primary_pool.clone()),
        primary_pool.clone(),
    ));
    settings_service.initialize().await.unwrap();
    let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service));
    let room_service = Arc::new(
        RoomService::new_with_options(
            primary_pool.clone(),
            (*user_service).clone(),
            RoomServiceOptions {
                read_pool: Some(read_pool.clone()),
                runtime_settings_store: Some(runtime_settings_store.clone()),
                ..RoomServiceOptions::test_defaults_with_settings(primary_pool.clone())
            },
        )
        .expect("room service should build"),
    );
    let owner = user_repo
        .create(&make_user("primary_discovery_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Primary Discovery Room".to_string(),
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
        synctv_api::ClientApiOptions {
            read_pool: Some(read_pool.clone()),
            user_service,
            room_service,
            connection_service,
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: Some(runtime_settings_store),
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        runtime,
    );
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();

    let feed = client_api
        .discover_public_rooms(synctv_proto::client::DiscoverRoomsRequest {
            page: 1,
            page_size: 20,
            search: String::new(),
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .await
        .unwrap();
    assert!(feed.featured_rooms.is_empty());
    assert!(feed.rooms.is_empty());

    let public_item = client_api
        .get_public_room_discovery(synctv_proto::client::GetRoomDiscoveryRequest {
            room_id: room_public_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(public_item.room.as_ref().unwrap().name, room.name);
    assert_eq!(public_item.room.as_ref().unwrap().member_count, 1);

    let user_item = client_api
        .get_room_discovery(
            &owner.id,
            synctv_proto::client::GetRoomDiscoveryRequest {
                room_id: room_public_id,
            },
        )
        .await
        .unwrap();
    assert!(user_item.joined);
    assert_eq!(
        user_item.access,
        synctv_proto::client::RoomDiscoveryAccess::Enter as i32
    );

    drop(client_api);
    drop(user_repo);
    primary_pool.close().await;
    read_pool.close().await;
    primary_postgres.cleanup().await;
    read_postgres.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn public_room_discovery_exposes_direct_guest_entry_and_token_flow() {
    let fixture = Box::pin(create_client_api_fixture()).await;
    let ClientApiFixture {
        client_api,
        room,
        owner,
        ..
    } = &fixture;

    let mut settings = client_api
        .room_service
        .get_room_settings(&room.id)
        .await
        .unwrap();
    settings.allow_guest_join = AllowGuestJoin(true);
    client_api
        .room_service
        .set_settings(room.id, owner.id, settings)
        .await
        .unwrap();

    let response = client_api
        .discover_public_rooms(synctv_proto::client::DiscoverRoomsRequest {
            page: 1,
            page_size: 20,
            search: room.name.clone(),
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .await
        .unwrap();
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();
    let item = response
        .rooms
        .iter()
        .find(|item| {
            item.room
                .as_ref()
                .is_some_and(|listed| listed.id == room_public_id)
        })
        .expect("guest-enabled room should be discoverable");
    assert!(item.can_join);
    assert!(!item.joined);
    assert!(!item.favorited);
    assert_eq!(
        item.access,
        synctv_proto::client::RoomDiscoveryAccess::Guest as i32
    );

    let token = client_api
        .create_guest_token_with_control(
            synctv_proto::client::CreateGuestTokenRequest {
                room_id: room_public_id,
            },
            None,
        )
        .await
        .unwrap();
    assert!(!token.token.is_empty());

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn old_guest_token_is_rejected_after_room_creator_becomes_inactive() {
    let fixture = Box::pin(create_client_api_fixture()).await;
    let ClientApiFixture {
        client_api,
        user_repo,
        room,
        owner,
        ..
    } = &fixture;

    let mut settings = client_api
        .room_service
        .get_room_settings(&room.id)
        .await
        .unwrap();
    settings.allow_guest_join = AllowGuestJoin(true);
    client_api
        .room_service
        .set_settings(room.id, owner.id, settings)
        .await
        .unwrap();

    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();
    let token = client_api
        .create_guest_token_with_control(
            synctv_proto::client::CreateGuestTokenRequest {
                room_id: room_public_id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    client_api
        .validate_guest_room_access(&token.token, &room_public_id)
        .await
        .unwrap();

    user_repo.ban(&owner.id, None, None).await.unwrap();

    let old_token_error = client_api
        .validate_guest_room_access(&token.token, &room_public_id)
        .await
        .unwrap_err();
    assert!(
        matches!(old_token_error, synctv_api::ApiError::Authorization(_)),
        "old guest tokens must stop authorizing access when the room creator is inactive"
    );

    let new_token_error = client_api
        .create_guest_token_with_control(
            synctv_proto::client::CreateGuestTokenRequest {
                room_id: room_public_id,
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(new_token_error, synctv_api::ApiError::Authorization(_)),
        "inactive room creators must prevent new guest token issuance"
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn list_playlist_items_root_includes_unavailable_resources_and_marks_availability() {
    let fixture = Box::pin(create_client_api_fixture()).await;
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
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 20,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.total, Some(4));
    assert_eq!(response.playlist_count, 2);
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
    let fixture = Box::pin(create_client_api_fixture()).await;
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
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 1,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32,
                refresh: false,
                preview_source_config: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(page_one.total, Some(2));
    assert_eq!(page_one.playlist_count, 1);
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
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 2 },
                    ),
                ),
                page_size: 1,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::Unavailable as i32,
                refresh: false,
                preview_source_config: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(page_two.total, Some(2));
    assert_eq!(page_two.playlist_count, 1);
    assert_eq!(page_two.file_count, 1);
    assert!(page_two.playlists.is_empty());
    assert_eq!(page_two.media.len(), 1);
    assert_eq!(page_two.media[0].id, unavailable_media_id);

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn list_playlist_items_root_returns_stable_version_until_contents_change() {
    let fixture = Box::pin(create_client_api_fixture()).await;
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
        target: None,
        pagination: Some(
            synctv_proto::client::list_playlist_items_request::Pagination::Page(
                synctv_proto::client::PagePagination { page: 1 },
            ),
        ),
        page_size: 20,
        search: String::new(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
        preview_source_config: None,
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
    let fixture = Box::pin(create_client_api_fixture()).await;
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
    let fixture = Box::pin(create_client_api_fixture()).await;
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
        .discover_public_rooms(synctv_proto::client::DiscoverRoomsRequest {
            page: 1,
            page_size: 20,
            search: String::new(),
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .await
        .unwrap();

    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();
    let listed_room = list_response
        .featured_rooms
        .iter()
        .chain(list_response.rooms.iter())
        .find(|item| {
            item.room
                .as_ref()
                .is_some_and(|room| room.id == room_public_id)
        })
        .expect("public list should still surface the room");
    let listed_room = listed_room.room.as_ref().unwrap();
    assert_eq!(
        listed_room.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );

    let discovery_item = client_api
        .get_public_room_discovery(synctv_proto::client::GetRoomDiscoveryRequest {
            room_id: room_public_id.clone(),
        })
        .await
        .unwrap();

    let discovered_room = discovery_item.room.as_ref().unwrap();
    assert_eq!(
        discovered_room.availability,
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    );
    assert_eq!(discovered_room.name, room.name);

    let authenticated = client_api
        .discover_rooms(
            &owner.id,
            synctv_proto::client::DiscoverRoomsRequest {
                page: 1,
                page_size: 20,
                search: room.name.clone(),
                category_id: String::new(),
                label_ids: Vec::new(),
            },
        )
        .await
        .unwrap();
    let joined_item = authenticated
        .rooms
        .iter()
        .find(|item| {
            item.room
                .as_ref()
                .is_some_and(|listed| listed.id == room_public_id)
        })
        .expect("joined room should remain visible to its member");
    assert!(joined_item.joined);
    assert!(!joined_item.can_join);
    assert_eq!(
        joined_item.access,
        synctv_proto::client::RoomDiscoveryAccess::Unavailable as i32
    );

    client_api
        .connection_service
        .unregister(&connection_id)
        .await;

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_discovery_prioritizes_online_rooms_outside_newest_page() {
    let fixture = Box::pin(create_client_api_fixture()).await;
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
        .discover_public_rooms(synctv_proto::client::DiscoverRoomsRequest {
            page: 1,
            page_size: 1,
            search: String::new(),
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .await
        .unwrap();

    let top_room = response
        .featured_rooms
        .first()
        .expect("room discovery should return one featured room");
    let top_room = top_room.room.as_ref().unwrap();
    assert_eq!(
        top_room.id,
        public_id_codec().encode_room_id(room.id).unwrap()
    );
    assert_eq!(
        top_room
            .presence
            .as_ref()
            .map(|value| value.online_member_count),
        Some(1)
    );
    assert_eq!(
        top_room
            .presence
            .as_ref()
            .map(|value| value.online_guest_count),
        Some(0)
    );

    let first_popular_room = response
        .rooms
        .first()
        .expect("room discovery should return one non-featured room");
    let first_popular_room = first_popular_room.room.as_ref().unwrap();
    assert_ne!(first_popular_room.id, top_room.id);

    client_api
        .connection_service
        .unregister(&connection_id)
        .await;
    fixture.cleanup().await;
}
