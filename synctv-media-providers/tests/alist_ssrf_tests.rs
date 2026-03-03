//! Alist SSRF validation tests
//!
//! Tests for SSRF validation of URLs returned by Alist API responses.
//! URLs in responses (raw_url, thumb, sign, transcoding urls, etc.) must be
//! validated to prevent SSRF attacks via malicious Alist servers.

#![allow(clippy::unwrap_used)]
use synctv_media_providers::AlistClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ============================================================================
// SSRF validation tests for fs_get response URLs
// ============================================================================

#[tokio::test]
async fn test_alist_fs_get_ssrf_safe_url_allowed() {
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
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Safe public URLs should be preserved
    assert_eq!(resp.raw_url, "https://cdn.example.com/video.mp4");
    assert_eq!(resp.thumb, "https://cdn.example.com/thumb.jpg");
}

#[tokio::test]
async fn test_alist_fs_get_ssrf_private_ip_url_cleared() {
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
                "raw_url": "http://192.168.1.1/internal/video.mp4",
                "thumb": "http://10.0.0.1/thumb.jpg",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Private IP URLs should be cleared (empty string)
    assert_eq!(resp.raw_url, "");
    assert_eq!(resp.thumb, "");
}

#[tokio::test]
async fn test_alist_fs_get_ssrf_localhost_url_cleared() {
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
                "raw_url": "http://localhost:8080/video.mp4",
                "thumb": "",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Localhost URL should be cleared
    assert_eq!(resp.raw_url, "");
}

#[tokio::test]
async fn test_alist_fs_get_ssrf_loopback_ip_url_cleared() {
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
                "raw_url": "http://127.0.0.1:8080/video.mp4",
                "thumb": "",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Loopback IP URL should be cleared
    assert_eq!(resp.raw_url, "");
}

#[tokio::test]
async fn test_alist_fs_get_ssrf_cloud_metadata_url_cleared() {
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
                "raw_url": "http://169.254.169.254/latest/meta-data/",
                "thumb": "",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Cloud metadata endpoint URL should be cleared
    assert_eq!(resp.raw_url, "");
}

#[tokio::test]
async fn test_alist_fs_get_ssrf_mixed_urls() {
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
                "raw_url": "https://valid-cdn.example.com/video.mp4",
                "thumb": "http://192.168.1.1/thumb.jpg",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Valid URL preserved, invalid cleared
    assert_eq!(resp.raw_url, "https://valid-cdn.example.com/video.mp4");
    assert_eq!(resp.thumb, "");
}

#[tokio::test]
async fn test_alist_fs_get_ssrf_related_urls() {
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
                "thumb": "",
                "sign": "",
                "provider": "local",
                "related": [
                    {
                        "name": "subtitle.srt",
                        "size": 1000,
                        "is_dir": false,
                        "modified": 0,
                        "created": 0,
                        "sign": "http://localhost/sign",
                        "thumb": "",
                        "type": 0,
                        "hashinfo": ""
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Related item's SSRF-unsafe sign URL should be cleared
    assert_eq!(resp.related.len(), 1);
    assert_eq!(resp.related[0].sign, "");
}

// ============================================================================
// SSRF validation tests for fs_list response URLs
// ============================================================================

#[tokio::test]
async fn test_alist_fs_list_ssrf_sign_and_thumb_cleared() {
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
                        "sign": "http://192.168.1.1/sign",
                        "thumb": "http://10.0.0.1/thumb.jpg",
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
    // Private IP URLs should be cleared
    assert_eq!(resp.content[0].sign, "");
    assert_eq!(resp.content[0].thumb, "");
}

#[tokio::test]
async fn test_alist_fs_list_ssrf_valid_urls_preserved() {
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
    // Valid public URLs should be preserved
    assert_eq!(resp.content[0].sign, "https://alist.example.com/sign");
    assert_eq!(resp.content[0].thumb, "https://cdn.example.com/thumb.jpg");
}

// ============================================================================
// SSRF validation tests for fs_other (transcoding) response URLs
// ============================================================================

#[tokio::test]
async fn test_alist_fs_other_ssrf_transcoding_url_cleared() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "drive_id": "",
                "file_id": "",
                "video_preview_play_info": {
                    "category": "live_transcoding",
                    "live_transcoding_subtitle_task_list": [],
                    "live_transcoding_task_list": [
                        {
                            "stage": "finished",
                            "status": "finished",
                            "template_height": 720,
                            "template_id": "720p",
                            "template_name": "720P",
                            "template_width": 1280,
                            "url": "http://192.168.1.1/transcode/720p.m3u8"
                        }
                    ],
                    "meta": null
                }
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_other("/movies/video.mp4", "video_preview", None)
        .await
        .unwrap();

    let preview = resp.video_preview_play_info.unwrap();
    assert_eq!(preview.live_transcoding_task_list.len(), 1);
    // Private IP URL should be cleared
    assert_eq!(preview.live_transcoding_task_list[0].url, "");
}

#[tokio::test]
async fn test_alist_fs_other_ssrf_subtitle_url_cleared() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "drive_id": "",
                "file_id": "",
                "video_preview_play_info": {
                    "category": "live_transcoding",
                    "live_transcoding_subtitle_task_list": [
                        {
                            "language": "en",
                            "status": "finished",
                            "url": "http://127.0.0.1/subtitles/en.vtt"
                        }
                    ],
                    "live_transcoding_task_list": [],
                    "meta": null
                }
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_other("/movies/video.mp4", "video_preview", None)
        .await
        .unwrap();

    let preview = resp.video_preview_play_info.unwrap();
    assert_eq!(preview.live_transcoding_subtitle_task_list.len(), 1);
    // Loopback URL should be cleared
    assert_eq!(preview.live_transcoding_subtitle_task_list[0].url, "");
}

#[tokio::test]
async fn test_alist_fs_other_ssrf_valid_urls_preserved() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "drive_id": "",
                "file_id": "",
                "video_preview_play_info": {
                    "category": "live_transcoding",
                    "live_transcoding_subtitle_task_list": [
                        {
                            "language": "en",
                            "status": "finished",
                            "url": "https://cdn.example.com/subs/en.vtt"
                        }
                    ],
                    "live_transcoding_task_list": [
                        {
                            "stage": "finished",
                            "status": "finished",
                            "template_height": 720,
                            "template_id": "720p",
                            "template_name": "720P",
                            "template_width": 1280,
                            "url": "https://cdn.example.com/transcode/720p.m3u8"
                        }
                    ],
                    "meta": null
                }
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_other("/movies/video.mp4", "video_preview", None)
        .await
        .unwrap();

    let preview = resp.video_preview_play_info.unwrap();
    // Valid public URLs should be preserved
    assert_eq!(
        preview.live_transcoding_task_list[0].url,
        "https://cdn.example.com/transcode/720p.m3u8"
    );
    assert_eq!(
        preview.live_transcoding_subtitle_task_list[0].url,
        "https://cdn.example.com/subs/en.vtt"
    );
}

// ============================================================================
// SSRF validation tests for empty/invalid URLs
// ============================================================================

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
    let resp = client.fs_get("/movies", None).await.unwrap();

    // Empty URLs should remain empty (not cause errors)
    assert_eq!(resp.raw_url, "");
    assert_eq!(resp.thumb, "");
    assert_eq!(resp.sign, "");
}

#[tokio::test]
async fn test_alist_fs_get_non_url_sign_preserved() {
    // Some Alist providers may return non-URL strings in the sign field
    // (like signature tokens rather than URLs)
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
                "thumb": "",
                "sign": "abc123def456_token_signature",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Non-URL sign values (like tokens) should be preserved
    assert_eq!(resp.sign, "abc123def456_token_signature");
}

// ============================================================================
// SSRF validation tests for IP encoding attacks
// ============================================================================

#[tokio::test]
async fn test_alist_fs_get_ssrf_decimal_ip_cleared() {
    let server = MockServer::start().await;
    // 3232235777 = 192.168.1.1 in decimal form

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
                "raw_url": "http://3232235777/video.mp4",
                "thumb": "",
                "sign": "",
                "provider": "local",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/video.mp4", None).await.unwrap();

    // Decimal-encoded IP should be detected and cleared
    assert_eq!(resp.raw_url, "");
}
