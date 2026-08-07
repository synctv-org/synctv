use synctv_xiu::hls::{HlsPlaylist, SegmentInfo, StreamProcessorState};
use synctv_xiu::streamhub::utils::Uuid;

fn segment(sequence: u64, start_ms: i64) -> SegmentInfo {
    SegmentInfo {
        sequence,
        duration_ms: 5_000,
        started_at_ms: 1_700_000_000_000 + start_ms,
        ts_name: format!("segment-{sequence}"),
        discontinuity: false,
    }
}

fn state(playlist: HlsPlaylist) -> StreamProcessorState {
    StreamProcessorState {
        app_name: "room".to_string(),
        stream_name: "media".to_string(),
        playlist,
        generation_id: Uuid::new(),
        marked_for_cleanup: false,
        cleanup_segment_names: Vec::new(),
    }
}

#[test]
fn live_playlist_generates_media_sequence() {
    let mut playlist = HlsPlaylist::new();
    for sequence in 0..3 {
        playlist.push_segment(segment(sequence, sequence.cast_signed() * 5_000));
    }

    let playlist = state(playlist).generate_m3u8(|name| format!("/hls/{name}.ts"));

    assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n"));
    assert!(playlist.contains("#EXT-X-TARGETDURATION:5"));
    assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
    assert!(playlist.contains("#EXT-X-PROGRAM-DATE-TIME:"));
    assert!(playlist.contains("#EXTINF:5.000,"));
    assert!(playlist.contains("/hls/segment-2.ts"));
}

#[test]
fn empty_playlist_has_valid_live_defaults() {
    let playlist = HlsPlaylist::new().generate_m3u8(|name| format!("/hls/{name}.ts"));

    assert!(playlist.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n"));
    assert!(playlist.contains("#EXT-X-TARGETDURATION:10"));
    assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
    assert!(!playlist.contains("#EXT-X-ENDLIST"));
}

#[test]
fn playlist_places_discontinuity_before_the_affected_segment() {
    let mut playlist = HlsPlaylist::new();
    playlist.push_segment(segment(0, 0));
    playlist.push_segment(SegmentInfo {
        discontinuity: true,
        ..segment(1, 5_000)
    });

    let m3u8 = playlist.generate_m3u8(|name| format!("/hls/{name}.ts"));
    let discontinuity = m3u8
        .find("#EXT-X-DISCONTINUITY")
        .expect("discontinuity marker should be generated");
    let affected_segment = m3u8
        .find("/hls/segment-1.ts")
        .expect("affected segment URL should be generated");

    assert!(discontinuity < affected_segment);
}

#[test]
fn playlist_uses_max_duration_and_preserves_millisecond_precision() {
    let mut playlist = HlsPlaylist::new();
    playlist.push_segment(SegmentInfo {
        duration_ms: 5_123,
        ..segment(0, 0)
    });
    playlist.push_segment(SegmentInfo {
        duration_ms: 7_001,
        ..segment(1, 5_123)
    });

    let m3u8 = playlist.generate_m3u8(|name| format!("/hls/{name}.ts"));

    assert!(m3u8.contains("#EXT-X-TARGETDURATION:8"));
    assert!(m3u8.contains("#EXTINF:5.123,"));
    assert!(m3u8.contains("#EXTINF:7.001,"));
}

#[test]
fn playlist_delegates_url_encoding_to_the_url_generator() {
    let mut playlist = HlsPlaylist::new();
    playlist.push_segment(SegmentInfo {
        ts_name: "segment 0.ts".to_string(),
        ..segment(0, 0)
    });

    let m3u8 = playlist.generate_m3u8(|name| format!("/hls/{}", name.replace(' ', "%20")));

    assert!(m3u8.contains("/hls/segment%200.ts"));
}

#[test]
fn playlist_preserves_nonzero_sequence_after_window_rollover() {
    let mut playlist = HlsPlaylist::new();
    for sequence in 100..107 {
        playlist.push_segment(segment(sequence, (sequence - 100).cast_signed() * 5_000));
    }

    let m3u8 = playlist.generate_m3u8(|name| format!("/hls/{name}.ts"));

    assert!(m3u8.contains("#EXT-X-MEDIA-SEQUENCE:101"));
    assert!(!m3u8.contains("/hls/segment-100.ts"));
    assert!(m3u8.contains("/hls/segment-106.ts"));
}

#[test]
fn ended_playlist_stops_at_the_final_segment() {
    let mut playlist = HlsPlaylist::new();
    playlist.push_segment(segment(0, 0));

    let live_playlist = playlist.generate_m3u8(|name| format!("/hls/{name}.ts"));
    assert!(!live_playlist.contains("#EXT-X-ENDLIST"));

    playlist.mark_ended();
    let ended_playlist = playlist.generate_m3u8(|name| format!("/hls/{name}.ts"));
    assert!(ended_playlist.ends_with("#EXT-X-ENDLIST\n"));
}

#[test]
fn playlist_window_prunes_metadata_without_deciding_storage_retention() {
    let mut playlist = HlsPlaylist::new();
    for sequence in 0..7 {
        playlist.push_segment(segment(sequence, sequence.cast_signed() * 5_000));
    }

    assert_eq!(playlist.segments.len(), 6);
    assert_eq!(playlist.segments.front().map(|item| item.sequence), Some(1));
}
