use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use reqwest::header::{COOKIE, REFERER, USER_AGENT};
use reqwest::Url;

use super::types::{
    RawAuthor, RawItem, RawListEnvelope, RawLiveEnvelope, RawLiveRoom, RawUserDetail,
};
use super::{
    TikTokAuthor, TikTokImage, TikTokListItem, TikTokListPage, TikTokMedia, TikTokMediaKind,
    TikTokMetadata, TikTokResource, TikTokSession, TikTokStreamFormat, TikTokSubtitle,
    TikTokVariant,
};
use crate::{
    check_response, fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT,
};

const TIKTOK_ORIGIN: &str = "https://www.tiktok.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokEndpoints {
    pub web_base: String,
    pub user_posts: String,
    pub live_user_room: String,
}

impl Default for TikTokEndpoints {
    fn default() -> Self {
        Self {
            web_base: TIKTOK_ORIGIN.to_string(),
            user_posts: format!("{TIKTOK_ORIGIN}/api/creator/item_list/"),
            live_user_room: format!("{TIKTOK_ORIGIN}/api-live/user/room"),
        }
    }
}

#[derive(Clone)]
pub struct TikTokClient {
    http: reqwest::Client,
    endpoints: TikTokEndpoints,
    device_id: Arc<String>,
}

impl TikTokClient {
    pub fn new() -> Result<Self, ProviderClientError> {
        let http =
            crate::provider_http_client_builder(synctv_common::ssrf::SsrfGuard::strict_policy())
                .user_agent(PROVIDER_USER_AGENT)
                .build()
                .map_err(|error| ProviderClientError::Network(error.to_string()))?;
        Ok(Self::with_http_client(http))
    }

    #[must_use]
    pub fn with_http_client(http: reqwest::Client) -> Self {
        Self {
            http,
            endpoints: TikTokEndpoints::default(),
            device_id: Arc::new(generate_device_id()),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: TikTokEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(input: &str) -> Result<TikTokResource, ProviderClientError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "TikTok URL or video ID is required".to_string(),
            ));
        }
        if is_numeric_id(input) {
            return Ok(TikTokResource::Video {
                video_id: input.to_string(),
            });
        }
        let url = Url::parse(input).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid TikTok URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !host.ends_with("tiktok.com") {
            return Err(ProviderClientError::InvalidConfig(
                "URL is outside TikTok".to_string(),
            ));
        }
        let segments = url
            .path_segments()
            .map(|segments| {
                segments
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(index) = segments.iter().position(|value| *value == "video") {
            return video_resource(segments.get(index + 1).copied());
        }
        if segments.len() >= 2 && segments[0].starts_with('@') && segments[1] == "live" {
            return live_resource(segments[0].strip_prefix('@'));
        }
        Err(ProviderClientError::InvalidConfig(
            "TikTok URL must identify a video or live room".to_string(),
        ))
    }

    pub async fn resolve_resource(
        &self,
        input: &str,
        session: Option<&TikTokSession>,
    ) -> Result<TikTokResource, ProviderClientError> {
        if let Ok(resource) = Self::parse_resource(input) {
            return Ok(resource);
        }
        let url = Url::parse(input.trim()).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid TikTok URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default();
        if !matches!(host, "vm.tiktok.com" | "vt.tiktok.com" | "www.tiktok.com") {
            return Self::parse_resource(input);
        }
        let response = check_response(
            Self::request(self.http.get(url), session, TIKTOK_ORIGIN)
                .send()
                .await?,
        )
        .await?;
        Self::parse_resource(response.url().as_str())
    }

    pub async fn resolve(
        &self,
        input: &str,
        session: Option<&TikTokSession>,
    ) -> Result<TikTokMedia, ProviderClientError> {
        match self.resolve_resource(input, session).await? {
            TikTokResource::Video { video_id } => self.video(&video_id, session).await,
            TikTokResource::Live { unique_id } => self.live(&unique_id, session).await,
        }
    }

    pub async fn video(
        &self,
        video_id: &str,
        session: Option<&TikTokSession>,
    ) -> Result<TikTokMedia, ProviderClientError> {
        validate_numeric_id(video_id, "video ID")?;
        let url = format!("{}/@_/video/{video_id}", self.endpoints.web_base);
        let html = self.webpage(&url, session).await?;
        let scope = universal_scope(&html)?;
        let detail = scope.get("webapp.video-detail").ok_or_else(|| {
            ProviderClientError::Parse("TikTok video detail is missing".to_string())
        })?;
        let status = detail
            .get("statusCode")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if status != 0 {
            return Err(ProviderClientError::Api {
                code: status,
                message: detail
                    .get("statusMsg")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("TikTok video is unavailable")
                    .to_string(),
            });
        }
        let raw = detail
            .pointer("/itemInfo/itemStruct")
            .cloned()
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: "TikTok video metadata is unavailable".to_string(),
            })?;
        let item: RawItem = serde_json::from_value(raw)?;
        media_from_item(item)
    }

    pub async fn user_sec_uid(
        &self,
        unique_id: &str,
        session: Option<&TikTokSession>,
    ) -> Result<String, ProviderClientError> {
        validate_unique_id(unique_id)?;
        let url = format!("{}/@{unique_id}", self.endpoints.web_base);
        let html = self.webpage(&url, session).await?;
        let scope = universal_scope(&html)?;
        let detail: RawUserDetail =
            serde_json::from_value(scope.get("webapp.user-detail").cloned().ok_or_else(|| {
                ProviderClientError::Api {
                    code: 404,
                    message: "TikTok user metadata is unavailable".to_string(),
                }
            })?)?;
        detail
            .user_info
            .and_then(|info| info.user)
            .map(|user| user.sec_uid)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: "TikTok user secUid is unavailable".to_string(),
            })
    }

    pub async fn user_posts(
        &self,
        sec_uid: &str,
        cursor: Option<&str>,
        count: u32,
        session: Option<&TikTokSession>,
    ) -> Result<TikTokListPage, ProviderClientError> {
        validate_sec_uid(sec_uid)?;
        let cursor = cursor.map_or_else(now_millis, str::to_string);
        if !cursor.chars().all(|value| value.is_ascii_digit()) {
            return Err(ProviderClientError::InvalidConfig(
                "TikTok cursor must be an unsigned millisecond timestamp".to_string(),
            ));
        }
        let count = count.clamp(1, 35);
        let envelope: RawListEnvelope = fetch_json(Self::request(
            self.http.get(&self.endpoints.user_posts).query(&[
                ("aid", "1988".to_string()),
                ("app_language", "en".to_string()),
                ("app_name", "tiktok_web".to_string()),
                ("browser_language", "en-US".to_string()),
                ("browser_name", "Mozilla".to_string()),
                ("browser_platform", "Win32".to_string()),
                ("channel", "tiktok_web".to_string()),
                ("count", count.to_string()),
                ("cursor", cursor.clone()),
                ("device_id", self.device_id.as_ref().clone()),
                ("device_platform", "web_pc".to_string()),
                ("region", "US".to_string()),
                ("secUid", sec_uid.to_string()),
                ("type", "1".to_string()),
            ]),
            session,
            TIKTOK_ORIGIN,
        ))
        .await?;
        let mut seen = HashSet::new();
        let items = envelope
            .item_list
            .iter()
            .filter(|item| !item.id.is_empty() && seen.insert(item.id.clone()))
            .filter_map(list_item_from_raw)
            .collect::<Vec<_>>();
        let next_cursor = envelope
            .item_list
            .iter()
            .filter_map(|item| item.create_time)
            .min()
            .and_then(|seconds| u64::try_from(seconds).ok())
            .map(|seconds| seconds.saturating_mul(1000).to_string());
        Ok(TikTokListPage {
            items,
            cursor: next_cursor,
            has_more: envelope.has_more_previous,
        })
    }

    pub async fn live(
        &self,
        unique_id: &str,
        session: Option<&TikTokSession>,
    ) -> Result<TikTokMedia, ProviderClientError> {
        validate_unique_id(unique_id)?;
        let envelope: RawLiveEnvelope = fetch_json(Self::request(
            self.http.get(&self.endpoints.live_user_room).query(&[
                ("aid", "1988"),
                ("sourceType", "54"),
                ("uniqueId", unique_id),
            ]),
            session,
            TIKTOK_ORIGIN,
        ))
        .await?;
        if envelope.status_code.unwrap_or(0) != 0 {
            return Err(ProviderClientError::Api {
                code: envelope.status_code.unwrap_or(-1),
                message: envelope
                    .message
                    .unwrap_or_else(|| "TikTok live API failed".to_string()),
            });
        }
        let room = envelope
            .data
            .and_then(|data| data.live_room)
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: "TikTok live room is unavailable".to_string(),
            })?;
        media_from_live(unique_id, room)
    }

    async fn webpage(
        &self,
        url: &str,
        session: Option<&TikTokSession>,
    ) -> Result<String, ProviderClientError> {
        let response = Self::request(self.http.get(url), session, TIKTOK_ORIGIN)
            .send()
            .await?;
        text_with_limit(check_response(response).await?).await
    }

    fn request(
        request: reqwest::RequestBuilder,
        session: Option<&TikTokSession>,
        referer: &'static str,
    ) -> reqwest::RequestBuilder {
        let request = request
            .header(USER_AGENT, PROVIDER_USER_AGENT)
            .header(REFERER, format!("{referer}/"));
        if let Some(cookie) = session.and_then(|session| session.cookie.as_deref()) {
            request.header(COOKIE, cookie)
        } else {
            request
        }
    }
}

fn universal_scope(
    html: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, ProviderClientError> {
    let regex = Regex::new(
        r#"(?s)<script[^>]+id=[\"']__UNIVERSAL_DATA_FOR_REHYDRATION__[\"'][^>]*>(.*?)</script>"#,
    )
    .expect("static TikTok hydration regex");
    let encoded = regex
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
        .ok_or_else(|| {
            ProviderClientError::Parse("TikTok hydration data is missing".to_string())
        })?;
    let decoded = html_escape::decode_html_entities(encoded);
    let value: serde_json::Value = serde_json::from_str(&decoded)?;
    value
        .pointer("/__DEFAULT_SCOPE__")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .ok_or_else(|| ProviderClientError::Parse("TikTok hydration scope is missing".to_string()))
}

fn media_from_item(item: RawItem) -> Result<TikTokMedia, ProviderClientError> {
    let video = item.video.as_ref();
    let music = item.music.as_ref();
    let mut variants = Vec::new();
    if let Some(video) = video {
        for bitrate in &video.bitrate_info {
            let Some(address) = &bitrate.play_addr else {
                continue;
            };
            let (codec, quality, parsed_bitrate) = address
                .url_key
                .as_deref()
                .map(parse_url_key)
                .unwrap_or_default();
            if codec.as_deref() == Some("vvc") {
                continue;
            }
            for url in &address.url_list {
                variants.push(video_variant(
                    url,
                    quality
                        .as_deref()
                        .or(bitrate.gear_name.as_deref())
                        .unwrap_or("play"),
                    codec.clone(),
                    bitrate.bit_rate.or(parsed_bitrate).or_else(|| {
                        address
                            .data_size
                            .zip(video.duration)
                            .and_then(|(size, duration)| {
                                (duration > 0).then_some(size.saturating_mul(8_000) / duration)
                            })
                    }),
                    video.width,
                    video.height,
                    false,
                ));
            }
        }
        if variants.is_empty() {
            variants.extend(
                urls_from_value(video.play_addr.as_ref())
                    .into_iter()
                    .map(|url| {
                        video_variant(
                            &url,
                            "play",
                            Some("avc".to_string()),
                            None,
                            video.width,
                            video.height,
                            false,
                        )
                    }),
            );
        }
        variants.extend(
            urls_from_value(video.download_addr.as_ref())
                .into_iter()
                .map(|url| {
                    video_variant(
                        &url,
                        "download",
                        Some("avc".to_string()),
                        None,
                        video.width,
                        video.height,
                        true,
                    )
                }),
        );
    }
    if variants.is_empty() {
        if let Some(url) = music.and_then(|music| music.play_url.as_deref()) {
            variants.push(TikTokVariant {
                url: normalize_url(url),
                format: TikTokStreamFormat::Audio,
                quality: "slideshow audio".to_string(),
                codec: Some("aac".to_string()),
                width: None,
                height: None,
                bitrate: None,
                audio_only: true,
                watermarked: false,
                headers_required: true,
            });
        }
    }
    if variants.is_empty() {
        return Err(ProviderClientError::Api {
            code: 415,
            message: "TikTok post has no SyncTV-playable video or audio".to_string(),
        });
    }
    deduplicate_variants(&mut variants);
    let author = author_from_raw(item.author.as_ref(), "");
    let stats = item.stats.as_ref();
    let subtitles = video
        .map(|video| {
            video
                .subtitle_infos
                .iter()
                .filter_map(|subtitle| {
                    Some(TikTokSubtitle {
                        language: subtitle
                            .language_code_name
                            .clone()
                            .unwrap_or_else(|| "und".to_string()),
                        format: subtitle
                            .format
                            .clone()
                            .unwrap_or_else(|| "vtt".to_string())
                            .to_ascii_lowercase(),
                        url: normalize_url(subtitle.url.as_deref()?),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let title = item_title(&item);
    Ok(TikTokMedia {
        resource: TikTokResource::Video {
            video_id: item.id.clone(),
        },
        metadata: TikTokMetadata {
            id: item.id,
            kind: TikTokMediaKind::Video,
            title,
            description: item.desc,
            author,
            cover: video.and_then(|video| {
                image(
                    video.cover.as_deref().or(video.origin_cover.as_deref()),
                    video.width,
                    video.height,
                )
            }),
            dynamic_cover: video
                .and_then(|video| image(video.dynamic_cover.as_deref(), video.width, video.height)),
            duration_ms: video.and_then(|video| video.duration),
            created_at: item.create_time,
            is_live: false,
            view_count: stats.and_then(|stats| stats.play_count),
            like_count: stats.and_then(|stats| stats.digg_count),
            comment_count: stats.and_then(|stats| stats.comment_count),
            share_count: stats.and_then(|stats| stats.share_count),
            collect_count: stats.and_then(|stats| stats.collect_count),
            concurrent_viewers: None,
            music_title: music.and_then(|music| music.title.clone()),
            music_author: music.and_then(|music| music.author_name.clone()),
            subtitles,
        },
        room_id: None,
        variants,
    })
}

fn media_from_live(unique_id: &str, room: RawLiveRoom) -> Result<TikTokMedia, ProviderClientError> {
    let variants = room
        .stream_data
        .as_ref()
        .and_then(|data| data.pull_data.as_ref())
        .map(|data| live_variants(&data.stream_data))
        .unwrap_or_default();
    let is_live = room.status == Some(2);
    let owner = author_from_raw(room.owner_info.as_ref(), unique_id);
    Ok(TikTokMedia {
        resource: TikTokResource::Live {
            unique_id: unique_id.to_string(),
        },
        metadata: TikTokMetadata {
            id: room
                .stream_id
                .clone()
                .unwrap_or_else(|| unique_id.to_string()),
            kind: TikTokMediaKind::Live,
            title: if room.title.is_empty() {
                format!("@{unique_id} live")
            } else {
                room.title.clone()
            },
            description: room.title,
            author: owner,
            cover: image(room.cover_url.as_deref(), None, None),
            dynamic_cover: None,
            duration_ms: None,
            created_at: None,
            is_live,
            view_count: None,
            like_count: None,
            comment_count: None,
            share_count: None,
            collect_count: None,
            concurrent_viewers: room.user_count,
            music_title: None,
            music_author: None,
            subtitles: Vec::new(),
        },
        room_id: room.stream_id,
        variants,
    })
}

fn live_variants(value: &serde_json::Value) -> Vec<TikTokVariant> {
    let mut variants = Vec::new();
    let Some(streams) = value.get("data").and_then(serde_json::Value::as_object) else {
        return variants;
    };
    for (quality, stream) in streams {
        let Some(main) = stream.get("main") else {
            continue;
        };
        let sdk = main
            .get("sdk_params")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
            .unwrap_or_default();
        let codec = sdk
            .get("VCodec")
            .or_else(|| sdk.get("vcodec"))
            .and_then(serde_json::Value::as_str)
            .map(normalize_codec);
        let bitrate = sdk.get("vbitrate").and_then(serde_json::Value::as_u64);
        let (width, height) = sdk
            .get("resolution")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_resolution)
            .map_or((None, None), |(width, height)| (Some(width), Some(height)));
        for (field, format) in [
            ("flv", TikTokStreamFormat::Flv),
            ("hls", TikTokStreamFormat::Hls),
        ] {
            if let Some(url) = main
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|url| !url.is_empty())
            {
                variants.push(TikTokVariant {
                    url: normalize_url(url),
                    format,
                    quality: quality.clone(),
                    codec: codec.clone(),
                    width,
                    height,
                    bitrate,
                    audio_only: false,
                    watermarked: false,
                    headers_required: true,
                });
            }
        }
    }
    deduplicate_variants(&mut variants);
    variants
}

fn list_item_from_raw(item: &RawItem) -> Option<TikTokListItem> {
    let video = item.video.as_ref()?;
    let playable = !video.bitrate_info.is_empty()
        || !urls_from_value(video.play_addr.as_ref()).is_empty()
        || item
            .music
            .as_ref()
            .and_then(|music| music.play_url.as_ref())
            .is_some();
    playable.then(|| TikTokListItem {
        video_id: item.id.clone(),
        title: item_title(item),
        author: author_from_raw(item.author.as_ref(), ""),
        cover: image(
            video.cover.as_deref().or(video.origin_cover.as_deref()),
            video.width,
            video.height,
        ),
        duration_ms: video.duration,
        created_at: item.create_time,
    })
}

fn video_variant(
    url: &str,
    quality: &str,
    codec: Option<String>,
    bitrate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    watermarked: bool,
) -> TikTokVariant {
    TikTokVariant {
        url: normalize_url(url),
        format: TikTokStreamFormat::Mp4,
        quality: quality.to_string(),
        codec,
        width,
        height,
        bitrate,
        audio_only: false,
        watermarked,
        headers_required: true,
    }
}

fn urls_from_value(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(url) = value.as_str() {
        return vec![url.to_string()];
    }
    for key in ["UrlList", "urlList"] {
        if let Some(urls) = value.get(key).and_then(serde_json::Value::as_array) {
            return urls
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect();
        }
    }
    ["src", "url", "download"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect()
}

fn parse_url_key(value: &str) -> (Option<String>, Option<String>, Option<u64>) {
    let regex = Regex::new(r"v[^_]+_([^_]+)_(\d+p)_(\d+)").expect("static TikTok URL key regex");
    let Some(captures) = regex.captures(value) else {
        return (None, None, None);
    };
    let codec = captures.get(1).map(|value| normalize_codec(value.as_str()));
    let quality = captures.get(2).map(|value| value.as_str().to_string());
    let bitrate = captures
        .get(3)
        .and_then(|value| value.as_str().parse().ok())
        .map(|value: u64| value.saturating_mul(1000));
    (codec, quality, bitrate)
}

fn normalize_codec(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "h264" | "avc" => "avc".to_string(),
        "h265" | "hevc" | "bytevc1" => "hevc".to_string(),
        "bytevc2" | "h266" | "vvc" => "vvc".to_string(),
        value => value.to_string(),
    }
}

fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let value = value.trim_end_matches('p');
    let (width, height) = value.split_once(['x', '*'])?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn author_from_raw(author: Option<&RawAuthor>, fallback_unique_id: &str) -> TikTokAuthor {
    let author = author.cloned().unwrap_or_default();
    TikTokAuthor {
        id: author.id,
        sec_uid: author.sec_uid,
        unique_id: if author.unique_id.is_empty() {
            fallback_unique_id.to_string()
        } else {
            author.unique_id
        },
        nickname: author.nickname,
        avatar: image(
            author
                .avatar_larger
                .as_deref()
                .or(author.avatar_thumb.as_deref()),
            None,
            None,
        ),
    }
}

fn image(url: Option<&str>, width: Option<u32>, height: Option<u32>) -> Option<TikTokImage> {
    let url = url.filter(|url| !url.is_empty())?;
    Some(TikTokImage {
        url: normalize_url(url),
        width,
        height,
    })
}

fn item_title(item: &RawItem) -> String {
    if item.desc.trim().is_empty() {
        format!("TikTok {}", item.id)
    } else {
        item.desc.clone()
    }
}

fn normalize_url(value: &str) -> String {
    if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.replacen("http://", "https://", 1)
    }
}

fn deduplicate_variants(variants: &mut Vec<TikTokVariant>) {
    variants.sort_by(|left, right| left.url.cmp(&right.url));
    variants.dedup_by(|left, right| left.url == right.url);
}

fn video_resource(value: Option<&str>) -> Result<TikTokResource, ProviderClientError> {
    let value = value.unwrap_or_default();
    validate_numeric_id(value, "video ID")?;
    Ok(TikTokResource::Video {
        video_id: value.to_string(),
    })
}

fn live_resource(value: Option<&str>) -> Result<TikTokResource, ProviderClientError> {
    let value = value.unwrap_or_default();
    validate_unique_id(value)?;
    Ok(TikTokResource::Live {
        unique_id: value.to_string(),
    })
}

fn validate_numeric_id(value: &str, name: &str) -> Result<(), ProviderClientError> {
    if !is_numeric_id(value) {
        return Err(ProviderClientError::InvalidConfig(format!(
            "TikTok {name} is invalid"
        )));
    }
    Ok(())
}

fn is_numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 32 && value.chars().all(|value| value.is_ascii_digit())
}

fn validate_unique_id(value: &str) -> Result<(), ProviderClientError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '.' | '-'))
    {
        return Err(ProviderClientError::InvalidConfig(
            "TikTok unique ID is invalid".to_string(),
        ));
    }
    Ok(())
}

fn validate_sec_uid(value: &str) -> Result<(), ProviderClientError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
    {
        return Err(ProviderClientError::InvalidConfig(
            "TikTok secUid is invalid".to_string(),
        ));
    }
    Ok(())
}

fn generate_device_id() -> String {
    use rand::RngExt;
    rand::rng()
        .random_range(7_250_000_000_000_000_000_u64..7_325_099_899_999_994_577_u64)
        .to_string()
}

fn now_millis() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn endpoints(server: &MockServer) -> TikTokEndpoints {
        TikTokEndpoints {
            web_base: server.uri(),
            user_posts: format!("{}/posts", server.uri()),
            live_user_room: format!("{}/live", server.uri()),
        }
    }

    fn client(server: &MockServer) -> TikTokClient {
        crate::install_process_crypto_provider();
        TikTokClient::with_http_client(reqwest::Client::new()).with_endpoints(endpoints(server))
    }

    fn hydration(scope: &serde_json::Value) -> String {
        format!(
            r#"<html><script id="__UNIVERSAL_DATA_FOR_REHYDRATION__" type="application/json">{{"__DEFAULT_SCOPE__":{scope}}}</script></html>"#
        )
    }

    #[test]
    fn parses_video_live_and_short_link_resources() {
        assert_eq!(
            TikTokClient::parse_resource(
                "https://www.tiktok.com/@creator/video/7123456789012345678"
            )
            .expect("test operation should succeed"),
            TikTokResource::Video {
                video_id: "7123456789012345678".to_string()
            }
        );
        assert_eq!(
            TikTokClient::parse_resource("https://www.tiktok.com/@creator.name/live")
                .expect("test operation should succeed"),
            TikTokResource::Live {
                unique_id: "creator.name".to_string()
            }
        );
        assert!(TikTokClient::parse_resource("https://vm.tiktok.com/short").is_err());
    }

    #[tokio::test]
    async fn resolves_video_formats_covers_subtitles_and_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/@_/video/7123456789012345678"))
            .respond_with(ResponseTemplate::new(200).set_body_string(hydration(&json!({
                "webapp.video-detail": {
                    "statusCode": 0,
                    "itemInfo": {"itemStruct": {
                        "id": "7123456789012345678",
                        "desc": "TikTok title",
                        "createTime": 1_700_000_000,
                        "author": {"id": "1", "secUid": "sec", "uniqueId": "creator", "nickname": "Creator"},
                        "stats": {"playCount": 42, "diggCount": 7, "collectCount": 3},
                        "music": {"title": "Song", "authorName": "Artist"},
                        "video": {
                            "duration": 15000,
                            "width": 1080,
                            "height": 1920,
                            "cover": "http://img/cover.jpg",
                            "dynamicCover": "https://img/dynamic.webp",
                            "bitrateInfo": [{
                                "BitRate": 2_000_000,
                                "PlayAddr": {
                                    "UrlList": ["http://media/hevc.mp4"],
                                    "UrlKey": "v09044g40000_bytevc1_1080p_2000"
                                }
                            }, {
                                "PlayAddr": {
                                    "UrlList": ["https://media/vvc.mp4"],
                                    "UrlKey": "v09044g40000_bytevc2_1080p_3000"
                                }
                            }],
                            "downloadAddr": "https://media/watermarked.mp4",
                            "subtitleInfos": [{"LanguageCodeName": "en", "Format": "webvtt", "Url": "https://sub/en.vtt"}]
                        }
                    }}
                }
            }))))
            .mount(&server)
            .await;
        let media = client(&server)
            .video("7123456789012345678", None)
            .await
            .expect("test operation should succeed");
        assert_eq!(media.metadata.title, "TikTok title");
        assert_eq!(media.metadata.view_count, Some(42));
        assert_eq!(media.metadata.subtitles.len(), 1);
        assert_eq!(
            media
                .metadata
                .cover
                .as_ref()
                .expect("test operation should succeed")
                .url,
            "https://img/cover.jpg"
        );
        assert!(media
            .variants
            .iter()
            .any(|variant| variant.codec.as_deref() == Some("hevc")));
        assert!(media.variants.iter().any(|variant| variant.watermarked));
        assert!(!media
            .variants
            .iter()
            .any(|variant| variant.codec.as_deref() == Some("vvc")));
    }

    #[tokio::test]
    async fn keeps_slideshow_audio_as_playable_variant() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/@_/video/7123456789012345679"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(hydration(&json!({
                    "webapp.video-detail": {"statusCode": 0, "itemInfo": {"itemStruct": {
                        "id": "7123456789012345679",
                        "desc": "slideshow",
                        "music": {"playUrl": "https://media/audio.m4a"}
                    }}}
                }))),
            )
            .mount(&server)
            .await;
        let media = client(&server)
            .video("7123456789012345679", None)
            .await
            .expect("test operation should succeed");
        assert!(media.variants[0].audio_only);
        assert_eq!(media.variants[0].format, TikTokStreamFormat::Audio);
    }

    #[tokio::test]
    async fn lists_user_posts_with_timestamp_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/posts"))
            .and(query_param("secUid", "MS4wLjAB-sec"))
            .and(query_param("cursor", "1700000000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hasMorePrevious": true,
                "itemList": [{
                    "id": "7001",
                    "desc": "one",
                    "createTime": 1_699_999_999,
                    "author": {"secUid": "MS4wLjAB-sec", "uniqueId": "creator"},
                    "video": {"playAddr": "https://media/one.mp4", "cover": "https://img/one.jpg"}
                }]
            })))
            .mount(&server)
            .await;
        let page = client(&server)
            .user_posts("MS4wLjAB-sec", Some("1700000000000"), 20, None)
            .await
            .expect("test operation should succeed");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.cursor.as_deref(), Some("1699999999000"));
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn resolves_live_flv_hls_and_viewers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/live"))
            .and(query_param("uniqueId", "creator"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "statusCode": 0,
                "data": {"liveRoom": {
                    "status": 2,
                    "streamId": "9988",
                    "title": "live title",
                    "coverUrl": "https://img/live.jpg",
                    "userCount": 321,
                    "ownerInfo": {"id": "9", "secUid": "sec", "uniqueId": "creator", "nickname": "Creator"},
                    "streamData": {"pull_data": {"stream_data": "{\"data\":{\"origin\":{\"main\":{\"flv\":\"http://media/live.flv\",\"hls\":\"https://media/live.m3u8\",\"sdk_params\":\"{\\\"VCodec\\\":\\\"h265\\\",\\\"vbitrate\\\":6000000,\\\"resolution\\\":\\\"1920x1080\\\"}\"}}}}"}}
                }}
            })))
            .mount(&server)
            .await;
        let media = client(&server)
            .live("creator", None)
            .await
            .expect("test operation should succeed");
        assert!(media.metadata.is_live);
        assert_eq!(media.metadata.concurrent_viewers, Some(321));
        assert_eq!(media.variants.len(), 2);
        assert!(media
            .variants
            .iter()
            .all(|variant| variant.codec.as_deref() == Some("hevc")));
    }

    #[tokio::test]
    async fn resolves_profile_name_to_sec_uid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/@creator"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(hydration(&json!({
                    "webapp.user-detail": {"userInfo": {"user": {"secUid": "MS4wLjAB-sec"}}}
                }))),
            )
            .mount(&server)
            .await;
        assert_eq!(
            client(&server)
                .user_sec_uid("creator", None)
                .await
                .expect("test operation should succeed"),
            "MS4wLjAB-sec"
        );
    }
}
