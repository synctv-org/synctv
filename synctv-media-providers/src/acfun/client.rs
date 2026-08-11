use std::collections::HashSet;
use std::time::Duration;

use reqwest::header::{COOKIE, REFERER};
use url::Url;

use super::types::{
    BangumiPage, DanmakuResponse, LiveInfoResponse, LivePlayJson, PlayJson, RawDanmaku,
    StartPlayResponse, VideoInfo, VideoPage, VisitorResponse,
};
use super::{
    AcFunDanmaku, AcFunLiveSession, AcFunMedia, AcFunMetadata, AcFunPlayback, AcFunQuality,
    AcFunResource, AcFunResourceKind, AcFunSession, AcFunStreamFormat,
};
use crate::{
    check_response, fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT,
};

const ACFUN_ORIGIN: &str = "https://www.acfun.cn";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFunEndpoints {
    pub web_base: String,
    pub live_base: String,
    pub visitor_login: String,
    pub start_play: String,
    pub danmaku_list: String,
}

impl Default for AcFunEndpoints {
    fn default() -> Self {
        Self {
            web_base: ACFUN_ORIGIN.to_string(),
            live_base: "https://live.acfun.cn".to_string(),
            visitor_login: "https://id.app.acfun.cn/rest/app/visitor/login".to_string(),
            start_play: "https://api.kuaishouzt.com/rest/zt/live/web/startPlay".to_string(),
            danmaku_list: format!("{ACFUN_ORIGIN}/rest/pc-direct/new-danmaku/list"),
        }
    }
}

#[derive(Debug, Clone)]
struct VisitorAuth {
    user_id: i64,
    device_id: String,
    security_key: String,
    service_token: String,
}

#[derive(Clone)]
pub struct AcFunClient {
    http: reqwest::Client,
    endpoints: AcFunEndpoints,
    visitor_cache: moka::future::Cache<String, VisitorAuth>,
}

impl AcFunClient {
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
            endpoints: AcFunEndpoints::default(),
            visitor_cache: moka::future::Cache::builder()
                .max_capacity(4)
                .time_to_live(Duration::from_mins(10))
                .build(),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: AcFunEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(raw: &str) -> Result<AcFunResource, ProviderClientError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "AcFun resource is required".to_string(),
            ));
        }
        if !raw.contains("://") {
            return parse_identifier(raw, None);
        }
        let url = Url::parse(raw).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid AcFun URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !matches!(
            host.as_str(),
            "acfun.cn" | "www.acfun.cn" | "m.acfun.cn" | "live.acfun.cn"
        ) {
            return Err(ProviderClientError::InvalidConfig(
                "URL is not an AcFun resource".to_string(),
            ));
        }
        let parts = url
            .path_segments()
            .map(|parts| parts.filter(|part| !part.is_empty()).collect::<Vec<_>>())
            .unwrap_or_default();
        let query = url.query().map(str::to_string);
        match parts.as_slice() {
            ["v" | "bangumi", id] => parse_identifier(id, query),
            ["live", id] => parse_live_id(id),
            [id] if host == "live.acfun.cn" => parse_live_id(id),
            _ => Err(ProviderClientError::InvalidConfig(
                "unsupported AcFun resource URL".to_string(),
            )),
        }
    }

    pub async fn resolve(
        &self,
        resource: &AcFunResource,
        session: Option<&AcFunSession>,
    ) -> Result<AcFunMedia, ProviderClientError> {
        match resource.kind {
            AcFunResourceKind::Video => self.resolve_video(resource, session).await,
            AcFunResourceKind::Bangumi => self.resolve_bangumi(resource, session).await,
            AcFunResourceKind::Live => self.resolve_live(resource, session).await,
        }
    }

    pub async fn metadata(
        &self,
        resource: &AcFunResource,
        session: Option<&AcFunSession>,
    ) -> Result<AcFunMetadata, ProviderClientError> {
        Ok(self.resolve(resource, session).await?.metadata)
    }

    pub async fn playback(
        &self,
        resource: &AcFunResource,
        session: Option<&AcFunSession>,
    ) -> Result<AcFunPlayback, ProviderClientError> {
        Ok(self.resolve(resource, session).await?.playback)
    }

    pub async fn video_danmakus(
        &self,
        resource_id: &str,
        session: Option<&AcFunSession>,
    ) -> Result<Vec<AcFunDanmaku>, ProviderClientError> {
        if resource_id.is_empty() || !resource_id.chars().all(|value| value.is_ascii_digit()) {
            return Err(ProviderClientError::InvalidConfig(
                "AcFun danmaku resource ID is invalid".to_string(),
            ));
        }
        let mut cursor = "1".to_string();
        let mut seen = HashSet::new();
        let mut output = Vec::new();
        for _ in 0..100 {
            if !seen.insert(cursor.clone()) {
                return Err(ProviderClientError::Parse(
                    "AcFun danmaku cursor repeated".to_string(),
                ));
            }
            let response: DanmakuResponse = fetch_json(Self::request(
                self.http.post(&self.endpoints.danmaku_list).form(&[
                    ("resourceId", resource_id),
                    ("resourceType", "9"),
                    ("enableAdvanced", "true"),
                    ("pcursor", cursor.as_str()),
                    ("count", "200"),
                    ("sortType", "1"),
                    ("asc", "true"),
                ]),
                session,
            ))
            .await?;
            if response.result != 0 {
                return Err(ProviderClientError::Api {
                    code: response.result,
                    message: "AcFun danmaku request failed".to_string(),
                });
            }
            let count = response.danmakus.len();
            output.extend(response.danmakus.into_iter().map(map_danmaku));
            if count == 0 || response.pcursor.is_empty() || response.pcursor == "no_more" {
                return Ok(output);
            }
            cursor = response.pcursor;
        }
        Err(ProviderClientError::Parse(
            "AcFun danmaku pagination exceeded 100 pages".to_string(),
        ))
    }

    async fn resolve_video(
        &self,
        resource: &AcFunResource,
        session: Option<&AcFunSession>,
    ) -> Result<AcFunMedia, ProviderClientError> {
        let url = resource_url(&self.endpoints.web_base, resource);
        let page = self.page(url, session).await?;
        let value = extract_json_object(&page, "window.videoInfo").ok_or_else(|| {
            ProviderClientError::Parse("AcFun videoInfo was not found".to_string())
        })?;
        let info: VideoPage = serde_json::from_str(value)?;
        let internal_id = value_string(&info.current_video_info.id).ok_or_else(|| {
            ProviderClientError::Parse("AcFun video ID was not found".to_string())
        })?;
        let title = video_title(&info, &internal_id);
        let metadata = AcFunMetadata {
            id: resource.id.clone(),
            title,
            author: info.user.name,
            author_id: extract_last_numeric(&info.user.href),
            category: None,
            thumbnail_url: nonempty(&info.cover_url),
            avatar_url: nonempty(&info.user.avatar_image),
            description: info.description.as_deref().and_then(nonempty),
            tags: info
                .tag_list
                .into_iter()
                .filter_map(|tag| nonempty(&tag.name))
                .collect(),
            is_live: false,
            duration_seconds: info
                .current_video_info
                .duration_millis
                .map(|value| std::time::Duration::from_millis(value).as_secs_f64()),
            view_count: info.view_count,
            like_count: info.like_count_show,
            comment_count: info.comment_count_show,
            published_at: info.current_video_info.upload_time.map(normalize_timestamp),
            started_at: None,
            danmaku_resource_id: Some(internal_id),
        };
        media_from_video_info(resource, metadata, &info.current_video_info)
    }

    async fn resolve_bangumi(
        &self,
        resource: &AcFunResource,
        session: Option<&AcFunSession>,
    ) -> Result<AcFunMedia, ProviderClientError> {
        let url = resource_url(&self.endpoints.web_base, resource);
        let page = self.page(url, session).await?;
        let value = extract_json_object(&page, "window.bangumiData").ok_or_else(|| {
            ProviderClientError::Parse("AcFun bangumiData was not found".to_string())
        })?;
        let info: BangumiPage = serde_json::from_str(value)?;
        let video = if resource.query.as_deref().is_some_and(has_highlight_query) {
            info.hl_video_info
                .as_ref()
                .unwrap_or(&info.current_video_info)
        } else {
            &info.current_video_info
        };
        let internal_id = value_string(&video.id).ok_or_else(|| {
            ProviderClientError::Parse("AcFun bangumi video ID was not found".to_string())
        })?;
        let metadata = AcFunMetadata {
            id: resource.id.clone(),
            title: first_nonempty([
                info.show_title.as_str(),
                video.title.as_str(),
                info.title.as_str(),
            ])
            .unwrap_or("AcFun bangumi")
            .to_string(),
            author: String::new(),
            author_id: None,
            category: nonempty(&info.bangumi_title),
            thumbnail_url: nonempty(&info.image),
            avatar_url: None,
            description: None,
            tags: Vec::new(),
            is_live: false,
            duration_seconds: video
                .duration_millis
                .map(|value| std::time::Duration::from_millis(value).as_secs_f64()),
            view_count: None,
            like_count: None,
            comment_count: info.comment_count,
            published_at: video.upload_time.map(normalize_timestamp),
            started_at: None,
            danmaku_resource_id: Some(internal_id),
        };
        media_from_video_info(resource, metadata, video)
    }

    async fn resolve_live(
        &self,
        resource: &AcFunResource,
        session: Option<&AcFunSession>,
    ) -> Result<AcFunMedia, ProviderClientError> {
        let author_id = resource.id.parse::<i64>().map_err(|_| {
            ProviderClientError::InvalidConfig("invalid AcFun live author ID".to_string())
        })?;
        let auth = self.visitor_auth().await?;
        let query = [
            ("subBiz", "mainApp".to_string()),
            ("kpn", "ACFUN_APP".to_string()),
            ("kpf", "PC_WEB".to_string()),
            ("userId", auth.user_id.to_string()),
            ("did", auth.device_id.clone()),
            ("acfun.api.visitor_st", auth.service_token.clone()),
        ];
        let (start, public_info) = tokio::join!(
            fetch_json::<StartPlayResponse>(
                self.http
                    .post(&self.endpoints.start_play)
                    .header(REFERER, "https://live.acfun.cn/")
                    .query(&query)
                    .form(&[
                        ("authorId", resource.id.as_str()),
                        ("pullStreamType", "FLV")
                    ]),
            ),
            fetch_json::<LiveInfoResponse>(Self::request(
                self.http
                    .get(format!(
                        "{}/api/live/info",
                        self.endpoints.live_base.trim_end_matches('/')
                    ))
                    .query(&[("authorId", resource.id.as_str())]),
                session,
            )),
        );
        let public_info = public_info.ok();
        let start = start?;
        let is_live = start.result == 1 && start.data.is_some();
        let data = start.data;
        let metadata = live_metadata(
            resource,
            author_id,
            public_info.as_ref(),
            data.as_ref(),
            is_live,
        );
        let Some(data) = data else {
            return Ok(AcFunMedia {
                metadata,
                playback: AcFunPlayback {
                    resource: resource.clone(),
                    qualities: Vec::new(),
                },
                live_session: None,
            });
        };
        let play: LivePlayJson = serde_json::from_str(&data.video_play_res)?;
        let qualities = play
            .live_adaptive_manifest
            .into_iter()
            .flat_map(|manifest| manifest.adaptation_set.representation)
            .filter(|quality| !quality.url.is_empty())
            .map(|quality| AcFunQuality {
                name: first_nonempty([
                    &quality.name,
                    quality.quality_type.as_deref().unwrap_or(""),
                ])
                .unwrap_or("Live")
                .to_string(),
                url: quality.url,
                format: AcFunStreamFormat::Flv,
                bitrate: quality.bitrate,
                width: None,
                height: None,
                fps: None,
                codec: quality.media_type,
                quality_type: quality.quality_type,
            })
            .collect();
        Ok(AcFunMedia {
            metadata,
            playback: AcFunPlayback {
                resource: resource.clone(),
                qualities,
            },
            live_session: Some(AcFunLiveSession {
                user_id: auth.user_id,
                author_id,
                device_id: auth.device_id,
                security_key: auth.security_key,
                service_token: auth.service_token,
                live_id: data.live_id,
                tickets: data.available_tickets,
                enter_room_attach: data.enter_room_attach,
            }),
        })
    }

    async fn visitor_auth(&self) -> Result<VisitorAuth, ProviderClientError> {
        if let Some(auth) = self.visitor_cache.get("visitor").await {
            return Ok(auth);
        }
        let device_id = format!("web_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let response: VisitorResponse = fetch_json(
            self.http
                .post(&self.endpoints.visitor_login)
                .header(COOKIE, format!("_did={device_id};"))
                .form(&[("sid", "acfun.api.visitor")]),
        )
        .await?;
        if response.result != 0 {
            return Err(ProviderClientError::Api {
                code: response.result,
                message: "AcFun visitor login failed".to_string(),
            });
        }
        let auth = VisitorAuth {
            user_id: response.user_id,
            device_id,
            security_key: response.ac_security,
            service_token: response.visitor_st,
        };
        self.visitor_cache
            .insert("visitor".to_string(), auth.clone())
            .await;
        Ok(auth)
    }

    async fn page(
        &self,
        url: String,
        session: Option<&AcFunSession>,
    ) -> Result<String, ProviderClientError> {
        text_with_limit(
            check_response(Self::request(self.http.get(url), session).send().await?).await?,
        )
        .await
    }

    fn request(
        mut request: reqwest::RequestBuilder,
        session: Option<&AcFunSession>,
    ) -> reqwest::RequestBuilder {
        request = request.header(REFERER, format!("{ACFUN_ORIGIN}/"));
        if let Some(cookie) = session.and_then(|session| session.cookie.as_deref()) {
            request = request.header(COOKIE, cookie);
        }
        request
    }
}

fn parse_identifier(id: &str, query: Option<String>) -> Result<AcFunResource, ProviderClientError> {
    let id = id.trim();
    if let Some(value) = id.strip_prefix("ac") {
        if valid_numeric_parts(value) {
            return Ok(AcFunResource {
                kind: AcFunResourceKind::Video,
                id: id.to_string(),
                query,
            });
        }
    }
    if let Some(value) = id.strip_prefix("aa") {
        if valid_numeric_parts(value) {
            return Ok(AcFunResource {
                kind: AcFunResourceKind::Bangumi,
                id: id.to_string(),
                query,
            });
        }
    }
    if id.chars().all(|value| value.is_ascii_digit()) {
        return parse_live_id(id);
    }
    Err(ProviderClientError::InvalidConfig(
        "invalid AcFun resource ID".to_string(),
    ))
}

fn parse_live_id(id: &str) -> Result<AcFunResource, ProviderClientError> {
    if id.is_empty() || !id.chars().all(|value| value.is_ascii_digit()) {
        return Err(ProviderClientError::InvalidConfig(
            "invalid AcFun live author ID".to_string(),
        ));
    }
    Ok(AcFunResource {
        kind: AcFunResourceKind::Live,
        id: id.to_string(),
        query: None,
    })
}

fn valid_numeric_parts(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('_')
            .all(|part| !part.is_empty() && part.chars().all(|value| value.is_ascii_digit()))
}

fn resource_url(base: &str, resource: &AcFunResource) -> String {
    let path = match resource.kind {
        AcFunResourceKind::Video => format!("v/{}", resource.id),
        AcFunResourceKind::Bangumi => format!("bangumi/{}", resource.id),
        AcFunResourceKind::Live => format!("live/{}", resource.id),
    };
    let mut url = format!("{}/{path}", base.trim_end_matches('/'));
    if let Some(query) = &resource.query {
        url.push('?');
        url.push_str(query);
    }
    url
}

fn media_from_video_info(
    resource: &AcFunResource,
    metadata: AcFunMetadata,
    video: &VideoInfo,
) -> Result<AcFunMedia, ProviderClientError> {
    let play: PlayJson = serde_json::from_str(&video.ks_play_json)?;
    let qualities = play
        .adaptation_set
        .into_iter()
        .flat_map(|set| set.representation)
        .filter(|quality| !quality.url.is_empty())
        .map(|quality| AcFunQuality {
            name: first_nonempty([&quality.name, quality.quality_type.as_deref().unwrap_or("")])
                .unwrap_or("HLS")
                .to_string(),
            url: quality.url,
            format: AcFunStreamFormat::Hls,
            bitrate: quality.avg_bitrate,
            width: quality.width,
            height: quality.height,
            fps: quality.frame_rate.as_ref().and_then(value_u32),
            codec: quality.codecs,
            quality_type: quality.quality_type,
        })
        .collect();
    Ok(AcFunMedia {
        metadata,
        playback: AcFunPlayback {
            resource: resource.clone(),
            qualities,
        },
        live_session: None,
    })
}

fn live_metadata(
    resource: &AcFunResource,
    author_id: i64,
    public: Option<&LiveInfoResponse>,
    start: Option<&super::types::StartPlayData>,
    is_live: bool,
) -> AcFunMetadata {
    AcFunMetadata {
        id: resource.id.clone(),
        title: start
            .and_then(|value| nonempty(&value.caption))
            .or_else(|| public.and_then(|value| value.title.clone()))
            .unwrap_or_else(|| "AcFun live".to_string()),
        author: public
            .map(|value| value.user.name.clone())
            .unwrap_or_default(),
        author_id: Some(author_id.to_string()),
        category: None,
        thumbnail_url: public
            .and_then(|value| value.cover_urls.as_ref())
            .and_then(|values| values.iter().find(|value| !value.is_empty()).cloned()),
        avatar_url: public.and_then(|value| nonempty(&value.user.head_url)),
        description: public.and_then(|value| value.user.signature.clone()),
        tags: Vec::new(),
        is_live,
        duration_seconds: None,
        view_count: public.and_then(|value| value.online_count),
        like_count: None,
        comment_count: None,
        published_at: None,
        started_at: start
            .and_then(|value| value.live_start_time)
            .or_else(|| public.and_then(|value| value.create_time))
            .map(normalize_timestamp),
        danmaku_resource_id: None,
    }
}

fn video_title(info: &VideoPage, internal_id: &str) -> String {
    let Some((index, part)) = info
        .video_list
        .iter()
        .enumerate()
        .find(|(_, part)| value_string(&part.id).as_deref() == Some(internal_id))
    else {
        return info.title.clone();
    };
    if info.video_list.len() <= 1 {
        info.title.clone()
    } else {
        format!("{} P{:02} {}", info.title, index + 1, part.title)
    }
}

fn extract_json_object<'a>(source: &'a str, marker: &str) -> Option<&'a str> {
    let start = source.find(marker)? + marker.len();
    let object_start = source[start..].find('{')? + start;
    let mut depth = 0_u32;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, byte) in source[object_start..].bytes().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return source.get(object_start..=object_start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn has_highlight_query(query: &str) -> bool {
    url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| key == "ac")
}

fn value_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn map_danmaku(value: RawDanmaku) -> AcFunDanmaku {
    AcFunDanmaku {
        id: value_string(&value.danmaku_id).unwrap_or_default(),
        user_id: value_string(&value.user_id).unwrap_or_default(),
        text: value.body,
        color: value.color.unwrap_or(0xff_ff_ff),
        position_ms: value.position.unwrap_or_default(),
        created_at_ms: value.create_time,
        mode: value.mode.unwrap_or(1),
        size: value.size.unwrap_or(25),
    }
}

fn normalize_timestamp(value: i64) -> i64 {
    if value > 10_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn extract_last_numeric(value: &str) -> Option<String> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .rfind(|part| !part.is_empty())
        .map(str::to_string)
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    values.into_iter().find(|value| !value.is_empty())
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn endpoints(server: &MockServer) -> AcFunEndpoints {
        AcFunEndpoints {
            web_base: server.uri(),
            live_base: server.uri(),
            visitor_login: format!("{}/visitor/login", server.uri()),
            start_play: format!("{}/startPlay", server.uri()),
            danmaku_list: format!("{}/danmaku/list", server.uri()),
        }
    }

    fn client(server: &MockServer) -> AcFunClient {
        crate::install_process_crypto_provider();
        AcFunClient::with_http_client(reqwest::Client::new()).with_endpoints(endpoints(server))
    }

    #[test]
    fn parses_video_bangumi_and_live_resources() {
        assert_eq!(
            AcFunClient::parse_resource("https://www.acfun.cn/v/ac123_2")
                .expect("test operation should succeed")
                .kind,
            AcFunResourceKind::Video
        );
        let bangumi = AcFunClient::parse_resource(
            "https://www.acfun.cn/bangumi/aa5023171_36188_1750645?ac=2",
        )
        .expect("test operation should succeed");
        assert_eq!(bangumi.kind, AcFunResourceKind::Bangumi);
        assert_eq!(bangumi.query.as_deref(), Some("ac=2"));
        assert_eq!(
            AcFunClient::parse_resource("https://live.acfun.cn/live/265502")
                .expect("test operation should succeed")
                .kind,
            AcFunResourceKind::Live
        );
    }

    #[test]
    fn balanced_json_extraction_ignores_braces_in_strings() {
        let source = r#"window.videoInfo = {"description":"a } b","nested":{"id":1}}; tail"#;
        assert_eq!(
            extract_json_object(source, "window.videoInfo"),
            Some(r#"{"description":"a } b","nested":{"id":1}}"#)
        );
    }

    #[tokio::test]
    async fn resolves_video_page_and_hls_representations() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let play = serde_json::json!({
            "adaptationSet": [{"representation": [{
                "name": "1080P",
                "url": "https://media.example/video.m3u8",
                "avgBitrate": 4_000_000,
                "width": 1920,
                "height": 1080,
                "frameRate": "60",
                "codecs": "avc1.64002a",
                "qualityType": "QUALITY_1080P"
            }]}]
        })
        .to_string();
        let page = format!(
            r#"<script>window.videoInfo = {{
                "title":"Main", "coverUrl":"https://img.example/cover.jpg",
                "description":null, "viewCount":"12", "likeCountShow":"点赞",
                "commentCountShow":"4",
                "user":{{"name":"Author","href":"/u/42","avatarImage":"https://img.example/avatar.jpg"}},
                "tagList":[{{"name":"Rust"}}],
                "currentVideoInfo":{{"id":1002,"title":"Part 2","ksPlayJson":{},"durationMillis":90000,"uploadTime":1700000000000}},
                "videoList":[{{"id":1001,"title":"Part 1"}},{{"id":1002,"title":"Part 2"}}]
            }};</script>"#,
            serde_json::to_string(&play).expect("test operation should succeed")
        );
        Mock::given(method("GET"))
            .and(path("/v/ac123_2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page))
            .mount(&server)
            .await;
        let client = client(&server);
        let resource =
            AcFunClient::parse_resource("ac123_2").expect("test operation should succeed");
        let media = client
            .resolve(&resource, None)
            .await
            .expect("test operation should succeed");
        assert_eq!(media.metadata.title, "Main P02 Part 2");
        assert_eq!(media.metadata.author_id.as_deref(), Some("42"));
        assert_eq!(media.metadata.description, None);
        assert_eq!(media.metadata.view_count, Some(12));
        assert_eq!(media.metadata.like_count, None);
        assert_eq!(media.metadata.comment_count, Some(4));
        assert_eq!(media.metadata.danmaku_resource_id.as_deref(), Some("1002"));
        assert_eq!(media.playback.qualities[0].height, Some(1080));
        assert_eq!(media.playback.qualities[0].fps, Some(60));
    }

    #[tokio::test]
    async fn resolves_live_session_metadata_and_flv_qualities() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/visitor/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": 0,
                "userId": 7,
                "acSecurity": "MDEyMzQ1Njc4OWFiY2RlZg==",
                "acfun.api.visitor_st": "visitor-token"
            })))
            .mount(&server)
            .await;
        let video_play_res = serde_json::json!({
            "liveAdaptiveManifest": [{"adaptationSet": {"representation": [{
                "name": "Original", "url": "https://media.example/live.flv",
                "bitrate": 8_000_000, "qualityType": "ORIGIN", "mediaType": "video"
            }]}}]
        })
        .to_string();
        Mock::given(method("POST"))
            .and(path("/startPlay"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": 1,
                "data": {
                    "liveId": "live-1", "availableTickets": ["ticket-1"],
                    "enterRoomAttach": "attach", "caption": "Live title",
                    "videoPlayRes": video_play_res, "liveStartTime": 1_700_000_000_000_i64
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/live/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "authorId": 265_502, "title": "Public title", "onlineCount": 123,
                "coverUrls": ["https://img.example/live.jpg"],
                "user": {"name": "Streamer", "headUrl": "https://img.example/avatar.jpg", "signature": "Bio"}
            })))
            .mount(&server)
            .await;
        let client = client(&server);
        let resource =
            AcFunClient::parse_resource("265502").expect("test operation should succeed");
        let media = client
            .resolve(&resource, None)
            .await
            .expect("test operation should succeed");
        assert!(media.metadata.is_live);
        assert_eq!(media.metadata.title, "Live title");
        assert_eq!(media.playback.qualities[0].name, "Original");
        let session = media.live_session.expect("test operation should succeed");
        assert_eq!(session.live_id, "live-1");
        assert_eq!(session.tickets, ["ticket-1"]);
    }

    #[tokio::test]
    async fn follows_vod_danmaku_cursor_until_no_more() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/danmaku/list"))
            .and(body_string_contains("pcursor=1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": 0,
                "pcursor": "next-token",
                "danmakus": [{"danmakuId": 1, "userId": 2, "body": "first", "position": 1000}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/danmaku/list"))
            .and(body_string_contains("pcursor=next-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": 0,
                "pcursor": "no_more",
                "danmakus": [{"danmakuId": 3, "userId": 4, "body": "second", "position": 2000}]
            })))
            .mount(&server)
            .await;
        let client = client(&server);
        let danmakus = client
            .video_danmakus("1002", None)
            .await
            .expect("test operation should succeed");
        assert_eq!(danmakus.len(), 2);
        assert_eq!(danmakus[1].text, "second");
    }
}
