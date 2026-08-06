/// Default maximum number of URLs that can be rewritten in a single M3U8 playlist.
/// This prevents abuse via extremely large playlists that could cause memory
/// exhaustion or excessive proxy traffic.
pub const MAX_M3U8_URLS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsResourceKind {
    /// A child multivariant or media playlist.
    Manifest,
    /// A complete media segment referenced by a media playlist URI line.
    Segment,
    /// A low-latency partial segment or part preload hint.
    Part,
    /// Media-encryption key material.
    Key,
    /// A media initialization section, including map preload hints.
    Init,
    /// Session data, content steering, and other URI-bearing metadata.
    Auxiliary,
}

/// The lifecycle class of an HLS playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsPlaylistKind {
    /// A multivariant playlist that points to child playlists.
    Master,
    /// A rolling media playlist whose segment window changes over time.
    LiveMedia,
    /// An append-only event media playlist.
    EventMedia,
    /// A complete media playlist, including an ended live playlist.
    VodMedia,
}

/// Classifies an HLS playlist by tags that define its update lifecycle.
#[must_use]
pub fn classify_hls_playlist(m3u8: &str) -> HlsPlaylistKind {
    let mut is_master = false;
    let mut is_event = false;
    let mut is_vod = false;
    let mut has_endlist = false;

    for line in m3u8.lines().map(str::trim) {
        is_master |= is_master_playlist_tag(line);
        has_endlist |= line == "#EXT-X-ENDLIST";
        if let Some(value) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            is_event |= value.trim().eq_ignore_ascii_case("EVENT");
            is_vod |= value.trim().eq_ignore_ascii_case("VOD");
        }
    }

    if is_master {
        HlsPlaylistKind::Master
    } else if is_event {
        HlsPlaylistKind::EventMedia
    } else if is_vod || has_endlist {
        HlsPlaylistKind::VodMedia
    } else {
        HlsPlaylistKind::LiveMedia
    }
}

/// Rewrite URLs inside an M3U8 playlist so they proxy through the server.
///
/// # Limits
/// - Maximum 1000 URLs per playlist by default (prevents abuse)
/// - Pass `max_urls` to override the default limit
///
/// # Security
/// - Returns an error if `proxy_base` contains line breaks (prevents response injection)
///
/// # Errors
///
/// Returns an error when a playlist URL cannot be parsed or a generated proxy URL
/// would be unsafe.
pub fn rewrite_m3u8(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
) -> Result<String, anyhow::Error> {
    rewrite_m3u8_with_limit_and_mapper(
        m3u8,
        source_url,
        proxy_base,
        None,
        |proxy_base, target_url, _kind| default_proxy_url(proxy_base, target_url),
    )
}

pub fn rewrite_m3u8_with_url_mapper<F>(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
    proxy_url_for_target: F,
) -> Result<String, anyhow::Error>
where
    F: Fn(&str, &str) -> String,
{
    rewrite_m3u8_with_limit_and_mapper(
        m3u8,
        source_url,
        proxy_base,
        None,
        |proxy_base, target_url, _kind| proxy_url_for_target(proxy_base, target_url),
    )
}

pub fn rewrite_m3u8_with_typed_url_mapper<F>(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
    proxy_url_for_target: F,
) -> Result<String, anyhow::Error>
where
    F: Fn(&str, &str, HlsResourceKind) -> String,
{
    rewrite_m3u8_with_limit_and_mapper(m3u8, source_url, proxy_base, None, proxy_url_for_target)
}

fn rewrite_m3u8_with_limit_and_mapper<F>(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
    max_urls: Option<usize>,
    proxy_url_for_target: F,
) -> Result<String, anyhow::Error>
where
    F: Fn(&str, &str, HlsResourceKind) -> String,
{
    if proxy_base.contains('\n') || proxy_base.contains('\r') {
        return Err(anyhow::anyhow!(
            "proxy_base contains line break characters, refusing to rewrite M3U8"
        ));
    }

    let max_urls = max_urls.unwrap_or(MAX_M3U8_URLS);
    let base = match url::Url::parse(source_url) {
        Ok(base) => Some(base),
        Err(error) => {
            tracing::debug!(
                source_url,
                %error,
                "M3U8 source URL is not absolute; relative entries will stay relative"
            );
            None
        }
    };
    let mut output = String::with_capacity(m3u8.len());
    let mut url_count = 0usize;

    let is_vod = m3u8.contains("#EXT-X-ENDLIST");
    let mut next_uri_is_manifest = false;

    for line in m3u8.lines() {
        if line.starts_with('#') {
            let (rewritten_line, count) = rewrite_uri_attribute_with_mapper(
                line,
                base.as_ref(),
                proxy_base,
                &proxy_url_for_target,
            );
            url_count += count;
            output.push_str(&rewritten_line);
            next_uri_is_manifest = line.starts_with("#EXT-X-STREAM-INF:");
        } else {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                output.push_str(line);
            } else {
                url_count += 1;
                if url_count > max_urls {
                    tracing::warn!(
                        source_url = %source_url,
                        url_count = url_count,
                        max = max_urls,
                        is_vod = is_vod,
                        "M3U8 playlist exceeded maximum URL limit, truncating"
                    );
                    if is_vod {
                        output.push_str("#EXT-X-ENDLIST\n");
                    }
                    break;
                }
                let absolute = strip_hls_tag_query_params(&make_absolute(trimmed, base.as_ref()));
                let kind = if next_uri_is_manifest {
                    HlsResourceKind::Manifest
                } else {
                    HlsResourceKind::Segment
                };
                let proxied = proxy_url_for_target(proxy_base, &absolute, kind);
                output.push_str(&proxied);
                next_uri_is_manifest = false;
            }
        }
        output.push('\n');
    }

    if url_count > max_urls / 2 {
        tracing::info!(
            source_url = %source_url,
            url_count = url_count,
            "M3U8 playlist has many URLs"
        );
    }

    Ok(output)
}

#[must_use]
pub(crate) fn default_proxy_url(proxy_base: &str, target_url: &str) -> String {
    let separator = if proxy_base.contains('?') { '&' } else { '?' };
    format!(
        "{}{separator}url={}",
        proxy_base,
        percent_encode(target_url)
    )
}

/// Resolve a possibly-relative URL to absolute using the given base URL.
#[must_use]
pub(crate) fn make_absolute(raw: &str, base: Option<&url::Url>) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_string();
    }
    if let Some(base) = base {
        if let Ok(joined) = base.join(raw) {
            return joined.to_string();
        }
    }
    raw.to_string()
}

fn strip_hls_tag_query_params(raw: &str) -> String {
    let Some((before_fragment, fragment)) = raw.split_once('#') else {
        return strip_hls_tag_query_params_before_fragment(raw);
    };
    format!(
        "{}#{fragment}",
        strip_hls_tag_query_params_before_fragment(before_fragment)
    )
}

fn strip_hls_tag_query_params_before_fragment(raw: &str) -> String {
    let Some((base, query)) = raw.split_once('?') else {
        return raw.to_string();
    };
    let filtered = query
        .split('&')
        .filter(|part| {
            !part
                .split_once('=')
                .map_or(*part, |(key, _)| key)
                .eq_ignore_ascii_case("EXTINF")
        })
        .collect::<Vec<_>>()
        .join("&");
    if filtered.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{filtered}")
    }
}

/// Rewrite any `URI="..."` values found in an M3U8 tag line.
/// Returns the rewritten line and the count of URLs rewritten.
#[must_use]
#[cfg(test)]
pub(crate) fn rewrite_uri_attribute_with_count(
    line: &str,
    base: Option<&url::Url>,
    proxy_base: &str,
) -> (String, usize) {
    rewrite_uri_attribute_with_mapper(line, base, proxy_base, |proxy_base, target_url, _kind| {
        default_proxy_url(proxy_base, target_url)
    })
}

fn rewrite_uri_attribute_with_mapper<F>(
    line: &str,
    base: Option<&url::Url>,
    proxy_base: &str,
    proxy_url_for_target: F,
) -> (String, usize)
where
    F: Fn(&str, &str, HlsResourceKind) -> String,
{
    let pattern = "URI=\"";
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;
    let mut count = 0usize;
    let kind = hls_tag_resource_kind(line);

    while let Some(start) = remaining.find(pattern) {
        result.push_str(&remaining[..start + pattern.len()]);
        remaining = &remaining[start + pattern.len()..];

        if let Some(end) = remaining.find('"') {
            let uri = &remaining[..end];
            let absolute = strip_hls_tag_query_params(&make_absolute(uri, base));
            let proxied = proxy_url_for_target(proxy_base, &absolute, kind);
            result.push_str(&proxied);
            result.push('"');
            remaining = &remaining[end + 1..];
            count += 1;
        } else {
            result.push_str(remaining);
            remaining = "";
        }
    }

    result.push_str(remaining);
    (result, count)
}

fn hls_tag_resource_kind(line: &str) -> HlsResourceKind {
    if line.starts_with("#EXT-X-MEDIA:")
        || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
        || line.starts_with("#EXT-X-IMAGE-STREAM-INF:")
        || line.starts_with("#EXT-X-RENDITION-REPORT:")
    {
        HlsResourceKind::Manifest
    } else if line.starts_with("#EXT-X-PART:")
        || (line.starts_with("#EXT-X-PRELOAD-HINT:")
            && hls_attribute_value(line, "TYPE")
                .is_some_and(|value| value.eq_ignore_ascii_case("PART")))
    {
        HlsResourceKind::Part
    } else if line.starts_with("#EXT-X-KEY:") || line.starts_with("#EXT-X-SESSION-KEY:") {
        HlsResourceKind::Key
    } else if line.starts_with("#EXT-X-MAP:")
        || (line.starts_with("#EXT-X-PRELOAD-HINT:")
            && hls_attribute_value(line, "TYPE")
                .is_some_and(|value| value.eq_ignore_ascii_case("MAP")))
    {
        HlsResourceKind::Init
    } else {
        HlsResourceKind::Auxiliary
    }
}

fn is_master_playlist_tag(line: &str) -> bool {
    line.starts_with("#EXT-X-STREAM-INF:")
        || line.starts_with("#EXT-X-MEDIA:")
        || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
        || line.starts_with("#EXT-X-IMAGE-STREAM-INF:")
        || line.starts_with("#EXT-X-SESSION-DATA:")
        || line.starts_with("#EXT-X-SESSION-KEY:")
        || line.starts_with("#EXT-X-CONTENT-STEERING:")
}

fn hls_attribute_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let attributes = line.split_once(':')?.1;
    let mut field_start = 0;
    let mut quoted = false;

    for (index, byte) in attributes.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                if let Some(value) =
                    hls_attribute_field_value(&attributes[field_start..index], name)
                {
                    return Some(value);
                }
                field_start = index + 1;
            }
            _ => {}
        }
    }

    hls_attribute_field_value(&attributes[field_start..], name)
}

fn hls_attribute_field_value<'a>(field: &'a str, name: &str) -> Option<&'a str> {
    let (key, value) = field.trim().split_once('=')?;
    key.eq_ignore_ascii_case(name)
        .then(|| value.trim().trim_matches('"'))
}

/// Percent-encode a string for use in URL query parameter values.
///
/// This function first decodes any existing percent-encoded sequences, then
/// re-encodes the result. This prevents double-encoding bugs where `%20`
/// would become `%2520`.
///
/// Uses the `NON_ALPHANUMERIC` encode set, which encodes everything except
/// `A-Z a-z 0-9` (the RFC 3986 "unreserved" alphanumeric characters).
/// Note: Unlike strict RFC 3986, this also encodes `-`, `_`, `.`, and `~`
/// to ensure consistent encoding for query parameter values.
#[must_use]
pub fn percent_encode(input: &str) -> String {
    let decoded = percent_encoding::percent_decode_str(input).collect::<Vec<u8>>();
    percent_encoding::percent_encode(&decoded, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod typed_resource_tests {
    use super::*;

    #[test]
    fn classifies_hls_uri_roles_from_tags() {
        let playlist = concat!(
            "#EXTM3U\n",
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",URI=\"audio/playlist\"\n",
            "#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1000,URI=\"iframe/playlist\"\n",
            "#EXT-X-RENDITION-REPORT:URI=\"../alternate\",LAST-MSN=10\n",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"keys/current\"\n",
            "#EXT-X-MAP:URI=\"init/current\"\n",
            "#EXT-X-PART:DURATION=1.0,URI=\"parts/100.1\"\n",
            "#EXT-X-PRELOAD-HINT:URI=\"parts/next,variant\",TYPE=PART\n",
            "#EXT-X-PRELOAD-HINT:URI=\"init/next.mp4\",TYPE=MAP\n",
            "#EXT-X-SESSION-DATA:DATA-ID=\"com.example.meta\",URI=\"metadata/session.json\"\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=2000\n",
            "video/high\n",
            "#EXTINF:6,\n",
            "segments/100\n",
        );

        let rewritten = rewrite_m3u8_with_typed_url_mapper(
            playlist,
            "https://cdn.example.com/master/index",
            "/proxy",
            |_, target, kind| format!("/{kind:?}?target={target}"),
        )
        .expect("HLS playlist should rewrite");

        assert!(rewritten
            .contains("URI=\"/Manifest?target=https://cdn.example.com/master/audio/playlist\""));
        assert!(rewritten
            .contains("URI=\"/Manifest?target=https://cdn.example.com/master/iframe/playlist\""));
        assert!(rewritten.contains("URI=\"/Manifest?target=https://cdn.example.com/alternate\""));
        assert!(rewritten.contains("/Manifest?target=https://cdn.example.com/master/video/high"));
        assert!(
            rewritten.contains("URI=\"/Key?target=https://cdn.example.com/master/keys/current\"")
        );
        assert!(
            rewritten.contains("URI=\"/Init?target=https://cdn.example.com/master/init/current\"")
        );
        assert!(
            rewritten.contains("URI=\"/Part?target=https://cdn.example.com/master/parts/100.1\"")
        );
        assert!(rewritten
            .contains("URI=\"/Part?target=https://cdn.example.com/master/parts/next,variant\""));
        assert!(
            rewritten.contains("URI=\"/Init?target=https://cdn.example.com/master/init/next.mp4\"")
        );
        assert!(rewritten.contains(
            "URI=\"/Auxiliary?target=https://cdn.example.com/master/metadata/session.json\""
        ));
        assert!(rewritten.contains("/Segment?target=https://cdn.example.com/master/segments/100"));
    }

    #[test]
    fn classifies_session_keys_separately_from_media_segments() {
        let playlist = concat!(
            "#EXTM3U\n",
            "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"keys/session\"\n",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"keys/current\"\n",
            "#EXTINF:6,\n",
            "segments/100.ts\n",
        );

        let rewritten = rewrite_m3u8_with_typed_url_mapper(
            playlist,
            "https://cdn.example.com/live/index.m3u8",
            "/proxy",
            |_, target, kind| format!("/{kind:?}?target={target}"),
        )
        .expect("HLS playlist should rewrite");

        assert!(rewritten.contains("URI=\"/Key?target=https://cdn.example.com/live/keys/session\""));
        assert!(rewritten.contains("URI=\"/Key?target=https://cdn.example.com/live/keys/current\""));
        assert!(rewritten.contains("/Segment?target=https://cdn.example.com/live/segments/100.ts"));
    }

    #[test]
    fn classifies_master_live_event_and_vod_playlists() {
        let master = concat!(
            "#EXTM3U\n",
            "#EXT-X-SESSION-DATA:DATA-ID=\"com.example.title\",VALUE=\"Example\"\n",
            "#EXT-X-STREAM-INF:BANDWIDTH=1280000\n",
            "video/main.m3u8\n",
        );
        let live = concat!(
            "#EXTM3U\n",
            "#EXT-X-TARGETDURATION:6\n",
            "#EXT-X-MEDIA-SEQUENCE:1200\n",
            "#EXTINF:6,\n",
            "segment-1200.ts\n",
        );
        let event = concat!(
            "#EXTM3U\n",
            "#EXT-X-PLAYLIST-TYPE:EVENT\n",
            "#EXTINF:6,\n",
            "segment-1.ts\n",
            "#EXT-X-ENDLIST\n",
        );
        let vod = concat!(
            "#EXTM3U\n",
            "#EXT-X-PLAYLIST-TYPE:VOD\n",
            "#EXTINF:120,\n",
            "movie.ts\n",
        );
        let ended_live = "#EXTM3U\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n";

        assert_eq!(classify_hls_playlist(master), HlsPlaylistKind::Master);
        assert_eq!(classify_hls_playlist(live), HlsPlaylistKind::LiveMedia);
        assert_eq!(classify_hls_playlist(event), HlsPlaylistKind::EventMedia);
        assert_eq!(classify_hls_playlist(vod), HlsPlaylistKind::VodMedia);
        assert_eq!(classify_hls_playlist(ended_live), HlsPlaylistKind::VodMedia);
    }
}
