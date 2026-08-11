use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn nextcloud_services_are_registered_in_descriptor_pools() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.nextcloud.NextcloudProviderService")
        .expect("Nextcloud provider service descriptor");
    assert_eq!(provider.methods().count(), 7);
    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.nextcloud.NextcloudPlaybackProviderService")
        .expect("Nextcloud playback provider service descriptor");
    assert_eq!(playback.methods().count(), 4);
}

#[test]
fn nextcloud_contract_preserves_native_ids_and_source_variants() {
    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Nextcloud(
            synctv_proto::source_config::NextcloudMediaSourceConfig {
                server_id: "cloud-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: "/Videos/movie.mkv".to_string(),
                file_id: 9_007_199_254_740_991,
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Nextcloud(
            synctv_proto::source_config::NextcloudPlaylistSourceConfig {
                server_id: "cloud-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(
                    synctv_proto::source_config::nextcloud_playlist_source_config::Source::Search(
                        synctv_proto::source_config::NextcloudSearchPlaylistSourceConfig {
                            path: "/Videos".to_string(),
                            query: "movie".to_string(),
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Nextcloud(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Nextcloud(_))
    ));

    let item = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.nextcloud.FileItem")
        .expect("Nextcloud file item descriptor");
    assert_eq!(
        item.get_field_by_name("file_id").expect("file_id").kind(),
        Kind::Uint64
    );
    assert_eq!(
        item.get_field_by_name("size").expect("size").kind(),
        Kind::Uint64
    );
    assert_eq!(
        item.get_field_by_name("duration_millis")
            .expect("duration_millis")
            .kind(),
        Kind::Uint64
    );

    let request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.nextcloud.ListRequest")
        .expect("Nextcloud list request descriptor");
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

    let response = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.nextcloud.ListResponse")
        .expect("Nextcloud list response descriptor");
    assert!(response
        .get_field_by_name("total")
        .expect("total")
        .supports_presence());

    let playback = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.PlaybackMetadata")
        .expect("playback metadata descriptor");
    assert_eq!(
        playback
            .get_field_by_name("nextcloud")
            .expect("nextcloud playback metadata")
            .kind(),
        Kind::Message(
            synctv_proto::DESCRIPTOR_POOL
                .get_message_by_name("synctv.client.NextcloudPlaybackMetadata")
                .expect("Nextcloud playback metadata descriptor")
        )
    );
}
