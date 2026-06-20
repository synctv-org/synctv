//! Emby/Jellyfin HTTP Client

use std::collections::HashSet;
use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE},
    Client,
};
use serde_json::{json, Value};
use tokio::sync::OnceCell;

use super::types::{
    device_profile_from_playback_client_profile, AuthResponse, FsListResponse, Item, ItemsResponse,
    PathInfo, PlaybackInfoResponse, SystemInfo, UserInfo,
};
use crate::error::with_retry;
use crate::error::{check_response, json_with_limit, ProviderClientError as EmbyError};

/// URL-encode a string for safe use in query parameters
fn url_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn normalize_api_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return String::new();
    }

    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// Validate that an item ID contains only safe characters.
/// Emby/Jellyfin item IDs are typically numeric or alphanumeric UUIDs.
/// Uses a whitelist approach: only alphanumeric characters, hyphens, and underscores are allowed.
fn validate_item_id(id: &str) -> Result<(), EmbyError> {
    if id.is_empty() {
        return Err(EmbyError::InvalidConfig(
            "Item ID must not be empty".to_string(),
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(EmbyError::InvalidConfig(
            "Item ID contains invalid characters (only alphanumeric, hyphens, underscores allowed)"
                .to_string(),
        ));
    }
    Ok(())
}

static API_PREFIX_CACHE: LazyLock<moka::future::Cache<String, String>> = LazyLock::new(|| {
    moka::future::Cache::builder()
        .max_capacity(1024)
        .time_to_live(Duration::from_hours(1))
        .build()
});

const X_EMBY_TOKEN: &str = "X-Emby-Token";

/// Emby/Jellyfin HTTP Client
pub struct EmbyClient {
    host: String,
    token: Option<String>,
    user_id: Option<String>,
    client: Client,
    detected_api_prefix: OnceCell<String>,
    device_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackInfoRequest<'a> {
    pub item_id: &'a str,
    pub media_source_id: Option<&'a str>,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub enable_direct_play: Option<bool>,
    pub enable_direct_stream: Option<bool>,
    pub enable_transcoding: Option<bool>,
    pub device_profile: Option<&'a crate::grpc::emby::PlaybackInfoDeviceProfile>,
}

impl EmbyClient {
    /// Create a new Emby client (reuses shared connection pool and per-host rate limiter)
    pub fn new(host: impl Into<String>) -> Result<Self, EmbyError> {
        Self::with_http_client(
            host,
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
                .map_err(|err| EmbyError::Network(err.to_string()))?,
        )
    }

    /// Create a new Emby client with a prebuilt HTTP client.
    pub fn with_http_client(host: impl Into<String>, client: Client) -> Result<Self, EmbyError> {
        Ok(Self {
            host: host.into(),
            token: None,
            user_id: None,
            client,
            detected_api_prefix: OnceCell::new(),
            device_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    /// Create a new Emby client with credentials (reuses shared connection pool and per-host rate limiter)
    pub fn with_credentials(
        host: impl Into<String>,
        token: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Result<Self, EmbyError> {
        Self::with_credentials_and_http_client(
            host,
            token,
            user_id,
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
                .map_err(|err| EmbyError::Network(err.to_string()))?,
        )
    }

    /// Create a new Emby client with credentials and a prebuilt HTTP client.
    pub fn with_credentials_and_http_client(
        host: impl Into<String>,
        token: impl Into<String>,
        user_id: impl Into<String>,
        client: Client,
    ) -> Result<Self, EmbyError> {
        Ok(Self {
            host: host.into(),
            token: Some(token.into()),
            user_id: Some(user_id.into()),
            client,
            detected_api_prefix: OnceCell::new(),
            device_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    fn parsed_host_url(&self) -> Result<url::Url, EmbyError> {
        url::Url::parse(&self.host)
            .map_err(|e| EmbyError::InvalidConfig(format!("Invalid host URL: {e}")))
    }

    fn api_prefix_cache_key(&self) -> Result<String, EmbyError> {
        let parsed = self.parsed_host_url()?;
        let origin = parsed.origin().unicode_serialization();
        let host_path = parsed.path().trim_end_matches('/');
        if host_path.is_empty() || host_path == "/" {
            Ok(origin)
        } else {
            Ok(format!("{}{}", origin.trim_end_matches('/'), host_path))
        }
    }

    fn should_use_shared_api_prefix_cache(&self) -> Result<bool, EmbyError> {
        let parsed = self.parsed_host_url()?;
        let Some(host) = parsed.host_str() else {
            return Ok(false);
        };
        if host.eq_ignore_ascii_case("localhost") {
            return Ok(false);
        }
        if host
            .parse::<IpAddr>()
            .is_ok_and(|ip_addr| ip_addr.is_loopback())
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// Set authentication token and user ID
    pub fn set_credentials(&mut self, token: impl Into<String>, user_id: impl Into<String>) {
        self.token = Some(token.into());
        self.user_id = Some(user_id.into());
    }

    fn configured_api_prefix(&self) -> Result<String, EmbyError> {
        let parsed = self.parsed_host_url()?;
        let host_path = parsed.path().trim_end_matches('/');
        if !host_path.is_empty() && host_path != "/" {
            return Ok(host_path.to_string());
        }

        Ok(String::new())
    }

    fn endpoint_url_with_prefix(
        &self,
        endpoint_path: &str,
        prefix: &str,
    ) -> Result<String, EmbyError> {
        let parsed = self.parsed_host_url()?;
        let origin = parsed.origin().unicode_serialization();
        let prefix = normalize_api_prefix(prefix);
        let endpoint_path = endpoint_path.trim_start_matches('/');
        if prefix.is_empty() {
            Ok(format!("{}/{endpoint_path}", origin.trim_end_matches('/')))
        } else {
            Ok(format!(
                "{}{prefix}/{endpoint_path}",
                origin.trim_end_matches('/')
            ))
        }
    }

    fn detection_candidates(&self) -> Result<Vec<String>, EmbyError> {
        let configured = self.configured_api_prefix()?;
        let mut candidates = vec![
            configured,
            String::new(),
            "/emby".to_string(),
            "/jellyfin".to_string(),
        ];
        let mut seen = HashSet::new();
        candidates.retain(|candidate| seen.insert(candidate.clone()));
        Ok(candidates)
    }

    async fn probe_api_prefix(&self, prefix: &str) -> Result<bool, EmbyError> {
        let url = self.endpoint_url_with_prefix("System/Info/Public", prefix)?;
        let response = self.client.get(url).send().await?;
        if response.status().is_client_error() || response.status().is_server_error() {
            return Ok(false);
        }
        let value: Value = json_with_limit(response).await?;
        Ok(value.as_object().is_some_and(|object| {
            object.contains_key("Id")
                || object.contains_key("ServerName")
                || object.contains_key("Version")
        }))
    }

    async fn detect_api_prefix(&self) -> Result<String, EmbyError> {
        let candidates = self.detection_candidates()?;
        let mut last_error = None;

        for prefix in &candidates {
            match self.probe_api_prefix(prefix).await {
                Ok(true) => return Ok(prefix.clone()),
                Ok(false) => {}
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            EmbyError::InvalidConfig(format!(
                "Unable to auto-detect Emby/Jellyfin API base path for '{}'. Tried: {}",
                self.host,
                candidates
                    .iter()
                    .map(|value| if value.is_empty() {
                        "/"
                    } else {
                        value.as_str()
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }))
    }

    async fn api_prefix(&self) -> Result<String, EmbyError> {
        self.detected_api_prefix
            .get_or_try_init(|| async {
                if !self.should_use_shared_api_prefix_cache()? {
                    return self.detect_api_prefix().await;
                }

                let cache_key = self.api_prefix_cache_key()?;
                if let Some(prefix) = API_PREFIX_CACHE.get(&cache_key).await {
                    return Ok(prefix);
                }

                let prefix = self.detect_api_prefix().await?;
                API_PREFIX_CACHE.insert(cache_key, prefix.clone()).await;
                Ok(prefix)
            })
            .await
            .cloned()
    }

    async fn endpoint_url(&self, endpoint_path: &str) -> Result<String, EmbyError> {
        let prefix = self.api_prefix().await?;
        self.endpoint_url_with_prefix(endpoint_path, &prefix)
    }

    /// Build request headers
    fn build_headers(&self) -> Result<HeaderMap, EmbyError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(ref token) = self.token {
            headers.insert(X_EMBY_TOKEN, HeaderValue::from_str(token)?);
        }

        Ok(headers)
    }

    fn build_emby_auth_header(&self) -> Result<HeaderValue, EmbyError> {
        HeaderValue::from_str(&format!(
            r#"Emby Client="Emby Web", Device="SyncTV", DeviceId="{}", Version="4.7.14.0""#,
            self.device_id
        ))
        .map_err(|e| EmbyError::InvalidConfig(format!("Failed to build auth header: {e}")))
    }

    /// Perform a retried GET request and deserialize the JSON response body.
    async fn send_get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        headers: &HeaderMap,
    ) -> Result<T, EmbyError> {
        let client = self.client.clone();
        with_retry(|| {
            let url = url.to_string();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client.get(&url).headers(headers).send().await?;
                let response = check_response(response).await?;
                json_with_limit(response).await
            }
        })
        .await
    }

    /// Perform a retried POST request with a JSON body and deserialize the JSON
    /// response body.
    async fn send_post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &Value,
        headers: &HeaderMap,
    ) -> Result<T, EmbyError> {
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
                json_with_limit(response).await
            }
        })
        .await
    }

    /// Perform a retried POST request with an optional JSON body, discarding the
    /// response body (status check only).
    async fn send_post_no_response(
        &self,
        url: &str,
        body: Option<&Value>,
        headers: &HeaderMap,
    ) -> Result<(), EmbyError> {
        let client = self.client.clone();
        with_retry(|| {
            let url = url.to_string();
            let body = body.cloned();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let mut req = client.post(&url).headers(headers);
                if let Some(ref body) = body {
                    req = req.json(body);
                }
                let resp = req.send().await?;
                check_response(resp).await?;
                Ok(())
            }
        })
        .await
    }

    /// Login to Emby/Jellyfin server
    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<(String, String), EmbyError> {
        let url = self.endpoint_url("Users/authenticatebyname").await?;

        let body = json!({
            "Username": username,
            "Pw": password,
        });

        let mut headers = self.build_headers()?;
        headers.insert(AUTHORIZATION, self.build_emby_auth_header()?);
        let client = self.client.clone();

        let (token, user_id) = with_retry(|| {
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
                let auth_resp: AuthResponse = json_with_limit(response).await?;
                let token = auth_resp.access_token;
                let user_id = auth_resp.user.id;
                Ok((token, user_id))
            }
        })
        .await?;

        self.set_credentials(token.clone(), user_id.clone());
        Ok((token, user_id))
    }

    /// Get item information
    pub async fn get_item(&self, item_id: &str) -> Result<Item, EmbyError> {
        validate_item_id(item_id)?;

        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?;
        let mut url = self
            .endpoint_url(&format!("Users/{}/Items", url_encode(user_id)))
            .await?;
        let _ = write!(
            url,
            "?Ids={}&Fields=MediaSources%2CParentId%2CContainer",
            url_encode(item_id)
        );
        let headers = self.build_headers()?;
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let response = client.get(&url).headers(headers).send().await?;

                let response = check_response(response).await?;
                let json: Value = json_with_limit(response).await?;
                let items = json["Items"]
                    .as_array()
                    .ok_or_else(|| EmbyError::Parse("Missing Items array".to_string()))?;

                if items.is_empty() {
                    return Err(EmbyError::Api {
                        code: 0,
                        message: "Item not found".to_string(),
                    });
                }

                let item: Item = serde_json::from_value(items[0].clone())?;
                Ok(item)
            }
        })
        .await
    }

    /// Get current user information
    pub async fn me(&self) -> Result<UserInfo, EmbyError> {
        let url = match self.user_id.as_deref() {
            Some(user_id) if !user_id.trim().is_empty() => {
                self.endpoint_url(&format!("Users/{}", url_encode(user_id)))
                    .await?
            }
            _ => self.endpoint_url("Users/Me").await?,
        };
        let headers = self.build_headers()?;

        self.send_get_json(&url, &headers).await
    }

    /// List users visible to the authenticated token.
    pub async fn list_users(&self) -> Result<Vec<UserInfo>, EmbyError> {
        let url = self.endpoint_url("Users").await?;
        let headers = self.build_headers()?;

        self.send_get_json(&url, &headers).await
    }

    /// Get items list
    pub async fn get_items(
        &self,
        parent_id: Option<&str>,
        search_term: Option<&str>,
    ) -> Result<ItemsResponse, EmbyError> {
        if let Some(pid) = parent_id {
            validate_item_id(pid)?;
        }

        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?;

        let mut url = self
            .endpoint_url(&format!("Users/{}/Items", url_encode(user_id)))
            .await?;
        url.push_str(
            "?SortBy=SortName&SortOrder=Ascending&Fields=MediaSources%2CParentId%2CContainer",
        );

        if let Some(pid) = parent_id {
            url.push_str("&ParentId=");
            url.push_str(&url_encode(pid));
        }

        if let Some(term) = search_term {
            url.push_str("&SearchTerm=");
            url.push_str(&url_encode(term));
            url.push_str("&Recursive=true");
        } else {
            url.push_str("&Filters=IsNotFolder");
        }

        let headers = self.build_headers()?;

        self.send_get_json(&url, &headers).await
    }

    /// Get system information
    pub async fn get_system_info(&self) -> Result<SystemInfo, EmbyError> {
        let url = self.endpoint_url("System/Info").await?;
        let headers = self.build_headers()?;

        self.send_get_json(&url, &headers).await
    }

    /// Filesystem list
    pub async fn fs_list(
        &self,
        path: Option<&str>,
        start_index: u64,
        limit: u64,
        search_term: Option<&str>,
    ) -> Result<FsListResponse, EmbyError> {
        if let Some(p) = path {
            validate_item_id(p)?;
        }

        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?;

        // Get user views (libraries) if no path specified
        if path.is_none() && search_term.is_none() {
            let url = self
                .endpoint_url(&format!("Users/{}/Views", url_encode(user_id)))
                .await?;
            let headers = self.build_headers()?;

            let views: ItemsResponse = self.send_get_json(&url, &headers).await?;

            return Ok(FsListResponse {
                items: views.items,
                paths: vec![PathInfo {
                    name: "Home".to_string(),
                    path: String::new(),
                }],
                total: views.total_record_count,
            });
        }

        // Query items with filters
        let mut url = self
            .endpoint_url(&format!("Users/{}/Items", url_encode(user_id)))
            .await?;
        let _ = write!(url, "?StartIndex={start_index}&Limit={limit}");

        if let Some(p) = path {
            url.push_str("&ParentId=");
            url.push_str(&url_encode(p));
        }

        if let Some(term) = search_term {
            url.push_str("&SearchTerm=");
            url.push_str(&url_encode(term));
            url.push_str("&Recursive=true");
        }

        let headers = self.build_headers()?;

        let items: ItemsResponse = self.send_get_json(&url, &headers).await?;

        let mut paths = vec![PathInfo {
            name: "Home".to_string(),
            path: String::new(),
        }];

        // Add current path if specified
        if let Some(p) = path {
            if let Ok(item) = self.get_item(p).await {
                paths.push(PathInfo {
                    name: item.name,
                    path: item.id,
                });
            }
        }

        Ok(FsListResponse {
            items: items.items,
            paths,
            total: items.total_record_count,
        })
    }

    /// Logout
    pub async fn logout(&self) -> Result<(), EmbyError> {
        let url = self.endpoint_url("Sessions/Logout").await?;
        let mut headers = self.build_headers()?;
        headers.insert(AUTHORIZATION, self.build_emby_auth_header()?);

        self.send_post_no_response(&url, None, &headers).await
    }

    /// Get playback information
    pub async fn get_playback_info(
        &self,
        request: PlaybackInfoRequest<'_>,
    ) -> Result<PlaybackInfoResponse, EmbyError> {
        validate_item_id(request.item_id)?;

        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?;

        let url = self
            .endpoint_url(&format!(
                "Items/{}/PlaybackInfo",
                url_encode(request.item_id)
            ))
            .await?;

        let mut body = json!({
            "UserId": user_id,
            "DeviceProfile": device_profile_from_playback_client_profile(request.device_profile),
        });

        if let Some(source_id) = request.media_source_id {
            body["MediaSourceId"] = json!(source_id);
        }
        if let Some(audio_idx) = request.audio_stream_index {
            body["AudioStreamIndex"] = json!(audio_idx);
        }
        if let Some(sub_idx) = request.subtitle_stream_index {
            body["SubtitleStreamIndex"] = json!(sub_idx);
        }
        if let Some(bitrate) = request.max_streaming_bitrate {
            body["MaxStreamingBitrate"] = json!(bitrate);
        }
        if let Some(max_audio_channels) = request.max_audio_channels {
            body["MaxAudioChannels"] = json!(max_audio_channels);
        }
        if let Some(enable_direct_play) = request.enable_direct_play {
            body["EnableDirectPlay"] = json!(enable_direct_play);
        }
        if let Some(enable_direct_stream) = request.enable_direct_stream {
            body["EnableDirectStream"] = json!(enable_direct_stream);
        }
        if let Some(enable_transcoding) = request.enable_transcoding {
            body["EnableTranscoding"] = json!(enable_transcoding);
        }

        let headers = self.build_headers()?;

        self.send_post_json(&url, &body, &headers).await
    }

    /// Delete active encodings
    pub async fn delete_active_encodings(&self, play_session_id: &str) -> Result<(), EmbyError> {
        validate_item_id(play_session_id)?;

        let mut url = self.endpoint_url("Videos/ActiveEncodings").await?;
        url.push_str("?PlaySessionId=");
        url.push_str(&url_encode(play_session_id));
        let mut headers = self.build_headers()?;
        headers.insert(AUTHORIZATION, self.build_emby_auth_header()?);
        let client = self.client.clone();

        with_retry(|| {
            let url = url.clone();
            let headers = headers.clone();
            let client = client.clone();
            async move {
                let resp = client.delete(&url).headers(headers).send().await?;
                check_response(resp).await?;
                Ok(())
            }
        })
        .await
    }

    /// Report playback start to Emby server
    pub async fn report_playback_start(
        &self,
        item_id: &str,
        play_session_id: &str,
        media_source_id: Option<&str>,
        position_ticks: i64,
    ) -> Result<(), EmbyError> {
        validate_item_id(item_id)?;

        let url = self.endpoint_url("Sessions/Playing").await?;

        let mut body = json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
        });

        if let Some(source_id) = media_source_id {
            body["MediaSourceId"] = json!(source_id);
        }

        let headers = self.build_headers()?;

        self.send_post_no_response(&url, Some(&body), &headers)
            .await
    }

    /// Report playback stop to Emby server
    pub async fn report_playback_stop(
        &self,
        item_id: &str,
        play_session_id: &str,
        position_ticks: i64,
    ) -> Result<(), EmbyError> {
        validate_item_id(item_id)?;

        let url = self.endpoint_url("Sessions/Playing/Stopped").await?;

        let body = json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
        });

        let headers = self.build_headers()?;

        self.send_post_no_response(&url, Some(&body), &headers)
            .await
    }

    /// Report playback progress to Emby server
    pub async fn report_playback_progress(
        &self,
        item_id: &str,
        play_session_id: &str,
        media_source_id: Option<&str>,
        position_ticks: i64,
        is_paused: bool,
    ) -> Result<(), EmbyError> {
        validate_item_id(item_id)?;

        let url = self.endpoint_url("Sessions/Playing/Progress").await?;

        let mut body = json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
            "IsPaused": is_paused,
        });

        if let Some(source_id) = media_source_id {
            body["MediaSourceId"] = json!(source_id);
        }

        let headers = self.build_headers()?;

        self.send_post_no_response(&url, Some(&body), &headers)
            .await
    }

    /// Get host URL
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Check if client has credentials
    #[must_use]
    pub const fn has_credentials(&self) -> bool {
        self.token.is_some() && self.user_id.is_some()
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
