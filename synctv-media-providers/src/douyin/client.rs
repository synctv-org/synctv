use std::collections::HashMap;
use std::sync::Arc;

use reqwest::header::{COOKIE, ORIGIN, REFERER, USER_AGENT};
use reqwest::Url;
use serde_json::json;
use tokio::sync::OnceCell;

use super::sign::{
    generate_ms_token, generate_nonce, generate_odin_ttid, generate_verify_fp, sign_a_bogus,
};
use super::types::{
    Aweme, AwemeDetailEnvelope, AwemeListEnvelope, LiveEnvelope, LiveRoom, RawAuthor, RawImage,
    RawLiveAuthor, RawPullData, RawStreamUrl,
};
use super::{
    DouyinAuthor, DouyinImage, DouyinListItem, DouyinListPage, DouyinMedia, DouyinMediaKind,
    DouyinMetadata, DouyinResource, DouyinSession, DouyinStreamFormat, DouyinVariant,
};
use crate::{fetch_json, ProviderClientError, PROVIDER_USER_AGENT};

const DOUYIN_ORIGIN: &str = "https://www.douyin.com";
const DOUYIN_LIVE_ORIGIN: &str = "https://live.douyin.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinEndpoints {
    pub web_base: String,
    pub detail: String,
    pub user_posts: String,
    pub live_enter: String,
    pub ttwid_register: String,
}

impl Default for DouyinEndpoints {
    fn default() -> Self {
        Self {
            web_base: DOUYIN_ORIGIN.to_string(),
            detail: format!("{DOUYIN_ORIGIN}/aweme/v1/web/aweme/detail/"),
            user_posts: format!("{DOUYIN_ORIGIN}/aweme/v1/web/aweme/post/"),
            live_enter: format!("{DOUYIN_LIVE_ORIGIN}/webcast/room/web/enter/"),
            ttwid_register: "https://ttwid.bytedance.com/ttwid/union/register/".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct DouyinClient {
    http: reqwest::Client,
    endpoints: DouyinEndpoints,
    anonymous_ttwid: Arc<OnceCell<String>>,
}

impl DouyinClient {
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
            endpoints: DouyinEndpoints::default(),
            anonymous_ttwid: Arc::new(OnceCell::new()),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: DouyinEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(input: &str) -> Result<DouyinResource, ProviderClientError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "Douyin URL or ID is required".to_string(),
            ));
        }
        if input.chars().all(|value| value.is_ascii_digit()) {
            return Ok(DouyinResource::Video {
                aweme_id: input.to_string(),
            });
        }
        let url = Url::parse(input).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid Douyin URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !host.ends_with("douyin.com") {
            return Err(ProviderClientError::InvalidConfig(
                "URL is outside Douyin".to_string(),
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
            return numeric_resource(segments.get(index + 1).copied(), false);
        }
        if host == "live.douyin.com" {
            return room_resource(segments.first().copied());
        }
        Err(ProviderClientError::InvalidConfig(
            "Douyin URL must identify a video or live room".to_string(),
        ))
    }

    pub async fn resolve_resource(
        &self,
        input: &str,
        session: Option<&DouyinSession>,
    ) -> Result<DouyinResource, ProviderClientError> {
        if let Ok(resource) = Self::parse_resource(input) {
            return Ok(resource);
        }
        let url = Url::parse(input.trim()).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid Douyin URL: {error}"))
        })?;
        if url.host_str() != Some("v.douyin.com") {
            return Self::parse_resource(input);
        }
        let cookie = self.session_cookie(session).await?;
        let response = Self::request(self.http.get(url), &cookie, DOUYIN_ORIGIN)
            .send()
            .await?;
        Self::parse_resource(response.url().as_str())
    }

    pub async fn resolve(
        &self,
        input: &str,
        session: Option<&DouyinSession>,
    ) -> Result<DouyinMedia, ProviderClientError> {
        match self.resolve_resource(input, session).await? {
            DouyinResource::Video { aweme_id } => self.video(&aweme_id, session).await,
            DouyinResource::Live { web_rid } => self.live(&web_rid, session).await,
        }
    }

    pub async fn video(
        &self,
        aweme_id: &str,
        session: Option<&DouyinSession>,
    ) -> Result<DouyinMedia, ProviderClientError> {
        validate_numeric_id(aweme_id, "aweme ID")?;
        let cookie = self.session_cookie(session).await?;
        let params = web_params([("aweme_id", aweme_id.to_string())]);
        let envelope: AwemeDetailEnvelope = fetch_json(Self::request(
            self.http
                .get(&self.endpoints.detail)
                .query(&signed_params(&params)),
            &cookie,
            DOUYIN_ORIGIN,
        ))
        .await?;
        check_api(envelope.status_code, &envelope.status_msg)?;
        let aweme = envelope
            .aweme_detail
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: "Douyin video is unavailable or requires a fresher cookie".to_string(),
            })?;
        media_from_aweme(&aweme)
    }

    pub async fn user_posts(
        &self,
        sec_uid: &str,
        cursor: Option<&str>,
        count: u32,
        session: Option<&DouyinSession>,
    ) -> Result<DouyinListPage, ProviderClientError> {
        let sec_uid = sec_uid.trim();
        if sec_uid.is_empty() || sec_uid.len() > 256 {
            return Err(ProviderClientError::InvalidConfig(
                "Douyin sec_uid is invalid".to_string(),
            ));
        }
        let count = count.clamp(1, 50);
        let cursor = cursor.unwrap_or("0");
        if cursor.is_empty() || !cursor.chars().all(|value| value.is_ascii_digit()) {
            return Err(ProviderClientError::InvalidConfig(
                "Douyin cursor must be an unsigned integer".to_string(),
            ));
        }
        let cookie = self.session_cookie(session).await?;
        let params = web_params([
            ("sec_user_id", sec_uid.to_string()),
            ("max_cursor", cursor.to_string()),
            ("count", count.to_string()),
            ("locate_query", "false".to_string()),
            ("show_live_replay_strategy", "1".to_string()),
            ("need_time_list", "1".to_string()),
            ("publish_video_strategy_type", "2".to_string()),
            ("from_user_page", "1".to_string()),
        ]);
        let envelope: AwemeListEnvelope = fetch_json(Self::request(
            self.http
                .get(&self.endpoints.user_posts)
                .query(&signed_params(&params)),
            &cookie,
            DOUYIN_ORIGIN,
        ))
        .await?;
        check_api(envelope.status_code, &envelope.status_msg)?;
        let aweme_list = envelope.aweme_list.unwrap_or_default();
        let items = aweme_list
            .iter()
            .filter(|aweme| {
                aweme
                    .video
                    .as_ref()
                    .and_then(|video| video.play_addr.as_ref())
                    .is_some()
            })
            .map(list_item_from_aweme)
            .collect();
        Ok(DouyinListPage {
            items,
            cursor: envelope.max_cursor.and_then(json_cursor),
            has_more: envelope.has_more,
        })
    }

    pub async fn live(
        &self,
        web_rid: &str,
        session: Option<&DouyinSession>,
    ) -> Result<DouyinMedia, ProviderClientError> {
        validate_room_key(web_rid)?;
        let cookie = self.session_cookie(session).await?;
        let params = live_params(web_rid);
        let envelope: LiveEnvelope = fetch_json(Self::request(
            self.http
                .get(&self.endpoints.live_enter)
                .query(&signed_params(&params)),
            &cookie,
            DOUYIN_LIVE_ORIGIN,
        ))
        .await?;
        let room = envelope
            .data
            .data
            .first()
            .cloned()
            .or(envelope.data.room)
            .ok_or_else(|| ProviderClientError::Api {
                code: 404,
                message: envelope
                    .data
                    .prompts
                    .unwrap_or_else(|| "Douyin live room is unavailable".to_string()),
            })?;
        media_from_live(web_rid, room, envelope.data.user.as_ref())
    }

    async fn session_cookie(
        &self,
        session: Option<&DouyinSession>,
    ) -> Result<String, ProviderClientError> {
        let mut cookie = session
            .and_then(|session| session.cookie.as_deref())
            .unwrap_or_default()
            .trim()
            .trim_end_matches(';')
            .to_string();
        if !has_cookie(&cookie, "ttwid") {
            let ttwid = self
                .anonymous_ttwid
                .get_or_try_init(|| self.fetch_ttwid())
                .await?;
            push_cookie(&mut cookie, "ttwid", ttwid);
        }
        if !has_cookie(&cookie, "odin_ttid") {
            push_cookie(&mut cookie, "odin_ttid", &generate_odin_ttid());
        }
        if !has_cookie(&cookie, "__ac_nonce") {
            push_cookie(&mut cookie, "__ac_nonce", &generate_nonce());
        }
        if !has_cookie(&cookie, "s_v_web_id") {
            push_cookie(&mut cookie, "s_v_web_id", &generate_verify_fp());
        }
        Ok(cookie)
    }

    async fn fetch_ttwid(&self) -> Result<String, ProviderClientError> {
        let response = self
            .http
            .post(&self.endpoints.ttwid_register)
            .header(USER_AGENT, PROVIDER_USER_AGENT)
            .json(&json!({
                "region": "cn",
                "aid": 6383,
                "needFid": false,
                "service": self.endpoints.web_base,
                "union": true,
                "fid": ""
            }))
            .send()
            .await?;
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .find_map(|value| value.strip_prefix("ttwid="))
            .map(ToString::to_string)
            .ok_or_else(|| {
                ProviderClientError::Auth("Douyin did not issue a ttwid cookie".to_string())
            })
    }

    fn request(
        request: reqwest::RequestBuilder,
        cookie: &str,
        origin: &'static str,
    ) -> reqwest::RequestBuilder {
        request
            .header(USER_AGENT, PROVIDER_USER_AGENT)
            .header(ORIGIN, origin)
            .header(REFERER, format!("{origin}/"))
            .header(COOKIE, cookie)
    }
}

fn numeric_resource(
    value: Option<&str>,
    live: bool,
) -> Result<DouyinResource, ProviderClientError> {
    let value = value.unwrap_or_default();
    validate_numeric_id(value, if live { "web room ID" } else { "aweme ID" })?;
    Ok(if live {
        DouyinResource::Live {
            web_rid: value.to_string(),
        }
    } else {
        DouyinResource::Video {
            aweme_id: value.to_string(),
        }
    })
}

fn room_resource(value: Option<&str>) -> Result<DouyinResource, ProviderClientError> {
    let value = value.unwrap_or_default();
    validate_room_key(value)?;
    Ok(DouyinResource::Live {
        web_rid: value.to_string(),
    })
}

fn validate_numeric_id(value: &str, name: &str) -> Result<(), ProviderClientError> {
    if value.is_empty() || value.len() > 32 || !value.chars().all(|value| value.is_ascii_digit()) {
        return Err(ProviderClientError::InvalidConfig(format!(
            "Douyin {name} is invalid"
        )));
    }
    Ok(())
}

fn validate_room_key(value: &str) -> Result<(), ProviderClientError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
    {
        return Err(ProviderClientError::InvalidConfig(
            "Douyin live room key is invalid".to_string(),
        ));
    }
    Ok(())
}

fn web_params<const N: usize>(extra: [(&str, String); N]) -> Vec<(String, String)> {
    let mut params = vec![
        ("device_platform".to_string(), "webapp".to_string()),
        ("aid".to_string(), "6383".to_string()),
        ("channel".to_string(), "channel_pc_web".to_string()),
        ("pc_client_type".to_string(), "1".to_string()),
        ("pc_libra_divert".to_string(), "Windows".to_string()),
        ("support_h265".to_string(), "1".to_string()),
        ("support_dash".to_string(), "1".to_string()),
        ("version_code".to_string(), "290100".to_string()),
        ("version_name".to_string(), "29.1.0".to_string()),
        ("cookie_enabled".to_string(), "true".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_name".to_string(), "Chrome".to_string()),
        ("browser_version".to_string(), "120.0.0.0".to_string()),
    ];
    params.extend(
        extra
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    );
    let verify_fp = generate_verify_fp();
    params.push(("verifyFp".to_string(), verify_fp.clone()));
    params.push(("fp".to_string(), verify_fp));
    params.push(("msToken".to_string(), generate_ms_token()));
    params
}

fn live_params(web_rid: &str) -> Vec<(String, String)> {
    vec![
        ("app_name".to_string(), "douyin_web".to_string()),
        ("enter_from".to_string(), "web_live".to_string()),
        ("live_id".to_string(), "1".to_string()),
        ("aid".to_string(), "6383".to_string()),
        ("device_platform".to_string(), "web".to_string()),
        ("browser_language".to_string(), "zh-CN".to_string()),
        ("browser_platform".to_string(), "Win32".to_string()),
        ("browser_name".to_string(), "Chrome".to_string()),
        ("browser_version".to_string(), "120.0.0.0".to_string()),
        ("web_rid".to_string(), web_rid.to_string()),
        ("is_need_double_stream".to_string(), "true".to_string()),
        ("msToken".to_string(), generate_ms_token()),
    ]
}

fn signed_params(params: &[(String, String)]) -> Vec<(String, String)> {
    let query = serde_urlencoded::to_string(params).expect("string pairs serialize");
    let mut output = params.to_vec();
    output.push((
        "a_bogus".to_string(),
        sign_a_bogus(&query, PROVIDER_USER_AGENT),
    ));
    output
}

fn check_api(code: i64, message: &str) -> Result<(), ProviderClientError> {
    if code == 0 {
        Ok(())
    } else {
        Err(ProviderClientError::Api {
            code,
            message: message.to_string(),
        })
    }
}

fn media_from_aweme(aweme: &Aweme) -> Result<DouyinMedia, ProviderClientError> {
    let default_author = RawAuthor::default();
    let author = author_from_raw(aweme.author.as_ref().unwrap_or(&default_author));
    let video = aweme
        .video
        .as_ref()
        .ok_or_else(|| ProviderClientError::Api {
            code: 415,
            message: "Douyin work has no playable video".to_string(),
        })?;
    let mut variants = Vec::new();
    for bitrate in &video.bit_rate {
        let Some(address) = &bitrate.play_addr else {
            continue;
        };
        for url in &address.url_list {
            variants.push(DouyinVariant {
                url: https_url(url),
                format: DouyinStreamFormat::Mp4,
                quality: if bitrate.gear_name.is_empty() {
                    format!("quality {}", bitrate.quality_type)
                } else {
                    bitrate.gear_name.clone()
                },
                codec: Some(
                    if bitrate.is_bytevc1 == 1 {
                        "hevc"
                    } else {
                        "avc"
                    }
                    .to_string(),
                ),
                width: address.width.or(video.width),
                height: address.height.or(video.height),
                fps: bitrate.fps,
                bitrate: bitrate.bit_rate,
                audio_only: false,
                headers_required: true,
            });
        }
    }
    if variants.is_empty() {
        if let Some(address) = &video.play_addr {
            variants.extend(address.url_list.iter().map(|url| {
                DouyinVariant {
                    url: https_url(url),
                    format: DouyinStreamFormat::Mp4,
                    quality: "default".to_string(),
                    codec: None,
                    width: address.width.or(video.width),
                    height: address.height.or(video.height),
                    fps: None,
                    bitrate: address
                        .data_size
                        .zip(video.duration)
                        .and_then(|(size, duration)| {
                            (duration > 0).then_some(size.saturating_mul(8_000) / duration)
                        }),
                    audio_only: false,
                    headers_required: true,
                }
            }));
        }
    }
    if variants.is_empty() {
        return Err(ProviderClientError::Api {
            code: 404,
            message: "Douyin video has no playback URL".to_string(),
        });
    }
    let statistics = aweme.statistics.as_ref();
    Ok(DouyinMedia {
        resource: DouyinResource::Video {
            aweme_id: aweme.aweme_id.clone(),
        },
        metadata: DouyinMetadata {
            id: aweme.aweme_id.clone(),
            kind: DouyinMediaKind::Video,
            title: title(aweme),
            description: aweme.desc.clone(),
            author,
            cover: image(video.cover.as_ref().or(video.origin_cover.as_ref())),
            dynamic_cover: image(video.dynamic_cover.as_ref()),
            duration_ms: aweme.duration.or(video.duration),
            created_at: aweme.create_time,
            is_live: false,
            view_count: statistics.and_then(|value| value.play_count),
            like_count: statistics.and_then(|value| value.digg_count),
            comment_count: statistics.and_then(|value| value.comment_count),
            share_count: statistics.and_then(|value| value.share_count),
            collect_count: statistics.and_then(|value| value.collect_count),
            music_title: aweme.music.as_ref().and_then(|value| value.title.clone()),
            music_author: aweme.music.as_ref().and_then(|value| value.author.clone()),
        },
        room_id: None,
        variants,
    })
}

fn media_from_live(
    web_rid: &str,
    room: LiveRoom,
    fallback_author: Option<&RawLiveAuthor>,
) -> Result<DouyinMedia, ProviderClientError> {
    let default_author = RawLiveAuthor::default();
    let raw_author = room
        .owner
        .as_ref()
        .or(fallback_author)
        .unwrap_or(&default_author);
    let variants = room
        .stream_url
        .as_ref()
        .map(variants_from_live)
        .unwrap_or_default();
    Ok(DouyinMedia {
        resource: DouyinResource::Live {
            web_rid: web_rid.to_string(),
        },
        metadata: DouyinMetadata {
            id: room.id_str.clone(),
            kind: DouyinMediaKind::Live,
            title: room.title.clone(),
            description: room.title,
            author: live_author(raw_author),
            cover: image(room.cover.as_ref()),
            dynamic_cover: None,
            duration_ms: None,
            created_at: None,
            is_live: room.status == 2,
            view_count: None,
            like_count: None,
            comment_count: None,
            share_count: None,
            collect_count: None,
            music_title: None,
            music_author: None,
        },
        room_id: Some(room.id_str),
        variants,
    })
}

fn variants_from_live(stream_url: &RawStreamUrl) -> Vec<DouyinVariant> {
    let mut output = Vec::new();
    if let Some(data) = stream_url
        .live_core_sdk_data
        .as_ref()
        .and_then(|data| data.pull_data.as_ref())
    {
        append_pull_data(&mut output, data);
    }
    for data in stream_url.pull_datas.values() {
        append_pull_data(&mut output, data);
    }
    if output.is_empty() {
        output.extend(
            stream_url.flv_pull_url.iter().map(|(quality, url)| {
                live_variant(url, DouyinStreamFormat::Flv, quality, None, None)
            }),
        );
        output.extend(
            stream_url.hls_pull_url_map.iter().map(|(quality, url)| {
                live_variant(url, DouyinStreamFormat::Hls, quality, None, None)
            }),
        );
    }
    output.sort_by(|left, right| left.url.cmp(&right.url));
    output.dedup_by(|left, right| left.url == right.url);
    output
}

fn append_pull_data(output: &mut Vec<DouyinVariant>, data: &RawPullData) {
    let qualities = data
        .options
        .as_ref()
        .map(|options| {
            options
                .qualities
                .iter()
                .map(|quality| (quality.sdk_key.as_str(), quality))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let Some(streams) = data
        .stream_data
        .get("data")
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };
    for (key, stream) in streams {
        let Some(main) = stream.get("main") else {
            continue;
        };
        let details = qualities.get(key.as_str()).copied();
        let quality = details
            .map(|value| value.name.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or(key);
        let codec = details
            .map(|value| normalize_codec(&value.v_codec))
            .filter(|value| !value.is_empty());
        let bitrate = details.and_then(|value| value.v_bit_rate);
        for (field, format) in [
            ("flv", DouyinStreamFormat::Flv),
            ("hls", DouyinStreamFormat::Hls),
            ("dash", DouyinStreamFormat::Dash),
            ("cmaf", DouyinStreamFormat::Cmaf),
            ("ll_hls", DouyinStreamFormat::LlHls),
            ("http_ts", DouyinStreamFormat::HttpTs),
        ] {
            if let Some(url) = main
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|url| !url.is_empty())
            {
                let mut variant = live_variant(url, format, quality, codec.clone(), bitrate);
                variant.fps = details.and_then(|value| value.fps);
                if let Some((width, height)) =
                    details.and_then(|value| parse_resolution(&value.resolution))
                {
                    variant.width = Some(width);
                    variant.height = Some(height);
                }
                variant.audio_only = key == "ao";
                output.push(variant);
            }
        }
    }
}

fn live_variant(
    url: &str,
    format: DouyinStreamFormat,
    quality: &str,
    codec: Option<String>,
    bitrate: Option<u64>,
) -> DouyinVariant {
    DouyinVariant {
        url: https_url(url),
        format,
        quality: quality.to_string(),
        codec,
        width: None,
        height: None,
        fps: None,
        bitrate,
        audio_only: false,
        headers_required: true,
    }
}

fn list_item_from_aweme(aweme: &Aweme) -> DouyinListItem {
    let video = aweme.video.as_ref();
    let default_author = RawAuthor::default();
    DouyinListItem {
        aweme_id: aweme.aweme_id.clone(),
        title: title(aweme),
        author: author_from_raw(aweme.author.as_ref().unwrap_or(&default_author)),
        cover: image(video.and_then(|video| video.cover.as_ref().or(video.origin_cover.as_ref()))),
        duration_ms: aweme
            .duration
            .or_else(|| video.and_then(|video| video.duration)),
        created_at: aweme.create_time,
    }
}

fn title(aweme: &Aweme) -> String {
    if aweme.desc.trim().is_empty() {
        format!("Douyin {}", aweme.aweme_id)
    } else {
        aweme.desc.clone()
    }
}

fn author_from_raw(author: &RawAuthor) -> DouyinAuthor {
    DouyinAuthor {
        id: author.uid.clone(),
        sec_uid: author.sec_uid.clone(),
        unique_id: author.unique_id.clone(),
        nickname: author.nickname.clone(),
        avatar: image(author.avatar_thumb.as_ref()),
    }
}

fn live_author(author: &RawLiveAuthor) -> DouyinAuthor {
    DouyinAuthor {
        id: author.id_str.clone(),
        sec_uid: author.sec_uid.clone(),
        unique_id: None,
        nickname: author.nickname.clone(),
        avatar: image(author.avatar_thumb.as_ref()),
    }
}

fn image(image: Option<&RawImage>) -> Option<DouyinImage> {
    let image = image?;
    Some(DouyinImage {
        url: https_url(image.url_list.first()?),
        width: image.width,
        height: image.height,
    })
}

fn normalize_codec(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "264" | "h264" => "avc".to_string(),
        "265" | "h265" | "bytevc1" => "hevc".to_string(),
        value => value.to_string(),
    }
}

fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', '*'])?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn https_url(value: &str) -> String {
    value.replacen("http://", "https://", 1)
}

fn json_cursor(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn has_cookie(cookie: &str, name: &str) -> bool {
    cookie
        .split(';')
        .any(|part| part.trim().starts_with(&format!("{name}=")))
}

fn push_cookie(cookie: &mut String, name: &str, value: &str) {
    if !cookie.is_empty() {
        cookie.push_str("; ");
    }
    cookie.push_str(name);
    cookie.push('=');
    cookie.push_str(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn endpoints(server: &MockServer) -> DouyinEndpoints {
        DouyinEndpoints {
            web_base: server.uri(),
            detail: format!("{}/detail", server.uri()),
            user_posts: format!("{}/posts", server.uri()),
            live_enter: format!("{}/live", server.uri()),
            ttwid_register: format!("{}/ttwid", server.uri()),
        }
    }

    async fn client(server: &MockServer) -> DouyinClient {
        crate::install_process_crypto_provider();
        Mock::given(method("POST"))
            .and(path("/ttwid"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("set-cookie", "ttwid=test-session; Path=/"),
            )
            .mount(server)
            .await;
        DouyinClient::with_http_client(reqwest::Client::new()).with_endpoints(endpoints(server))
    }

    #[test]
    fn parses_video_and_live_resources() {
        assert_eq!(
            DouyinClient::parse_resource("https://www.douyin.com/video/7123456789012345678")
                .expect("test operation should succeed"),
            DouyinResource::Video {
                aweme_id: "7123456789012345678".to_string()
            }
        );
        assert_eq!(
            DouyinClient::parse_resource("https://live.douyin.com/room_name")
                .expect("test operation should succeed"),
            DouyinResource::Live {
                web_rid: "room_name".to_string()
            }
        );
    }

    #[tokio::test]
    async fn resolves_video_metadata_and_all_bitrates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/detail"))
            .and(query_param("aweme_id", "7123456789012345678"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status_code": 0,
                "aweme_detail": {
                    "aweme_id": "7123456789012345678",
                    "desc": "video title",
                    "create_time": 1_700_000_000,
                    "author": {"uid": "1", "sec_uid": "sec", "nickname": "creator"},
                    "statistics": {"play_count": 42, "digg_count": 7},
                    "music": {"title": "song", "author": "artist"},
                    "video": {
                        "duration": 15000,
                        "cover": {
                            "url_list": ["http://img/cover.jpg"],
                            "width": 1080,
                            "height": 1920
                        },
                        "bit_rate": [{
                            "gear_name": "1080p",
                            "bit_rate": 2_000_000,
                            "fps": 60,
                            "is_bytevc1": 1,
                            "play_addr": {
                                "url_list": ["http://media/video.mp4"],
                                "width": 1080,
                                "height": 1920
                            }
                        }]
                    }
                }
            })))
            .mount(&server)
            .await;
        let media = client(&server)
            .await
            .video("7123456789012345678", None)
            .await
            .expect("test operation should succeed");
        assert_eq!(media.metadata.title, "video title");
        assert_eq!(media.metadata.view_count, Some(42));
        assert_eq!(media.variants[0].codec.as_deref(), Some("hevc"));
        assert_eq!(media.variants[0].fps, Some(60));
        assert_eq!(media.variants[0].url, "https://media/video.mp4");
    }

    #[tokio::test]
    async fn lists_video_posts_with_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/posts"))
            .and(query_param("sec_user_id", "sec-user"))
            .and(query_param("max_cursor", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status_code": 0,
                "has_more": 1,
                "max_cursor": 200,
                "aweme_list": [{
                    "aweme_id": "7001",
                    "desc": "one",
                    "author": {"uid": "1", "sec_uid": "sec-user", "nickname": "creator"},
                    "video": {
                        "duration": 1000,
                        "play_addr": {"url_list": ["https://media/one.mp4"]}
                    }
                }, {
                    "aweme_id": "7002",
                    "desc": "image post",
                    "author": {"uid": "1", "sec_uid": "sec-user", "nickname": "creator"}
                }]
            })))
            .mount(&server)
            .await;
        let page = client(&server)
            .await
            .user_posts("sec-user", Some("100"), 18, None)
            .await
            .expect("test operation should succeed");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.cursor.as_deref(), Some("200"));
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn treats_null_video_posts_as_an_empty_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/posts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status_code": 0,
                "has_more": 0,
                "max_cursor": null,
                "aweme_list": null
            })))
            .mount(&server)
            .await;

        let page = client(&server)
            .await
            .user_posts("sec-user", None, 18, None)
            .await
            .expect("null post lists should deserialize as an empty page");

        assert!(page.items.is_empty());
        assert_eq!(page.cursor, None);
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn resolves_live_sdk_protocols_and_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/live"))
            .and(query_param("web_rid", "12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "user": {
                        "id_str": "9",
                        "sec_uid": "live-sec",
                        "nickname": "anchor"
                    },
                    "data": [{
                        "id_str": "9988",
                        "status": 2,
                        "title": "live title",
                        "cover": {"url_list": ["https://img/live.jpg"]},
                        "stream_url": {"live_core_sdk_data": {"pull_data": {
                            "options": {"qualities": [{
                                "name": "原画",
                                "sdk_key": "origin",
                                "v_codec": "h265",
                                "resolution": "1920x1080",
                                "v_bit_rate": 6_000_000,
                                "fps": 60
                            }]},
                            "stream_data": "{\"data\":{\"origin\":{\"main\":{\"flv\":\"http://media/live.flv\",\"hls\":\"https://media/live.m3u8\",\"dash\":\"https://media/live.mpd\",\"cmaf\":\"https://media/live.cmaf\",\"ll_hls\":\"https://media/live-ll.m3u8\",\"http_ts\":\"https://media/live.ts\"}}}}"
                        }}}
                    }]
                }
            })))
            .mount(&server)
            .await;
        let media = client(&server)
            .await
            .live("12345", None)
            .await
            .expect("test operation should succeed");
        assert!(media.metadata.is_live);
        assert_eq!(media.room_id.as_deref(), Some("9988"));
        assert_eq!(media.variants.len(), 6);
        assert!(media
            .variants
            .iter()
            .any(|variant| variant.format == DouyinStreamFormat::Dash));
        assert!(media.variants.iter().all(|variant| variant.fps == Some(60)));
    }
}
