use prost_reflect::Kind;
use synctv_proto::source_config::media_source_config;

#[test]
fn douyu_services_and_wire_contract_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.douyu.DouyuProviderService")
        .expect("Douyu provider service descriptor");
    assert_eq!(provider.methods().count(), 1);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.douyu.DouyuPlaybackProviderService")
        .expect("Douyu playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Douyu(
            synctv_proto::source_config::DouyuMediaSourceConfig {
                room: "9999".to_string(),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Douyu(_))
    ));

    for message_name in [
        "synctv.playback_provider.douyu.GetDouyuResourceRequest",
        "synctv.playback_provider.douyu.WatchDouyuDanmakuRequest",
    ] {
        let request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("Douyu indexed request descriptor");
        assert_eq!(
            request
                .get_field_by_name("media_index")
                .expect("Douyu media index")
                .kind(),
            Kind::Uint32
        );
    }

    for (message_name, field_name, expected_number) in [
        ("synctv.source_config.MediaSourceConfig", "douyu", 10),
        ("synctv.client.PlaybackMetadata", "douyu", 12),
    ] {
        let message = synctv_proto::DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("Douyu containing message descriptor");
        assert_eq!(
            message
                .get_field_by_name(field_name)
                .expect("Douyu oneof field")
                .number(),
            expected_number
        );
    }
}
