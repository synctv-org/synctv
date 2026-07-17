use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use moka::sync::Cache;
use regex::Regex;
use reqwest::{Method, Url};
use serde_json::json;
use sha1::{Digest, Sha1};

use crate::{check_response, fetch_json, text_with_limit, ProviderClientError};

use super::{
    YoutubeChallengeSolver, YoutubeListItem, YoutubeListPage, YoutubePlayerResponse,
    YoutubeThumbnail,
};

const INNERTUBE_API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const WEB_CLIENT_VERSION: &str = "2.20260708.00.00";
const WEB_SAFARI_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)";
const ANDROID_VR_CLIENT_VERSION: &str = "1.65.10";
const ANDROID_VR_USER_AGENT: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";

#[derive(Debug, Clone, Copy)]
enum YoutubePlayerClient {
    WebSafari,
    AndroidVr,
}

impl YoutubePlayerClient {
    const fn name(self) -> &'static str {
        match self {
            Self::WebSafari => "WEB",
            Self::AndroidVr => "ANDROID_VR",
        }
    }

    const fn version(self) -> &'static str {
        match self {
            Self::WebSafari => WEB_CLIENT_VERSION,
            Self::AndroidVr => ANDROID_VR_CLIENT_VERSION,
        }
    }

    const fn numeric_id(self) -> &'static str {
        match self {
            Self::WebSafari => "1",
            Self::AndroidVr => "28",
        }
    }

    const fn user_agent(self) -> &'static str {
        match self {
            Self::WebSafari => WEB_SAFARI_USER_AGENT,
            Self::AndroidVr => ANDROID_VR_USER_AGENT,
        }
    }

    const fn supports_cookies(self) -> bool {
        matches!(self, Self::WebSafari)
    }

    fn context(self) -> serde_json::Value {
        match self {
            Self::WebSafari => json!({
                "clientName": self.name(),
                "clientVersion": self.version(),
                "userAgent": self.user_agent(),
                "hl": "en",
                "gl": "US",
            }),
            Self::AndroidVr => json!({
                "clientName": self.name(),
                "clientVersion": self.version(),
                "deviceMake": "Oculus",
                "deviceModel": "Quest 3",
                "androidSdkVersion": 32,
                "userAgent": self.user_agent(),
                "osName": "Android",
                "osVersion": "12L",
                "hl": "en",
                "gl": "US",
            }),
        }
    }
}

#[derive(Clone)]
pub struct YoutubeClient {
    client: reqwest::Client,
    endpoint: Url,
    challenge_cache: Cache<String, Arc<YoutubeChallengeSolver>>,
}

impl YoutubeClient {
    pub fn with_http_client(client: reqwest::Client) -> Result<Self, ProviderClientError> {
        Self::with_endpoint("https://www.youtube.com/", client)
    }

    pub fn with_endpoint(
        endpoint: &str,
        client: reqwest::Client,
    ) -> Result<Self, ProviderClientError> {
        let mut endpoint = Url::parse(endpoint.trim()).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("Invalid YouTube endpoint: {error}"))
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(ProviderClientError::InvalidConfig(
                "YouTube endpoint must be an HTTP(S) URL".to_string(),
            ));
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        endpoint.set_path("/");
        Ok(Self {
            client,
            endpoint,
            challenge_cache: Cache::new(16),
        })
    }

    pub async fn player(
        &self,
        input: &str,
        visitor_data: Option<&str>,
        po_token: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<YoutubePlayerResponse, ProviderClientError> {
        let video_id = normalize_video_id(input)?;
        let mut last_response = None;
        for player_client in [
            YoutubePlayerClient::WebSafari,
            YoutubePlayerClient::AndroidVr,
        ] {
            let response = self
                .request_player(&video_id, visitor_data, po_token, cookie, player_client)
                .await?;
            if response.playability_status.is_playable() {
                return self
                    .resolve_player_response(&video_id, response, cookie)
                    .await;
            }
            last_response = Some(response);
        }
        let response = last_response.expect("YouTube player client list is non-empty");
        let message = if response.playability_status.reason.is_empty() {
            response.playability_status.status
        } else {
            response.playability_status.reason
        };
        Err(ProviderClientError::Api {
            code: 0,
            message: format!("YouTube video is unavailable: {message}"),
        })
    }

    async fn request_player(
        &self,
        video_id: &str,
        visitor_data: Option<&str>,
        po_token: Option<&str>,
        cookie: Option<&str>,
        player_client: YoutubePlayerClient,
    ) -> Result<YoutubePlayerResponse, ProviderClientError> {
        let mut client = player_client.context();
        if let Some(visitor_data) = visitor_data.filter(|value| !value.trim().is_empty()) {
            client["visitorData"] = json!(visitor_data.trim());
        }
        let mut body = json!({
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
            "context": {
                "client": client,
                "user": {"lockedSafetyMode": false},
                "request": {"useSsl": true}
            },
            "playbackContext": {
                "contentPlaybackContext": {
                    "html5Preference": "HTML5_PREF_WANTS"
                }
            }
        });
        if let Some(token) = po_token.filter(|value| !value.trim().is_empty()) {
            body["serviceIntegrityDimensions"] = json!({"poToken": token.trim()});
        }
        let url = self
            .endpoint
            .join("youtubei/v1/player")
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let mut request = self
            .client
            .request(Method::POST, url)
            .header(reqwest::header::USER_AGENT, player_client.user_agent())
            .header("x-youtube-client-name", player_client.numeric_id())
            .header("x-youtube-client-version", player_client.version())
            .query(&[("key", INNERTUBE_API_KEY), ("prettyPrint", "false")])
            .json(&body);
        if player_client.supports_cookies() {
            request = with_youtube_cookie_auth(request, cookie);
        }
        fetch_json(request).await
    }

    async fn resolve_player_response(
        &self,
        video_id: &str,
        mut response: YoutubePlayerResponse,
        cookie: Option<&str>,
    ) -> Result<YoutubePlayerResponse, ProviderClientError> {
        if player_requires_challenge(&response) {
            let solver = self.challenge_solver(video_id, cookie).await?;
            resolve_player_urls(&mut response, &solver)?;
        }
        Ok(response)
    }

    async fn challenge_solver(
        &self,
        video_id: &str,
        cookie: Option<&str>,
    ) -> Result<Arc<YoutubeChallengeSolver>, ProviderClientError> {
        let watch_url = self
            .endpoint
            .join("watch")
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let watch_response = with_youtube_cookie_auth(
            self.client
                .get(watch_url)
                .header(reqwest::header::USER_AGENT, WEB_SAFARI_USER_AGENT)
                .query(&[("v", video_id)]),
            cookie,
        )
        .send()
        .await?;
        let watch_html = text_with_limit(check_response(watch_response).await?).await?;
        let player_js_path = extract_player_js_path(&watch_html)?;
        let player_js_url = self
            .endpoint
            .join(&player_js_path)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        let cache_key = player_js_url.to_string();
        if let Some(solver) = self.challenge_cache.get(&cache_key) {
            return Ok(solver);
        }

        let player_response = with_youtube_cookie_auth(
            self.client
                .get(player_js_url)
                .header(reqwest::header::USER_AGENT, WEB_SAFARI_USER_AGENT),
            cookie,
        )
        .send()
        .await?;
        let player_js = text_with_limit(check_response(player_response).await?).await?;
        let solver = Arc::new(YoutubeChallengeSolver::prepare(&player_js)?);
        self.challenge_cache.insert(cache_key, solver.clone());
        Ok(solver)
    }

    pub async fn playlist(
        &self,
        playlist_id: &str,
        cursor: Option<&str>,
        visitor_data: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<YoutubeListPage, ProviderClientError> {
        let playlist_id = playlist_id.trim();
        if playlist_id.is_empty()
            || !playlist_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(ProviderClientError::InvalidConfig(
                "YouTube playlist ID is invalid".to_string(),
            ));
        }
        let query = cursor.filter(|value| !value.trim().is_empty()).map_or_else(
            || json!({"browseId": format!("VL{playlist_id}")}),
            continuation_query,
        );
        self.list_api("browse", query, visitor_data, cookie).await
    }

    pub async fn channel(
        &self,
        browse_id: &str,
        tab: super::types::YoutubeChannelTab,
        cursor: Option<&str>,
        visitor_data: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<YoutubeListPage, ProviderClientError> {
        let browse_id = normalize_channel_id(browse_id)?;
        let query = cursor.filter(|value| !value.trim().is_empty()).map_or_else(
            || json!({"browseId": browse_id, "params": tab.params()}),
            continuation_query,
        );
        self.list_api("browse", query, visitor_data, cookie).await
    }

    pub async fn feed(
        &self,
        browse_id: &str,
        cursor: Option<&str>,
        visitor_data: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<YoutubeListPage, ProviderClientError> {
        let query = cursor
            .filter(|value| !value.trim().is_empty())
            .map_or_else(|| json!({"browseId": browse_id}), continuation_query);
        self.list_api("browse", query, visitor_data, cookie).await
    }

    pub async fn search(
        &self,
        query: &str,
        cursor: Option<&str>,
        visitor_data: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<YoutubeListPage, ProviderClientError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "YouTube search query is required".to_string(),
            ));
        }
        let body = cursor.filter(|value| !value.trim().is_empty()).map_or_else(
            || json!({"query": query, "params": "EgIQAQ%3D%3D"}),
            continuation_query,
        );
        self.list_api("search", body, visitor_data, cookie).await
    }

    async fn list_api(
        &self,
        endpoint: &str,
        mut query: serde_json::Value,
        visitor_data: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<YoutubeListPage, ProviderClientError> {
        let mut client = json!({
            "clientName": "WEB",
            "clientVersion": WEB_CLIENT_VERSION,
            "userAgent": WEB_SAFARI_USER_AGENT,
            "hl": "en",
            "gl": "US",
        });
        if let Some(visitor_data) = visitor_data.filter(|value| !value.trim().is_empty()) {
            client["visitorData"] = json!(visitor_data.trim());
        }
        query["context"] = json!({"client": client});
        let url = self
            .endpoint
            .join(&format!("youtubei/v1/{endpoint}"))
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let request = self
            .client
            .request(Method::POST, url)
            .header(reqwest::header::USER_AGENT, WEB_SAFARI_USER_AGENT)
            .header("x-youtube-client-name", "1")
            .header("x-youtube-client-version", WEB_CLIENT_VERSION)
            .query(&[("key", INNERTUBE_API_KEY), ("prettyPrint", "false")])
            .json(&query);
        let value: serde_json::Value =
            fetch_json(with_youtube_cookie_auth(request, cookie)).await?;
        Ok(parse_list_page(&value))
    }
}

fn continuation_query(cursor: &str) -> serde_json::Value {
    json!({
        "continuation": percent_encoding::percent_decode_str(cursor.trim())
            .decode_utf8_lossy()
    })
}

fn with_youtube_cookie_auth(
    request: reqwest::RequestBuilder,
    cookie: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(cookie) = cookie.map(str::trim).filter(|value| !value.is_empty()) else {
        return request;
    };
    let request = request.header(reqwest::header::COOKIE, cookie);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Some(authorization) = youtube_cookie_authorization(cookie, timestamp) {
        request
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header(reqwest::header::ORIGIN, "https://www.youtube.com")
            .header("x-goog-authuser", "0")
    } else {
        request
    }
}

fn youtube_cookie_authorization(cookie: &str, timestamp: u64) -> Option<String> {
    let sapisid = youtube_cookie_value(cookie, "SAPISID")
        .or_else(|| youtube_cookie_value(cookie, "__Secure-3PAPISID"));
    [
        ("SAPISIDHASH", sapisid),
        (
            "SAPISID1PHASH",
            youtube_cookie_value(cookie, "__Secure-1PAPISID"),
        ),
        (
            "SAPISID3PHASH",
            youtube_cookie_value(cookie, "__Secure-3PAPISID"),
        ),
    ]
    .into_iter()
    .filter_map(|(scheme, sid)| {
        sid.map(|sid| {
            let digest = Sha1::digest(format!("{timestamp} {sid} https://www.youtube.com"));
            format!("{scheme} {timestamp}_{}", hex::encode(digest))
        })
    })
    .reduce(|mut authorization, value| {
        authorization.push(' ');
        authorization.push_str(&value);
        authorization
    })
}

fn youtube_cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

fn player_requires_challenge(response: &YoutubePlayerResponse) -> bool {
    let urls = response
        .streaming_data
        .iter()
        .flat_map(|streaming| {
            streaming
                .formats
                .iter()
                .chain(&streaming.adaptive_formats)
                .filter_map(|format| format.url.as_deref())
                .chain(streaming.hls_manifest_url.as_deref())
                .chain(streaming.dash_manifest_url.as_deref())
        })
        .chain(
            response
                .captions
                .iter()
                .filter_map(|captions| captions.player_captions_tracklist_renderer.as_ref())
                .flat_map(|tracklist| tracklist.caption_tracks.iter())
                .map(|track| track.base_url.as_str()),
        );
    response.streaming_data.as_ref().is_some_and(|streaming| {
        streaming
            .formats
            .iter()
            .chain(&streaming.adaptive_formats)
            .any(|format| format.signature_cipher.is_some())
    }) || urls.filter_map(|url| Url::parse(url).ok()).any(|url| {
        url.query_pairs()
            .any(|(key, value)| key == "n" && !value.is_empty())
    })
}

fn extract_player_js_path(watch_html: &str) -> Result<String, ProviderClientError> {
    static JSON_PATH: OnceLock<Regex> = OnceLock::new();
    static SCRIPT_PATH: OnceLock<Regex> = OnceLock::new();
    let json_path = JSON_PATH.get_or_init(|| {
        Regex::new(r#"\"(?:jsUrl|PLAYER_JS_URL)\"\s*:\s*\"([^\"]+)\""#)
            .expect("valid YouTube player URL regex")
    });
    if let Some(value) = json_path
        .captures(watch_html)
        .and_then(|captures| captures.get(1))
    {
        let quoted = format!("\"{}\"", value.as_str());
        return serde_json::from_str(&quoted).map_err(Into::into);
    }
    let script_path = SCRIPT_PATH.get_or_init(|| {
        Regex::new(r#"<script[^>]+src=\"([^\"]*/s/player/[^\"]+\.js)\""#)
            .expect("valid YouTube player script regex")
    });
    script_path
        .captures(watch_html)
        .and_then(|captures| captures.get(1))
        .map(|value| html_escape::decode_html_entities(value.as_str()).into_owned())
        .ok_or_else(|| {
            ProviderClientError::Parse(
                "YouTube watch page does not contain a Player JavaScript URL".to_string(),
            )
        })
}

fn resolve_player_urls(
    response: &mut YoutubePlayerResponse,
    solver: &YoutubeChallengeSolver,
) -> Result<(), ProviderClientError> {
    if let Some(streaming) = &mut response.streaming_data {
        for format in streaming
            .formats
            .iter_mut()
            .chain(&mut streaming.adaptive_formats)
        {
            let source_url = match (&format.url, &format.signature_cipher) {
                (Some(url), _) => url.clone(),
                (None, Some(cipher)) => resolve_signature_cipher(cipher, solver)?,
                (None, None) => continue,
            };
            format.url = Some(resolve_n_parameter(&source_url, solver)?);
        }
        if let Some(url) = streaming.hls_manifest_url.as_mut() {
            *url = resolve_n_parameter(url, solver)?;
        }
        if let Some(url) = streaming.dash_manifest_url.as_mut() {
            *url = resolve_n_parameter(url, solver)?;
        }
    }
    if let Some(tracklist) = response
        .captions
        .as_mut()
        .and_then(|captions| captions.player_captions_tracklist_renderer.as_mut())
    {
        for track in &mut tracklist.caption_tracks {
            track.base_url = resolve_n_parameter(&track.base_url, solver)?;
        }
    }
    Ok(())
}

fn resolve_signature_cipher(
    cipher: &str,
    solver: &YoutubeChallengeSolver,
) -> Result<String, ProviderClientError> {
    let values = url::form_urlencoded::parse(cipher.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let source_url = values.get("url").ok_or_else(|| {
        ProviderClientError::Parse("YouTube signatureCipher has no URL".to_string())
    })?;
    let signature = values.get("s").ok_or_else(|| {
        ProviderClientError::Parse("YouTube signatureCipher has no signature".to_string())
    })?;
    let signature_name = values.get("sp").map_or("signature", String::as_str);
    let mut url = Url::parse(source_url).map_err(|error| {
        ProviderClientError::Parse(format!("Invalid YouTube media URL: {error}"))
    })?;
    url.query_pairs_mut()
        .append_pair(signature_name, &solver.solve_signature(signature)?);
    Ok(url.into())
}

fn resolve_n_parameter(
    source_url: &str,
    solver: &YoutubeChallengeSolver,
) -> Result<String, ProviderClientError> {
    let mut url = Url::parse(source_url).map_err(|error| {
        ProviderClientError::Parse(format!("Invalid YouTube media URL: {error}"))
    })?;
    let pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if pairs.iter().all(|(key, _)| key != "n") {
        return Ok(source_url.to_string());
    }
    let mut query = url.query_pairs_mut();
    query.clear();
    for (key, value) in pairs {
        if key == "n" && !value.is_empty() {
            query.append_pair(&key, &solver.solve_n(&value)?);
        } else {
            query.append_pair(&key, &value);
        }
    }
    drop(query);
    Ok(url.into())
}

fn parse_list_page(value: &serde_json::Value) -> YoutubeListPage {
    let mut page = YoutubeListPage::default();
    visit_renderers(value, &mut page);
    page
}

fn visit_renderers(value: &serde_json::Value, page: &mut YoutubeListPage) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                visit_renderers(value, page);
            }
        }
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                match key.as_str() {
                    "videoRenderer"
                    | "gridVideoRenderer"
                    | "playlistVideoRenderer"
                    | "compactVideoRenderer"
                    | "reelItemRenderer" => {
                        if let Some(item) = parse_video_renderer(value, key == "reelItemRenderer") {
                            if !page
                                .items
                                .iter()
                                .any(|existing| existing.video_id == item.video_id)
                            {
                                page.items.push(item);
                            }
                        }
                    }
                    "lockupViewModel" => {
                        if let Some(item) = parse_lockup_view_model(value) {
                            push_unique_list_item(page, item);
                        }
                    }
                    "shortsLockupViewModel" => {
                        if let Some(item) = parse_shorts_lockup_view_model(value) {
                            push_unique_list_item(page, item);
                        }
                    }
                    "continuationCommand" => {
                        if page.next_cursor.is_none() {
                            page.next_cursor = value
                                .get("token")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string);
                        }
                    }
                    _ => visit_renderers(value, page),
                }
            }
        }
        _ => {}
    }
}

fn push_unique_list_item(page: &mut YoutubeListPage, item: YoutubeListItem) {
    if !page
        .items
        .iter()
        .any(|existing| existing.video_id == item.video_id)
    {
        page.items.push(item);
    }
}

fn parse_lockup_view_model(value: &serde_json::Value) -> Option<YoutubeListItem> {
    if value.get("contentType").and_then(serde_json::Value::as_str)
        != Some("LOCKUP_CONTENT_TYPE_VIDEO")
    {
        return None;
    }
    let video_id = value.get("contentId")?.as_str()?.to_string();
    let metadata = value.pointer("/metadata/lockupMetadataViewModel")?;
    let title = metadata.pointer("/title/content")?.as_str()?.to_string();
    let metadata_rows = metadata
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(serde_json::Value::as_array);
    let metadata_parts = metadata_rows
        .into_iter()
        .flatten()
        .flat_map(|row| {
            row.get("metadataParts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect::<Vec<_>>();
    let metadata_text = metadata_parts
        .iter()
        .filter_map(|part| {
            part.pointer("/text/content")
                .and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>();
    let channel_part = metadata_parts.iter().find(|part| {
        part.pointer("/text/commandRuns/0/onTap/innertubeCommand/browseEndpoint/browseId")
            .is_some()
    });
    let channel_name = channel_part
        .and_then(|part| part.pointer("/text/content"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let channel_id = channel_part
        .and_then(|part| {
            part.pointer("/text/commandRuns/0/onTap/innertubeCommand/browseEndpoint/browseId")
        })
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let badge_text = value
        .pointer("/contentImage/thumbnailViewModel/overlays")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|overlay| {
            overlay
                .pointer("/thumbnailBottomOverlayViewModel/badges")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|badge| {
            badge
                .pointer("/thumbnailBadgeViewModel/text")
                .and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>();

    Some(YoutubeListItem {
        video_id,
        title,
        channel_name,
        channel_id,
        duration_seconds: badge_text.iter().find_map(|text| parse_duration(text)),
        view_count_text: metadata_text
            .iter()
            .find(|text| text.to_ascii_lowercase().contains("view"))
            .copied()
            .unwrap_or_default()
            .to_string(),
        published_time_text: metadata_text
            .iter()
            .find(|text| {
                let text = text.to_ascii_lowercase();
                text.contains("ago") || text.contains("streamed") || text.contains("premieres")
            })
            .copied()
            .unwrap_or_default()
            .to_string(),
        thumbnail: image_sources_thumbnail(
            value.pointer("/contentImage/thumbnailViewModel/image/sources"),
        ),
        is_live: badge_text
            .iter()
            .any(|text| text.to_ascii_uppercase().contains("LIVE")),
        is_short: false,
    })
}

fn parse_shorts_lockup_view_model(value: &serde_json::Value) -> Option<YoutubeListItem> {
    Some(YoutubeListItem {
        video_id: value
            .pointer("/onTap/innertubeCommand/reelWatchEndpoint/videoId")?
            .as_str()?
            .to_string(),
        title: value
            .pointer("/overlayMetadata/primaryText/content")?
            .as_str()?
            .to_string(),
        channel_name: String::new(),
        channel_id: String::new(),
        duration_seconds: None,
        view_count_text: value
            .pointer("/overlayMetadata/secondaryText/content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        published_time_text: String::new(),
        thumbnail: image_sources_thumbnail(
            value.pointer("/thumbnailViewModel/thumbnailViewModel/image/sources"),
        ),
        is_live: false,
        is_short: true,
    })
}

fn image_sources_thumbnail(value: Option<&serde_json::Value>) -> Option<YoutubeThumbnail> {
    value
        .and_then(serde_json::Value::as_array)
        .and_then(|sources| sources.last())
        .and_then(|thumbnail| {
            Some(YoutubeThumbnail {
                url: thumbnail.get("url")?.as_str()?.to_string(),
                width: thumbnail
                    .get("width")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                height: thumbnail
                    .get("height")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            })
        })
}

fn parse_video_renderer(value: &serde_json::Value, is_short: bool) -> Option<YoutubeListItem> {
    let video_id = value.get("videoId")?.as_str()?.to_string();
    let title = text_value(value.get("title")?);
    if title.is_empty() {
        return None;
    }
    let channel = value
        .get("ownerText")
        .or_else(|| value.get("shortBylineText"))
        .or_else(|| value.get("longBylineText"));
    let channel_name = channel.map(text_value).unwrap_or_default();
    let channel_id = channel
        .and_then(|value| value.get("runs"))
        .and_then(serde_json::Value::as_array)
        .and_then(|runs| runs.first())
        .and_then(|run| run.pointer("/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let duration_seconds = value
        .get("lengthText")
        .map(text_value)
        .as_deref()
        .and_then(parse_duration);
    let thumbnail = value
        .pointer("/thumbnail/thumbnails")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.last())
        .and_then(|thumbnail| {
            Some(YoutubeThumbnail {
                url: thumbnail.get("url")?.as_str()?.to_string(),
                width: thumbnail
                    .get("width")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                height: thumbnail
                    .get("height")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            })
        });
    let badges = value
        .get("badges")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let is_live = value.get("upcomingEventData").is_some()
        || badges.iter().any(|badge| {
            badge
                .pointer("/metadataBadgeRenderer/style")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|style| style.contains("LIVE"))
        });
    Some(YoutubeListItem {
        video_id,
        title,
        channel_name,
        channel_id,
        duration_seconds,
        view_count_text: value
            .get("viewCountText")
            .map(text_value)
            .unwrap_or_default(),
        published_time_text: value
            .get("publishedTimeText")
            .map(text_value)
            .unwrap_or_default(),
        thumbnail,
        is_live,
        is_short,
    })
}

fn text_value(value: &serde_json::Value) -> String {
    value
        .get("simpleText")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("runs")
                .and_then(serde_json::Value::as_array)
                .map(|runs| {
                    runs.iter()
                        .filter_map(|run| run.get("text").and_then(serde_json::Value::as_str))
                        .collect()
                })
        })
        .unwrap_or_default()
}

fn parse_duration(value: &str) -> Option<u64> {
    value.split(':').try_fold(0_u64, |total, part| {
        total.checked_mul(60)?.checked_add(part.parse().ok()?)
    })
}

pub fn normalize_video_id(input: &str) -> Result<String, ProviderClientError> {
    let input = input.trim();
    if is_video_id(input) {
        return Ok(input.to_string());
    }
    let url = Url::parse(input).map_err(|_| {
        ProviderClientError::InvalidConfig("YouTube video ID or URL is invalid".to_string())
    })?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let id = if host == "youtu.be" || host.ends_with(".youtu.be") {
        url.path_segments()
            .and_then(|mut parts| parts.next().map(str::to_string))
    } else if host == "youtube.com" || host.ends_with(".youtube.com") {
        url.query_pairs()
            .find_map(|(key, value)| (key == "v").then_some(value.into_owned()))
            .or_else(|| {
                let mut parts = url.path_segments()?;
                let kind = parts.next()?;
                matches!(kind, "shorts" | "live" | "embed" | "v")
                    .then(|| parts.next().map(str::to_string))
                    .flatten()
            })
    } else {
        None
    };
    id.filter(|id| is_video_id(id)).ok_or_else(|| {
        ProviderClientError::InvalidConfig("YouTube URL does not contain a video ID".to_string())
    })
}

pub fn normalize_channel_id(input: &str) -> Result<String, ProviderClientError> {
    let input = input.trim();
    if is_channel_id(input) {
        return Ok(input.to_string());
    }
    let url = Url::parse(input).map_err(|_| {
        ProviderClientError::InvalidConfig("YouTube channel ID or URL is invalid".to_string())
    })?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "youtube.com" && !host.ends_with(".youtube.com") {
        return Err(ProviderClientError::InvalidConfig(
            "YouTube channel URL host is invalid".to_string(),
        ));
    }
    let mut parts = url.path_segments().into_iter().flatten();
    let (Some("channel"), Some(channel_id)) = (parts.next(), parts.next()) else {
        return Err(ProviderClientError::InvalidConfig(
            "YouTube channel URL must contain a channel ID".to_string(),
        ));
    };
    is_channel_id(channel_id)
        .then(|| channel_id.to_string())
        .ok_or_else(|| {
            ProviderClientError::InvalidConfig(
                "YouTube channel URL contains an invalid channel ID".to_string(),
            )
        })
}

fn is_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_channel_id(value: &str) -> bool {
    value.starts_with("UC")
        && value.len() >= 3
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn normalizes_supported_youtube_urls() {
        for input in [
            "dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=1",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                normalize_video_id(input).expect("video ID should parse"),
                "dQw4w9WgXcQ"
            );
        }
        assert!(normalize_video_id("https://example.com/dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn normalizes_channel_ids_and_urls() {
        for input in [
            "UC1234567890123456789012",
            "https://www.youtube.com/channel/UC1234567890123456789012",
            "https://m.youtube.com/channel/UC1234567890123456789012/videos",
        ] {
            assert_eq!(
                normalize_channel_id(input).expect("channel ID should parse"),
                "UC1234567890123456789012"
            );
        }
        assert!(normalize_channel_id("https://youtube.com/@synctv").is_err());
        assert!(normalize_channel_id("https://example.com/channel/UC123").is_err());
    }

    #[test]
    fn builds_all_youtube_cookie_authorization_schemes() {
        let authorization = youtube_cookie_authorization(
            "LOGIN_INFO=login; SAPISID=primary; __Secure-1PAPISID=first; __Secure-3PAPISID=third",
            123,
        )
        .expect("SID cookies should produce authorization");
        let parts = authorization.split(' ').collect::<Vec<_>>();

        assert_eq!(parts.len(), 6);
        assert_eq!(parts[0], "SAPISIDHASH");
        assert!(parts[1].starts_with("123_") && parts[1].len() == 44);
        assert_eq!(parts[2], "SAPISID1PHASH");
        assert!(parts[3].starts_with("123_") && parts[3].len() == 44);
        assert_eq!(parts[4], "SAPISID3PHASH");
        assert!(parts[5].starts_with("123_") && parts[5].len() == 44);
        assert!(youtube_cookie_authorization("PREF=value", 123).is_none());
    }

    #[tokio::test]
    async fn player_preserves_innertube_metadata_and_advanced_resources() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/player"))
            .and(query_param("key", INNERTUBE_API_KEY))
            .and(header("x-youtube-client-name", "1"))
            .and(body_string_contains("dQw4w9WgXcQ"))
            .and(body_string_contains("visitor-data"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "playabilityStatus": {"status": "OK"},
                "videoDetails": {
                    "videoId": "dQw4w9WgXcQ",
                    "title": "Video",
                    "lengthSeconds": "212",
                    "channelId": "channel",
                    "author": "Author",
                    "isLiveContent": false,
                    "thumbnail": {"thumbnails": [{"url": "https://i.ytimg.com/vi/x/maxresdefault.jpg", "width": 1280, "height": 720}]}
                },
                "streamingData": {
                    "expiresInSeconds": "21600",
                    "hlsManifestUrl": "https://manifest.example/live.m3u8",
                    "dashManifestUrl": "https://manifest.example/video.mpd",
                    "formats": [{"itag": 18, "url": "https://video.example/file", "mimeType": "video/mp4", "height": 360}]
                },
                "captions": {"playerCaptionsTracklistRenderer": {"captionTracks": [{"baseUrl": "https://caption.example/api", "name": {"simpleText": "English"}, "vssId": ".en", "languageCode": "en", "isTranslatable": true}]}},
                "storyboards": {"playerStoryboardSpecRenderer": {"spec": "https://i.ytimg.com/sb/$L/$N.jpg|160#90#..."}}
            })))
            .mount(&server)
            .await;
        let client = YoutubeClient::with_endpoint(&server.uri(), reqwest::Client::new())
            .expect("client should build");
        let response = client
            .player("dQw4w9WgXcQ", Some("visitor-data"), None, None)
            .await
            .expect("player should succeed");
        assert_eq!(response.video_details.expect("details").title, "Video");
        assert_eq!(
            response.streaming_data.expect("streams").formats[0].itag,
            18
        );
        assert_eq!(
            response
                .captions
                .expect("captions")
                .player_captions_tracklist_renderer
                .expect("tracklist")
                .caption_tracks[0]
                .name
                .value(),
            "English"
        );
        assert!(response.storyboards.is_some());
    }

    #[tokio::test]
    async fn player_falls_back_to_android_vr_when_web_is_unplayable() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/player"))
            .and(header("x-youtube-client-name", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "playabilityStatus": {"status": "ERROR", "reason": "Video unavailable"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/player"))
            .and(header("x-youtube-client-name", "28"))
            .and(body_string_contains("ANDROID_VR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "playabilityStatus": {"status": "OK"},
                "videoDetails": {"videoId": "aqz-KE-bpKQ", "title": "Video"},
                "streamingData": {
                    "formats": [{
                        "itag": 18,
                        "url": "https://video.example/file",
                        "mimeType": "video/mp4"
                    }]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = YoutubeClient::with_endpoint(&server.uri(), reqwest::Client::new())
            .expect("client should build");
        let response = client
            .player("aqz-KE-bpKQ", None, None, None)
            .await
            .expect("Android VR fallback should succeed");

        assert_eq!(
            response.streaming_data.expect("streams").formats[0].itag,
            18
        );
    }

    #[tokio::test]
    #[ignore = "requires live YouTube access"]
    async fn live_player_returns_resolved_playable_resources() {
        if !matches!(std::env::var("YOUTUBE_LIVE_TEST").as_deref(), Ok("1")) {
            return;
        }
        crate::install_process_crypto_provider();
        let client =
            YoutubeClient::with_http_client(reqwest::Client::new()).expect("client should build");
        let cookie = std::env::var("YOUTUBE_COOKIE").ok();
        let response = client
            .player("aqz-KE-bpKQ", None, None, cookie.as_deref())
            .await
            .expect("live player request should succeed");
        let streaming = response
            .streaming_data
            .expect("live player should return streaming data");

        assert!(
            streaming
                .formats
                .iter()
                .chain(&streaming.adaptive_formats)
                .any(|format| format.url.is_some())
                || streaming.hls_manifest_url.is_some()
                || streaming.dash_manifest_url.is_some()
        );
    }

    #[tokio::test]
    async fn playlist_extracts_video_renderers_and_continuation_cursor() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/browse"))
            .and(body_string_contains("VLPL123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "contents": [{"playlistVideoRenderer": {
                    "videoId": "dQw4w9WgXcQ",
                    "title": {"runs": [{"text": "Video"}]},
                    "shortBylineText": {"runs": [{"text": "Author", "navigationEndpoint": {"browseEndpoint": {"browseId": "UC123"}}}]},
                    "lengthText": {"simpleText": "3:32"},
                    "viewCountText": {"simpleText": "1,000 views"},
                    "thumbnail": {"thumbnails": [{"url": "https://i.ytimg.com/vi/x/default.jpg", "width": 120, "height": 90}]}
                }}, {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "next-token"}}}}]
            })))
            .mount(&server)
            .await;
        let client = YoutubeClient::with_endpoint(&server.uri(), reqwest::Client::new())
            .expect("client should build");
        let page = client
            .playlist("PL123", None, None, None)
            .await
            .expect("playlist should succeed");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].duration_seconds, Some(212));
        assert_eq!(page.items[0].channel_id, "UC123");
        assert_eq!(page.next_cursor.as_deref(), Some("next-token"));
    }

    #[tokio::test]
    async fn playlist_decodes_continuation_cursor_before_sending_json() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/browse"))
            .and(body_string_contains(r#""continuation":"next-token==""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"contents": []})))
            .expect(1)
            .mount(&server)
            .await;
        let client = YoutubeClient::with_endpoint(&server.uri(), reqwest::Client::new())
            .expect("client should build");

        client
            .playlist("PL123", Some("next-token%3D%3D"), None, None)
            .await
            .expect("playlist continuation should succeed");
    }

    #[test]
    fn list_page_extracts_current_lockup_view_models() {
        let page = parse_list_page(&json!({
            "contents": [
                {"lockupViewModel": {
                    "contentId": "dQw4w9WgXcQ",
                    "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                    "contentImage": {"thumbnailViewModel": {
                        "image": {"sources": [{
                            "url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
                            "width": 336,
                            "height": 188
                        }]},
                        "overlays": [{"thumbnailBottomOverlayViewModel": {"badges": [{
                            "thumbnailBadgeViewModel": {"text": "3:32"}
                        }]}}]
                    }},
                    "metadata": {"lockupMetadataViewModel": {
                        "title": {"content": "Video title"},
                        "metadata": {"contentMetadataViewModel": {"metadataRows": [
                            {"metadataParts": [{"text": {
                                "content": "Channel",
                                "commandRuns": [{"onTap": {"innertubeCommand": {
                                    "browseEndpoint": {"browseId": "UC123"}
                                }}}]
                            }}]},
                            {"metadataParts": [
                                {"text": {"content": "1K views"}},
                                {"text": {"content": "2 days ago"}}
                            ]}
                        ]}}
                    }}
                }},
                {"shortsLockupViewModel": {
                    "onTap": {"innertubeCommand": {"reelWatchEndpoint": {
                        "videoId": "S069PVmKXZ4"
                    }}},
                    "overlayMetadata": {
                        "primaryText": {"content": "Short title"},
                        "secondaryText": {"content": "20K views"}
                    },
                    "thumbnailViewModel": {"thumbnailViewModel": {"image": {
                        "sources": [{
                            "url": "https://i.ytimg.com/vi/S069PVmKXZ4/oar2.jpg",
                            "width": 405,
                            "height": 720
                        }]
                    }}}
                }}
            ]
        }));

        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].video_id, "dQw4w9WgXcQ");
        assert_eq!(page.items[0].channel_id, "UC123");
        assert_eq!(page.items[0].duration_seconds, Some(212));
        assert_eq!(page.items[0].view_count_text, "1K views");
        assert_eq!(page.items[0].published_time_text, "2 days ago");
        assert!(page.items[1].is_short);
        assert_eq!(page.items[1].view_count_text, "20K views");
    }

    #[tokio::test]
    async fn channel_tabs_and_native_feeds_use_distinct_browse_contracts() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        for marker in [
            "EgZ2aWRlb3PyBgQKAjoA",
            "EgZzaG9ydHPyBgUKA5oBAA==",
            "EgdzdHJlYW1z8gYECgJ6AA==",
            "FEsubscriptions",
        ] {
            Mock::given(method("POST"))
                .and(path("/youtubei/v1/browse"))
                .and(body_string_contains(marker))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "contents": []
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        let client = YoutubeClient::with_endpoint(&server.uri(), reqwest::Client::new())
            .expect("client should build");

        for tab in [
            super::super::types::YoutubeChannelTab::Videos,
            super::super::types::YoutubeChannelTab::Shorts,
            super::super::types::YoutubeChannelTab::Live,
        ] {
            client
                .channel("UC123", tab, None, None, None)
                .await
                .expect("channel tab should succeed");
        }
        client
            .feed("FEsubscriptions", None, None, Some("SID=session"))
            .await
            .expect("subscriptions should succeed");
    }

    #[tokio::test]
    async fn player_resolves_signature_cipher_and_n_with_cached_player_js() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/youtubei/v1/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "playabilityStatus": {"status": "OK"},
                "streamingData": {
                    "formats": [{
                        "itag": 18,
                        "signatureCipher": "url=https%3A%2F%2Fvideo.example%2Ffile%3Fn%3Dlower&s=abc&sp=sig"
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/watch"))
            .and(query_param("v", "dQw4w9WgXcQ"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"jsUrl":"\/s\/player\/test\/base.js"}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/s/player/test/base.js"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"(function(){
var H={};H.mark=function(a,b){};
function U(url,key,value){this.values={};if(value!==undefined)this.values[key]=value;var n=/[?&]n=([^&]+)/.exec(url);if(n)this.values.n=n[1]}
U.prototype.set=function(key,value){if(value!==undefined)this.values[key]=value};
U.prototype.get=function(key){return this.values[key]};
U.prototype.clone=function(){return this};
U.prototype.transform=function(){if(this.values.s!==undefined)this.values.s=this.values.s.split("").reverse().join("");if(this.values.n!==undefined)this.values.n=this.values.n.toUpperCase()};
function Transform(url,key,value){H.mark("alr","yes");return new U(url,key,value)}
}).call(this);"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let client = YoutubeClient::with_endpoint(&server.uri(), reqwest::Client::new())
            .expect("client should build");
        for _ in 0..2 {
            let response = client
                .player("dQw4w9WgXcQ", None, None, None)
                .await
                .expect("player should resolve");
            let url = Url::parse(
                response.streaming_data.expect("streams").formats[0]
                    .url
                    .as_deref()
                    .expect("resolved URL"),
            )
            .expect("valid resolved URL");
            let query = url
                .query_pairs()
                .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(
                query.get("n").map(std::convert::AsRef::as_ref),
                Some("LOWER")
            );
            assert_eq!(
                query.get("sig").map(std::convert::AsRef::as_ref),
                Some("cba")
            );
        }
    }
}
