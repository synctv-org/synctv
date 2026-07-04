use super::{
    build_playback_state_update, build_start_playback_request, static_media_source_provider,
    PlaybackStateUpdateCommand,
};
use chrono::Utc;
use synctv_core::models::{Media, MediaId, PlaylistId, ProviderTarget, RoomId, SourceProvider};

const EMPTY_TARGET_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn api_ok<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn api_err<T>(result: Result<T, crate::impls::ApiError>) -> TestResult<crate::impls::ApiError> {
    match result {
        Ok(_) => Err(test_error("expected API error")),
        Err(error) => Ok(error),
    }
}

fn codec_ok<T>(result: Result<T, String>) -> TestResult<T> {
    result.map_err(test_error)
}

fn proto_alist_target(relative_path: &str) -> Option<synctv_proto::client::ProviderTarget> {
    Some(synctv_proto::client::ProviderTarget {
        target: Some(synctv_proto::client::provider_target::Target::Alist(
            synctv_proto::client::AlistTarget {
                relative_path: relative_path.to_string(),
            },
        )),
    })
}

#[test]
fn test_build_start_playback_request_rejects_proto_contract_violation() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let err = api_err(build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: codec_ok(codec.encode_media_id(MediaId::expect_positive(1)))?,
            playlist_id: codec_ok(codec.encode_playlist_id(PlaylistId::expect_positive(2)))?,
            target: None,
        },
        &codec,
    ))?;

    assert!(err.to_string().contains("start_playback"));
    Ok(())
}

#[test]
fn test_build_start_playback_request_parses_alist_target() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let playlist_id = PlaylistId::expect_positive(123);
    let playlist_public_id = codec_ok(codec.encode_playlist_id(playlist_id))?;
    let target = ProviderTarget::alist("/tv/ep1.mp4".to_string());
    let parsed = api_ok(build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: String::new(),
            playlist_id: playlist_public_id,
            target: proto_alist_target("/tv/ep1.mp4"),
        },
        &codec,
    ))?;

    assert!(parsed.media_id.is_none());
    assert_eq!(parsed.playlist_id, Some(playlist_id));
    assert_eq!(parsed.target, Some(target));
    Ok(())
}

#[test]
fn test_build_playback_state_update_rejects_missing_type() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let err = api_err(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest::default(),
        &codec,
    ))?;

    let message = err.to_string();
    assert!(
        message.contains("update_playback_state.type_required") || message.contains("type"),
        "{message}"
    );
    Ok(())
}

#[test]
fn test_build_playback_state_update_rejects_unknown_type() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let err = api_err(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    assert!(err.to_string().contains("type"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_rejects_playing_false_for_play() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let err = api_err(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    assert!(err.to_string().contains("cannot request paused state"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_play_defaults_to_playing() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let parsed = api_ok(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    match parsed {
        PlaybackStateUpdateCommand::Patch {
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
    Ok(())
}

#[test]
fn test_build_playback_state_update_pause_defaults_to_paused() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let parsed = api_ok(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    match parsed {
        PlaybackStateUpdateCommand::Patch {
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
    Ok(())
}

#[test]
fn test_build_playback_state_update_seek_requires_position() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let err = api_err(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    assert!(err.to_string().contains("requires position"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_speed_requires_speed() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let err = api_err(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    assert!(err.to_string().contains("requires speed"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_seek_parses_full_state() -> TestResult {
    let codec = crate::public_id::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(55);
    let media_public_id = codec_ok(codec.encode_media_id(media_id))?;
    let parsed = api_ok(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
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
    ))?;

    match parsed {
        PlaybackStateUpdateCommand::Patch {
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
            let expected_source = expected_source
                .ok_or_else(|| test_error("seek should carry source expectation"))?;
            assert_eq!(expected_source.media_id, Some(media_id));
            assert!(expected_source.playlist_id.is_none());
            assert_eq!(expected_source.target_hash, EMPTY_TARGET_HASH);
        }
    }
    Ok(())
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
        source_provider: synctv_core::models::SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        provider_instance_name: Some(provider_instance_name.to_string()),
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

#[test]
fn test_static_media_source_provider_ignores_explicit_instance_binding() -> TestResult {
    let media = make_media("direct_url");
    assert_eq!(
        api_ok(static_media_source_provider(&media))?,
        SourceProvider::DirectUrl
    );
    Ok(())
}

#[test]
fn test_static_media_source_provider_accepts_default_instance_binding() -> TestResult {
    let media = make_media("");
    assert_eq!(
        api_ok(static_media_source_provider(&media))?,
        SourceProvider::DirectUrl
    );
    Ok(())
}

#[test]
fn test_build_start_playback_request_converts_proto_validated_ids_without_reparsing() -> TestResult
{
    let codec = crate::public_id::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(123);
    let target = api_ok(build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: codec_ok(codec.encode_media_id(media_id))?,
            playlist_id: String::new(),
            target: None,
        },
        &codec,
    ))?;

    assert_eq!(target.media_id, Some(media_id));
    assert!(target.playlist_id.is_none());
    assert!(target.target.is_none());
    Ok(())
}
