use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde_json::json;
use url::Url;

use super::types::{
    AccessTokenData, ClipData, GraphQlEnvelope, GraphQlError, HelixPage, RawAccessToken,
    RawHelixCategory, RawHelixChannelSearchItem, RawHelixScheduleResponse, RawHelixStream,
    RawSessionIdentity, TwitchAccessToken, TwitchBrowseItem, TwitchBrowseKind, TwitchBrowsePage,
    TwitchCategory, TwitchCategoryPage, TwitchChannelSearchItem, TwitchChannelSearchPage,
    TwitchMetadata, TwitchPlayback, TwitchQuality, TwitchResource, TwitchResourceKind,
    TwitchSchedulePage, TwitchScheduleSegment, TwitchSession, TwitchSessionIdentity,
    TwitchStreamItem, TwitchStreamPage,
};
use crate::{fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT};

const TWITCH_WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const CLIP_QUERY_HASH: &str = "993d9a5131f15a37bd16f32342c44ed1e0b1a9b968c6afdb662d2cddd595f6c5";
const VIDEOS_QUERY_HASH: &str = "67004f7881e65c297936f32c75246470629557a393788fb5a69d6d9a25a8fd5f";
const CLIPS_QUERY_HASH: &str = "1cd671bfa12cec480499c087319f26d21925e9695d1f80225aae6a4354f23088";
const STREAM_METADATA_QUERY_HASH: &str =
    "ad022ca32220d5523d03a23cbcb5beaa1e0999889c1f8f78f9f2520dafb5cae6";
const VIDEO_METADATA_QUERY_HASH: &str =
    "45111672eea2e507f8ba44d101a61862f9c56b11dee09a15634cb75cb9b9084d";
const VIDEO_CHAPTERS_QUERY_HASH: &str =
    "71835d5ef425e154bf282453a926d99b328cdc5e32f36d3a209d0f4778b41203";
const VIDEO_STORYBOARD_QUERY_HASH: &str =
    "07e99e4d56c5a7c67117a154777b0baf85a5ffefa393b213f4bc712ccaf85dd6";
const GRAPHQL_MAX_ATTEMPTS: usize = 3;
const GRAPHQL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchEndpoints {
    pub gql: String,
    pub usher: String,
    pub helix: String,
    pub oauth_validate: String,
}

impl Default for TwitchEndpoints {
    fn default() -> Self {
        Self {
            gql: "https://gql.twitch.tv/gql".to_string(),
            usher: "https://usher.ttvnw.net".to_string(),
            helix: "https://api.twitch.tv/helix".to_string(),
            oauth_validate: "https://id.twitch.tv/oauth2/validate".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct TwitchClient {
    http: Client,
    endpoints: TwitchEndpoints,
}

impl TwitchClient {
    pub fn new() -> Result<Self, ProviderClientError> {
        let http =
            crate::provider_http_client_builder(synctv_common::ssrf::SsrfGuard::strict_policy())
                .user_agent(PROVIDER_USER_AGENT)
                .build()
                .map_err(|error| ProviderClientError::Network(error.to_string()))?;
        Ok(Self::with_http_client(http))
    }

    #[must_use]
    pub fn with_http_client(http: Client) -> Self {
        Self {
            http,
            endpoints: TwitchEndpoints::default(),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: TwitchEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(raw: &str) -> Result<TwitchResource, ProviderClientError> {
        let url = Url::parse(raw.trim()).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid Twitch URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let segments = url
            .path_segments()
            .map(|segments| {
                segments
                    .filter(|segment| !segment.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if host == "clips.twitch.tv" {
            let id = segments.first().copied().unwrap_or_default();
            return resource(TwitchResourceKind::Clip, id);
        }
        if host != "twitch.tv"
            && host != "www.twitch.tv"
            && host != "m.twitch.tv"
            && host != "go.twitch.tv"
        {
            return Err(ProviderClientError::InvalidConfig(
                "URL host is not Twitch".to_string(),
            ));
        }
        match segments.as_slice() {
            ["videos", id, ..] if id.chars().all(|value| value.is_ascii_digit()) => {
                resource(TwitchResourceKind::Video, id)
            }
            [channel, "clip", id, ..] if !channel.is_empty() => {
                resource(TwitchResourceKind::Clip, id)
            }
            [channel, ..] if valid_channel(channel) => {
                resource(TwitchResourceKind::Channel, &channel.to_ascii_lowercase())
            }
            _ => Err(ProviderClientError::InvalidConfig(
                "unsupported Twitch URL".to_string(),
            )),
        }
    }

    pub async fn playback(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchPlayback, ProviderClientError> {
        match resource.kind {
            TwitchResourceKind::Clip => self.clip_playback(resource, session).await,
            TwitchResourceKind::Channel | TwitchResourceKind::Video => {
                let token = self.access_token(resource, session).await?;
                let master_url = self.usher_url(resource, &token)?;
                let playlist = Self::request(self.http.get(&master_url), session)
                    .send()
                    .await
                    .map_err(ProviderClientError::from)?;
                let playlist = text_with_limit(crate::check_response(playlist).await?).await?;
                let qualities = parse_master_playlist(&playlist, &master_url)?;
                Ok(TwitchPlayback {
                    resource: resource.clone(),
                    master_url: Some(master_url),
                    qualities,
                    token: Some(token),
                })
            }
        }
    }

    pub async fn metadata(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchMetadata, ProviderClientError> {
        match resource.kind {
            TwitchResourceKind::Channel => self.channel_metadata(resource, session).await,
            TwitchResourceKind::Video => self.video_metadata(resource, session).await,
            TwitchResourceKind::Clip => self.clip_metadata(resource, session).await,
        }
    }

    pub async fn validate_session(
        &self,
        session: &TwitchSession,
    ) -> Result<TwitchSessionIdentity, ProviderClientError> {
        let auth_token = session
            .auth_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ProviderClientError::InvalidConfig("Twitch auth token is required".to_string())
            })?;
        let identity: RawSessionIdentity =
            fetch_json(self.http.get(&self.endpoints.oauth_validate).header(
                reqwest::header::AUTHORIZATION,
                format!("OAuth {auth_token}"),
            ))
            .await?;
        Ok(TwitchSessionIdentity {
            client_id: identity.client_id,
            login: identity.login,
            user_id: identity.user_id,
            expires_in: identity.expires_in,
            scopes: identity.scopes,
        })
    }

    pub async fn followed_live(
        &self,
        cursor: Option<&str>,
        page_size: u32,
        session: &TwitchSession,
    ) -> Result<TwitchStreamPage, ProviderClientError> {
        let user_id = required_session_value(session.user_id.as_deref(), "Twitch user ID")?;
        let mut request = self.helix_get("streams/followed", session)?;
        request = request.query(&[
            ("user_id", user_id),
            ("first", &page_size.clamp(1, 100).to_string()),
        ]);
        if let Some(cursor) = non_empty(cursor) {
            request = request.query(&[("after", cursor)]);
        }
        self.stream_page(request).await
    }

    pub async fn category_streams(
        &self,
        category_id: &str,
        cursor: Option<&str>,
        page_size: u32,
        session: &TwitchSession,
    ) -> Result<TwitchStreamPage, ProviderClientError> {
        let category_id = required_value(category_id, "Twitch category ID")?;
        let mut request = self.helix_get("streams", session)?.query(&[
            ("game_id", category_id),
            ("first", &page_size.clamp(1, 100).to_string()),
        ]);
        if let Some(cursor) = non_empty(cursor) {
            request = request.query(&[("after", cursor)]);
        }
        self.stream_page(request).await
    }

    pub async fn top_categories(
        &self,
        cursor: Option<&str>,
        page_size: u32,
        session: &TwitchSession,
    ) -> Result<TwitchCategoryPage, ProviderClientError> {
        let mut request = self
            .helix_get("games/top", session)?
            .query(&[("first", page_size.clamp(1, 100))]);
        if let Some(cursor) = non_empty(cursor) {
            request = request.query(&[("after", cursor)]);
        }
        let page: HelixPage<RawHelixCategory> = fetch_json(request).await?;
        Ok(TwitchCategoryPage {
            items: page
                .data
                .into_iter()
                .map(|item| TwitchCategory {
                    id: item.id,
                    name: item.name,
                    box_art_url: image_size(&item.box_art_url, 285, 380),
                })
                .collect(),
            next_cursor: page.pagination.cursor,
        })
    }

    pub async fn search_live_channels(
        &self,
        query: &str,
        cursor: Option<&str>,
        page_size: u32,
        session: &TwitchSession,
    ) -> Result<TwitchChannelSearchPage, ProviderClientError> {
        let query = required_value(query, "Twitch channel search query")?;
        let mut request = self.helix_get("search/channels", session)?.query(&[
            ("query", query),
            ("live_only", "true"),
            ("first", &page_size.clamp(1, 100).to_string()),
        ]);
        if let Some(cursor) = non_empty(cursor) {
            request = request.query(&[("after", cursor)]);
        }
        let page: HelixPage<RawHelixChannelSearchItem> = fetch_json(request).await?;
        Ok(TwitchChannelSearchPage {
            items: page
                .data
                .into_iter()
                .map(|item| TwitchChannelSearchItem {
                    user_id: item.id,
                    channel: item.broadcaster_login,
                    display_name: item.display_name,
                    title: item.title,
                    category_id: item.game_id,
                    category_name: item.game_name,
                    thumbnail_url: item.thumbnail_url,
                    is_live: item.is_live,
                    started_at: item.started_at,
                    language: item.broadcaster_language,
                    tags: item.tags,
                })
                .collect(),
            next_cursor: page.pagination.cursor,
        })
    }

    pub async fn schedule(
        &self,
        broadcaster_id: &str,
        cursor: Option<&str>,
        page_size: u32,
        session: &TwitchSession,
    ) -> Result<TwitchSchedulePage, ProviderClientError> {
        let broadcaster_id = required_value(broadcaster_id, "Twitch broadcaster ID")?;
        let mut request = self.helix_get("schedule", session)?.query(&[
            ("broadcaster_id", broadcaster_id),
            ("first", &page_size.clamp(1, 25).to_string()),
        ]);
        if let Some(cursor) = non_empty(cursor) {
            request = request.query(&[("after", cursor)]);
        }
        let response: RawHelixScheduleResponse = fetch_json(request).await?;
        Ok(TwitchSchedulePage {
            broadcaster_id: response.data.broadcaster_id,
            broadcaster_login: response.data.broadcaster_login,
            broadcaster_name: response.data.broadcaster_name,
            segments: response
                .data
                .segments
                .unwrap_or_default()
                .into_iter()
                .map(|segment| TwitchScheduleSegment {
                    id: segment.id,
                    start_time: segment.start_time,
                    end_time: segment.end_time,
                    title: segment.title,
                    category_id: segment.category.as_ref().map(|value| value.id.clone()),
                    category_name: segment.category.map(|value| value.name),
                    canceled_until: segment.canceled_until,
                    is_recurring: segment.is_recurring,
                })
                .collect(),
            next_cursor: response.pagination.cursor,
        })
    }

    async fn stream_page(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<TwitchStreamPage, ProviderClientError> {
        let page: HelixPage<RawHelixStream> = fetch_json(request).await?;
        Ok(TwitchStreamPage {
            items: page.data.into_iter().map(stream_item).collect(),
            next_cursor: page.pagination.cursor,
        })
    }

    fn helix_get(
        &self,
        path: &str,
        session: &TwitchSession,
    ) -> Result<reqwest::RequestBuilder, ProviderClientError> {
        let auth_token =
            required_session_value(session.auth_token.as_deref(), "Twitch auth token")?;
        let client_id = required_session_value(session.client_id.as_deref(), "Twitch client ID")?;
        Ok(self
            .http
            .get(format!(
                "{}/{}",
                self.endpoints.helix.trim_end_matches('/'),
                path
            ))
            .header("Client-ID", client_id)
            .bearer_auth(auth_token))
    }

    async fn channel_metadata(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchMetadata, ProviderClientError> {
        let data = self
            .persisted_query(
                "StreamMetadata",
                STREAM_METADATA_QUERY_HASH,
                json!({ "channelLogin": resource.id, "includeIsDJ": true }),
                session,
            )
            .await?;
        let user = data
            .get("user")
            .filter(|value| !value.is_null())
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: "Twitch channel was not found".to_string(),
            })?;
        let stream = user.get("stream").filter(|value| !value.is_null());
        let title = stream
            .and_then(|value| value.get("title"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| user.get("displayName").and_then(serde_json::Value::as_str))
            .unwrap_or(&resource.id)
            .to_string();
        Ok(TwitchMetadata {
            id: resource.id.clone(),
            title,
            author: user
                .get("displayName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&resource.id)
                .to_string(),
            game: stream
                .and_then(|value| value.pointer("/game/displayName"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            thumbnail_url: stream
                .and_then(|value| value.get("previewImageURL"))
                .or_else(|| user.get("profileImageURL"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            is_live: stream.is_some(),
            description: user
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            duration_seconds: None,
            view_count: stream
                .and_then(|value| value.get("viewersCount"))
                .and_then(value_as_u64),
            published_at: stream
                .and_then(|value| value.get("createdAt"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            chapters: Vec::new(),
            storyboard_url: None,
        })
    }

    async fn video_metadata(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchMetadata, ProviderClientError> {
        let variables = json!({ "channelLogin": "", "videoID": resource.id });
        let (metadata, chapters, storyboard) = tokio::try_join!(
            self.persisted_query(
                "VideoMetadata",
                VIDEO_METADATA_QUERY_HASH,
                variables,
                session,
            ),
            self.persisted_query(
                "VideoPlayer_ChapterSelectButtonVideo",
                VIDEO_CHAPTERS_QUERY_HASH,
                json!({ "includePrivate": false, "videoID": resource.id }),
                session,
            ),
            self.persisted_query(
                "VideoPlayer_VODSeekbarPreviewVideo",
                VIDEO_STORYBOARD_QUERY_HASH,
                json!({ "includePrivate": false, "videoID": resource.id }),
                session,
            ),
        )?;
        let video = metadata
            .get("video")
            .filter(|value| !value.is_null())
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: "Twitch video was not found".to_string(),
            })?;
        let chapter_items = chapters
            .pointer("/video/moments/edges")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|edge| {
                let node = edge.get("node")?;
                let start = node.get("positionMilliseconds").and_then(value_as_u64)? / 1000;
                let duration = node.get("durationMilliseconds").and_then(value_as_u64)? / 1000;
                Some(super::types::TwitchChapter {
                    title: node
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Chapter")
                        .to_string(),
                    start_seconds: start,
                    end_seconds: start.saturating_add(duration),
                })
            })
            .collect();
        Ok(TwitchMetadata {
            id: video
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&resource.id)
                .to_string(),
            title: video
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Untitled Broadcast")
                .to_string(),
            author: video
                .pointer("/owner/displayName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            game: video
                .pointer("/game/displayName")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            thumbnail_url: video
                .get("previewThumbnailURL")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            is_live: false,
            description: video
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            duration_seconds: video.get("lengthSeconds").and_then(value_as_u64),
            view_count: video.get("viewCount").and_then(value_as_u64),
            published_at: video
                .get("publishedAt")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            chapters: chapter_items,
            storyboard_url: storyboard
                .pointer("/video/seekPreviewsURL")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
        })
    }

    async fn persisted_query(
        &self,
        operation_name: &str,
        hash: &str,
        variables: serde_json::Value,
        session: Option<&TwitchSession>,
    ) -> Result<serde_json::Value, ProviderClientError> {
        let body = json!({
            "operationName": operation_name,
            "extensions": {
                "persistedQuery": { "version": 1, "sha256Hash": hash }
            },
            "variables": variables,
        });
        let envelope: GraphQlEnvelope<serde_json::Value> = self.graph_ql(&body, session).await?;
        envelope.data.ok_or_else(|| {
            ProviderClientError::Parse(format!("Twitch {operation_name} response is missing data"))
        })
    }

    async fn graph_ql<T: serde::de::DeserializeOwned>(
        &self,
        body: &serde_json::Value,
        session: Option<&TwitchSession>,
    ) -> Result<GraphQlEnvelope<T>, ProviderClientError> {
        for attempt in 0..GRAPHQL_MAX_ATTEMPTS {
            let request = Self::request(
                self.http
                    .post(&self.endpoints.gql)
                    .header("Client-ID", TWITCH_WEB_CLIENT_ID)
                    .json(body),
                session,
            );
            let envelope: GraphQlEnvelope<T> = fetch_json(request).await?;
            if envelope.errors.is_empty() {
                return Ok(envelope);
            }
            let message = graph_ql_error_message(&envelope.errors);
            if attempt + 1 == GRAPHQL_MAX_ATTEMPTS || !retryable_graph_ql_error(&message) {
                return Err(ProviderClientError::Api { code: -1, message });
            }
            tokio::time::sleep(GRAPHQL_RETRY_DELAY).await;
        }
        unreachable!("GraphQL retry loop always returns")
    }

    pub async fn access_token(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchAccessToken, ProviderClientError> {
        let (field, argument, id) = match resource.kind {
            TwitchResourceKind::Channel => (
                "streamPlaybackAccessToken",
                "channelName",
                resource.id.as_str(),
            ),
            TwitchResourceKind::Video => ("videoPlaybackAccessToken", "id", resource.id.as_str()),
            TwitchResourceKind::Clip => {
                return Err(ProviderClientError::InvalidConfig(
                    "Twitch clips use clip playback tokens".to_string(),
                ));
            }
        };
        let query = format!(
            "query {{ {field}({argument}: \"{id}\", params: {{ platform: \"web\", playerBackend: \"mediaplayer\", playerType: \"site\" }}) {{ value signature }} }}"
        );
        let request = Self::request(
            self.http
                .post(&self.endpoints.gql)
                .header("Client-ID", TWITCH_WEB_CLIENT_ID)
                .json(&json!({ "query": query })),
            session,
        );
        let envelope: GraphQlEnvelope<AccessTokenData> = fetch_json(request).await?;
        if !envelope.errors.is_empty() {
            return Err(ProviderClientError::Api {
                code: -1,
                message: envelope
                    .errors
                    .into_iter()
                    .map(|error| error.message)
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
        let data = envelope.data.ok_or_else(|| {
            ProviderClientError::Parse("Twitch token response is missing data".to_string())
        })?;
        let raw = match resource.kind {
            TwitchResourceKind::Channel => data.stream_playback_access_token,
            TwitchResourceKind::Video => data.video_playback_access_token,
            TwitchResourceKind::Clip => None,
        }
        .ok_or_else(|| ProviderClientError::Api {
            code: 404,
            message: "Twitch resource is offline, unavailable, or unauthorized".to_string(),
        })?;
        Ok(map_token(raw))
    }

    async fn clip_playback(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchPlayback, ProviderClientError> {
        let body = json!({
            "operationName": "VideoAccessToken_Clip",
            "extensions": {
                "persistedQuery": { "version": 1, "sha256Hash": CLIP_QUERY_HASH }
            },
            "variables": { "slug": resource.id, "platform": "web" }
        });
        let envelope: GraphQlEnvelope<ClipData> = self.graph_ql(&body, session).await?;
        let clip =
            envelope
                .data
                .and_then(|data| data.clip)
                .ok_or_else(|| ProviderClientError::Api {
                    code: 404,
                    message: "Twitch clip was not found".to_string(),
                })?;
        let raw_token = clip.playback_access_token.ok_or_else(|| {
            ProviderClientError::Parse("Twitch clip playback token is missing".to_string())
        })?;
        let token = TwitchAccessToken {
            signature: raw_token.signature,
            value: raw_token.value,
        };
        let qualities = clip
            .video_qualities
            .into_iter()
            .filter(|quality| !quality.source_url.is_empty())
            .map(|quality| {
                Ok(TwitchQuality {
                    name: quality.quality.unwrap_or_else(|| "clip".to_string()),
                    url: clip_quality_url(&quality.source_url, &token)?,
                    bandwidth: None,
                    width: None,
                    height: None,
                    frame_rate: quality.frame_rate.map(|value| value.to_string()),
                    codecs: None,
                })
            })
            .collect::<Result<Vec<_>, ProviderClientError>>()?;
        Ok(TwitchPlayback {
            resource: resource.clone(),
            master_url: None,
            qualities,
            token: Some(token),
        })
    }

    pub async fn clip_metadata(
        &self,
        resource: &TwitchResource,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchMetadata, ProviderClientError> {
        if resource.kind != TwitchResourceKind::Clip {
            return Err(ProviderClientError::InvalidConfig(
                "resource is not a Twitch clip".to_string(),
            ));
        }
        let body = json!({
            "operationName": "VideoAccessToken_Clip",
            "extensions": {
                "persistedQuery": { "version": 1, "sha256Hash": CLIP_QUERY_HASH }
            },
            "variables": { "slug": resource.id, "platform": "web" }
        });
        let envelope: GraphQlEnvelope<ClipData> = self.graph_ql(&body, session).await?;
        let clip =
            envelope
                .data
                .and_then(|data| data.clip)
                .ok_or_else(|| ProviderClientError::Api {
                    code: 404,
                    message: "Twitch clip was not found".to_string(),
                })?;
        let id = if clip.id.is_empty() {
            resource.id.clone()
        } else {
            clip.id
        };
        let title = if clip.title.is_empty() {
            id.clone()
        } else {
            clip.title
        };
        Ok(TwitchMetadata {
            id,
            title,
            author: clip
                .broadcaster
                .map_or_else(String::new, |value| value.display_name),
            game: clip.game.map(|value| value.name),
            thumbnail_url: clip.thumbnail_url,
            is_live: false,
            description: None,
            duration_seconds: clip.duration_seconds.and_then(nonnegative_seconds),
            view_count: clip.view_count,
            published_at: clip.created_at,
            chapters: Vec::new(),
            storyboard_url: None,
        })
    }

    pub async fn browse_channel(
        &self,
        channel: &str,
        kind: TwitchBrowseKind,
        cursor: Option<&str>,
        page_size: u32,
        session: Option<&TwitchSession>,
    ) -> Result<TwitchBrowsePage, ProviderClientError> {
        if !valid_channel(channel) {
            return Err(ProviderClientError::InvalidConfig(
                "invalid Twitch channel".to_string(),
            ));
        }
        let page_size = match kind {
            TwitchBrowseKind::Clips => page_size.clamp(1, 20),
            _ => page_size.clamp(1, 100),
        };
        let (operation_name, hash, variables, collection) = match kind {
            TwitchBrowseKind::Clips => (
                "ClipsCards__User",
                CLIPS_QUERY_HASH,
                json!({
                    "login": channel,
                    "criteria": { "filter": "ALL_TIME" },
                    "limit": page_size,
                    "cursor": cursor,
                }),
                "clips",
            ),
            TwitchBrowseKind::Videos | TwitchBrowseKind::Highlights | TwitchBrowseKind::Uploads => {
                let broadcast_type = match kind {
                    TwitchBrowseKind::Videos => serde_json::Value::Null,
                    TwitchBrowseKind::Highlights => json!("HIGHLIGHT"),
                    TwitchBrowseKind::Uploads => json!("UPLOAD"),
                    TwitchBrowseKind::Clips => unreachable!(),
                };
                (
                    "FilterableVideoTower_Videos",
                    VIDEOS_QUERY_HASH,
                    json!({
                        "channelOwnerLogin": channel,
                        "broadcastType": broadcast_type,
                        "videoSort": "TIME",
                        "limit": page_size,
                        "cursor": cursor,
                    }),
                    "videos",
                )
            }
        };
        let body = json!({
            "operationName": operation_name,
            "extensions": {
                "persistedQuery": { "version": 1, "sha256Hash": hash }
            },
            "variables": variables,
        });
        let envelope: GraphQlEnvelope<serde_json::Value> = self.graph_ql(&body, session).await?;
        let data = envelope.data.ok_or_else(|| {
            ProviderClientError::Parse("Twitch browse response is missing data".to_string())
        })?;
        if data
            .pointer("/user/id")
            .is_some_and(serde_json::Value::is_null)
        {
            return Err(ProviderClientError::Api {
                code: 404,
                message: "Twitch channel was not found".to_string(),
            });
        }
        let edges = data
            .pointer(&format!("/user/{collection}/edges"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                ProviderClientError::Parse(format!(
                    "Twitch browse response is missing {collection} edges"
                ))
            })?;
        let mut items = Vec::with_capacity(edges.len());
        let mut next_cursor = None;
        for edge in edges {
            let Some(node) = edge.get("node") else {
                continue;
            };
            let is_clip = kind == TwitchBrowseKind::Clips;
            let id_field = if is_clip { "slug" } else { "id" };
            let Some(id) = node.get(id_field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            next_cursor = edge
                .get("cursor")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
                .or(next_cursor);
            items.push(TwitchBrowseItem {
                resource: TwitchResource {
                    kind: if is_clip {
                        TwitchResourceKind::Clip
                    } else {
                        TwitchResourceKind::Video
                    },
                    id: id.to_string(),
                },
                title: node
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Untitled")
                    .to_string(),
                thumbnail_url: node
                    .get(if is_clip {
                        "thumbnailURL"
                    } else {
                        "previewThumbnailURL"
                    })
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                duration_seconds: node
                    .get(if is_clip {
                        "durationSeconds"
                    } else {
                        "lengthSeconds"
                    })
                    .and_then(value_as_u64),
                view_count: node.get("viewCount").and_then(value_as_u64),
                published_at: node
                    .get(if is_clip { "createdAt" } else { "publishedAt" })
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
            });
        }
        Ok(TwitchBrowsePage { items, next_cursor })
    }

    fn usher_url(
        &self,
        resource: &TwitchResource,
        token: &TwitchAccessToken,
    ) -> Result<String, ProviderClientError> {
        let path = match resource.kind {
            TwitchResourceKind::Channel => {
                format!("/api/v2/channel/hls/{}.m3u8", resource.id)
            }
            TwitchResourceKind::Video => format!("/vod/v2/{}.m3u8", resource.id),
            TwitchResourceKind::Clip => {
                return Err(ProviderClientError::InvalidConfig(
                    "Twitch clips have direct quality URLs".to_string(),
                ));
            }
        };
        let mut url = Url::parse(&format!(
            "{}{}",
            self.endpoints.usher.trim_end_matches('/'),
            path
        ))
        .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos());
        url.query_pairs_mut()
            .append_pair("allow_source", "true")
            .append_pair("allow_audio_only", "true")
            .append_pair("allow_spectre", "true")
            .append_pair("playlist_include_framerate", "true")
            .append_pair("supported_codecs", "av1,h265,h264")
            .append_pair("platform", "web")
            .append_pair("player", "twitchweb")
            .append_pair("p", &nonce.to_string())
            .append_pair("sig", &token.signature)
            .append_pair("token", &token.value);
        Ok(url.to_string())
    }

    fn request(
        mut request: reqwest::RequestBuilder,
        session: Option<&TwitchSession>,
    ) -> reqwest::RequestBuilder {
        if let Some(session) = session {
            if let Some(token) = session.auth_token.as_deref() {
                request = request.header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"));
            }
            if let Some(device_id) = session.device_id.as_deref() {
                request = request.header("Device-ID", device_id);
            }
            if let Some(integrity) = session.client_integrity.as_deref() {
                request = request.header("Client-Integrity", integrity);
            }
        }
        request
    }
}

fn clip_quality_url(
    source_url: &str,
    token: &TwitchAccessToken,
) -> Result<String, ProviderClientError> {
    let mut url = Url::parse(source_url)
        .map_err(|error| ProviderClientError::Parse(format!("invalid Twitch clip URL: {error}")))?;
    url.query_pairs_mut()
        .append_pair("sig", &token.signature)
        .append_pair("token", &token.value);
    Ok(url.to_string())
}

fn value_as_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().and_then(nonnegative_seconds))
        .or_else(|| value.as_str()?.parse().ok())
}

fn graph_ql_error_message(errors: &[GraphQlError]) -> String {
    errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

fn retryable_graph_ql_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("service error")
        || message.contains("internal server error")
        || message.contains("temporarily unavailable")
        || message.contains("timeout")
}

fn nonnegative_seconds(value: f64) -> Option<u64> {
    std::time::Duration::try_from_secs_f64(value)
        .ok()
        .map(|duration| duration.as_secs())
}

fn stream_item(item: RawHelixStream) -> TwitchStreamItem {
    TwitchStreamItem {
        stream_id: item.id,
        user_id: item.user_id,
        channel: item.user_login,
        display_name: item.user_name,
        title: item.title,
        category_id: item.game_id,
        category_name: item.game_name,
        thumbnail_url: image_size(&item.thumbnail_url, 640, 360),
        viewer_count: item.viewer_count,
        started_at: item.started_at,
        language: item.language,
        tags: item.tags,
        is_mature: item.is_mature,
    }
}

fn image_size(value: &str, width: u32, height: u32) -> String {
    value
        .replace("{width}", &width.to_string())
        .replace("{height}", &height.to_string())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn required_value<'a>(value: &'a str, name: &str) -> Result<&'a str, ProviderClientError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ProviderClientError::InvalidConfig(format!(
            "{name} is required"
        )))
    } else {
        Ok(value)
    }
}

fn required_session_value<'a>(
    value: Option<&'a str>,
    name: &str,
) -> Result<&'a str, ProviderClientError> {
    required_value(value.unwrap_or_default(), name)
}

fn valid_channel(value: &str) -> bool {
    let len = value.len();
    (3..=25).contains(&len)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn resource(kind: TwitchResourceKind, id: &str) -> Result<TwitchResource, ProviderClientError> {
    if id.is_empty() {
        Err(ProviderClientError::InvalidConfig(
            "Twitch resource id is empty".to_string(),
        ))
    } else {
        Ok(TwitchResource {
            kind,
            id: id.to_string(),
        })
    }
}

fn map_token(raw: RawAccessToken) -> TwitchAccessToken {
    TwitchAccessToken {
        signature: raw.signature,
        value: raw.value,
    }
}

fn parse_master_playlist(
    playlist: &str,
    master_url: &str,
) -> Result<Vec<TwitchQuality>, ProviderClientError> {
    let base = Url::parse(master_url)
        .map_err(|error| ProviderClientError::Parse(format!("invalid Twitch HLS URL: {error}")))?;
    let mut qualities = Vec::new();
    let mut pending = None;
    for line in playlist
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(parse_hls_attributes(attributes));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let Some(attributes) = pending.take() else {
            continue;
        };
        let url = base
            .join(line)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?
            .to_string();
        let resolution = attributes.get("RESOLUTION").and_then(|value| {
            let (width, height) = value.split_once('x')?;
            Some((width.parse().ok()?, height.parse().ok()?))
        });
        let name = attributes
            .get("VIDEO")
            .or_else(|| attributes.get("NAME"))
            .cloned()
            .or_else(|| resolution.map(|(_, height)| format!("{height}p")))
            .unwrap_or_else(|| "auto".to_string());
        qualities.push(TwitchQuality {
            name,
            url,
            bandwidth: attributes
                .get("BANDWIDTH")
                .and_then(|value| value.parse().ok()),
            width: resolution.map(|value| value.0),
            height: resolution.map(|value| value.1),
            frame_rate: attributes.get("FRAME-RATE").cloned(),
            codecs: attributes.get("CODECS").cloned(),
        });
    }
    if qualities.is_empty() {
        return Err(ProviderClientError::Parse(
            "Twitch HLS master playlist has no variants".to_string(),
        ));
    }
    Ok(qualities)
}

fn parse_hls_attributes(input: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, character) in input.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                insert_hls_attribute(&mut values, &input[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    insert_hls_attribute(&mut values, &input[start..]);
    values
}

fn insert_hls_attribute(values: &mut HashMap<String, String>, raw: &str) {
    if let Some((key, value)) = raw.split_once('=') {
        values.insert(
            key.trim().to_string(),
            value.trim().trim_matches('"').to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_channel_video_and_clip_urls() {
        assert_eq!(
            TwitchClient::parse_resource("https://www.twitch.tv/synctv")
                .expect("channel should parse"),
            TwitchResource {
                kind: TwitchResourceKind::Channel,
                id: "synctv".to_string(),
            }
        );
        assert_eq!(
            TwitchClient::parse_resource("https://www.twitch.tv/videos/123456")
                .expect("video should parse")
                .kind,
            TwitchResourceKind::Video
        );
        assert_eq!(
            TwitchClient::parse_resource("https://clips.twitch.tv/ClipSlug")
                .expect("clip should parse")
                .kind,
            TwitchResourceKind::Clip
        );
    }

    #[test]
    fn parses_twitch_hls_variants() {
        let playlist = r#"#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=6000000,CODECS="avc1.64002A,mp4a.40.2",RESOLUTION=1920x1080,VIDEO="chunked",FRAME-RATE=60.000
https://video.example.test/chunked/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,VIDEO="720p60"
720/index.m3u8
"#;
        let values = parse_master_playlist(playlist, "https://usher.example.test/master.m3u8")
            .expect("playlist should parse");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].name, "chunked");
        assert_eq!(values[0].height, Some(1080));
        assert_eq!(values[1].url, "https://usher.example.test/720/index.m3u8");
    }

    #[tokio::test]
    async fn resolves_live_token_and_quality_playlist() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/gql"))
            .and(matchers::header("Client-ID", TWITCH_WEB_CLIENT_ID))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "streamPlaybackAccessToken": {
                        "signature": "signature",
                        "value": "{\"channel\":\"synctv\"}"
                    }
                }
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/api/v2/channel/hls/synctv.m3u8"))
            .and(matchers::query_param("sig", "signature"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080,VIDEO=chunked\nchunked/index.m3u8\n",
            ))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );
        let playback = client
            .playback(
                &TwitchResource {
                    kind: TwitchResourceKind::Channel,
                    id: "synctv".to_string(),
                },
                None,
            )
            .await
            .expect("live playback should resolve");
        assert_eq!(playback.qualities.len(), 1);
        assert_eq!(playback.qualities[0].height, Some(1080));
        assert_eq!(
            playback.token.expect("token should exist").signature,
            "signature"
        );
    }

    #[tokio::test]
    async fn rejects_oversized_quality_playlist() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/gql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "streamPlaybackAccessToken": {
                        "signature": "signature",
                        "value": "{\"channel\":\"synctv\"}"
                    }
                }
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/api/v2/channel/hls/synctv.m3u8"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'x';
                crate::MAX_RESPONSE_SIZE
                    + 1
            ]))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );

        let error = client
            .playback(
                &TwitchResource {
                    kind: TwitchResourceKind::Channel,
                    id: "synctv".to_string(),
                },
                None,
            )
            .await
            .expect_err("oversized playlist should fail");
        assert!(matches!(
            error,
            ProviderClientError::ResponseTooLarge { .. }
        ));
    }

    #[tokio::test]
    async fn browses_channel_videos_with_cursor() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/gql"))
            .and(matchers::body_partial_json(json!({
                "operationName": "FilterableVideoTower_Videos"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "user": {
                        "id": "user-1",
                        "videos": {
                            "edges": [{
                                "cursor": "next-1",
                                "node": {
                                    "id": "123456",
                                    "title": "Past broadcast",
                                    "previewThumbnailURL": "https://image.example.test/vod.jpg",
                                    "lengthSeconds": 3600,
                                    "viewCount": 42,
                                    "publishedAt": "2026-01-01T00:00:00Z"
                                }
                            }]
                        }
                    }
                }
            })))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );
        let page = client
            .browse_channel("synctv", TwitchBrowseKind::Videos, None, 50, None)
            .await
            .expect("videos should browse");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].resource.id, "123456");
        assert_eq!(page.next_cursor.as_deref(), Some("next-1"));
    }

    #[tokio::test]
    async fn browsed_channel_clips_use_playable_slug_targets() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/gql"))
            .and(matchers::body_partial_json(json!({
                "operationName": "ClipsCards__User"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "user": {
                        "id": "user-1",
                        "clips": {"edges": [{"node": {
                            "id": "1323590834",
                            "slug": "DepressedAbnegateElkUWot",
                            "title": "Playable clip"
                        }}]}
                    }
                }
            })))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );

        let page = client
            .browse_channel("synctv", TwitchBrowseKind::Clips, None, 1, None)
            .await
            .expect("clips should browse");

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].resource.kind, TwitchResourceKind::Clip);
        assert_eq!(page.items[0].resource.id, "DepressedAbnegateElkUWot");
    }

    #[tokio::test]
    async fn retries_transient_graphql_service_errors() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let matcher = || {
            Mock::given(matchers::method("POST"))
                .and(matchers::path("/gql"))
                .and(matchers::body_partial_json(json!({
                    "operationName": "FilterableVideoTower_Videos"
                })))
        };
        matcher()
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{"message": "service error"}]
            })))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        matcher()
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "user": {
                        "id": "user-1",
                        "videos": {"edges": [{"node": {
                            "id": "123456",
                            "title": "Recovered highlight"
                        }}]}
                    }
                }
            })))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );

        let page = client
            .browse_channel("synctv", TwitchBrowseKind::Highlights, None, 1, None)
            .await
            .expect("transient Twitch service errors should be retried");

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].resource.id, "123456");
    }

    #[tokio::test]
    async fn loads_vod_metadata_chapters_and_storyboard() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/gql"))
            .and(matchers::body_partial_json(
                json!({ "operationName": "VideoMetadata" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "video": {
                    "id": "123456",
                    "title": "Past broadcast",
                    "description": "A complete VOD",
                    "lengthSeconds": 3600,
                    "viewCount": 42,
                    "publishedAt": "2026-01-01T00:00:00Z",
                    "previewThumbnailURL": "https://image.example.test/vod.jpg",
                    "owner": { "displayName": "SyncTV" },
                    "game": { "displayName": "Software and Game Development" }
                }}
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::body_partial_json(json!({
                "operationName": "VideoPlayer_ChapterSelectButtonVideo"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "video": { "moments": { "edges": [{ "node": {
                    "description": "Chapter one",
                    "positionMilliseconds": 10000,
                    "durationMilliseconds": 20000
                }}] } } }
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::body_partial_json(json!({
                "operationName": "VideoPlayer_VODSeekbarPreviewVideo"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "video": {
                    "seekPreviewsURL": "https://storyboard.example.test/index.json"
                }}
            })))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );

        let metadata = client
            .metadata(
                &TwitchResource {
                    kind: TwitchResourceKind::Video,
                    id: "123456".to_string(),
                },
                None,
            )
            .await
            .expect("VOD metadata should load");
        assert_eq!(metadata.duration_seconds, Some(3600));
        assert_eq!(metadata.chapters.len(), 1);
        assert_eq!(metadata.chapters[0].start_seconds, 10);
        assert_eq!(metadata.chapters[0].end_seconds, 30);
        assert_eq!(
            metadata.storyboard_url.as_deref(),
            Some("https://storyboard.example.test/index.json")
        );
    }

    #[tokio::test]
    async fn loads_clip_metadata() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/gql"))
            .and(matchers::body_partial_json(
                json!({ "operationName": "VideoAccessToken_Clip" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "clip": {
                    "id": "ClipSlug",
                    "broadcaster": { "displayName": "SyncTV" },
                    "game": { "name": "Software and Game Development" },
                    "thumbnailURL": "https://image.example.test/clip.jpg",
                    "durationSeconds": 61.75,
                    "viewCount": 42,
                    "createdAt": "2026-01-01T00:00:00Z",
                    "playbackAccessToken": {
                        "signature": "clip-signature",
                        "value": "{\"clip\":\"token\"}"
                    },
                    "videoQualities": [{
                        "frameRate": 30,
                        "quality": "1080",
                        "sourceURL": "https://video.example.test/clip.mp4"
                    }]
                }}
            })))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );

        let metadata = client
            .metadata(
                &TwitchResource {
                    kind: TwitchResourceKind::Clip,
                    id: "ClipSlug".to_string(),
                },
                None,
            )
            .await
            .expect("clip metadata should load");
        assert_eq!(metadata.duration_seconds, Some(61));
        assert_eq!(metadata.title, "ClipSlug");
        assert_eq!(metadata.view_count, Some(42));
        assert_eq!(
            metadata.thumbnail_url.as_deref(),
            Some("https://image.example.test/clip.jpg")
        );
        assert_eq!(
            metadata.published_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );

        let playback = client
            .playback(
                &TwitchResource {
                    kind: TwitchResourceKind::Clip,
                    id: "ClipSlug".to_string(),
                },
                None,
            )
            .await
            .expect("clip playback should load");
        assert_eq!(playback.qualities.len(), 1);
        let playback_url = Url::parse(&playback.qualities[0].url).expect("valid playback URL");
        assert_eq!(playback_url.path(), "/clip.mp4");
        let query = playback_url.query_pairs().collect::<HashMap<_, _>>();
        assert_eq!(query.get("sig").map(AsRef::as_ref), Some("clip-signature"));
        assert_eq!(
            query.get("token").map(AsRef::as_ref),
            Some("{\"clip\":\"token\"}")
        );
    }

    #[tokio::test]
    async fn helix_discovery_uses_typed_native_endpoints_and_cursor() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/helix/streams/followed"))
            .and(matchers::query_param("user_id", "user-1"))
            .and(matchers::query_param("after", "cursor-1"))
            .and(matchers::header("Authorization", "Bearer oauth-token"))
            .and(matchers::header("Client-ID", "web-client"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "stream-1",
                    "user_id": "broadcaster-1",
                    "user_login": "synctv",
                    "user_name": "SyncTV",
                    "game_id": "game-1",
                    "game_name": "Development",
                    "type": "live",
                    "title": "Building SyncTV",
                    "viewer_count": 42,
                    "started_at": "2026-07-14T00:00:00Z",
                    "language": "en",
                    "thumbnail_url": "https://image/{width}x{height}.jpg",
                    "tags": ["Rust"],
                    "is_mature": false
                }],
                "pagination": { "cursor": "cursor-2" }
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/helix/games/top"))
            .and(matchers::header("Authorization", "Bearer oauth-token"))
            .and(matchers::header("Client-ID", "web-client"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "game-1",
                    "name": "Development",
                    "box_art_url": "https://box/{width}x{height}.jpg",
                    "igdb_id": ""
                }],
                "pagination": {}
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/helix/search/channels"))
            .and(matchers::query_param("query", "sync"))
            .and(matchers::query_param("live_only", "true"))
            .and(matchers::header("Authorization", "Bearer oauth-token"))
            .and(matchers::header("Client-ID", "web-client"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "broadcaster-1",
                    "broadcaster_login": "synctv",
                    "display_name": "SyncTV",
                    "title": "Building SyncTV",
                    "game_id": "game-1",
                    "game_name": "Development",
                    "thumbnail_url": "https://profile.jpg",
                    "is_live": true,
                    "started_at": "2026-07-14T00:00:00Z",
                    "broadcaster_language": "en",
                    "tags": ["Rust"]
                }],
                "pagination": {}
            })))
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/helix/schedule"))
            .and(matchers::query_param("broadcaster_id", "broadcaster-1"))
            .and(matchers::header("Authorization", "Bearer oauth-token"))
            .and(matchers::header("Client-ID", "web-client"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "segments": [{
                        "id": "segment-1",
                        "start_time": "2026-07-15T00:00:00Z",
                        "end_time": "2026-07-15T02:00:00Z",
                        "title": "Release stream",
                        "canceled_until": null,
                        "category": { "id": "game-1", "name": "Development" },
                        "is_recurring": true
                    }],
                    "broadcaster_id": "broadcaster-1",
                    "broadcaster_name": "SyncTV",
                    "broadcaster_login": "synctv",
                    "vacation": null
                },
                "pagination": {}
            })))
            .mount(&server)
            .await;
        let client = TwitchClient::with_http_client(reqwest::Client::new()).with_endpoints(
            TwitchEndpoints {
                gql: format!("{}/gql", server.uri()),
                usher: server.uri(),
                helix: format!("{}/helix", server.uri()),
                oauth_validate: format!("{}/oauth2/validate", server.uri()),
            },
        );
        let session = TwitchSession {
            user_id: Some("user-1".to_string()),
            client_id: Some("web-client".to_string()),
            auth_token: Some("oauth-token".to_string()),
            ..TwitchSession::default()
        };

        let followed = client
            .followed_live(Some("cursor-1"), 20, &session)
            .await
            .expect("followed live should load");
        assert_eq!(followed.items[0].channel, "synctv");
        assert_eq!(followed.items[0].thumbnail_url, "https://image/640x360.jpg");
        assert_eq!(followed.next_cursor.as_deref(), Some("cursor-2"));

        let categories = client
            .top_categories(None, 20, &session)
            .await
            .expect("top categories should load");
        assert_eq!(categories.items[0].box_art_url, "https://box/285x380.jpg");

        let search = client
            .search_live_channels("sync", None, 20, &session)
            .await
            .expect("channel search should load");
        assert!(search.items[0].is_live);

        let schedule = client
            .schedule("broadcaster-1", None, 20, &session)
            .await
            .expect("schedule should load");
        assert_eq!(schedule.broadcaster_login, "synctv");
        assert_eq!(
            schedule.segments[0].category_name.as_deref(),
            Some("Development")
        );
    }
}
