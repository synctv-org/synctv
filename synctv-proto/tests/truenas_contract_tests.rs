use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn truenas_services_and_source_variants_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.truenas.TrueNasProviderService")
        .expect("TrueNAS provider service descriptor");
    assert_eq!(provider.methods().count(), 4);
    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.truenas.TrueNasPlaybackProviderService")
        .expect("TrueNAS playback provider service descriptor");
    assert_eq!(playback.methods().count(), 4);

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Truenas(
            synctv_proto::source_config::TrueNasMediaSourceConfig {
                server_id: "nas-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: "/mnt/tank/Movie.mkv".to_string(),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Truenas(
            synctv_proto::source_config::TrueNasPlaylistSourceConfig {
                server_id: "nas-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(
                    synctv_proto::source_config::true_nas_playlist_source_config::Source::Search(
                        synctv_proto::source_config::TrueNasSearchPlaylistSourceConfig {
                            path: "/mnt/tank".to_string(),
                            query: "movie".to_string(),
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Truenas(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Truenas(_))
    ));

    let request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.truenas.ListRequest")
        .expect("TrueNAS list request descriptor");
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

    let item = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.truenas.FileItem")
        .expect("TrueNAS file item descriptor");
    assert_eq!(
        item.get_field_by_name("allocation_size")
            .expect("allocation_size")
            .kind(),
        Kind::Uint64
    );
    assert_eq!(
        item.get_field_by_name("mount_id").expect("mount_id").kind(),
        Kind::Uint64
    );

    let playback = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.PlaybackMetadata")
        .expect("playback metadata descriptor");
    assert_eq!(
        playback
            .get_field_by_name("truenas")
            .expect("TrueNAS playback metadata")
            .kind(),
        Kind::Message(
            synctv_proto::DESCRIPTOR_POOL
                .get_message_by_name("synctv.client.TrueNasPlaybackMetadata")
                .expect("TrueNAS playback metadata descriptor")
        )
    );
}
