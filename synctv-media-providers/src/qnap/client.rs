use std::collections::HashMap;

use base64::Engine;
use reqwest::{Client, Method, RequestBuilder};
use serde::de::DeserializeOwned;
use url::Url;

use super::types::{
    QnapHardwareTranscode, QnapList, QnapLogin, QnapShare, QnapStatus, QnapTranscodeResolution,
};
use crate::{fetch_json, ProviderClientError, PROVIDER_USER_AGENT};

#[derive(Clone)]
pub struct QnapClient {
    origin: String,
    client: Client,
}

impl QnapClient {
    pub fn new(endpoint: &str) -> Result<Self, ProviderClientError> {
        let client =
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())?;
        Self::with_http_client(endpoint, client)
    }

    pub fn with_http_client(endpoint: &str, client: Client) -> Result<Self, ProviderClientError> {
        let mut url = Url::parse(endpoint)
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(ProviderClientError::InvalidConfig(
                "QNAP endpoint must use HTTP(S)".to_string(),
            ));
        }
        url.set_query(None);
        url.set_fragment(None);
        Ok(Self {
            origin: url.as_str().trim_end_matches('/').to_string(),
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

    fn request(&self, path: &str, query: &[(impl AsRef<str>, impl AsRef<str>)]) -> RequestBuilder {
        self.client
            .request(Method::GET, self.endpoint(path))
            .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT)
            .query(
                &query
                    .iter()
                    .map(|(key, value)| (key.as_ref(), value.as_ref()))
                    .collect::<Vec<_>>(),
            )
    }

    async fn json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, ProviderClientError> {
        fetch_json(request).await
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<QnapLogin, ProviderClientError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(password.as_bytes());
        let response: QnapLogin = Self::json(self.request(
            "/cgi-bin/filemanager/wfm2Login.cgi",
            &[("user", username), ("pwd", encoded.as_str())],
        ))
        .await?;
        if response.status != 1 || response.sid.trim().is_empty() {
            return Err(ProviderClientError::Auth(
                "QNAP File Station login failed".to_string(),
            ));
        }
        Ok(response)
    }

    pub async fn logout(&self, sid: &str) -> Result<(), ProviderClientError> {
        let response: QnapStatus =
            Self::json(self.request("/cgi-bin/filemanager/wfm2Logout.cgi", &[("sid", sid)]))
                .await?;
        if response.status == 1 {
            Ok(())
        } else {
            Err(ProviderClientError::Api {
                code: response.status,
                message: "QNAP File Station logout failed".to_string(),
            })
        }
    }

    pub async fn shares(&self, sid: &str) -> Result<Vec<QnapShare>, ProviderClientError> {
        Self::json(self.request(
            "/cgi-bin/filemanager/utilRequest.cgi",
            &[("func", "get_tree"), ("node", "share_root"), ("sid", sid)],
        ))
        .await
    }

    pub async fn list(
        &self,
        sid: &str,
        path: &str,
        offset: u64,
        limit: u32,
        search: Option<&str>,
    ) -> Result<QnapList, ProviderClientError> {
        let offset = offset.to_string();
        let limit = limit.to_string();
        let mut query = vec![
            ("func", "get_list"),
            ("sid", sid),
            ("is_iso", "0"),
            ("list_mode", "all"),
            ("path", path),
            ("dir", "ASC"),
            ("sort", "filename"),
            ("start", offset.as_str()),
            ("limit", limit.as_str()),
            ("hidden_file", "0"),
        ];
        if let Some(search) = search.filter(|value| !value.trim().is_empty()) {
            query.push(("filename", search));
        }
        let response: QnapList =
            Self::json(self.request("/cgi-bin/filemanager/utilRequest.cgi", &query)).await?;
        if response.status.is_some_and(|status| status != 1) {
            return Err(ProviderClientError::Api {
                code: response.status.unwrap_or_default(),
                message: "QNAP File Station list failed".to_string(),
            });
        }
        Ok(response)
    }

    #[must_use]
    pub fn download_url(&self, sid: &str, path: &str) -> Result<String, ProviderClientError> {
        let (parent, file_name) = split_file_path(path)?;
        self.url(
            "/cgi-bin/filemanager/utilRequest.cgi",
            &[
                ("func", "download"),
                ("sid", sid),
                ("isfolder", "0"),
                ("compress", "0"),
                ("source_path", parent),
                ("source_file", file_name),
                ("source_total", "1"),
            ],
        )
    }

    #[must_use]
    pub fn thumbnail_url(
        &self,
        sid: &str,
        path: &str,
        size: u32,
    ) -> Result<String, ProviderClientError> {
        let (parent, file_name) = split_file_path(path)?;
        let size = match size {
            0..=160 => "80",
            161..=480 => "320",
            _ => "640",
        };
        self.url(
            "/cgi-bin/filemanager/utilRequest.cgi",
            &[
                ("func", "get_thumb"),
                ("sid", sid),
                ("path", parent),
                ("name", file_name),
                ("size", size),
            ],
        )
    }

    #[must_use]
    pub fn viewer_url(
        &self,
        sid: &str,
        path: &str,
        resolution: Option<QnapTranscodeResolution>,
        realtime: bool,
        start_seconds: Option<u64>,
    ) -> Result<String, ProviderClientError> {
        let (parent, file_name) = split_file_path(path)?;
        let mut query = vec![
            ("func", "get_viewer".to_string()),
            ("sid", sid.to_string()),
            ("source_path", parent.to_string()),
            ("source_file", file_name.to_string()),
        ];
        if let Some(resolution) = resolution {
            if realtime {
                query.push(("rtt", "1".to_string()));
                query.push(("s", resolution.label().to_string()));
                query.push(("vq", "2".to_string()));
                if let Some(start_seconds) = start_seconds {
                    query.push(("ss", start_seconds.to_string()));
                }
            } else {
                query.push(("format", resolution.viewer_format().to_string()));
            }
        }
        let query = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        self.url("/cgi-bin/filemanager/utilRequest.cgi", &query)
    }

    pub async fn hardware_transcode(
        &self,
        sid: &str,
    ) -> Result<QnapHardwareTranscode, ProviderClientError> {
        Self::json(self.request(
            "/cgi-bin/filemanager/utilRequest.cgi",
            &[("func", "hwts"), ("sid", sid)],
        ))
        .await
    }

    fn url(
        &self,
        path: &str,
        query: &[(impl AsRef<str>, impl AsRef<str>)],
    ) -> Result<String, ProviderClientError> {
        let mut url = Url::parse(&self.endpoint(path))
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key.as_ref(), value.as_ref());
            }
        }
        Ok(url.to_string())
    }

    #[must_use]
    pub fn auth_headers() -> HashMap<String, String> {
        HashMap::new()
    }
}

fn split_file_path(path: &str) -> Result<(&str, &str), ProviderClientError> {
    let normalized = path.trim();
    if normalized.is_empty() || normalized.split('/').any(|segment| segment == "..") {
        return Err(ProviderClientError::InvalidConfig(
            "QNAP file path is invalid".to_string(),
        ));
    }
    let (parent, file_name) = normalized.rsplit_once('/').unwrap_or(("/", normalized));
    if file_name.is_empty() {
        return Err(ProviderClientError::InvalidConfig(
            "QNAP file name is required".to_string(),
        ));
    }
    Ok((if parent.is_empty() { "/" } else { parent }, file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn logs_in_and_lists_with_native_offset_pagination() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cgi-bin/filemanager/wfm2Login.cgi"))
            .and(query_param("user", "alice"))
            .and(query_param("pwd", "c2VjcmV0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 1,
                "sid": "session-id",
                "servername": "QNAP",
                "supportRTT": 1,
                "version": "5.2"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cgi-bin/filemanager/utilRequest.cgi"))
            .and(query_param("func", "get_list"))
            .and(query_param("sid", "session-id"))
            .and(query_param("path", "/Multimedia"))
            .and(query_param("start", "50"))
            .and(query_param("limit", "25"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 76,
                "rtt_support": 1,
                "datas": [{
                    "filename": "Movie.mkv",
                    "isfolder": 0,
                    "filesize": "123456",
                    "epochmt": "1700000000",
                    "filetype": 2,
                    "mp4_720": 1,
                    "mp4_1080": "1"
                }]
            })))
            .mount(&server)
            .await;

        let client = QnapClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let login = client
            .login("alice", "secret")
            .await
            .expect("test operation should succeed");
        assert_eq!(login.sid, "session-id");
        assert_eq!(login.support_rtt, 1);
        let listing = client
            .list(&login.sid, "/Multimedia", 50, 25, None)
            .await
            .expect("test operation should succeed");
        assert_eq!(listing.total, 76);
        assert_eq!(listing.datas[0].filesize, 123_456);
        assert_eq!(
            listing.datas[0].available_mp4_resolutions(),
            vec![
                QnapTranscodeResolution::P720,
                QnapTranscodeResolution::P1080
            ]
        );
    }

    #[test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    fn builds_signed_session_media_urls_with_encoded_paths() {
        crate::install_process_crypto_provider();
        let client = QnapClient::with_http_client("https://nas.example/qts/", Client::new())
            .expect("test operation should succeed");
        let download = client
            .download_url("sid", "/Multimedia/Films/A Movie.mkv")
            .expect("test operation should succeed");
        assert!(
            download.starts_with("https://nas.example/qts/cgi-bin/filemanager/utilRequest.cgi?")
        );
        assert!(download.contains("source_file=A+Movie.mkv"));
        let viewer = client
            .viewer_url(
                "sid",
                "/Multimedia/Films/A Movie.mkv",
                Some(QnapTranscodeResolution::P720),
                true,
                Some(42),
            )
            .expect("test operation should succeed");
        assert!(viewer.contains("rtt=1"));
        assert!(viewer.contains("s=720p"));
        assert!(viewer.contains("ss=42"));
    }
}
