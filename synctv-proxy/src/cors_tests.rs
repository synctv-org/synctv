use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn response_header<'a>(
    response: &'a axum::response::Response,
    name: &str,
) -> Option<Result<&'a str, axum::http::header::ToStrError>> {
    response.headers().get(name).map(|value| value.to_str())
}

#[test]
fn test_build_rate_limit_response() -> TestResult {
    let response = build_rate_limit_response();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response_header(&response, "Content-Type").transpose()?,
        Some("text/plain")
    );
    assert_eq!(
        response_header(&response, "Retry-After").transpose()?,
        Some("60")
    );
    Ok(())
}

#[test]
fn test_build_wildcard_cors_response() -> TestResult {
    let response = build_wildcard_cors_response();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Origin").transpose()?,
        Some("*")
    );
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Methods").transpose()?,
        Some("GET, HEAD, OPTIONS")
    );
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Headers").transpose()?,
        Some("Authorization, Content-Type, Accept, Range")
    );
    assert_eq!(
        response_header(&response, "Access-Control-Max-Age").transpose()?,
        Some("86400")
    );
    assert!(response
        .headers()
        .get("Access-Control-Allow-Credentials")
        .is_none());
    assert!(response.headers().get("Vary").is_none());
    Ok(())
}

#[test]
fn test_build_no_origin_cors_response() -> TestResult {
    let response = build_no_origin_cors_response();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response
        .headers()
        .get("Access-Control-Allow-Origin")
        .is_none());
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Methods").transpose()?,
        Some("GET, HEAD, OPTIONS")
    );
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Headers").transpose()?,
        Some("Authorization, Content-Type, Accept, Range")
    );
    Ok(())
}

#[test]
fn test_build_forbidden_cors_response() -> TestResult {
    let response = build_forbidden_cors_response();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response_header(&response, "Content-Type").transpose()?,
        Some("text/plain")
    );
    Ok(())
}

#[test]
fn test_build_allowed_cors_response() -> TestResult {
    let origin = "https://example.com";
    let response = build_allowed_cors_response(origin);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Origin").transpose()?,
        Some(origin)
    );
    assert!(response
        .headers()
        .get("Access-Control-Allow-Credentials")
        .is_none());
    assert_eq!(
        response_header(&response, "Vary").transpose()?,
        Some("Origin")
    );
    Ok(())
}

#[test]
fn test_handle_cors_preflight_wildcard_mode() -> TestResult {
    let config = CorsConfig::new_wildcard();
    let response = handle_cors_preflight(Some("https://example.com"), &config);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Origin").transpose()?,
        Some("*")
    );
    Ok(())
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
fn test_handle_cors_preflight_origin_allowed() -> TestResult {
    let allowed_origin = "https://allowed.com";
    let config = CorsConfig::new(vec![allowed_origin.to_string()]);
    let response = handle_cors_preflight(Some(allowed_origin), &config);

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response_header(&response, "Access-Control-Allow-Origin").transpose()?,
        Some(allowed_origin)
    );
    assert!(response
        .headers()
        .get("Access-Control-Allow-Credentials")
        .is_none());
    assert_eq!(
        response_header(&response, "Vary").transpose()?,
        Some("Origin")
    );
    Ok(())
}

#[test]
fn test_handle_cors_preflight_empty_allowed_list_rejects_all() {
    let config = CorsConfig::new(vec![]);
    let response = handle_cors_preflight(Some("https://example.com"), &config);

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
