use super::{
    apply_live_stream_generation, apply_static_direct_url_thumbnail, build_playback_state_update,
    build_start_playback_request, filter_unavailable_live_whep, generation_supports_whep,
    remove_unavailable_live_whep, retain_live_whep_if_enabled, static_media_source_provider,
    PlaybackBuildActor,
};
use synctv_core::models::{
    Media, MediaId, PlaybackDirectUrlMedia, PlaybackInfo, PlaybackMedia, PlaybackMediaProvider,
    PlaybackResult, PlaylistId, ProviderTarget, RoomId, SourceProvider, UserId,
};
use synctv_core::provider::ProviderActor;

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

fn live_generation(ready: bool) -> synctv_livestream::StreamGeneration {
    synctv_livestream::StreamGeneration {
        node_id: "node-live".to_string(),
        cluster_address: "127.0.0.1:50051".to_string(),
        app_name: "live".to_string(),
        user_id: "user-live".to_string(),
        started_at: synctv_core::SystemClock.now(),
        ready_at: ready.then(|| synctv_core::SystemClock.now()),
        ended_at: None,
        lease_epoch: 1,
        generation_id: "generation-live".to_string(),
        supports_rtp: false,
    }
}

fn live_playback_result(default_mode: &str, modes: &[&str]) -> PlaybackResult {
    let media_id = MediaId::expect_positive(1);
    let room_id = RoomId::expect_positive(2);
    let playback_infos = modes
        .iter()
        .map(|mode| {
            (
                (*mode).to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: Vec::new(),
                    default_media_index: None,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )
        })
        .collect();
    PlaybackResult {
        id: Some(media_id),
        playlist_id: None,
        room_id,
        name: "live".to_string(),
        provider: SourceProvider::Rtmp,
        provider_instance_name: None,
        position: 0.0,
        playback_infos,
        default_mode: default_mode.to_string(),
        duration_seconds: None,
        playback_kind: synctv_core::models::PlaybackKind::Live,
        target: None,
        metadata: Some(synctv_core::models::PlaybackMetadata::Live(
            synctv_core::models::LivePlaybackMetadata { media_id, room_id },
        )),
    }
}

#[test]
fn playback_build_actor_keeps_guest_and_user_identity_shapes_distinct() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
    let user_id = UserId::expect_positive(42);
    let user = PlaybackBuildActor::user(&user_id);
    let guest = PlaybackBuildActor::guest("gst_session");

    assert_eq!(user.provider_actor(), ProviderActor::User(user_id));
    assert_eq!(guest.provider_actor(), ProviderActor::Guest);
    assert_eq!(
        api_ok(user.public_actor_id(&codec))?,
        codec_ok(codec.encode_user_id(user_id))?
    );
    assert_eq!(api_ok(guest.public_actor_id(&codec))?, "gst_session");
    Ok(())
}

#[test]
fn live_stream_generation_maps_to_authoritative_playback_availability() {
    let mut metadata = synctv_proto::client::LivePlaybackMetadata::default();

    apply_live_stream_generation(&mut metadata, None);
    assert_eq!(
        metadata.availability,
        synctv_proto::client::LiveStreamAvailability::Offline as i32
    );
    assert!(metadata.stream_generation_id.is_empty());

    let pending = live_generation(false);
    apply_live_stream_generation(&mut metadata, Some(&pending));
    assert_eq!(
        metadata.availability,
        synctv_proto::client::LiveStreamAvailability::Offline as i32
    );
    assert!(metadata.stream_generation_id.is_empty());

    let live = live_generation(true);
    apply_live_stream_generation(&mut metadata, Some(&live));
    assert_eq!(
        metadata.availability,
        synctv_proto::client::LiveStreamAvailability::Live as i32
    );
    assert_eq!(metadata.stream_generation_id, "generation-live");
}

#[test]
fn whep_requires_a_ready_rtp_generation() {
    let mut generation = live_generation(true);
    assert!(!generation_supports_whep(Some(&generation)));

    generation.supports_rtp = true;
    assert!(generation_supports_whep(Some(&generation)));

    generation.ready_at = None;
    assert!(!generation_supports_whep(Some(&generation)));
    assert!(!generation_supports_whep(None));
}

#[test]
fn unavailable_whep_falls_back_to_hls_deterministically() -> TestResult {
    let mut result = live_playback_result("whep", &["whep", "flv", "hls"]);

    api_ok(remove_unavailable_live_whep(&mut result))?;

    assert!(!result.playback_infos.contains_key("whep"));
    assert_eq!(result.default_mode, "hls");
    Ok(())
}

#[test]
fn unavailable_whep_rejects_a_webrtc_only_client_result() -> TestResult {
    let mut result = live_playback_result("whep", &["whep"]);

    let error = api_err(remove_unavailable_live_whep(&mut result))?;

    assert!(matches!(
        error,
        crate::impls::ApiError::ClientIncompatible { .. }
    ));
    Ok(())
}

#[test]
fn disabled_webrtc_removes_whep_before_provider_specific_filtering() -> TestResult {
    for provider in [SourceProvider::Rtmp, SourceProvider::LiveProxy] {
        let mut result = live_playback_result("whep", &["whep", "hls"]);
        result.provider = provider;

        assert!(!api_ok(retain_live_whep_if_enabled(&mut result, false))?);
        assert!(!result.playback_infos.contains_key("whep"));
        assert_eq!(result.default_mode, "hls");
    }
    Ok(())
}

#[tokio::test]
async fn live_generation_is_resolved_for_clients_without_whep() -> TestResult {
    let mut result = live_playback_result("hls", &["hls", "flv"]);
    let room_id = result.room_id.to_string();
    let media_id = result
        .id
        .ok_or_else(|| test_error("live playback result is missing media id"))?
        .to_string();
    let registry = synctv_livestream::local_stream_registry();
    let generation_id = "00000000-0000-4000-8000-000000000001";
    assert!(
        registry
            .try_activate_generation(
                &room_id,
                &media_id,
                "node-live",
                "user-live",
                "127.0.0.1:50051",
                generation_id,
            )
            .await?
    );
    let generation = registry
        .get_active_generation(&room_id, &media_id)
        .await?
        .ok_or_else(|| test_error("active generation is missing"))?;
    assert!(
        registry
            .mark_generation_ready(&room_id, &media_id, generation_id, generation.lease_epoch,)
            .await?
    );

    let (event_sender, _event_receiver) = tokio::sync::mpsc::channel(8);
    let infrastructure = std::sync::Arc::new(synctv_livestream::LiveStreamingInfrastructure::new(
        registry,
        event_sender,
        std::sync::Arc::new(synctv_livestream::StreamTracker::new()),
        "node-live".to_string(),
        synctv_common::ssrf::SsrfGuard::disabled(),
    )?);

    let generation =
        api_ok(filter_unavailable_live_whep(Some(&infrastructure), &mut result).await)?
            .ok_or_else(|| test_error("active generation was not resolved"))?;

    assert_eq!(generation.generation_id, generation_id);
    assert!(generation.ready_at.is_some());
    assert_eq!(result.default_mode, "hls");
    assert!(!result.playback_infos.contains_key("whep"));
    Ok(())
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
fn test_build_start_playback_request_accepts_static_playlist_context() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(1);
    let playlist_id = PlaylistId::expect_positive(2);
    let parsed = api_ok(build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: codec_ok(codec.encode_media_id(media_id))?,
            playlist_id: codec_ok(codec.encode_playlist_id(playlist_id))?,
            target: None,
            client_operation_id: None,
        },
        &codec,
    ))?;

    assert_eq!(parsed.media_id, Some(media_id));
    assert_eq!(parsed.playlist_id, Some(playlist_id));
    assert!(parsed.target.is_none());
    Ok(())
}

#[test]
fn test_build_start_playback_request_parses_alist_target() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
    let playlist_id = PlaylistId::expect_positive(123);
    let playlist_public_id = codec_ok(codec.encode_playlist_id(playlist_id))?;
    let target = ProviderTarget::alist("/tv/ep1.mp4".to_string());
    let parsed = api_ok(build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: String::new(),
            playlist_id: playlist_public_id,
            target: proto_alist_target("/tv/ep1.mp4"),
            client_operation_id: None,
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
    let codec = synctv_adapter::PublicIdCodec::plain();
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
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert!(err.to_string().contains("type"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_rejects_playing_false_for_play() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert!(err.to_string().contains("cannot request paused state"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_play_defaults_to_playing() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert_eq!(parsed.playing, Some(true));
    assert_eq!(parsed.position, None);
    assert_eq!(parsed.speed, Some(1.25));
    assert_eq!(parsed.version, Some(8));
    assert!(parsed.expected_source.is_none());
    Ok(())
}

#[test]
fn test_build_playback_state_update_pause_defaults_to_paused() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert_eq!(parsed.playing, Some(false));
    assert_eq!(parsed.position, None);
    assert_eq!(parsed.speed, None);
    assert_eq!(parsed.version, Some(9));
    assert!(parsed.expected_source.is_none());
    Ok(())
}

#[test]
fn test_build_playback_state_update_seek_requires_position() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert!(err.to_string().contains("requires position"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_speed_requires_speed() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert!(err.to_string().contains("requires speed"));
    Ok(())
}

#[test]
fn test_build_playback_state_update_accepts_sparse_speed_patch() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
    let parsed = api_ok(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Speed as i32,
            playing: None,
            position: None,
            speed: Some(1.5),
            version: None,
            expected_media_id: None,
            expected_playlist_id: None,
            expected_target_hash: None,
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert_eq!(parsed.playing, None);
    assert_eq!(parsed.position, None);
    assert_eq!(parsed.speed, Some(1.5));
    Ok(())
}

#[test]
fn test_build_playback_state_update_seek_parses_full_state() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
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
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    assert_eq!(parsed.playing, Some(false));
    assert_eq!(parsed.position, Some(42.5));
    assert_eq!(parsed.speed, Some(1.5));
    assert_eq!(parsed.version, Some(9));
    let expected_source = parsed
        .expected_source
        .ok_or_else(|| test_error("seek should carry source expectation"))?;
    assert_eq!(expected_source.media_id, Some(media_id));
    assert!(expected_source.playlist_id.is_none());
    assert_eq!(expected_source.target_hash, EMPTY_TARGET_HASH);
    Ok(())
}

#[test]
fn test_build_playback_state_update_accepts_omitted_static_playlist_guard() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(56);
    let media_public_id = codec_ok(codec.encode_media_id(media_id))?;
    let parsed = api_ok(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Play as i32,
            playing: Some(true),
            position: Some(1.0),
            speed: None,
            version: None,
            expected_media_id: Some(media_public_id),
            expected_playlist_id: None,
            expected_target_hash: Some(EMPTY_TARGET_HASH.to_string()),
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    let expected_source = parsed
        .expected_source
        .ok_or_else(|| test_error("play should carry source expectation"))?;
    assert_eq!(expected_source.media_id, Some(media_id));
    assert!(expected_source.playlist_id.is_none());
    Ok(())
}

#[test]
fn test_build_playback_state_update_accepts_omitted_dynamic_media_guard() -> TestResult {
    let codec = synctv_adapter::PublicIdCodec::plain();
    let playlist_id = synctv_core::models::PlaylistId::expect_positive(57);
    let playlist_public_id = codec_ok(codec.encode_playlist_id(playlist_id))?;
    let parsed = api_ok(build_playback_state_update(
        synctv_proto::client::UpdatePlaybackStateRequest {
            r#type: synctv_proto::client::PlaybackUpdateType::Pause as i32,
            playing: Some(false),
            position: Some(1.0),
            speed: None,
            version: None,
            expected_media_id: None,
            expected_playlist_id: Some(playlist_public_id),
            expected_target_hash: Some(EMPTY_TARGET_HASH.to_string()),
            client_operation_id: None,
            client_time_millis: None,
        },
        &codec,
    ))?;

    let expected_source = parsed
        .expected_source
        .ok_or_else(|| test_error("pause should carry source expectation"))?;
    assert!(expected_source.media_id.is_none());
    assert_eq!(expected_source.playlist_id, Some(playlist_id));
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
        thumbnail_file_reference_id: None,
        added_at: synctv_core::SystemClock.now(),
        updated_at: synctv_core::SystemClock.now(),
        version: 0,
    }
}

fn test_media(url: &str) -> PlaybackMedia {
    PlaybackMedia {
        name: "media".to_string(),
        format: "mp4".to_string(),
        expire_at: None,
        metadata: None,
        p2p_swarm_id: None,
        provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
            url: url.to_string(),
            headers: std::collections::HashMap::new(),
        }),
    }
}

#[test]
fn test_direct_url_thumbnail_overrides_playback_modes() -> TestResult {
    let mut result = PlaybackResult::builder(None, RoomId::new(), "media".to_string(), 0.0)
        .provider(synctv_core::models::SourceProvider::DirectUrl)
        .default_mode("direct".to_string())
        .add_mode(
            "direct".to_string(),
            PlaybackInfo::builder()
                .add_media(test_media("https://example.com/direct.mp4"))
                .build(),
        )
        .add_mode(
            "provider".to_string(),
            PlaybackInfo::builder()
                .thumbnail(Some("https://provider.example.com/thumb.jpg".to_string()))
                .add_media(test_media("https://example.com/provider.mp4"))
                .build(),
        )
        .build()
        .ok_or_else(|| test_error("playback result should build"))?;

    apply_static_direct_url_thumbnail(
        &mut result,
        SourceProvider::DirectUrl,
        Some("https://upload.example.com/thumbnail.jpg"),
    );

    assert_eq!(
        result
            .playback_infos
            .get("direct")
            .and_then(|info| info.thumbnail.as_deref()),
        Some("https://upload.example.com/thumbnail.jpg")
    );
    assert_eq!(
        result
            .playback_infos
            .get("provider")
            .and_then(|info| info.thumbnail.as_deref()),
        Some("https://upload.example.com/thumbnail.jpg")
    );
    Ok(())
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
    let codec = synctv_adapter::PublicIdCodec::plain();
    let media_id = MediaId::expect_positive(123);
    let target = api_ok(build_start_playback_request(
        synctv_proto::client::StartPlaybackRequest {
            media_id: codec_ok(codec.encode_media_id(media_id))?,
            playlist_id: String::new(),
            target: None,
            client_operation_id: None,
        },
        &codec,
    ))?;

    assert_eq!(target.media_id, Some(media_id));
    assert!(target.playlist_id.is_none());
    assert!(target.target.is_none());
    Ok(())
}
