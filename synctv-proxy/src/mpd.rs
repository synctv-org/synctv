//! MPEG-DASH MPD XML generation from structured DASH data.
//!
//! Pure data → XML string conversion. No I/O, no async.

use std::fmt::Write;
use synctv_proto::dash::DashManifestData;

/// Options for MPD generation
pub struct MpdOptions<'a> {
    /// If set, rewrite `BaseURLs` to proxy paths relative to this base.
    /// Streams are indexed: video[0..N], then audio[N..M].
    pub proxy_base_url: Option<&'a str>,
    /// JWT token appended to proxy URLs for auth.
    pub token: Option<&'a str>,
}

/// Generate MPEG-DASH MPD XML from structured DASH data.
#[must_use] 
pub fn generate_mpd(data: &DashManifestData, opts: &MpdOptions<'_>) -> String {
    let mut xml = String::with_capacity(4096);

    // XML declaration
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");

    // MPD root element
    let duration_str = format_duration(data.duration);
    let min_buf_str = format_duration(data.min_buffer_time.max(1.5));

    let _ = writeln!(
        xml,
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
         profiles=\"urn:mpeg:dash:profile:isoff-on-demand:2011\" \
         type=\"static\" \
         mediaPresentationDuration=\"{duration_str}\" \
         minBufferTime=\"{min_buf_str}\">"
    );

    // Period
    xml.push_str("  <Period>\n");

    // Video AdaptationSet
    if !data.video_streams.is_empty() {
        xml.push_str("    <AdaptationSet mimeType=\"video/mp4\" segmentAlignment=\"true\" startWithSAP=\"1\">\n");

        for (idx, v) in data.video_streams.iter().enumerate() {
            let _ = writeln!(
                xml,
                "      <Representation id=\"{}\" codecs=\"{}\" width=\"{}\" height=\"{}\" \
                 frameRate=\"{}\" bandwidth=\"{}\" sar=\"{}\" startWithSAP=\"{}\">",
                xml_escape(&v.id),
                xml_escape(&v.codecs),
                v.width,
                v.height,
                xml_escape(&v.frame_rate),
                v.bandwidth,
                xml_escape(if v.sar.is_empty() { "1:1" } else { &v.sar }),
                v.start_with_sap,
            );

            write_base_url(&mut xml, idx, v.base_url.as_str(), opts);
            write_segment_base(&mut xml, &v.segment_base.initialization, &v.segment_base.index_range);

            xml.push_str("      </Representation>\n");
        }

        xml.push_str("    </AdaptationSet>\n");
    }

    // Audio AdaptationSet
    if !data.audio_streams.is_empty() {
        xml.push_str("    <AdaptationSet mimeType=\"audio/mp4\" segmentAlignment=\"true\" startWithSAP=\"1\">\n");

        let video_count = data.video_streams.len();
        for (idx, a) in data.audio_streams.iter().enumerate() {
            let stream_idx = video_count + idx;
            let _ = writeln!(
                xml,
                "      <Representation id=\"{}\" codecs=\"{}\" bandwidth=\"{}\" \
                 audioSamplingRate=\"{}\" startWithSAP=\"{}\">",
                xml_escape(&a.id),
                xml_escape(&a.codecs),
                a.bandwidth,
                a.audio_sampling_rate,
                a.start_with_sap,
            );

            write_base_url(&mut xml, stream_idx, a.base_url.as_str(), opts);
            write_segment_base(&mut xml, &a.segment_base.initialization, &a.segment_base.index_range);

            xml.push_str("      </Representation>\n");
        }

        xml.push_str("    </AdaptationSet>\n");
    }

    xml.push_str("  </Period>\n");
    xml.push_str("</MPD>\n");

    xml
}

/// Write `<BaseURL>` element — either a proxy path or the original CDN URL.
fn write_base_url(xml: &mut String, stream_idx: usize, cdn_url: &str, opts: &MpdOptions<'_>) {
    if let Some(base) = opts.proxy_base_url {
        let mut url = format!("{base}/stream/{stream_idx}");
        if let Some(token) = opts.token {
            // URL-encode the token to prevent query parameter injection
            let encoded_token = super::percent_encode(token);
            let _ = write!(url, "?token={encoded_token}");
        }
        let _ = writeln!(xml, "        <BaseURL>{}</BaseURL>", xml_escape(&url));
    } else {
        let _ = writeln!(xml, "        <BaseURL>{}</BaseURL>", xml_escape(cdn_url));
    }
}

/// Write `<SegmentBase>` with `indexRange` and `<Initialization>`.
fn write_segment_base(xml: &mut String, initialization: &str, index_range: &str) {
    if initialization.is_empty() && index_range.is_empty() {
        return;
    }
    let escaped_index_range = xml_escape(index_range);
    let escaped_initialization = xml_escape(initialization);
    let _ = write!(
        xml,
        "        <SegmentBase indexRange=\"{escaped_index_range}\">\n\
                   <Initialization range=\"{escaped_initialization}\"/>\n\
                 </SegmentBase>\n"
    );
}

/// Format seconds as ISO 8601 duration (e.g. `PT3M45.2S`).
fn format_duration(secs: f64) -> String {
    if secs <= 0.0 {
        return "PT0S".to_string();
    }
    let total_secs = secs;
    let hours = (total_secs / 3600.0).floor() as u64;
    let mins = ((total_secs % 3600.0) / 60.0).floor() as u64;
    let remaining = total_secs % 60.0;

    let mut s = String::from("PT");
    if hours > 0 {
        let _ = write!(s, "{hours}H");
    }
    if mins > 0 {
        let _ = write!(s, "{mins}M");
    }
    if remaining > 0.0 || (hours == 0 && mins == 0) {
        // Use up to 1 decimal place, trim trailing zero
        let formatted = format!("{remaining:.1}");
        let formatted = formatted.trim_end_matches('0').trim_end_matches('.');
        let _ = write!(s, "{formatted}S");
    }
    s
}

/// Minimal XML escaping for attribute/text content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_proto::dash::{DashAudioStream, DashSegmentBase, DashVideoStream};

    fn sample_data() -> DashManifestData {
        DashManifestData {
            duration: 225.5,
            min_buffer_time: 1.5,
            video_streams: vec![DashVideoStream {
                id: "480P 标清".to_string(),
                base_url: "https://cdn.bilibili.com/v1.m4s".to_string(),
                backup_urls: vec![],
                mime_type: "video/mp4".to_string(),
                codecs: "avc1.64001F".to_string(),
                width: 854,
                height: 480,
                frame_rate: "30".to_string(),
                bandwidth: 500_000,
                sar: "1:1".to_string(),
                start_with_sap: 1,
                segment_base: DashSegmentBase {
                    initialization: "0-926".to_string(),
                    index_range: "927-9286".to_string(),
                },
            }],
            audio_streams: vec![DashAudioStream {
                id: "audio_30280".to_string(),
                base_url: "https://cdn.bilibili.com/a1.m4s".to_string(),
                backup_urls: vec![],
                mime_type: "audio/mp4".to_string(),
                codecs: "mp4a.40.2".to_string(),
                bandwidth: 128_000,
                audio_sampling_rate: 44100,
                start_with_sap: 1,
                segment_base: DashSegmentBase {
                    initialization: "0-800".to_string(),
                    index_range: "801-5000".to_string(),
                },
            }],
        }
    }

    #[test]
    fn test_direct_mpd() {
        let data = sample_data();
        let opts = MpdOptions {
            proxy_base_url: None,
            token: None,
        };
        let mpd = generate_mpd(&data, &opts);
        assert!(mpd.contains("mediaPresentationDuration=\"PT3M45.5S\""));
        assert!(mpd.contains("https://cdn.bilibili.com/v1.m4s"));
        assert!(mpd.contains("indexRange=\"927-9286\""));
        assert!(mpd.contains("codecs=\"avc1.64001F\""));
        assert!(mpd.contains("codecs=\"mp4a.40.2\""));
    }

    #[test]
    fn test_proxy_mpd() {
        let data = sample_data();
        let opts = MpdOptions {
            proxy_base_url: Some("/api/providers/bilibili/proxy/room1/media1"),
            token: Some("jwt123"),
        };
        let mpd = generate_mpd(&data, &opts);
        assert!(mpd.contains("/api/providers/bilibili/proxy/room1/media1/stream/0?token=jwt123"));
        assert!(mpd.contains("/api/providers/bilibili/proxy/room1/media1/stream/1?token=jwt123"));
        // CDN URL should NOT appear
        assert!(!mpd.contains("https://cdn.bilibili.com"));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0.0), "PT0S");
        assert_eq!(format_duration(30.0), "PT30S");
        assert_eq!(format_duration(90.5), "PT1M30.5S");
        assert_eq!(format_duration(3661.0), "PT1H1M1S");
    }
}
