//! Alist HTTP Client
//!
//! Pure HTTP client for Alist API, no dependency on `MediaProvider`

use std::collections::HashMap;
use std::sync::LazyLock;

use reqwest::{
    header::{
        HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN, REFERER,
        USER_AGENT,
    },
    Client,
};
use serde_json::json;

use super::error::{check_response, json_with_limit, AlistError};
use super::types::{
    AlistResp, HttpFsGetResp, HttpFsListResp, HttpFsOtherResp, HttpFsSearchResp, HttpMeResp,
    LoginData,
};
use crate::error::with_retry;

/// Validate that a path does not contain traversal components.
fn validate_path(path: &str) -> Result<(), AlistError> {
    synctv_common::validation::validate_path_for_traversal(path)
        .map_err(|e| AlistError::InvalidConfig(format!("Path traversal detected: {}", e.reason)))
}

fn parse_host_url(host: &str) -> Result<url::Url, AlistError> {
    let parsed = url::Url::parse(host)
        .map_err(|e| AlistError::InvalidConfig(format!("Invalid host URL: {e}")))?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AlistError::InvalidConfig(
            "Host URL must not include userinfo credentials".to_string(),
        ));
    }

    Ok(parsed)
}

fn origin_value(url: &url::Url) -> Result<HeaderValue, AlistError> {
    let origin = url.origin().unicode_serialization();
    HeaderValue::from_str(&origin)
        .map_err(|e| AlistError::InvalidConfig(format!("Invalid Origin header value: {e}")))
}

fn referer_value(url: &url::Url) -> Result<HeaderValue, AlistError> {
    let mut referer = url.clone();
    referer.set_query(None);
    referer.set_fragment(None);
    HeaderValue::from_str(referer.as_str())
        .map_err(|e| AlistError::InvalidConfig(format!("Invalid Referer header value: {e}")))
}

fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

fn effective_user_agent(headers: &HashMap<String, String>) -> &str {
    header_value(headers, USER_AGENT.as_str()).unwrap_or(crate::error::PROVIDER_USER_AGENT)
}

/// Shared HTTP client for all Alist requests (connection pooling).
/// SSRF-safe: uses the common DNS resolver and disables redirects.
static SHARED_CLIENT: LazyLock<Result<Client, reqwest::Error>> =
    LazyLock::new(crate::build_provider_http_client);

fn shared_client() -> Result<Client, AlistError> {
    SHARED_CLIENT
        .as_ref()
        .map(Clone::clone)
        .map_err(|err| AlistError::Network(err.to_string()))
}

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
        Self::with_http_client(host, shared_client()?)
    }

    /// Create a new Alist client with a prebuilt HTTP client.
    pub fn with_http_client(host: impl Into<String>, client: Client) -> Result<Self, AlistError> {
        Ok(Self {
            host: host.into(),
            token: None,
            client,
        })
    }

    /// Create a new Alist client with token (reuses shared connection pool and per-host rate limiter)
    pub fn with_token(
        host: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self, AlistError> {
        Self::with_token_and_http_client(host, token, shared_client()?)
    }

    /// Create a new Alist client with token and a prebuilt HTTP client.
    pub fn with_token_and_http_client(
        host: impl Into<String>,
        token: impl Into<String>,
        client: Client,
    ) -> Result<Self, AlistError> {
        Ok(Self {
            host: host.into(),
            token: Some(token.into()),
            client,
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

    /// Build request headers.
    fn build_headers(
        &self,
        request_headers: &HashMap<String, String>,
    ) -> Result<HeaderMap, AlistError> {
        let parsed_host = parse_host_url(&self.host)?;
        let user_agent = effective_user_agent(request_headers);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_str(user_agent)?);
        headers.insert(ORIGIN, origin_value(&parsed_host)?);
        headers.insert(REFERER, referer_value(&parsed_host)?);

        for (name, value) in request_headers {
            if name.eq_ignore_ascii_case(CONTENT_TYPE.as_str())
                || name.eq_ignore_ascii_case(AUTHORIZATION.as_str())
                || name.eq_ignore_ascii_case(ORIGIN.as_str())
                || name.eq_ignore_ascii_case(REFERER.as_str())
            {
                continue;
            }

            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|err| AlistError::InvalidHeader(err.to_string()))?;
            let header_value = HeaderValue::from_str(value)?;
            headers.insert(header_name, header_value);
        }

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
    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
        hashed: bool,
    ) -> Result<String, AlistError> {
        self.login_with_otp(username, password, hashed, None).await
    }

    /// Login to Alist server with an optional TOTP/2FA code.
    ///
    /// Returns authentication token on success.
    /// When `hashed` is true, uses the `/api/auth/login/hash` endpoint
    /// which accepts a pre-hashed password.
    pub async fn login_with_otp(
        &mut self,
        username: &str,
        password: &str,
        hashed: bool,
        otp_code: Option<&str>,
    ) -> Result<String, AlistError> {
        let url = if hashed {
            format!("{}/api/auth/login/hash", self.host)
        } else {
            format!("{}/api/auth/login", self.host)
        };
        let body = json!({
            "username": username,
            "password": password,
            "otp_code": otp_code.unwrap_or(""),
        });
        let headers = self.build_headers(&HashMap::new())?;
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
        })
        .await?;

        self.set_token(token.clone());
        Ok(token)
    }

    /// Get file/folder information
    ///
    /// # Arguments
    /// * `path` - File or directory path
    /// * `password` - Optional password for protected directories
    ///
    pub async fn fs_get(
        &self,
        path: &str,
        password: Option<&str>,
        request_headers: &HashMap<String, String>,
    ) -> Result<HttpFsGetResp, AlistError> {
        validate_path(path)?;
        let user_agent = effective_user_agent(request_headers);
        let url = format!("{}/api/fs/get", self.host);
        let body = json!({
            "path": path,
            "password": password.unwrap_or(""),
            "user_agent": user_agent,
        });
        let headers = self.build_headers(request_headers)?;
        let client = self.client.clone();

        let result = with_retry(|| {
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

                resp.data
                    .ok_or_else(|| AlistError::Parse("Missing data in fs_get response".to_string()))
            }
        })
        .await;

        result
    }

    /// List directory contents
    ///
    /// # Arguments
    /// * `path` - Directory path
    /// * `page` - Page number (1-indexed)
    /// * `per_page` - Items per page
    /// * `password` - Optional password for protected directories
    ///
    pub async fn fs_list(
        &self,
        path: &str,
        page: u64,
        per_page: u64,
        password: Option<&str>,
    ) -> Result<HttpFsListResp, AlistError> {
        self.fs_list_with_refresh(path, page, per_page, password, false)
            .await
    }

    /// List directory contents, optionally forcing upstream refresh.
    pub async fn fs_list_with_refresh(
        &self,
        path: &str,
        page: u64,
        per_page: u64,
        password: Option<&str>,
        refresh: bool,
    ) -> Result<HttpFsListResp, AlistError> {
        validate_path(path)?;
        let url = format!("{}/api/fs/list", self.host);
        let body = json!({
            "path": path,
            "password": password.unwrap_or(""),
            "page": page,
            "per_page": per_page,
            "refresh": refresh,
        });
        let headers = self.build_headers(&HashMap::new())?;
        let client = self.client.clone();

        let result = with_retry(|| {
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

                resp.data.ok_or_else(|| {
                    AlistError::Parse("Missing data in fs_list response".to_string())
                })
            }
        })
        .await;

        result
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
    ///
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
        let headers = self.build_headers(&HashMap::new())?;
        let client = self.client.clone();

        let result = with_retry(|| {
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

                resp.data.ok_or_else(|| {
                    AlistError::Parse("Missing data in fs_other response".to_string())
                })
            }
        })
        .await;

        result
    }

    /// Get current user information
    ///
    /// Requires authentication token
    pub async fn me(&self) -> Result<HttpMeResp, AlistError> {
        let url = format!("{}/api/me", self.host);
        let headers = self.build_headers(&HashMap::new())?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client.get(&url).headers(headers).send().await?;

                let response = check_response(response).await?;
                let resp: AlistResp<HttpMeResp> = json_with_limit(response).await?;

                if resp.code != 200 {
                    return Err(AlistError::Api {
                        code: resp.code,
                        message: resp.message,
                    });
                }

                resp.data
                    .ok_or_else(|| AlistError::Parse("Missing data in me response".to_string()))
            }
        })
        .await
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
        let headers = self.build_headers(&HashMap::new())?;
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

                resp.data.ok_or_else(|| {
                    AlistError::Parse("Missing data in fs_search response".to_string())
                })
            }
        })
        .await
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
    fn test_validate_path_double_encoded_traversal() {
        // %252e%252e -> first decode -> %2e%2e -> second decode -> ..
        assert!(validate_path("/movies/%252e%252e/etc/passwd").is_err());
        // Triple encoding
        assert!(validate_path("/movies/%25252e%25252e/secret").is_err());
    }

    #[test]
    fn test_validate_path_backslash_traversal() {
        // Backslash used as path separator
        assert!(validate_path("/movies/..\\..\\etc\\passwd").is_err());
        // URL-encoded backslash (%5c / %5C)
        assert!(validate_path("/movies/..%5c..%5cetc%5cpasswd").is_err());
        assert!(validate_path("/movies/..%5C..%5Cetc").is_err());
    }

    #[test]
    fn test_validate_path_null_bytes() {
        // Literal null byte
        assert!(validate_path("/movies/video\0.mp4").is_err());
        // URL-encoded null byte
        assert!(validate_path("/movies/video%00.mp4").is_err());
    }

    #[test]
    fn test_validate_path_encoded_traversal_single_layer() {
        // Single-layer URL-encoded .. (%2e%2e / %2f)
        assert!(validate_path("/movies/%2e%2e%2fetc%2fpasswd").is_err());
        assert!(validate_path("/movies/%2E%2E%2Fetc").is_err());
    }

    #[test]
    fn test_validate_path_valid_paths_still_pass() {
        assert!(validate_path("/").is_ok());
        assert!(validate_path("/movies/video.mp4").is_ok());
        assert!(validate_path("/a/b/c/d").is_ok());
        assert!(validate_path("/movies/.hidden-file").is_ok());
        assert!(validate_path("/path/with spaces/file.mp4").is_ok());
        assert!(validate_path("/path/file%20name.mp4").is_ok());
    }

    #[test]
    fn test_client_creation() {
        let client = AlistClient::new("https://alist.example.com").unwrap();
        assert_eq!(client.host(), "https://alist.example.com");
        assert!(!client.has_token());

        let client_with_token =
            AlistClient::with_token("https://alist.example.com", "test_token").unwrap();
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

    #[test]
    fn test_build_headers_uses_origin_without_path_or_query() {
        let client = AlistClient::new("https://alist.example.com/base?token=secret#frag").unwrap();
        let headers = client.build_headers(&HashMap::new()).unwrap();

        assert_eq!(
            headers.get(ORIGIN).and_then(|v| v.to_str().ok()),
            Some("https://alist.example.com")
        );
        assert_eq!(
            headers.get(REFERER).and_then(|v| v.to_str().ok()),
            Some("https://alist.example.com/base")
        );
    }

    #[test]
    fn test_build_headers_rejects_userinfo_in_host() {
        let client = AlistClient::new("https://user:pass@alist.example.com").unwrap();
        let err = client
            .build_headers(&HashMap::new())
            .expect_err("userinfo must not be accepted in provider host");
        assert!(
            err.to_string().contains("Origin header")
                || err.to_string().contains("userinfo")
                || err.to_string().contains("Invalid host URL"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_alist_resp_deserialize_success() {
        let json = r#"{"code": 200, "message": "success", "data": {"token": "abc123"}}"#;
        let resp: crate::alist::types::AlistResp<crate::alist::types::LoginData> =
            serde_json::from_str(json).unwrap();
        assert_eq!(resp.code, 200);
        assert_eq!(resp.message, "success");
        assert_eq!(resp.data.unwrap().token, "abc123");
    }

    #[test]
    fn test_alist_resp_deserialize_no_data() {
        let json = r#"{"code": 401, "message": "unauthorized", "data": null}"#;
        let resp: crate::alist::types::AlistResp<crate::alist::types::LoginData> =
            serde_json::from_str(json).unwrap();
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
        assert_eq!(resp.size, 5_000_000);
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

    #[test]
    fn test_fs_list_content_to_proto() {
        let content = crate::alist::types::HttpFsListContent {
            name: "video.mp4".to_string(),
            size: 1024,
            is_dir: false,
            modified: 1_700_000_000,
            sign: "abc".to_string(),
            thumb: String::new(),
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
                    sign: String::new(),
                    thumb: String::new(),
                    r#type: 0,
                },
                crate::alist::types::HttpFsListContent {
                    name: "folder".to_string(),
                    size: 0,
                    is_dir: true,
                    modified: 0,
                    sign: String::new(),
                    thumb: String::new(),
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
