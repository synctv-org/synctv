use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn fnos_provider_services_are_in_descriptor_pools() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.fnos.FnosProviderService")
        .expect("FNOS provider service descriptor");
    assert_eq!(provider.methods().count(), 9);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.fnos.FnosPlaybackProviderService")
        .expect("FNOS playback provider service descriptor");
    assert_eq!(playback.methods().count(), 4);
}

#[test]
fn fnos_source_config_uses_provider_specific_messages() {
    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Fnos(
            synctv_proto::source_config::FnosMediaSourceConfig {
                server_id: "server".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(
                    synctv_proto::source_config::fnos_media_source_config::Source::File(
                        synctv_proto::source_config::FnosFileSourceConfig {
                            path: "Videos/movie.mkv".to_string(),
                        },
                    ),
                ),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Fnos(
            synctv_proto::source_config::FnosPlaylistSourceConfig {
                server_id: "server".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(
                    synctv_proto::source_config::fnos_playlist_source_config::Source::Files(
                        synctv_proto::source_config::FnosFilesPlaylistSourceConfig {
                            path: "Videos".to_string(),
                        },
                    ),
                ),
            },
        )),
    };

    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Fnos(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Fnos(_))
    ));

    let playlist_source = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.source_config.FnosPlaylistSourceConfig")
        .expect("FNOS playlist source descriptor");
    assert_eq!(
        playlist_source
            .get_field_by_name("favorites")
            .expect("favorites")
            .number(),
        4
    );
    assert_eq!(
        playlist_source
            .get_field_by_name("history")
            .expect("history")
            .number(),
        5
    );

    let list = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.fnos.ListResponse")
        .expect("FNOS list response descriptor");
    assert_eq!(
        list.get_field_by_name("total").expect("total").kind(),
        Kind::Uint64
    );

    let media_request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.fnos.ListMediaItemsRequest")
        .expect("FNOS media list request descriptor");
    assert_eq!(
        media_request
            .get_field_by_name("collection")
            .expect("collection")
            .number(),
        2
    );
    assert_eq!(
        media_request
            .get_field_by_name("search")
            .expect("search")
            .number(),
        7
    );
    assert_eq!(
        media_request
            .get_field_by_name("instance_name")
            .expect("instance_name")
            .number(),
        8
    );

    let media_item = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.fnos.MediaItem")
        .expect("FNOS media item descriptor");
    assert_eq!(
        media_item
            .get_field_by_name("favorite")
            .expect("favorite")
            .number(),
        15
    );
}
