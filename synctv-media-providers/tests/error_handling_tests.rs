//! Error handling tests with wiremock
//!
//! Tests for json_with_limit, check_response, and with_retry using mock HTTP responses.

use synctv_media_providers::error::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

// === json_with_limit Tests ===

#[tokio::test]
async fn test_json_with_limit_valid_json() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"key": "value", "number": 42})),
        )
        .mount(&server)
        .await;

    let resp = reqwest::get(&server.uri()).await.unwrap();
    let result: Result<serde_json::Value, _> = json_with_limit(resp).await;
    assert!(result.is_ok());
    let val = result.unwrap();
    assert_eq!(val["key"], "value");
    assert_eq!(val["number"], 42);
}

#[tokio::test]
async fn test_json_with_limit_exceeds_max() {
    let server = MockServer::start().await;

    // Create a response with a large Content-Length that matches the actual body.
    // We generate a body larger than MAX_RESPONSE_SIZE (16 MB = 16_777_216 bytes).
    // The json_with_limit function checks Content-Length first, so we only need
    // to set a large CL with a matching body.
    let large_body = "x".repeat(MAX_RESPONSE_SIZE + 1);
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(large_body),
        )
        .mount(&server)
        .await;

    let resp = reqwest::get(&server.uri()).await.unwrap();
    let result: Result<serde_json::Value, _> = json_with_limit(resp).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ProviderClientError::ResponseTooLarge { .. }),
        "Expected ResponseTooLarge, got: {err:?}"
    );
}

// === check_response Tests ===

#[tokio::test]
async fn test_check_response_200_ok() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let resp = reqwest::get(&server.uri()).await.unwrap();
    let result = check_response(resp).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_check_response_429_captures_retry_after() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "30")
                .set_body_string("rate limited"),
        )
        .mount(&server)
        .await;

    let resp = reqwest::get(&server.uri()).await.unwrap();
    let result = check_response(resp).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    match &err {
        ProviderClientError::Http {
            status,
            retry_after_secs,
            body,
            ..
        } => {
            assert_eq!(*status, reqwest::StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(*retry_after_secs, Some(30));
            assert!(body.contains("rate limited"));
        }
        other => panic!("Expected Http error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_check_response_500_captures_body() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(500).set_body_string("Internal Server Error: db connection failed"),
        )
        .mount(&server)
        .await;

    let resp = reqwest::get(&server.uri()).await.unwrap();
    let result = check_response(resp).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    match &err {
        ProviderClientError::Http {
            status,
            body,
            retry_after_secs,
            ..
        } => {
            assert_eq!(*status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
            assert!(body.contains("db connection failed"));
            assert_eq!(*retry_after_secs, None);
        }
        other => panic!("Expected Http error, got: {other:?}"),
    }
}

// === with_retry Tests ===

#[tokio::test]
async fn test_with_retry_succeeds_first_attempt() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": "success"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let url = server.uri();
    let result: Result<serde_json::Value, ProviderClientError> = with_retry(|| {
        let url = url.clone();
        async move {
            let resp = reqwest::get(&url).await?;
            let resp = check_response(resp).await?;
            let val: serde_json::Value = json_with_limit(resp).await?;
            Ok(val)
        }
    })
    .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap()["result"], "success");
}

#[tokio::test]
async fn test_with_retry_retries_on_5xx() {
    let server = MockServer::start().await;

    // First request returns 500, second returns 200
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_string("error"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": "recovered"})),
        )
        .mount(&server)
        .await;

    let url = server.uri();
    let result: Result<serde_json::Value, ProviderClientError> = with_retry(|| {
        let url = url.clone();
        async move {
            let resp = reqwest::get(&url).await?;
            let resp = check_response(resp).await?;
            let val: serde_json::Value = json_with_limit(resp).await?;
            Ok(val)
        }
    })
    .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap()["result"], "recovered");
}

#[tokio::test]
async fn test_with_retry_no_retry_on_4xx() {
    let server = MockServer::start().await;

    // 404 should NOT be retried
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1) // Should only be called once (no retries)
        .mount(&server)
        .await;

    let url = server.uri();
    let result: Result<serde_json::Value, ProviderClientError> = with_retry(|| {
        let url = url.clone();
        async move {
            let resp = reqwest::get(&url).await?;
            let resp = check_response(resp).await?;
            let val: serde_json::Value = json_with_limit(resp).await?;
            Ok(val)
        }
    })
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(!err.is_retryable(), "4xx errors should not be retryable");
}
