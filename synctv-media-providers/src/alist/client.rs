//! Alist HTTP Client
//!
//! Pure HTTP client for Alist API, no dependency on `MediaProvider`

use reqwest::{
    header::{
        HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN, REFERER,
        USER_AGENT,
    },
    Client,
};
use serde_json::json;
use std::collections::HashMap;

use super::types::{
    AlistResp, HttpFsGetResp, HttpFsListResp, HttpFsOtherResp, HttpFsSearchResp, HttpMeResp,
    LoginData,
};
use crate::error::with_retry;
use crate::error::{check_response, json_with_limit, ProviderClientError as AlistError};
use serde::de::DeserializeOwned;

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

fn normalize_host_url(host: &str) -> Result<String, AlistError> {
    let parsed = parse_host_url(host)?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(AlistError::InvalidConfig(
            "Host URL must not include query or fragment".to_string(),
        ));
    }

    Ok(parsed.to_string().trim_end_matches('/').to_string())
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
    header_value(headers, USER_AGENT.as_str()).unwrap_or(crate::PROVIDER_USER_AGENT)
}

/// Return an `AlistError::Api` when the Alist response envelope reports a
/// non-200 status code.
fn check_alist_code<T>(resp: &AlistResp<T>) -> Result<(), AlistError> {
    if resp.code != 200 {
        return Err(AlistError::Api {
            code: resp.code,
            message: resp.message.clone(),
        });
    }
    Ok(())
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
        Self::with_http_client(
            host,
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
                .map_err(|err| AlistError::Network(err.to_string()))?,
        )
    }

    /// Create a new Alist client with a prebuilt HTTP client.
    pub fn with_http_client(host: impl Into<String>, client: Client) -> Result<Self, AlistError> {
        Ok(Self {
            host: normalize_host_url(&host.into())?,
            token: None,
            client,
        })
    }

    /// Create a new Alist client with token (reuses shared connection pool and per-host rate limiter)
    pub fn with_token(
        host: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self, AlistError> {
        Self::with_token_and_http_client(
            host,
            token,
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
                .map_err(|err| AlistError::Network(err.to_string()))?,
        )
    }

    /// Create a new Alist client with token and a prebuilt HTTP client.
    pub fn with_token_and_http_client(
        host: impl Into<String>,
        token: impl Into<String>,
        client: Client,
    ) -> Result<Self, AlistError> {
        Ok(Self {
            host: normalize_host_url(&host.into())?,
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

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.host, path)
    }

    /// Perform a retried POST request, decode the `AlistResp<T>` envelope, check
    /// its status code and return the unwrapped data payload.
    ///
    /// `ctx` is used to build the "Missing data in `{ctx}` response" parse error
    /// when the payload is absent.
    async fn post_json<T: DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &HeaderMap,
        ctx: &'static str,
    ) -> Result<T, AlistError> {
        let client = self.client.clone();
        with_retry(|| {
            let url = url.to_string();
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
                let resp: AlistResp<T> = json_with_limit(response).await?;
                check_alist_code(&resp)?;
                resp.data
                    .ok_or_else(|| AlistError::Parse(format!("Missing data in {ctx} response")))
            }
        })
        .await
    }

    /// Perform a retried GET request, decode the `AlistResp<T>` envelope, check
    /// its status code and return the unwrapped data payload.
    async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        headers: &HeaderMap,
        ctx: &'static str,
    ) -> Result<T, AlistError> {
        let client = self.client.clone();
        with_retry(|| {
            let url = url.to_string();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client.get(&url).headers(headers).send().await?;
                let response = check_response(response).await?;
                let resp: AlistResp<T> = json_with_limit(response).await?;
                check_alist_code(&resp)?;
                resp.data
                    .ok_or_else(|| AlistError::Parse(format!("Missing data in {ctx} response")))
            }
        })
        .await
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
            self.endpoint("/api/auth/login/hash")
        } else {
            self.endpoint("/api/auth/login")
        };
        let body = json!({
            "username": username,
            "password": password,
            "otp_code": otp_code.unwrap_or(""),
        });
        let headers = self.build_headers(&HashMap::new())?;

        let data: LoginData = self.post_json(&url, &body, &headers, "login").await?;
        let token = data.token;

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
        let url = self.endpoint("/api/fs/get");
        let body = json!({
            "path": path,
            "password": password.unwrap_or(""),
            "user_agent": user_agent,
        });
        let headers = self.build_headers(request_headers)?;

        self.post_json(&url, &body, &headers, "fs_get").await
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
        let url = self.endpoint("/api/fs/list");
        let body = json!({
            "path": path,
            "password": password.unwrap_or(""),
            "page": page,
            "per_page": per_page,
            "refresh": refresh,
        });
        let headers = self.build_headers(&HashMap::new())?;

        self.post_json(&url, &body, &headers, "fs_list").await
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
        let url = self.endpoint("/api/fs/other");
        let body = json!({
            "path": path,
            "method": method,
            "password": password.unwrap_or(""),
        });
        let headers = self.build_headers(&HashMap::new())?;

        self.post_json(&url, &body, &headers, "fs_other").await
    }

    /// Get current user information
    ///
    /// Requires authentication token
    pub async fn me(&self) -> Result<HttpMeResp, AlistError> {
        let url = self.endpoint("/api/me");
        let headers = self.build_headers(&HashMap::new())?;

        self.get_json(&url, &headers, "me").await
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
        let url = self.endpoint("/api/fs/search");
        let body = json!({
            "parent": parent,
            "keywords": keywords,
            "scope": scope,
            "page": page,
            "per_page": per_page,
            "password": password.unwrap_or(""),
        });
        let headers = self.build_headers(&HashMap::new())?;

        self.post_json(&url, &body, &headers, "fs_search").await
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
