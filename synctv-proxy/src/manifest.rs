/// Default maximum number of URLs that can be rewritten in a single M3U8 playlist.
/// This prevents abuse via extremely large playlists that could cause memory
/// exhaustion or excessive proxy traffic.
pub const MAX_M3U8_URLS: usize = 1000;

/// Rewrite URLs inside an M3U8 playlist so they proxy through the server.
///
/// # Limits
/// - Maximum 1000 URLs per playlist by default (prevents abuse)
/// - Pass `max_urls` to override the default limit
///
/// # Security
/// - Returns an error if proxy_base contains line breaks (prevents response injection)
pub fn rewrite_m3u8(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
) -> Result<String, anyhow::Error> {
    rewrite_m3u8_with_limit(m3u8, source_url, proxy_base, None)
}

/// Rewrite URLs inside an M3U8 playlist with a custom URL limit.
///
/// # Arguments
/// * `m3u8` - The M3U8 playlist content
/// * `source_url` - The original URL of the playlist (for resolving relative URLs)
/// * `proxy_base` - The base URL for proxying
/// * `max_urls` - Optional maximum number of URLs to rewrite (defaults to MAX_M3U8_URLS)
///
/// # Security
/// - Returns an error if proxy_base contains line breaks (prevents response injection)
pub fn rewrite_m3u8_with_limit(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
    max_urls: Option<usize>,
) -> Result<String, anyhow::Error> {
    if proxy_base.contains('\n') || proxy_base.contains('\r') {
        return Err(anyhow::anyhow!(
            "proxy_base contains line break characters, refusing to rewrite M3U8"
        ));
    }

    let max_urls = max_urls.unwrap_or(MAX_M3U8_URLS);
    let base = url::Url::parse(source_url).ok();
    let mut output = String::with_capacity(m3u8.len());
    let mut url_count = 0usize;

    let is_vod = m3u8.contains("#EXT-X-ENDLIST");

    for line in m3u8.lines() {
        if line.starts_with('#') {
            let (rewritten_line, count) =
                rewrite_uri_attribute_with_count(line, base.as_ref(), proxy_base);
            url_count += count;
            output.push_str(&rewritten_line);
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
                let absolute = make_absolute(trimmed, base.as_ref());
                let separator = if proxy_base.contains('?') { '&' } else { '?' };
                let proxied = format!("{}{separator}url={}", proxy_base, percent_encode(&absolute));
                output.push_str(&proxied);
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

/// Resolve a possibly-relative URL to absolute using the given base URL.
#[must_use]
pub fn make_absolute(raw: &str, base: Option<&url::Url>) -> String {
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

/// Rewrite any `URI="..."` values found in an M3U8 tag line.
/// Returns the rewritten line and the count of URLs rewritten.
#[must_use]
pub fn rewrite_uri_attribute_with_count(
    line: &str,
    base: Option<&url::Url>,
    proxy_base: &str,
) -> (String, usize) {
    let pattern = "URI=\"";
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;
    let mut count = 0usize;

    while let Some(start) = remaining.find(pattern) {
        result.push_str(&remaining[..start + pattern.len()]);
        remaining = &remaining[start + pattern.len()..];

        if let Some(end) = remaining.find('"') {
            let uri = &remaining[..end];
            let absolute = make_absolute(uri, base);
            let separator = if proxy_base.contains('?') { '&' } else { '?' };
            let proxied = format!("{}{separator}url={}", proxy_base, percent_encode(&absolute));
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
