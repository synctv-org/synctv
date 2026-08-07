//! Alist client response tests
//!
//! The Alist client no longer sanitizes URLs in API responses.
//! Transport-time SSRF enforcement is handled by the shared HTTP client
//! policy in `synctv-common`; with the current runtime default, that policy
//! is disabled unless callers opt into a strict guard.
//!
//! These tests verify that the Alist client correctly passes through URLs
//! from the API responses as-is.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use synctv_media_providers::AlistClient;
use synctv_media_providers::PROVIDER_USER_AGENT;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_headers() -> HashMap<String, String> {
    HashMap::from([("User-Agent".to_string(), PROVIDER_USER_AGENT.to_string())])
}

#[tokio::test]
async fn test_alist_fs_get_preserves_urls() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "name": "video.mp4",
                "size": 1024,
                "is_dir": false,
                "modified": 0,
                "created": 0,
                "raw_url": "https://cdn.example.com/video.mp4",
                "thumb": "https://cdn.example.com/thumb.jpg",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_get("/movies/video.mp4", None, &provider_headers())
        .await
        .unwrap();

    assert_eq!(resp.raw_url, "https://cdn.example.com/video.mp4");
    assert_eq!(resp.thumb, "https://cdn.example.com/thumb.jpg");
}

#[tokio::test]
async fn test_alist_fs_get_empty_urls_preserved() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "name": "folder",
                "size": 0,
                "is_dir": true,
                "modified": 0,
                "created": 0,
                "raw_url": "",
                "thumb": "",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_get("/movies", None, &provider_headers())
        .await
        .unwrap();

    assert_eq!(resp.raw_url, "");
    assert_eq!(resp.thumb, "");
    assert_eq!(resp.sign, "");
}

#[tokio::test]
async fn test_alist_fs_list_preserves_urls() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": [
                    {
                        "name": "file1.mp4",
                        "size": 100,
                        "is_dir": false,
                        "modified": 0,
                        "sign": "https://alist.example.com/sign",
                        "thumb": "https://cdn.example.com/thumb.jpg",
                        "type": 2
                    }
                ],
                "total": 1,
                "readme": "",
                "write": false,
                "provider": "local"
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_list("/movies", 1, 20, None).await.unwrap();

    assert_eq!(resp.content.len(), 1);
    assert_eq!(resp.content[0].sign, "https://alist.example.com/sign");
    assert_eq!(resp.content[0].thumb, "https://cdn.example.com/thumb.jpg");
}
