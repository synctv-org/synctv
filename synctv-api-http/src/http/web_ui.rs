use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

const PROVIDER_VERIFICATION_PAGE: &str = "provider_verification.html";
const PROVIDER_VERIFICATION_CSP: &str = "default-src 'none'; \
    script-src 'self' https://static.geetest.com https://*.geetest.com https://dn-staticdown.qbox.me; \
    connect-src https://geetest.com https://*.geetest.com https://monitor.geetest.com https://dn-staticdown.qbox.me; \
    img-src data: blob: https://geetest.com https://*.geetest.com https://dn-staticdown.qbox.me; \
    style-src 'self' 'unsafe-inline' https://*.geetest.com; \
    font-src data: https://*.geetest.com; \
    frame-src https://*.geetest.com; \
    frame-ancestors 'self'; \
    base-uri 'none'; \
    form-action 'none'";

pub async fn index(headers: HeaderMap) -> Response {
    serve_path("index.html", true, &headers)
}

pub async fn fallback(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        return serve_path("index.html", true, &headers);
    }
    if path.starts_with("api/")
        || path == "api"
        || path.starts_with("ws/")
        || path == "ws"
        || path.starts_with("grpc/")
        || path == "grpc"
    {
        return not_found();
    }
    if path.contains("..") || path.contains('\\') {
        return not_found();
    }
    if let Some(response) = find_asset(path).map(|asset| asset_response(asset, false, &headers)) {
        return response;
    }

    let accepts_html = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|part| {
                part.split(';').next().is_some_and(|media_type| {
                    matches!(media_type.trim(), "text/html" | "application/xhtml+xml")
                })
            })
        });
    if !path.contains('.') && (accepts_html || headers.get(header::ACCEPT).is_none()) {
        return serve_path("index.html", true, &headers);
    }
    not_found()
}

fn serve_path(path: &str, html_navigation: bool, headers: &HeaderMap) -> Response {
    if !WEB_UI_AVAILABLE {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "The embedded SyncTV Web client is not available in this build.",
        )
            .into_response();
    }
    find_asset(path)
        .map(|asset| asset_response(asset, html_navigation, headers))
        .unwrap_or_else(not_found)
}

fn find_asset(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.path == path)
}

fn asset_response(asset: &'static Asset, html_navigation: bool, headers: &HeaderMap) -> Response {
    let cache_control = if asset.path == PROVIDER_VERIFICATION_PAGE {
        "no-store"
    } else if versioned_playback_asset(asset.path) {
        "public, max-age=31536000, immutable"
    } else if html_navigation || update_metadata_asset(asset.path) {
        "no-cache"
    } else {
        "public, max-age=0, must-revalidate"
    };
    let representation = match select_representation(asset, headers) {
        Some(representation) => representation,
        None => return StatusCode::NOT_ACCEPTABLE.into_response(),
    };
    if if_none_match_matches(headers, representation.etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response
            .headers_mut()
            .insert(header::ETAG, HeaderValue::from_static(representation.etag));
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
        apply_representation_headers(response.headers_mut(), representation);
        apply_asset_security_headers(asset.path, response.headers_mut());
        return response;
    }
    let mut response = Response::new(Body::from(representation.bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_static(representation.etag));
    apply_representation_headers(response.headers_mut(), representation);
    apply_asset_security_headers(asset.path, response.headers_mut());
    response
}

fn apply_asset_security_headers(path: &str, headers: &mut HeaderMap) {
    if path != PROVIDER_VERIFICATION_PAGE {
        return;
    }
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PROVIDER_VERIFICATION_CSP),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("SAMEORIGIN"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "accelerometer=(), camera=(), geolocation=(), gyroscope=(), \
             magnetometer=(), microphone=(), payment=(), picture-in-picture=(), usb=()",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
}

#[derive(Clone, Copy)]
struct Representation {
    bytes: &'static [u8],
    etag: &'static str,
    encoding: Option<&'static str>,
}

fn select_representation(asset: &'static Asset, headers: &HeaderMap) -> Option<Representation> {
    let qualities = accepted_encoding_qualities(headers);
    let brotli_quality = asset.brotli.map_or(0, |_| qualities.brotli);
    let gzip_quality = asset.gzip.map_or(0, |_| qualities.gzip);
    if brotli_quality > 0 && brotli_quality >= gzip_quality && brotli_quality >= qualities.identity
    {
        let encoded = asset.brotli?;
        return Some(Representation {
            bytes: encoded.bytes,
            etag: encoded.etag,
            encoding: Some("br"),
        });
    }
    if gzip_quality > 0 && gzip_quality >= qualities.identity {
        let encoded = asset.gzip?;
        return Some(Representation {
            bytes: encoded.bytes,
            etag: encoded.etag,
            encoding: Some("gzip"),
        });
    }
    (qualities.identity > 0).then_some(Representation {
        bytes: asset.bytes,
        etag: asset.etag,
        encoding: None,
    })
}

fn apply_representation_headers(headers: &mut HeaderMap, representation: Representation) {
    headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some(encoding) = representation.encoding {
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EncodingQualities {
    brotli: u16,
    gzip: u16,
    identity: u16,
}

fn accepted_encoding_qualities(headers: &HeaderMap) -> EncodingQualities {
    if !headers.contains_key(header::ACCEPT_ENCODING) {
        return EncodingQualities {
            brotli: 0,
            gzip: 0,
            identity: 1000,
        };
    }

    let mut brotli = None;
    let mut gzip = None;
    let mut identity = None;
    let mut wildcard = None;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for item in value.split(',') {
            let mut parts = item.trim().split(';');
            let name = parts.next().unwrap_or_default().trim();
            if name.is_empty() {
                continue;
            }
            let mut quality = 1000;
            for parameter in parts {
                let Some((key, value)) = parameter.trim().split_once('=') else {
                    quality = 0;
                    break;
                };
                if key.trim().eq_ignore_ascii_case("q") {
                    quality = parse_quality(value.trim()).unwrap_or(0);
                }
            }
            match name.to_ascii_lowercase().as_str() {
                "br" => brotli = Some(quality),
                "gzip" | "x-gzip" => gzip = Some(quality),
                "identity" => identity = Some(quality),
                "*" => wildcard = Some(quality),
                _ => {}
            }
        }
    }

    EncodingQualities {
        brotli: brotli.or(wildcard).unwrap_or(0),
        gzip: gzip.or(wildcard).unwrap_or(0),
        identity: identity.unwrap_or_else(|| if wildcard == Some(0) { 0 } else { 1000 }),
    }
}

fn parse_quality(value: &str) -> Option<u16> {
    if value == "0" {
        return Some(0);
    }
    if value == "1" {
        return Some(1000);
    }
    let (whole, fraction) = value.split_once('.')?;
    if !matches!(whole, "0" | "1") || fraction.len() > 3 || fraction.is_empty() {
        return None;
    }
    let mut fraction_value = fraction.parse::<u16>().ok()?;
    if whole == "1" && fraction_value != 0 {
        return None;
    }
    for _ in fraction.len()..3 {
        fraction_value *= 10;
    }
    Some(if whole == "1" { 1000 } else { fraction_value })
}

fn versioned_playback_asset(path: &str) -> bool {
    path.starts_with("playback/") && path.as_bytes().iter().any(u8::is_ascii_digit)
}

fn update_metadata_asset(path: &str) -> bool {
    path.ends_with(".html")
        || matches!(
            path,
            "manifest.json" | "version.json" | "flutter_service_worker.js"
        )
}

fn if_none_match_matches(headers: &HeaderMap, current_etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .any(|value| etag_list_matches(value.as_bytes(), current_etag.as_bytes()))
}

fn etag_list_matches(value: &[u8], current_etag: &[u8]) -> bool {
    let mut index = 0;
    let mut matched = false;

    while index < value.len() {
        skip_optional_whitespace(value, &mut index);
        if value.get(index) == Some(&b'*') {
            index += 1;
            skip_optional_whitespace(value, &mut index);
            return index == value.len();
        }
        if value.get(index..index + 2) == Some(b"W/") {
            index += 2;
        }
        if value.get(index) != Some(&b'"') {
            return false;
        }

        let tag_start = index;
        index += 1;
        while let Some(byte) = value.get(index) {
            if *byte == b'"' {
                break;
            }
            if !matches!(*byte, 0x21 | 0x23..=0x7e | 0x80..=0xff) {
                return false;
            }
            index += 1;
        }
        if value.get(index) != Some(&b'"') {
            return false;
        }
        index += 1;
        matched |= &value[tag_start..index] == current_etag;

        skip_optional_whitespace(value, &mut index);
        if index == value.len() {
            return matched;
        }
        if value.get(index) != Some(&b',') {
            return false;
        }
        index += 1;
        if index == value.len() {
            return false;
        }
    }

    false
}

fn skip_optional_whitespace(value: &[u8], index: &mut usize) {
    while value
        .get(*index)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        *index += 1;
    }
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ETAG: &str = "\"0123456789abcdef-42\"";

    #[test]
    fn entity_tag_lists_use_weak_comparison_and_support_wildcards() {
        assert!(etag_list_matches(ETAG.as_bytes(), ETAG.as_bytes()));
        assert!(etag_list_matches(
            b"W/\"0123456789abcdef-42\"",
            ETAG.as_bytes()
        ));
        assert!(etag_list_matches(
            b"\"different\", W/\"0123456789abcdef-42\"",
            ETAG.as_bytes()
        ));
        assert!(etag_list_matches(b" * \t", ETAG.as_bytes()));
    }

    #[test]
    fn malformed_or_partial_entity_tags_do_not_match() {
        assert!(!etag_list_matches(
            b"\"0123456789abcdef-42",
            ETAG.as_bytes()
        ));
        assert!(!etag_list_matches(
            b"\"0123456789abcdef-42\" trailing",
            ETAG.as_bytes()
        ));
        assert!(!etag_list_matches(
            b"\"different, \"0123456789abcdef-42\"",
            ETAG.as_bytes()
        ));
        assert!(!etag_list_matches(b"*, \"other\"", ETAG.as_bytes()));
    }

    #[test]
    fn only_versioned_playback_assets_are_immutable() {
        assert!(versioned_playback_asset("playback/hls-1.7.1.min.js"));
        assert!(!versioned_playback_asset("main.dart.js"));
        assert!(!versioned_playback_asset("playback/engine.min.js"));
    }

    #[test]
    fn browser_update_metadata_always_revalidates() {
        assert!(update_metadata_asset("index.html"));
        assert!(update_metadata_asset("manifest.json"));
        assert!(update_metadata_asset("version.json"));
        assert!(update_metadata_asset("flutter_service_worker.js"));
        assert!(!update_metadata_asset("main.dart.js"));
    }

    #[tokio::test]
    async fn oauth_callback_uses_the_spa_entrypoint() {
        let response = fallback(
            "/oauth2/callback"
                .parse::<Uri>()
                .expect("valid callback URI"),
            HeaderMap::new(),
        )
        .await;

        if !WEB_UI_AVAILABLE {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            return;
        }
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-cache"))
        );
    }

    #[test]
    fn provider_verification_page_has_dedicated_security_policy() {
        let mut headers = HeaderMap::new();
        apply_asset_security_headers(PROVIDER_VERIFICATION_PAGE, &mut headers);

        let csp = headers
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .expect("verification CSP should be valid");
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("script-src 'self' https://static.geetest.com"));
        assert!(csp.contains("https://monitor.geetest.com"));
        assert!(csp.contains("https://dn-staticdown.qbox.me"));
        assert!(csp.contains("frame-ancestors 'self'"));
        assert!(!csp.contains("unsafe-eval"));
        assert_eq!(
            headers.get("x-frame-options"),
            Some(&HeaderValue::from_static("SAMEORIGIN"))
        );
        assert_eq!(
            headers.get(header::REFERRER_POLICY),
            Some(&HeaderValue::from_static("no-referrer"))
        );
        assert_eq!(
            headers.get("cross-origin-resource-policy"),
            Some(&HeaderValue::from_static("same-origin"))
        );
    }

    #[test]
    fn ordinary_assets_keep_the_global_security_policy() {
        let mut headers = HeaderMap::new();
        apply_asset_security_headers("index.html", &mut headers);
        assert!(!headers.contains_key(header::CONTENT_SECURITY_POLICY));
        assert!(!headers.contains_key("x-frame-options"));
    }

    #[test]
    fn content_encoding_quality_prefers_brotli_then_gzip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        assert_eq!(
            accepted_encoding_qualities(&headers),
            EncodingQualities {
                brotli: 1000,
                gzip: 1000,
                identity: 1000,
            }
        );

        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("br;q=0.4, gzip;q=0.8, identity;q=0.1"),
        );
        assert_eq!(
            accepted_encoding_qualities(&headers),
            EncodingQualities {
                brotli: 400,
                gzip: 800,
                identity: 100,
            }
        );
    }

    #[test]
    fn content_encoding_quality_honors_identity_and_wildcard_exclusions() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("br;q=0.5"),
        );
        assert_eq!(accepted_encoding_qualities(&headers).identity, 1000);

        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("*;q=0, gzip;q=0.7"),
        );
        assert_eq!(
            accepted_encoding_qualities(&headers),
            EncodingQualities {
                brotli: 0,
                gzip: 700,
                identity: 0,
            }
        );
    }

    #[test]
    fn quality_parser_rejects_values_outside_http_range() {
        assert_eq!(parse_quality("0.5"), Some(500));
        assert_eq!(parse_quality("1.000"), Some(1000));
        assert_eq!(parse_quality("1.1"), None);
        assert_eq!(parse_quality("0.0000"), None);
        assert_eq!(parse_quality("invalid"), None);
    }

    #[tokio::test]
    async fn fallback_negotiates_encoded_assets_and_revalidates_each_representation() {
        if !WEB_UI_AVAILABLE {
            return;
        }
        let asset = ASSETS
            .iter()
            .find(|asset| asset.brotli.is_some() && asset.gzip.is_some())
            .expect("the Web UI build should contain a compressible asset");
        let uri = format!("/{}", asset.path).parse::<Uri>().unwrap();

        let mut brotli_headers = HeaderMap::new();
        brotli_headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("br"));
        let brotli_response = fallback(uri.clone(), brotli_headers).await;
        assert_eq!(brotli_response.status(), StatusCode::OK);
        assert_eq!(
            brotli_response.headers().get(header::CONTENT_ENCODING),
            Some(&HeaderValue::from_static("br"))
        );
        assert_eq!(
            brotli_response.headers().get(header::VARY),
            Some(&HeaderValue::from_static("Accept-Encoding"))
        );
        let brotli_etag = brotli_response
            .headers()
            .get(header::ETAG)
            .cloned()
            .expect("Brotli response should include an ETag");

        let mut gzip_headers = HeaderMap::new();
        gzip_headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        let gzip_response = fallback(uri.clone(), gzip_headers).await;
        assert_eq!(gzip_response.status(), StatusCode::OK);
        assert_eq!(
            gzip_response.headers().get(header::CONTENT_ENCODING),
            Some(&HeaderValue::from_static("gzip"))
        );
        assert_ne!(
            gzip_response.headers().get(header::ETAG),
            Some(&brotli_etag)
        );

        let mut revalidation_headers = HeaderMap::new();
        revalidation_headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("br"));
        revalidation_headers.insert(header::IF_NONE_MATCH, brotli_etag.clone());
        let revalidation_response = fallback(uri.clone(), revalidation_headers).await;
        assert_eq!(revalidation_response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            revalidation_response
                .headers()
                .get(header::CONTENT_ENCODING),
            Some(&HeaderValue::from_static("br"))
        );
        assert_eq!(
            revalidation_response.headers().get(header::ETAG),
            Some(&brotli_etag)
        );

        let mut rejected_headers = HeaderMap::new();
        rejected_headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("br;q=0, gzip;q=0, identity;q=0"),
        );
        let rejected_response = fallback(uri, rejected_headers).await;
        assert_eq!(rejected_response.status(), StatusCode::NOT_ACCEPTABLE);
    }
}
