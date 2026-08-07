use url::Url;

use crate::{Error, Result};

pub(super) fn parse_hls_media_duration(manifest: &str) -> Result<Option<f64>> {
    let mut total = 0.0;
    let mut found = false;
    for line in manifest.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("#EXTINF:") else {
            continue;
        };
        let value = rest.split(',').next().unwrap_or_default().trim();
        let duration = value.parse::<f64>().map_err(|error| {
            Error::InvalidInput(format!("invalid HLS EXTINF duration: {error}"))
        })?;
        if !duration.is_finite() || duration < 0.0 {
            return Err(Error::InvalidInput(
                "invalid HLS EXTINF duration".to_string(),
            ));
        }
        found = true;
        total += duration;
    }

    Ok((found && total > 0.0).then_some(total))
}

pub(super) fn first_hls_variant_url(base_url: &str, manifest: &str) -> Result<String> {
    let mut next_uri_is_variant = false;
    for line in manifest.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-STREAM-INF") {
            next_uri_is_variant = true;
            continue;
        }
        if next_uri_is_variant && !line.starts_with('#') {
            return resolve_relative_url(base_url, line);
        }
    }
    Err(Error::InvalidInput(
        "HLS master manifest does not contain a variant URI".to_string(),
    ))
}

fn resolve_relative_url(base_url: &str, uri: &str) -> Result<String> {
    Url::parse(base_url)
        .and_then(|base| base.join(uri))
        .map(|url| url.to_string())
        .map_err(|error| Error::InvalidInput(format!("invalid manifest URI: {error}")))
}

pub(super) fn parse_dash_duration(manifest: &str) -> Option<f64> {
    let attr = find_xml_attr(manifest, "mediaPresentationDuration")?;
    parse_iso8601_duration_seconds(attr)
}

fn find_xml_attr<'a>(text: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("{attr}=\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn parse_iso8601_duration_seconds(value: &str) -> Option<f64> {
    let value = value.strip_prefix('P')?;
    let mut number = String::new();
    let mut seconds = 0.0;
    let mut in_time = false;

    for ch in value.chars() {
        match ch {
            'T' => in_time = true,
            '0'..='9' | '.' => number.push(ch),
            'D' => {
                seconds += take_duration_number(&mut number)? * 86_400.0;
            }
            'H' if in_time => {
                seconds += take_duration_number(&mut number)? * 3_600.0;
            }
            'M' if in_time => {
                seconds += take_duration_number(&mut number)? * 60.0;
            }
            'S' if in_time => {
                seconds += take_duration_number(&mut number)?;
            }
            _ => return None,
        }
    }

    (number.is_empty() && seconds > 0.0 && seconds.is_finite()).then_some(seconds)
}

fn take_duration_number(number: &mut String) -> Option<f64> {
    let value = number.parse().ok()?;
    number.clear();
    Some(value)
}

pub(super) fn parse_mp4_duration(bytes: &[u8]) -> Option<f64> {
    const MOOV: [u8; 4] = *b"moov";
    const MVHD: [u8; 4] = *b"mvhd";

    let mut offset = 0_usize;
    while offset + 8 <= bytes.len() {
        let header = Mp4BoxHeader::parse(bytes, offset)?;
        let body_start = header.header_end;
        let body_end = header.end.min(bytes.len());
        match header.name {
            MOOV => {
                if let Some(duration) = parse_mp4_duration(&bytes[body_start..body_end]) {
                    return Some(duration);
                }
            }
            MVHD => {
                return parse_mvhd_duration(&bytes[body_start..body_end]);
            }
            _ => {}
        }
        if header.end <= offset {
            return None;
        }
        offset = header.end;
    }
    None
}

struct Mp4BoxHeader {
    name: [u8; 4],
    header_end: usize,
    end: usize,
}

impl Mp4BoxHeader {
    fn parse(bytes: &[u8], offset: usize) -> Option<Self> {
        let size32 = u64::from(read_u32(bytes, offset)?);
        let name = bytes.get(offset + 4..offset + 8)?.try_into().ok()?;
        let (size, header_len) = if size32 == 1 {
            (read_u64(bytes, offset + 8)?, 16_usize)
        } else if size32 == 0 {
            ((bytes.len() - offset) as u64, 8_usize)
        } else {
            (size32, 8_usize)
        };
        if size < header_len as u64 {
            return None;
        }
        let end = offset.checked_add(usize::try_from(size).ok()?)?;
        Some(Self {
            name,
            header_end: offset + header_len,
            end,
        })
    }
}

fn parse_mvhd_duration(bytes: &[u8]) -> Option<f64> {
    let version = *bytes.first()?;
    match version {
        0 => {
            let timescale = f64::from(read_u32(bytes, 12)?);
            let duration = f64::from(read_u32(bytes, 16)?);
            valid_scaled_duration(duration, timescale)
        }
        1 => {
            let timescale = f64::from(read_u32(bytes, 20)?);
            let duration = u64_to_f64(read_u64(bytes, 24)?);
            valid_scaled_duration(duration, timescale)
        }
        _ => None,
    }
}

#[allow(clippy::cast_precision_loss)]
fn u64_to_f64(value: u64) -> f64 {
    value as f64
}

fn valid_scaled_duration(duration: f64, timescale: f64) -> Option<f64> {
    if duration.is_finite() && timescale.is_finite() && duration > 0.0 && timescale > 0.0 {
        Some(duration / timescale)
    } else {
        None
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::TestResultExt;

    #[test]
    fn hls_media_duration_sums_extinf_segments() {
        let manifest = "#EXTM3U\n#EXTINF:4.5,\nsegment-1.ts\n#EXTINF:5.5,\nsegment-2.ts\n";

        assert_eq!(
            parse_hls_media_duration(manifest).checked("HLS duration should parse"),
            Some(10.0)
        );
    }

    #[test]
    fn hls_master_variant_resolves_relative_uri() {
        let manifest = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1280000\nvariants/main.m3u8\n";

        assert_eq!(
            first_hls_variant_url("https://media.example.test/live/master.m3u8", manifest)
                .checked("HLS variant should resolve"),
            "https://media.example.test/live/variants/main.m3u8"
        );
    }

    #[test]
    fn dash_duration_parses_iso8601_presentation_duration() {
        let manifest = r#"<MPD mediaPresentationDuration="PT1H2M3.5S"></MPD>"#;

        assert_eq!(parse_dash_duration(manifest), Some(3723.5));
    }

    #[test]
    fn mp4_duration_reads_mvhd_inside_moov() {
        let mut mvhd_payload = vec![0_u8; 20];
        mvhd_payload[12..16].copy_from_slice(&1000_u32.to_be_bytes());
        mvhd_payload[16..20].copy_from_slice(&90_000_u32.to_be_bytes());
        let bytes = mp4_box(*b"moov", &mp4_box(*b"mvhd", &mvhd_payload));

        assert_eq!(parse_mp4_duration(&bytes), Some(90.0));
    }

    fn mp4_box(name: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size =
            u32::try_from(8 + payload.len()).checked("test MP4 box should fit in 32-bit size");
        let mut bytes = Vec::with_capacity(size as usize);
        bytes.extend_from_slice(&size.to_be_bytes());
        bytes.extend_from_slice(&name);
        bytes.extend_from_slice(payload);
        bytes
    }
}
