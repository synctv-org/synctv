use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use rand::RngExt;
use reqwest::{header, Client, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use super::media_types::{
    FnosItemGuidRequest, FnosMediaCommandRequest, FnosMediaLibrary, FnosMediaList,
    FnosMediaListRequest, FnosMediaLogin, FnosPlayInfo, FnosPlayInfoRequest, FnosPlayRecordRequest,
    FnosPlayRequest, FnosPlayResponse, FnosStream, FnosStreamRequest, MediaResponse,
};
use crate::{fetch_json, ProviderClientError, PROVIDER_USER_AGENT};

const API_KEY: &str = "NDzZTVxnRKP8Z0jXg1VAMonaG8akvh";
const API_SECRET: &str = "16CCEB3D-AB42-077D-36A1-F355324E4237";

#[derive(Clone)]
pub struct FnosMediaClient {
    origin: String,
    client: Client,
}

impl FnosMediaClient {
    pub fn new(endpoint: &str) -> Result<Self, ProviderClientError> {
        let client =
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())?;
        Self::with_http_client(endpoint, client)
    }

    pub fn with_http_client(endpoint: &str, client: Client) -> Result<Self, ProviderClientError> {
        Ok(Self {
            origin: media_origin(endpoint)?,
            client,
        })
    }

    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    fn request<T: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<&T>,
    ) -> Result<RequestBuilder, ProviderClientError> {
        let authx = authx(path, body)?;
        let mut request = self
            .client
            .request(method, self.endpoint(path))
            .header("Authx", authx)
            .header(header::USER_AGENT, PROVIDER_USER_AGENT);
        if let Some(token) = token.filter(|value| !value.is_empty()) {
            request = request
                .bearer_auth(token)
                .header(header::COOKIE, format!("Trim-MC-token={token}"));
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        Ok(request)
    }

    async fn unwrap<T: DeserializeOwned>(
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, ProviderClientError> {
        let response: MediaResponse<T> = fetch_json(request).await?;
        if response.code != 0 {
            return Err(ProviderClientError::Api {
                code: response.code,
                message: if response.msg.is_empty() {
                    format!("FNOS media {operation} failed")
                } else {
                    response.msg
                },
            });
        }
        response.data.ok_or_else(|| {
            ProviderClientError::Parse(format!("FNOS media {operation} response has no data"))
        })
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<FnosMediaLogin, ProviderClientError> {
        let body = serde_json::json!({
            "username": username,
            "password": password,
            "app_name": "trimemedia-web",
        });
        Self::unwrap(
            self.request(Method::POST, "/v/api/v1/login", None, Some(&body))?,
            "login",
        )
        .await
    }

    pub async fn libraries(
        &self,
        token: &str,
    ) -> Result<Vec<FnosMediaLibrary>, ProviderClientError> {
        Self::unwrap(
            self.request::<serde_json::Value>(
                Method::GET,
                "/v/api/v1/mediadb/list",
                Some(token),
                None,
            )?,
            "library list",
        )
        .await
    }

    pub async fn items(
        &self,
        token: &str,
        request: &FnosMediaListRequest,
    ) -> Result<FnosMediaList, ProviderClientError> {
        Self::unwrap(
            self.request(
                Method::POST,
                "/v/api/v1/item/list",
                Some(token),
                Some(request),
            )?,
            "item list",
        )
        .await
    }

    pub async fn favorites(
        &self,
        token: &str,
        request: &FnosMediaListRequest,
    ) -> Result<FnosMediaList, ProviderClientError> {
        Self::unwrap(
            self.request(
                Method::POST,
                "/v/api/v1/favorite/list",
                Some(token),
                Some(request),
            )?,
            "favorite list",
        )
        .await
    }

    pub async fn history(
        &self,
        token: &str,
    ) -> Result<Vec<super::media_types::FnosMediaItem>, ProviderClientError> {
        Self::unwrap(
            self.request::<serde_json::Value>(
                Method::GET,
                "/v/api/v1/play/list",
                Some(token),
                None,
            )?,
            "play history",
        )
        .await
    }

    pub async fn search(
        &self,
        token: &str,
        query: &str,
    ) -> Result<Vec<super::media_types::FnosMediaItem>, ProviderClientError> {
        let path = "/v/api/v1/search/list";
        let canonical_query = format!("q={query}");
        let request = self
            .client
            .request(Method::GET, self.endpoint(path))
            .header("Authx", authx_data(path, canonical_query.as_bytes()))
            .header(header::USER_AGENT, PROVIDER_USER_AGENT)
            .bearer_auth(token)
            .header(header::COOKIE, format!("Trim-MC-token={token}"))
            .query(&[("q", query)]);
        Self::unwrap(request, "media search").await
    }

    pub async fn set_favorite(
        &self,
        token: &str,
        item_guid: &str,
        favorite: bool,
    ) -> Result<bool, ProviderClientError> {
        let body = FnosItemGuidRequest { guid: item_guid };
        let method = if favorite {
            Method::PUT
        } else {
            Method::DELETE
        };
        Self::unwrap(
            self.request(method, "/v/api/v1/item/favorite", Some(token), Some(&body))?,
            "favorite update",
        )
        .await
    }

    pub async fn set_watched(
        &self,
        token: &str,
        item_guid: &str,
        watched: bool,
    ) -> Result<bool, ProviderClientError> {
        let body = FnosItemGuidRequest { guid: item_guid };
        let method = if watched {
            Method::POST
        } else {
            Method::DELETE
        };
        Self::unwrap(
            self.request(method, "/v/api/v1/item/watched", Some(token), Some(&body))?,
            "watched update",
        )
        .await
    }

    pub async fn play_info(
        &self,
        token: &str,
        item_guid: &str,
        media_guid: Option<&str>,
    ) -> Result<FnosPlayInfo, ProviderClientError> {
        let body = FnosPlayInfoRequest {
            item_guid: item_guid.to_string(),
            media_guid: media_guid.map(str::to_string),
        };
        Self::unwrap(
            self.request(
                Method::POST,
                "/v/api/v1/play/info",
                Some(token),
                Some(&body),
            )?,
            "play info",
        )
        .await
    }

    pub async fn stream(
        &self,
        token: &str,
        media_guid: &str,
        identity: &str,
    ) -> Result<FnosStream, ProviderClientError> {
        let body = FnosStreamRequest {
            header: std::collections::HashMap::from([(
                "User-Agent".to_string(),
                vec![PROVIDER_USER_AGENT.to_string()],
            )]),
            level: 1,
            media_guid: media_guid.to_string(),
            ip: md5_hex(identity.as_bytes()),
            nonce: rand::rng().random_range(100_000_u32..1_000_000).to_string(),
        };
        Self::unwrap(
            self.request(Method::POST, "/v/api/v1/stream", Some(token), Some(&body))?,
            "stream",
        )
        .await
    }

    pub async fn play(
        &self,
        token: &str,
        request: &FnosPlayRequest,
    ) -> Result<FnosPlayResponse, ProviderClientError> {
        Self::unwrap(
            self.request(
                Method::POST,
                "/v/api/v1/play/play",
                Some(token),
                Some(request),
            )?,
            "transcode playback",
        )
        .await
    }

    pub async fn record_playback(
        &self,
        token: &str,
        request: &FnosPlayRecordRequest,
    ) -> Result<bool, ProviderClientError> {
        Self::unwrap(
            self.request(
                Method::POST,
                "/v/api/v1/play/record",
                Some(token),
                Some(request),
            )?,
            "playback record",
        )
        .await
    }

    pub async fn media_command(
        &self,
        token: &str,
        request: &FnosMediaCommandRequest,
    ) -> Result<serde_json::Value, ProviderClientError> {
        Self::unwrap(
            self.request(
                Method::POST,
                "/v/api/v1/media/p",
                Some(token),
                Some(request),
            )?,
            "media command",
        )
        .await
    }

    pub fn resolve_media_url(&self, path_or_url: &str) -> Result<String, ProviderClientError> {
        if let Ok(url) = Url::parse(path_or_url) {
            return match url.scheme() {
                "http" | "https" => Ok(url.to_string()),
                _ => Err(ProviderClientError::InvalidConfig(
                    "FNOS playback URL must use HTTP(S)".to_string(),
                )),
            };
        }
        Url::parse(&format!("{}/", self.origin))
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?
            .join(path_or_url.trim_start_matches('/'))
            .map(|url| url.to_string())
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))
    }

    #[must_use]
    pub fn media_url(&self, media_guid: &str, quality_index: Option<usize>) -> String {
        quality_index.map_or_else(
            || self.endpoint(&format!("/v/api/v1/media/range/{media_guid}")),
            |index| {
                self.endpoint(&format!(
                    "/v/api/v1/media/range/{media_guid}?direct_link_quality_index={index}"
                ))
            },
        )
    }

    #[must_use]
    pub fn subtitle_url(&self, subtitle_guid: &str) -> String {
        self.endpoint(&format!("/v/api/v1/subtitle/dl/{subtitle_guid}"))
    }

    #[must_use]
    pub fn image_url(&self, path: &str, width: u32) -> String {
        self.endpoint(&format!("/v/api/v1/sys/img{path}?w={width}"))
    }

    #[must_use]
    pub fn auth_headers(token: &str) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::from([
            ("Authorization".to_string(), format!("Bearer {token}")),
            ("Cookie".to_string(), format!("Trim-MC-token={token}")),
        ])
    }
}

fn media_origin(endpoint: &str) -> Result<String, ProviderClientError> {
    let mut url = Url::parse(endpoint)
        .or_else(|_| Url::parse(&format!("https://{endpoint}")))
        .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
    let scheme = match url.scheme() {
        "ws" => "http".to_string(),
        "wss" => "https".to_string(),
        "http" | "https" => url.scheme().to_string(),
        _ => {
            return Err(ProviderClientError::InvalidConfig(
                "FNOS media endpoint must use HTTP(S) or WS(S)".to_string(),
            ));
        }
    };
    url.set_scheme(&scheme)
        .map_err(|()| ProviderClientError::InvalidConfig("invalid FNOS media scheme".into()))?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn authx<T: Serialize + ?Sized>(
    path: &str,
    body: Option<&T>,
) -> Result<String, ProviderClientError> {
    let data = body
        .map(serde_json::to_vec)
        .transpose()?
        .unwrap_or_default();
    Ok(authx_data(path, &data))
}

fn authx_data(path: &str, data: &[u8]) -> String {
    let nonce = rand::rng().random_range(100_000_u32..1_000_000).to_string();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();
    let data_md5 = md5_hex(data);
    let signature =
        md5_hex(format!("{API_KEY}_{path}_{nonce}_{timestamp}_{data_md5}_{API_SECRET}").as_bytes());
    format!("nonce={nonce}&timestamp={timestamp}&sign={signature}")
}

fn md5_hex(value: &[u8]) -> String {
    hex::encode(Md5::digest(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn derives_media_origin_from_websocket_endpoint() {
        assert_eq!(
            media_origin("wss://nas.example:5667/websocket?type=main")
                .expect("test operation should succeed"),
            "https://nas.example:5667"
        );
    }

    #[test]
    fn authx_contains_nonce_timestamp_and_md5_signature() {
        let value = authx("/v/api/v1/login", Some(&serde_json::json!({"a": 1})))
            .expect("test operation should succeed");
        assert!(value.starts_with("nonce="));
        assert!(value.contains("&timestamp="));
        assert_eq!(
            value
                .rsplit("sign=")
                .next()
                .expect("test operation should succeed")
                .len(),
            32
        );
    }

    #[test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    fn resolves_relative_media_links_against_origin() {
        crate::install_process_crypto_provider();
        let client = FnosMediaClient::with_http_client("https://nas.example:5667", Client::new())
            .expect("test operation should succeed");
        assert_eq!(
            client
                .resolve_media_url("/v/api/v1/play/preset/session/index.m3u8")
                .expect("test operation should succeed"),
            "https://nas.example:5667/v/api/v1/play/preset/session/index.m3u8"
        );
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn starts_transcode_with_authx_and_media_token() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let request = FnosPlayRequest {
            media_guid: "media".to_string(),
            video_guid: "video".to_string(),
            video_encoder: "hevc".to_string(),
            resolution: "1920x1080".to_string(),
            bitrate: 8_000_000,
            start_timestamp: 0,
            audio_encoder: "aac".to_string(),
            audio_guid: "audio".to_string(),
            subtitle_guid: String::new(),
            channels: 2,
            forced_sdr: 1,
        };
        Mock::given(method("POST"))
            .and(path("/v/api/v1/play/play"))
            .and(header("authorization", "Bearer media-token"))
            .and(header("cookie", "Trim-MC-token=media-token"))
            .and(header_exists("authx"))
            .and(body_json(&request))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "play_link": "/v/api/v1/play/preset/session/index.m3u8"
                }
            })))
            .mount(&server)
            .await;

        let client = FnosMediaClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let response = client
            .play("media-token", &request)
            .await
            .expect("test operation should succeed");

        assert_eq!(
            response.play_link,
            "/v/api/v1/play/preset/session/index.m3u8"
        );
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn searches_the_native_media_index_with_signed_query() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v/api/v1/search/list"))
            .and(query_param("q", "星际穿越"))
            .and(header("authorization", "Bearer media-token"))
            .and(header("cookie", "Trim-MC-token=media-token"))
            .and(header_exists("authx"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": [{
                    "guid": "item",
                    "title": "星际穿越",
                    "type": "Movie",
                    "poster": "/poster.jpg",
                    "duration": 101,
                    "watched": 1,
                    "is_favorite": 1
                }]
            })))
            .mount(&server)
            .await;

        let client = FnosMediaClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let items = client
            .search("media-token", "星际穿越")
            .await
            .expect("test operation should succeed");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].guid, "item");
        assert_eq!(items[0].is_favorite, 1);
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn updates_native_favorite_and_watched_state() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        for (method_name, endpoint) in [
            ("PUT", "/v/api/v1/item/favorite"),
            ("DELETE", "/v/api/v1/item/watched"),
        ] {
            Mock::given(method(method_name))
                .and(path(endpoint))
                .and(header("authorization", "Bearer media-token"))
                .and(header_exists("authx"))
                .and(body_json(serde_json::json!({"guid": "item"})))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "code": 0,
                    "data": true
                })))
                .mount(&server)
                .await;
        }

        let client = FnosMediaClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        assert!(client
            .set_favorite("media-token", "item", true)
            .await
            .expect("test operation should succeed"));
        assert!(client
            .set_watched("media-token", "item", false)
            .await
            .expect("test operation should succeed"));
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn loads_native_favorites_and_play_history() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let request = FnosMediaListRequest {
            ancestor_guid: None,
            exclude_grouped_video: 1,
            sort_type: "DESC".to_string(),
            sort_column: "create_time".to_string(),
            page_size: 20,
            page: 1,
            tags: crate::fnos::media_types::FnosMediaTags {
                media_types: vec!["Movie".to_string()],
            },
        };
        let item = serde_json::json!({
            "guid": "item",
            "title": "Movie",
            "type": "Movie",
            "duration": 100,
            "is_favorite": 1
        });
        Mock::given(method("POST"))
            .and(path("/v/api/v1/favorite/list"))
            .and(body_json(&request))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {"total": 1, "list": [item.clone()]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v/api/v1/play/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": [item]
            })))
            .mount(&server)
            .await;

        let client = FnosMediaClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        assert_eq!(
            client
                .favorites("media-token", &request)
                .await
                .expect("test operation should succeed")
                .total,
            1
        );
        assert_eq!(
            client
                .history("media-token")
                .await
                .expect("test operation should succeed")
                .len(),
            1
        );
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn records_playback_with_native_stream_identifiers() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let request = FnosPlayRecordRequest {
            item_guid: "item".to_string(),
            media_guid: "media".to_string(),
            video_guid: "video".to_string(),
            audio_guid: "audio".to_string(),
            subtitle_guid: Some("subtitle".to_string()),
            resolution: "1920x1080".to_string(),
            bitrate: 8_000_000,
            ts: 42,
            duration: 120,
            play_link: Some("/v/api/v1/play/preset/session/index.m3u8".to_string()),
        };
        Mock::given(method("POST"))
            .and(path("/v/api/v1/play/record"))
            .and(header("authorization", "Bearer media-token"))
            .and(header("cookie", "Trim-MC-token=media-token"))
            .and(header_exists("authx"))
            .and(body_json(&request))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": true
            })))
            .mount(&server)
            .await;

        let client = FnosMediaClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        assert!(client
            .record_playback("media-token", &request)
            .await
            .expect("test operation should succeed"));
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn sends_media_quit_with_camel_case_play_link() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let request = FnosMediaCommandRequest {
            req: "media.quit".to_string(),
            reqid: "session".to_string(),
            play_link: "/v/api/v1/play/preset/session/index.m3u8".to_string(),
        };
        Mock::given(method("POST"))
            .and(path("/v/api/v1/media/p"))
            .and(header("authorization", "Bearer media-token"))
            .and(header_exists("authx"))
            .and(body_json(&request))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {"result": "success"}
            })))
            .mount(&server)
            .await;

        let client = FnosMediaClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let response = client
            .media_command("media-token", &request)
            .await
            .expect("test operation should succeed");
        assert_eq!(response["result"], "success");
    }
}
