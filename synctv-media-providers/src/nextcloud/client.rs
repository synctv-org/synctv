use base64::Engine;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use reqwest::{Client, Method, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use url::Url;

use super::dav::{favorites_report, parse_multistatus, propfind_body, search_report};
use super::types::{
    CapabilitiesData, NextcloudCapabilities, NextcloudList, NextcloudLoginFlow,
    NextcloudLoginFlowCredentials, NextcloudServerInfo, NextcloudUser, OcsEnvelope,
};
use crate::{
    check_response, fetch_json, text_with_limit, ProviderClientError, PROVIDER_USER_AGENT,
};

const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

#[derive(Clone)]
pub struct NextcloudClient {
    origin: String,
    client: Client,
}

impl NextcloudClient {
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
                "Nextcloud endpoint must use HTTP(S)".to_string(),
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
        format!("{}/{}", self.origin, path.trim_start_matches('/'))
    }

    fn authenticated(
        &self,
        method: Method,
        path: &str,
        username: &str,
        app_password: &str,
    ) -> RequestBuilder {
        self.client
            .request(method, self.endpoint(path))
            .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT)
            .basic_auth(username, Some(app_password))
    }

    async fn ocs<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, ProviderClientError> {
        let envelope: OcsEnvelope<T> = fetch_json(request.header("OCS-APIRequest", "true")).await?;
        if envelope.ocs.meta.statuscode == 100
            || envelope.ocs.meta.status.eq_ignore_ascii_case("ok")
        {
            return Ok(envelope.ocs.data);
        }
        Err(ProviderClientError::Api {
            code: envelope.ocs.meta.statuscode,
            message: envelope.ocs.meta.message,
        })
    }

    pub async fn user(
        &self,
        username: &str,
        app_password: &str,
    ) -> Result<NextcloudUser, ProviderClientError> {
        self.ocs(self.authenticated(
            Method::GET,
            "/ocs/v2.php/cloud/user?format=json",
            username,
            app_password,
        ))
        .await
    }

    pub async fn capabilities(
        &self,
        username: &str,
        app_password: &str,
    ) -> Result<NextcloudCapabilities, ProviderClientError> {
        let data: CapabilitiesData = self
            .ocs(self.authenticated(
                Method::GET,
                "/ocs/v1.php/cloud/capabilities?format=json",
                username,
                app_password,
            ))
            .await?;
        Ok(data.into())
    }

    pub async fn server_info(
        &self,
        username: &str,
        app_password: &str,
    ) -> Result<NextcloudServerInfo, ProviderClientError> {
        let (user, capabilities) = tokio::try_join!(
            self.user(username, app_password),
            self.capabilities(username, app_password)
        )?;
        Ok(NextcloudServerInfo { user, capabilities })
    }

    pub async fn start_login_flow(&self) -> Result<NextcloudLoginFlow, ProviderClientError> {
        fetch_json(
            self.client
                .post(self.endpoint("/index.php/login/v2"))
                .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT),
        )
        .await
    }

    pub async fn poll_login_flow(
        &self,
        endpoint: &str,
        token: &str,
    ) -> Result<NextcloudLoginFlowCredentials, ProviderClientError> {
        let endpoint = Url::parse(endpoint)
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let origin = Url::parse(&self.origin)
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        if endpoint.scheme() != origin.scheme()
            || endpoint.host_str() != origin.host_str()
            || endpoint.port_or_known_default() != origin.port_or_known_default()
        {
            return Err(ProviderClientError::InvalidConfig(
                "Nextcloud login poll endpoint must use the configured server origin".to_string(),
            ));
        }
        fetch_json(
            self.client
                .post(endpoint)
                .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT)
                .form(&[("token", token)]),
        )
        .await
    }

    pub async fn list(
        &self,
        username: &str,
        app_password: &str,
        path: &str,
        page: u64,
        page_size: u32,
    ) -> Result<NextcloudList, ProviderClientError> {
        let request_path = Self::dav_file_path(username, path);
        let response = self
            .authenticated(
                Method::from_bytes(b"PROPFIND").expect("valid method"),
                &request_path,
                username,
                app_password,
            )
            .header("Depth", "1")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(propfind_body())
            .send()
            .await?;
        let response = check_response(response).await?;
        let xml = text_with_limit(response).await?;
        let requested = normalize_path(path);
        let mut items = parse_multistatus(&xml, username)?;
        items.retain(|item| item.path != requested);
        items.sort_by_cached_key(|item| (!item.is_directory, item.name.to_lowercase()));
        Ok(paginate(items, page, page_size))
    }

    pub async fn search(
        &self,
        username: &str,
        app_password: &str,
        path: &str,
        query: &str,
        page: u64,
        page_size: u32,
    ) -> Result<NextcloudList, ProviderClientError> {
        if query.trim().chars().count() < 3 {
            return Err(ProviderClientError::InvalidConfig(
                "Nextcloud search query must contain at least 3 characters".to_string(),
            ));
        }
        let page = page.max(1);
        let page_size = page_size.clamp(1, 500);
        let offset = page.saturating_sub(1).saturating_mul(u64::from(page_size));
        let fetch_size = page_size.saturating_add(1);
        let response = self
            .authenticated(
                Method::from_bytes(b"SEARCH").expect("valid method"),
                "/remote.php/dav/",
                username,
                app_password,
            )
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(search_report(
                username,
                path,
                query.trim(),
                offset,
                fetch_size,
            ))
            .send()
            .await?;
        let response = check_response(response).await?;
        let mut items = parse_multistatus(&text_with_limit(response).await?, username)?;
        let has_more = items.len() > page_size as usize;
        items.truncate(page_size as usize);
        Ok(NextcloudList {
            items,
            total: None,
            page,
            page_size,
            has_more,
        })
    }

    pub async fn favorites(
        &self,
        username: &str,
        app_password: &str,
        page: u64,
        page_size: u32,
    ) -> Result<NextcloudList, ProviderClientError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 500);
        let offset = page.saturating_sub(1).saturating_mul(u64::from(page_size));
        let fetch_size = page_size.saturating_add(1);
        let response = self
            .authenticated(
                Method::from_bytes(b"REPORT").expect("valid method"),
                &format!("/remote.php/dav/files/{}", encode_segment(username)),
                username,
                app_password,
            )
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(favorites_report(offset, fetch_size))
            .send()
            .await?;
        let response = check_response(response).await?;
        let mut items = parse_multistatus(&text_with_limit(response).await?, username)?;
        let has_more = items.len() > page_size as usize;
        items.truncate(page_size as usize);
        Ok(NextcloudList {
            items,
            total: None,
            page,
            page_size,
            has_more,
        })
    }

    pub async fn metadata(
        &self,
        username: &str,
        app_password: &str,
        path: &str,
    ) -> Result<super::types::NextcloudDavItem, ProviderClientError> {
        let response = self
            .authenticated(
                Method::from_bytes(b"PROPFIND").expect("valid method"),
                &Self::dav_file_path(username, path),
                username,
                app_password,
            )
            .header("Depth", "0")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/xml; charset=utf-8",
            )
            .body(propfind_body())
            .send()
            .await?;
        let response = check_response(response).await?;
        let requested = normalize_path(path);
        parse_multistatus(&text_with_limit(response).await?, username)?
            .into_iter()
            .find(|item| item.path == requested)
            .ok_or_else(|| {
                ProviderClientError::Parse("Nextcloud DAV metadata is empty".to_string())
            })
    }

    pub async fn file(
        &self,
        username: &str,
        app_password: &str,
        path: &str,
    ) -> Result<Response, ProviderClientError> {
        let response = self
            .authenticated(
                Method::GET,
                &Self::dav_file_path(username, path),
                username,
                app_password,
            )
            .send()
            .await?;
        check_response(response).await
    }

    pub fn file_url(&self, username: &str, path: &str) -> String {
        self.endpoint(&Self::dav_file_path(username, path))
    }

    pub fn preview_url(
        &self,
        file_id: u64,
        width: u32,
        height: u32,
        crop: bool,
    ) -> Result<String, ProviderClientError> {
        let mut url = Url::parse(&self.endpoint("/core/preview"))
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("fileId", &file_id.to_string())
            .append_pair("x", &width.max(1).to_string())
            .append_pair("y", &height.max(1).to_string())
            .append_pair("a", if crop { "0" } else { "1" })
            .append_pair("forceIcon", "false")
            .append_pair("mimeFallback", "false");
        Ok(url.into())
    }

    #[must_use]
    pub fn auth_headers(
        username: &str,
        app_password: &str,
    ) -> std::collections::HashMap<String, String> {
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{app_password}"));
        std::collections::HashMap::from([("Authorization".to_string(), format!("Basic {token}"))])
    }

    pub async fn file_range(
        &self,
        username: &str,
        app_password: &str,
        path: &str,
        range: Option<&str>,
    ) -> Result<Response, ProviderClientError> {
        let mut request = self.authenticated(
            Method::GET,
            &Self::dav_file_path(username, path),
            username,
            app_password,
        );
        if let Some(range) = range {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = request.send().await?;
        check_response(response).await
    }

    pub async fn preview(
        &self,
        username: &str,
        app_password: &str,
        file_id: u64,
        width: u32,
        height: u32,
        crop: bool,
    ) -> Result<Response, ProviderClientError> {
        let response = self
            .authenticated(Method::GET, "/core/preview", username, app_password)
            .query(&[
                ("fileId", file_id.to_string()),
                ("x", width.max(1).to_string()),
                ("y", height.max(1).to_string()),
                ("a", if crop { "0" } else { "1" }.to_string()),
                ("forceIcon", "false".to_string()),
                ("mimeFallback", "false".to_string()),
            ])
            .send()
            .await?;
        check_response(response).await
    }

    fn dav_file_path(username: &str, path: &str) -> String {
        let mut value = format!("/remote.php/dav/files/{}", encode_segment(username));
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            value.push('/');
            value.push_str(&encode_segment(segment));
        }
        value
    }
}

fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}

fn normalize_path(path: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    }
}

fn paginate(
    mut items: Vec<super::types::NextcloudDavItem>,
    page: u64,
    page_size: u32,
) -> NextcloudList {
    let total = items.len() as u64;
    let page = page.max(1);
    let page_size = page_size.clamp(1, 500);
    let offset = page.saturating_sub(1).saturating_mul(u64::from(page_size));
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(items.len());
    let end = start.saturating_add(page_size as usize).min(items.len());
    let has_more = end < items.len();
    let items = items.drain(start..end).collect();
    NextcloudList {
        items,
        total: Some(total),
        page,
        page_size,
        has_more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{basic_auth, body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn lists_dav_files_with_client_side_page_and_large_ids() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let body = r#"<d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:response><d:href>/remote.php/dav/files/alice/Videos/</d:href><d:propstat><d:prop><d:displayname>Videos</d:displayname><d:resourcetype><d:collection/></d:resourcetype><oc:fileid>1</oc:fileid></d:prop></d:propstat></d:response><d:response><d:href>/remote.php/dav/files/alice/Videos/a.mp4</d:href><d:propstat><d:prop><d:displayname>a.mp4</d:displayname><d:getcontentlength>5000000000</d:getcontentlength><oc:fileid>6000000000</oc:fileid><nc:has-preview>true</nc:has-preview></d:prop></d:propstat></d:response></d:multistatus>"#;
        Mock::given(method("PROPFIND"))
            .and(path("/remote.php/dav/files/alice/Videos"))
            .and(header("Depth", "1"))
            .and(basic_auth("alice", "secret"))
            .respond_with(ResponseTemplate::new(207).set_body_string(body))
            .mount(&server)
            .await;
        let client = NextcloudClient::with_http_client(&server.uri(), reqwest::Client::new())
            .expect("test operation should succeed");
        let list = client
            .list("alice", "secret", "/Videos", 1, 100)
            .await
            .expect("test operation should succeed");
        assert_eq!(list.total, Some(1));
        assert!(!list.has_more);
        assert_eq!(list.items[0].file_id, 6_000_000_000);
        assert_eq!(list.items[0].size, 5_000_000_000);
    }

    #[tokio::test]
    async fn uses_native_offset_for_search_and_preview_parameters() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("SEARCH"))
            .and(path("/remote.php/dav/"))
            .and(body_string_contains("<nc:firstresult>200</nc:firstresult>"))
            .and(body_string_contains("<d:nresults>101</d:nresults>"))
            .respond_with(
                ResponseTemplate::new(207).set_body_string("<d:multistatus xmlns:d=\"DAV:\"/>"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/core/preview"))
            .and(query_param("fileId", "42"))
            .and(query_param("a", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1, 2, 3]))
            .mount(&server)
            .await;
        let client = NextcloudClient::with_http_client(&server.uri(), reqwest::Client::new())
            .expect("test operation should succeed");
        client
            .search("alice", "secret", "/Videos", "sample", 3, 100)
            .await
            .expect("test operation should succeed");
        client
            .preview("alice", "secret", 42, 640, 360, true)
            .await
            .expect("test operation should succeed");
    }

    #[tokio::test]
    async fn loads_single_file_metadata_with_depth_zero() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        let body = r#"<d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:response><d:href>/remote.php/dav/files/alice/Videos/movie.mp4</d:href><d:propstat><d:prop><d:displayname>movie.mp4</d:displayname><d:getcontentlength>42</d:getcontentlength><d:getcontenttype>video/mp4</d:getcontenttype><oc:fileid>99</oc:fileid><oc:favorite>1</oc:favorite><nc:has-preview>true</nc:has-preview><nc:metadata-width>1920</nc:metadata-width><nc:metadata-height>1080</nc:metadata-height><nc:metadata-duration>12.5</nc:metadata-duration></d:prop></d:propstat></d:response></d:multistatus>"#;
        Mock::given(method("PROPFIND"))
            .and(path("/remote.php/dav/files/alice/Videos/movie.mp4"))
            .and(header("Depth", "0"))
            .respond_with(ResponseTemplate::new(207).set_body_string(body))
            .mount(&server)
            .await;
        let client = NextcloudClient::with_http_client(&server.uri(), reqwest::Client::new())
            .expect("test operation should succeed");
        let item = client
            .metadata("alice", "secret", "/Videos/movie.mp4")
            .await
            .expect("test operation should succeed");
        assert_eq!(item.file_id, 99);
        assert_eq!(item.content_type.as_deref(), Some("video/mp4"));
        assert_eq!(item.duration_millis, Some(12_500));
        assert_eq!((item.width, item.height), (Some(1920), Some(1080)));
    }
}
