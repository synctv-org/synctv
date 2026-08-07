use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;
use uuid::Uuid;

use super::crypto::{sign_request, LoginCipher};
use super::types::{FileListResponse, FnosFile, RawFile};
use super::{
    FnosCredential, FnosFileList, FnosLogin, FnosLoginChallenge, FnosServerInfo, FnosWebDavConfig,
};
use crate::{ProviderClientError, MAX_RESPONSE_SIZE};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosEndpoints {
    pub websocket: String,
    pub webdav: Option<String>,
}

impl FnosEndpoints {
    pub fn parse(endpoint: &str) -> Result<Self, ProviderClientError> {
        let raw = endpoint.trim().trim_end_matches('/');
        let mut url = if raw.contains("://") {
            Url::parse(raw)
        } else {
            Url::parse(&format!("ws://{raw}"))
        }
        .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        match url.scheme() {
            "http" => url
                .set_scheme("ws")
                .map_err(|()| ProviderClientError::InvalidConfig("invalid FNOS scheme".into()))?,
            "https" => url
                .set_scheme("wss")
                .map_err(|()| ProviderClientError::InvalidConfig("invalid FNOS scheme".into()))?,
            "ws" | "wss" => {}
            _ => {
                return Err(ProviderClientError::InvalidConfig(
                    "FNOS endpoint must use ws, wss, http, or https".to_string(),
                ));
            }
        }
        if url.host_str().is_none() {
            return Err(ProviderClientError::InvalidConfig(
                "FNOS endpoint host is required".to_string(),
            ));
        }
        url.set_path("/websocket");
        url.set_query(Some("type=main"));
        url.set_fragment(None);
        Ok(Self {
            websocket: url.to_string(),
            webdav: None,
        })
    }
}

#[derive(Clone)]
pub struct FnosClient {
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    timeout: Duration,
}

impl Default for FnosClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FnosClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            timeout: Duration::from_secs(10),
        }
    }

    #[must_use]
    pub fn with_ssrf_guard(mut self, ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        self.ssrf_guard = ssrf_guard;
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn server_info(
        &self,
        endpoints: &FnosEndpoints,
    ) -> Result<FnosServerInfo, ProviderClientError> {
        let session = self.connect(endpoints).await?;
        Ok(session.server_info)
    }

    pub async fn login(
        &self,
        endpoints: &FnosEndpoints,
        username: &str,
        password: &str,
        twofa_code: Option<&str>,
        trust_device: bool,
    ) -> Result<FnosLogin, ProviderClientError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(ProviderClientError::InvalidConfig(
                "FNOS username and password are required".to_string(),
            ));
        }
        let mut session = self.connect(endpoints).await?;
        let cipher = LoginCipher::random();
        let did = device_id();
        let payload = serde_json::json!({
            "reqid": request_id(),
            "user": username,
            "password": password,
            "stay": true,
            "deviceType": "Server",
            "deviceName": "SyncTV",
            "did": did,
            "req": "user.login",
            "si": session.session_id,
        });
        session
            .send_json(cipher.encrypted_request(
                &session.public_key,
                serde_json::to_string(&payload)?.as_bytes(),
            )?)
            .await?;
        let mut response = session.receive_json().await?;
        if is_twofa_challenge(&response) {
            let access_token = response
                .get("accessToken")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let setup_required = response
                .get("isTwofaEnforced")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !response
                    .get("isBindTwofaSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let Some(code) = twofa_code else {
                return Ok(FnosLogin::Challenge(FnosLoginChallenge {
                    access_token,
                    setup_required,
                }));
            };
            if setup_required {
                return Ok(FnosLogin::Challenge(FnosLoginChallenge {
                    access_token,
                    setup_required,
                }));
            }
            if code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()) {
                return Err(ProviderClientError::InvalidConfig(
                    "FNOS two-factor code must contain 6 digits".to_string(),
                ));
            }
            let verify = serde_json::json!({
                "reqid": request_id(),
                "code": code,
                "isTrustedDevice": trust_device,
                "accessToken": access_token,
                "stay": 1,
                "deviceName": "SyncTV",
                "deviceType": "Server",
                "did": did,
                "req": "user.2fa.loginVerify",
                "si": session.session_id,
            });
            session
                .send_json(cipher.encrypted_request(
                    &session.public_key,
                    serde_json::to_string(&verify)?.as_bytes(),
                )?)
                .await?;
            response = session.receive_json().await?;
        }
        ensure_success(&response, "FNOS login failed")?;
        let token = required_string(&response, "token")?;
        let encrypted_secret = required_string(&response, "secret")?;
        Ok(FnosLogin::Authenticated(FnosCredential {
            username: username.to_string(),
            password: password.to_string(),
            token,
            long_token: optional_string(&response, "longToken"),
            secret: cipher.decrypt_secret(&encrypted_secret)?,
        }))
    }

    pub async fn list(
        &self,
        endpoints: &FnosEndpoints,
        credential: &FnosCredential,
        path: Option<&str>,
    ) -> Result<FnosFileList, ProviderClientError> {
        let mut session = self.authenticated_session(endpoints, credential).await?;
        let mut payload = Map::new();
        if let Some(path) = path.filter(|path| !path.is_empty()) {
            payload.insert("path".to_string(), Value::String(path.to_string()));
        }
        let response = session
            .request("file.ls", Value::Object(payload), &credential.secret)
            .await?;
        let response: FileListResponse = serde_json::from_value(response)?;
        if response
            .result
            .as_deref()
            .is_some_and(|result| result != "succ")
        {
            return Err(ProviderClientError::Api {
                code: response.errno.unwrap_or(-1),
                message: response
                    .msg
                    .or(response.errmsg)
                    .unwrap_or_else(|| "FNOS file listing failed".to_string()),
            });
        }
        let parent = path.unwrap_or_default().trim_end_matches('/');
        Ok(FnosFileList {
            files: response
                .files
                .into_iter()
                .filter(|file| !file.name.is_empty())
                .map(|file| map_file(parent, file))
                .collect(),
            revision: response.uver,
        })
    }

    pub async fn webdav_config(
        &self,
        endpoints: &FnosEndpoints,
        credential: &FnosCredential,
    ) -> Result<FnosWebDavConfig, ProviderClientError> {
        if let Some(endpoint) = &endpoints.webdav {
            return Ok(FnosWebDavConfig {
                enabled: true,
                endpoint: Some(normalize_webdav_endpoint(endpoint)?),
                root: "/".to_string(),
            });
        }
        let mut session = self.authenticated_session(endpoints, credential).await?;
        let response = session
            .request(
                "appcgi.share.webdav.opt",
                Value::Object(Map::new()),
                &credential.secret,
            )
            .await?;
        ensure_success(&response, "FNOS WebDAV configuration request failed")?;
        Ok(parse_webdav_config(&response, endpoints))
    }

    pub fn webdav_file_url(
        config: &FnosWebDavConfig,
        path: &str,
    ) -> Result<String, ProviderClientError> {
        if !config.enabled {
            return Err(ProviderClientError::InvalidConfig(
                "FNOS WebDAV service is disabled".to_string(),
            ));
        }
        let endpoint = config.endpoint.as_deref().ok_or_else(|| {
            ProviderClientError::InvalidConfig(
                "FNOS WebDAV endpoint could not be discovered; configure it explicitly".to_string(),
            )
        })?;
        let mut url = Url::parse(endpoint)
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let segments = config
            .root
            .split('/')
            .chain(path.split('/'))
            .filter(|segment| !segment.is_empty());
        url.path_segments_mut()
            .map_err(|()| {
                ProviderClientError::InvalidConfig("invalid FNOS WebDAV endpoint".to_string())
            })?
            .clear()
            .extend(segments);
        Ok(url.to_string())
    }

    async fn authenticated_session(
        &self,
        endpoints: &FnosEndpoints,
        credential: &FnosCredential,
    ) -> Result<FnosSession, ProviderClientError> {
        let mut session = self.connect(endpoints).await?;
        let mut response = session
            .request(
                "user.authToken",
                serde_json::json!({"main": true, "token": credential.token, "si": session.session_id}),
                &credential.secret,
            )
            .await?;
        if response.get("errno").and_then(Value::as_i64) == Some(135_168) {
            let long_token = credential.long_token.as_deref().ok_or_else(|| {
                ProviderClientError::InvalidConfig(
                    "FNOS short token expired and no long token is available".to_string(),
                )
            })?;
            response = session
                .request(
                    "user.tokenLogin",
                    serde_json::json!({
                        "deviceType": "Server",
                        "deviceName": "SyncTV",
                        "did": device_id(),
                        "si": session.session_id,
                        "token": long_token,
                    }),
                    &credential.secret,
                )
                .await?;
        }
        ensure_success(&response, "FNOS token authentication failed")?;
        Ok(session)
    }

    async fn connect(&self, endpoints: &FnosEndpoints) -> Result<FnosSession, ProviderClientError> {
        let url = Url::parse(&endpoints.websocket)
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let host = url.host_str().ok_or_else(|| {
            ProviderClientError::InvalidConfig("FNOS endpoint host is required".to_string())
        })?;
        let port = url.port_or_known_default().ok_or_else(|| {
            ProviderClientError::InvalidConfig("FNOS endpoint port is required".to_string())
        })?;
        self.ssrf_guard
            .validate_url_target_with_default_port(
                host,
                port,
                if url.scheme() == "wss" { 443 } else { 80 },
            )
            .map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_RESPONSE_SIZE))
            .max_frame_size(Some(MAX_RESPONSE_SIZE));
        let (socket, _) = tokio::time::timeout(
            self.timeout,
            tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), true),
        )
        .await
        .map_err(|_| ProviderClientError::Network("FNOS connection timed out".to_string()))?
        .map_err(|error| ProviderClientError::Network(error.to_string()))?;
        let mut session = FnosSession {
            socket,
            session_id: String::new(),
            public_key: String::new(),
            server_info: FnosServerInfo {
                host_name: String::new(),
                version: None,
            },
            timeout: self.timeout,
        };
        let crypto_reqid = request_id();
        session
            .send_json(serde_json::json!({
                "reqid": crypto_reqid,
                "req": "util.crypto.getRSAPub"
            }))
            .await?;
        let crypto = session.receive_json().await?;
        session.public_key = required_string(&crypto, "pub")?;
        session.session_id = required_string(&crypto, "si")?;
        let info_reqid = request_id();
        session
            .send_json(serde_json::json!({
                "reqid": info_reqid,
                "req": "appcgi.sysinfo.getHostName"
            }))
            .await?;
        let info = session.receive_json().await?;
        ensure_success(&info, "FNOS server information request failed")?;
        session.server_info = FnosServerInfo {
            host_name: info
                .pointer("/data/hostName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            version: info
                .pointer("/data/trimVersion")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        Ok(session)
    }
}

struct FnosSession {
    socket: Socket,
    session_id: String,
    public_key: String,
    server_info: FnosServerInfo,
    timeout: Duration,
}

impl FnosSession {
    async fn request(
        &mut self,
        request: &str,
        payload: Value,
        secret: &str,
    ) -> Result<Value, ProviderClientError> {
        let reqid = request_id();
        let mut payload = payload.as_object().cloned().unwrap_or_default();
        payload.insert("req".to_string(), Value::String(request.to_string()));
        payload.insert("reqid".to_string(), Value::String(reqid.clone()));
        let json = serde_json::to_string(&payload)?;
        self.socket
            .send(Message::Text(sign_request(secret, &json)?.into()))
            .await
            .map_err(|error| ProviderClientError::Network(error.to_string()))?;
        loop {
            let response = self.receive_json().await?;
            if response.get("reqid").and_then(Value::as_str) == Some(reqid.as_str()) {
                return Ok(response);
            }
        }
    }

    async fn send_json(&mut self, value: Value) -> Result<(), ProviderClientError> {
        let text = serde_json::to_string(&value)?;
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|error| ProviderClientError::Network(error.to_string()))
    }

    async fn receive_json(&mut self) -> Result<Value, ProviderClientError> {
        loop {
            let message = tokio::time::timeout(self.timeout, self.socket.next())
                .await
                .map_err(|_| ProviderClientError::Network("FNOS request timed out".to_string()))?
                .ok_or_else(|| ProviderClientError::Network("FNOS connection closed".to_string()))?
                .map_err(|error| ProviderClientError::Network(error.to_string()))?;
            match message {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
                Message::Ping(bytes) => {
                    self.socket
                        .send(Message::Pong(bytes))
                        .await
                        .map_err(|error| ProviderClientError::Network(error.to_string()))?;
                }
                Message::Close(_) => {
                    return Err(ProviderClientError::Network(
                        "FNOS connection closed".to_string(),
                    ));
                }
                _ => {}
            }
        }
    }
}

fn map_file(parent: &str, file: RawFile) -> FnosFile {
    let path = if parent.is_empty() {
        file.name.clone()
    } else {
        format!("{parent}/{}", file.name)
    };
    FnosFile {
        name: file.name,
        path,
        size: file.size,
        modified_at: file.mtim,
        created_at: file.btim,
        is_dir: file.dir == Some(1),
        storage_id: file.v,
    }
}

fn is_twofa_challenge(value: &Value) -> bool {
    value.get("result").and_then(Value::as_str) == Some("succ")
        && value.get("accessToken").and_then(Value::as_str).is_some()
        && value.get("token").is_none()
}

fn ensure_success(value: &Value, fallback: &str) -> Result<(), ProviderClientError> {
    if value.get("result").and_then(Value::as_str) == Some("fail")
        || value
            .get("errno")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
    {
        return Err(ProviderClientError::Api {
            code: value.get("errno").and_then(Value::as_i64).unwrap_or(-1),
            message: value
                .get("msg")
                .or_else(|| value.get("errmsg"))
                .and_then(Value::as_str)
                .unwrap_or(fallback)
                .to_string(),
        });
    }
    Ok(())
}

fn required_string(value: &Value, key: &str) -> Result<String, ProviderClientError> {
    optional_string(value, key)
        .ok_or_else(|| ProviderClientError::Parse(format!("FNOS response field {key} is missing")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn request_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{millis}{}", &Uuid::new_v4().simple().to_string()[..12])
}

fn device_id() -> String {
    format!("synctv-{}", Uuid::new_v4().simple())
}

fn normalize_webdav_endpoint(value: &str) -> Result<String, ProviderClientError> {
    let mut url =
        Url::parse(value).map_err(|error| ProviderClientError::InvalidConfig(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ProviderClientError::InvalidConfig(
            "FNOS WebDAV endpoint must be an HTTP(S) URL".to_string(),
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn parse_webdav_config(value: &Value, endpoints: &FnosEndpoints) -> FnosWebDavConfig {
    let data = value.get("data").unwrap_or(value);
    let enabled = find_bool(data, &["webdavEnable", "enable", "enabled"]).unwrap_or(false);
    let port = find_u16(data, &["svcPort", "port", "webdavPort"]);
    let secure = find_bool(data, &["httpsEnable", "tls", "ssl"]).unwrap_or(false);
    let endpoint = Url::parse(&endpoints.websocket).ok().and_then(|url| {
        let host = url.host_str()?;
        let port = port?;
        Some(format!(
            "{}://{}:{port}",
            if secure { "https" } else { "http" },
            host
        ))
    });
    let root = find_string(data, &["root", "path", "mountPath"]).unwrap_or_else(|| "/".to_string());
    FnosWebDavConfig {
        enabled,
        endpoint,
        root,
    }
}

fn find_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    find_value(value, keys).and_then(|value| {
        value
            .as_bool()
            .or_else(|| value.as_i64().map(|value| value != 0))
    })
}

fn find_u16(value: &Value, keys: &[&str]) -> Option<u16> {
    find_value(value, keys)
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    find_value(value, keys)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn find_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(map) => keys
            .iter()
            .find_map(|key| map.get(*key))
            .or_else(|| map.values().find_map(|value| find_value(value, keys))),
        Value::Array(values) => values.iter().find_map(|value| find_value(value, keys)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints_and_file_entries() {
        let endpoints = FnosEndpoints::parse("https://nas.example:5667/")
            .expect("test operation should succeed");
        assert_eq!(
            endpoints.websocket,
            "wss://nas.example:5667/websocket?type=main"
        );
        let file = map_file(
            "vol1/1000/Videos",
            RawFile {
                name: "movie.mp4".to_string(),
                size: Some(42),
                mtim: Some(1),
                btim: Some(2),
                dir: None,
                v: None,
            },
        );
        assert_eq!(file.path, "vol1/1000/Videos/movie.mp4");
        assert!(!file.is_dir);
    }

    #[test]
    fn discovers_nested_webdav_configuration() {
        let endpoints =
            FnosEndpoints::parse("ws://nas.example:5666").expect("test operation should succeed");
        let config = parse_webdav_config(
            &serde_json::json!({
                "result": "succ",
                "data": {"option": {"webdavEnable": true, "svcPort": 5005}}
            }),
            &endpoints,
        );
        assert!(config.enabled);
        assert_eq!(config.endpoint.as_deref(), Some("http://nas.example:5005"));
    }
}
