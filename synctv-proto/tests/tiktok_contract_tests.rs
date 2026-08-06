use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn tiktok_services_and_wire_contract_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.tiktok.TikTokProviderService")
        .expect("TikTok provider service descriptor");
    assert_eq!(provider.methods().count(), 6);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.tiktok.TikTokPlaybackProviderService")
        .expect("TikTok playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);
    assert!(playback
        .methods()
        .any(|method| method.name() == "GetHlsResource"));

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Tiktok(
            synctv_proto::source_config::TikTokMediaSourceConfig {
                source: Some(
                    synctv_proto::source_config::tik_tok_media_source_config::Source::Live(
                        synctv_proto::source_config::TikTokLiveSourceConfig {
                            unique_id: "creator".to_string(),
                            shared: true,
                        },
                    ),
                ),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Tiktok(
            synctv_proto::source_config::TikTokPlaylistSourceConfig {
                sec_uid: "MS4wLjABAAAAexample".to_string(),
                shared: true,
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Tiktok(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Tiktok(_))
    ));

    for message_name in [
        "synctv.playback_provider.tiktok.GetTikTokResourceRequest",
        "synctv.playback_provider.tiktok.GetTikTokHlsResourceRequest",
        "synctv.playback_provider.tiktok.GetTikTokSubtitleRequest",
    ] {
        let request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("TikTok indexed request descriptor");
        let index_name = if message_name.contains("Subtitle") {
            "subtitle_index"
        } else {
            "media_index"
        };
        assert_eq!(
            request
                .get_field_by_name(index_name)
                .expect("TikTok request index")
                .kind(),
            Kind::Uint32
        );
        if message_name.contains("HlsResource") {
            assert!(matches!(
                request
                    .get_field_by_name("resource_kind")
                    .expect("TikTok HLS resource kind")
                    .kind(),
                Kind::Enum(_)
            ));
        }
    }

    for (message_name, field_name, expected_number) in [
        ("synctv.source_config.MediaSourceConfig", "tiktok", 21),
        ("synctv.source_config.PlaylistSourceConfig", "tiktok", 21),
        ("synctv.client.ProviderTarget", "tiktok", 13),
        ("synctv.client.PlaybackMetadata", "tiktok", 10),
    ] {
        let message = synctv_proto::DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("TikTok containing message descriptor");
        assert_eq!(
            message
                .get_field_by_name(field_name)
                .expect("TikTok oneof field")
                .number(),
            expected_number
        );
    }
}
