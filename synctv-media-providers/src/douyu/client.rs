use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{COOKIE, ORIGIN, REFERER, USER_AGENT};
use url::Url;

use super::sign::sign;
use super::types::{
    BetardEnvelope, BetardRoom, EncryptionData, EncryptionEnvelope, PlayData, PlayEnvelope,
    RoomInfoData, RoomInfoEnvelope,
};
use super::{
    DouyuCodec, DouyuMedia, DouyuMetadata, DouyuPlayback, DouyuQuality, DouyuResource,
    DouyuSession, DouyuStreamFormat, DouyuVariant,
};
use crate::{
    check_response, fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT,
};

const DOUYU_ORIGIN: &str = "https://www.douyu.com";
const DEFAULT_DEVICE_ID: &str = "10000000000000000000000000001501";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuEndpoints {
    pub web_base: String,
    pub mobile_base: String,
    pub room_api_base: String,
    pub encryption: String,
    pub play_base: String,
}

impl Default for DouyuEndpoints {
    fn default() -> Self {
        Self {
            web_base: DOUYU_ORIGIN.to_string(),
            mobile_base: "https://m.douyu.com".to_string(),
            room_api_base: "https://open.douyucdn.cn/api/RoomApi/room".to_string(),
            encryption: format!("{DOUYU_ORIGIN}/wgapi/livenc/liveweb/websec/getEncryption"),
            play_base: format!("{DOUYU_ORIGIN}/lapi/live/getH5PlayV1"),
        }
    }
}

#[derive(Clone)]
pub struct DouyuClient {
    http: reqwest::Client,
    endpoints: DouyuEndpoints,
    encryption_cache: moka::future::Cache<String, EncryptionData>,
}

impl DouyuClient {
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
            endpoints: DouyuEndpoints::default(),
            encryption_cache: moka::future::Cache::builder()
                .max_capacity(16)
                .time_to_live(Duration::from_secs(300))
                .build(),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: DouyuEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(raw: &str) -> Result<DouyuResource, ProviderClientError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "Douyu room is required".to_string(),
            ));
        }
        if !raw.contains("://") {
            return validate_room_key(raw);
        }
        let url = Url::parse(raw).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid Douyu URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !matches!(host.as_str(), "douyu.com" | "www.douyu.com" | "m.douyu.com") {
            return Err(ProviderClientError::InvalidConfig(
                "URL is not a Douyu room".to_string(),
            ));
        }
        if let Some(room) = url
            .query_pairs()
            .find(|(key, _)| key == "rid")
            .map(|(_, value)| value.into_owned())
        {
            return validate_room_key(&room);
        }
        let room = url
            .path_segments()
            .and_then(|mut segments| segments.find(|value| !value.is_empty()))
            .ok_or_else(|| {
                ProviderClientError::InvalidConfig("Douyu room is missing".to_string())
            })?;
        validate_room_key(room)
    }

    pub async fn resolve(
        &self,
        resource: &DouyuResource,
        session: Option<&DouyuSession>,
    ) -> Result<DouyuMedia, ProviderClientError> {
        let room_id = self.resolve_room_id(&resource.room, session).await?;
        let metadata = self.metadata_by_id(&room_id, session).await?;
        let qualities = if should_request_playback(&metadata) {
            let data = self
                .play_data(&room_id, "", 0, DouyuCodec::Hevc, session, false)
                .await?;
            qualities(&data)
        } else {
            Vec::new()
        };
        Ok(DouyuMedia {
            metadata,
            playback: DouyuPlayback { room_id, qualities },
        })
    }

    pub async fn metadata(
        &self,
        resource: &DouyuResource,
        session: Option<&DouyuSession>,
    ) -> Result<DouyuMetadata, ProviderClientError> {
        let room_id = self.resolve_room_id(&resource.room, session).await?;
        self.metadata_by_id(&room_id, session).await
    }

    pub async fn playback(
        &self,
        resource: &DouyuResource,
        session: Option<&DouyuSession>,
    ) -> Result<DouyuPlayback, ProviderClientError> {
        let media = self.resolve(resource, session).await?;
        Ok(media.playback)
    }

    pub async fn variant(
        &self,
        room_id: &str,
        cdn: &str,
        rate: i64,
        codec: DouyuCodec,
        session: Option<&DouyuSession>,
    ) -> Result<DouyuVariant, ProviderClientError> {
        validate_numeric_room_id(room_id)?;
        let audio_only = codec == DouyuCodec::Aac;
        let data = self
            .play_data(room_id, cdn, rate, codec, session, audio_only)
            .await?;
        let url = format!(
            "{}/{}",
            data.rtmp_url.trim_end_matches('/'),
            data.rtmp_live.trim_start_matches('/')
        );
        if data.rtmp_url.is_empty() || data.rtmp_live.is_empty() {
            return Err(ProviderClientError::Api {
                code: 404,
                message: "Douyu stream URL is unavailable".to_string(),
            });
        }
        Ok(DouyuVariant {
            format: stream_format(&data.rtmp_live),
            codec: if audio_only {
                DouyuCodec::Aac
            } else {
                data.cdns
                    .iter()
                    .find(|candidate| candidate.cdn == data.rtmp_cdn)
                    .map_or(codec, |candidate| {
                        if candidate.is_h265 {
                            DouyuCodec::Hevc
                        } else {
                            DouyuCodec::Avc
                        }
                    })
            },
            url,
        })
    }

    async fn resolve_room_id(
        &self,
        room: &str,
        session: Option<&DouyuSession>,
    ) -> Result<String, ProviderClientError> {
        let resource = validate_room_key(room)?;
        if resource.room.chars().all(|value| value.is_ascii_digit()) {
            return Ok(resource.room);
        }
        let url = format!(
            "{}/{}",
            self.endpoints.mobile_base.trim_end_matches('/'),
            resource.room
        );
        let page = text_with_limit(
            check_response(Self::request(self.http.get(url), session).send().await?).await?,
        )
        .await?;
        extract_room_id(&page).ok_or_else(|| ProviderClientError::Api {
            code: 404,
            message: "Douyu room alias could not be resolved".to_string(),
        })
    }

    async fn metadata_by_id(
        &self,
        room_id: &str,
        session: Option<&DouyuSession>,
    ) -> Result<DouyuMetadata, ProviderClientError> {
        validate_numeric_room_id(room_id)?;
        let betard_url = format!(
            "{}/betard/{room_id}",
            self.endpoints.web_base.trim_end_matches('/')
        );
        let room_api_url = format!(
            "{}/{room_id}",
            self.endpoints.room_api_base.trim_end_matches('/')
        );
        let (betard, room_api) = tokio::join!(
            fetch_json::<BetardEnvelope>(Self::request(self.http.get(betard_url), session)),
            fetch_json::<RoomInfoEnvelope>(Self::request(self.http.get(room_api_url), session)),
        );
        let betard = betard.ok().and_then(|value| value.room);
        let room_api = room_api
            .ok()
            .filter(|value| value.error == 0)
            .and_then(|value| value.data);
        if betard.is_none() && room_api.is_none() {
            return Err(ProviderClientError::Api {
                code: 404,
                message: "Douyu room was not found".to_string(),
            });
        }
        Ok(metadata_from(room_id, betard.as_ref(), room_api.as_ref()))
    }

    async fn play_data(
        &self,
        room_id: &str,
        cdn: &str,
        rate: i64,
        codec: DouyuCodec,
        session: Option<&DouyuSession>,
        audio_only: bool,
    ) -> Result<PlayData, ProviderClientError> {
        for attempt in 0..2 {
            if attempt == 1 {
                self.encryption_cache.invalidate(DEFAULT_DEVICE_ID).await;
            }
            let encryption = self.encryption(session).await?;
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| ProviderClientError::Network(error.to_string()))?
                .as_secs();
            let signed = sign(&encryption, room_id, DEFAULT_DEVICE_ID, timestamp);
            let url = format!(
                "{}/{room_id}",
                self.endpoints.play_base.trim_end_matches('/')
            );
            let form = [
                ("enc_data", signed.enc_data),
                ("tt", signed.timestamp.to_string()),
                ("did", signed.device_id),
                ("auth", signed.auth),
                ("cdn", cdn.to_string()),
                ("rate", rate.to_string()),
                ("ver", "Douyu_new".to_string()),
                ("iar", "0".to_string()),
                ("ive", "0".to_string()),
                ("rid", room_id.to_string()),
                ("hevc", i32::from(codec == DouyuCodec::Hevc).to_string()),
                ("fa", i32::from(audio_only).to_string()),
                ("sov", "0".to_string()),
            ];
            let envelope: PlayEnvelope =
                match fetch_json(Self::request(self.http.post(url).form(&form), session)).await {
                    Ok(envelope) => envelope,
                    Err(error) if should_retry_transport_auth(&error, attempt) => continue,
                    Err(error) => return Err(error),
                };
            if envelope.error == 0 {
                let data: PlayData = serde_json::from_value(envelope.data)?;
                if data.room_id.to_string() != room_id {
                    return Err(ProviderClientError::Parse(
                        "Douyu playback response room ID does not match the request".to_string(),
                    ));
                }
                return Ok(data);
            }
            if is_auth_failure(envelope.error, &envelope.msg) && attempt == 0 {
                continue;
            }
            return Err(play_error(envelope.error, &envelope.msg));
        }
        Err(ProviderClientError::Api {
            code: 401,
            message: "Douyu stream authorization failed".to_string(),
        })
    }

    async fn encryption(
        &self,
        session: Option<&DouyuSession>,
    ) -> Result<EncryptionData, ProviderClientError> {
        if let Some(value) = self.encryption_cache.get(DEFAULT_DEVICE_ID).await {
            return Ok(value);
        }
        let envelope: EncryptionEnvelope = fetch_json(Self::request(
            self.http
                .get(&self.endpoints.encryption)
                .query(&[("did", DEFAULT_DEVICE_ID)]),
            session,
        ))
        .await?;
        if envelope.error != 0 {
            return Err(ProviderClientError::Api {
                code: envelope.error,
                message: envelope.msg,
            });
        }
        let data = envelope.data.ok_or_else(|| {
            ProviderClientError::Parse("Douyu encryption response has no data".to_string())
        })?;
        self.encryption_cache
            .insert(DEFAULT_DEVICE_ID.to_string(), data.clone())
            .await;
        Ok(data)
    }

    fn request(
        mut request: reqwest::RequestBuilder,
        session: Option<&DouyuSession>,
    ) -> reqwest::RequestBuilder {
        request = request
            .header(ORIGIN, DOUYU_ORIGIN)
            .header(REFERER, format!("{DOUYU_ORIGIN}/"))
            .header(USER_AGENT, PROVIDER_USER_AGENT);
        if let Some(cookie) = session.and_then(|session| session.cookie.as_deref()) {
            request = request.header(COOKIE, cookie);
        }
        request
    }
}

fn validate_room_key(room: &str) -> Result<DouyuResource, ProviderClientError> {
    let room = room.trim();
    let valid = room
        .chars()
        .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'));
    if room.is_empty() || !valid {
        return Err(ProviderClientError::InvalidConfig(
            "invalid Douyu room".to_string(),
        ));
    }
    Ok(DouyuResource {
        room: room.to_string(),
    })
}

fn validate_numeric_room_id(room_id: &str) -> Result<(), ProviderClientError> {
    if room_id.is_empty() || !room_id.chars().all(|value| value.is_ascii_digit()) {
        return Err(ProviderClientError::InvalidConfig(
            "invalid Douyu room ID".to_string(),
        ));
    }
    Ok(())
}

fn extract_room_id(page: &str) -> Option<String> {
    ["roomInfo\":{\"rid\":", "roomID:", "roomID :"]
        .into_iter()
        .find_map(|marker| {
            let rest = page.get(page.find(marker)? + marker.len()..)?.trim_start();
            let value: String = rest
                .chars()
                .skip_while(|value| *value == '\"')
                .take_while(char::is_ascii_digit)
                .collect();
            (!value.is_empty()).then_some(value)
        })
}

fn metadata_from(
    room_id: &str,
    betard: Option<&BetardRoom>,
    room_api: Option<&RoomInfoData>,
) -> DouyuMetadata {
    let title = first_nonempty([
        betard.map(|value| value.room_name.as_str()),
        room_api.map(|value| value.room_name.as_str()),
    ])
    .unwrap_or("Douyu live")
    .to_string();
    let author = first_nonempty([
        betard.map(|value| value.owner_name.as_str()),
        room_api.map(|value| value.owner_name.as_str()),
    ])
    .unwrap_or_default()
    .to_string();
    let is_replay = betard.is_some_and(|value| value.video_loop != 0);
    let is_live = betard.map_or_else(
        || room_api.is_some_and(|value| value.room_status == "1"),
        |value| value.show_status == 1 && !is_replay,
    );
    DouyuMetadata {
        room_id: betard
            .map(|value| value.room_id.clone())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                room_api
                    .map(|value| value.room_id.clone())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| room_id.to_string()),
        title,
        author,
        category: room_api
            .and_then(|value| nonempty(&value.cate_name))
            .map(str::to_string),
        thumbnail_url: first_nonempty([
            betard.map(|value| value.room_thumb.as_str()),
            room_api.map(|value| value.room_thumb.as_str()),
        ])
        .map(str::to_string),
        avatar_url: betard
            .and_then(|value| {
                first_nonempty([
                    Some(value.avatar.big.as_str()),
                    Some(value.avatar.middle.as_str()),
                    Some(value.avatar.small.as_str()),
                ])
            })
            .or_else(|| {
                room_api
                    .map(|value| value.avatar.as_str())
                    .filter(|value| !value.is_empty())
            })
            .map(str::to_string),
        is_live,
        is_replay,
        is_vip: betard.is_some_and(|value| value.is_vip == 1),
        viewer_count: room_api.and_then(|value| (value.online != 0).then_some(value.online)),
        started_at: room_api
            .and_then(|value| nonempty(&value.start_time))
            .map(str::to_string),
    }
}

const fn should_request_playback(metadata: &DouyuMetadata) -> bool {
    metadata.is_live || metadata.is_replay
}

fn qualities(data: &PlayData) -> Vec<DouyuQuality> {
    let format = stream_format(&data.rtmp_live);
    let cdns = if data.cdns.is_empty() {
        vec![super::types::RawCdn {
            name: data.rtmp_cdn.clone(),
            cdn: data.rtmp_cdn.clone(),
            is_h265: false,
        }]
    } else {
        data.cdns.clone()
    };
    let rates = if data.multirates.is_empty() {
        vec![super::types::RawRate {
            name: "Original".to_string(),
            rate: 0,
            bit: 0,
        }]
    } else {
        data.multirates.clone()
    };
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for cdn in &cdns {
        if cdn.cdn.starts_with("scdn") || cdn.cdn.is_empty() {
            continue;
        }
        let codec = if cdn.is_h265 {
            DouyuCodec::Hevc
        } else {
            DouyuCodec::Avc
        };
        for rate in &rates {
            if seen.insert((cdn.cdn.clone(), rate.rate, codec)) {
                output.push(DouyuQuality {
                    name: nonempty(&rate.name).unwrap_or("Original").to_string(),
                    cdn: cdn.cdn.clone(),
                    cdn_name: nonempty(&cdn.name).unwrap_or(&cdn.cdn).to_string(),
                    rate: rate.rate,
                    bitrate: (rate.bit != 0).then_some(rate.bit),
                    codec,
                    format,
                });
            }
        }
        if seen.insert((cdn.cdn.clone(), 0, DouyuCodec::Aac)) {
            output.push(DouyuQuality {
                name: "Audio only".to_string(),
                cdn: cdn.cdn.clone(),
                cdn_name: nonempty(&cdn.name).unwrap_or(&cdn.cdn).to_string(),
                rate: 0,
                bitrate: None,
                codec: DouyuCodec::Aac,
                format,
            });
        }
    }
    output
}

fn stream_format(path: &str) -> DouyuStreamFormat {
    if path.to_ascii_lowercase().contains("m3u8") {
        DouyuStreamFormat::Hls
    } else {
        DouyuStreamFormat::Flv
    }
}

fn play_error(code: i64, message: &str) -> ProviderClientError {
    let message = match code {
        -5..=-3 => "Douyu room is offline".to_string(),
        -9 => "Douyu rejected the local timestamp".to_string(),
        126 => "Douyu stream is unavailable in this region".to_string(),
        _ if message.is_empty() => "Douyu playback request failed".to_string(),
        _ => message.to_string(),
    };
    ProviderClientError::Api { code, message }
}

fn is_auth_failure(code: i64, message: &str) -> bool {
    code == 401 || message.contains("鉴权失败")
}

fn should_retry_transport_auth(error: &ProviderClientError, attempt: usize) -> bool {
    attempt == 0
        && matches!(
            error,
            ProviderClientError::Http {
                status: reqwest::StatusCode::FORBIDDEN,
                ..
            }
        )
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values.into_iter().flatten().find(|value| !value.is_empty())
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_numeric_alias_and_url_rooms() {
        assert_eq!(
            DouyuClient::parse_resource("123")
                .expect("test operation should succeed")
                .room,
            "123"
        );
        assert_eq!(
            DouyuClient::parse_resource("https://www.douyu.com/some_room")
                .expect("test operation should succeed")
                .room,
            "some_room"
        );
        assert_eq!(
            DouyuClient::parse_resource("https://m.douyu.com/0?rid=456")
                .expect("test operation should succeed")
                .room,
            "456"
        );
        assert!(DouyuClient::parse_resource("https://example.com/123").is_err());
    }

    #[test]
    fn builds_cdn_rate_codec_and_audio_quality_matrix() {
        let data = PlayData {
            room_id: 123,
            rtmp_cdn: "ws-h5".to_string(),
            rtmp_url: "https://example.com/live".to_string(),
            rtmp_live: "stream.flv".to_string(),
            cdns: vec![
                super::super::types::RawCdn {
                    name: "Web".to_string(),
                    cdn: "ws-h5".to_string(),
                    is_h265: false,
                },
                super::super::types::RawCdn {
                    name: "HEVC".to_string(),
                    cdn: "tct-h5".to_string(),
                    is_h265: true,
                },
                super::super::types::RawCdn {
                    name: "P2P".to_string(),
                    cdn: "scdn-1".to_string(),
                    is_h265: false,
                },
            ],
            multirates: vec![
                super::super::types::RawRate {
                    name: "Original".to_string(),
                    rate: 0,
                    bit: 8_000,
                },
                super::super::types::RawRate {
                    name: "HD".to_string(),
                    rate: 2,
                    bit: 2_000,
                },
            ],
        };
        let qualities = qualities(&data);
        assert_eq!(qualities.len(), 6);
        assert!(qualities
            .iter()
            .any(|value| value.codec == DouyuCodec::Hevc));
        assert!(qualities.iter().any(|value| value.codec == DouyuCodec::Aac));
        assert!(qualities.iter().all(|value| !value.cdn.starts_with("scdn")));
    }

    #[test]
    fn retries_first_forbidden_play_request_with_a_fresh_key() {
        let error = ProviderClientError::Http {
            status: reqwest::StatusCode::FORBIDDEN,
            url: "https://www.douyu.com/play".to_string(),
            retry_after_secs: None,
            body: "鉴权失败".to_string(),
        };
        assert!(should_retry_transport_auth(&error, 0));
        assert!(!should_retry_transport_auth(&error, 1));
    }

    #[test]
    fn live_and_replay_rooms_request_playback_qualities() {
        let metadata = |is_live, is_replay| DouyuMetadata {
            room_id: "123".to_string(),
            title: "Room".to_string(),
            author: "Anchor".to_string(),
            category: None,
            thumbnail_url: None,
            avatar_url: None,
            is_live,
            is_replay,
            is_vip: false,
            viewer_count: None,
            started_at: None,
        };

        assert!(should_request_playback(&metadata(true, false)));
        assert!(should_request_playback(&metadata(false, true)));
        assert!(!should_request_playback(&metadata(false, false)));
    }

    #[tokio::test]
    async fn resolves_metadata_qualities_and_selected_variant() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/betard/123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "room": {
                    "room_id": 123,
                    "room_name": "Live title",
                    "owner_name": "Anchor",
                    "show_status": 1,
                    "videoLoop": 0,
                    "isVip": 1,
                    "room_thumb": "https://example.com/cover.jpg",
                    "avatar": {"big": "https://example.com/avatar.jpg"}
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/room/123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": 0,
                "data": {
                    "room_id": "123",
                    "cate_name": "Games",
                    "room_status": "1",
                    "start_time": "2026-01-02 03:04:05",
                    "online": "9876"
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/encryption"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": 0,
                "data": {
                    "rand_str": "seed",
                    "enc_time": 1,
                    "key": "key",
                    "is_special": 0,
                    "enc_data": "opaque"
                }
            })))
            .mount(&server)
            .await;
        let play_body = serde_json::json!({
            "error": 0,
            "data": {
                "room_id": 123,
                "rtmp_cdn": "tct-h5",
                "rtmp_url": "https://edge.example/live",
                "rtmp_live": "stream.flv?token=abc",
                "cdnsWithName": [
                    {"name": "Web", "cdn": "ws-h5", "isH265": 0},
                    {"name": "HEVC", "cdn": "tct-h5", "isH265": 1}
                ],
                "multirates": [
                    {"name": "Original", "rate": 0, "bit": 8000},
                    {"name": "HD", "rate": 2, "bit": 2000}
                ]
            }
        });
        Mock::given(method("POST"))
            .and(path("/play/123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(play_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/selected/123"))
            .and(body_string_contains("cdn=tct-h5"))
            .and(body_string_contains("rate=2"))
            .and(body_string_contains("hevc=1"))
            .and(body_string_contains("fa=0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(play_body))
            .mount(&server)
            .await;

        let client =
            DouyuClient::with_http_client(reqwest::Client::new()).with_endpoints(DouyuEndpoints {
                web_base: server.uri(),
                mobile_base: server.uri(),
                room_api_base: format!("{}/room", server.uri()),
                encryption: format!("{}/encryption", server.uri()),
                play_base: format!("{}/play", server.uri()),
            });
        let media = client
            .resolve(
                &DouyuResource {
                    room: "123".to_string(),
                },
                None,
            )
            .await
            .expect("Douyu media should resolve");
        assert_eq!(media.metadata.title, "Live title");
        assert_eq!(media.metadata.category.as_deref(), Some("Games"));
        assert_eq!(media.metadata.viewer_count, Some(9876));
        assert!(media.metadata.is_live);
        assert!(media.metadata.is_vip);
        assert_eq!(media.playback.qualities.len(), 6);

        let selected = client.clone().with_endpoints(DouyuEndpoints {
            play_base: format!("{}/selected", server.uri()),
            ..client.endpoints.clone()
        });
        let variant = selected
            .variant("123", "tct-h5", 2, DouyuCodec::Hevc, None)
            .await
            .expect("selected variant should resolve");
        assert_eq!(variant.codec, DouyuCodec::Hevc);
        assert_eq!(variant.format, DouyuStreamFormat::Flv);
        assert_eq!(
            variant.url,
            "https://edge.example/live/stream.flv?token=abc"
        );
    }
}
