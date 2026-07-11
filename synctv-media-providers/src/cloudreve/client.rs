use reqwest::{header, Client};
use serde::de::DeserializeOwned;
use serde_json::json;

use super::types::{
    CloudreveList, CloudreveLogin, CloudreveResponse, CloudreveSearch, CloudreveThumbnail,
    CloudreveToken, CloudreveUrl, CloudreveUser,
};
use crate::{fetch_json, ProviderClientError, PROVIDER_USER_AGENT};

fn normalize_host(host: &str) -> Result<String, ProviderClientError> {
    let parsed = url::Url::parse(host).map_err(|error| {
        ProviderClientError::InvalidConfig(format!("Invalid host URL: {error}"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ProviderClientError::InvalidConfig(
            "Cloudreve host must be an HTTP(S) origin without credentials, query, or fragment"
                .to_string(),
        ));
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn normalize_uri(path: &str) -> Result<String, ProviderClientError> {
    let trimmed = path.trim();
    let uri = if trimmed.starts_with("cloudreve://") {
        trimmed.to_string()
    } else if trimmed.is_empty() || trimmed == "/" {
        "cloudreve://my/".to_string()
    } else {
        format!("cloudreve://my/{}", trimmed.trim_start_matches('/'))
    };
    let parsed = url::Url::parse(&uri).map_err(|error| {
        ProviderClientError::InvalidConfig(format!("Invalid Cloudreve path: {error}"))
    })?;
    if parsed.scheme() != "cloudreve" || parsed.host_str() != Some("my") {
        return Err(ProviderClientError::InvalidConfig(
            "Cloudreve path must target cloudreve://my".to_string(),
        ));
    }
    if parsed
        .path_segments()
        .is_some_and(|segments| segments.into_iter().any(|part| part == ".."))
    {
        return Err(ProviderClientError::InvalidConfig(
            "Cloudreve path traversal is not allowed".to_string(),
        ));
    }
    Ok(uri)
}

pub struct CloudreveClient {
    host: String,
    client: Client,
}

impl CloudreveClient {
    pub fn new(host: &str) -> Result<Self, ProviderClientError> {
        let client =
            crate::build_provider_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())?;
        Self::with_http_client(host, client)
    }

    pub fn with_http_client(host: &str, client: Client) -> Result<Self, ProviderClientError> {
        Ok(Self {
            host: normalize_host(host)?,
            client,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.host)
    }

    async fn unwrap<T: DeserializeOwned>(
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<T, ProviderClientError> {
        let response: CloudreveResponse<T> = fetch_json(request).await?;
        if response.code != 0 {
            return Err(ProviderClientError::Api {
                code: response.code,
                message: response.msg,
            });
        }
        response.data.ok_or_else(|| {
            ProviderClientError::Parse(format!("Cloudreve {operation} response has no data"))
        })
    }

    fn authenticated(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(method, self.endpoint(path))
            .bearer_auth(token)
            .header(header::USER_AGENT, PROVIDER_USER_AGENT)
    }

    pub async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> Result<CloudreveToken, ProviderClientError> {
        let response: CloudreveLogin = Self::unwrap(
            self.client
                .post(self.endpoint("/api/v4/session/token"))
                .header(header::USER_AGENT, PROVIDER_USER_AGENT)
                .json(&json!({ "email": email, "password": password })),
            "login",
        )
        .await?;
        Ok(response.token)
    }

    pub async fn me(&self, token: &str) -> Result<CloudreveUser, ProviderClientError> {
        Self::unwrap(
            self.authenticated(reqwest::Method::GET, "/api/v4/user/me", token),
            "me",
        )
        .await
    }

    pub async fn list(
        &self,
        token: &str,
        path: &str,
        page: u32,
        next_page_token: Option<&str>,
        per_page: u32,
    ) -> Result<CloudreveList, ProviderClientError> {
        let uri = normalize_uri(path)?;
        let mut request = self
            .authenticated(reqwest::Method::GET, "/api/v4/file", token)
            .query(&[
                ("uri", uri),
                ("page", page.max(1).to_string()),
                ("page_size", per_page.clamp(1, 200).to_string()),
            ]);
        if let Some(next_page_token) = next_page_token.filter(|value| !value.is_empty()) {
            request = request.query(&[("next_page_token", next_page_token)]);
        }
        Self::unwrap(request, "list").await
    }

    pub async fn search(
        &self,
        token: &str,
        keywords: &str,
        offset: u64,
    ) -> Result<CloudreveSearch, ProviderClientError> {
        Self::unwrap(
            self.authenticated(reqwest::Method::GET, "/api/v4/file/search", token)
                .query(&[("query", keywords), ("offset", &offset.to_string())]),
            "search",
        )
        .await
    }

    pub async fn file_url(
        &self,
        token: &str,
        path: &str,
    ) -> Result<CloudreveUrl, ProviderClientError> {
        let uri = normalize_uri(path)?;
        Self::unwrap(
            self.authenticated(reqwest::Method::POST, "/api/v4/file/url", token)
                .json(&json!({
                    "uris": [uri],
                    "download": false,
                    "redirect": false,
                })),
            "file URL",
        )
        .await
    }

    pub async fn thumbnail(
        &self,
        token: &str,
        path: &str,
    ) -> Result<CloudreveThumbnail, ProviderClientError> {
        let uri = normalize_uri(path)?;
        Self::unwrap(
            self.authenticated(reqwest::Method::GET, "/api/v4/file/thumb", token)
                .query(&[("uri", uri)]),
            "thumbnail",
        )
        .await
    }
}
