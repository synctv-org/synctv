use prost_reflect::Kind;
use synctv_proto::source_config::media_source_config;

#[test]
fn huya_services_and_wire_contract_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.huya.HuyaProviderService")
        .expect("Huya provider service descriptor");
    assert_eq!(provider.methods().count(), 1);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.huya.HuyaPlaybackProviderService")
        .expect("Huya playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Huya(
            synctv_proto::source_config::HuyaMediaSourceConfig {
                source: Some(
                    synctv_proto::source_config::huya_media_source_config::Source::Live(
                        synctv_proto::source_config::HuyaLiveSourceConfig {
                            room_id: "660000".to_string(),
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Huya(_))
    ));

    for message_name in [
        "synctv.playback_provider.huya.GetHuyaResourceRequest",
        "synctv.playback_provider.huya.WatchHuyaDanmakuRequest",
    ] {
        let request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("Huya indexed request descriptor");
        assert_eq!(
            request
                .get_field_by_name("media_index")
                .expect("Huya media index")
                .kind(),
            Kind::Uint32
        );
    }

    for (message_name, field_name, expected_number) in [
        ("synctv.source_config.MediaSourceConfig", "huya", 9),
        ("synctv.client.PlaybackMetadata", "huya", 11),
    ] {
        let message = synctv_proto::DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("Huya containing message descriptor");
        assert_eq!(
            message
                .get_field_by_name(field_name)
                .expect("Huya oneof field")
                .number(),
            expected_number
        );
    }
}
