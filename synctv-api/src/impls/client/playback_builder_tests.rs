use super::{
    build_start_playback_request, build_update_playback, providers_manager_unavailable_error,
    static_media_source_provider, PlaybackUpdateCommand,
};
use crate::impls::ErrorKind;
use chrono::Utc;
use synctv_core::models::{Media, MediaId, PlaylistId, RoomId};

const EMPTY_TARGET_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn test_build_start_playback_request_rejects_proto_contract_violation() {
    let codec = crate::PublicIdCodec::plain();
    let err = build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: codec.encode_media_id(MediaId::expect_positive(1)).unwrap(),
            playlist_id: codec
                .encode_playlist_id(PlaylistId::expect_positive(2))
                .unwrap(),
            target: Vec::new(),
        },
        &codec,
    )
    .unwrap_err();

    assert!(err.to_string().contains("start_playback"));
}

#[test]
fn test_build_start_playback_request_parses_dynamic_target() {
    let codec = crate::PublicIdCodec::plain();
    let playlist_id = PlaylistId::expect_positive(123);
    let playlist_public_id = codec.encode_playlist_id(playlist_id).unwrap();
    let target = br#"{"path":"/tv/ep1.mp4"}"#.to_vec();
    let parsed = build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: String::new(),
            playlist_id: playlist_public_id,
            target: target.clone(),
        },
        &codec,
    )
    .unwrap();

    assert!(parsed.media_id.is_none());
    assert_eq!(parsed.playlist_id, Some(playlist_id));
    assert_eq!(parsed.target, target);
}

#[test]
fn test_build_update_playback_rejects_missing_type() {
    let codec = crate::PublicIdCodec::plain();
    let err = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest::default(),
        &codec,
    )
    .unwrap_err();

    let message = err.to_string();
    assert!(
        message.contains("update_playback.type_required") || message.contains("type"),
        "{message}"
    );
}

#[test]
fn test_build_update_playback_rejects_unknown_type() {
    let codec = crate::PublicIdCodec::plain();
    let err = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: 99,
            playing: None,
            position: None,
            speed: None,
            version: None,
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
        },
        &codec,
    )
    .unwrap_err();

    assert!(err.to_string().contains("type"));
}

#[test]
fn test_build_update_playback_rejects_playing_false_for_play() {
    let codec = crate::PublicIdCodec::plain();
    let err = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Play as i32,
            playing: Some(false),
            position: None,
            speed: None,
            version: None,
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
        },
        &codec,
    )
    .unwrap_err();

    assert!(err.to_string().contains("cannot request paused state"));
}

#[test]
fn test_build_update_playback_play_defaults_to_playing() {
    let codec = crate::PublicIdCodec::plain();
    let parsed = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Play as i32,
            playing: None,
            position: None,
            speed: Some(1.25),
            version: Some(8),
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
        },
        &codec,
    )
    .unwrap();

    match parsed {
        PlaybackUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
            expected_source,
        } => {
            assert_eq!(playing, Some(true));
            assert_eq!(position, None);
            assert_eq!(speed, Some(1.25));
            assert_eq!(version, Some(8));
            assert!(expected_source.is_none());
        }
    }
}

#[test]
fn test_build_update_playback_pause_defaults_to_paused() {
    let codec = crate::PublicIdCodec::plain();
    let parsed = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Pause as i32,
            playing: None,
            position: None,
            speed: None,
            version: Some(9),
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
        },
        &codec,
    )
    .unwrap();

    match parsed {
        PlaybackUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
            expected_source,
        } => {
            assert_eq!(playing, Some(false));
            assert_eq!(position, None);
            assert_eq!(speed, None);
            assert_eq!(version, Some(9));
            assert!(expected_source.is_none());
        }
    }
}

#[test]
fn test_build_update_playback_seek_requires_position() {
    let codec = crate::PublicIdCodec::plain();
    let err = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Seek as i32,
            playing: None,
            position: None,
            speed: Some(1.5),
            version: None,
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
        },
        &codec,
    )
    .unwrap_err();

    assert!(err.to_string().contains("requires position"));
}

#[test]
fn test_build_update_playback_speed_requires_speed() {
    let codec = crate::PublicIdCodec::plain();
    let err = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Speed as i32,
            playing: Some(true),
            position: Some(5.0),
            speed: None,
            version: None,
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
        },
        &codec,
    )
    .unwrap_err();

    assert!(err.to_string().contains("requires speed"));
}

#[test]
fn test_build_update_playback_seek_parses_full_state() {
    let codec = crate::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(55);
    let media_public_id = codec.encode_media_id(media_id).unwrap();
    let parsed = build_update_playback(
        synctv_proto::client::UpdatePlaybackRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Seek as i32,
            playing: Some(false),
            position: Some(42.5),
            speed: Some(1.5),
            version: Some(9),
            expected_media_id: Some(media_public_id),
            expected_playlist_id: Some(String::new()),
            expected_target_hash: Some(EMPTY_TARGET_HASH.to_string()),
        },
        &codec,
    )
    .unwrap();

    match parsed {
        PlaybackUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
            expected_source,
        } => {
            assert_eq!(playing, Some(false));
            assert_eq!(position, Some(42.5));
            assert_eq!(speed, Some(1.5));
            assert_eq!(version, Some(9));
            let expected_source = expected_source.expect("seek should carry source expectation");
            assert_eq!(expected_source.media_id, Some(media_id));
            assert!(expected_source.playlist_id.is_none());
            assert_eq!(expected_source.target_hash, EMPTY_TARGET_HASH);
        }
    }
}

fn make_media(provider_instance_name: &str) -> Media {
    Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: RoomId::new(),
        creator_id: None,
        name: "Static Media".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: Some(provider_instance_name.to_string()),
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

#[test]
fn test_static_media_source_provider_ignores_explicit_instance_binding() {
    let media = make_media("direct_url");
    assert_eq!(static_media_source_provider(&media).unwrap(), "direct_url");
}

#[test]
fn test_static_media_source_provider_accepts_default_instance_binding() {
    let media = make_media("");
    assert_eq!(static_media_source_provider(&media).unwrap(), "direct_url");
}

#[test]
fn test_providers_manager_missing_is_service_unavailable() {
    let err = providers_manager_unavailable_error();
    assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
    assert_eq!(
        err.message(),
        "Playback providers are not available on this server."
    );
}

#[test]
fn test_build_start_playback_request_converts_proto_validated_ids_without_reparsing() {
    let codec = crate::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(123);
    let target = build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: codec.encode_media_id(media_id).unwrap(),
            playlist_id: String::new(),
            target: Vec::new(),
        },
        &codec,
    )
    .expect("valid playback request");

    assert_eq!(target.media_id, Some(media_id));
    assert!(target.playlist_id.is_none());
    assert!(target.target.is_empty());
}
