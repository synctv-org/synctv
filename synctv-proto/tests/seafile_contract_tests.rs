use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn seafile_services_are_registered_in_descriptor_pools() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.seafile.SeafileProviderService")
        .expect("Seafile provider service descriptor");
    assert_eq!(provider.methods().count(), 7);
    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.seafile.SeafilePlaybackProviderService")
        .expect("Seafile playback provider service descriptor");
    assert_eq!(playback.methods().count(), 4);
}

#[test]
fn seafile_contract_preserves_hash_ids_and_source_variants() {
    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Seafile(
            synctv_proto::source_config::SeafileMediaSourceConfig {
                server_id: "seafile-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                repository_id: "0f70c3e8-3f73-4f07-a489-65d4c33e9130".to_string(),
                path: "/Videos/movie.mkv".to_string(),
                object_id: "0123456789abcdef0123456789abcdef01234567".to_string(),
                has_thumbnail: true,
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Seafile(
            synctv_proto::source_config::SeafilePlaylistSourceConfig {
                server_id: "seafile-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(
                    synctv_proto::source_config::seafile_playlist_source_config::Source::Search(
                        synctv_proto::source_config::SeafileSearchPlaylistSourceConfig {
                            repository_id: "0f70c3e8-3f73-4f07-a489-65d4c33e9130".to_string(),
                            query: "movie".to_string(),
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Seafile(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Seafile(_))
    ));

    let item = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.seafile.FileItem")
        .expect("Seafile file item descriptor");
    assert_eq!(
        item.get_field_by_name("object_id")
            .expect("object_id")
            .kind(),
        Kind::String
    );
    assert_eq!(
        item.get_field_by_name("size").expect("size").kind(),
        Kind::Uint64
    );

    let request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.seafile.ListRequest")
        .expect("Seafile list request descriptor");
    assert_eq!(
        request.get_field_by_name("page").expect("page").kind(),
        Kind::Uint64
    );
    assert_eq!(
        request
            .get_field_by_name("page_size")
            .expect("page_size")
            .kind(),
        Kind::Uint32
    );

    let playback = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.PlaybackMetadata")
        .expect("playback metadata descriptor");
    assert_eq!(
        playback
            .get_field_by_name("seafile")
            .expect("seafile playback metadata")
            .kind(),
        Kind::Message(
            synctv_proto::DESCRIPTOR_POOL
                .get_message_by_name("synctv.client.SeafilePlaybackMetadata")
                .expect("Seafile playback metadata descriptor")
        )
    );
}
