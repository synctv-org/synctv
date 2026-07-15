use reqwest::header::{COOKIE, ORIGIN, REFERER};
use url::Url;

use super::sign::sign_anti_code;
use super::types::{
    HuyaChatIdentity, HuyaMedia, HuyaMetadata, HuyaPlayback, HuyaQuality, HuyaResource,
    HuyaResourceKind, HuyaSession, HuyaStreamFormat, MomentEnvelope, RawBitrate, RawDefinition,
    RawLiveInfo, RawMoment, RawStream, WebStreamResponse,
};
use crate::{
    check_response, fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT,
};

const HUYA_ORIGIN: &str = "https://www.huya.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaEndpoints {
    pub room_base: String,
    pub mobile_base: String,
    pub moment: String,
}

impl Default for HuyaEndpoints {
    fn default() -> Self {
        Self {
            room_base: HUYA_ORIGIN.to_string(),
            mobile_base: "https://m.huya.com".to_string(),
            moment: "https://liveapi.huya.com/moment/getMomentContent".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct HuyaClient {
    http: reqwest::Client,
    endpoints: HuyaEndpoints,
}

impl HuyaClient {
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
            endpoints: HuyaEndpoints::default(),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: HuyaEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(raw: &str) -> Result<HuyaResource, ProviderClientError> {
        let raw = raw.trim();
        if !raw.contains("://") {
            return parse_huya_id(raw, HuyaResourceKind::Live);
        }
        let url = Url::parse(raw).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid Huya URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if host != "huya.com" && host != "www.huya.com" && host != "m.huya.com" {
            return Err(ProviderClientError::InvalidConfig(
                "URL is not a Huya resource".to_string(),
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
        if let ["video", "play", file] = segments.as_slice() {
            return parse_huya_id(
                file.strip_suffix(".html").unwrap_or(file),
                HuyaResourceKind::Video,
            );
        }
        segments.first().map_or_else(
            || {
                Err(ProviderClientError::InvalidConfig(
                    "Huya room ID is missing".to_string(),
                ))
            },
            |id| parse_huya_id(id, HuyaResourceKind::Live),
        )
    }

    pub async fn resolve(
        &self,
        resource: &HuyaResource,
        session: Option<&HuyaSession>,
    ) -> Result<HuyaMedia, ProviderClientError> {
        match resource.kind {
            HuyaResourceKind::Live => self.resolve_live(resource, session).await,
            HuyaResourceKind::Video => self.resolve_video(resource, session).await,
        }
    }

    pub async fn metadata(
        &self,
        resource: &HuyaResource,
        session: Option<&HuyaSession>,
    ) -> Result<HuyaMetadata, ProviderClientError> {
        Ok(self.resolve(resource, session).await?.metadata)
    }

    pub async fn playback(
        &self,
        resource: &HuyaResource,
        session: Option<&HuyaSession>,
    ) -> Result<HuyaPlayback, ProviderClientError> {
        Ok(self.resolve(resource, session).await?.playback)
    }

    pub async fn chat_identity(
        &self,
        room_id: &str,
        session: Option<&HuyaSession>,
    ) -> Result<HuyaChatIdentity, ProviderClientError> {
        parse_huya_id(room_id, HuyaResourceKind::Live)?;
        let url = format!(
            "{}/{}",
            self.endpoints.mobile_base.trim_end_matches('/'),
            room_id
        );
        let mut request = self.http.get(url);
        if let Some(cookie) = session.and_then(|session| session.cookie.as_deref()) {
            request = request.header(COOKIE, cookie);
        }
        let page = text_with_limit(check_response(request.send().await?).await?).await?;
        Ok(HuyaChatIdentity {
            presenter_uid: capture_i64(&page, "ayyuid:").ok_or_else(|| {
                ProviderClientError::Parse("Huya presenter UID was not found".to_string())
            })?,
            top_sid: capture_i64(&page, "var TOPSID =").unwrap_or_default(),
            sub_sid: capture_i64(&page, "var SUBSID =").unwrap_or_default(),
        })
    }

    async fn resolve_live(
        &self,
        resource: &HuyaResource,
        session: Option<&HuyaSession>,
    ) -> Result<HuyaMedia, ProviderClientError> {
        let url = format!(
            "{}/{}",
            self.endpoints.room_base.trim_end_matches('/'),
            resource.id
        );
        let mut request = self
            .http
            .get(url)
            .header(ORIGIN, HUYA_ORIGIN)
            .header(REFERER, format!("{HUYA_ORIGIN}/"));
        if let Some(cookie) = session.and_then(|session| session.cookie.as_deref()) {
            request = request.header(COOKIE, cookie);
        }
        let page = text_with_limit(check_response(request.send().await?).await?).await?;
        let stream_json = extract_json_object(&page, "stream:").ok_or_else(|| {
            ProviderClientError::Parse("Huya stream data was not found".to_string())
        })?;
        let response: WebStreamResponse = serde_json::from_str(stream_json)?;
        let container =
            response
                .data
                .into_iter()
                .next()
                .ok_or_else(|| ProviderClientError::Api {
                    code: 404,
                    message: "Huya room was not found".to_string(),
                })?;
        let metadata = live_metadata(
            resource,
            &container.game_live_info,
            !container.streams.is_empty(),
        );
        let qualities = live_qualities(
            container.streams,
            &response.multi_streams,
            container.game_live_info.uid,
        )?;
        Ok(HuyaMedia {
            metadata,
            playback: HuyaPlayback {
                resource: resource.clone(),
                qualities,
            },
        })
    }

    async fn resolve_video(
        &self,
        resource: &HuyaResource,
        session: Option<&HuyaSession>,
    ) -> Result<HuyaMedia, ProviderClientError> {
        let mut request = self
            .http
            .get(&self.endpoints.moment)
            .query(&[("videoId", resource.id.as_str())])
            .header(ORIGIN, HUYA_ORIGIN)
            .header(REFERER, format!("{HUYA_ORIGIN}/"));
        if let Some(cookie) = session.and_then(|session| session.cookie.as_deref()) {
            request = request.header(COOKIE, cookie);
        }
        let envelope: MomentEnvelope = fetch_json(request).await?;
        let status = envelope.status.unwrap_or_default();
        let moment =
            envelope
                .data
                .and_then(|data| data.moment)
                .ok_or_else(|| ProviderClientError::Api {
                    code: i64::from(status),
                    message: envelope
                        .message
                        .unwrap_or_else(|| "Huya video was not found".to_string()),
                })?;
        let metadata = video_metadata(resource, &moment);
        let qualities = video_qualities(&moment.video_info.definitions);
        if qualities.is_empty() {
            return Err(ProviderClientError::Api {
                code: 404,
                message: "Huya video has no playable definitions".to_string(),
            });
        }
        Ok(HuyaMedia {
            metadata,
            playback: HuyaPlayback {
                resource: resource.clone(),
                qualities,
            },
        })
    }
}

fn parse_huya_id(id: &str, kind: HuyaResourceKind) -> Result<HuyaResource, ProviderClientError> {
    let valid = match kind {
        HuyaResourceKind::Live => id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_')),
        HuyaResourceKind::Video => id.chars().all(|value| value.is_ascii_digit()),
    };
    if id.is_empty() || !valid {
        return Err(ProviderClientError::InvalidConfig(
            "invalid Huya resource ID".to_string(),
        ));
    }
    Ok(HuyaResource {
        kind,
        id: id.to_string(),
    })
}

fn live_metadata(resource: &HuyaResource, info: &RawLiveInfo, is_live: bool) -> HuyaMetadata {
    HuyaMetadata {
        id: resource.id.clone(),
        title: first_nonempty([&info.room_name, &info.introduction])
            .unwrap_or("Huya live")
            .to_string(),
        author: info.nick.clone(),
        author_id: (info.uid != 0).then(|| info.uid.to_string()),
        category: nonempty(&info.game_full_name),
        thumbnail_url: nonempty(&info.screenshot),
        avatar_url: None,
        is_live,
        description: nonempty(&info.content_intro),
        duration_seconds: None,
        view_count: Some(info.total_count),
        comment_count: None,
        like_count: None,
        published_at: None,
        presenter_uid: (info.uid != 0).then_some(info.uid),
    }
}

fn video_metadata(resource: &HuyaResource, moment: &RawMoment) -> HuyaMetadata {
    let info = &moment.video_info;
    HuyaMetadata {
        id: resource.id.clone(),
        title: info.video_title.clone(),
        author: info.nick_name.clone(),
        author_id: value_string(&info.uid),
        category: category(&info.category),
        thumbnail_url: first_nonempty([&info.video_big_cover, &info.video_cover])
            .map(str::to_string),
        avatar_url: None,
        is_live: false,
        description: nonempty(&moment.content),
        duration_seconds: duration_seconds(&info.video_duration),
        view_count: info.video_play_num,
        comment_count: moment.comment_count,
        like_count: moment.favor_count,
        published_at: moment.c_time,
        presenter_uid: None,
    }
}

fn live_qualities(
    streams: Vec<RawStream>,
    bitrates: &[RawBitrate],
    fallback_presenter_uid: i64,
) -> Result<Vec<HuyaQuality>, ProviderClientError> {
    let bitrates = if bitrates.is_empty() {
        vec![RawBitrate {
            s_display_name: "Original".to_string(),
            i_bit_rate: 0,
        }]
    } else {
        bitrates
            .iter()
            .map(|value| RawBitrate {
                s_display_name: value.s_display_name.clone(),
                i_bit_rate: value.i_bit_rate,
            })
            .collect()
    };
    let mut output = Vec::new();
    for stream in streams {
        let presenter_uid = if stream.l_presenter_uid != 0 {
            stream.l_presenter_uid
        } else {
            fallback_presenter_uid
        };
        for bitrate in &bitrates {
            if !stream.s_flv_url.is_empty() && !stream.s_flv_url_suffix.is_empty() {
                let query = sign_anti_code(
                    &stream.s_stream_name,
                    &stream.s_flv_anti_code,
                    u64::try_from(presenter_uid).ok(),
                    bitrate.i_bit_rate,
                )?;
                output.push(quality(
                    &stream,
                    bitrate,
                    HuyaStreamFormat::Flv,
                    format!(
                        "{}/{}.{}?{}",
                        stream.s_flv_url, stream.s_stream_name, stream.s_flv_url_suffix, query
                    ),
                ));
            }
            if !stream.s_hls_url.is_empty() && !stream.s_hls_url_suffix.is_empty() {
                let query = sign_anti_code(
                    &stream.s_stream_name,
                    &stream.s_hls_anti_code,
                    u64::try_from(presenter_uid).ok(),
                    bitrate.i_bit_rate,
                )?;
                output.push(quality(
                    &stream,
                    bitrate,
                    HuyaStreamFormat::Hls,
                    format!(
                        "{}/{}.{}?{}",
                        stream.s_hls_url, stream.s_stream_name, stream.s_hls_url_suffix, query
                    ),
                ));
            }
        }
    }
    Ok(output)
}

fn quality(
    stream: &RawStream,
    bitrate: &RawBitrate,
    format: HuyaStreamFormat,
    url: String,
) -> HuyaQuality {
    let (width, height) = resolution(&bitrate.s_display_name);
    HuyaQuality {
        name: if bitrate.s_display_name.is_empty() {
            "Original".to_string()
        } else {
            bitrate.s_display_name.clone()
        },
        cdn: stream.s_cdn_type.clone(),
        format,
        url,
        bitrate: (bitrate.i_bit_rate != 0).then_some(bitrate.i_bit_rate),
        width,
        height,
    }
}

fn video_qualities(definitions: &[RawDefinition]) -> Vec<HuyaQuality> {
    definitions
        .iter()
        .filter_map(|definition| {
            let url = definition
                .m3u8
                .as_deref()
                .filter(|value| !value.is_empty())?;
            Some(HuyaQuality {
                name: definition
                    .def_name
                    .clone()
                    .unwrap_or_else(|| "HLS".to_string()),
                cdn: String::new(),
                format: HuyaStreamFormat::Hls,
                url: url.to_string(),
                bitrate: definition.definition,
                width: definition.width,
                height: definition.height,
            })
        })
        .collect()
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
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return source.get(object_start..=object_start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn capture_i64(source: &str, marker: &str) -> Option<i64> {
    let tail = source
        .get(source.find(marker)? + marker.len()..)?
        .trim_start();
    let tail = tail.strip_prefix(['\'', '"']).unwrap_or(tail);
    let digits = tail
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '-')
        .collect::<String>();
    digits.parse().ok()
}

fn resolution(name: &str) -> (Option<u32>, Option<u32>) {
    if name.contains("蓝光") || name.eq_ignore_ascii_case("original") {
        (Some(1920), Some(1080))
    } else if name.contains("超清") {
        (Some(1280), Some(720))
    } else if name.contains("流畅") {
        (Some(800), Some(480))
    } else {
        (None, None)
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn first_nonempty<const N: usize>(values: [&str; N]) -> Option<&str> {
    values.into_iter().find(|value| !value.trim().is_empty())
}

fn value_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) if !value.is_empty() => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn duration_seconds(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(value) => value
            .as_u64()
            .or_else(|| value.as_f64().and_then(nonnegative_seconds)),
        serde_json::Value::String(value) => {
            let parts = value.split(':').collect::<Vec<_>>();
            parts.iter().try_fold(0_u64, |total, part| {
                part.parse::<u64>()
                    .ok()
                    .and_then(|part| total.checked_mul(60)?.checked_add(part))
            })
        }
        _ => None,
    }
}

fn nonnegative_seconds(value: f64) -> Option<u64> {
    std::time::Duration::try_from_secs_f64(value)
        .ok()
        .map(|duration| duration.as_secs())
}

fn category(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => nonempty(value),
        serde_json::Value::Array(values) => values.iter().find_map(value_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn parses_live_and_video_urls() {
        assert_eq!(
            HuyaClient::parse_resource("https://www.huya.com/xiaoyugame")
                .expect("live URL should parse"),
            HuyaResource {
                kind: HuyaResourceKind::Live,
                id: "xiaoyugame".to_string(),
            }
        );
        assert_eq!(
            HuyaClient::parse_resource("https://www.huya.com/video/play/1002412640.html")
                .expect("video URL should parse")
                .kind,
            HuyaResourceKind::Video
        );
    }

    #[tokio::test]
    async fn resolves_live_metadata_flv_hls_cdns_and_qualities() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let stream = json!({
            "data": [{
                "gameLiveInfo": {
                    "uid": "12345678", "roomName": "Live room", "introduction": "Intro",
                    "nick": "Streamer", "screenshot": "https://img.test/live.jpg",
                    "contentIntro": "Description", "gameFullName": "Game", "totalCount": 42
                },
                "gameStreamInfoList": [{
                    "sStreamName": "12345678-stream", "sFlvUrl": "https://flv.test/live",
                    "sFlvUrlSuffix": "flv", "sFlvAntiCode": "wsTime=65aa0000&fm=YWJjX3Q%3D&ctype=huya_live",
                    "sHlsUrl": "https://hls.test/live", "sHlsUrlSuffix": "m3u8",
                    "sHlsAntiCode": "wsTime=65aa0000&fm=YWJjX3Q%3D&ctype=huya_live",
                    "sCdnType": "AL", "lPresenterUid": "12345678"
                }]
            }],
            "vMultiStreamInfo": [
                {"sDisplayName": "蓝光", "iBitRate": 0},
                {"sDisplayName": "超清", "iBitRate": 4000}
            ]
        });
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/660000"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "<script>var hyPlayerConfig = {{ stream: {stream} }};</script>"
            )))
            .mount(&server)
            .await;
        let client =
            HuyaClient::with_http_client(reqwest::Client::new()).with_endpoints(HuyaEndpoints {
                room_base: server.uri(),
                mobile_base: server.uri(),
                moment: format!("{}/moment", server.uri()),
            });
        let media = client
            .resolve(
                &HuyaResource {
                    kind: HuyaResourceKind::Live,
                    id: "660000".to_string(),
                },
                None,
            )
            .await
            .expect("live media should resolve");
        assert!(media.metadata.is_live);
        assert_eq!(media.metadata.author, "Streamer");
        assert_eq!(media.playback.qualities.len(), 4);
        assert!(media.playback.qualities.iter().any(|quality| {
            quality.format == HuyaStreamFormat::Hls
                && quality.bitrate == Some(4000)
                && quality.url.contains("ratio=4000")
        }));
    }

    #[tokio::test]
    async fn resolves_video_metadata_and_definitions() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/moment"))
            .and(matchers::query_param("videoId", "1002412640"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": 200,
                "data": {"moment": {
                    "content": "Description", "commentCount": 3, "favorCount": 4,
                    "cTime": 1_722_675_433,
                    "videoInfo": {
                        "videoTitle": "Replay", "category": ["Game"], "videoDuration": "01:02",
                        "videoBigCover": "https://img.test/video.jpg", "nickName": "Streamer",
                        "uid": "1564376151", "videoPlayNum": 42,
                        "definitions": [{
                            "m3u8": "https://video.test/replay.m3u8", "defName": "1080p",
                            "width": 1920, "height": 1080, "definition": 10000
                        }]
                    }
                }}
            })))
            .mount(&server)
            .await;
        let client =
            HuyaClient::with_http_client(reqwest::Client::new()).with_endpoints(HuyaEndpoints {
                room_base: server.uri(),
                mobile_base: server.uri(),
                moment: format!("{}/moment", server.uri()),
            });
        let media = client
            .resolve(
                &HuyaResource {
                    kind: HuyaResourceKind::Video,
                    id: "1002412640".to_string(),
                },
                None,
            )
            .await
            .expect("video should resolve");
        assert_eq!(media.metadata.duration_seconds, Some(62));
        assert_eq!(media.metadata.comment_count, Some(3));
        assert_eq!(media.metadata.like_count, Some(4));
        assert_eq!(media.playback.qualities[0].height, Some(1080));
    }

    #[tokio::test]
    async fn resolves_chat_identity_from_mobile_room_page() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/660000"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "var TOPSID = '123'; var SUBSID = '456'; window.room = { ayyuid: '789' };",
            ))
            .mount(&server)
            .await;
        let client =
            HuyaClient::with_http_client(reqwest::Client::new()).with_endpoints(HuyaEndpoints {
                room_base: server.uri(),
                mobile_base: server.uri(),
                moment: format!("{}/moment", server.uri()),
            });
        let identity = client
            .chat_identity("660000", None)
            .await
            .expect("chat identity should resolve");
        assert_eq!(identity.presenter_uid, 789);
        assert_eq!(identity.top_sid, 123);
        assert_eq!(identity.sub_sid, 456);
    }
}
