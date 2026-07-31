use std::collections::VecDeque;
use std::fmt::Write as _;

use chrono::{SecondsFormat, TimeZone as _, Utc};

const LIVE_WINDOW_SEGMENTS: usize = 6;
const DEFAULT_TARGET_DURATION_SECS: i64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    pub sequence: u64,
    pub duration_ms: i64,
    pub started_at_ms: i64,
    pub ts_name: String,
    pub discontinuity: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HlsPlaylist {
    pub segments: VecDeque<SegmentInfo>,
    ended: bool,
}

impl HlsPlaylist {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_segment(&mut self, segment: SegmentInfo) {
        self.segments.push_back(segment);
        self.prune();
    }

    fn prune(&mut self) {
        while self.segments.len() > LIVE_WINDOW_SEGMENTS {
            self.segments.pop_front();
        }
    }

    pub fn mark_ended(&mut self) {
        self.ended = true;
    }

    #[must_use]
    pub fn generate_m3u8<F>(&self, mut gen_ts_url: F) -> String
    where
        F: FnMut(&str) -> String,
    {
        let mut content = String::new();
        content.push_str("#EXTM3U\n#EXT-X-VERSION:3\n");

        let target_duration = self
            .segments
            .iter()
            .map(|segment| segment.duration_ms.max(0).saturating_add(999) / 1000)
            .max()
            .unwrap_or(DEFAULT_TARGET_DURATION_SECS);
        let _ = writeln!(content, "#EXT-X-TARGETDURATION:{target_duration}");
        let first_sequence = self.segments.front().map_or(0, |segment| segment.sequence);
        let _ = writeln!(content, "#EXT-X-MEDIA-SEQUENCE:{first_sequence}");

        for segment in &self.segments {
            if segment.discontinuity {
                content.push_str("#EXT-X-DISCONTINUITY\n");
            }
            if let Some(started_at) = Utc.timestamp_millis_opt(segment.started_at_ms).single() {
                let _ = writeln!(
                    content,
                    "#EXT-X-PROGRAM-DATE-TIME:{}",
                    started_at.to_rfc3339_opts(SecondsFormat::Millis, true)
                );
            }
            let duration =
                std::time::Duration::from_millis(segment.duration_ms.max(0).cast_unsigned())
                    .as_secs_f64();
            let _ = writeln!(content, "#EXTINF:{duration:.3},");
            content.push_str(&gen_ts_url(&segment.ts_name));
            content.push('\n');
        }

        if self.ended {
            content.push_str("#EXT-X-ENDLIST\n");
        }

        content
    }
}
