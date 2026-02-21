//! Alist HTTP Client
//!
//! Pure HTTP client for Alist API, no dependency on `MediaProvider`

use std::sync::LazyLock;
use std::time::Duration;

use reqwest::{Client, header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN, REFERER, USER_AGENT}};
use serde_json::json;

use super::error::{AlistError, check_response, json_with_limit};
use super::types::{AlistResp, LoginData, HttpFsGetResp, HttpFsListResp, HttpFsOtherResp, HttpMeResp, HttpFsSearchResp};
use crate::error::with_retry;

/// Validate that a path does not contain traversal components (`..` or `.`).
/// Rejects paths that attempt directory traversal to escape the intended root.
/// URL-encoded paths are decoded before validation to prevent encoded bypass attempts.
fn validate_path(path: &str) -> Result<(), AlistError> {
    // Decode URL encoding first to catch encoded traversal attempts
    // Manual decoding of common URL-encoded traversal patterns
    let decoded = path
        .replace("%2e", ".")
        .replace("%2E", ".")
        .replace("%2f", "/")
        .replace("%2F", "/");

    // Check for directory traversal components
    for component in decoded.split('/') {
        if component == ".." || component == "." {
            return Err(AlistError::InvalidConfig(format!(
                "Path traversal detected: '{}' components are not allowed",
                component
            )));
        }
    }
    Ok(())
}

/// Shared HTTP client for all Alist requests (connection pooling)
/// Redirects are disabled to prevent SSRF via redirect to private IPs.
/// The gRPC validation layer checks the initial URL, but redirect targets
/// would bypass that check without this policy.
static SHARED_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Failed to build Alist shared HTTP client")
});

/// Alist HTTP Client
///
/// Provides methods for interacting with Alist API:
/// - Authentication (login)
/// - File operations (fs/get, fs/list, fs/other)
pub struct AlistClient {
    host: String,
    token: Option<String>,
    client: Client,
}

impl AlistClient {
    /// Create a new Alist client (reuses shared connection pool and per-host rate limiter)
    pub fn new(host: impl Into<String>) -> Result<Self, AlistError> {
        Ok(Self {
            host: host.into(),
            token: None,
            client: SHARED_CLIENT.clone(),
        })
    }

    /// Create a new Alist client with token (reuses shared connection pool and per-host rate limiter)
    pub fn with_token(host: impl Into<String>, token: impl Into<String>) -> Result<Self, AlistError> {
        Ok(Self {
            host: host.into(),
            token: Some(token.into()),
            client: SHARED_CLIENT.clone(),
        })
    }

    /// Set authentication token
    pub fn set_token(&mut self, token: impl Into<String>) {
        self.token = Some(token.into());
    }

    /// Get current host
    #[must_use] 
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Check if client has token
    #[must_use] 
    pub const fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Build request headers
    fn build_headers(&self) -> Result<HeaderMap, AlistError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(crate::error::PROVIDER_USER_AGENT));
        headers.insert(ORIGIN, HeaderValue::from_str(&self.host)?);
        headers.insert(REFERER, HeaderValue::from_str(&format!("{}/", self.host))?);

        if let Some(ref token) = self.token {
            headers.insert(AUTHORIZATION, HeaderValue::from_str(token)?);
        }

        Ok(headers)
    }

    /// Login to Alist server
    ///
    /// Returns authentication token on success.
    /// When `hashed` is true, uses the `/api/auth/login/hash` endpoint
    /// which accepts a pre-hashed password.
    pub async fn login(&mut self, username: &str, password: &str, hashed: bool) -> Result<String, AlistError> {

        let url = if hashed {
            format!("{}/api/auth/login/hash", self.host)
        } else {
            format!("{}/api/auth/login", self.host)
        };
        let body = json!({
            "username": username,
            "password": password,
        });
        let headers = self.build_headers()?;
        let client = self.client.clone();

        let token = with_retry(|| {
            let url = url.clone();
            let body = body.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client
                    .post(&url)
                    .headers(headers)
                    .json(&body)
                    .send()
                    .await?;

                let response = check_response(response).await?;
                let resp: AlistResp<LoginData> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data
                    .ok_or_else(|| AlistError::Parse("Missing login data in response".to_string()))
                    .map(|d| d.token)
            }
        }).await?;

        self.set_token(token.clone());
        Ok(token)
    }

    /// Get file/folder information
    ///
    /// # Arguments
    /// * `path` - File or directory path
    /// * `password` - Optional password for protected directories
    pub async fn fs_get(&self, path: &str, password: Option<&str>) -> Result<HttpFsGetResp, AlistError> {

        validate_path(path)?;
        let url = format!("{}/api/fs/get", self.host);
        let body = json!({
            "path": path,
            "password": password.unwrap_or(""),
        });
        let headers = self.build_headers()?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let body = body.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client
                    .post(&url)
                    .headers(headers)
                    .json(&body)
                    .send()
                    .await?;

                let response = check_response(response).await?;
                let resp: AlistResp<HttpFsGetResp> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data.ok_or_else(|| AlistError::Parse("Missing data in fs_get response".to_string()))
            }
        }).await
    }

    /// List directory contents
    ///
    /// # Arguments
    /// * `path` - Directory path
    /// * `page` - Page number (1-indexed)
    /// * `per_page` - Items per page
    /// * `password` - Optional password for protected directories
    pub async fn fs_list(
        &self,
        path: &str,
        page: u64,
        per_page: u64,
        password: Option<&str>,
    ) -> Result<HttpFsListResp, AlistError> {

        validate_path(path)?;
        let url = format!("{}/api/fs/list", self.host);
        let body = json!({
            "path": path,
            "password": password.unwrap_or(""),
            "page": page,
            "per_page": per_page,
            "refresh": false,
        });
        let headers = self.build_headers()?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let body = body.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client
                    .post(&url)
                    .headers(headers)
                    .json(&body)
                    .send()
                    .await?;

                let response = check_response(response).await?;
                let resp: AlistResp<HttpFsListResp> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data.ok_or_else(|| AlistError::Parse("Missing data in fs_list response".to_string()))
            }
        }).await
    }

    /// Get video preview information (for instances supporting transcoding)
    ///
    /// # Arguments
    /// * `path` - File path
    /// * `method` - Method name (e.g., "`video_preview`")
    /// * `password` - Optional password for protected directories
    ///
    /// # Returns
    /// Video transcoding information if supported by the Alist instance.
    /// The response includes:
    /// - Available transcoding quality levels
    /// - Transcoding task status
    /// - Playback URLs for transcoded versions
    /// - Video metadata (duration, dimensions)
    pub async fn fs_other(
        &self,
        path: &str,
        method: &str,
        password: Option<&str>,
    ) -> Result<HttpFsOtherResp, AlistError> {

        validate_path(path)?;
        let url = format!("{}/api/fs/other", self.host);
        let body = json!({
            "path": path,
            "method": method,
            "password": password.unwrap_or(""),
        });
        let headers = self.build_headers()?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let body = body.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client
                    .post(&url)
                    .headers(headers)
                    .json(&body)
                    .send()
                    .await?;

                let response = check_response(response).await?;
                let resp: AlistResp<HttpFsOtherResp> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data.ok_or_else(|| AlistError::Parse("Missing data in fs_other response".to_string()))
            }
        }).await
    }

    /// Get current user information
    ///
    /// Requires authentication token
    pub async fn me(&self) -> Result<HttpMeResp, AlistError> {

        let url = format!("{}/api/me", self.host);
        let headers = self.build_headers()?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client
                    .get(&url)
                    .headers(headers)
                    .send()
                    .await?;

                let response = check_response(response).await?;
                let resp: AlistResp<HttpMeResp> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data.ok_or_else(|| AlistError::Parse("Missing data in me response".to_string()))
            }
        }).await
    }

    /// Get video transcoding/preview information
    ///
    /// This is a convenience wrapper around `fs_other` specifically for video transcoding.
    /// It handles the common case of requesting video preview information.
    ///
    /// # Arguments
    /// * `path` - Video file path
    /// * `password` - Optional password for protected directories
    ///
    /// # Returns
    /// Transcoding information including available quality levels and playback URLs
    pub async fn get_video_transcode(
        &self,
        path: &str,
        password: Option<&str>,
    ) -> Result<HttpFsOtherResp, AlistError> {
        self.fs_other(path, "video_preview", password).await
    }

    /// Search files and directories
    ///
    /// # Arguments
    /// * `parent` - Parent directory path
    /// * `keywords` - Search keywords
    /// * `scope` - Search scope (0: current dir, 1: recursive)
    /// * `page` - Page number (1-indexed)
    /// * `per_page` - Items per page
    /// * `password` - Optional password for protected directories
    pub async fn fs_search(
        &self,
        parent: &str,
        keywords: &str,
        scope: u64,
        page: u64,
        per_page: u64,
        password: Option<&str>,
    ) -> Result<HttpFsSearchResp, AlistError> {

        validate_path(parent)?;
        let url = format!("{}/api/fs/search", self.host);
        let body = json!({
            "parent": parent,
            "keywords": keywords,
            "scope": scope,
            "page": page,
            "per_page": per_page,
            "password": password.unwrap_or(""),
        });
        let headers = self.build_headers()?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let body = body.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client
                    .post(&url)
                    .headers(headers)
                    .json(&body)
                    .send()
                    .await?;

                let response = check_response(response).await?;
                let resp: AlistResp<HttpFsSearchResp> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data.ok_or_else(|| AlistError::Parse("Missing data in fs_search response".to_string()))
            }
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_path_normal() {
        assert!(validate_path("/movies/video.mp4").is_ok());
        assert!(validate_path("/").is_ok());
        assert!(validate_path("/a/b/c").is_ok());
        assert!(validate_path("relative/path").is_ok());
    }

    #[test]
    fn test_validate_path_traversal_rejected() {
        assert!(validate_path("/movies/../etc/passwd").is_err());
        assert!(validate_path("..").is_err());
        assert!(validate_path("/..").is_err());
        assert!(validate_path("/../secret").is_err());
        assert!(validate_path("/a/b/../../c").is_err());
    }

    #[test]
    fn test_validate_path_dot_allowed() {
        // Single dot and dotfiles should be allowed
        assert!(validate_path("/movies/.hidden").is_ok());
        assert!(validate_path("/.config/app").is_ok());
    }

    #[test]
    fn test_client_creation() {
        let client = AlistClient::new("https://alist.example.com").unwrap();
        assert_eq!(client.host(), "https://alist.example.com");
        assert!(!client.has_token());

        let client_with_token = AlistClient::with_token("https://alist.example.com", "test_token").unwrap();
        assert!(client_with_token.has_token());
    }

    #[test]
    fn test_set_token() {
        let mut client = AlistClient::new("https://alist.example.com").unwrap();
        assert!(!client.has_token());

        client.set_token("new_token");
        assert!(client.has_token());
    }

    #[test]
    fn test_client_host_preserved() {
        let client = AlistClient::new("https://my-server.com:5244").unwrap();
        assert_eq!(client.host(), "https://my-server.com:5244");
    }

    #[test]
    fn test_client_with_token_host() {
        let client = AlistClient::with_token("https://alist.example.com", "token123").unwrap();
        assert_eq!(client.host(), "https://alist.example.com");
        assert!(client.has_token());
    }

    #[test]
    fn test_set_token_overwrite() {
        let mut client = AlistClient::with_token("https://alist.example.com", "old_token").unwrap();
        assert!(client.has_token());
        client.set_token("new_token");
        assert!(client.has_token());
    }

    // === Alist Types Deserialization Tests ===

    #[test]
    fn test_alist_resp_deserialize_success() {
        let json = r#"{"code": 200, "message": "success", "data": {"token": "abc123"}}"#;
        let resp: crate::alist::types::AlistResp<crate::alist::types::LoginData> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "success");
        assert_eq!(resp.data.unwrap().token, "abc123");
    }

    #[test]
    fn test_alist_resp_deserialize_no_data() {
        let json = r#"{"code": 401, "message": "unauthorized", "data": null}"#;
        let resp: crate::alist::types::AlistResp<crate::alist::types::LoginData> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.code, 401);
        assert!(resp.data.is_none());
    }

    #[test]
    fn test_fs_list_resp_deserialize() {
        let json = r#"{
            "content": [
                {"name": "movie.mkv", "size": 1000000, "is_dir": false, "modified": 1234567890, "sign": "", "thumb": "", "type": 2}
            ],
            "total": 1,
            "readme": "",
            "write": false,
            "provider": "local"
        }"#;
        let resp: crate::alist::types::HttpFsListResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].name, "movie.mkv");
        assert!(!resp.content[0].is_dir);
    }

    #[test]
    fn test_fs_get_resp_deserialize() {
        let json = r#"{
            "name": "video.mp4",
            "size": 5000000,
            "is_dir": false,
            "modified": 1234567890,
            "created": 1234567800,
            "raw_url": "https://cdn.example.com/video.mp4",
            "provider": "s3"
        }"#;
        let resp: crate::alist::types::HttpFsGetResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.name, "video.mp4");
        assert_eq!(resp.size, 5000000);
        assert!(!resp.is_dir);
        assert_eq!(resp.raw_url, "https://cdn.example.com/video.mp4");
        assert_eq!(resp.provider, "s3");
    }

    #[test]
    fn test_fs_get_resp_with_defaults() {
        // Minimal JSON with only required fields, defaults for the rest
        let json = r#"{"name": "test", "size": 0, "is_dir": true}"#;
        let resp: crate::alist::types::HttpFsGetResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.name, "test");
        assert!(resp.is_dir);
        assert_eq!(resp.modified, 0);
        assert_eq!(resp.raw_url, "");
        assert!(resp.related.is_empty());
    }

    #[test]
    fn test_me_resp_deserialize() {
        let json = r#"{
            "id": 1,
            "username": "admin",
            "base_path": "/",
            "role": 0,
            "disabled": false,
            "permission": 511,
            "sso_id": "",
            "otp": false
        }"#;
        let resp: crate::alist::types::HttpMeResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 1);
        assert_eq!(resp.username, "admin");
        assert_eq!(resp.role, 0);
        assert!(!resp.disabled);
    }

    // === Proto Conversion Tests ===

    #[test]
    fn test_fs_list_content_to_proto() {
        let content = crate::alist::types::HttpFsListContent {
            name: "video.mp4".to_string(),
            size: 1024,
            is_dir: false,
            modified: 1700000000,
            sign: "abc".to_string(),
            thumb: "".to_string(),
            r#type: 2,
        };
        let proto: crate::grpc::alist::fs_list_resp::FsListContent = content.into();
        assert_eq!(proto.name, "video.mp4");
        assert_eq!(proto.size, 1024);
        assert!(!proto.is_dir);
    }

    #[test]
    fn test_fs_list_resp_to_proto() {
        let resp = crate::alist::types::HttpFsListResp {
            content: vec![
                crate::alist::types::HttpFsListContent {
                    name: "a.mp4".to_string(),
                    size: 100,
                    is_dir: false,
                    modified: 0,
                    sign: "".to_string(),
                    thumb: "".to_string(),
                    r#type: 0,
                },
                crate::alist::types::HttpFsListContent {
                    name: "folder".to_string(),
                    size: 0,
                    is_dir: true,
                    modified: 0,
                    sign: "".to_string(),
                    thumb: "".to_string(),
                    r#type: 1,
                },
            ],
            total: 2,
            readme: "readme text".to_string(),
            write: true,
            provider: "local".to_string(),
        };
        let proto: crate::grpc::alist::FsListResp = resp.into();
        assert_eq!(proto.total, 2);
        assert_eq!(proto.content.len(), 2);
        assert_eq!(proto.readme, "readme text");
        assert!(proto.write);
    }
}
