//! Scheduler and metadata unit tests.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use synctv_xiu::flv::amf0::{Amf0ValueType, amf0_reader::Amf0Reader, amf0_writer::Amf0Writer};
use synctv_xiu::streamhub::utils::Uuid;

use super::{
    RungState, SourceState, TranscodePreset, parse_source_metadata, pick_most_demanded,
    rung_stream_name, split_rung_stream_name,
};

fn preset(name: &str, height: u32) -> TranscodePreset {
    TranscodePreset {
        name: name.to_string(),
        height,
        video_kbps: 1000,
        audio_kbps: 64,
        max_fps: 30,
    }
}

fn preset_with_fps(name: &str, height: u32, max_fps: u32) -> TranscodePreset {
    TranscodePreset {
        max_fps,
        ..preset(name, height)
    }
}

fn waiting(mut rung: RungState, viewers: u32) -> RungState {
    rung.waiting_viewers = viewers;
    rung
}

type SourceEntries = Vec<((String, String), Vec<(String, RungState)>)>;

fn sources(entries: SourceEntries) -> HashMap<(String, String), SourceState> {
    entries
        .into_iter()
        .map(|(key, rungs)| {
            (
                key,
                SourceState {
                    rungs: rungs.into_iter().collect(),
                    source_resolution: None,
                    source_frame_rate: None,
                    source_video_bitrate_kbps: None,
                },
            )
        })
        .collect()
}

#[test]
fn rung_stream_names_round_trip() {
    assert_eq!(rung_stream_name("123", "480p"), "123-480p");
    assert_eq!(split_rung_stream_name("123-480p"), Some(("123", "480p")));
    // Preset names may contain the separator; the numeric base makes the
    // first separator unambiguous.
    assert_eq!(
        split_rung_stream_name("123-high-quality"),
        Some(("123", "high-quality"))
    );
    assert_eq!(split_rung_stream_name("123"), None);
    assert_eq!(split_rung_stream_name("-480p"), None);
    assert_eq!(split_rung_stream_name("123-"), None);
    // A stream key with a non-numeric base is never a rung key.
    assert_eq!(split_rung_stream_name("media-480p"), None);
}

#[test]
fn pick_prefers_the_most_waiting_viewers() {
    let presets = vec![
        preset("720p", 720),
        preset("480p", 480),
        preset("360p", 360),
    ];
    let sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![
            ("720p".to_string(), waiting(RungState::default(), 1)),
            ("480p".to_string(), waiting(RungState::default(), 5)),
            ("360p".to_string(), waiting(RungState::default(), 3)),
        ],
    )]);
    let (_, rung, demand) =
        pick_most_demanded(&sources, &presets, Instant::now()).expect("a pick exists");
    assert_eq!((rung, demand), ("480p", 5));
}

#[test]
fn pick_skips_running_unsupported_cooldown_and_low_value_rungs() {
    let presets = vec![
        preset("720p", 720),
        preset("480p", 480),
        preset("360p", 360),
    ];
    let mut running = RungState::default();
    running.running = Some(super::RunningRung {
        cancel: tokio_util::sync::CancellationToken::new(),
        idle_since: None,
    });
    let mut unsupported = RungState::default();
    unsupported.unsupported = Some("no decoder".to_string());
    let mut cooling_down = waiting(RungState::default(), 9);
    cooling_down.last_failure = Some(Instant::now());
    let sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![
            ("720p".to_string(), running),
            ("480p".to_string(), unsupported),
            ("360p".to_string(), cooling_down),
        ],
    )]);
    assert!(pick_most_demanded(&sources, &presets, Instant::now()).is_none());

    // After the cooldown lapses the rung becomes schedulable again.
    let mut elapsed_sources = sources;
    let rung = elapsed_sources
        .get_mut(&("room".to_string(), "media".to_string()))
        .and_then(|source| source.rungs.get_mut("360p"))
        .expect("rung exists");
    rung.last_failure = Instant::now().checked_sub(Duration::from_secs(31));
    let (_, rung, _) = pick_most_demanded(&elapsed_sources, &presets, Instant::now())
        .expect("pick after cooldown");
    assert_eq!(rung, "360p");
}

#[test]
fn pick_skips_rungs_at_or_above_source_resolution() {
    let presets = vec![
        preset("1080p", 1080),
        preset("720p", 720),
        preset("480p", 480),
    ];
    let mut sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![
            ("1080p".to_string(), waiting(RungState::default(), 9)),
            ("720p".to_string(), waiting(RungState::default(), 8)),
            ("480p".to_string(), waiting(RungState::default(), 1)),
        ],
    )]);
    // A 720p source makes both the 1080p and 720p rungs pointless.
    sources
        .get_mut(&("room".to_string(), "media".to_string()))
        .expect("source exists")
        .source_resolution = Some((1280, 720));
    let (_, rung, _) =
        pick_most_demanded(&sources, &presets, Instant::now()).expect("a pick exists");
    assert_eq!(rung, "480p");
}

#[test]
fn pick_schedules_same_height_rung_that_caps_frame_rate() {
    // 1080p60 source, a same-height 1080p30 rung, and a lower 720p30 rung:
    // both genuinely re-encode (one caps fps, one steps down), so demand
    // decides.
    let presets = vec![preset("1080p", 1080), preset("720p", 720)];
    let mut sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![
            ("1080p".to_string(), waiting(RungState::default(), 5)),
            ("720p".to_string(), waiting(RungState::default(), 2)),
        ],
    )]);
    let source = sources
        .get_mut(&("room".to_string(), "media".to_string()))
        .expect("source exists");
    source.source_resolution = Some((1920, 1080));
    source.source_frame_rate = Some(60.0);
    let (_, rung, _) =
        pick_most_demanded(&sources, &presets, Instant::now()).expect("a pick exists");
    assert_eq!(
        rung, "1080p",
        "the same-height fps-capping rung is schedulable"
    );
}

#[test]
fn pick_skips_same_height_rung_when_frame_rate_within_ceiling() {
    // 1080p30 source: a 1080p30 rung would only re-encode at the source's
    // own quality, so only the 720p rung can be picked.
    let presets = vec![preset("1080p", 1080), preset("720p", 720)];
    let mut sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![
            ("1080p".to_string(), waiting(RungState::default(), 9)),
            ("720p".to_string(), waiting(RungState::default(), 1)),
        ],
    )]);
    let source = sources
        .get_mut(&("room".to_string(), "media".to_string()))
        .expect("source exists");
    source.source_resolution = Some((1920, 1080));
    source.source_frame_rate = Some(29.97);
    let (_, rung, _) =
        pick_most_demanded(&sources, &presets, Instant::now()).expect("a pick exists");
    assert_eq!(rung, "720p");
}

#[test]
fn pick_allows_taller_preset_that_caps_frame_rate() {
    // A 4K30 preset against a 1080p60 source never upscales (the ffmpeg
    // scale caps at the source height) while still halving the frame rate,
    // so it is worth scheduling.
    let presets = vec![preset_with_fps("4k", 2160, 30)];
    let mut sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![("4k".to_string(), waiting(RungState::default(), 1))],
    )]);
    let source = sources
        .get_mut(&("room".to_string(), "media".to_string()))
        .expect("source exists");
    source.source_resolution = Some((1920, 1080));
    source.source_frame_rate = Some(59.94);
    let (_, rung, _) =
        pick_most_demanded(&sources, &presets, Instant::now()).expect("a pick exists");
    assert_eq!(rung, "4k");
}

#[test]
fn metadata_parses_real_amf_ecma_array() {
    let mut properties = IndexMap::new();
    properties.insert("width".to_string(), Amf0ValueType::Number(1920.0));
    properties.insert("height".to_string(), Amf0ValueType::Number(1080.0));
    properties.insert("framerate".to_string(), Amf0ValueType::Number(60.0));
    properties.insert("videocodecid".to_string(), Amf0ValueType::Number(7.0));
    let mut writer = Amf0Writer::new();
    writer
        .write_string(&"@setDataFrame".to_string())
        .expect("AMF0 string writes into an in-memory buffer");
    writer
        .write_string(&"onMetaData".to_string())
        .expect("AMF0 string writes into an in-memory buffer");
    writer
        .write_eacm_array(&properties)
        .expect("AMF0 array writes into an in-memory buffer");
    let body = writer.extract_current_bytes();

    // The script tag body starts at the "onMetaData" value, matching how
    // FrameData::MetaData arrives (see the RTMP message path).
    let values = Amf0Reader::new(synctv_xiu::bytesio::bytes_reader::BytesReader::new(
        body.clone(),
    ))
    .read_all()
    .expect("amf parse");
    assert_eq!(
        values.first(),
        Some(&Amf0ValueType::UTF8String("@setDataFrame".to_string()))
    );

    // The parser tolerates both a bare onMetaData body and the RTMP
    // @setDataFrame framing by scanning for the first ECMA array.
    assert_eq!(
        parse_source_metadata(&body),
        super::SourceMetadata {
            resolution: Some((1920, 1080)),
            frame_rate: Some(60.0),
            video_bitrate_kbps: None,
        }
    );

    assert_eq!(parse_source_metadata(b""), super::SourceMetadata::default());
    assert_eq!(
        parse_source_metadata(b"not amf at all"),
        super::SourceMetadata::default()
    );

    // The alternate `frame_rate` key and junk values.
    let mut properties = IndexMap::new();
    properties.insert("width".to_string(), Amf0ValueType::Number(1280.0));
    properties.insert("height".to_string(), Amf0ValueType::Number(720.0));
    properties.insert("frame_rate".to_string(), Amf0ValueType::Number(59.94));
    let mut writer = Amf0Writer::new();
    writer
        .write_string(&"onMetaData".to_string())
        .expect("AMF0 string writes into an in-memory buffer");
    writer
        .write_eacm_array(&properties)
        .expect("AMF0 array writes into an in-memory buffer");
    assert_eq!(
        parse_source_metadata(&writer.extract_current_bytes()),
        super::SourceMetadata {
            resolution: Some((1280, 720)),
            frame_rate: Some(59.94),
            video_bitrate_kbps: None,
        }
    );

    // Junk frame rates are dropped while resolution still parses.
    let mut properties = IndexMap::new();
    properties.insert("width".to_string(), Amf0ValueType::Number(1280.0));
    properties.insert("height".to_string(), Amf0ValueType::Number(720.0));
    properties.insert("framerate".to_string(), Amf0ValueType::Number(0.0));
    let mut writer = Amf0Writer::new();
    writer
        .write_string(&"onMetaData".to_string())
        .expect("AMF0 string writes into an in-memory buffer");
    writer
        .write_eacm_array(&properties)
        .expect("AMF0 array writes into an in-memory buffer");
    assert_eq!(
        parse_source_metadata(&writer.extract_current_bytes()),
        super::SourceMetadata {
            resolution: Some((1280, 720)),
            frame_rate: None,
            video_bitrate_kbps: None,
        }
    );
}

#[test]
fn uuid_generation_ids_are_distinct() {
    // Rung generations must never collide across jobs of the same source.
    assert_ne!(Uuid::new(), Uuid::new());
}

#[test]
fn eligible_presets_filter_by_resolution_and_verdicts() {
    use super::eligible_presets_for;

    let presets = vec![
        preset("1080p", 1080),
        preset("720p", 720),
        preset("480p", 480),
    ];

    // No local source state yet: the full ladder is offered.
    let offered = eligible_presets_for(&presets, None);
    assert_eq!(offered.len(), 3);

    // Source known to be 720p: rungs at or above it never re-encode usefully.
    let mut state = SourceState {
        rungs: HashMap::new(),
        source_resolution: Some((1280, 720)),
        source_frame_rate: None,
        source_video_bitrate_kbps: None,
    };
    state.rungs.insert(
        "480p".to_string(),
        RungState {
            unsupported: Some("source has no decodable video track".to_string()),
            ..RungState::default()
        },
    );
    let offered = eligible_presets_for(&presets, Some(&state));
    assert_eq!(
        offered.len(),
        0,
        "1080p and 720p never re-encode a 720p source and 480p is unsupported"
    );

    // A rung that only failed (retryable cooldown) stays offered.
    state.rungs.insert(
        "480p".to_string(),
        RungState {
            last_failure: Some(Instant::now()),
            ..RungState::default()
        },
    );
    let offered = eligible_presets_for(&presets, Some(&state));
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].name, "480p");

    // A same-height rung is offered when it caps a higher source frame
    // rate (1080p60 source, 1080p30 rung), and only the genuinely
    // pointless same-quality rung disappears.
    state.source_resolution = Some((1920, 1080));
    state.source_frame_rate = Some(60.0);
    state.rungs.clear();
    let offered = eligible_presets_for(&presets, Some(&state));
    assert_eq!(offered.len(), 3, "every rung caps or steps down");

    // 1080p30 source: the 1080p30 rung adds nothing.
    state.source_frame_rate = Some(29.97);
    let offered = eligible_presets_for(&presets, Some(&state));
    assert_eq!(
        offered.len(),
        2,
        "the same-height rung within the frame-rate ceiling is not offered"
    );
    assert!(offered.iter().all(|preset| preset.name != "1080p"));

    // A 480p source within the frame-rate ceiling offers nothing: every
    // rung would re-encode at the source's own quality. (A 480p60 source
    // would still be offered the fps-capping rungs.)
    state.source_resolution = Some((854, 480));
    state.source_frame_rate = Some(24.0);
    let offered = eligible_presets_for(&presets, Some(&state));
    assert_eq!(offered.len(), 0, "no rung improves on a 480p24 source");
}

#[test]
fn metadata_parses_videodatarate_and_rejects_junk() {
    let body = |props: Vec<(&str, Amf0ValueType)>| -> bytes::Bytes {
        let mut properties = IndexMap::new();
        for (key, value) in props {
            properties.insert(key.to_string(), value);
        }
        let mut writer = Amf0Writer::new();
        writer
            .write_string(&"onMetaData".to_string())
            .expect("AMF0 string writes into an in-memory buffer");
        writer
            .write_eacm_array(&properties)
            .expect("AMF0 array writes into an in-memory buffer");
        writer.extract_current_bytes().freeze()
    };

    assert_eq!(
        parse_source_metadata(&body(vec![
            ("width", Amf0ValueType::Number(1920.0)),
            ("height", Amf0ValueType::Number(1080.0)),
            ("videodatarate", Amf0ValueType::Number(8000.4)),
        ]))
        .video_bitrate_kbps,
        Some(8000)
    );

    // Out-of-envelope values are dropped rather than trusted.
    for junk in [0.0, -5.0, 100_001.0] {
        assert_eq!(
            parse_source_metadata(&body(vec![
                ("width", Amf0ValueType::Number(1920.0)),
                ("height", Amf0ValueType::Number(1080.0)),
                ("videodatarate", Amf0ValueType::Number(junk)),
            ]))
            .video_bitrate_kbps,
            None
        );
    }
}

#[test]
fn pick_skips_every_rung_when_source_bitrate_is_starved() {
    use super::eligible_presets_for;

    // The abusive shape: 1080p60 metadata with a near-zero video bitrate.
    // Every ladder rung would encode at a higher bitrate than the source
    // publishes, amplifying the stream; the scheduler must pick nothing.
    let presets = vec![
        preset("1080p", 1080),
        preset("720p", 720),
        preset("480p", 480),
    ];
    let mut sources = sources(vec![(
        ("room".to_string(), "media".to_string()),
        vec![
            ("1080p".to_string(), waiting(RungState::default(), 9)),
            ("720p".to_string(), waiting(RungState::default(), 5)),
            ("480p".to_string(), waiting(RungState::default(), 1)),
        ],
    )]);
    let source = sources
        .get_mut(&("room".to_string(), "media".to_string()))
        .expect("source exists");
    source.source_resolution = Some((1920, 1080));
    source.source_frame_rate = Some(60.0);
    source.source_video_bitrate_kbps = Some(100);

    assert!(pick_most_demanded(&sources, &presets, Instant::now()).is_none());
    let offered = eligible_presets_for(
        &presets,
        sources.get(&("room".to_string(), "media".to_string())),
    );
    assert_eq!(
        offered.len(),
        0,
        "no rung may up-transcode a starved source"
    );
}

#[test]
fn bitrate_gate_skips_rungs_at_or_above_source_bitrate() {
    use super::eligible_presets_for;

    // Without source dimensions the rung encodes at its reference bitrate:
    // 480p x 1000 kbps. An equal source bitrate leaves nothing to gain.
    let presets = vec![preset("480p", 480)];
    let mut state = SourceState {
        rungs: HashMap::new(),
        source_resolution: None,
        source_frame_rate: None,
        source_video_bitrate_kbps: Some(1000),
    };
    assert_eq!(eligible_presets_for(&presets, Some(&state)).len(), 0);

    // One kbps below the rung: it genuinely reduces the data rate again.
    state.source_video_bitrate_kbps = Some(1001);
    assert_eq!(eligible_presets_for(&presets, Some(&state)).len(), 1);
}

#[test]
fn bitrate_gate_keeps_the_ladder_for_a_healthy_source() {
    use super::eligible_presets_for;

    // A real 1080p60 publish at 8000 kbps: every rung still encodes at a
    // lower effective bitrate, so the whole ladder stays offered.
    let presets = vec![
        preset("1080p", 1080),
        preset("720p", 720),
        preset("480p", 480),
    ];
    let state = SourceState {
        rungs: HashMap::new(),
        source_resolution: Some((1920, 1080)),
        source_frame_rate: Some(60.0),
        source_video_bitrate_kbps: Some(8000),
    };
    assert_eq!(
        eligible_presets_for(&presets, Some(&state)).len(),
        3,
        "every rung steps the data rate down"
    );
}

fn lifecycle_test_manager() -> std::sync::Arc<super::TranscodeManager> {
    let (hub, _) = tokio::sync::mpsc::channel(1);
    lifecycle_manager_with_hub(hub, "ffmpeg")
}

fn lifecycle_manager_with_hub(
    hub: synctv_xiu::streamhub::define::StreamHubEventSender,
    binary: &str,
) -> std::sync::Arc<super::TranscodeManager> {
    use crate::relay::in_memory_registry::InMemoryStreamRegistry;

    let worker = super::FfmpegWorkerConfig {
        max_concurrent: 1,
        binary: binary.to_owned(),
        x264_preset: Default::default(),
    };
    let config = super::TranscodeConfig {
        enabled: true,
        workers: vec![super::WorkerConfig {
            name: None,
            priority: 100,
            engine: super::WorkerEngine::Ffmpeg(worker.clone()),
        }],
        presets: vec![preset("1080p", 1080), preset("480p", 480)],
    };
    assert!(config.validate().is_empty());
    super::TranscodeManager::new(
        config,
        vec![super::FfmpegWorker::new(&worker)],
        std::sync::Arc::new(InMemoryStreamRegistry::new()),
        hub,
        "node".to_string(),
        "127.0.0.1:1935".to_string(),
    )
}

#[tokio::test(start_paused = true)]
async fn dropping_manager_releases_background_tasks_in_a_live_runtime() {
    let (events, _) = tokio::sync::broadcast::channel(1);
    for _ in 0..2 {
        let manager = lifecycle_test_manager();
        let weak = std::sync::Arc::downgrade(&manager.inner);
        let tasks = manager.inner.tasks.clone();
        let watcher = manager.spawn_lifecycle_watcher(events.subscribe());
        tokio::task::yield_now().await;
        drop(manager);
        tokio::time::timeout(std::time::Duration::from_secs(1), tasks.wait())
            .await
            .expect("transcode tasks should stop");
        watcher.await.expect("lifecycle watcher should exit");
        assert!(weak.upgrade().is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn rejected_transcode_job_releases_metadata_observer() {
    use synctv_xiu::streamhub::define::{DataReceiver, FrameDataReceiver, StreamHubEvent};

    let (hub, mut events) = tokio::sync::mpsc::channel(1);
    let manager = lifecycle_manager_with_hub(hub, "./missing-test-ffmpeg");
    manager
        .inner
        .add_viewer("room", "media", "480p", super::ViewerRole::Waiting);
    let Some(StreamHubEvent::Subscribe { result_sender, .. }) = events.recv().await else {
        panic!("expected source subscription");
    };
    let (frames, receiver) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        result_sender
            .send(Ok((
                DataReceiver {
                    frame_receiver: Some(FrameDataReceiver::Unbounded(receiver)),
                    packet_receiver: None,
                },
                None
            )))
            .is_ok()
    );
    frames.closed().await;

    let inner = std::sync::Arc::downgrade(&manager.inner);
    manager.shutdown().await;
    drop(manager);
    assert!(inner.upgrade().is_none());
}

#[test]
fn last_viewer_preserves_source_metadata() {
    let manager = lifecycle_test_manager();
    let guard = manager.attach_rung_viewer("room", "media", "480p");
    let key = ("room".to_owned(), "media".to_owned());
    manager
        .inner
        .state
        .lock()
        .expect("state")
        .get_mut(&key)
        .expect("source")
        .source_resolution = Some((640, 360));
    drop(guard);
    let state = manager.inner.state.lock().expect("state");
    let source = state.get(&key).expect("metadata survives the last viewer");
    assert!(source.rungs.is_empty());
    assert_eq!(source.source_resolution, Some((640, 360)));
}

#[tokio::test(start_paused = true)]
async fn hls_master_keeps_rung_demand_while_resolving_generation() -> anyhow::Result<()> {
    use crate::{HlsStreamingApi, LiveStreamingInfrastructure, StreamTracker};
    use std::sync::Arc;

    let manager = lifecycle_test_manager();
    let generation = crate::util::TEST_GENERATION_ID;
    let rung = rung_stream_name("123", "480p");
    manager
        .inner
        .registry
        .try_activate_generation("room", &rung, "node", "", "127.0.0.1:1935", generation)
        .await?;
    // The scheduler sees the publisher before the HLS lookup observes it.
    let registry = crate::local_stream_registry();
    let infrastructure = LiveStreamingInfrastructure::new(
        registry.clone(),
        manager.inner.stream_hub_event_sender.clone(),
        Arc::new(StreamTracker::new()),
        "node".to_string(),
        synctv_common::ssrf::SsrfGuard::disabled(),
    )?
    .with_transcode(Some(Arc::clone(&manager)));
    let lookup =
        HlsStreamingApi::resolve_active_generation_with_pull(&infrastructure, "room", &rung, None);
    tokio::pin!(lookup);
    assert!(futures::poll!(&mut lookup).is_pending());
    let subscribers = || {
        let state = manager
            .inner
            .state
            .lock()
            .expect("transcode state poisoned");
        state
            .get(&("room".to_string(), "123".to_string()))
            .and_then(|source| source.rungs.get("480p"))
            .map_or(0, |rung| rung.subscribers)
    };
    assert_eq!(subscribers(), 1);
    registry
        .try_activate_generation("room", &rung, "node", "", "127.0.0.1:1935", generation)
        .await?;
    assert_eq!(
        lookup.await?.map(|stream| stream.generation_id),
        Some(generation.to_string())
    );
    assert!(
        manager
            .inner
            .state
            .lock()
            .expect("transcode state poisoned")
            .is_empty(),
        "the last viewer releases a source with no local metadata"
    );
    Ok(())
}

fn seed_lifecycle_source(manager: &super::TranscodeManager) {
    let mut state = manager
        .inner
        .state
        .lock()
        .expect("transcode state poisoned");
    state.insert(
        ("room".to_string(), "media".to_string()),
        SourceState {
            rungs: [("1080p".to_string(), waiting(RungState::default(), 2))]
                .into_iter()
                .collect(),
            // A previous 720p session's metadata.
            source_resolution: Some((1280, 720)),
            source_frame_rate: Some(30.0),
            source_video_bitrate_kbps: Some(2500),
        },
    );
}

#[test]
fn source_restart_resets_stale_session_metadata_but_keeps_demand() {
    let manager = lifecycle_test_manager();
    seed_lifecycle_source(&manager);

    // The stale 720p session blocks the 1080p rung: a fresh 1080p60
    // publish must not be judged against the earlier session's metadata.
    assert!(
        !manager
            .eligible_presets("room", "media")
            .iter()
            .any(|p| p.name == "1080p")
    );

    manager.note_source_started("room", "media");

    {
        let state = manager
            .inner
            .state
            .lock()
            .expect("transcode state poisoned");
        let source = state
            .get(&("room".to_string(), "media".to_string()))
            .expect("source entry survives a restart");
        assert_eq!(source.source_resolution, None);
        assert_eq!(source.source_frame_rate, None);
        assert_eq!(source.source_video_bitrate_kbps, None);
        assert_eq!(
            source.rungs.get("1080p").map(|r| r.waiting_viewers),
            Some(2),
            "waiting demand carries into the new session"
        );
    }
    assert!(
        manager
            .eligible_presets("room", "media")
            .iter()
            .any(|p| p.name == "1080p")
    );

    manager.note_source_ended("room", "media");
    let state = manager
        .inner
        .state
        .lock()
        .expect("transcode state poisoned");
    assert!(
        !state.contains_key(&("room".to_string(), "media".to_string())),
        "source end drops the bookkeeping entirely"
    );
}

#[tokio::test]
async fn lifecycle_watcher_follows_hub_publish_events() {
    use synctv_xiu::streamhub::define::{BroadcastEvent, PublishType};

    let manager = lifecycle_test_manager();
    seed_lifecycle_source(&manager);
    // This test runs inside a Tokio runtime, so the manager's scheduler is
    // live: zero out the seeded demand so a wake cannot try to subscribe to
    // a source that is not in the (absent) hub and legitimately clear the
    // entry under test.
    {
        let mut state = manager
            .inner
            .state
            .lock()
            .expect("transcode state poisoned");
        state
            .get_mut(&("room".to_string(), "media".to_string()))
            .expect("seeded source")
            .rungs
            .get_mut("1080p")
            .expect("seeded rung")
            .waiting_viewers = 0;
    }

    let (events_tx, events_rx) = tokio::sync::broadcast::channel::<BroadcastEvent>(8);
    let watcher = manager.spawn_lifecycle_watcher(events_rx);

    let identifier = |stream_name: &str| synctv_xiu::streamhub::stream::StreamIdentifier::Rtmp {
        app_name: "room".to_string(),
        stream_name: stream_name.to_string(),
    };

    // A new publish session resets the stale metadata...
    events_tx
        .send(BroadcastEvent::Publish {
            identifier: identifier("media"),
            pub_type: PublishType::RtmpPush,
            generation_id: Uuid::new(),
        })
        .expect("send publish");
    // ...while rung publications are no-ops on unrelated keys.
    events_tx
        .send(BroadcastEvent::Publish {
            identifier: identifier("media-480p"),
            pub_type: PublishType::Transcode,
            generation_id: Uuid::new(),
        })
        .expect("send rung publish");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let state = manager
            .inner
            .state
            .lock()
            .expect("transcode state poisoned");
        assert_eq!(state.len(), 1, "rung publications create no source state");
        let source = state
            .get(&("room".to_string(), "media".to_string()))
            .expect("source entry kept");
        assert_eq!(source.source_resolution, None);
        assert_eq!(source.source_video_bitrate_kbps, None);
    }

    events_tx
        .send(BroadcastEvent::UnPublish {
            identifier: identifier("media"),
            generation_id: Uuid::new(),
        })
        .expect("send unpublish");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let state = manager
            .inner
            .state
            .lock()
            .expect("transcode state poisoned");
        assert!(
            !state.contains_key(&("room".to_string(), "media".to_string())),
            "source unpublish clears the source state"
        );
    }

    watcher.abort();
}
