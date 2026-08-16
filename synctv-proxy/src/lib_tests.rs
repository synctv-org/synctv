use super::*;
use crate::manifest::{make_absolute, rewrite_uri_attribute_with_count};
use crate::redirect::{send_with_redirect_validation, REDIRECT_PRESERVE_HEADERS};
use axum::http::StatusCode;
use http_body_util::BodyExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn test_proxy_client() -> Result<reqwest::Client, reqwest::Error> {
    proxy_client_builder().build()
}

fn proxy_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_zstd()
}

fn require_proxy_err<T>(
    result: anyhow::Result<T>,
    message: &'static str,
) -> Result<anyhow::Error, Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Err(message.into()),
        Err(error) => Ok(error),
    }
}

async fn start_request_close_listener(
) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer).await;
            drop(stream);
        }
    });
    Ok((address, task))
}

#[test]
fn test_redirect_preserve_headers_includes_critical_headers() {
    assert!(
        REDIRECT_PRESERVE_HEADERS.contains(&"referer"),
        "Referer must be preserved across redirects for provider auth"
    );
    assert!(
        REDIRECT_PRESERVE_HEADERS.contains(&"user-agent"),
        "User-Agent must be preserved across redirects for provider auth"
    );
    assert!(
        REDIRECT_PRESERVE_HEADERS.contains(&"range"),
        "Range must be preserved across redirects for partial content"
    );
    for header in ["authorization", "cookie", "origin", "proxy-authorization"] {
        assert!(
            REDIRECT_PRESERVE_HEADERS.contains(&header),
            "{header} must participate in redirect scope handling"
        );
    }
}

#[test]
fn test_make_absolute_already_absolute() {
    assert_eq!(
        make_absolute("https://cdn.example.com/seg1.ts", None),
        "https://cdn.example.com/seg1.ts"
    );
}

#[test]
fn test_make_absolute_relative() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/path/master.m3u8")?;
    assert_eq!(
        make_absolute("seg1.ts", Some(&base)),
        "https://cdn.example.com/path/seg1.ts"
    );
    Ok(())
}

#[test]
fn test_make_absolute_no_base_returns_raw() {
    assert_eq!(make_absolute("seg1.ts", None), "seg1.ts");
}

#[test]
fn test_make_absolute_root_relative() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/path/master.m3u8")?;
    assert_eq!(
        make_absolute("/other/seg1.ts", Some(&base)),
        "https://cdn.example.com/other/seg1.ts"
    );
    Ok(())
}

#[test]
fn test_make_absolute_protocol_relative() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/hls/master.m3u8")?;
    let result = make_absolute("//other.cdn.com/seg.ts", Some(&base));
    assert!(result.contains("other.cdn.com/seg.ts"));
    Ok(())
}

#[test]
fn test_make_absolute_parent_directory() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/hls/stream/master.m3u8")?;
    assert_eq!(
        make_absolute("../init.mp4", Some(&base)),
        "https://cdn.example.com/hls/init.mp4"
    );
    Ok(())
}

#[test]
fn test_make_absolute_deep_relative() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/a/b/c/master.m3u8")?;
    assert_eq!(
        make_absolute("d/seg.ts", Some(&base)),
        "https://cdn.example.com/a/b/c/d/seg.ts"
    );
    Ok(())
}

#[test]
fn test_make_absolute_with_traversal() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/hls/stream/master.m3u8")?;

    assert_eq!(
        make_absolute("../secret.ts", Some(&base)),
        "https://cdn.example.com/hls/secret.ts"
    );

    let result = make_absolute("../../../../etc/passwd", Some(&base));
    assert!(result.starts_with("https://cdn.example.com/"));
    assert!(!result.contains(".."));
    Ok(())
}

#[test]
fn test_make_absolute_scheme_injection() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/hls/master.m3u8")?;
    assert_eq!(
        make_absolute("file:///etc/passwd", Some(&base)),
        "file:///etc/passwd"
    );
    Ok(())
}

#[test]
fn test_rewrite_uri_multiple_uris() -> TestResult {
    let base = url::Url::parse("https://cdn.example.com/hls/master.m3u8")?;
    let line =
        "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key1.bin\",KEYFORMAT=\"urn\",URI=\"key2.bin\"";
    let (result, count) = rewrite_uri_attribute_with_count(line, Some(&base), "/proxy");
    assert_eq!(count, 2);
    assert_eq!(result.match_indices("/proxy?url=").count(), 2);
    Ok(())
}

#[test]
fn test_rewrite_uri_malformed_no_closing_quote() {
    let (result, count) =
        rewrite_uri_attribute_with_count("#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin", None, "/proxy");
    assert_eq!(count, 0);
    assert!(result.contains("URI=\""));
    assert!(result.contains("key.bin"));
}

#[test]
fn test_rewrite_uri_no_uri_attribute() {
    let (result, count) = rewrite_uri_attribute_with_count("#EXT-X-VERSION:3", None, "/proxy");
    assert_eq!(count, 0);
    assert_eq!(result, "#EXT-X-VERSION:3");
}

#[test]
fn test_rewrite_m3u8_rejects_newline_in_proxy_base() {
    let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\nseg1.ts\n";
    let result = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/path/master.m3u8",
        "/proxy/stream\nSet-Cookie: malicious=value",
    );
    assert!(result.is_err());

    let result = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/path/master.m3u8",
        "/proxy/stream\rSet-Cookie: malicious=value",
    );
    assert!(result.is_err());

    let result = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/path/master.m3u8",
        "/proxy/stream\r\nSet-Cookie: malicious=value",
    );
    assert!(result.is_err());
}

#[test]
fn test_ssrf_acl_blocks_private_ips() -> TestResult {
    use std::net::IpAddr;
    let blocked: &[&str] = &["127.0.0.1", "192.168.1.1", "10.0.0.1"];
    for ip_str in blocked {
        let ip: IpAddr = ip_str.parse()?;
        assert!(
            synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
            "strict SSRF policy should block {ip}"
        );
    }
    Ok(())
}

#[test]
fn test_ssrf_acl_allows_public_ips() -> TestResult {
    use std::net::IpAddr;
    let guard = synctv_common::ssrf::SsrfGuard::strict_policy();
    let allowed: &[&str] = &["1.1.1.1", "8.8.8.8"];
    for ip_str in allowed {
        let ip: IpAddr = ip_str.parse()?;
        assert!(!guard.is_ip_blocked(&ip), "IP {ip} should be allowed");
    }
    Ok(())
}

#[test]
fn test_proxy_ssrf_rejects_hostname_non_default_ports() -> TestResult {
    let url = url::Url::parse("https://public.example:25/video.mp4")?;
    let err =
        validate_target_url_against_ssrf(&url, &synctv_common::ssrf::SsrfGuard::strict_policy())
            .expect_err("strict SSRF policy should reject disallowed hostname ports");

    assert!(
        err.to_string().contains("target port `25` is blocked"),
        "unexpected SSRF error: {err}"
    );
    Ok(())
}

#[test]
fn test_proxy_ssrf_allows_hostname_default_ports() -> TestResult {
    let url = url::Url::parse("https://public.example/video.mp4")?;
    validate_target_url_against_ssrf(&url, &synctv_common::ssrf::SsrfGuard::strict_policy())?;
    Ok(())
}

// URL scheme validation tests

#[tokio::test]
async fn test_proxy_fetch_rejects_file_scheme() -> TestResult {
    let provider_headers = HashMap::new();
    let client = test_proxy_client()?;
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: "file:///etc/passwd",
        provider_headers: &provider_headers,
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };

    let result = proxy_fetch_and_forward_inner(cfg).await;
    assert!(result.is_err());
    let err = result.expect_err("invalid proxy URL scheme should fail");
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
    Ok(())
}

#[tokio::test]
async fn test_proxy_fetch_rejects_ftp_scheme() -> TestResult {
    let provider_headers = HashMap::new();
    let client = test_proxy_client()?;
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: "ftp://example.com/file.txt",
        provider_headers: &provider_headers,
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };

    let result = proxy_fetch_and_forward_inner(cfg).await;
    assert!(result.is_err());
    let err = result.expect_err("invalid proxy URL scheme should fail");
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
    Ok(())
}

#[tokio::test]
async fn test_proxy_fetch_rejects_javascript_scheme() -> TestResult {
    let provider_headers = HashMap::new();
    let client = test_proxy_client()?;
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: "javascript:alert(1)",
        provider_headers: &provider_headers,
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };

    let result = proxy_fetch_and_forward_inner(cfg).await;
    assert!(result.is_err());
    let err = result.expect_err("invalid proxy URL scheme should fail");
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
    Ok(())
}

#[tokio::test]
async fn test_proxy_fetch_rejects_data_scheme() -> TestResult {
    let provider_headers = HashMap::new();
    let client = test_proxy_client()?;
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: "data:text/plain,hello",
        provider_headers: &provider_headers,
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };

    let result = proxy_fetch_and_forward_inner(cfg).await;
    assert!(result.is_err());
    let err = result.expect_err("invalid proxy URL scheme should fail");
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
    Ok(())
}

#[tokio::test]
async fn test_proxy_fetch_ignores_outer_deadline_for_body_lifetime() -> TestResult {
    let server = wiremock::MockServer::start().await;
    let public_origin = format!("http://cdn.example.com:{}", server.address().port());

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/slow.mp4"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_string("slow body"),
        )
        .mount(&server)
        .await;

    let client = proxy_client_builder()
        .resolve("cdn.example.com", *server.address())
        .build()?;
    let provider_headers = HashMap::new();
    let request_control = ExecutionControl::from_timeout(Some(Duration::from_millis(50)));
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: &format!("{public_origin}/slow.mp4"),
        provider_headers: &provider_headers,
        range_header: None,
        request_control: Some(&request_control),
        upstream_header_timeout: None,
    };

    let response = proxy_fetch_and_forward(cfg, &NoopMetrics).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn test_proxy_head_and_forward_applies_upstream_header_timeout() -> TestResult {
    let server = wiremock::MockServer::start().await;
    let public_origin = format!("http://cdn.example.com:{}", server.address().port());

    wiremock::Mock::given(wiremock::matchers::method("HEAD"))
        .and(wiremock::matchers::path("/slow-head.mp4"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_millis(200)))
        .mount(&server)
        .await;

    let client = proxy_client_builder()
        .resolve("cdn.example.com", *server.address())
        .build()?;
    let provider_headers = HashMap::new();
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: &format!("{public_origin}/slow-head.mp4"),
        provider_headers: &provider_headers,
        range_header: None,
        request_control: None,
        upstream_header_timeout: Some(Duration::from_millis(25)),
    };

    let err = proxy_head_and_forward(cfg)
        .await
        .expect_err("HEAD proxy should enforce upstream header timeout");

    assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::Timeout));
    Ok(())
}

#[tokio::test]
async fn test_proxy_fetch_preserves_content_encoding_for_byte_transparent_body() -> TestResult {
    let server = wiremock::MockServer::start().await;
    let public_origin = format!("http://cdn.example.com:{}", server.address().port());

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/encoded.bin"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_bytes(b"gzip-bytes".to_vec())
                .insert_header("content-encoding", "gzip")
                .insert_header("content-type", "application/octet-stream"),
        )
        .mount(&server)
        .await;

    let client = proxy_client_builder()
        .resolve("cdn.example.com", *server.address())
        .build()?;
    let provider_headers = HashMap::new();
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let cfg = ProxyConfig {
        ssrf_guard: &ssrf_guard,
        client: &client,
        url: &format!("{public_origin}/encoded.bin"),
        provider_headers: &provider_headers,
        range_header: None,
        request_control: None,
        upstream_header_timeout: None,
    };

    let response = proxy_fetch_and_forward(cfg, &NoopMetrics).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
    Ok(())
}

#[test]
fn test_proxy_error_kind_mapping() {
    assert_eq!(
        ProxyError::Cancelled("x".into()).kind(),
        ProxyErrorKind::Cancelled
    );
    assert_eq!(
        ProxyError::Timeout("x".into()).kind(),
        ProxyErrorKind::Timeout
    );
    assert_eq!(
        ProxyError::Connection("x".into()).kind(),
        ProxyErrorKind::Connection
    );
    assert_eq!(
        ProxyError::BodyTooLarge("x".into()).kind(),
        ProxyErrorKind::BodyTooLarge
    );
    assert_eq!(ProxyError::Ssrf("x".into()).kind(), ProxyErrorKind::Ssrf);
    assert_eq!(
        ProxyError::InvalidRequest("x".into()).kind(),
        ProxyErrorKind::InvalidRequest
    );
    assert_eq!(
        ProxyError::RangeNotSatisfiable {
            message: "x".into(),
            total_size: 42,
        }
        .kind(),
        ProxyErrorKind::RangeNotSatisfiable
    );
    assert_eq!(
        ProxyError::Upstream("x".into()).kind(),
        ProxyErrorKind::Upstream
    );
}

#[test]
fn test_proxy_error_kind_from_error_chain() {
    let err = anyhow::Error::from(ProxyError::BodyTooLarge(
        "stream exceeded limit".to_string(),
    ))
    .context("outer context");

    assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::BodyTooLarge));
}

#[test]
fn test_proxy_range_not_satisfiable_total_size_from_error_chain() {
    let err = anyhow::Error::from(ProxyError::RangeNotSatisfiable {
        message: "range start beyond total size".to_string(),
        total_size: 4096,
    })
    .context("outer context");

    assert_eq!(
        proxy_error_kind(&err),
        Some(ProxyErrorKind::RangeNotSatisfiable)
    );
    assert_eq!(proxy_range_not_satisfiable_total_size(&err), Some(4096));
}

#[tokio::test]
async fn test_proxy_body_stream_preserves_typed_oversize_error() {
    let stream = futures::stream::iter([
        Ok(Bytes::from_static(b"1234")),
        Ok(Bytes::from_static(b"567")),
    ]);
    let body = Body::from_stream(proxy_body_stream(stream, 6));
    let err = body
        .collect()
        .await
        .expect_err("oversized streaming body should fail");

    assert_eq!(
        proxy_error_kind_from_std_error(&err),
        Some(ProxyErrorKind::BodyTooLarge)
    );
}

#[tokio::test]
async fn test_send_with_redirect_validation_resolves_relative_location() -> TestResult {
    let server = wiremock::MockServer::start().await;
    let public_origin = format!("http://cdn.example.com:{}", server.address().port());

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/start"))
        .respond_with(wiremock::ResponseTemplate::new(302).insert_header("location", "/final"))
        .mount(&server)
        .await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/final"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let client = proxy_client_builder()
        .resolve("cdn.example.com", *server.address())
        .build()?;
    let request = client.get(format!("{public_origin}/start"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let proxy_response = send_with_redirect_validation(&client, request, &ssrf_guard).await?;
    assert_eq!(proxy_response.response.status(), reqwest::StatusCode::OK);
    let body = proxy_response.response.bytes().await?;
    assert_eq!(body.as_ref(), b"ok");
    assert!(proxy_response.followed_redirects);
    Ok(())
}

#[tokio::test]
async fn test_send_with_redirect_validation_dns_rebind_error_is_typed_ssrf() -> TestResult {
    let client = build_proxy_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())?;
    let request = client.get("http://localhost/private");
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();

    let err = require_proxy_err(
        send_with_redirect_validation(&client, request, &ssrf_guard).await,
        "DNS-level SSRF denial should fail",
    )?;

    assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::Ssrf));
    Ok(())
}

#[tokio::test]
async fn test_send_with_redirect_validation_redirect_to_connection_close_fails_with_disabled_ssrf(
) -> TestResult {
    let server = wiremock::MockServer::start().await;
    let (close_address, close_task) = start_request_close_listener().await?;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/start"))
        .respond_with(
            wiremock::ResponseTemplate::new(302)
                .insert_header("location", format!("http://{close_address}/private")),
        )
        .mount(&server)
        .await;

    let client = test_proxy_client()?;
    let request = client.get(format!("{}/start", server.uri()));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let err = require_proxy_err(
        send_with_redirect_validation(&client, request, &ssrf_guard).await,
        "redirect to a closing loopback connection must fail",
    )?;
    close_task.abort();
    let proxy_err = err
        .downcast_ref::<ProxyError>()
        .ok_or("error should downcast to ProxyError")?;
    assert!(matches!(proxy_err, ProxyError::Connection(_)));
    assert!(
        proxy_err.to_string().contains("Connection failed"),
        "unexpected error: {proxy_err}"
    );
    Ok(())
}

#[tokio::test]
async fn test_send_with_redirect_validation_initial_loopback_fails_by_connection_with_disabled_ssrf(
) -> TestResult {
    let client = test_proxy_client()?;
    let (close_address, close_task) = start_request_close_listener().await?;
    let request = client.get(format!("http://{close_address}/private"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let err = require_proxy_err(
        send_with_redirect_validation(&client, request, &ssrf_guard).await,
        "initial closing loopback target must fail",
    )?;
    close_task.abort();
    let proxy_err = err
        .downcast_ref::<ProxyError>()
        .ok_or("error should downcast to ProxyError")?;
    assert!(matches!(proxy_err, ProxyError::Connection(_)));
    assert!(
        proxy_err.to_string().contains("Connection failed"),
        "unexpected error: {proxy_err}"
    );
    Ok(())
}

#[tokio::test]
async fn test_send_with_redirect_validation_closed_connection_with_private_path_is_typed_connection(
) -> TestResult {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await?;
        Ok::<(), std::io::Error>(())
    });

    let client = test_proxy_client()?;
    let request = client.get(format!("http://{addr}/private"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let err = require_proxy_err(
        send_with_redirect_validation(&client, request, &ssrf_guard).await,
        "closed upstream connection must fail",
    )?;

    server.await??;
    assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::Connection));
    Ok(())
}

#[tokio::test]
async fn test_send_with_redirect_validation_malformed_response_is_typed_bad_gateway_error(
) -> TestResult {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await?;
        stream
            .write_all(b"this is not an http response\r\n\r\n")
            .await?;
        Ok::<(), std::io::Error>(())
    });

    let client = test_proxy_client()?;
    let request = client.get(format!("http://{addr}/malformed"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let err = require_proxy_err(
        send_with_redirect_validation(&client, request, &ssrf_guard).await,
        "malformed upstream response must fail",
    )?;

    server.await??;
    assert!(matches!(
        proxy_error_kind(&err),
        Some(ProxyErrorKind::Connection | ProxyErrorKind::Upstream)
    ));
    Ok(())
}

#[tokio::test]
async fn test_proxy_m3u8_and_rewrite_initial_loopback_fails_by_connection_with_disabled_ssrf(
) -> TestResult {
    let client = test_proxy_client()?;
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let err = proxy_m3u8_and_rewrite(
        &client,
        &ssrf_guard,
        &format!("http://127.0.0.2:{port}/private.m3u8"),
        &HashMap::new(),
        "/proxy",
    )
    .await
    .expect_err("unbound sibling loopback manifest request must fail");
    drop(listener);

    assert!(
        err.to_string().contains("Connection failed"),
        "unexpected error: {err}"
    );
    Ok(())
}
