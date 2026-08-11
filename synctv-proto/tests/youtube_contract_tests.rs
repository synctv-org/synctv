use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn youtube_services_and_source_variants_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.youtube.YoutubeProviderService")
        .expect("YouTube provider service descriptor");
    assert_eq!(provider.methods().count(), 5);
    assert!(provider.methods().any(|method| method.name() == "Resolve"));
    assert!(provider.methods().any(|method| method.name() == "List"));
    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.youtube.YoutubePlaybackProviderService")
        .expect("YouTube playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Youtube(
            synctv_proto::source_config::YoutubeMediaSourceConfig {
                video_id: "dQw4w9WgXcQ".to_string(),
                shared: true,
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Youtube(
            synctv_proto::source_config::YoutubePlaylistSourceConfig {
                shared: true,
                source: Some(
                    synctv_proto::source_config::youtube_playlist_source_config::Source::Search(
                        synctv_proto::source_config::youtube_playlist_source_config::Search {
                            query: "SyncTV".to_string(),
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Youtube(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Youtube(_))
    ));

    let request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_message_by_name("synctv.playback_provider.youtube.GetYoutubeResourceRequest")
        .expect("YouTube resource request descriptor");
    assert_eq!(
        request
            .get_field_by_name("media_index")
            .expect("media_index")
            .kind(),
        Kind::Uint32
    );

    let bind_request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.youtube.BindRequest")
        .expect("YouTube bind request descriptor");
    assert_eq!(
        bind_request
            .get_field_by_name("cookie")
            .expect("cookie")
            .number(),
        4
    );
    assert_eq!(
        bind_request
            .get_field_by_name("instance_name")
            .expect("instance_name")
            .number(),
        5
    );
    let bind_info = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.youtube.BindInfo")
        .expect("YouTube bind info descriptor");
    assert_eq!(
        bind_info
            .get_field_by_name("has_cookie")
            .expect("has_cookie")
            .number(),
        6
    );
    assert_eq!(
        bind_info
            .get_field_by_name("provider_instance_name")
            .expect("provider_instance_name")
            .number(),
        8
    );
}
