use reqwest::header::REFERER;
use url::Url;

use super::types::VideoInfoResponse;
use super::{
    CctvChapter, CctvMedia, CctvMetadata, CctvPlayback, CctvResource, CctvStream, CctvStreamKind,
};
use crate::{fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CctvEndpoints {
    pub video_info: String,
}

impl Default for CctvEndpoints {
    fn default() -> Self {
        Self {
            video_info: "http://vdn.apps.cntv.cn/api/getHttpVideoInfo.do".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct CctvClient {
    http: reqwest::Client,
    endpoints: CctvEndpoints,
}

impl CctvClient {
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
            endpoints: CctvEndpoints::default(),
        }
    }

    #[must_use]
    pub fn with_endpoints(mut self, endpoints: CctvEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn parse_resource(raw: &str) -> Result<CctvResource, ProviderClientError> {
        let value = raw.trim();
        if is_video_id(value) {
            return Ok(CctvResource {
                page_url: None,
                video_id: Some(value.to_ascii_lowercase()),
            });
        }
        let url = Url::parse(value).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("invalid CCTV URL: {error}"))
        })?;
        if !is_cctv_host(url.host_str().unwrap_or_default()) {
            return Err(ProviderClientError::InvalidConfig(
                "URL is not a CCTV resource".to_string(),
            ));
        }
        Ok(CctvResource {
            page_url: Some(url.to_string()),
            video_id: direct_video_id(&url),
        })
    }

    pub async fn resolve(&self, resource: &CctvResource) -> Result<CctvMedia, ProviderClientError> {
        let (video_id, description) = match (&resource.video_id, &resource.page_url) {
            (Some(video_id), None) => (video_id.clone(), None),
            (_, Some(page_url)) => {
                let page =
                    text_with_limit(self.http.get(page_url).send().await?.error_for_status()?)
                        .await?;
                let video_id = resource
                    .video_id
                    .clone()
                    .or_else(|| extract_video_id(&page))
                    .ok_or_else(|| {
                        ProviderClientError::Parse("CCTV video center ID was not found".to_string())
                    })?;
                (video_id, extract_description(&page))
            }
            _ => {
                return Err(ProviderClientError::InvalidConfig(
                    "CCTV resource is incomplete".to_string(),
                ));
            }
        };
        let page_url = resource.page_url.as_deref().unwrap_or_default();
        let response: VideoInfoResponse = fetch_json(
            self.http
                .get(&self.endpoints.video_info)
                .header(REFERER, page_url)
                .query(&[
                    ("pid", video_id.as_str()),
                    ("url", page_url),
                    ("idl", "32"),
                    ("idlr", "32"),
                    ("modifyed", "false"),
                ]),
        )
        .await?;
        if response.ack.as_deref() != Some("yes") || response.status.as_deref() != Some("001") {
            return Err(ProviderClientError::Api {
                code: response
                    .status
                    .as_deref()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(-1),
                message: "CCTV video info request failed".to_string(),
            });
        }
        let protected = response.is_protected.as_deref() == Some("1")
            || response.is_invalid_copyright.as_deref() == Some("1");
        let streams = streams(&response, protected);
        if streams.is_empty() {
            return Err(ProviderClientError::Api {
                code: 404,
                message: "CCTV returned no playable streams".to_string(),
            });
        }
        let duration_seconds = response
            .video
            .as_ref()
            .and_then(|video| video.total_length.as_deref())
            .and_then(|value| value.parse().ok());
        let metadata = CctvMetadata {
            video_id: video_id.clone(),
            title: response.title,
            description,
            uploader: nonempty(response.editer_name),
            producer: nonempty(response.produce),
            channel: nonempty(response.play_channel),
            column: nonempty(response.column),
            tags: response
                .tag
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            thumbnail_url: response.image.and_then(normalize_http_url),
            duration_seconds,
            published_at: response.f_pgmtime.as_deref().and_then(parse_timestamp),
            chapters: response
                .segments
                .into_iter()
                .filter_map(|chapter| {
                    Some(CctvChapter {
                        id: chapter.guid,
                        title: chapter.title,
                        start_ms: chapter.start?,
                        end_ms: chapter.end?,
                    })
                })
                .collect(),
            protected,
        };
        Ok(CctvMedia {
            metadata,
            playback: CctvPlayback { video_id, streams },
        })
    }

    pub async fn metadata(
        &self,
        resource: &CctvResource,
    ) -> Result<CctvMetadata, ProviderClientError> {
        Ok(self.resolve(resource).await?.metadata)
    }
}

fn streams(response: &VideoInfoResponse, protected: bool) -> Vec<CctvStream> {
    let mut streams = Vec::new();
    if !protected {
        if let Some(url) = response.hls_url.as_deref().and_then(clean_hls_url) {
            streams.push(CctvStream {
                name: "HLS".to_string(),
                url,
                kind: CctvStreamKind::VideoHls,
            });
        }
        if let Some(manifest) = &response.manifest {
            if let Some(url) = manifest
                .hls_audio_url
                .as_deref()
                .or(manifest.audio_mp3.as_deref())
                .and_then(normalize_http_url)
            {
                streams.push(CctvStream {
                    name: "Audio".to_string(),
                    url,
                    kind: CctvStreamKind::AudioHls,
                });
            }
        }
    }
    if let Some(video) = &response.video {
        for (name, files) in [("Low", &video.low_chapters), ("HTTP", &video.chapters)] {
            if let Some(url) = files
                .iter()
                .find_map(|file| normalize_http_url(file.url.clone()))
            {
                streams.push(CctvStream {
                    name: name.to_string(),
                    url,
                    kind: CctvStreamKind::Http,
                });
            }
        }
    }
    streams
}

fn extract_video_id(page: &str) -> Option<String> {
    const MARKERS: [&str; 6] = [
        "var guid",
        "videoCenterId",
        "changePlayer",
        "loadVideo",
        "loadvideo",
        "var initMyAray",
    ];
    for marker in MARKERS {
        let mut rest = page;
        while let Some(index) = rest.find(marker) {
            rest = &rest[index + marker.len()..];
            if let Some(id) = first_quoted_video_id(rest) {
                return Some(id);
            }
        }
    }
    if let Some(index) = page.find("var ids") {
        return first_quoted_video_id(&page[index + "var ids".len()..]);
    }
    None
}

fn first_quoted_video_id(value: &str) -> Option<String> {
    for (index, character) in value.char_indices() {
        if !matches!(character, '\'' | '"') {
            continue;
        }
        let rest = &value[index + character.len_utf8()..];
        let end = rest.find(character)?;
        let candidate = &rest[..end];
        if is_video_id(candidate) {
            return Some(candidate.to_ascii_lowercase());
        }
    }
    None
}

fn extract_description(page: &str) -> Option<String> {
    for marker in ["name=\"description\"", "name='description'"] {
        let Some(index) = page.find(marker) else {
            continue;
        };
        let tail = &page[index..page.len().min(index + 2048)];
        for content in ["content=\"", "content='"] {
            if let Some(start) = tail.find(content) {
                let value = &tail[start + content.len()..];
                let quote = content.chars().last()?;
                return value.find(quote).and_then(|end| {
                    let text = html_escape::decode_html_entities(&value[..end])
                        .trim()
                        .to_string();
                    (!text.is_empty()).then_some(text)
                });
            }
        }
    }
    None
}

fn direct_video_id(url: &Url) -> Option<String> {
    url.path_segments()?
        .rfind(|part| is_video_id(part))
        .map(str::to_ascii_lowercase)
}

fn is_video_id(value: &str) -> bool {
    value.len() == 32 && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn is_cctv_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "ncpa-classic.com"
        || host == "www.ncpa-classic.com"
        || ["cctv.com", "cctv.cn", "cntv.com", "cntv.cn"]
            .into_iter()
            .any(|suffix| host == suffix || host.ends_with(&format!(".{suffix}")))
}

fn clean_hls_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    let pairs = url
        .query_pairs()
        .filter(|(key, _)| key != "maxbr")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    normalize_http_url(url.to_string())
}

fn normalize_http_url(value: impl Into<String>) -> Option<String> {
    let value = value.into();
    if value.is_empty() {
        return None;
    }
    let url = Url::parse(&value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(value)
}

fn parse_timestamp(value: &str) -> Option<i64> {
    use chrono::TimeZone;

    let local = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok()?;
    chrono::FixedOffset::east_opt(8 * 60 * 60)?
        .from_local_datetime(&local)
        .single()
        .map(|value| value.timestamp())
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_supported_pages_and_direct_ids() {
        assert!(
            CctvClient::parse_resource("https://news.cctv.com/2024/02/21/ARTIfoo.shtml").is_ok()
        );
        let direct = CctvClient::parse_resource("5c846c0518444308ba32c4159df3b3e0")
            .expect("test operation should succeed");
        assert_eq!(
            direct.video_id.as_deref(),
            Some("5c846c0518444308ba32c4159df3b3e0")
        );
        assert!(CctvClient::parse_resource("https://example.com/video.shtml").is_err());
    }

    #[test]
    fn extracts_all_known_page_embed_forms() {
        for page in [
            r#"var guid = "5c846c0518444308ba32c4159df3b3e0";"#,
            r#"videoCenterId: "5c846c0518444308ba32c4159df3b3e0""#,
            r"changePlayer('5c846c0518444308ba32c4159df3b3e0')",
            r#"loadVideo("5c846c0518444308ba32c4159df3b3e0")"#,
            r#"var ids = ["5c846c0518444308ba32c4159df3b3e0"]"#,
        ] {
            assert_eq!(
                extract_video_id(page).as_deref(),
                Some("5c846c0518444308ba32c4159df3b3e0")
            );
        }
    }

    #[test]
    fn extracts_single_quoted_description_and_cleans_hls_query() {
        assert_eq!(
            extract_description(r"<meta name='description' content='News &amp; current affairs'>")
                .as_deref(),
            Some("News & current affairs")
        );
        assert_eq!(
            clean_hls_url("https://media.example/master.m3u8?maxbr=2048").as_deref(),
            Some("https://media.example/master.m3u8")
        );
        assert_eq!(
            clean_hls_url("https://media.example/master.m3u8?token=abc&maxbr=2048").as_deref(),
            Some("https://media.example/master.m3u8?token=abc")
        );
    }

    #[tokio::test]
    async fn resolves_page_metadata_chapters_and_all_stream_kinds() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let video_id = "5c846c0518444308ba32c4159df3b3e0";
        Mock::given(method("GET"))
            .and(path("/page.shtml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"<meta name='description' content='Episode &amp; notes'><script>videoCenterId: "{video_id}"</script>"#
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/video-info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ack": "yes",
                "status": "001",
                "title": "CCTV episode",
                "tag": "news documentary",
                "play_channel": "CCTV-1",
                "produce": "Producer",
                "editer_name": "Editor",
                "column": "Programme",
                "f_pgmtime": "2024-02-22 06:35:40",
                "image": "https://media.example/cover.jpg",
                "hls_url": "https://media.example/master.m3u8?token=abc&maxbr=2048",
                "manifest": {
                    "hls_audio_url": "https://media.example/audio.m3u8"
                },
                "video": {
                    "totalLength": "123.5",
                    "lowChapters": [{"url": "https://media.example/low.mp4"}],
                    "chapters": [{"url": "https://media.example/high.mp4"}]
                },
                "segments": [{
                    "guid": "chapter-1",
                    "title": "Opening",
                    "start": 0,
                    "end": 15000
                }],
                "is_invalid_copyright": "0",
                "is_protected": "0"
            })))
            .mount(&server)
            .await;

        let client =
            CctvClient::with_http_client(reqwest::Client::new()).with_endpoints(CctvEndpoints {
                video_info: format!("{}/video-info", server.uri()),
            });
        let media = client
            .resolve(&CctvResource {
                page_url: Some(format!("{}/page.shtml", server.uri())),
                video_id: None,
            })
            .await
            .expect("CCTV media should resolve");

        assert_eq!(media.metadata.video_id, video_id);
        assert_eq!(
            media.metadata.description.as_deref(),
            Some("Episode & notes")
        );
        assert_eq!(media.metadata.duration_seconds, Some(123.5));
        assert_eq!(media.metadata.published_at, Some(1_708_554_940));
        assert_eq!(media.metadata.chapters.len(), 1);
        assert_eq!(media.playback.streams.len(), 4);
        assert_eq!(
            media.playback.streams[0].url,
            "https://media.example/master.m3u8?token=abc"
        );
        assert_eq!(media.playback.streams[1].kind, CctvStreamKind::AudioHls);
        assert_eq!(media.playback.streams[2].kind, CctvStreamKind::Http);
    }

    #[tokio::test]
    async fn reports_unplayable_protected_media() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/video-info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ack": "yes",
                "status": "001",
                "title": "Protected",
                "hls_url": "https://media.example/master.m3u8",
                "is_invalid_copyright": "1"
            })))
            .mount(&server)
            .await;
        let client =
            CctvClient::with_http_client(reqwest::Client::new()).with_endpoints(CctvEndpoints {
                video_info: format!("{}/video-info", server.uri()),
            });
        let result = client
            .resolve(&CctvResource {
                page_url: None,
                video_id: Some("5c846c0518444308ba32c4159df3b3e0".to_string()),
            })
            .await;
        assert!(matches!(
            result,
            Err(ProviderClientError::Api { code: 404, .. })
        ));
    }
}
