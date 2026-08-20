use std::collections::HashMap;

use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use url::Url;

use super::types::{
    SynologyApiInfo, SynologyApiMap, SynologyAudioTrackList, SynologyEnvelope, SynologyEpisodeList,
    SynologyFileList, SynologyHomeVideoList, SynologyLibraryList, SynologyLogin, SynologyMovieList,
    SynologySearchTask, SynologyStreamFile, SynologyStreamProfile, SynologyStreamSession,
    SynologySubtitle, SynologyTvRecordingList, SynologyTvShowList, SynologyVideoItemKind,
    SynologyVideoMetadata,
};
use crate::{fetch_json, ProviderClientError, PROVIDER_USER_AGENT};

const FILE_ADDITIONAL: &str = r#"["real_path","size","owner","time","type","mount_point_type"]"#;
const VIDEO_ADDITIONAL: &str = r#"["summary","actor","file","extra","genre","writer","director","collection","poster_mtime","watched_ratio","conversion_produced","backdrop_mtime","parental_control"]"#;

#[derive(Clone)]
pub struct SynologyClient {
    origin: String,
    client: Client,
}

impl SynologyClient {
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
                "Synology endpoint must use HTTP(S)".to_string(),
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
        let path = path.trim_start_matches('/');
        format!("{}/webapi/{path}", self.origin)
    }

    fn form_request(&self, path: &str, form: &HashMap<&str, String>) -> RequestBuilder {
        self.client
            .post(self.endpoint(path))
            .header(reqwest::header::USER_AGENT, PROVIDER_USER_AGENT)
            .form(form)
    }

    async fn envelope<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, ProviderClientError> {
        let envelope: SynologyEnvelope<T> = fetch_json(request).await?;
        if envelope.success {
            return envelope.data.ok_or_else(|| {
                ProviderClientError::Parse(format!("Synology {operation} response has no data"))
            });
        }
        let code = envelope.error.map_or(0, |error| error.code);
        Err(ProviderClientError::Api {
            code,
            message: synology_error_message(code, operation).to_string(),
        })
    }

    async fn empty_envelope(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<(), ProviderClientError> {
        let envelope: SynologyEnvelope<serde_json::Value> = fetch_json(request).await?;
        if envelope.success {
            return Ok(());
        }
        let code = envelope.error.map_or(0, |error| error.code);
        Err(ProviderClientError::Api {
            code,
            message: synology_error_message(code, operation).to_string(),
        })
    }

    pub async fn discover(&self, query: &[&str]) -> Result<SynologyApiMap, ProviderClientError> {
        let query = query.join(",");
        let form = HashMap::from([
            ("api", "SYNO.API.Info".to_string()),
            ("version", "1".to_string()),
            ("method", "query".to_string()),
            ("query", query),
        ]);
        self.envelope(self.form_request("query.cgi", &form), "API discovery")
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn login(
        &self,
        auth_api: &SynologyApiInfo,
        account: &str,
        password: &str,
        session: &str,
        otp_code: Option<&str>,
        device_name: Option<&str>,
    ) -> Result<SynologyLogin, ProviderClientError> {
        let mut form = api_form("SYNO.API.Auth", auth_api, "login", None);
        form.insert("account", account.to_string());
        form.insert("passwd", password.to_string());
        form.insert("session", session.to_string());
        form.insert("format", "sid".to_string());
        if let Some(otp_code) = otp_code.filter(|value| !value.trim().is_empty()) {
            form.insert("otp_code", otp_code.trim().to_string());
        }
        if let Some(device_name) = device_name.filter(|value| !value.trim().is_empty()) {
            form.insert("enable_device_token", "yes".to_string());
            form.insert("device_name", device_name.trim().to_string());
        }
        self.envelope(self.form_request(&auth_api.path, &form), "authentication")
            .await
    }

    pub async fn logout(
        &self,
        auth_api: &SynologyApiInfo,
        sid: &str,
        session: &str,
    ) -> Result<(), ProviderClientError> {
        let mut form = api_form("SYNO.API.Auth", auth_api, "logout", Some(sid));
        form.insert("session", session.to_string());
        let envelope: SynologyEnvelope<serde_json::Value> =
            fetch_json(self.form_request(&auth_api.path, &form)).await?;
        if envelope.success {
            return Ok(());
        }
        let code = envelope.error.map_or(0, |error| error.code);
        Err(ProviderClientError::Api {
            code,
            message: synology_error_message(code, "logout").to_string(),
        })
    }

    pub async fn list_shares(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        offset: u64,
        limit: u32,
    ) -> Result<SynologyFileList, ProviderClientError> {
        let mut form = api_form("SYNO.FileStation.List", api, "list_share", Some(sid));
        add_list_params(&mut form, offset, limit);
        self.envelope(self.form_request(&api.path, &form), "share listing")
            .await
    }

    pub async fn list_files(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        folder_path: &str,
        offset: u64,
        limit: u32,
    ) -> Result<SynologyFileList, ProviderClientError> {
        let mut form = api_form("SYNO.FileStation.List", api, "list", Some(sid));
        form.insert("folder_path", folder_path.to_string());
        add_list_params(&mut form, offset, limit);
        self.envelope(self.form_request(&api.path, &form), "file listing")
            .await
    }

    pub async fn start_search(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        folder_path: &str,
        keyword: &str,
        recursive: bool,
    ) -> Result<SynologySearchTask, ProviderClientError> {
        let mut form = api_form("SYNO.FileStation.Search", api, "start", Some(sid));
        form.insert("folder_path", folder_path.to_string());
        form.insert("pattern", format!("*{}*", keyword.trim()));
        form.insert("recursive", recursive.to_string());
        self.envelope(self.form_request(&api.path, &form), "search start")
            .await
    }

    pub async fn list_search(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        task_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<SynologyFileList, ProviderClientError> {
        let mut form = api_form("SYNO.FileStation.Search", api, "list", Some(sid));
        form.insert("taskid", task_id.to_string());
        add_list_params(&mut form, offset, limit);
        self.envelope(self.form_request(&api.path, &form), "search results")
            .await
    }

    pub async fn stop_search(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        task_id: &str,
    ) -> Result<(), ProviderClientError> {
        let mut form = api_form("SYNO.FileStation.Search", api, "stop", Some(sid));
        form.insert("taskid", task_id.to_string());
        let envelope: SynologyEnvelope<serde_json::Value> =
            fetch_json(self.form_request(&api.path, &form)).await?;
        if envelope.success {
            return Ok(());
        }
        let code = envelope.error.map_or(0, |error| error.code);
        Err(ProviderClientError::Api {
            code,
            message: synology_error_message(code, "search stop").to_string(),
        })
    }

    pub fn download_url(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        path: &str,
    ) -> Result<String, ProviderClientError> {
        self.resource_url(
            api,
            "SYNO.FileStation.Download",
            "download",
            sid,
            &[("path", path), ("mode", "download")],
        )
    }

    pub fn thumbnail_url(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        path: &str,
        size: &str,
    ) -> Result<String, ProviderClientError> {
        let size = match size {
            "small" | "medium" | "large" | "original" => size,
            _ => "medium",
        };
        self.resource_url(
            api,
            "SYNO.FileStation.Thumb",
            "get",
            sid,
            &[("path", path), ("size", size), ("rotate", "0")],
        )
    }

    pub async fn list_video_libraries(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
    ) -> Result<SynologyLibraryList, ProviderClientError> {
        let form = api_form("SYNO.VideoStation.Library", api, "list", Some(sid));
        self.envelope(self.form_request(&api.path, &form), "video library listing")
            .await
    }

    pub async fn list_movies(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        library_id: i64,
        offset: u64,
        limit: u32,
        search: Option<&str>,
    ) -> Result<SynologyMovieList, ProviderClientError> {
        let form = video_list_form(
            "SYNO.VideoStation.Movie",
            api,
            sid,
            library_id,
            offset,
            limit,
            search,
        );
        self.envelope(self.form_request(&api.path, &form), "movie listing")
            .await
    }

    pub async fn list_tv_shows(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        library_id: i64,
        offset: u64,
        limit: u32,
        search: Option<&str>,
    ) -> Result<SynologyTvShowList, ProviderClientError> {
        let form = video_list_form(
            "SYNO.VideoStation.TVShow",
            api,
            sid,
            library_id,
            offset,
            limit,
            search,
        );
        self.envelope(self.form_request(&api.path, &form), "TV show listing")
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_episodes(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        library_id: i64,
        tv_show_id: i64,
        offset: u64,
        limit: u32,
        search: Option<&str>,
    ) -> Result<SynologyEpisodeList, ProviderClientError> {
        let mut form = video_list_form(
            "SYNO.VideoStation.TVShowEpisode",
            api,
            sid,
            library_id,
            offset,
            limit,
            search,
        );
        form.insert("tvshow_id", tv_show_id.to_string());
        self.envelope(self.form_request(&api.path, &form), "TV episode listing")
            .await
    }

    pub async fn list_home_videos(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        library_id: i64,
        offset: u64,
        limit: u32,
        search: Option<&str>,
    ) -> Result<SynologyHomeVideoList, ProviderClientError> {
        let form = video_list_form(
            "SYNO.VideoStation.HomeVideo",
            api,
            sid,
            library_id,
            offset,
            limit,
            search,
        );
        self.envelope(self.form_request(&api.path, &form), "home video listing")
            .await
    }

    pub async fn list_tv_recordings(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        library_id: i64,
        offset: u64,
        limit: u32,
        search: Option<&str>,
    ) -> Result<SynologyTvRecordingList, ProviderClientError> {
        let form = video_list_form(
            "SYNO.VideoStation.TVRecording",
            api,
            sid,
            library_id,
            offset,
            limit,
            search,
        );
        self.envelope(self.form_request(&api.path, &form), "TV recording listing")
            .await
    }

    pub async fn video_item(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        kind: SynologyVideoItemKind,
        item_id: i64,
    ) -> Result<SynologyVideoMetadata, ProviderClientError> {
        let (api_name, response_keys) = match kind {
            SynologyVideoItemKind::Movie => ("SYNO.VideoStation2.Movie", &["movie", "movies"][..]),
            SynologyVideoItemKind::Episode => (
                "SYNO.VideoStation2.TVShowEpisode",
                &["episode", "episodes"][..],
            ),
            SynologyVideoItemKind::HomeVideo => (
                "SYNO.VideoStation2.HomeVideo",
                &["home_video", "homevideos", "videos"][..],
            ),
            SynologyVideoItemKind::TvRecording => (
                "SYNO.VideoStation2.TVRecording",
                &["tv_recording", "recordings", "tv_recordings"][..],
            ),
        };
        let mut form = api_form(api_name, api, "getinfo", Some(sid));
        form.insert("id", format!("[{item_id}]"));
        form.insert("additional", VIDEO_ADDITIONAL.to_string());
        let data: serde_json::Value = self
            .envelope(self.form_request(&api.path, &form), "video item lookup")
            .await?;
        let item = response_keys
            .iter()
            .find_map(|key| data.get(key))
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .ok_or_else(|| {
                ProviderClientError::Parse("Synology video item response is empty".to_string())
            })?;
        serde_json::from_value(item).map_err(|error| ProviderClientError::Parse(error.to_string()))
    }

    pub async fn list_audio_tracks(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        file_id: i64,
    ) -> Result<SynologyAudioTrackList, ProviderClientError> {
        let mut form = api_form("SYNO.VideoStation.AudioTrack", api, "list", Some(sid));
        form.insert("id", file_id.to_string());
        self.envelope(self.form_request(&api.path, &form), "audio track listing")
            .await
    }

    pub async fn list_subtitles(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        file_id: i64,
    ) -> Result<Vec<SynologySubtitle>, ProviderClientError> {
        let mut form = api_form("SYNO.VideoStation.Subtitle", api, "list", Some(sid));
        form.insert("id", file_id.to_string());
        self.envelope(self.form_request(&api.path, &form), "subtitle listing")
            .await
    }

    pub async fn set_watch_position(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        file_id: i64,
        position_seconds: u64,
    ) -> Result<(), ProviderClientError> {
        let mut form = api_form("SYNO.VideoStation.WatchStatus", api, "setinfo", Some(sid));
        form.insert("id", file_id.to_string());
        form.insert("position", position_seconds.to_string());
        self.empty_envelope(self.form_request(&api.path, &form), "watch status update")
            .await
    }

    pub async fn open_stream(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        file_id: i64,
        profile: SynologyStreamProfile,
        audio_track: Option<i64>,
        ac3_passthrough: bool,
    ) -> Result<SynologyStreamSession, ProviderClientError> {
        let mut form = api_form("SYNO.VideoStation2.Streaming", api, "open", Some(sid));
        let file = serde_json::to_string(&SynologyStreamFile {
            id: file_id,
            path: "",
        })
        .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
        form.insert("file", file);

        let mut options = serde_json::Map::new();
        if let Some(audio_track) = audio_track.filter(|track| *track > 0) {
            options.insert("audio_track".to_string(), audio_track.into());
        }
        if ac3_passthrough {
            options.insert("audio_format".to_string(), "ac3_copy".into());
        }
        let (format, profile_name) = match profile {
            SynologyStreamProfile::Raw => ("raw", None),
            SynologyStreamProfile::HlsRemux => ("hls_remux", None),
            SynologyStreamProfile::HlsMedium => ("hls", Some("hd_medium")),
            SynologyStreamProfile::HlsLow => ("hls", Some("hd_low")),
        };
        if let Some(profile_name) = profile_name {
            options.insert("force_open_vte".to_string(), false.into());
            options.insert("profile".to_string(), profile_name.into());
        }
        if profile == SynologyStreamProfile::Raw {
            options.clear();
        }
        form.insert(format, serde_json::Value::Object(options).to_string());
        self.envelope(self.form_request(&api.path, &form), "stream open")
            .await
    }

    pub async fn close_stream(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        stream_id: &str,
        format: &str,
    ) -> Result<(), ProviderClientError> {
        let mut form = api_form("SYNO.VideoStation.Streaming", api, "close", Some(sid));
        form.insert("id", stream_id.to_string());
        form.insert("format", normalize_stream_format(format).to_string());
        form.insert("force_close", "true".to_string());
        self.empty_envelope(self.form_request(&api.path, &form), "stream close")
            .await
    }

    pub fn stream_url(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        stream_id: &str,
        format: &str,
    ) -> Result<String, ProviderClientError> {
        self.resource_url(
            api,
            "SYNO.VideoStation.Streaming",
            "stream",
            sid,
            &[
                ("id", stream_id),
                ("format", normalize_stream_format(format)),
            ],
        )
    }

    pub fn subtitle_url(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        file_id: i64,
        subtitle_id: &str,
        preview: bool,
    ) -> Result<String, ProviderClientError> {
        let file_id = file_id.to_string();
        let preview = preview.to_string();
        self.resource_url(
            api,
            "SYNO.VideoStation.Subtitle",
            "get",
            sid,
            &[
                ("id", &file_id),
                ("preview", &preview),
                ("subtitle_id", subtitle_id),
            ],
        )
    }

    pub fn poster_url(
        &self,
        api: &SynologyApiInfo,
        sid: &str,
        media_id: i64,
        media_type: &str,
        poster_mtime: Option<&str>,
    ) -> Result<String, ProviderClientError> {
        let media_id = media_id.to_string();
        let mut params = vec![("id", media_id.as_str()), ("type", media_type)];
        if let Some(poster_mtime) = poster_mtime.filter(|value| !value.is_empty()) {
            params.push(("poster_mtime", poster_mtime));
        }
        self.resource_url(api, "SYNO.VideoStation.Poster", "getimage", sid, &params)
    }

    fn resource_url(
        &self,
        api: &SynologyApiInfo,
        api_name: &str,
        method: &str,
        sid: &str,
        params: &[(&str, &str)],
    ) -> Result<String, ProviderClientError> {
        let mut url = Url::parse(&self.endpoint(&api.path))
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let version = preferred_version(api).to_string();
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("api", api_name)
                .append_pair("version", &version)
                .append_pair("method", method)
                .append_pair("_sid", sid);
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }
        Ok(url.to_string())
    }
}

fn api_form(
    api_name: &'static str,
    api: &SynologyApiInfo,
    method: &str,
    sid: Option<&str>,
) -> HashMap<&'static str, String> {
    let mut form = HashMap::from([
        ("api", api_name.to_string()),
        ("version", preferred_version(api).to_string()),
        ("method", method.to_string()),
    ]);
    if let Some(sid) = sid {
        form.insert("_sid", sid.to_string());
    }
    form
}

fn add_list_params(form: &mut HashMap<&'static str, String>, offset: u64, limit: u32) {
    form.insert("offset", offset.to_string());
    form.insert("limit", limit.to_string());
    form.insert("sort_by", "name".to_string());
    form.insert("sort_direction", "asc".to_string());
    form.insert("additional", FILE_ADDITIONAL.to_string());
}

#[allow(clippy::too_many_arguments)]
fn video_list_form(
    api_name: &'static str,
    api: &SynologyApiInfo,
    sid: &str,
    library_id: i64,
    offset: u64,
    limit: u32,
    search: Option<&str>,
) -> HashMap<&'static str, String> {
    let mut form = api_form(api_name, api, "list", Some(sid));
    form.insert("library_id", library_id.to_string());
    form.insert("offset", offset.to_string());
    form.insert("limit", limit.clamp(1, 1_000).to_string());
    form.insert("sort_by", "title".to_string());
    form.insert("sort_direction", "asc".to_string());
    form.insert("additional", VIDEO_ADDITIONAL.to_string());
    if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
        form.insert(
            "title",
            serde_json::Value::Array(vec![search.into()]).to_string(),
        );
    }
    form
}

fn normalize_stream_format(format: &str) -> &str {
    match format {
        "hls" | "hls_remux" => format,
        _ => "raw",
    }
}

fn preferred_version(api: &SynologyApiInfo) -> u32 {
    api.max_version.max(api.min_version)
}

fn synology_error_message(code: i64, operation: &str) -> &'static str {
    match code {
        100 => "Unknown Synology API error",
        101 => "Invalid Synology API parameter",
        102 => "Requested Synology API does not exist",
        103 => "Requested Synology API method does not exist",
        104 => "Requested Synology API version is unsupported",
        105 => "Synology user lacks application permission",
        106 => "Synology session timed out",
        107 => "Synology session was interrupted",
        400 => "Invalid Synology credentials",
        401 => "Synology account is disabled",
        402 => "Synology permission denied",
        403 => "Synology requires two-factor authentication",
        404 => "Synology two-factor authentication failed",
        _ if operation == "authentication" => "Synology authentication failed",
        _ => "Synology API request failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn api(path: &str, max_version: u32) -> SynologyApiInfo {
        SynologyApiInfo {
            path: path.to_string(),
            min_version: 1,
            max_version,
        }
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn discovers_logs_in_and_lists_file_station_resources() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/webapi/query.cgi"))
            .and(body_string_contains("api=SYNO.API.Info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {
                    "SYNO.API.Auth": {"path":"auth.cgi","minVersion":1,"maxVersion":7},
                    "SYNO.FileStation.List": {"path":"entry.cgi","minVersion":1,"maxVersion":2}
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/auth.cgi"))
            .and(body_string_contains("session=FileStation"))
            .and(body_string_contains("otp_code=123456"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {"sid":"file-sid","did":"device-id"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/entry.cgi"))
            .and(body_string_contains("method=list_share"))
            .and(body_string_contains("_sid=file-sid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {
                    "offset": 0,
                    "total": 1,
                    "shares": [{
                        "name": "video",
                        "path": "/video",
                        "isdir": true,
                        "additional": {"size": 1024, "time": {"mtime": 1_700_000_000}}
                    }]
                }
            })))
            .mount(&server)
            .await;

        let client = SynologyClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let discovered = client
            .discover(&["SYNO.API.Auth", "SYNO.FileStation.*"])
            .await
            .expect("test operation should succeed");
        let login = client
            .login(
                &discovered["SYNO.API.Auth"],
                "alice",
                "secret",
                "FileStation",
                Some("123456"),
                Some("SyncTV"),
            )
            .await
            .expect("test operation should succeed");
        let shares = client
            .list_shares(&discovered["SYNO.FileStation.List"], &login.sid, 0, 50)
            .await
            .expect("test operation should succeed");
        assert_eq!(shares.total, 1);
        assert_eq!(shares.files[0].path, "/video");
        assert_eq!(shares.files[0].additional.time.mtime, 1_700_000_000);
    }

    #[test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    fn builds_download_and_thumbnail_urls_from_discovered_paths() {
        crate::install_process_crypto_provider();
        let client = SynologyClient::with_http_client("https://nas.example/dsm/", Client::new())
            .expect("test operation should succeed");
        let api = api("entry.cgi", 2);
        let download = client
            .download_url(&api, "sid", "/video/A Movie.mkv")
            .expect("test operation should succeed");
        assert!(download.contains("api=SYNO.FileStation.Download"));
        assert!(download.contains("path=%2Fvideo%2FA+Movie.mkv"));
        let thumbnail = client
            .thumbnail_url(&api, "sid", "/video/A Movie.mkv", "large")
            .expect("test operation should succeed");
        assert!(thumbnail.contains("api=SYNO.FileStation.Thumb"));
        assert!(thumbnail.contains("size=large"));
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn lists_video_station_metadata_and_tracks() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/webapi/VideoStation/library.cgi"))
            .and(body_string_contains("api=SYNO.VideoStation.Library"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {
                    "offset": 0,
                    "total": 1,
                    "libraries": [{
                        "id": 0,
                        "is_public": true,
                        "title": "Movie",
                        "type": "movie",
                        "visible": true
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/VideoStation/movie.cgi"))
            .and(body_string_contains("api=SYNO.VideoStation.Movie"))
            .and(body_string_contains("library_id=0"))
            .and(body_string_contains("title=%5B%22Bond%22%5D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {
                    "offset": 0,
                    "total": 1,
                    "movies": [{
                        "id": 100,
                        "library_id": 0,
                        "mapper_id": 212,
                        "title": "Tomorrow Never Dies",
                        "additional": {
                            "summary": "Bond investigates a media mogul.",
                            "poster_mtime": "2018-04-07 10:23:51.029677",
                            "file": [{
                                "id": 71,
                                "path": "/volume1/video/movie.mkv",
                                "sharepath": "/video/movie.mkv",
                                "filesize": 16_875_216_934_u64,
                                "duration": "1:59:20",
                                "position": 173,
                                "resolutionx": 1916,
                                "resolutiony": 820,
                                "video_codec": "h264",
                                "audio_codec": "ac3"
                            }]
                        }
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/VideoStation/audiotrack.cgi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {
                    "trackinfo": [{
                        "id": 1,
                        "track": 1,
                        "language": "eng",
                        "is_default": true,
                        "codec": "ac3",
                        "channel": 6
                    }]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/VideoStation/subtitle.cgi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": [
                    {"id": 1, "embedded": true, "format": "srt", "lang": "eng"},
                    {"id": "/video/movie.zh.srt", "embedded": false, "format": "srt", "lang": "zho"}
                ]
            })))
            .mount(&server)
            .await;

        let client = SynologyClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let libraries = client
            .list_video_libraries(&api("VideoStation/library.cgi", 2), "video-sid")
            .await
            .expect("test operation should succeed");
        assert_eq!(libraries.libraries[0].library_type, "movie");
        let movies = client
            .list_movies(
                &api("VideoStation/movie.cgi", 2),
                "video-sid",
                0,
                0,
                100,
                Some("Bond"),
            )
            .await
            .expect("test operation should succeed");
        let file = &movies.movies[0].metadata.additional.file[0];
        assert_eq!(file.filesize, 16_875_216_934);
        assert_eq!(file.duration_seconds(), Some(7_160));
        let tracks = client
            .list_audio_tracks(&api("VideoStation/audiotrack.cgi", 1), "video-sid", file.id)
            .await
            .expect("test operation should succeed");
        assert!(tracks.trackinfo[0].is_default);
        let subtitles = client
            .list_subtitles(&api("VideoStation/subtitle.cgi", 2), "video-sid", file.id)
            .await
            .expect("test operation should succeed");
        assert_eq!(subtitles[0].id, "1");
        assert_eq!(subtitles[1].id, "/video/movie.zh.srt");
    }

    #[tokio::test]
    #[cfg(any(
        feature = "tls-aws-lc",
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    ))]
    async fn opens_remux_stream_and_updates_watch_position() {
        crate::install_process_crypto_provider();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/webapi/entry.cgi"))
            .and(body_string_contains("api=SYNO.VideoStation2.Streaming"))
            .and(body_string_contains("method=open"))
            .and(body_string_contains("hls_remux="))
            .and(body_string_contains("audio_track"))
            .and(body_string_contains("ac3_copy"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {"stream_id": "stream-123", "format": "hls_remux"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/VideoStation/watchstatus.cgi"))
            .and(body_string_contains("method=setinfo"))
            .and(body_string_contains("position=321"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/webapi/entry.cgi"))
            .and(body_string_contains("api=SYNO.VideoStation.Streaming"))
            .and(body_string_contains("method=close"))
            .and(body_string_contains("id=stream-123"))
            .and(body_string_contains("format=hls_remux"))
            .and(body_string_contains("force_close=true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "data": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = SynologyClient::with_http_client(&server.uri(), Client::new())
            .expect("test operation should succeed");
        let stream = client
            .open_stream(
                &api("entry.cgi", 1),
                "video-sid",
                71,
                SynologyStreamProfile::HlsRemux,
                Some(2),
                true,
            )
            .await
            .expect("test operation should succeed");
        assert_eq!(stream.stream_id, "stream-123");
        let stream_url = client
            .stream_url(
                &api("VideoStation/vtestreaming.cgi", 3),
                "video-sid",
                &stream.stream_id,
                &stream.format,
            )
            .expect("test operation should succeed");
        assert!(stream_url.contains("method=stream"));
        assert!(stream_url.contains("format=hls_remux"));
        client
            .set_watch_position(
                &api("VideoStation/watchstatus.cgi", 1),
                "video-sid",
                71,
                321,
            )
            .await
            .expect("test operation should succeed");
        client
            .close_stream(
                &api("entry.cgi", 1),
                "video-sid",
                &stream.stream_id,
                &stream.format,
            )
            .await
            .expect("test operation should succeed");
    }
}
