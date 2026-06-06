use super::*;

#[test]
fn test_build_rate_limit_response() {
    let response = build_rate_limit_response();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get("Content-Type")
            .map(|v| v.to_str().unwrap()),
        Some("text/plain")
    );
    assert_eq!(
        response
            .headers()
            .get("Retry-After")
            .map(|v| v.to_str().unwrap()),
        Some("60")
    );
}

#[test]
fn test_build_wildcard_cors_response() {
    let response = build_wildcard_cors_response();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .map(|v| v.to_str().unwrap()),
        Some("*")
    );
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Methods")
            .map(|v| v.to_str().unwrap()),
        Some("GET, HEAD, OPTIONS")
    );
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Headers")
            .map(|v| v.to_str().unwrap()),
        Some("Authorization, Content-Type, Accept, Range")
    );
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Max-Age")
            .map(|v| v.to_str().unwrap()),
        Some("86400")
    );
    assert!(response
        .headers()
        .get("Access-Control-Allow-Credentials")
        .is_none());
    assert!(response.headers().get("Vary").is_none());
}

#[test]
fn test_build_no_origin_cors_response() {
    let response = build_no_origin_cors_response();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response
        .headers()
        .get("Access-Control-Allow-Origin")
        .is_none());
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Methods")
            .map(|v| v.to_str().unwrap()),
        Some("GET, HEAD, OPTIONS")
    );
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Headers")
            .map(|v| v.to_str().unwrap()),
        Some("Authorization, Content-Type, Accept, Range")
    );
}

#[test]
fn test_build_forbidden_cors_response() {
    let response = build_forbidden_cors_response();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response
            .headers()
            .get("Content-Type")
            .map(|v| v.to_str().unwrap()),
        Some("text/plain")
    );
}

#[test]
fn test_build_allowed_cors_response() {
    let origin = "https://example.com";
    let response = build_allowed_cors_response(origin);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .map(|v| v.to_str().unwrap()),
        Some(origin)
    );
    assert!(response
        .headers()
        .get("Access-Control-Allow-Credentials")
        .is_none());
    assert_eq!(
        response.headers().get("Vary").map(|v| v.to_str().unwrap()),
        Some("Origin")
    );
}

#[test]
fn test_handle_cors_preflight_wildcard_mode() {
    let config = CorsConfig::new_wildcard();
    let response = handle_cors_preflight(Some("https://example.com"), &config);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .map(|v| v.to_str().unwrap()),
        Some("*")
    );
}

#[test]
fn test_handle_cors_preflight_no_origin_header() {
    let config = CorsConfig::new(vec!["https://example.com".to_string()]);
    let response = handle_cors_preflight(None, &config);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response
        .headers()
        .get("Access-Control-Allow-Origin")
        .is_none());
}

#[test]
fn test_handle_cors_preflight_origin_not_allowed() {
    let config = CorsConfig::new(vec!["https://allowed.com".to_string()]);
    let response = handle_cors_preflight(Some("https://evil.com"), &config);

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[test]
fn test_handle_cors_preflight_origin_allowed() {
    let allowed_origin = "https://allowed.com";
    let config = CorsConfig::new(vec![allowed_origin.to_string()]);
    let response = handle_cors_preflight(Some(allowed_origin), &config);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("Access-Control-Allow-Origin")
            .map(|v| v.to_str().unwrap()),
        Some(allowed_origin)
    );
    assert!(response
        .headers()
        .get("Access-Control-Allow-Credentials")
        .is_none());
    assert_eq!(
        response.headers().get("Vary").map(|v| v.to_str().unwrap()),
        Some("Origin")
    );
}

#[test]
fn test_handle_cors_preflight_empty_allowed_list_rejects_all() {
    let config = CorsConfig::new(vec![]);
    let response = handle_cors_preflight(Some("https://example.com"), &config);

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
