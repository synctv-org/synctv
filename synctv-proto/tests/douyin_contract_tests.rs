use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn douyin_services_and_source_variants_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.douyin.DouyinProviderService")
        .expect("Douyin provider service descriptor");
    assert_eq!(provider.methods().count(), 5);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.douyin.DouyinPlaybackProviderService")
        .expect("Douyin playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);
    assert!(playback
        .methods()
        .any(|method| method.name() == "GetHlsResource"));

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Douyin(
            synctv_proto::source_config::DouyinMediaSourceConfig {
                source: Some(
                    synctv_proto::source_config::douyin_media_source_config::Source::Live(
                        synctv_proto::source_config::DouyinLiveSourceConfig {
                            web_rid: "123456".to_string(),
                            shared: true,
                        },
                    ),
                ),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Douyin(
            synctv_proto::source_config::DouyinPlaylistSourceConfig {
                sec_uid: "MS4wLjABAAAAexample".to_string(),
                shared: true,
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Douyin(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Douyin(_))
    ));

    let media_config = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.source_config.MediaSourceConfig")
        .expect("media source config descriptor");
    assert_eq!(
        media_config
            .get_field_by_name("douyin")
            .expect("douyin media source field")
            .number(),
        11
    );
    let playlist_config = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.source_config.PlaylistSourceConfig")
        .expect("playlist source config descriptor");
    assert_eq!(
        playlist_config
            .get_field_by_name("douyin")
            .expect("douyin playlist source field")
            .number(),
        11
    );

    let resource_request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_message_by_name("synctv.playback_provider.douyin.GetDouyinResourceRequest")
        .expect("Douyin resource request descriptor");
    assert_eq!(
        resource_request
            .get_field_by_name("media_index")
            .expect("media_index")
            .kind(),
        Kind::Uint32
    );
    let hls_request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_message_by_name("synctv.playback_provider.douyin.GetDouyinHlsResourceRequest")
        .expect("Douyin HLS resource request descriptor");
    assert_eq!(
        hls_request
            .get_field_by_name("media_index")
            .expect("HLS media_index")
            .kind(),
        Kind::Uint32
    );
    assert!(matches!(
        hls_request
            .get_field_by_name("resource_kind")
            .expect("HLS resource_kind")
            .kind(),
        Kind::Enum(_)
    ));
}
