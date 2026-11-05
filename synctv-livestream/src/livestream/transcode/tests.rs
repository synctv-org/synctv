//! Scheduler and metadata unit tests.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use synctv_xiu::flv::amf0::{amf0_reader::Amf0Reader, amf0_writer::Amf0Writer, Amf0ValueType};
use synctv_xiu::streamhub::utils::Uuid;

use super::{
    parse_metadata_resolution, pick_most_demanded, rung_stream_name, split_rung_stream_name,
    RungState, SourceState, TranscodePreset,
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
fn metadata_resolution_parses_real_amf_ecma_array() {
    let mut properties = IndexMap::new();
    properties.insert("width".to_string(), Amf0ValueType::Number(1920.0));
    properties.insert("height".to_string(), Amf0ValueType::Number(1080.0));
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
    assert_eq!(parse_metadata_resolution(&body), Some((1920, 1080)));

    assert_eq!(parse_metadata_resolution(b""), None);
    assert_eq!(parse_metadata_resolution(b"not amf at all"), None);
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

    // A smaller source keeps only the rungs strictly below it.
    state.source_resolution = Some((854, 480));
    state.rungs.clear();
    let offered = eligible_presets_for(&presets, Some(&state));
    assert_eq!(offered.len(), 0, "no preset sits below a 480p source");
}
