//! Emby/Jellyfin HTTP Client

use std::sync::LazyLock;
use std::time::Duration;

use reqwest::{Client, header::{HeaderMap, HeaderValue, CONTENT_TYPE}};
use serde_json::{json, Value};

use super::error::{EmbyError, check_response, json_with_limit};
use super::types::{AuthResponse, Item, UserInfo, ItemsResponse, SystemInfo, FsListResponse, PathInfo, PlaybackInfoResponse, default_device_profile};
use crate::error::with_retry;
use crate::ssrf::ssrf_safe_dns_resolver;

/// URL-encode a string for safe use in query parameters
fn url_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Validate that an item ID contains only safe characters.
/// Emby/Jellyfin item IDs are typically numeric or alphanumeric UUIDs.
/// Uses a whitelist approach: only alphanumeric characters, hyphens, and underscores are allowed.
fn validate_item_id(id: &str) -> Result<(), EmbyError> {
    if id.is_empty() {
        return Err(EmbyError::InvalidConfig("Item ID must not be empty".to_string()));
    }
    if !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err(EmbyError::InvalidConfig(
            "Item ID contains invalid characters (only alphanumeric, hyphens, underscores allowed)".to_string(),
        ));
    }
    Ok(())
}

/// Shared HTTP client for all Emby requests (connection pooling)
/// Redirects are disabled to prevent SSRF via redirect to private IPs.
/// Uses SsrfSafeDnsResolver to check resolved IPs at connection time,
/// preventing DNS rebinding attacks.
static SHARED_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .dns_resolver(ssrf_safe_dns_resolver())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Failed to build Emby shared HTTP client")
});

const X_EMBY_TOKEN: &str = "X-Emby-Token";

/// Emby/Jellyfin HTTP Client
pub struct EmbyClient {
    host: String,
    token: Option<String>,
    user_id: Option<String>,
    client: Client,
    api_prefix: Option<String>,
}

impl EmbyClient {
    /// Create a new Emby client (reuses shared connection pool and per-host rate limiter)
    pub fn new(host: impl Into<String>) -> Result<Self, EmbyError> {
        Ok(Self {
            host: host.into(),
            token: None,
            user_id: None,
            client: SHARED_CLIENT.clone(),
            api_prefix: None,
        })
    }

    /// Create a new Emby client with credentials (reuses shared connection pool and per-host rate limiter)
    pub fn with_credentials(
        host: impl Into<String>,
        token: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Result<Self, EmbyError> {
        Ok(Self {
            host: host.into(),
            token: Some(token.into()),
            user_id: Some(user_id.into()),
            client: SHARED_CLIENT.clone(),
            api_prefix: None,
        })
    }

    /// Set a custom API prefix (e.g., "/emby" or "/jellyfin").
    /// When set, overrides the auto-detection based on hostname.
    pub fn set_api_prefix(&mut self, prefix: impl Into<String>) {
        self.api_prefix = Some(prefix.into());
    }

    /// Set authentication token and user ID
    pub fn set_credentials(&mut self, token: impl Into<String>, user_id: impl Into<String>) {
        self.token = Some(token.into());
        self.user_id = Some(user_id.into());
    }

    /// Get API prefix (/emby or /jellyfin).
    /// Uses the explicitly set prefix if available, otherwise auto-detects
    /// based on whether the host URL's hostname contains "jellyfin".
    fn get_api_prefix(&self) -> &str {
        if let Some(ref prefix) = self.api_prefix {
            return prefix;
        }
        // Parse the host URL and check only the hostname component to avoid
        // false matches from "jellyfin" appearing in paths or query strings.
        let is_jellyfin = url::Url::parse(&self.host)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .is_some_and(|host| host.contains("jellyfin"));
        if is_jellyfin {
            "/jellyfin"
        } else {
            "/emby"
        }
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

    /// Login to Emby/Jellyfin server
    pub async fn login(&mut self, username: &str, password: &str) -> Result<(String, String), EmbyError> {

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Users/authenticatebyname", self.host, prefix);

        let body = json!({
            "Username": username,
            "Pw": password,
        });

        let response = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(EmbyError::Auth(format!("Login failed: {}", response.status())));
        }

        let auth_resp: AuthResponse = json_with_limit(response).await?;
        let token = auth_resp.access_token;
        let user_id = auth_resp.user.id;

        self.set_credentials(token.clone(), user_id.clone());
        Ok((token, user_id))
    }

    /// Get item information
    pub async fn get_item(&self, item_id: &str) -> Result<Item, EmbyError> {
        validate_item_id(item_id)?;

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Users/{}/Items?Ids={}",
            self.host,
            prefix,
            url_encode(self.user_id.as_ref().ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?),
            url_encode(item_id)
        );
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
                let json: Value = json_with_limit(response).await?;
                let items = json["Items"].as_array()
                    .ok_or_else(|| EmbyError::Parse("Missing Items array".to_string()))?;

                if items.is_empty() {
                    return Err(EmbyError::Api { code: 0, message: "Item not found".to_string() });
                }

                let item: Item = serde_json::from_value(items[0].clone())?;
                Ok(item)
            }
        }).await
    }

    /// Get current user information
    pub async fn me(&self) -> Result<UserInfo, EmbyError> {

        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?;

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Users/{}", self.host, prefix, url_encode(user_id));
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
                let user: UserInfo = json_with_limit(response).await?;
                Ok(user)
            }
        }).await
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

        let prefix = self.get_api_prefix();
        let mut url = format!("{}{}/Users/{}/Items?SortBy=SortName&SortOrder=Ascending",
            self.host, prefix, url_encode(user_id));

        if let Some(pid) = parent_id {
            url.push_str(&format!("&ParentId={}", url_encode(pid)));
        }

        if let Some(term) = search_term {
            url.push_str(&format!("&SearchTerm={}&Recursive=true", url_encode(term)));
        } else {
            url.push_str("&Filters=IsNotFolder");
        }

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
                let items: ItemsResponse = json_with_limit(response).await?;
                Ok(items)
            }
        }).await
    }

    /// Get system information
    pub async fn get_system_info(&self) -> Result<SystemInfo, EmbyError> {

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/System/Info", self.host, prefix);
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
                let info: SystemInfo = json_with_limit(response).await?;
                Ok(info)
            }
        }).await
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

        let prefix = self.get_api_prefix();

        // Get user views (libraries) if no path specified
        if path.is_none() && search_term.is_none() {
            let url = format!("{}{}/Users/{}/Views", self.host, prefix, url_encode(user_id));
            let headers = self.build_headers()?;
            let client = self.client.clone();

            let views: ItemsResponse = with_retry(|| {
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
                    let views: ItemsResponse = json_with_limit(response).await?;
                    Ok(views)
                }
            }).await?;

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
        let mut url = format!(
            "{}{}/Users/{}/Items?StartIndex={}&Limit={}",
            self.host, prefix, url_encode(user_id), start_index, limit
        );

        if let Some(p) = path {
            url.push_str(&format!("&ParentId={}", url_encode(p)));
        }

        if let Some(term) = search_term {
            url.push_str(&format!("&SearchTerm={}&Recursive=true", url_encode(term)));
        }

        let headers = self.build_headers()?;
        let client = self.client.clone();

        let items: ItemsResponse = with_retry(|| {
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
                let items: ItemsResponse = json_with_limit(response).await?;
                Ok(items)
            }
        }).await?;

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

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Sessions/Logout", self.host, prefix);

        let resp = self.client
            .post(&url)
            .headers(self.build_headers()?)
            .send()
            .await?;

        check_response(resp).await?;
        Ok(())
    }

    /// Get playback information
    pub async fn get_playback_info(
        &self,
        item_id: &str,
        media_source_id: Option<&str>,
        audio_stream_index: Option<i32>,
        subtitle_stream_index: Option<i32>,
        max_streaming_bitrate: Option<i64>,
    ) -> Result<PlaybackInfoResponse, EmbyError> {
        validate_item_id(item_id)?;

        let user_id = self
            .user_id
            .as_ref()
            .ok_or_else(|| EmbyError::InvalidConfig("Missing user_id".to_string()))?;

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Items/{}/PlaybackInfo", self.host, prefix, url_encode(item_id));

        let mut body = json!({
            "UserId": user_id,
            "DeviceProfile": default_device_profile(),
        });

        if let Some(source_id) = media_source_id {
            body["MediaSourceId"] = json!(source_id);
        }
        if let Some(audio_idx) = audio_stream_index {
            body["AudioStreamIndex"] = json!(audio_idx);
        }
        if let Some(sub_idx) = subtitle_stream_index {
            body["SubtitleStreamIndex"] = json!(sub_idx);
        }
        if let Some(bitrate) = max_streaming_bitrate {
            body["MaxStreamingBitrate"] = json!(bitrate);
        }

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
                let playback_info: PlaybackInfoResponse = json_with_limit(response).await?;
                Ok(playback_info)
            }
        }).await
    }

    /// Delete active encodings
    pub async fn delete_active_encodings(&self, play_session_id: &str) -> Result<(), EmbyError> {
        validate_item_id(play_session_id)?;

        let prefix = self.get_api_prefix();
        let url = format!(
            "{}{}/Videos/ActiveEncodings?PlaySessionId={}",
            self.host, prefix, url_encode(play_session_id)
        );

        let resp = self.client
            .delete(&url)
            .headers(self.build_headers()?)
            .send()
            .await?;

        check_response(resp).await?;
        Ok(())
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

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Sessions/Playing", self.host, prefix);

        let mut body = json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
        });

        if let Some(source_id) = media_source_id {
            body["MediaSourceId"] = json!(source_id);
        }

        let resp = self.client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        check_response(resp).await?;
        Ok(())
    }

    /// Report playback stop to Emby server
    pub async fn report_playback_stop(
        &self,
        item_id: &str,
        play_session_id: &str,
        position_ticks: i64,
    ) -> Result<(), EmbyError> {
        validate_item_id(item_id)?;

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Sessions/Playing/Stopped", self.host, prefix);

        let body = json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
        });

        let resp = self.client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        check_response(resp).await?;
        Ok(())
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

        let prefix = self.get_api_prefix();
        let url = format!("{}{}/Sessions/Playing/Progress", self.host, prefix);

        let mut body = json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
            "IsPaused": is_paused,
        });

        if let Some(source_id) = media_source_id {
            body["MediaSourceId"] = json!(source_id);
        }

        let resp = self.client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        check_response(resp).await?;
        Ok(())
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
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = EmbyClient::new("https://emby.example.com").unwrap();
        assert_eq!(client.host(), "https://emby.example.com");
        assert!(!client.has_credentials());

        let client_with_creds = EmbyClient::with_credentials(
            "https://emby.example.com",
            "test_token",
            "user123"
        ).unwrap();
        assert!(client_with_creds.has_credentials());
    }

    #[test]
    fn test_api_prefix_detection() {
        let emby_client = EmbyClient::new("https://emby.example.com").unwrap();
        assert_eq!(emby_client.get_api_prefix(), "/emby");

        let jellyfin_client = EmbyClient::new("https://jellyfin.example.com").unwrap();
        assert_eq!(jellyfin_client.get_api_prefix(), "/jellyfin");
    }

    #[test]
    fn test_api_prefix_custom() {
        let mut client = EmbyClient::new("https://media.example.com").unwrap();
        client.set_api_prefix("/custom");
        assert_eq!(client.get_api_prefix(), "/custom");
    }

    #[test]
    fn test_api_prefix_custom_overrides_auto() {
        let mut client = EmbyClient::new("https://jellyfin.example.com").unwrap();
        assert_eq!(client.get_api_prefix(), "/jellyfin");
        client.set_api_prefix("/emby");
        assert_eq!(client.get_api_prefix(), "/emby");
    }

    #[test]
    fn test_client_host() {
        let client = EmbyClient::new("https://emby.myserver.com:8096").unwrap();
        assert_eq!(client.host(), "https://emby.myserver.com:8096");
    }

    #[test]
    fn test_client_credentials() {
        let client = EmbyClient::with_credentials(
            "https://emby.example.com",
            "token123",
            "user456",
        ).unwrap();
        assert!(client.has_credentials());
    }

    #[test]
    fn test_set_credentials() {
        let mut client = EmbyClient::new("https://emby.example.com").unwrap();
        assert!(!client.has_credentials());
        client.set_credentials("token", "user");
        assert!(client.has_credentials());
    }

    // === Emby Types Deserialization Tests ===

    #[test]
    fn test_auth_response_deserialize() {
        let json = r#"{
            "AccessToken": "abc123xyz",
            "User": {"Id": "user1", "Name": "Admin"}
        }"#;
        let resp: crate::emby::types::AuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "abc123xyz");
        assert_eq!(resp.user.id, "user1");
        assert_eq!(resp.user.name, "Admin");
    }

    #[test]
    fn test_items_response_deserialize() {
        let json = r#"{
            "Items": [
                {
                    "Id": "item1",
                    "Name": "Movie 1",
                    "Type": "Movie",
                    "IsFolder": false
                },
                {
                    "Id": "folder1",
                    "Name": "Series",
                    "Type": "Series",
                    "IsFolder": true
                }
            ],
            "TotalRecordCount": 2
        }"#;
        let resp: crate::emby::types::ItemsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total_record_count, 2);
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.items[0].name, "Movie 1");
        assert!(!resp.items[0].is_folder);
        assert!(resp.items[1].is_folder);
    }

    #[test]
    fn test_item_with_media_sources() {
        let json = r#"{
            "Id": "video1",
            "Name": "Test Video",
            "Type": "Movie",
            "IsFolder": false,
            "MediaSources": [
                {
                    "Id": "src1",
                    "Name": "Direct",
                    "Path": "/path/to/video.mkv",
                    "Container": "mkv",
                    "Protocol": "File",
                    "SupportsDirectPlay": true,
                    "SupportsTranscoding": true,
                    "MediaStreams": [
                        {"Codec": "h264", "Type": "Video", "Index": 0, "IsDefault": true},
                        {"Codec": "aac", "Type": "Audio", "Language": "eng", "Index": 1, "IsDefault": true}
                    ]
                }
            ],
            "RunTimeTicks": 72000000000
        }"#;
        let item: crate::emby::types::Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.media_sources.len(), 1);
        assert_eq!(item.media_sources[0].container, "mkv");
        assert_eq!(item.media_sources[0].media_streams.len(), 2);
        assert!(item.media_sources[0].supports_direct_play);
        assert_eq!(item.run_time_ticks, Some(72000000000));
    }

    #[test]
    fn test_user_info_deserialize() {
        let json = r#"{
            "Id": "user1",
            "Name": "TestUser",
            "ServerId": "server1",
            "Policy": {
                "IsAdministrator": true,
                "IsHidden": false,
                "IsDisabled": false,
                "EnableAllFolders": true
            }
        }"#;
        let user: crate::emby::types::UserInfo = serde_json::from_str(json).unwrap();
        assert_eq!(user.id, "user1");
        assert!(user.policy.as_ref().unwrap().is_administrator);
        assert!(!user.policy.as_ref().unwrap().is_disabled);
    }

    #[test]
    fn test_user_info_no_policy() {
        let json = r#"{"Id": "user1", "Name": "TestUser", "ServerId": "server1"}"#;
        let user: crate::emby::types::UserInfo = serde_json::from_str(json).unwrap();
        assert!(user.policy.is_none());
    }

    #[test]
    fn test_playback_info_response_deserialize() {
        let json = r#"{
            "PlaySessionId": "session123",
            "MediaSources": [
                {"Id": "src1", "Container": "mp4", "Protocol": "Http", "SupportsDirectPlay": true, "SupportsTranscoding": false}
            ]
        }"#;
        let resp: crate::emby::types::PlaybackInfoResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.play_session_id, "session123");
        assert_eq!(resp.media_sources.len(), 1);
    }

    #[test]
    fn test_default_device_profile() {
        let profile = crate::emby::types::default_device_profile();
        assert!(profile.get("DirectPlayProfiles").is_some());
        assert!(profile.get("TranscodingProfiles").is_some());
        assert!(profile.get("SubtitleProfiles").is_some());
        // Check it has common video codecs
        let direct_play = profile["DirectPlayProfiles"].as_array().unwrap();
        assert!(!direct_play.is_empty());
    }

    // === Proto Conversion Tests ===

    #[test]
    fn test_media_stream_to_proto() {
        let stream = crate::emby::types::MediaStream {
            codec: "h264".to_string(),
            language: "eng".to_string(),
            stream_type: "Video".to_string(),
            title: "".to_string(),
            display_title: "1080p H.264".to_string(),
            display_language: "English".to_string(),
            is_default: true,
            index: 0,
            protocol: "".to_string(),
            delivery_url: "".to_string(),
        };
        let proto: crate::grpc::emby::MediaStreamInfo = stream.into();
        assert_eq!(proto.codec, "h264");
        assert_eq!(proto.language, "eng");
        assert!(proto.is_default);
    }

    #[test]
    fn test_item_to_proto() {
        let item = crate::emby::types::Item {
            id: "item1".to_string(),
            name: "Test Movie".to_string(),
            item_type: "Movie".to_string(),
            is_folder: false,
            parent_id: Some("parent1".to_string()),
            series_name: None,
            series_id: None,
            season_name: None,
            season_id: None,
            collection_type: None,
            media_sources: vec![],
            run_time_ticks: None,
            production_year: Some(2024),
        };
        let proto: crate::grpc::emby::Item = item.into();
        assert_eq!(proto.id, "item1");
        assert_eq!(proto.name, "Test Movie");
        assert_eq!(proto.parent_id, "parent1");
        assert_eq!(proto.series_name, ""); // None -> empty
    }

    #[test]
    fn test_user_policy_to_proto() {
        let policy = crate::emby::types::UserPolicy {
            is_administrator: true,
            is_hidden: false,
            is_disabled: false,
            enable_all_folders: true,
        };
        let proto: crate::grpc::emby::UserPolicy = policy.into();
        assert!(proto.is_administrator);
        assert!(!proto.is_hidden);
        assert!(proto.enable_all_folders);
    }

    // === Item ID Validation Tests ===

    #[test]
    fn test_validate_item_id_normal() {
        assert!(validate_item_id("12345").is_ok());
        assert!(validate_item_id("abc-def-ghi").is_ok());
        assert!(validate_item_id("a1b2c3d4e5f6").is_ok());
    }

    #[test]
    fn test_validate_item_id_rejects_traversal() {
        assert!(validate_item_id("../etc/passwd").is_err());
        assert!(validate_item_id("..").is_err());
        assert!(validate_item_id("foo/bar").is_err());
        assert!(validate_item_id("foo\\bar").is_err());
    }

    #[test]
    fn test_validate_item_id_rejects_empty() {
        assert!(validate_item_id("").is_err());
    }

    #[test]
    fn test_validate_item_id_rejects_null_bytes() {
        assert!(validate_item_id("abc\0def").is_err());
    }

    // === API Prefix Detection Tests ===

    #[test]
    fn test_api_prefix_no_false_positive_on_path() {
        // "jellyfin" in path should NOT trigger jellyfin prefix
        let client = EmbyClient::new("https://media.example.com/jellyfin").unwrap();
        assert_eq!(client.get_api_prefix(), "/emby");
    }

    #[test]
    fn test_api_prefix_hostname_detection() {
        let client = EmbyClient::new("https://jellyfin.home.local:8096").unwrap();
        assert_eq!(client.get_api_prefix(), "/jellyfin");
    }
}
