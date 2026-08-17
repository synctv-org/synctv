use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn qnap_services_are_registered_in_descriptor_pools() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.qnap.QnapProviderService")
        .expect("QNAP provider service descriptor");
    assert_eq!(provider.methods().count(), 6);
    assert!(provider
        .methods()
        .any(|method| method.name() == "GetThumbnail" && method.is_server_streaming()));

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.qnap.QnapPlaybackProviderService")
        .expect("QNAP playback provider service descriptor");
    assert_eq!(playback.methods().count(), 6);
    assert!(playback
        .methods()
        .any(|method| method.name() == "GetThumbnailResource"));
}

#[test]
fn qnap_source_and_provider_contracts_keep_native_types() {
    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Qnap(
            synctv_proto::source_config::QnapMediaSourceConfig {
                server_id: "qnap-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: "/Multimedia/Movie.mkv".to_string(),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Qnap(
            synctv_proto::source_config::QnapPlaylistSourceConfig {
                server_id: "qnap-home".to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: "/Multimedia".to_string(),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Qnap(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Qnap(_))
    ));

    let list_request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.qnap.ListRequest")
        .expect("QNAP list request descriptor");
    assert_eq!(
        list_request.get_field_by_name("page").expect("page").kind(),
        Kind::Uint64
    );
    assert_eq!(
        list_request
            .get_field_by_name("page_size")
            .expect("page_size")
            .kind(),
        Kind::Uint32
    );

    let item = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.qnap.FileItem")
        .expect("QNAP file item descriptor");
    assert!(item
        .get_field_by_name("pre_transcoded_heights")
        .expect("pre_transcoded_heights")
        .is_list());

    let resource = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_message_by_name("synctv.playback_provider.qnap.GetQnapResourceRequest")
        .expect("QNAP resource request descriptor");
    assert_eq!(
        resource.get_field_by_name("range").expect("range").number(),
        8
    );
    assert_eq!(
        resource.get_field_by_name("head").expect("head").number(),
        9
    );
}
