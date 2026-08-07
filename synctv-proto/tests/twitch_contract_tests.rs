use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn twitch_services_and_wire_contract_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.twitch.TwitchProviderService")
        .expect("Twitch provider service descriptor");
    assert_eq!(provider.methods().count(), 10);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.twitch.TwitchPlaybackProviderService")
        .expect("Twitch playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);

    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Twitch(
            synctv_proto::source_config::TwitchMediaSourceConfig {
                source: Some(
                    synctv_proto::source_config::twitch_media_source_config::Source::Video(
                        synctv_proto::source_config::TwitchVideoSourceConfig {
                            video_id: "1234".to_string(),
                            shared: true,
                        },
                    ),
                ),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Twitch(
            synctv_proto::source_config::TwitchPlaylistSourceConfig {
                shared: true,
                source: Some(
                    synctv_proto::source_config::twitch_playlist_source_config::Source::Channel(
                        synctv_proto::source_config::twitch_playlist_source_config::Channel {
                            channel: "synctv".to_string(),
                            content: synctv_proto::source_config::TwitchPlaylistContent::Clips
                                as i32,
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Twitch(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Twitch(_))
    ));

    for message_name in [
        "synctv.playback_provider.twitch.GetTwitchResourceRequest",
        "synctv.playback_provider.twitch.WatchTwitchChatRequest",
    ] {
        let request = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("Twitch indexed request descriptor");
        assert_eq!(
            request
                .get_field_by_name("media_index")
                .expect("Twitch media index")
                .kind(),
            Kind::Uint32
        );
    }

    let list_request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.twitch.ListChannelItemsRequest")
        .expect("Twitch list request descriptor");
    assert_eq!(
        list_request
            .get_field_by_name("page_size")
            .expect("Twitch list page size")
            .kind(),
        Kind::Uint32
    );

    for (message_name, field_name, expected_number) in [
        ("synctv.source_config.MediaSourceConfig", "twitch", 8),
        ("synctv.source_config.PlaylistSourceConfig", "twitch", 8),
        ("synctv.client.ProviderTarget", "twitch", 4),
        ("synctv.client.PlaybackMetadata", "twitch", 7),
    ] {
        let message = synctv_proto::DESCRIPTOR_POOL
            .get_message_by_name(message_name)
            .expect("Twitch containing message descriptor");
        assert_eq!(
            message
                .get_field_by_name(field_name)
                .expect("Twitch oneof field")
                .number(),
            expected_number
        );
    }
}
