use std::borrow::Cow;
use std::collections::HashMap;

use reqwest::{Client, Method, RequestBuilder, Response};
use url::Url;

use super::types::{
    AuthTokenResponse, DirectoryResponse, SeafileAccount, SeafileFileInfo, SeafileItem,
    SeafileList, SeafileRepository, SeafileServerInfo, SearchResponse, StarredResponse,
};
use crate::{check_response, fetch_json, ProviderClientError, PROVIDER_USER_AGENT};

#[derive(Clone)]
pub struct SeafileClient {
    origin: String,
    client: Client,
}

impl SeafileClient {
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
                "Seafile endpoint must use HTTP(S)".to_string(),
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

    pub fn auth_headers(token: &str) -> HashMap<String, String> {
        HashMap::from([("Authorization".to_string(), format!("Token {token}"))])
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.origin, path.trim_start_matches('/'))
    }

    fn authenticated(&self, method: Method, path: &str, token: &str) -> RequestBuilder {
        self.client
            .request(method, self.endpoint(path))
            .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT)
            .header(reqwest::header::AUTHORIZATION, format!("Token {token}"))
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<String, ProviderClientError> {
        let response: AuthTokenResponse = fetch_json(
            self.client
                .post(self.endpoint("/api2/auth-token/"))
                .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT)
                .form(&[("username", username), ("password", password)]),
        )
        .await?;
        if response.token.trim().is_empty() {
            return Err(ProviderClientError::Auth(
                "Seafile returned an empty API token".to_string(),
            ));
        }
        Ok(response.token)
    }

    pub async fn server_info(&self) -> Result<SeafileServerInfo, ProviderClientError> {
        fetch_json(
            self.client
                .get(self.endpoint("/api2/server-info/"))
                .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT),
        )
        .await
    }

    pub async fn account(&self, token: &str) -> Result<SeafileAccount, ProviderClientError> {
        fetch_json(self.authenticated(Method::GET, "/api2/account/info/", token)).await
    }

    pub async fn repositories(
        &self,
        token: &str,
        page: u64,
        page_size: u32,
    ) -> Result<SeafileList, ProviderClientError> {
        let repositories: Vec<SeafileRepository> =
            fetch_json(self.authenticated(Method::GET, "/api2/repos/", token)).await?;
        let items = repositories
            .into_iter()
            .map(|repository| SeafileItem {
                repository_id: repository.id,
                repository_name: repository.name.clone(),
                path: "/".to_string(),
                name: repository.name,
                is_directory: true,
                size: repository.size,
                modified_at: repository.mtime.to_string(),
                permission: repository.permission,
                modifier_name: repository.owner_name,
                repository_encrypted: repository.encrypted,
                password_required: repository.password_need,
                ..SeafileItem::default()
            })
            .collect();
        Ok(paginate(items, page, page_size))
    }

    pub async fn unlock_repository(
        &self,
        token: &str,
        repository_id: &str,
        password: &str,
    ) -> Result<(), ProviderClientError> {
        let response = self
            .authenticated(
                Method::POST,
                &format!("/api2/repos/{repository_id}/?op=setpassword"),
                token,
            )
            .form(&[("password", password)])
            .send()
            .await?;
        check_response(response).await?;
        Ok(())
    }

    pub async fn list(
        &self,
        token: &str,
        repository_id: &str,
        path: &str,
        page: u64,
        page_size: u32,
    ) -> Result<SeafileList, ProviderClientError> {
        let directory = fetch_json::<DirectoryResponse>(
            self.authenticated(
                Method::GET,
                &format!("/api/v2.1/repos/{repository_id}/dir/"),
                token,
            )
            .query(&[
                ("p", normalize_path(path)),
                ("with_thumbnail", Cow::Borrowed("true")),
                ("thumbnail_size", Cow::Borrowed("640")),
            ]),
        )
        .await?
        .into_items();
        let parent = normalize_path(path).into_owned();
        let items = directory
            .into_iter()
            .map(|item| {
                let item_path = join_path(&parent, &item.name);
                SeafileItem {
                    repository_id: repository_id.to_string(),
                    path: item_path,
                    name: item.name,
                    object_id: item.id,
                    is_directory: item.kind == "dir",
                    size: item.size,
                    modified_at: item.mtime.to_string(),
                    permission: item.permission,
                    modifier_name: item.modifier_name,
                    starred: item.starred,
                    has_thumbnail: !item.encoded_thumbnail_src.is_empty(),
                    ..SeafileItem::default()
                }
            })
            .collect();
        Ok(paginate(items, page, page_size))
    }

    pub async fn search(
        &self,
        token: &str,
        repository_id: &str,
        query: &str,
        page: u64,
        page_size: u32,
    ) -> Result<SeafileList, ProviderClientError> {
        if query.trim().is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "Seafile search query is required".to_string(),
            ));
        }
        let response: SearchResponse = fetch_json(
            self.authenticated(Method::GET, "/api/v2.1/search-file/", token)
                .query(&[("repo_id", repository_id), ("q", query.trim())]),
        )
        .await?;
        let items = response
            .data
            .into_iter()
            .map(|item| SeafileItem {
                repository_id: repository_id.to_string(),
                name: item
                    .path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                path: normalize_path(&item.path).into_owned(),
                is_directory: item.kind == "folder",
                size: item.size,
                modified_at: item.mtime,
                ..SeafileItem::default()
            })
            .collect();
        Ok(paginate(items, page, page_size))
    }

    pub async fn starred(
        &self,
        token: &str,
        page: u64,
        page_size: u32,
    ) -> Result<SeafileList, ProviderClientError> {
        let response: StarredResponse =
            fetch_json(self.authenticated(Method::GET, "/api/v2.1/starred-items/", token)).await?;
        let items = response
            .starred_item_list
            .into_iter()
            .filter(|item| !item.deleted)
            .map(|item| SeafileItem {
                repository_id: item.repo_id,
                repository_name: item.repo_name,
                path: normalize_path(&item.path).into_owned(),
                name: item.obj_name,
                is_directory: item.is_dir,
                modified_at: item.mtime,
                starred: true,
                has_thumbnail: !item.encoded_thumbnail_src.is_empty(),
                ..SeafileItem::default()
            })
            .collect();
        Ok(paginate(items, page, page_size))
    }

    pub async fn download_url(
        &self,
        token: &str,
        repository_id: &str,
        path: &str,
    ) -> Result<Url, ProviderClientError> {
        let value: String = fetch_json(
            self.authenticated(
                Method::GET,
                &format!("/api2/repos/{repository_id}/file/"),
                token,
            )
            .query(&[("p", normalize_path(path)), ("reuse", Cow::Borrowed("1"))]),
        )
        .await?;
        let base = Url::parse(&self.origin)
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let url = base
            .join(&value)
            .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str() != base.host_str() {
            return Err(ProviderClientError::InvalidConfig(
                "Seafile download URL must use the configured server host".to_string(),
            ));
        }
        Ok(url)
    }

    pub async fn file_info(
        &self,
        token: &str,
        repository_id: &str,
        path: &str,
    ) -> Result<SeafileFileInfo, ProviderClientError> {
        fetch_json(
            self.authenticated(
                Method::GET,
                &format!("/api/v2.1/repos/{repository_id}/file/"),
                token,
            )
            .query(&[("p", normalize_path(path))]),
        )
        .await
    }

    pub async fn file(
        &self,
        url: Url,
        range: Option<&str>,
    ) -> Result<Response, ProviderClientError> {
        let mut request = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT);
        if let Some(range) = range {
            request = request.header(reqwest::header::RANGE, range);
        }
        check_response(request.send().await?).await
    }

    pub async fn thumbnail(
        &self,
        token: &str,
        repository_id: &str,
        path: &str,
        size: u32,
    ) -> Result<Response, ProviderClientError> {
        let response = self
            .authenticated(
                Method::GET,
                &format!("/api2/repos/{repository_id}/thumbnail/"),
                token,
            )
            .query(&[
                ("p", normalize_path(path)),
                ("size", Cow::Owned(size.to_string())),
            ])
            .send()
            .await?;
        check_response(response).await
    }

    pub fn thumbnail_url(
        &self,
        repository_id: &str,
        path: &str,
        size: u32,
    ) -> Result<String, ProviderClientError> {
        let mut url =
            Url::parse(&self.endpoint(&format!("/api2/repos/{repository_id}/thumbnail/")))
                .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("p", normalize_path(path).as_ref())
            .append_pair("size", &size.to_string());
        Ok(url.into())
    }
}

fn normalize_path(path: &str) -> Cow<'_, str> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return Cow::Borrowed("/");
    }
    Cow::Owned(format!("/{}", trimmed.trim_matches('/')))
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{}", name.trim_matches('/'))
    } else {
        format!(
            "{}/{}",
            parent.trim_end_matches('/'),
            name.trim_matches('/')
        )
    }
}

fn paginate(mut items: Vec<SeafileItem>, page: u64, page_size: u32) -> SeafileList {
    items.sort_by_cached_key(|item| (!item.is_directory, item.name.to_lowercase()));
    let total = items.len() as u64;
    let page = page.max(1);
    let start = page.saturating_sub(1).saturating_mul(u64::from(page_size));
    let items = usize::try_from(start).ok().map_or_else(Vec::new, |start| {
        items
            .into_iter()
            .skip(start)
            .take(page_size as usize)
            .collect()
    });
    SeafileList {
        items,
        total,
        page,
        page_size,
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::SeafileClient;

    #[tokio::test]
    async fn login_and_list_preserve_seafile_contract() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api2/auth-token/"))
            .and(body_string_contains("username=alice%40example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token": "api-token"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2.1/repos/repo-id/dir/"))
            .and(header("authorization", "Token api-token"))
            .and(query_param("p", "/Videos"))
            .and(query_param("with_thumbnail", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"type":"file","id":"file-id","name":"Movie.mkv","parent_dir":"/Videos","size":9_007_199_254_740_993_u64,"mtime":10,"permission":"rw","starred":true,"encoded_thumbnail_src":"thumb"},
                {"type":"dir","id":"dir-id","name":"Series","parent_dir":"/Videos","mtime":11,"permission":"r"}
            ])))
            .mount(&server)
            .await;

        let client = SeafileClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let token = client
            .login("alice@example.com", "secret")
            .await
            .expect("test operation should succeed");
        let list = client
            .list(&token, "repo-id", "/Videos", 1, 50)
            .await
            .expect("test operation should succeed");

        assert_eq!(token, "api-token");
        assert_eq!(list.items[0].path, "/Videos/Series");
        assert_eq!(list.items[1].size, 9_007_199_254_740_993);
        assert!(list.items[1].has_thumbnail);
    }

    #[tokio::test]
    async fn starred_search_and_download_use_seafile_native_endpoints() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2.1/search-file/"))
            .and(query_param("repo_id", "repo-id"))
            .and(query_param("q", "movie"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"type":"file","path":"/Videos/Movie.mkv","size":42,"mtime":"2026-07-12T00:00:00+00:00"}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2.1/repos/repo-id/file/"))
            .and(query_param("p", "/Videos/Movie.mkv"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "type": "file",
                "repo_id": "repo-id",
                "parent_dir": "/Videos",
                "obj_name": "Movie.mkv",
                "obj_id": "file-id",
                "size": 42,
                "mtime": "2026-07-12T00:00:00+00:00",
                "is_locked": false,
                "can_preview": true,
                "can_edit": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2.1/starred-items/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"starred_item_list":[{"repo_id":"repo-id","repo_name":"Media","path":"/Videos/Movie.mkv","obj_name":"Movie.mkv","is_dir":false,"deleted":false,"mtime":"now","encoded_thumbnail_src":"thumb"}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api2/repos/repo-id/file/"))
            .and(query_param("p", "/Videos/Movie.mkv"))
            .and(query_param("reuse", "1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(format!("{}/seafhttp/file", server.uri())),
            )
            .mount(&server)
            .await;

        let client = SeafileClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let search = client
            .search("token", "repo-id", "movie", 1, 50)
            .await
            .expect("test operation should succeed");
        let starred = client
            .starred("token", 1, 50)
            .await
            .expect("test operation should succeed");
        let download = client
            .download_url("token", "repo-id", "/Videos/Movie.mkv")
            .await
            .expect("test operation should succeed");
        let info = client
            .file_info("token", "repo-id", "/Videos/Movie.mkv")
            .await
            .expect("test operation should succeed");

        assert_eq!(search.items[0].path, "/Videos/Movie.mkv");
        assert!(starred.items[0].starred);
        assert_eq!(download.path(), "/seafhttp/file");
        assert_eq!(info.object_id, "file-id");
        assert!(info.can_preview);
    }

    #[tokio::test]
    async fn list_accepts_current_seafile_directory_envelope() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2.1/repos/repo-id/dir/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_perm": "rw",
                "dirent_list": [{
                    "type": "file",
                    "id": "file-id",
                    "name": "Movie.mp4",
                    "mtime": 1720000000,
                    "size": 42,
                    "permission": "rw"
                }]
            })))
            .mount(&server)
            .await;

        let client = SeafileClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let list = client
            .list("token", "repo-id", "/Movies", 1, 50)
            .await
            .expect("test operation should succeed");

        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].name, "Movie.mp4");
        assert_eq!(list.items[0].path, "/Movies/Movie.mp4");
    }
}
