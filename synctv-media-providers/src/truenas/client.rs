use reqwest::{Method, RequestBuilder, Url};
use serde_json::json;

use crate::error::{fetch_json, ProviderClientError};
use crate::PROVIDER_USER_AGENT;

use super::{
    TrueNasDownloadTicket, TrueNasFileItem, TrueNasFileStat, TrueNasList, TrueNasSystemInfo,
};

#[derive(Clone)]
pub struct TrueNasClient {
    client: reqwest::Client,
    endpoint: Url,
}

impl TrueNasClient {
    pub fn with_http_client(
        endpoint: &str,
        client: reqwest::Client,
    ) -> Result<Self, ProviderClientError> {
        let mut endpoint = Url::parse(endpoint.trim()).map_err(|error| {
            ProviderClientError::InvalidConfig(format!("Invalid TrueNAS endpoint: {error}"))
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(ProviderClientError::InvalidConfig(
                "TrueNAS endpoint must be an HTTP(S) URL".to_string(),
            ));
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        Ok(Self { client, endpoint })
    }

    fn url(&self, path: &str) -> Result<Url, ProviderClientError> {
        self.endpoint
            .join(path.trim_start_matches('/'))
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))
    }

    fn authenticated(
        &self,
        method: Method,
        path: &str,
        api_key: &str,
    ) -> Result<RequestBuilder, ProviderClientError> {
        Ok(self
            .client
            .request(method, self.url(path)?)
            .bearer_auth(api_key)
            .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT))
    }

    pub async fn system_info(
        &self,
        api_key: &str,
    ) -> Result<TrueNasSystemInfo, ProviderClientError> {
        fetch_json(self.authenticated(Method::GET, "api/v2.0/system/info/", api_key)?).await
    }

    pub async fn list(
        &self,
        api_key: &str,
        path: &str,
        page: u64,
        page_size: u32,
        search: Option<&str>,
    ) -> Result<TrueNasList, ProviderClientError> {
        let items = self.list_all(api_key, path, search).await?;
        Ok(paginate(items, page, page_size))
    }

    pub async fn list_all(
        &self,
        api_key: &str,
        path: &str,
        search: Option<&str>,
    ) -> Result<Vec<TrueNasFileItem>, ProviderClientError> {
        let path = normalize_path(path)?;
        let mut items: Vec<TrueNasFileItem> = fetch_json(
            self.authenticated(Method::GET, "api/v2.0/filesystem/listdir/", api_key)?
                .query(&[("path", path)]),
        )
        .await?;
        if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
            let search = search.to_lowercase();
            items.retain(|item| item.name.to_lowercase().contains(&search));
        }
        items.sort_by(|left, right| {
            right
                .is_directory()
                .cmp(&left.is_directory())
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        Ok(items)
    }

    pub async fn download_ticket(
        &self,
        api_key: &str,
        path: &str,
    ) -> Result<TrueNasDownloadTicket, ProviderClientError> {
        let path = normalize_file_path(path)?;
        let filename = path.rsplit('/').next().unwrap_or("media");
        let response: (u64, String) = fetch_json(
            self.authenticated(Method::POST, "api/v2.0/core/download/", api_key)?
                .json(&json!({
                    "method": "filesystem.get",
                    "args": [path],
                    "filename": filename,
                    "buffered": false
                })),
        )
        .await?;
        let url = self.url(response.1.trim_start_matches('/'))?;
        if url.host_str() != self.endpoint.host_str() {
            return Err(ProviderClientError::InvalidConfig(
                "TrueNAS download URL changed host".to_string(),
            ));
        }
        Ok(TrueNasDownloadTicket {
            job_id: response.0,
            url,
        })
    }

    pub async fn stat(
        &self,
        api_key: &str,
        path: &str,
    ) -> Result<TrueNasFileStat, ProviderClientError> {
        let path = normalize_file_path(path)?;
        let stat: TrueNasFileStat = fetch_json(
            self.authenticated(Method::POST, "api/v2.0/filesystem/stat/", api_key)?
                .json(&json!({"path": path})),
        )
        .await?;
        if !stat.realpath.starts_with("/mnt/") {
            return Err(ProviderClientError::InvalidConfig(
                "TrueNAS file realpath must remain under /mnt".to_string(),
            ));
        }
        Ok(stat)
    }
}

fn normalize_path(path: &str) -> Result<&str, ProviderClientError> {
    let path = if path.trim().is_empty() {
        "/mnt"
    } else {
        path.trim()
    };
    if !path.starts_with("/mnt") || path.split('/').any(|segment| segment == "..") {
        return Err(ProviderClientError::InvalidConfig(
            "TrueNAS path must remain under /mnt".to_string(),
        ));
    }
    Ok(path)
}

fn normalize_file_path(path: &str) -> Result<&str, ProviderClientError> {
    let path = normalize_path(path)?;
    if path == "/mnt" || path.ends_with('/') {
        return Err(ProviderClientError::InvalidConfig(
            "TrueNAS media path must identify a file".to_string(),
        ));
    }
    Ok(path)
}

fn paginate(items: Vec<TrueNasFileItem>, page: u64, page_size: u32) -> TrueNasList {
    let page = page.max(1);
    let page_size = page_size.clamp(1, 200);
    let total = items.len() as u64;
    let start = page.saturating_sub(1).saturating_mul(u64::from(page_size));
    let items = usize::try_from(start)
        .ok()
        .map(|start| {
            items
                .into_iter()
                .skip(start)
                .take(page_size as usize)
                .collect()
        })
        .unwrap_or_default();
    TrueNasList {
        items,
        total,
        page,
        page_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn paths_are_scoped_to_storage_mounts() {
        assert_eq!(normalize_path("").expect("root should normalize"), "/mnt");
        assert!(normalize_path("/etc/passwd").is_err());
        assert!(normalize_path("/mnt/tank/../secret").is_err());
    }

    #[tokio::test]
    async fn uses_native_filesystem_and_download_contracts() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2.0/filesystem/listdir/"))
            .and(query_param("path", "/mnt/tank"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name":"Movie.mkv","path":"/mnt/tank/Movie.mkv","type":"FILE","size":42},
                {"name":"Shows","path":"/mnt/tank/Shows","type":"DIRECTORY"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2.0/core/download/"))
            .and(header("authorization", "Bearer secret"))
            .and(body_string_contains("filesystem.get"))
            .and(body_string_contains("/mnt/tank/Movie.mkv"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([17, "/_download/17"])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2.0/filesystem/stat/"))
            .and(header("authorization", "Bearer secret"))
            .and(body_string_contains("/mnt/tank/Movie.mkv"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "realpath": "/mnt/tank/Movie.mkv",
                "type": "FILE",
                "size": 42,
                "allocation_size": 4096,
                "mode": 33188,
                "mount_id": 7,
                "uid": 1000,
                "gid": 1000,
                "atime": 1.0,
                "mtime": 2.0,
                "ctime": 3.0,
                "btime": 4.0,
                "dev": 8,
                "inode": 9,
                "nlink": 1,
                "acl": true,
                "is_mountpoint": false,
                "is_ctldir": false,
                "attributes": ["ARCHIVE"],
                "user": "media",
                "group": "media"
            })))
            .mount(&server)
            .await;

        let client = TrueNasClient::with_http_client(&server.uri(), reqwest::Client::new())
            .expect("client should build");
        let list = client
            .list("secret", "/mnt/tank", 1, 50, None)
            .await
            .expect("list should succeed");
        assert!(list.items[0].is_directory());
        let ticket = client
            .download_ticket("secret", "/mnt/tank/Movie.mkv")
            .await
            .expect("download ticket should succeed");
        let stat = client
            .stat("secret", "/mnt/tank/Movie.mkv")
            .await
            .expect("stat should succeed");
        assert_eq!(ticket.job_id, 17);
        assert_eq!(ticket.url.path(), "/_download/17");
        assert_eq!(stat.inode, 9);
        assert_eq!(stat.user.as_deref(), Some("media"));
    }
}
