use super::*;
use axum::http::StatusCode;
use http_body_util::BodyExt;

fn test_proxy_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("test proxy client should build")
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
}

#[test]
fn test_make_absolute_already_absolute() {
    assert_eq!(
        make_absolute("https://cdn.example.com/seg1.ts", None),
        "https://cdn.example.com/seg1.ts"
    );
}

#[test]
fn test_make_absolute_relative() {
    let base = url::Url::parse("https://cdn.example.com/path/master.m3u8").unwrap();
    assert_eq!(
        make_absolute("seg1.ts", Some(&base)),
        "https://cdn.example.com/path/seg1.ts"
    );
}

#[test]
fn test_rewrite_m3u8_basic() {
    let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\nseg1.ts\nseg2.ts\n";
    let rewritten = rewrite_m3u8(
        m3u8,
        "https://cdn.example.com/path/master.m3u8",
        "/proxy/stream",
    )
    .unwrap();
    assert!(rewritten.contains("/proxy/stream?url="));
    assert!(rewritten.contains("cdn%2Eexample%2Ecom"));
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
fn test_ssrf_acl_blocks_private_ips() {
    use std::net::IpAddr;
    let blocked: &[&str] = &["127.0.0.1", "192.168.1.1", "10.0.0.1"];
    for ip_str in blocked {
        let ip: IpAddr = ip_str.parse().unwrap();
        assert!(
            synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
            "strict SSRF policy should block {ip}"
        );
    }
}

#[test]
fn test_ssrf_acl_allows_public_ips() {
    use std::net::IpAddr;
    let guard = synctv_common::ssrf::SsrfGuard::strict_policy();
    let allowed: &[&str] = &["1.1.1.1", "8.8.8.8"];
    for ip_str in allowed {
        let ip: IpAddr = ip_str.parse().unwrap();
        assert!(!guard.is_ip_blocked(&ip), "IP {ip} should be allowed");
    }
}

#[test]
fn test_proxy_ssrf_rejects_hostname_non_default_ports() {
    let url = url::Url::parse("https://public.example:25/video.mp4").unwrap();
    let err =
        validate_target_url_against_ssrf(&url, &synctv_common::ssrf::SsrfGuard::strict_policy())
            .expect_err("strict SSRF policy should reject disallowed hostname ports");

    assert!(
        err.to_string().contains("target port `25` is blocked"),
        "unexpected SSRF error: {err}"
    );
}

#[test]
fn test_proxy_ssrf_allows_hostname_default_ports() {
    let url = url::Url::parse("https://public.example/video.mp4").unwrap();
    validate_target_url_against_ssrf(&url, &synctv_common::ssrf::SsrfGuard::strict_policy())
        .expect("strict SSRF policy should allow default HTTPS ports for public hostnames");
}

// URL scheme validation tests

#[tokio::test]
async fn test_proxy_fetch_rejects_file_scheme() {
    let provider_headers = HashMap::new();
    let client = test_proxy_client();
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
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
}

#[tokio::test]
async fn test_proxy_fetch_rejects_ftp_scheme() {
    let provider_headers = HashMap::new();
    let client = test_proxy_client();
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
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
}

#[tokio::test]
async fn test_proxy_fetch_rejects_javascript_scheme() {
    let provider_headers = HashMap::new();
    let client = test_proxy_client();
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
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
}

#[tokio::test]
async fn test_proxy_fetch_rejects_data_scheme() {
    let provider_headers = HashMap::new();
    let client = test_proxy_client();
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
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("only http and https are supported"),
        "Expected invalid-request scheme rejection, got: {err}"
    );
}

#[tokio::test]
async fn test_proxy_fetch_ignores_outer_deadline_for_body_lifetime() {
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

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("cdn.example.com", *server.address())
        .build()
        .expect("client should build");
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

    let response = proxy_fetch_and_forward(cfg, &NoopMetrics)
        .await
        .expect("proxy fetch should not inherit the outer request deadline");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_proxy_head_and_forward_applies_upstream_header_timeout() {
    let server = wiremock::MockServer::start().await;
    let public_origin = format!("http://cdn.example.com:{}", server.address().port());

    wiremock::Mock::given(wiremock::matchers::method("HEAD"))
        .and(wiremock::matchers::path("/slow-head.mp4"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_millis(200)))
        .mount(&server)
        .await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("cdn.example.com", *server.address())
        .build()
        .expect("client should build");
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
}

#[tokio::test]
async fn test_proxy_fetch_preserves_content_encoding_for_byte_transparent_body() {
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

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("cdn.example.com", *server.address())
        .build()
        .expect("client should build");
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

    let response = proxy_fetch_and_forward(cfg, &NoopMetrics)
        .await
        .expect("proxy fetch should succeed");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
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
async fn test_send_with_redirect_validation_resolves_relative_location() {
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

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("cdn.example.com", *server.address())
        .build()
        .expect("client should build");
    let request = client.get(format!("{public_origin}/start"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let result = send_with_redirect_validation(&client, request, &ssrf_guard).await;
    assert!(
        result.is_ok(),
        "relative redirects should resolve against original URL"
    );

    let proxy_response = result.expect("redirect should succeed");
    assert_eq!(proxy_response.response.status(), reqwest::StatusCode::OK);
    let body = proxy_response
        .response
        .bytes()
        .await
        .expect("body should be readable");
    assert_eq!(body.as_ref(), b"ok");
    assert!(proxy_response.followed_redirects);
}

#[tokio::test]
async fn test_send_with_redirect_validation_dns_rebind_error_is_typed_ssrf() {
    let client = build_proxy_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
        .expect("strict proxy client should build");
    let request = client.get("http://localhost/private");
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();

    let Err(err) = send_with_redirect_validation(&client, request, &ssrf_guard).await else {
        panic!("DNS-level SSRF denial should fail");
    };

    assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::Ssrf));
}

#[tokio::test]
async fn test_send_with_redirect_validation_redirect_to_loopback_without_listener_fails_with_disabled_ssrf(
) {
    let server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/start"))
        .respond_with(
            wiremock::ResponseTemplate::new(302)
                .insert_header("location", "http://127.0.0.1:12345/private"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client should build");
    let request = client.get(format!("{}/start", server.uri()));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let result = send_with_redirect_validation(&client, request, &ssrf_guard).await;
    let Err(err) = result else {
        panic!("redirect to loopback without a listener must fail");
    };
    let proxy_err = err
        .downcast_ref::<ProxyError>()
        .expect("error should downcast to ProxyError");
    assert!(matches!(proxy_err, ProxyError::Connection(_)));
    assert!(
        proxy_err.to_string().contains("Connection failed"),
        "unexpected error: {proxy_err}"
    );
}

#[tokio::test]
async fn test_send_with_redirect_validation_initial_loopback_fails_by_connection_with_disabled_ssrf(
) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client should build");
    let request = client.get("http://127.0.0.1:12345/private");

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let Err(err) = send_with_redirect_validation(&client, request, &ssrf_guard).await else {
        panic!("initial loopback target without a listener must fail");
    };
    let proxy_err = err
        .downcast_ref::<ProxyError>()
        .expect("error should downcast to ProxyError");
    assert!(matches!(proxy_err, ProxyError::Connection(_)));
    assert!(
        proxy_err.to_string().contains("Connection failed"),
        "unexpected error: {proxy_err}"
    );
}

#[tokio::test]
async fn test_send_with_redirect_validation_closed_connection_is_typed_connection() {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener
        .local_addr()
        .expect("test listener should expose local addr");

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("client should connect");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
    });

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client should build");
    let request = client.get(format!("http://{addr}/closed"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let Err(err) = send_with_redirect_validation(&client, request, &ssrf_guard).await else {
        panic!("closed upstream connection must fail");
    };

    server.await.expect("test server task should finish");
    assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::Connection));
}

#[tokio::test]
async fn test_send_with_redirect_validation_malformed_response_is_typed_bad_gateway_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener
        .local_addr()
        .expect("test listener should expose local addr");

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("client should connect");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(b"this is not an http response\r\n\r\n")
            .await
            .expect("malformed response should write");
    });

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client should build");
    let request = client.get(format!("http://{addr}/malformed"));

    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
    let Err(err) = send_with_redirect_validation(&client, request, &ssrf_guard).await else {
        panic!("malformed upstream response must fail");
    };

    server.await.expect("test server task should finish");
    assert!(matches!(
        proxy_error_kind(&err),
        Some(ProxyErrorKind::Connection | ProxyErrorKind::Upstream)
    ));
}

#[tokio::test]
async fn test_proxy_m3u8_and_rewrite_initial_loopback_fails_by_connection_with_disabled_ssrf() {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client should build");
    let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();

    let err = proxy_m3u8_and_rewrite(
        &client,
        &ssrf_guard,
        "http://127.0.0.1:12345/private.m3u8",
        &HashMap::new(),
        "/proxy",
    )
    .await
    .expect_err("loopback manifest without a listener must fail");

    assert!(
        err.to_string().contains("Connection failed"),
        "unexpected error: {err}"
    );
}
