use prost_reflect::Kind;
use synctv_proto::source_config::media_source_config;

#[test]
fn acfun_services_and_wire_contract_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.acfun.AcFunProviderService")
        .expect("AcFun provider service descriptor");
    assert_eq!(provider.methods().count(), 1);
    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.acfun.AcFunPlaybackProviderService")
        .expect("AcFun playback provider service descriptor");
    assert_eq!(playback.methods().count(), 4);
    assert!(playback
        .methods()
        .any(|method| method.name() == "GetHlsResource"));

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::AcFun(
            synctv_proto::source_config::AcFunMediaSourceConfig {
                source: Some(
                    synctv_proto::source_config::ac_fun_media_source_config::Source::Bangumi(
                        synctv_proto::source_config::AcFunBangumiSourceConfig {
                            bangumi_id: "aa123".to_string(),
                            episode_query: Some("ac=456".to_string()),
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::AcFun(_))
    ));

    for message_name in [
        "synctv.playback_provider.acfun.GetAcFunResourceRequest",
        "synctv.playback_provider.acfun.GetAcFunHlsResourceRequest",
        "synctv.playback_provider.acfun.GetAcFunDanmakuFileRequest",
        "synctv.playback_provider.acfun.WatchAcFunDanmakuRequest",
    ] {
        let request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("AcFun indexed request descriptor");
        assert_eq!(
            request
                .get_field_by_name("media_index")
                .expect("AcFun media index")
                .kind(),
            Kind::Uint32
        );
        if message_name.contains("HlsResource") {
            assert!(matches!(
                request
                    .get_field_by_name("resource_kind")
                    .expect("AcFun HLS resource kind")
                    .kind(),
                Kind::Enum(_)
            ));
        }
    }

    for (message_name, field_name, expected_number) in [
        ("synctv.source_config.MediaSourceConfig", "ac_fun", 12),
        ("synctv.client.PlaybackMetadata", "ac_fun", 13),
    ] {
        let message = synctv_proto::DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("AcFun containing message descriptor");
        assert_eq!(
            message
                .get_field_by_name(field_name)
                .expect("AcFun oneof field")
                .number(),
            expected_number
        );
    }
}
