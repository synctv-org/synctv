use prost_reflect::Kind;
use synctv_proto::source_config::{media_source_config, playlist_source_config};

#[test]
fn synology_services_are_registered_in_descriptor_pools() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.synology.SynologyProviderService")
        .expect("Synology provider service descriptor");
    assert_eq!(provider.methods().count(), 10);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.synology.SynologyPlaybackProviderService")
        .expect("Synology playback provider service descriptor");
    assert_eq!(playback.methods().count(), 3);
}

#[test]
fn synology_contract_separates_browse_and_playable_item_kinds() {
    let media = synctv_proto::source_config::MediaSourceConfig {
        provider: Some(media_source_config::Provider::Synology(
            synctv_proto::source_config::SynologyMediaSourceConfig {
                server_id: "dsm-home".to_string(),
                source: Some(
                    synctv_proto::source_config::synology_media_source_config::Source::LibraryItem(
                        synctv_proto::source_config::SynologyLibraryItemSourceConfig {
                            kind: synctv_proto::source_config::SynologyLibraryItemKind::Episode
                                as i32,
                            item_id: 42,
                            file_id: 84,
                        },
                    ),
                ),
            },
        )),
    };
    let playlist = synctv_proto::source_config::PlaylistSourceConfig {
        provider: Some(playlist_source_config::Provider::Synology(
            synctv_proto::source_config::SynologyPlaylistSourceConfig {
                server_id: "dsm-home".to_string(),
                source: Some(
                    synctv_proto::source_config::synology_playlist_source_config::Source::TvShows(
                        synctv_proto::source_config::SynologyTvShowsPlaylistSourceConfig {
                            library_id: 7,
                        },
                    ),
                ),
            },
        )),
    };
    assert!(matches!(
        media.provider,
        Some(media_source_config::Provider::Synology(_))
    ));
    assert!(matches!(
        playlist.provider,
        Some(playlist_source_config::Provider::Synology(_))
    ));

    let list_request = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.synology.ListFilesRequest")
        .expect("Synology list request descriptor");
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

    let browse_kind = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_enum_by_name("synctv.provider.synology.SynologyVideoEntryKind")
        .expect("Synology browse kind descriptor");
    assert!(browse_kind
        .get_value_by_name("SYNOLOGY_VIDEO_ENTRY_KIND_TV_SHOW")
        .is_some());

    let video_item = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.synology.VideoItem")
        .expect("Synology video item descriptor");
    assert_eq!(
        video_item
            .get_field_by_name("watched_ratio")
            .expect("watched ratio")
            .kind(),
        Kind::Double
    );
    assert_eq!(
        video_item
            .get_field_by_name("files")
            .expect("video files")
            .number(),
        24
    );

    let video_file = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.synology.VideoFile")
        .expect("Synology video file descriptor");
    assert_eq!(
        video_file
            .get_field_by_name("video_bitrate")
            .expect("video bitrate")
            .kind(),
        Kind::Uint64
    );
    assert_eq!(
        video_file
            .get_field_by_name("audio_channels")
            .expect("audio channels")
            .kind(),
        Kind::Uint32
    );

    let playback_metadata = synctv_proto::DESCRIPTOR_POOL
        .get_message_by_name("synctv.client.SynologyPlaybackMetadata")
        .expect("Synology playback metadata descriptor");
    assert_eq!(
        playback_metadata
            .get_field_by_name("subtitles")
            .expect("Synology subtitles")
            .number(),
        36
    );

    let playable_kind = synctv_proto::DESCRIPTOR_POOL
        .get_enum_by_name("synctv.source_config.SynologyLibraryItemKind")
        .expect("Synology playable kind descriptor");
    assert!(playable_kind
        .get_value_by_name("SYNOLOGY_LIBRARY_ITEM_KIND_TV_SHOW")
        .is_none());

    let segment = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_message_by_name("synctv.playback_provider.synology.GetSynologySegmentRequest")
        .expect("Synology segment request descriptor");
    assert_eq!(
        segment
            .get_field_by_name("target_url")
            .expect("target_url")
            .number(),
        2
    );
}
