//! Bilibili HTTP Client
#![allow(clippy::must_use_candidate)]

use std::collections::HashMap;
#[cfg(not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")))]
use std::future::ready;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::{RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use futures_util::{SinkExt, StreamExt};
use md5::{Digest, Md5};
use regex::Regex;
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize};
use synctv_common::ssrf::SsrfGuard;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tokio::net::TcpStream;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use tokio::task::JoinHandle;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tokio_tungstenite::client_async_tls_with_config;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots", test))]
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use super::types;
use crate::error::with_retry;
use crate::error::{check_response, json_with_limit, ProviderClientError as BilibiliError};

static RE_BVID: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"BV[a-zA-Z0-9]+"));
static RE_EPID: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| Regex::new(r"ep(\d+)"));

use crate::PROVIDER_USER_AGENT as USER_AGENT;
const REFERER: &str = "https://www.bilibili.com";
const LIVE_ORIGIN: &str = "https://live.bilibili.com";
const BILIBILI_SHORT_LINK_MAX_REDIRECTS: usize = 5;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots", test))]
const LIVE_DANMAKU_WS_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots", test))]
const LIVE_DANMAKU_WS_MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

fn shared_client() -> Result<Client, BilibiliError> {
    crate::provider_http_client_builder(synctv_common::ssrf::SsrfGuard::strict_policy())
        .user_agent(USER_AGENT)
        .build()
        .map_err(|err| BilibiliError::Network(err.to_string()))
}

fn required_first_segment_url(
    segments: &[VideoSegment],
    endpoint: &'static str,
) -> Result<String, BilibiliError> {
    segments
        .first()
        .map(|segment| segment.url.clone())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| BilibiliError::Parse(format!("{endpoint} response missing media URL")))
}

fn video_segments_from_durls(durls: &[types::DurlInfo]) -> Vec<VideoSegment> {
    durls
        .iter()
        .map(|durl| VideoSegment {
            url: durl.url.clone(),
            size: durl.size,
            duration_millis: durl.length,
            backup_urls: durl.backup_url.clone().unwrap_or_default(),
        })
        .collect()
}

fn quality_to_u32(quality: u64, endpoint: &'static str) -> Result<u32, BilibiliError> {
    u32::try_from(quality)
        .map_err(|_| BilibiliError::Parse(format!("{endpoint} quality {quality} exceeds u32")))
}

fn normalized_subtitle_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        Some(url.to_string())
    } else {
        url.strip_prefix("//").map(|url| format!("https://{url}"))
    }
}

fn subtitle_player_to_map(items: Vec<types::SubtitleItem>) -> HashMap<String, String> {
    items
        .into_iter()
        .filter_map(|item| {
            normalized_subtitle_url(&item.subtitle_url).map(|url| (item.lan_doc, url))
        })
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

fn parse_bilibili_live_started_at(value: &str) -> Option<i64> {
    let local = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    FixedOffset::east_opt(8 * 60 * 60)?
        .from_local_datetime(&local)
        .single()
        .map(|started_at| started_at.timestamp())
}

fn parse_live_stream_expires_at(value: &str) -> Option<i64> {
    url::Url::parse(value)
        .ok()?
        .query_pairs()
        .filter(|(key, _)| key == "expires")
        .find_map(|(_, value)| {
            value
                .parse::<i64>()
                .ok()
                .filter(|expires_at| *expires_at > 0)
        })
}

const fn live_stream_format_rank(format: &str) -> u8 {
    match format.as_bytes() {
        b"ts" => 0,
        b"fmp4" => 1,
        _ => 2,
    }
}

const fn live_stream_codec_rank(codec: &str) -> u8 {
    match codec.as_bytes() {
        b"avc" => 0,
        b"hevc" => 1,
        _ => 2,
    }
}

fn bilibili_api_error(code: i64, context: &'static str) -> BilibiliError {
    let message = match code {
        -101 => "Bilibili authentication is required".to_string(),
        -352 => "Bilibili rejected the request signature".to_string(),
        -401 => "Bilibili request is unauthorized".to_string(),
        87007 => "Bilibili SMS login requires captcha verification".to_string(),
        86038 => "Bilibili QR code has expired".to_string(),
        86090 => "Bilibili QR code has been scanned".to_string(),
        86101 => "Bilibili QR code is waiting to be scanned".to_string(),
        _ => format!("Bilibili {context} API returned code {code}"),
    };
    BilibiliError::Api { code, message }
}

fn unix_timestamp_secs() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(err) => {
            tracing::warn!(%err, "system clock is before Unix epoch");
            0
        }
    }
}

fn parse_colon_duration(value: &str) -> u64 {
    value.split(':').fold(0_u64, |duration, component| {
        duration
            .saturating_mul(60)
            .saturating_add(component.parse::<u64>().unwrap_or(0))
    })
}

#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots", test))]
fn live_danmaku_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(LIVE_DANMAKU_WS_MAX_MESSAGE_SIZE))
        .max_frame_size(Some(LIVE_DANMAKU_WS_MAX_FRAME_SIZE))
}

#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
async fn connect_live_danmaku_websocket(
    ws_url: &str,
    socket: TcpStream,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    BilibiliError,
> {
    client_async_tls_with_config(ws_url, socket, Some(live_danmaku_websocket_config()), None)
        .await
        .map(|(stream, _response)| stream)
        .map_err(|e| BilibiliError::Network(format!("Failed to connect to danmaku WebSocket: {e}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilibiliEndpoints {
    pub web_base: String,
    pub api_base: String,
    pub passport_base: String,
    pub live_api_base: String,
}

impl Default for BilibiliEndpoints {
    fn default() -> Self {
        Self {
            web_base: "https://www.bilibili.com".to_string(),
            api_base: "https://api.bilibili.com".to_string(),
            passport_base: "https://passport.bilibili.com".to_string(),
            live_api_base: "https://api.live.bilibili.com".to_string(),
        }
    }
}

impl BilibiliEndpoints {
    fn join(base: &str, path: &str) -> String {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn api_url(&self, path: &str) -> String {
        Self::join(&self.api_base, path)
    }

    fn passport_url(&self, path: &str) -> String {
        Self::join(&self.passport_base, path)
    }

    fn live_api_url(&self, path: &str) -> String {
        Self::join(&self.live_api_base, path)
    }
}

// WBI Signing

/// Predefined character-index table for generating the WBI mixin key.
/// This table is derived from Bilibili's frontend JavaScript and maps
/// positions in the concatenated `img_key + sub_key` string to positions
/// in the resulting mixin key.
const MIXIN_KEY_ENC_TAB: [u8; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// Cached WBI keys with expiration timestamp.
struct WbiKeys {
    mixin_key: String,
    expires_at: std::time::Instant,
    generation: u64,
}

impl WbiKeys {
    /// Check if the cached key is still valid
    fn is_valid(&self) -> bool {
        std::time::Instant::now() < self.expires_at
    }
}

/// Shared WBI refresh state.
///
/// This is scoped to a service/client instance and injected explicitly instead of
/// living in process-global statics. That avoids cross-instance interference in
/// production and keeps tests isolated.
///
/// Concurrency is handled with a single-flight pattern: the `refresh` mutex is held
/// only for the duration of the network fetch, so concurrent callers queue on the
/// mutex rather than spinning on a notification. The first waiter to acquire the lock
/// re-checks the cache (filled by the task that just released the lock) and returns
/// immediately on a hit, so at most one fetch runs per expiry.
pub(crate) struct WbiState {
    key_cache: std::sync::RwLock<Option<WbiKeys>>,
    key_generation: AtomicU64,
    refresh: tokio::sync::Mutex<()>,
    consecutive_failures: AtomicUsize,
    #[cfg(test)]
    api_call_count: AtomicUsize,
}

impl Default for WbiState {
    fn default() -> Self {
        Self {
            key_cache: std::sync::RwLock::new(None),
            key_generation: AtomicU64::new(0),
            refresh: tokio::sync::Mutex::new(()),
            consecutive_failures: AtomicUsize::new(0),
            #[cfg(test)]
            api_call_count: AtomicUsize::new(0),
        }
    }
}

/// Maximum number of consecutive refresh failures before we give up and return an error.
/// This prevents infinite waiting when the WBI API is persistently unavailable.
const WBI_MAX_CONSECUTIVE_FAILURES: usize = 3;

/// WBI key cache TTL (refresh keys every 30 minutes).
const WBI_KEY_TTL: Duration = Duration::from_mins(30);

impl WbiState {
    fn read_key_cache(&self) -> RwLockReadGuard<'_, Option<WbiKeys>> {
        match self.key_cache.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("WBI key cache read lock was poisoned; recovering cached value");
                poisoned.into_inner()
            }
        }
    }

    fn write_key_cache(&self) -> RwLockWriteGuard<'_, Option<WbiKeys>> {
        match self.key_cache.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("WBI key cache write lock was poisoned; recovering cached value");
                poisoned.into_inner()
            }
        }
    }

    pub(crate) fn get_valid_wbi_key(&self) -> Option<String> {
        self.get_valid_wbi_key_newer_than(0)
    }

    fn get_valid_wbi_key_newer_than(&self, min_generation: u64) -> Option<String> {
        let guard = self.read_key_cache();
        guard
            .as_ref()
            .filter(|k| k.is_valid() && k.generation > min_generation)
            .map(|k| k.mixin_key.clone())
    }

    pub(crate) fn set_wbi_key(&self, mixin_key: String) {
        let mut guard = self.write_key_cache();
        let generation = self.key_generation.fetch_add(1, Ordering::AcqRel) + 1;
        *guard = Some(WbiKeys {
            mixin_key,
            expires_at: std::time::Instant::now() + WBI_KEY_TTL,
            generation,
        });
    }

    fn current_key_generation(&self) -> u64 {
        let guard = self.read_key_cache();
        guard.as_ref().map_or(0, |key| key.generation)
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Release);
    }

    fn record_failure(&self) {
        self.consecutive_failures.fetch_add(1, Ordering::AcqRel);
    }

    fn has_exceeded_max_failures(&self) -> bool {
        self.consecutive_failures.load(Ordering::Acquire) >= WBI_MAX_CONSECUTIVE_FAILURES
    }

    #[cfg(test)]
    pub(crate) fn reset_for_tests(&self) {
        *self.write_key_cache() = None;
        self.key_generation.store(0, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
        self.api_call_count.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn api_call_count(&self) -> usize {
        self.api_call_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn record_failure_for_tests(&self) {
        self.record_failure();
    }

    #[cfg(test)]
    pub(crate) fn has_exceeded_max_failures_for_tests(&self) -> bool {
        self.has_exceeded_max_failures()
    }

    #[cfg(test)]
    pub(crate) async fn acquire_refresh_for_tests(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.refresh.lock().await
    }
}

fn wbi_refresh_unavailable_error() -> BilibiliError {
    BilibiliError::Parse("WBI key refresh unavailable: too many consecutive failures".to_string())
}

/// Generate the mixin key from `img_key` and `sub_key` using the encoding table.
///
/// 1. Concatenate `img_key + sub_key` (should be 64 chars if both are 32 chars).
/// 2. For each index in `MIXIN_KEY_ENC_TAB`, pick the character at that position.
/// 3. Take the first 32 characters of the result.
fn gen_mixin_key(img_key: &str, sub_key: &str) -> String {
    let combined: Vec<u8> = format!("{img_key}{sub_key}").into_bytes();
    if combined.is_empty() {
        return String::new();
    }
    MIXIN_KEY_ENC_TAB
        .iter()
        .filter_map(|&idx| combined.get(idx as usize).copied())
        .take(32)
        .map(|b| b as char)
        .collect()
}

/// Extract the key portion from a WBI image URL.
///
/// The URL looks like `https://i0.hdslb.com/bfs/wbi/<key>.png`.
/// We extract the filename stem (without extension).
fn extract_key_from_url(url: &str) -> Option<String> {
    url.rsplit('/')
        .next()
        .and_then(|filename| filename.rsplit_once('.'))
        .map(|(stem, _ext)| stem.to_string())
}

/// Sign query parameters using Bilibili's WBI algorithm.
///
/// 1. Add `wts` (current Unix timestamp) to params.
/// 2. Sort params by key lexicographically.
/// 3. Filter values: remove characters `!'()*` from each value.
/// 4. URL-encode and concatenate as `key=value&key=value...`.
/// 5. Append the `mixin_key`.
/// 6. Compute MD5 hash → `w_rid`.
/// 7. Return the signed query string with `w_rid` and `wts` appended.
fn wbi_sign(params: &[(&str, String)], mixin_key: &str) -> Vec<(String, String)> {
    let wts = unix_timestamp_secs().to_string();

    // Collect all params plus wts
    let mut all_params: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    all_params.push(("wts".to_string(), wts));

    // Sort by key
    all_params.sort_by(|a, b| a.0.cmp(&b.0));

    // Filter special characters from values: remove !'()*
    for param in &mut all_params {
        param.1 = param.1.chars().filter(|c| !"!'()*".contains(*c)).collect();
    }

    // Build query string for hashing (URL-encoded, matching Go's url.Values.Encode())
    let query_str: String = {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &all_params {
            ser.append_pair(k, v);
        }
        ser.finish()
    };

    // Compute MD5 hash of query_string + mixin_key
    let to_hash = format!("{query_str}{mixin_key}");
    let mut hasher = Md5::new();
    hasher.update(to_hash.as_bytes());
    let w_rid = hex::encode(hasher.finalize());

    // Add w_rid to the params
    all_params.push(("w_rid".to_string(), w_rid));

    all_params
}

/// Sanitize a single cookie key-value pair by stripping CR/LF characters
/// to prevent HTTP header injection.
fn sanitize_cookie_pair(key: &str, value: &str) -> String {
    let safe_k: String = key.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    let safe_v: String = value.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    format!("{safe_k}={safe_v}")
}

/// Bilibili HTTP Client
pub struct BilibiliClient {
    client: Client,
    short_link_client: Client,
    cookies: Option<HashMap<String, String>>,
    live_danmaku_device_cookies: Arc<OnceCell<HashMap<String, String>>>,
    wbi_state: Arc<WbiState>,
    endpoints: BilibiliEndpoints,
    #[allow(dead_code)]
    ssrf_guard: SsrfGuard,
}

impl BilibiliClient {
    /// Create a new Bilibili client (reuses shared connection pool and rate limiter).
    pub fn new() -> Result<Self, BilibiliError> {
        Self::new_with_wbi_state(Arc::new(WbiState::default()))
    }

    pub(crate) fn new_with_wbi_state(wbi_state: Arc<WbiState>) -> Result<Self, BilibiliError> {
        let client = shared_client()?;
        Ok(Self::new_with_transport(
            client.clone(),
            client,
            BilibiliEndpoints::default(),
            wbi_state,
            SsrfGuard::strict_policy(),
        ))
    }

    pub(crate) fn new_with_transport(
        client: Client,
        short_link_client: Client,
        endpoints: BilibiliEndpoints,
        wbi_state: Arc<WbiState>,
        ssrf_guard: SsrfGuard,
    ) -> Self {
        Self {
            client,
            short_link_client,
            cookies: None,
            live_danmaku_device_cookies: Arc::new(OnceCell::new()),
            wbi_state,
            endpoints,
            ssrf_guard,
        }
    }

    pub fn new_with_transport_defaults(
        client: Client,
        endpoints: BilibiliEndpoints,
    ) -> Result<Self, BilibiliError> {
        let short_link_client = crate::provider_http_client_builder(SsrfGuard::strict_policy())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|err| BilibiliError::Network(err.to_string()))?;
        Ok(Self::new_with_transport(
            client,
            short_link_client,
            endpoints,
            Arc::new(WbiState::default()),
            SsrfGuard::strict_policy(),
        ))
    }

    pub fn new_with_short_link_transport_defaults(
        client: Client,
        short_link_client: Client,
        endpoints: BilibiliEndpoints,
    ) -> Self {
        Self::new_with_transport(
            client,
            short_link_client,
            endpoints,
            Arc::new(WbiState::default()),
            SsrfGuard::strict_policy(),
        )
    }

    /// Create a new Bilibili client with cookies (reuses shared connection pool and rate limiter).
    pub fn with_cookies(cookies: HashMap<String, String>) -> Result<Self, BilibiliError> {
        Self::with_cookies_and_wbi_state(cookies, Arc::new(WbiState::default()))
    }

    pub(crate) fn with_cookies_and_wbi_state(
        cookies: HashMap<String, String>,
        wbi_state: Arc<WbiState>,
    ) -> Result<Self, BilibiliError> {
        let client = shared_client()?;
        Ok(Self::with_cookies_and_transport(
            cookies,
            client.clone(),
            client,
            BilibiliEndpoints::default(),
            wbi_state,
            SsrfGuard::strict_policy(),
        ))
    }

    pub(crate) fn with_cookies_and_transport(
        cookies: HashMap<String, String>,
        client: Client,
        short_link_client: Client,
        endpoints: BilibiliEndpoints,
        wbi_state: Arc<WbiState>,
        ssrf_guard: SsrfGuard,
    ) -> Self {
        Self {
            client,
            short_link_client,
            cookies: Some(cookies),
            live_danmaku_device_cookies: Arc::new(OnceCell::new()),
            wbi_state,
            endpoints,
            ssrf_guard,
        }
    }

    pub(crate) fn with_live_danmaku_device_cookies(
        mut self,
        live_danmaku_device_cookies: Arc<OnceCell<HashMap<String, String>>>,
    ) -> Self {
        self.live_danmaku_device_cookies = live_danmaku_device_cookies;
        self
    }

    pub fn with_cookies_and_transport_defaults(
        cookies: HashMap<String, String>,
        client: Client,
        endpoints: BilibiliEndpoints,
    ) -> Result<Self, BilibiliError> {
        let short_link_client = crate::provider_http_client_builder(SsrfGuard::strict_policy())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|err| BilibiliError::Network(err.to_string()))?;
        Ok(Self::with_cookies_and_transport(
            cookies,
            client,
            short_link_client,
            endpoints,
            Arc::new(WbiState::default()),
            SsrfGuard::strict_policy(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn shared_wbi_state(&self) -> Arc<WbiState> {
        self.wbi_state.clone()
    }

    #[cfg(test)]
    pub(crate) async fn get_wbi_mixin_key_for_tests(
        &self,
        force_refresh: bool,
    ) -> Result<String, BilibiliError> {
        self.get_wbi_mixin_key_internal(force_refresh).await
    }

    /// Get WBI mixin key, fetching and caching it if necessary.
    /// Internal method with optional force refresh.
    ///
    /// Fetches from Bilibili's nav API and caches in memory for 30 minutes.
    /// Uses a single-flight refresh (one network fetch per expiry) to prevent
    /// thundering herd when the cache expires: concurrent callers queue on a mutex,
    /// and every waiter re-checks the cache after acquiring it.
    ///
    /// # Failure Handling
    /// - Tracks consecutive failures and returns an error after the max is exceeded
    /// - The fetch itself is bounded by the HTTP client's request timeout
    async fn get_wbi_mixin_key_internal(
        &self,
        force_refresh: bool,
    ) -> Result<String, BilibiliError> {
        let min_cache_generation = if force_refresh {
            self.wbi_state.current_key_generation()
        } else {
            0
        };

        // Fast path: serve a valid cached key without acquiring the refresh lock.
        if !force_refresh {
            if let Some(key) = self.wbi_state.get_valid_wbi_key() {
                self.wbi_state.record_success();
                return Ok(key);
            }
        }

        if self.wbi_state.has_exceeded_max_failures() {
            return Err(wbi_refresh_unavailable_error());
        }

        // Single-flight: only the lock holder fetches. Others block here, then find
        // the freshly-cached key on the re-check below.
        let _refresh_guard = self.wbi_state.refresh.lock().await;

        // Re-check the cache: a previous lock holder may have just refreshed it.
        let refreshed_key = if force_refresh {
            self.wbi_state
                .get_valid_wbi_key_newer_than(min_cache_generation)
        } else {
            self.wbi_state.get_valid_wbi_key()
        };
        if let Some(key) = refreshed_key {
            self.wbi_state.record_success();
            return Ok(key);
        }

        if self.wbi_state.has_exceeded_max_failures() {
            return Err(wbi_refresh_unavailable_error());
        }

        match self.fetch_and_cache_wbi_key().await {
            Ok(key) => {
                self.wbi_state.record_success();
                Ok(key)
            }
            Err(error) => {
                self.wbi_state.record_failure();
                Err(error)
            }
        }
    }

    /// Fetch WBI key from Bilibili API and cache it.
    async fn fetch_and_cache_wbi_key(&self) -> Result<String, BilibiliError> {
        #[cfg(test)]
        self.wbi_state
            .api_call_count
            .fetch_add(1, Ordering::Relaxed);

        let url = self.endpoints.api_url("/x/web-interface/nav");
        let req = self.add_cookies(self.client.get(url).header("Referer", REFERER));
        let resp = check_response(req.send().await?).await?;
        let json: types::NavResp = json_with_limit(resp).await?;

        let wbi_img = match json.data.and_then(|data| data.wbi_img) {
            Some(wbi_img) => wbi_img,
            None if json.code != 0 => {
                return Err(bilibili_api_error(i64::from(json.code), "nav"));
            }
            None => {
                return Err(BilibiliError::Parse(
                    "Missing wbi_img in nav response".to_string(),
                ));
            }
        };

        if json.code != 0 {
            tracing::debug!(
                code = json.code,
                "Using WBI keys from anonymous Bilibili nav response"
            );
        }

        if json.code != 0 && wbi_img.img_url.is_empty() && wbi_img.sub_url.is_empty() {
            return Err(bilibili_api_error(i64::from(json.code), "nav"));
        }

        let img_key = extract_key_from_url(&wbi_img.img_url).ok_or_else(|| {
            BilibiliError::Parse(format!(
                "Cannot extract img_key from URL: {}",
                wbi_img.img_url
            ))
        })?;
        let sub_key = extract_key_from_url(&wbi_img.sub_url).ok_or_else(|| {
            BilibiliError::Parse(format!(
                "Cannot extract sub_key from URL: {}",
                wbi_img.sub_url
            ))
        })?;

        let mixin_key = gen_mixin_key(&img_key, &sub_key);
        if mixin_key.is_empty() {
            return Err(BilibiliError::Parse(
                "Generated empty mixin key".to_string(),
            ));
        }

        // Store in cache with TTL
        self.wbi_state.set_wbi_key(mixin_key.clone());

        Ok(mixin_key)
    }

    /// Detect if WBI signature is stale based on error response
    const fn is_wbi_stale_error(error: &BilibiliError) -> bool {
        match error {
            BilibiliError::Api { code, .. } => {
                // -352: signature error, -401: unauthorized (could be stale key)
                *code == -352 || *code == -401
            }
            _ => false,
        }
    }

    /// Build a sanitized cookie header string from stored cookies.
    /// Returns `None` if no cookies are configured.
    /// This is useful when `self` cannot be borrowed inside a closure (e.g. `with_retry`).
    fn build_cookie_header(&self) -> Option<String> {
        self.cookies
            .as_ref()
            .and_then(Self::cookie_header_from_pairs)
    }

    fn cookie_header_from_pairs(cookies: &HashMap<String, String>) -> Option<String> {
        if cookies.is_empty() {
            return None;
        }

        let mut cookies = cookies.iter().collect::<Vec<_>>();
        cookies.sort_unstable_by_key(|(key, _)| *key);
        Some(
            cookies
                .into_iter()
                .map(|(key, value)| sanitize_cookie_pair(key, value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    async fn live_danmaku_cookie_header(&self) -> Result<Option<String>, BilibiliError> {
        let mut cookies = self.cookies.clone().unwrap_or_default();
        if !cookies.contains_key("buvid3") {
            let device_cookies = self
                .live_danmaku_device_cookies
                .get_or_try_init(|| self.get_buvid_cookies())
                .await?;
            cookies.extend(
                device_cookies
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        Ok(Self::cookie_header_from_pairs(&cookies))
    }

    /// Add cookies to request.
    /// Cookie values are sanitized to prevent header injection via \r\n.
    fn add_cookies(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.build_cookie_header() {
            Some(cookie_str) => req.header("Cookie", cookie_str),
            None => req,
        }
    }

    async fn get_api<T>(
        &self,
        path: &str,
        query: Vec<(String, String)>,
    ) -> Result<types::ApiEnvelope<T>, BilibiliError>
    where
        T: DeserializeOwned + Send,
    {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let endpoint = self.endpoints.api_url(path);
        let referer = self.endpoints.web_base.clone();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let endpoint = endpoint.clone();
            let query = query.clone();
            let referer = referer.clone();
            async move {
                let mut request = client
                    .get(&endpoint)
                    .query(&query)
                    .header("Referer", referer.as_str());
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                json_with_limit(response).await
            }
        })
        .await
    }

    async fn get_wbi_api<T>(
        &self,
        path: &str,
        query: Vec<(&str, String)>,
    ) -> Result<types::ApiEnvelope<T>, BilibiliError>
    where
        T: DeserializeOwned + Send,
    {
        let first = self.get_wbi_api_once(path, &query, false).await;
        if matches!(&first, Err(error) if Self::is_wbi_stale_error(error)) {
            return self.get_wbi_api_once(path, &query, true).await;
        }
        first
    }

    async fn get_wbi_api_once<T>(
        &self,
        path: &str,
        query: &[(&str, String)],
        force_key_refresh: bool,
    ) -> Result<types::ApiEnvelope<T>, BilibiliError>
    where
        T: DeserializeOwned + Send,
    {
        let mixin_key = self.get_wbi_mixin_key_internal(force_key_refresh).await?;
        let signed = wbi_sign(query, &mixin_key);
        let response = self.get_api(path, signed).await?;
        if response.code != 0 {
            return Err(bilibili_api_error(i64::from(response.code), "signed list"));
        }
        Ok(response)
    }

    async fn get_wbi_live_api<T>(
        &self,
        path: &str,
        query: Vec<(&str, String)>,
    ) -> Result<types::ApiEnvelope<T>, BilibiliError>
    where
        T: DeserializeOwned + Send,
    {
        let cookie_header = self.live_danmaku_cookie_header().await?;
        let first = self
            .get_wbi_live_api_once(path, &query, cookie_header.clone(), false)
            .await;
        if matches!(&first, Err(error) if Self::is_wbi_stale_error(error)) {
            return self
                .get_wbi_live_api_once(path, &query, cookie_header, true)
                .await;
        }
        first
    }

    async fn get_wbi_live_api_once<T>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cookie_header: Option<String>,
        force_key_refresh: bool,
    ) -> Result<types::ApiEnvelope<T>, BilibiliError>
    where
        T: DeserializeOwned + Send,
    {
        let mixin_key = self.get_wbi_mixin_key_internal(force_key_refresh).await?;
        let signed = wbi_sign(query, &mixin_key);
        let client = self.client.clone();
        let endpoint = self.endpoints.live_api_url(path);

        let response: types::ApiEnvelope<T> = with_retry(|| {
            let client = client.clone();
            let endpoint = endpoint.clone();
            let signed = signed.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut request = client
                    .get(endpoint)
                    .query(&signed)
                    .header("Referer", REFERER)
                    .header("Origin", LIVE_ORIGIN);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                json_with_limit(response).await
            }
        })
        .await?;

        if response.code != 0 {
            return Err(bilibili_api_error(i64::from(response.code), "live danmaku"));
        }
        Ok(response)
    }

    /// Generate QR code for login
    pub async fn new_qr_code(&self) -> Result<(String, String), BilibiliError> {
        #[derive(Deserialize)]
        struct QrCodeData {
            url: String,
            qrcode_key: String,
        }

        #[derive(Deserialize)]
        struct QrCodeResp {
            code: i32,
            data: Option<QrCodeData>,
        }

        let url = self
            .endpoints
            .passport_url("/x/passport-login/web/qrcode/generate");
        let referer = self.endpoints.passport_url("/login");
        let req = self.client.get(url).header("Referer", referer);

        let resp = check_response(req.send().await?).await?;
        let json: QrCodeResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(bilibili_api_error(i64::from(json.code), "QR code"));
        }

        let data = json
            .data
            .ok_or_else(|| BilibiliError::Parse("Missing QR code data".to_string()))?;
        Ok((data.url, data.qrcode_key))
    }

    /// Check QR code login status
    pub async fn login_with_qr_code(
        &self,
        key: &str,
    ) -> Result<(u32, Option<HashMap<String, String>>), BilibiliError> {
        #[derive(Deserialize)]
        struct LoginData {
            code: u32,
        }

        #[derive(Deserialize)]
        struct LoginResp {
            code: i32,
            data: Option<LoginData>,
        }

        let req = self
            .client
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
            .query(&[("qrcode_key", key)])
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = check_response(req.send().await?).await?;

        // Extract ALL relevant cookies (SESSDATA, bili_jct, DedeUserID, DedeUserID__ckMd5)
        let cookies = {
            let relevant: HashMap<String, String> = resp
                .cookies()
                .filter(|c| {
                    matches!(
                        c.name(),
                        "SESSDATA" | "bili_jct" | "DedeUserID" | "DedeUserID__ckMd5"
                    )
                })
                .map(|c| (c.name().to_string(), c.value().to_string()))
                .collect();
            if relevant.is_empty() {
                None
            } else {
                Some(relevant)
            }
        };

        let json: LoginResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(bilibili_api_error(i64::from(json.code), "QR login"));
        }

        let data = json
            .data
            .ok_or_else(|| BilibiliError::Parse("Missing login data".to_string()))?;

        // QR code status codes:
        // 0: success
        // 86038: expired
        // 86090: scanned
        // 86101: not scanned
        Ok((data.code, cookies))
    }

    /// Get new captcha for SMS login
    pub async fn new_captcha(&self) -> Result<(String, String, String), BilibiliError> {
        #[derive(Deserialize)]
        struct Geetest {
            challenge: String,
            gt: String,
        }

        #[derive(Deserialize)]
        struct CaptchaData {
            token: String,
            geetest: Geetest,
        }

        #[derive(Deserialize)]
        struct CaptchaResp {
            code: i32,
            data: Option<CaptchaData>,
        }

        let url = "https://passport.bilibili.com/x/passport-login/captcha";
        let req = self
            .client
            .get(url)
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = check_response(req.send().await?).await?;
        let json: CaptchaResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(bilibili_api_error(i64::from(json.code), "captcha"));
        }

        let data = json
            .data
            .ok_or_else(|| BilibiliError::Parse("Missing captcha data".to_string()))?;
        Ok((data.token, data.geetest.gt, data.geetest.challenge))
    }

    /// Get BUVID cookies for SMS operations
    async fn get_buvid_cookies(&self) -> Result<HashMap<String, String>, BilibiliError> {
        #[derive(Deserialize)]
        struct SpiData {
            #[serde(rename = "b_3")]
            b3: String,
            #[serde(rename = "b_4")]
            b4: String,
        }

        #[derive(Deserialize)]
        struct SpiResp {
            code: i32,
            data: Option<SpiData>,
        }

        let url = self.endpoints.api_url("/x/frontend/finger/spi");
        let req = self
            .add_cookies(self.client.get(url))
            .header("User-Agent", USER_AGENT)
            .header("Referer", REFERER);

        let resp = check_response(req.send().await?).await?;
        let json: SpiResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(bilibili_api_error(i64::from(json.code), "BUVID"));
        }

        let data = json
            .data
            .ok_or_else(|| BilibiliError::Parse("Missing BUVID data".to_string()))?;
        let mut cookies = HashMap::new();
        cookies.insert("buvid3".to_string(), data.b3);
        cookies.insert("buvid4".to_string(), data.b4);
        Ok(cookies)
    }

    /// Send SMS verification code
    pub async fn new_sms(
        &self,
        phone: &str,
        token: &str,
        challenge: &str,
        validate: &str,
    ) -> Result<String, BilibiliError> {
        #[derive(Deserialize)]
        struct SmsData {
            captcha_key: String,
        }

        #[derive(Deserialize)]
        struct SmsResp {
            code: i32,
            data: Option<SmsData>,
        }

        // Get BUVID cookies
        let buvid_cookies = self.get_buvid_cookies().await?;

        let seccode = format!("{validate}|jordan");
        let params = [
            ("cid", "86"),
            ("tel", phone),
            ("source", "main-fe-header"),
            ("token", token),
            ("challenge", challenge),
            ("validate", validate),
            ("seccode", &seccode),
        ];

        let url = "https://passport.bilibili.com/x/passport-login/web/sms/send";
        let mut req = self
            .client
            .post(url)
            .header("Referer", "https://passport.bilibili.com/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params);

        // Add BUVID cookies as single Cookie header.
        // Sanitize \r\n to prevent header injection, consistent with add_cookies().
        let cookie_str: String = buvid_cookies
            .iter()
            .map(|(name, value)| sanitize_cookie_pair(name, value))
            .collect::<Vec<_>>()
            .join("; ");
        if !cookie_str.is_empty() {
            req = req.header("Cookie", cookie_str);
        }

        let resp = check_response(req.send().await?).await?;
        let json: SmsResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(bilibili_api_error(i64::from(json.code), "SMS"));
        }

        let data = json
            .data
            .ok_or_else(|| BilibiliError::Parse("Missing SMS data".to_string()))?;
        Ok(data.captcha_key)
    }

    /// Login with SMS verification code
    pub async fn login_with_sms(
        &self,
        phone: &str,
        code: &str,
        captcha_key: &str,
    ) -> Result<HashMap<String, String>, BilibiliError> {
        #[derive(Deserialize)]
        struct LoginSmsData {
            status: i32,
        }

        #[derive(Deserialize)]
        struct LoginSmsResp {
            code: i32,
            data: Option<LoginSmsData>,
        }

        let params = [
            ("cid", "86"),
            ("tel", phone),
            ("code", code),
            ("source", "main-fe-header"),
            ("captcha_key", captcha_key),
        ];

        let url = "https://passport.bilibili.com/x/passport-login/web/login/sms";
        let req = self
            .client
            .post(url)
            .header("Referer", "https://passport.bilibili.com/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params);

        let resp = check_response(req.send().await?).await?;

        // Extract cookies from headers BEFORE consuming body.
        // Cookies are in Set-Cookie headers, so we must read them before json_with_limit.
        let cookies: HashMap<String, String> = resp
            .cookies()
            .filter(|c| {
                matches!(
                    c.name(),
                    "SESSDATA" | "bili_jct" | "DedeUserID" | "DedeUserID__ckMd5"
                )
            })
            .map(|c| (c.name().to_string(), c.value().to_string()))
            .collect();

        let json: LoginSmsResp = json_with_limit(resp).await?;

        // Check API-level status before trusting the cookies
        if json.code != 0 {
            return Err(bilibili_api_error(i64::from(json.code), "SMS login"));
        }

        // Check data.status field -- non-zero indicates SMS login failure
        if let Some(data) = &json.data {
            if data.status != 0 {
                return Err(BilibiliError::Api {
                    code: i64::from(data.status),
                    message: format!("SMS login failed with status: {}", data.status),
                });
            }
        }

        if cookies.is_empty() {
            return Err(BilibiliError::Parse(
                "No auth cookies found in response".to_string(),
            ));
        }

        Ok(cookies)
    }

    /// Extract BVID from URL
    #[must_use]
    pub fn extract_bvid(url: &str) -> Option<String> {
        RE_BVID
            .as_ref()
            .ok()?
            .find(url)
            .map(|m| m.as_str().to_string())
    }

    /// Extract EPID from URL
    #[must_use]
    pub fn extract_epid(url: &str) -> Option<String> {
        RE_EPID
            .as_ref()
            .ok()?
            .captures(url)
            .and_then(|cap| cap.get(1))
            .map(|m| format!("ep{}", m.as_str()))
    }

    /// Check if a hostname belongs to a known Bilibili domain.
    ///
    /// Known domains: `*.bilibili.com`, `*.b23.tv`, `*.bilivideo.com`,
    /// `*.bilivideo.cn`, `*.hdslb.com`.
    #[must_use]
    pub fn is_bilibili_domain(host: &str) -> bool {
        const BILIBILI_DOMAINS: &[&str] = &[
            "bilibili.com",
            "b23.tv",
            "bilivideo.com",
            "bilivideo.cn",
            "hdslb.com",
        ];
        BILIBILI_DOMAINS
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
    }

    /// Validate that a URL points to a known Bilibili domain.
    ///
    /// Returns `Ok(())` if the URL's host is a known Bilibili domain,
    /// or an error otherwise.
    pub fn validate_bilibili_url(url: &str) -> Result<(), BilibiliError> {
        let parsed =
            url::Url::parse(url).map_err(|e| BilibiliError::Parse(format!("Invalid URL: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(BilibiliError::Parse(format!(
                "URL scheme is not allowed for Bilibili URLs: {}",
                parsed.scheme()
            )));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| BilibiliError::Parse("URL has no host".to_string()))?;
        if !Self::is_bilibili_domain(host) {
            return Err(BilibiliError::Parse(format!(
                "URL host is not a known Bilibili domain: {host}"
            )));
        }
        Ok(())
    }

    /// Check if URL is a short link (b23.tv)
    ///
    /// Uses proper URL host parsing to avoid false positives from URLs like
    /// `evil.com/b23.tv` or `b23.tv.evil.com`.
    #[must_use]
    pub fn is_short_link(url: &str) -> bool {
        url::Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .and_then(|u| u.host_str().map(String::from))
            .is_some_and(|host| host == "b23.tv" || host.ends_with(".b23.tv"))
    }

    /// Resolve short link to full URL.
    ///
    /// The short-link client has `redirect(Policy::none())`, so we manually
    /// follow the `Location` header from b23.tv to get the resolved URL.
    /// The resolved URL is structurally validated before returning.
    pub async fn resolve_short_link(&self, url: &str) -> Result<String, BilibiliError> {
        if !Self::is_short_link(url) {
            return Err(BilibiliError::Parse(
                "Short-link resolver only accepts http(s) b23.tv URLs".to_string(),
            ));
        }

        let mut current =
            url::Url::parse(url).map_err(|e| BilibiliError::Parse(format!("Invalid URL: {e}")))?;

        for _ in 0..BILIBILI_SHORT_LINK_MAX_REDIRECTS {
            Self::validate_bilibili_url(current.as_str())?;

            // Redirects are handled manually; private-network blocking depends on
            // the configured runtime SSRF policy.
            let response = self.short_link_client.get(current.clone()).send().await?;
            let status = response.status();

            if status.is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .ok_or_else(|| {
                        BilibiliError::Parse(
                            "Redirect response missing Location header".to_string(),
                        )
                    })?
                    .to_str()
                    .map_err(|e| BilibiliError::Parse(format!("Invalid Location header: {e}")))?;

                current = current
                    .join(location)
                    .map_err(|e| BilibiliError::Parse(format!("Invalid redirect Location: {e}")))?;
                Self::validate_bilibili_url(current.as_str())?;
                continue;
            }

            if status.is_success() {
                if response.url() != &current {
                    return Err(BilibiliError::Parse(format!(
                        "Short-link request followed an unexpected redirect to {}",
                        response.url()
                    )));
                }
                Self::validate_bilibili_url(current.as_str())?;
                return Ok(current.to_string());
            }

            return Err(BilibiliError::Http {
                status,
                url: response.url().to_string(),
                retry_after_secs: None,
                body: String::new(),
            });
        }

        Err(BilibiliError::Parse(format!(
            "Short link exceeded {BILIBILI_SHORT_LINK_MAX_REDIRECTS} redirects"
        )))
    }

    /// Parse video page to get video information
    pub async fn list_popular_videos(
        &self,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let response = self
            .get_api::<types::PopularListData>(
                "/x/web-interface/popular",
                vec![
                    ("pn".to_string(), page.to_string()),
                    ("ps".to_string(), page_size.to_string()),
                ],
            )
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(i64::from(response.code), "popular list"));
        }
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("Popular list response missing data".to_string())
        })?;
        Ok(types::BilibiliVideoListPage {
            items: data
                .list
                .into_iter()
                .map(types::ArchiveSummaryDto::into_item)
                .collect(),
            total: None,
            has_more: !data.no_more,
        })
    }

    pub async fn list_recommended_videos(
        &self,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let page_size = page_size.min(30);
        let response = self
            .get_wbi_api::<types::RecommendedListData>(
                "/x/web-interface/wbi/index/top/feed/rcmd",
                vec![
                    ("fresh_type", "4".to_string()),
                    ("fresh_idx", page.to_string()),
                    ("fresh_idx_1h", page.to_string()),
                    ("brush", page.to_string()),
                    ("ps", page_size.to_string()),
                    ("web_location", "1430650".to_string()),
                ],
            )
            .await?;
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("Recommended list response missing data".to_string())
        })?;
        let items = data
            .item
            .into_iter()
            .filter_map(types::RecommendedArchiveDto::into_item)
            .collect::<Vec<_>>();
        Ok(types::BilibiliVideoListPage {
            has_more: items.len() >= page_size as usize,
            items,
            total: None,
        })
    }

    pub async fn list_up_videos(
        &self,
        mid: u64,
        keyword: &str,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let response = self
            .get_wbi_api::<types::UpVideoListData>(
                "/x/space/wbi/arc/search",
                vec![
                    ("mid", mid.to_string()),
                    ("keyword", keyword.to_string()),
                    ("order", "pubdate".to_string()),
                    ("pn", page.to_string()),
                    ("ps", page_size.to_string()),
                ],
            )
            .await?;
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("UP video list response missing data".to_string())
        })?;
        let total = data.page.total;
        let items = data
            .list
            .vlist
            .into_iter()
            .map(|video| types::BilibiliVideoListItem {
                bvid: video.bvid,
                aid: video.aid,
                cid: 0,
                epid: 0,
                title: video.title,
                cover: video.pic,
                author: video.author,
                description: video.description,
                duration_seconds: parse_colon_duration(&video.length),
                part_count: 0,
                published_at: video.created,
            })
            .collect();
        Ok(types::BilibiliVideoListPage {
            items,
            total: Some(total),
            has_more: page.saturating_mul(u64::from(page_size)) < total,
        })
    }

    pub async fn list_favorite_videos(
        &self,
        media_id: u64,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let response = self
            .get_api::<types::FavoriteListData>(
                "/x/v3/fav/resource/list",
                vec![
                    ("media_id".to_string(), media_id.to_string()),
                    ("pn".to_string(), page.to_string()),
                    ("ps".to_string(), page_size.min(20).to_string()),
                    ("platform".to_string(), "web".to_string()),
                ],
            )
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(
                i64::from(response.code),
                "favorite list",
            ));
        }
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("Favorite list response missing data".to_string())
        })?;
        Ok(types::BilibiliVideoListPage {
            items: data
                .medias
                .into_iter()
                .map(types::ArchiveSummaryDto::into_item)
                .collect(),
            total: Some(data.info.media_count),
            has_more: data.has_more,
        })
    }

    pub async fn list_collection_videos(
        &self,
        mid: u64,
        season_id: u64,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let response = self
            .get_wbi_api::<types::ArchivePageData>(
                "/x/polymer/web-space/seasons_archives_list",
                vec![
                    ("mid", mid.to_string()),
                    ("season_id", season_id.to_string()),
                    ("page_num", page.to_string()),
                    ("page_size", page_size.to_string()),
                    ("sort_reverse", "false".to_string()),
                    ("web_location", "333.999".to_string()),
                ],
            )
            .await?;
        Self::archive_page(response, page, page_size, "Collection list")
    }

    pub async fn list_series_videos(
        &self,
        mid: u64,
        series_id: u64,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let response = self
            .get_api::<types::ArchivePageData>(
                "/x/series/archives",
                vec![
                    ("mid".to_string(), mid.to_string()),
                    ("series_id".to_string(), series_id.to_string()),
                    ("only_normal".to_string(), "true".to_string()),
                    ("sort".to_string(), "desc".to_string()),
                    ("pn".to_string(), page.to_string()),
                    ("ps".to_string(), page_size.to_string()),
                ],
            )
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(i64::from(response.code), "series list"));
        }
        Self::archive_page(response, page, page_size, "Series list")
    }

    fn archive_page(
        response: types::ApiEnvelope<types::ArchivePageData>,
        page: u64,
        page_size: u32,
        context: &'static str,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let data = response
            .data
            .ok_or_else(|| BilibiliError::Parse(format!("{context} response missing data")))?;
        let total = data.page.total;
        Ok(types::BilibiliVideoListPage {
            items: data
                .archives
                .into_iter()
                .map(types::ArchiveSummaryDto::into_item)
                .collect(),
            total: Some(total),
            has_more: page.saturating_mul(u64::from(page_size)) < total,
        })
    }

    pub async fn list_watch_later_videos(
        &self,
        page: u64,
        page_size: u32,
    ) -> Result<types::BilibiliVideoListPage, BilibiliError> {
        let response = self
            .get_api::<types::WatchLaterData>("/x/v2/history/toview", Vec::new())
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(
                i64::from(response.code),
                "watch-later list",
            ));
        }
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("Watch-later list response missing data".to_string())
        })?;
        let start = page.saturating_sub(1).saturating_mul(u64::from(page_size));
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let items = data
            .list
            .into_iter()
            .skip(start)
            .take(page_size as usize)
            .map(types::ArchiveSummaryDto::into_item)
            .collect::<Vec<_>>();
        let end = u64::try_from(start.saturating_add(items.len())).unwrap_or(u64::MAX);
        Ok(types::BilibiliVideoListPage {
            has_more: end < data.count,
            items,
            total: Some(data.count),
        })
    }

    pub async fn list_video_parts(
        &self,
        aid: u64,
        bvid: &str,
    ) -> Result<types::BilibiliVideoParts, BilibiliError> {
        let response = self
            .get_api::<types::VideoPageData>(
                "/x/web-interface/view",
                if bvid.is_empty() {
                    vec![("aid".to_string(), aid.to_string())]
                } else {
                    vec![("bvid".to_string(), bvid.to_string())]
                },
            )
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(i64::from(response.code), "video parts"));
        }
        let data = response
            .data
            .ok_or_else(|| BilibiliError::Parse("Video parts response missing data".to_string()))?;
        let parts = data
            .pages
            .into_iter()
            .map(|part| types::BilibiliVideoPart {
                bvid: data.bvid.clone(),
                aid: data.aid,
                cid: part.cid,
                page: part.page,
                title: part.part,
                cover: if part.first_frame.is_empty() {
                    data.pic.clone()
                } else {
                    part.first_frame
                },
                duration_seconds: part.duration,
                width: part.dimension.width,
                height: part.dimension.height,
            })
            .collect();
        Ok(types::BilibiliVideoParts {
            title: data.title,
            author: data.owner.name,
            parts,
        })
    }

    pub async fn parse_video_page(
        &self,
        aid: u64,
        bvid: &str,
    ) -> Result<VideoPageInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let bvid = bvid.to_string();
        let endpoint = self.endpoints.api_url("/x/web-interface/view");
        let referer = self.endpoints.web_base.clone();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let bvid = bvid.clone();
            let endpoint = endpoint.clone();
            let referer = referer.clone();
            async move {
                let mut req = client.get(&endpoint);
                if bvid.is_empty() {
                    req = req.query(&[("aid", &aid.to_string())]);
                } else {
                    req = req.query(&[("bvid", &bvid)]);
                }
                req = req.header("Referer", referer.as_str());
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::VideoPageInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "video page"));
                }

                let data = json
                    .data
                    .ok_or_else(|| BilibiliError::Parse("Missing video page data".to_string()))?;
                let title = data.title;
                let owner_name = data.owner.name;
                let cover = data.pic.clone();
                let aid = data.aid;
                let collection = data.ugc_season.and_then(|season| {
                    (season.id > 0 && season.mid > 0).then_some(BilibiliCollectionInfo {
                        mid: season.mid,
                        season_id: season.id,
                        title: season.title,
                        cover: season.cover,
                    })
                });

                let mut video_infos = Vec::new();
                for page in data.pages {
                    video_infos.push(VideoInfoItem {
                        bvid: data.bvid.clone(),
                        aid,
                        cid: page.cid,
                        epid: 0,
                        page: page.page,
                        name: page.part,
                        cover_image: if page.first_frame.is_empty() {
                            cover.clone()
                        } else {
                            page.first_frame
                        },
                        live: false,
                        duration_seconds: page.duration,
                        width: page.dimension.width,
                        height: page.dimension.height,
                    });
                }

                Ok(VideoPageInfo {
                    title,
                    actors: vec![owner_name],
                    video_infos,
                    season_id: 0,
                    cover,
                    collection,
                    live_started_at: None,
                    is_currently_live: false,
                })
            }
        })
        .await
    }

    /// Get video playback URL (normal video, not DASH)
    pub async fn get_video_url(
        &self,
        aid: u64,
        bvid: &str,
        cid: u64,
        quality: Option<u32>,
    ) -> Result<VideoUrlInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let bvid = bvid.to_string();
        let qn = quality.unwrap_or(80); // Default to 1080P
        let cid_str = cid.to_string();
        let qn_str = qn.to_string();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let bvid = bvid.clone();
            let cid_str = cid_str.clone();
            let qn_str = qn_str.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/x/player/playurl");
                if bvid.is_empty() {
                    req = req.query(&[
                        ("aid", &aid.to_string()),
                        ("cid", &cid_str),
                        ("qn", &qn_str),
                    ]);
                } else {
                    req = req.query(&[("bvid", &bvid), ("cid", &cid_str), ("qn", &qn_str)]);
                }
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::VideoUrlResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "video URL"));
                }

                let data = json.data.ok_or_else(|| {
                    BilibiliError::Parse("video URL response missing data".to_string())
                })?;
                let accept_quality: Vec<u32> = data
                    .accept_quality
                    .into_iter()
                    .map(|q| quality_to_u32(q, "video URL"))
                    .collect::<Result<_, _>>()?;
                let accept_description = data.accept_description;
                let current_quality = quality_to_u32(data.quality, "video URL")?;
                let segments = video_segments_from_durls(&data.durl);
                let url = required_first_segment_url(&segments, "video URL")?;

                Ok(VideoUrlInfo {
                    accept_quality,
                    accept_description,
                    current_quality,
                    url,
                    segments,
                })
            }
        })
        .await
    }

    /// Get DASH video URL - returns structured DASH data for upper layer to generate MPD.
    ///
    /// This endpoint (`/x/player/wbi/playurl`) requires WBI parameter signing.
    /// Query parameters are signed with the WBI mixin key before sending.
    /// Automatically detects and retries on stale WBI key errors.
    pub async fn get_dash_video_url(
        &self,
        aid: u64,
        bvid: &str,
        cid: u64,
    ) -> Result<(DashData, DashData), BilibiliError> {
        // First attempt with cached key
        let result = self
            .get_dash_video_url_internal(aid, bvid, cid, false)
            .await;

        // If we get a WBI stale error, retry once with fresh key
        if let Err(ref e) = result {
            if Self::is_wbi_stale_error(e) {
                tracing::warn!("WBI key appears stale, refreshing and retrying");
                return self.get_dash_video_url_internal(aid, bvid, cid, true).await;
            }
        }

        result
    }

    /// Internal method for DASH video URL with optional key refresh
    async fn get_dash_video_url_internal(
        &self,
        aid: u64,
        bvid: &str,
        cid: u64,
        force_key_refresh: bool,
    ) -> Result<(DashData, DashData), BilibiliError> {
        // Obtain the WBI mixin key (cached, refreshed on expiry or if forced)
        let mixin_key = self.get_wbi_mixin_key_internal(force_key_refresh).await?;
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let bvid = bvid.to_string();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let mixin_key = mixin_key.clone();
            let bvid = bvid.clone();
            async move {
                // Build query parameters
                let mut params: Vec<(&str, String)> =
                    vec![("cid", cid.to_string()), ("fnval", "4048".to_string())];
                if bvid.is_empty() {
                    params.push(("aid", aid.to_string()));
                } else {
                    params.push(("bvid", bvid.clone()));
                }

                // Sign parameters with WBI (re-signs on each retry for fresh wts)
                let signed_params = wbi_sign(&params, &mixin_key);

                let mut req = client.get("https://api.bilibili.com/x/player/wbi/playurl");
                req = req.query(&signed_params);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::DashVideoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "DASH video URL"));
                }

                // Parse DASH data into structured format
                let data = json.data.ok_or_else(|| {
                    BilibiliError::Parse("DASH video URL response missing payload".to_string())
                })?;
                let dash_info = data.dash.ok_or_else(|| BilibiliError::Api {
                    code: i64::from(json.code),
                    message: "DASH video URL response did not include DASH streams".to_string(),
                })?;
                let (regular_dash, hevc_dash) = parse_dash_info(&dash_info, &data.support_formats);

                Ok((regular_dash, hevc_dash))
            }
        })
        .await
    }

    /// Get subtitles for a video
    pub async fn get_subtitles(
        &self,
        aid: u64,
        bvid: &str,
        cid: u64,
    ) -> Result<HashMap<String, String>, BilibiliError> {
        self.get_subtitles_from_player(aid, bvid, cid).await
    }

    async fn get_subtitles_from_player(
        &self,
        aid: u64,
        bvid: &str,
        cid: u64,
    ) -> Result<HashMap<String, String>, BilibiliError> {
        let mut query = vec![("cid".to_string(), cid.to_string())];
        if bvid.is_empty() {
            query.push(("aid".to_string(), aid.to_string()));
        } else {
            query.push(("bvid".to_string(), bvid.to_string()));
        }
        let response = self
            .get_api::<types::PlayerV2Data>("/x/player/v2", query)
            .await?;
        let data = response
            .data
            .ok_or_else(|| BilibiliError::Parse("subtitle response missing data".to_string()))?;

        Ok(subtitle_player_to_map(data.subtitle.subtitles))
    }

    /// Get user information
    pub async fn user_info(&self) -> Result<UserInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self.endpoints.api_url("/x/web-interface/nav");

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut req = client.get(url).header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::NavResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "user info"));
                }

                let data = json.data.ok_or_else(|| {
                    BilibiliError::Parse("user info response missing data".to_string())
                })?;
                Ok(UserInfo {
                    is_login: data.is_login,
                    user_id: data.mid,
                    username: data.uname,
                    face: data.face,
                    is_vip: data.vip_status == 1,
                })
            }
        })
        .await
    }

    /// Parse PGC (anime/bangumi) page
    pub async fn parse_pgc_page(
        &self,
        epid: u64,
        ssid: u64,
    ) -> Result<VideoPageInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self.endpoints.api_url("/pgc/view/web/season");

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut req = client.get(url);
                if epid != 0 {
                    req = req.query(&[("ep_id", epid)]);
                } else {
                    req = req.query(&[("season_id", ssid)]);
                }
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::SeasonInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "PGC page"));
                }

                let result = json.result;
                let title = result.title;
                let cover = result.cover;
                let season_id = result.season_id;
                let actors_str = result.actors;
                let actors = if actors_str.is_empty() {
                    vec![]
                } else {
                    vec![actors_str]
                };

                let mut video_infos = Vec::new();
                for (index, ep) in result.episodes.into_iter().enumerate() {
                    video_infos.push(VideoInfoItem {
                        bvid: ep.bvid,
                        aid: ep.aid,
                        cid: ep.cid,
                        epid: ep.ep_id,
                        page: u32::try_from(index + 1).unwrap_or(u32::MAX),
                        name: if ep.long_title.is_empty() {
                            ep.title
                        } else {
                            ep.long_title
                        },
                        cover_image: ep.cover,
                        live: false,
                        duration_seconds: ep.duration / 1_000,
                        width: 0,
                        height: 0,
                    });
                }
                for section in result.sections {
                    for ep in section.episodes {
                        let page = u32::try_from(video_infos.len() + 1).unwrap_or(u32::MAX);
                        let episode_title = if ep.long_title.is_empty() {
                            ep.title
                        } else {
                            ep.long_title
                        };
                        let name = if section.title.is_empty() {
                            episode_title
                        } else {
                            format!("{} - {}", section.title, episode_title)
                        };
                        video_infos.push(VideoInfoItem {
                            bvid: ep.bvid,
                            aid: ep.aid,
                            cid: ep.cid,
                            epid: ep.ep_id,
                            page,
                            name,
                            cover_image: ep.cover,
                            live: false,
                            duration_seconds: ep.duration / 1_000,
                            width: 0,
                            height: 0,
                        });
                    }
                }

                Ok(VideoPageInfo {
                    title,
                    actors,
                    video_infos,
                    season_id,
                    cover,
                    collection: None,
                    live_started_at: None,
                    is_currently_live: false,
                })
            }
        })
        .await
    }

    /// Get PGC playback URL
    pub async fn get_pgc_url(
        &self,
        epid: u64,
        cid: u64,
        quality: Option<u32>,
    ) -> Result<VideoUrlInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let qn = quality.unwrap_or(80);

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client
                    .get("https://api.bilibili.com/pgc/player/web/playurl")
                    .query(&[("ep_id", epid), ("cid", cid), ("qn", u64::from(qn))]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::PgcUrlResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "PGC URL"));
                }

                let result = json.result.ok_or_else(|| {
                    BilibiliError::Parse("PGC URL response missing result".to_string())
                })?;
                let accept_quality: Vec<u32> = result
                    .accept_quality
                    .into_iter()
                    .map(|q| quality_to_u32(q, "PGC URL"))
                    .collect::<Result<_, _>>()?;
                let accept_description = result.accept_description;
                let current_quality = quality_to_u32(result.quality, "PGC URL")?;
                let segments = video_segments_from_durls(&result.durl);
                let url = required_first_segment_url(&segments, "PGC URL")?;

                Ok(VideoUrlInfo {
                    accept_quality,
                    accept_description,
                    current_quality,
                    url,
                    segments,
                })
            }
        })
        .await
    }

    /// Get DASH PGC URL - returns structured DASH data for upper layer to generate MPD
    pub async fn get_dash_pgc_url(
        &self,
        epid: u64,
        cid: u64,
    ) -> Result<(DashData, DashData), BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client
                    .get("https://api.bilibili.com/pgc/player/web/playurl")
                    .query(&[("ep_id", epid), ("cid", cid), ("fnval", 4048u64)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::DashPgcResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "DASH PGC URL"));
                }

                let result = json.result.ok_or_else(|| {
                    BilibiliError::Parse("PGC playurl response missing result".to_string())
                })?;
                let dash_info = result.dash.ok_or_else(|| BilibiliError::Api {
                    code: i64::from(result.code),
                    message: "PGC playurl response did not include DASH streams".to_string(),
                })?;
                let (regular_dash, hevc_dash) =
                    parse_dash_info(&dash_info, &result.support_formats);

                Ok((regular_dash, hevc_dash))
            }
        })
        .await
    }

    /// Resolve short links and classify a Bilibili URL into a typed resource.
    pub async fn match_resource(
        &self,
        input: &str,
    ) -> Result<MatchedBilibiliResource, BilibiliError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(BilibiliError::Parse(
                "Bilibili resource URL must not be empty".to_string(),
            ));
        }
        let normalized = if Self::is_short_link(input) {
            self.resolve_short_link(input).await?
        } else {
            input.to_string()
        };
        Self::match_url(&normalized)
    }

    /// Classify a full Bilibili URL or canonical resource identifier.
    pub fn match_url(input: &str) -> Result<MatchedBilibiliResource, BilibiliError> {
        fn positive_id(value: &str, field: &str) -> Result<u64, BilibiliError> {
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| BilibiliError::Parse(format!("Invalid Bilibili {field}: {value}")))
        }

        fn query_value(url: &url::Url, key: &str) -> Option<String> {
            url.query_pairs()
                .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
        }

        let input = input.trim();
        if RE_BVID
            .as_ref()
            .is_ok_and(|regex| regex.find(input).is_some())
            && !input.contains("://")
        {
            let bvid = RE_BVID
                .as_ref()
                .ok()
                .and_then(|regex| regex.find(input))
                .map(|value| value.as_str().to_string())
                .ok_or_else(|| BilibiliError::Parse("Invalid BVID".to_string()))?;
            return Ok(MatchedBilibiliResource {
                normalized_url: format!("https://www.bilibili.com/video/{bvid}"),
                resource: BilibiliResource::Video {
                    bvid,
                    aid: 0,
                    page: 0,
                },
            });
        }
        if let Some(aid) = input
            .strip_prefix("av")
            .or_else(|| input.strip_prefix("AV"))
        {
            let aid = positive_id(aid, "aid")?;
            return Ok(MatchedBilibiliResource {
                normalized_url: format!("https://www.bilibili.com/video/av{aid}"),
                resource: BilibiliResource::Video {
                    bvid: String::new(),
                    aid,
                    page: 0,
                },
            });
        }
        if let Some(episode_id) = input.strip_prefix("ep") {
            let episode_id = positive_id(episode_id, "episode ID")?;
            return Ok(MatchedBilibiliResource {
                normalized_url: format!("https://www.bilibili.com/bangumi/play/ep{episode_id}"),
                resource: BilibiliResource::PgcEpisode { episode_id },
            });
        }
        if let Some(season_id) = input.strip_prefix("ss") {
            let season_id = positive_id(season_id, "season ID")?;
            return Ok(MatchedBilibiliResource {
                normalized_url: format!("https://www.bilibili.com/bangumi/play/ss{season_id}"),
                resource: BilibiliResource::PgcSeason { season_id },
            });
        }

        let url = url::Url::parse(input)
            .map_err(|error| BilibiliError::Parse(format!("Invalid Bilibili URL: {error}")))?;
        Self::validate_bilibili_url(url.as_str())?;
        let host = url.host_str().unwrap_or_default();
        let segments = url
            .path_segments()
            .map(|segments| {
                segments
                    .filter(|segment| !segment.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let page = query_value(&url, "p")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);

        let resource = if host == "live.bilibili.com" || host.ends_with(".live.bilibili.com") {
            if segments.is_empty() {
                BilibiliResource::LiveRecommended
            } else if segments.starts_with(&["p", "eden", "area-tags"]) {
                let parent_area_id = query_value(&url, "parentAreaId")
                    .or_else(|| query_value(&url, "parent_area_id"))
                    .unwrap_or_default();
                let area_id = query_value(&url, "areaId")
                    .or_else(|| query_value(&url, "area_id"))
                    .unwrap_or_default();
                BilibiliResource::LiveArea {
                    parent_area_id: positive_id(&parent_area_id, "live parent area ID")?,
                    area_id: positive_id(&area_id, "live area ID")?,
                }
            } else {
                let room_id = segments
                    .strip_prefix(&["live"])
                    .unwrap_or(&segments)
                    .first()
                    .ok_or_else(|| {
                        BilibiliError::Parse("Missing Bilibili live room ID".to_string())
                    })?;
                BilibiliResource::Live {
                    room_id: positive_id(room_id, "live room ID")?,
                }
            }
        } else if host == "space.bilibili.com" || host.ends_with(".space.bilibili.com") {
            let mid = positive_id(segments.first().copied().unwrap_or_default(), "UP mid")?;
            match segments.get(1).copied() {
                Some("favlist") => BilibiliResource::FavoriteVideos {
                    media_id: positive_id(
                        &query_value(&url, "fid").unwrap_or_default(),
                        "favorite media ID",
                    )?,
                },
                Some("lists") => {
                    let list_id = positive_id(
                        segments.get(2).copied().unwrap_or_default(),
                        "collection or series ID",
                    )?;
                    match query_value(&url, "type").as_deref() {
                        Some("series") => BilibiliResource::SeriesVideos {
                            mid,
                            series_id: list_id,
                        },
                        _ => BilibiliResource::CollectionVideos {
                            mid,
                            season_id: list_id,
                        },
                    }
                }
                Some("channel") if segments.get(2) == Some(&"seriesdetail") => {
                    BilibiliResource::SeriesVideos {
                        mid,
                        series_id: positive_id(
                            &query_value(&url, "sid").unwrap_or_default(),
                            "series ID",
                        )?,
                    }
                }
                Some("channel") if segments.get(2) == Some(&"collectiondetail") => {
                    BilibiliResource::CollectionVideos {
                        mid,
                        season_id: positive_id(
                            &query_value(&url, "sid").unwrap_or_default(),
                            "collection season ID",
                        )?,
                    }
                }
                None | Some("video") => BilibiliResource::UpVideos { mid },
                _ => {
                    return Err(BilibiliError::Parse(
                        "Unsupported Bilibili space resource URL".to_string(),
                    ));
                }
            }
        } else if segments.first() == Some(&"video") {
            let identifier = segments.get(1).copied().unwrap_or_default();
            if let Some(aid) = identifier
                .strip_prefix("av")
                .or_else(|| identifier.strip_prefix("AV"))
            {
                BilibiliResource::Video {
                    bvid: String::new(),
                    aid: positive_id(aid, "aid")?,
                    page,
                }
            } else {
                let bvid = RE_BVID
                    .as_ref()
                    .ok()
                    .and_then(|regex| regex.find(identifier))
                    .map(|value| value.as_str().to_string())
                    .ok_or_else(|| BilibiliError::Parse("Invalid Bilibili video ID".to_string()))?;
                BilibiliResource::Video { bvid, aid: 0, page }
            }
        } else if segments.first() == Some(&"bangumi") && segments.get(1) == Some(&"play") {
            let identifier = segments.get(2).copied().unwrap_or_default();
            if let Some(episode_id) = identifier.strip_prefix("ep") {
                BilibiliResource::PgcEpisode {
                    episode_id: positive_id(episode_id, "episode ID")?,
                }
            } else if let Some(season_id) = identifier.strip_prefix("ss") {
                BilibiliResource::PgcSeason {
                    season_id: positive_id(season_id, "season ID")?,
                }
            } else {
                return Err(BilibiliError::Parse("Invalid Bilibili PGC URL".to_string()));
            }
        } else if segments.first() == Some(&"watchlater") {
            BilibiliResource::WatchLater
        } else if segments.first() == Some(&"list") {
            let list_id = positive_id(
                segments.get(1).copied().unwrap_or_default(),
                "collection or series ID",
            )?;
            let mid = positive_id(
                &query_value(&url, "mid")
                    .or_else(|| query_value(&url, "uid"))
                    .unwrap_or_default(),
                "UP mid",
            )?;
            match query_value(&url, "type").as_deref() {
                Some("series") => BilibiliResource::SeriesVideos {
                    mid,
                    series_id: list_id,
                },
                _ => BilibiliResource::CollectionVideos {
                    mid,
                    season_id: list_id,
                },
            }
        } else {
            return Err(BilibiliError::Parse(
                "Unsupported Bilibili resource URL".to_string(),
            ));
        };

        Ok(MatchedBilibiliResource {
            normalized_url: url.to_string(),
            resource,
        })
    }

    /// Parse live page
    pub async fn parse_live_page(&self, room_id: u64) -> Result<VideoPageInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let room_info_url = self.endpoints.live_api_url("/room/v1/Room/get_info");
        let master_info_url = self.endpoints.live_api_url("/live_user/v1/Master/info");

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let room_info_url = room_info_url.clone();
            let master_info_url = master_info_url.clone();
            async move {
                let mut req = client.get(room_info_url).query(&[("room_id", room_id)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::ParseLivePageResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "live page"));
                }

                let data = json.data;
                let title = data.title.clone();

                // Fetch streamer name from master info API using uid from room info
                let uname = {
                    let uid = data.uid;
                    let mut master_req = client.get(master_info_url).query(&[("uid", uid)]);
                    master_req = master_req.header("Referer", REFERER);
                    if let Some(ref cookies) = cookie_header {
                        master_req = master_req.header("Cookie", cookies.as_str());
                    }
                    match check_response(master_req.send().await?).await {
                        Ok(master_resp) => {
                            match json_with_limit::<types::GetLiveMasterInfoResp>(master_resp).await
                            {
                                Ok(master_json)
                                    if master_json.code == 0
                                        && !master_json.data.info.uname.is_empty() =>
                                {
                                    master_json.data.info.uname
                                }
                                _ => uid.to_string(),
                            }
                        }
                        _ => uid.to_string(),
                    }
                };
                let cover = data.user_cover.clone();
                let live_started_at = (data.live_status == 1)
                    .then(|| parse_bilibili_live_started_at(&data.live_time))
                    .flatten();

                let video_info = VideoInfoItem {
                    bvid: String::new(),
                    aid: 0,
                    cid: room_id,
                    epid: 0,
                    page: 0,
                    name: title.clone(),
                    cover_image: cover.clone(),
                    live: true,
                    duration_seconds: 0,
                    width: 0,
                    height: 0,
                };

                Ok(VideoPageInfo {
                    title,
                    actors: vec![uname],
                    video_infos: vec![video_info],
                    season_id: 0,
                    cover,
                    collection: None,
                    live_started_at,
                    is_currently_live: data.live_status == 1,
                })
            }
        })
        .await
    }

    pub async fn list_recommended_live_rooms(
        &self,
        page: u64,
        page_size: u32,
    ) -> Result<LiveRoomList, BilibiliError> {
        if page > 1 {
            return Ok(LiveRoomList {
                items: Vec::new(),
                total: None,
                has_more: false,
            });
        }
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self
            .endpoints
            .live_api_url("/xlive/web-interface/v1/webMain/getMoreRecList");
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut request = client.get(url).query(&[("platform", "web")]);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                let payload: types::LiveRecommendedResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(
                        i64::from(payload.code),
                        "live recommendations",
                    ));
                }
                let mut items = payload
                    .data
                    .recommend_room_list
                    .into_iter()
                    .filter_map(live_room_from_card)
                    .collect::<Vec<_>>();
                items.truncate(page_size.clamp(1, 50) as usize);
                Ok(LiveRoomList {
                    items,
                    total: None,
                    has_more: false,
                })
            }
        })
        .await
    }

    pub async fn list_followed_live_rooms(
        &self,
        page: u64,
        page_size: u32,
    ) -> Result<LiveRoomList, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self
            .endpoints
            .live_api_url("/xlive/web-ucenter/user/following");
        let page = page.max(1);
        let page_size = page_size.clamp(1, 10);
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut request = client.get(url).query(&[
                    ("page", page.to_string()),
                    ("page_size", page_size.to_string()),
                    ("ignoreRecord", "1".to_string()),
                    ("hit_ab", "true".to_string()),
                ]);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                let payload: types::LiveFollowingResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(
                        i64::from(payload.code),
                        "followed live rooms",
                    ));
                }
                let total = payload.data.count.value();
                let has_more = page < payload.data.total_page.value();
                Ok(LiveRoomList {
                    items: payload
                        .data
                        .list
                        .into_iter()
                        .filter_map(live_room_from_card)
                        .collect(),
                    total: Some(total),
                    has_more,
                })
            }
        })
        .await
    }

    pub async fn list_area_live_rooms(
        &self,
        parent_area_id: u64,
        area_id: u64,
        page: u64,
        page_size: u32,
    ) -> Result<LiveRoomList, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self
            .endpoints
            .live_api_url("/xlive/web-interface/v1/second/getList");
        let page = page.max(1);
        let page_size = page_size.clamp(1, 50);
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut request = client.get(url).query(&[
                    ("platform", "web".to_string()),
                    ("parent_area_id", parent_area_id.to_string()),
                    ("area_id", area_id.to_string()),
                    ("sort_type", "online".to_string()),
                    ("page", page.to_string()),
                    ("page_size", page_size.to_string()),
                ]);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                let payload: types::LiveAreaRoomsResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(
                        i64::from(payload.code),
                        "live area rooms",
                    ));
                }
                let total = payload.data.count.value();
                let items = payload
                    .data
                    .list
                    .into_iter()
                    .filter_map(live_room_from_card)
                    .collect::<Vec<_>>();
                let has_more = payload.data.has_more.value() != 0
                    || page.saturating_mul(u64::from(page_size)) < total;
                Ok(LiveRoomList {
                    items,
                    total: Some(total),
                    has_more,
                })
            }
        })
        .await
    }

    pub async fn list_live_areas(&self) -> Result<Vec<LiveAreaItem>, BilibiliError> {
        let client = self.client.clone();
        let url = self.endpoints.live_api_url("/room/v1/Area/getList");
        with_retry(|| {
            let client = client.clone();
            let url = url.clone();
            async move {
                let response = check_response(client.get(url).send().await?).await?;
                let payload: types::LiveAreasResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(i64::from(payload.code), "live areas"));
                }
                Ok(payload
                    .data
                    .into_iter()
                    .flat_map(|parent| {
                        let parent_id = parent.id.value();
                        let parent_name = parent.name;
                        parent.list.into_iter().filter_map(move |area| {
                            let id = area.id.value();
                            (id > 0).then(|| LiveAreaItem {
                                id,
                                parent_id: if area.parent_id.value() > 0 {
                                    area.parent_id.value()
                                } else {
                                    parent_id
                                },
                                name: area.name,
                                parent_name: if area.parent_name.is_empty() {
                                    parent_name.clone()
                                } else {
                                    area.parent_name
                                },
                                picture: area.pic,
                                hot: area.hot_status.value() != 0,
                            })
                        })
                    })
                    .collect())
            }
        })
        .await
    }

    pub async fn list_favorite_folders(&self) -> Result<Vec<FavoriteFolder>, BilibiliError> {
        let user = self.user_info().await?;
        if !user.is_login || user.user_id == 0 {
            return Err(BilibiliError::Api {
                code: -101,
                message: "Bilibili authentication is required".to_string(),
            });
        }
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self.endpoints.api_url("/x/v3/fav/folder/created/list-all");
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut request = client.get(url).query(&[("up_mid", user.user_id)]);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                let payload: types::FavoriteFoldersResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(
                        i64::from(payload.code),
                        "favorite folders",
                    ));
                }
                Ok(payload
                    .data
                    .list
                    .into_iter()
                    .filter(|folder| folder.id > 0)
                    .map(|folder| FavoriteFolder {
                        media_id: folder.id,
                        title: folder.title,
                        media_count: folder.media_count,
                        private: folder.attr & 1 != 0,
                        default_folder: folder.attr & 2 == 0,
                    })
                    .collect())
            }
        })
        .await
    }

    pub async fn list_followed_pgc(
        &self,
        season_type: u32,
        page: u64,
        page_size: u32,
    ) -> Result<FollowedPgcList, BilibiliError> {
        if !matches!(season_type, 1 | 2) {
            return Err(BilibiliError::InvalidConfig(
                "Bilibili followed PGC type must be 1 (anime) or 2 (cinema)".to_string(),
            ));
        }
        let user = self.user_info().await?;
        if !user.is_login || user.user_id == 0 {
            return Err(BilibiliError::Api {
                code: -101,
                message: "Bilibili authentication is required".to_string(),
            });
        }
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self.endpoints.api_url("/x/space/bangumi/follow/list");
        let page = page.max(1);
        let page_size = page_size.clamp(1, 30);
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut request = client.get(url).query(&[
                    ("vmid", user.user_id.to_string()),
                    ("type", season_type.to_string()),
                    ("pn", page.to_string()),
                    ("ps", page_size.to_string()),
                ]);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                let payload: types::FollowedPgcResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(i64::from(payload.code), "followed PGC"));
                }
                let total = payload.data.total;
                Ok(FollowedPgcList {
                    items: payload
                        .data
                        .list
                        .into_iter()
                        .filter(|season| season.season_id > 0)
                        .map(|season| FollowedPgcSeason {
                            season_id: season.season_id,
                            title: season.title,
                            cover: season.cover,
                            description: season.evaluate,
                            latest_episode: season.new_ep.index_show,
                        })
                        .collect(),
                    total,
                    has_more: page.saturating_mul(u64::from(page_size)) < total,
                })
            }
        })
        .await
    }

    pub async fn list_history(
        &self,
        history_type: &str,
        cursor: Option<&HistoryCursor>,
        page_size: u32,
    ) -> Result<HistoryPage, BilibiliError> {
        let history_type = match history_type {
            "all" | "archive" | "live" => history_type,
            _ => {
                return Err(BilibiliError::InvalidConfig(
                    "Bilibili history type must be all, archive, or live".to_string(),
                ));
            }
        };
        let mut query = vec![
            ("type".to_string(), history_type.to_string()),
            ("ps".to_string(), page_size.clamp(1, 30).to_string()),
        ];
        if let Some(cursor) = cursor {
            query.extend([
                ("max".to_string(), cursor.max.to_string()),
                ("view_at".to_string(), cursor.view_at.to_string()),
                ("business".to_string(), cursor.business.clone()),
            ]);
        }
        let response = self
            .get_api::<types::HistoryDataDto>("/x/web-interface/history/cursor", query)
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(i64::from(response.code), "history list"));
        }
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("Bilibili history response missing data".to_string())
        })?;
        let upstream_count = data.list.len();
        let cursor = HistoryCursor {
            max: data.cursor.max,
            view_at: data.cursor.view_at,
            business: data.cursor.business,
        };
        let items = data
            .list
            .into_iter()
            .filter_map(|item| {
                let resource = match item.history.business.as_str() {
                    "archive"
                        if !item.history.bvid.is_empty()
                            && item.history.oid > 0
                            && item.history.cid > 0 =>
                    {
                        HistoryResource::Video {
                            bvid: item.history.bvid,
                            aid: item.history.oid,
                            cid: item.history.cid,
                        }
                    }
                    "pgc" if item.history.epid > 0 && item.history.cid > 0 => {
                        HistoryResource::Pgc {
                            epid: item.history.epid,
                            cid: item.history.cid,
                        }
                    }
                    "live" if item.history.oid > 0 && item.live_status == 1 => {
                        HistoryResource::Live {
                            room_id: item.history.oid,
                        }
                    }
                    _ => return None,
                };
                Some(HistoryItem {
                    resource,
                    title: item.title,
                    subtitle: item.long_title,
                    cover: item.cover,
                    author: item.author_name,
                    viewed_at: item.view_at,
                    progress_seconds: item.progress,
                    duration_seconds: item.duration,
                })
            })
            .collect();
        let has_more = upstream_count > 0
            && cursor.view_at > 0
            && cursor.max > 0
            && !cursor.business.is_empty();
        Ok(HistoryPage {
            items,
            cursor: has_more.then_some(cursor),
            has_more,
        })
    }

    pub async fn list_pgc_timeline(
        &self,
        timeline_type: u32,
        before_days: u32,
        after_days: u32,
    ) -> Result<Vec<PgcTimelineItem>, BilibiliError> {
        if !matches!(timeline_type, 1 | 3 | 4) {
            return Err(BilibiliError::InvalidConfig(
                "Bilibili timeline type must be anime, cinema, or guochuang".to_string(),
            ));
        }
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let url = self.endpoints.api_url("/pgc/web/timeline");
        let before_days = before_days.min(7);
        let after_days = after_days.min(7);
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let url = url.clone();
            async move {
                let mut request = client.get(url).query(&[
                    ("types", timeline_type),
                    ("before", before_days),
                    ("after", after_days),
                ]);
                if let Some(cookies) = cookie_header.as_deref() {
                    request = request.header("Cookie", cookies);
                }
                let response = check_response(request.send().await?).await?;
                let payload: types::TimelineResp = json_with_limit(response).await?;
                if payload.code != 0 {
                    return Err(bilibili_api_error(i64::from(payload.code), "PGC timeline"));
                }
                let items = payload
                    .result
                    .into_iter()
                    .flat_map(|day| {
                        day.episodes
                            .into_iter()
                            .map(move |episode| PgcTimelineItem {
                                episode_id: episode.episode_id,
                                season_id: episode.season_id,
                                cid: 0,
                                title: episode.title,
                                episode_title: episode.pub_index,
                                cover: episode.cover,
                                episode_cover: episode.ep_cover,
                                publish_at: episode.pub_ts,
                                published: episode.published != 0,
                                date: day.date.clone(),
                                day_of_week: day.day_of_week,
                                delayed: episode.delay != 0,
                                delay_reason: episode.delay_reason,
                            })
                    })
                    .filter(|item| item.episode_id > 0)
                    .collect::<Vec<_>>();
                let mut items = futures_util::stream::iter(items.into_iter().enumerate())
                    .map(|(index, mut item)| async move {
                        if item.published {
                            item.cid = self
                                .parse_pgc_page(item.episode_id, 0)
                                .await
                                .ok()
                                .and_then(|page| {
                                    page.video_infos
                                        .into_iter()
                                        .find(|video| video.epid == item.episode_id)
                                })
                                .map_or(0, |video| video.cid);
                        }
                        (index, item)
                    })
                    .buffer_unordered(8)
                    .collect::<Vec<_>>()
                    .await;
                items.sort_unstable_by_key(|(index, _)| *index);
                Ok(items.into_iter().map(|(_, item)| item).collect())
            }
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_pgc_seasons(
        &self,
        season_type: u32,
        page: u64,
        page_size: u32,
        order: u32,
        ascending: bool,
        finished: Option<bool>,
        area: Option<&str>,
        year: Option<&str>,
        style_id: Option<u64>,
    ) -> Result<PgcSeasonIndexPage, BilibiliError> {
        if !matches!(season_type, 1 | 2 | 3 | 4 | 5 | 7) || order > 6 {
            return Err(BilibiliError::InvalidConfig(
                "Bilibili PGC season index filter is invalid".to_string(),
            ));
        }
        let page = page.max(1);
        let page_size = page_size.clamp(1, 50);
        let mut query = vec![
            ("season_type".to_string(), season_type.to_string()),
            ("st".to_string(), season_type.to_string()),
            ("type".to_string(), "1".to_string()),
            ("page".to_string(), page.to_string()),
            ("pagesize".to_string(), page_size.to_string()),
            ("order".to_string(), order.to_string()),
            (
                "sort".to_string(),
                if ascending { "1" } else { "0" }.to_string(),
            ),
        ];
        if let Some(finished) = finished {
            query.push((
                "is_finish".to_string(),
                if finished { "1" } else { "0" }.to_string(),
            ));
        }
        if let Some(area) = area.map(str::trim).filter(|value| !value.is_empty()) {
            query.push(("area".to_string(), area.to_string()));
        }
        if let Some(year) = year.map(str::trim).filter(|value| !value.is_empty()) {
            let field = if matches!(season_type, 1 | 4) {
                "year"
            } else {
                "release_date"
            };
            query.push((field.to_string(), year.to_string()));
        }
        if let Some(style_id) = style_id.filter(|value| *value > 0) {
            query.push(("style_id".to_string(), style_id.to_string()));
        }
        let response = self
            .get_api::<types::SeasonIndexDataDto>("/pgc/season/index/result", query)
            .await?;
        if response.code != 0 {
            return Err(bilibili_api_error(
                i64::from(response.code),
                "PGC season index",
            ));
        }
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("Bilibili PGC season index response missing data".to_string())
        })?;
        Ok(PgcSeasonIndexPage {
            items: data
                .list
                .into_iter()
                .filter(|item| item.season_id > 0)
                .map(|item| PgcSeasonIndexItem {
                    season_id: item.season_id,
                    media_id: item.media_id,
                    first_episode_id: item.first_ep.ep_id,
                    title: item.title,
                    subtitle: item.subtitle,
                    cover: item.cover,
                    first_episode_cover: item.first_ep.cover,
                    badge: item.badge,
                    progress: item.index_show,
                    score: item.score,
                    finished: item.is_finish != 0,
                    season_type: item.season_type,
                })
                .collect(),
            total: data.total,
            has_more: data.has_next != 0,
        })
    }

    /// Get live streams
    pub async fn get_live_streams(
        &self,
        room_id: u64,
        hls: bool,
    ) -> Result<Vec<LiveStream>, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let room_id_str = room_id.to_string();
        let play_info_url = self
            .endpoints
            .live_api_url("/xlive/web-room/v2/index/getRoomPlayInfo");

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let room_id_str = room_id_str.clone();
            let play_info_url = play_info_url.clone();
            async move {
                let fetch = |qn: u64| {
                    let client = client.clone();
                    let cookie_header = cookie_header.clone();
                    let room_id_str = room_id_str.clone();
                    let play_info_url = play_info_url.clone();
                    async move {
                        let qn = qn.to_string();
                        let mut req = client.get(play_info_url).query(&[
                            ("room_id", room_id_str.as_str()),
                            ("protocol", "0,1"),
                            ("format", "0,1,2"),
                            ("codec", "0,1"),
                            ("qn", qn.as_str()),
                            ("platform", "web"),
                            ("ptype", "8"),
                        ]);
                        req = req.header("Referer", REFERER);
                        if let Some(ref cookies) = cookie_header {
                            req = req.header("Cookie", cookies.as_str());
                        }
                        let resp = check_response(req.send().await?).await?;
                        let json: types::RoomPlayInfoResp = json_with_limit(resp).await?;
                        if json.code != 0 {
                            return Err(bilibili_api_error(i64::from(json.code), "live streams"));
                        }
                        Ok(json)
                    }
                };

                let initial = fetch(10_000).await?;
                let Some(initial_playurl) = initial
                    .data
                    .playurl_info
                    .as_ref()
                    .and_then(|info| info.playurl.as_ref())
                else {
                    return Ok(Vec::new());
                };
                let quality_names = initial_playurl
                    .g_qn_desc
                    .iter()
                    .map(|quality| (quality.qn, quality.desc.clone()))
                    .collect::<HashMap<_, _>>();
                let mut qualities = initial_playurl
                    .stream
                    .iter()
                    .flat_map(|stream| &stream.format)
                    .flat_map(|format| &format.codec)
                    .flat_map(|codec| codec.accept_qn.iter().copied())
                    .collect::<Vec<_>>();
                qualities.push(10_000);
                qualities.sort_unstable();
                qualities.dedup();

                let dominated = if hls { "http_hls" } else { "http_stream" };
                let mut streams = Vec::new();
                for requested_quality in qualities {
                    let response = if requested_quality == 10_000 {
                        initial.clone()
                    } else {
                        fetch(requested_quality).await?
                    };
                    let stream_list = response
                        .data
                        .playurl_info
                        .as_ref()
                        .and_then(|info| info.playurl.as_ref())
                        .map(|playurl| &playurl.stream[..])
                        .unwrap_or_default();
                    for stream in stream_list {
                        if !stream.protocol_name.is_empty() && stream.protocol_name != dominated {
                            continue;
                        }
                        for format in &stream.format {
                            for codec in &format.codec {
                                let quality = quality_to_u32(codec.current_qn, "live stream URL")?;
                                if u64::from(quality) != requested_quality
                                    && requested_quality != 10_000
                                {
                                    continue;
                                }
                                let urls = codec
                                    .url_info
                                    .iter()
                                    .filter(|info| !info.host.is_empty())
                                    .map(|info| {
                                        let url = format!(
                                            "{}{}{}",
                                            info.host, codec.base_url, info.extra
                                        );
                                        LiveStreamUrl {
                                            host: info.host.clone(),
                                            expires_at: parse_live_stream_expires_at(&url),
                                            url,
                                        }
                                    })
                                    .collect::<Vec<_>>();
                                if !urls.is_empty() {
                                    streams.push(LiveStream {
                                        quality,
                                        quality_name: quality_names
                                            .get(&u64::from(quality))
                                            .filter(|name| !name.trim().is_empty())
                                            .cloned()
                                            .unwrap_or_else(|| format!("Quality {quality}")),
                                        protocol: stream.protocol_name.clone(),
                                        format: format.format_name.clone(),
                                        codec: codec.codec_name.clone(),
                                        urls,
                                    });
                                }
                            }
                        }
                    }
                }
                streams.sort_by(|left, right| {
                    right
                        .quality
                        .cmp(&left.quality)
                        .then_with(|| {
                            live_stream_format_rank(&left.format)
                                .cmp(&live_stream_format_rank(&right.format))
                        })
                        .then_with(|| {
                            live_stream_codec_rank(&left.codec)
                                .cmp(&live_stream_codec_rank(&right.codec))
                        })
                        .then(left.protocol.cmp(&right.protocol))
                });
                streams.dedup_by(|left, right| {
                    left.quality == right.quality
                        && left.protocol == right.protocol
                        && left.format == right.format
                        && left.codec == right.codec
                });
                Ok(streams)
            }
        })
        .await
    }

    /// Get live danmaku server info
    pub async fn get_live_danmu_info(&self, room_id: u64) -> Result<LiveDanmuInfo, BilibiliError> {
        let response: types::ApiEnvelope<types::LiveDanmuData> = self
            .get_wbi_live_api(
                "/xlive/web-room/v1/index/getDanmuInfo",
                vec![
                    ("id", room_id.to_string()),
                    ("type", "0".to_string()),
                    ("web_location", "444.8".to_string()),
                ],
            )
            .await?;
        let data = response.data.ok_or_else(|| {
            BilibiliError::Parse("live danmaku response missing data".to_string())
        })?;
        let host_list = data
            .host_list
            .into_iter()
            .map(|host| DanmuHost {
                host: host.host,
                port: host.port,
                wss_port: host.wss_port,
                ws_port: host.ws_port,
            })
            .collect();

        Ok(LiveDanmuInfo {
            token: data.token,
            host_list,
        })
    }

    /// Connect to live danmaku WebSocket and return a message stream
    ///
    /// Returns a tuple of (sender, receiver) for bidirectional communication
    pub fn connect_live_danmaku(
        &self,
        room_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<LiveDanmakuConnection, BilibiliError>> + Send + '_>>
    {
        #[cfg(not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")))]
        {
            let _ = (self, room_id);
            Box::pin(ready(Err(BilibiliError::InvalidConfig(
                "Bilibili live danmaku requires a TLS root feature".to_string(),
            ))))
        }

        #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
        Box::pin(async move {
            // Get danmaku server info
            let danmu_info = self.get_live_danmu_info(room_id).await?;

            // Select first available host with wss_port
            let host = danmu_info
                .host_list
                .first()
                .ok_or_else(|| BilibiliError::Parse("No danmaku host available".to_string()))?;

            // Build WebSocket URL (use wss:// for secure connection)
            let ws_url = format!("wss://{}:{}/sub", host.host, host.wss_port);
            let validated_addr =
                resolve_validated_danmaku_addr(&host.host, host.wss_port, &self.ssrf_guard).await?;

            let ws_stream = {
                let ws_connect_timeout = Duration::from_secs(10);
                tokio::time::timeout(ws_connect_timeout, async {
                    let socket = TcpStream::connect(validated_addr).await.map_err(|e| {
                        BilibiliError::Network(format!(
                            "Failed to connect to danmaku WebSocket socket {validated_addr}: {e}"
                        ))
                    })?;
                    connect_live_danmaku_websocket(ws_url.as_str(), socket).await
                })
                .await
                .map_err(|_| BilibiliError::Network("WebSocket connection timeout".to_string()))??
            };

            let (mut write, read) = ws_stream.split();

            // Send authentication packet
            let auth_packet = build_auth_packet(room_id, &danmu_info.token)?;
            write
                .send(Message::Binary(auth_packet.into()))
                .await
                .map_err(|e| BilibiliError::Network(format!("Failed to send auth packet: {e}")))?;

            Ok(LiveDanmakuConnection {
                write: AsyncMutex::new(write),
                read: AsyncMutex::new(read),
                room_id,
                heartbeat_handle: AsyncMutex::new(None),
                heartbeat_stop: Arc::new(AtomicBool::new(false)),
            })
        })
    }

    /// Connect to live danmaku WebSocket with automatic reconnection support
    ///
    /// Returns a [`ReconnectableLiveDanmakuConnection`] that will automatically
    /// reconnect if the WebSocket connection is lost, using exponential backoff.
    ///
    /// # Arguments
    /// * `room_id` - The live room ID to connect to
    /// * `config` - Reconnection configuration (max retries, delays, etc.)
    ///
    pub async fn connect_live_danmaku_with_reconnect(
        self: &Arc<Self>,
        room_id: u64,
        config: ReconnectConfig,
    ) -> Result<ReconnectableLiveDanmakuConnection, BilibiliError> {
        // Establish initial connection
        let connection = self.connect_live_danmaku(room_id).await?;
        let connection = Arc::new(connection);

        Ok(ReconnectableLiveDanmakuConnection {
            client: Arc::clone(self),
            connection: Some(connection),
            room_id,
            config,
            current_retry: 0,
            heartbeat_config: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        })
    }
}

/// Configuration for automatic heartbeat keepalive
#[derive(Debug, Clone, Copy)]
pub struct HeartbeatConfig {
    /// Interval between heartbeat packets (default: 30 seconds)
    pub interval: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
        }
    }
}

/// Configuration for automatic WebSocket reconnection
#[derive(Debug, Clone, Copy)]
pub struct ReconnectConfig {
    /// Maximum number of reconnection attempts before giving up
    pub max_retries: u32,
    /// Initial delay before first reconnection attempt
    pub initial_delay: Duration,
    /// Maximum delay between reconnection attempts (for exponential backoff)
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (default: 2.0)
    pub backoff_multiplier: f64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

impl ReconnectConfig {
    /// Calculate the delay for a given retry attempt using exponential backoff
    ///
    /// The delay is calculated as: `initial_delay * backoff_multiplier^retry_count`
    /// and is capped at `max_delay`.
    ///
    /// # Arguments
    /// * `retry_count` - Zero-based retry attempt number (0 = first retry)
    ///
    /// # Example
    /// ```
    /// use std::time::Duration;
    /// use synctv_media_providers::bilibili::ReconnectConfig;
    ///
    /// let config = ReconnectConfig {
    ///     initial_delay: Duration::from_secs(1),
    ///     max_delay: Duration::from_secs(30),
    ///     backoff_multiplier: 2.0,
    ///     ..Default::default()
    /// };
    ///
    /// assert_eq!(config.delay_for_retry(0), Duration::from_secs(1));
    /// assert_eq!(config.delay_for_retry(1), Duration::from_secs(2));
    /// assert_eq!(config.delay_for_retry(2), Duration::from_secs(4));
    /// ```
    pub fn delay_for_retry(&self, retry_count: u32) -> Duration {
        let exponent = i32::try_from(retry_count).unwrap_or(i32::MAX);
        let delay_secs = self.initial_delay.as_secs_f64() * self.backoff_multiplier.powi(exponent);
        let capped_secs = delay_secs.min(self.max_delay.as_secs_f64());
        Duration::from_secs_f64(capped_secs)
    }
}

/// Live danmaku WebSocket connection with automatic heartbeat support
pub struct LiveDanmakuConnection {
    write: AsyncMutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
    read: AsyncMutex<
        futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    >,
    room_id: u64,
    /// Handle for the automatic heartbeat task
    heartbeat_handle: AsyncMutex<Option<JoinHandle<()>>>,
    /// Flag to signal the heartbeat task to stop
    heartbeat_stop: Arc<AtomicBool>,
}

impl LiveDanmakuConnection {
    /// Receive next danmaku messages from one WebSocket frame.
    ///
    /// A single binary frame can contain multiple compressed sub-packets
    /// (e.g. several chat messages batched together). This returns all
    /// parsed messages from the frame.
    pub async fn recv(&self) -> Result<Vec<DanmakuMessage>, BilibiliError> {
        let mut read = self.read.lock().await;

        match read.next().await {
            Some(Ok(Message::Binary(data))) => Ok(parse_danmaku_packet(&data)),
            Some(Ok(Message::Close(_))) => Err(BilibiliError::Parse(
                "Danmaku WebSocket connection closed by server".to_string(),
            )),
            Some(Ok(_)) => Ok(Vec::new()), // Ignore non-binary messages (ping/pong/text)
            Some(Err(e)) => Err(BilibiliError::Parse(format!("WebSocket error: {e}"))),
            None => {
                // Stream ended = connection closed unexpectedly
                Err(BilibiliError::Parse(
                    "Danmaku WebSocket connection closed".to_string(),
                ))
            }
        }
    }

    /// Send heartbeat to keep connection alive
    pub async fn send_heartbeat(&self) -> Result<(), BilibiliError> {
        let mut write = self.write.lock().await;
        let heartbeat_packet = build_heartbeat_packet();
        write
            .send(Message::Binary(heartbeat_packet.into()))
            .await
            .map_err(|e| BilibiliError::Network(format!("Failed to send heartbeat: {e}")))
    }

    /// Get room ID
    pub const fn room_id(&self) -> u64 {
        self.room_id
    }

    /// Stop the automatic heartbeat loop.
    ///
    /// This method signals the heartbeat task to stop and aborts it.
    /// After calling this, you can call [`start_heartbeat_loop`](Self::start_heartbeat_loop)
    /// again if needed.
    pub async fn stop_heartbeat_loop(&self) {
        // Signal the heartbeat task to stop
        self.heartbeat_stop.store(true, Ordering::SeqCst);

        // Wait for the task to complete and drop the handle
        let mut handle_guard = self.heartbeat_handle.lock().await;
        if let Some(handle) = handle_guard.take() {
            // Abort the task for immediate cleanup
            handle.abort();
        }
    }

    /// Check if the heartbeat loop is currently running
    pub async fn is_heartbeat_running(&self) -> bool {
        let handle_guard = self.heartbeat_handle.lock().await;
        handle_guard.as_ref().is_some_and(|h| !h.is_finished())
    }
}

impl LiveDanmakuConnection {
    /// Start the automatic heartbeat loop.
    ///
    /// This is the recommended way to start the heartbeat loop when you have
    /// an `Arc<LiveDanmakuConnection>`.
    ///
    /// # Arguments
    /// * `config` - Heartbeat configuration including interval
    ///
    /// # Returns
    /// * `true` if the heartbeat loop was started
    /// * `false` if a heartbeat loop was already running
    ///
    /// # Example
    /// ```no_run
    /// use std::sync::Arc;
    /// use std::time::Duration;
    /// use synctv_media_providers::BilibiliError;
    /// use synctv_media_providers::bilibili::{BilibiliClient, HeartbeatConfig};
    ///
    /// # async fn demo() -> Result<(), BilibiliError> {
    /// let room_id = 123_u64;
    /// let client = BilibiliClient::new()?;
    /// let conn = Arc::new(client.connect_live_danmaku(room_id).await?);
    /// let config = HeartbeatConfig {
    ///     interval: Duration::from_secs(30),
    /// };
    /// conn.start_heartbeat_loop(config).await;
    ///
    /// while let Ok(messages) = conn.recv().await {
    ///     for message in messages {
    ///         tracing::debug!(?message, "received danmaku message");
    ///     }
    /// }
    ///
    /// conn.stop_heartbeat_loop().await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn start_heartbeat_loop(self: &Arc<Self>, config: HeartbeatConfig) -> bool {
        let mut handle_guard = self.heartbeat_handle.lock().await;

        // Already running
        if let Some(ref handle) = *handle_guard {
            if !handle.is_finished() {
                return false;
            }
        }

        // Reset stop flag
        self.heartbeat_stop.store(false, Ordering::SeqCst);

        let stop_flag = Arc::clone(&self.heartbeat_stop);
        let connection = Arc::downgrade(self);
        let interval = config.interval;

        // Spawn the heartbeat task
        let handle = tokio::spawn(async move {
            loop {
                // Check if we should stop
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }

                // Wait for the interval
                tokio::time::sleep(interval).await;

                // Check again after sleeping
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }

                let Some(connection) = connection.upgrade() else {
                    break;
                };
                if connection.send_heartbeat().await.is_err() {
                    // Connection likely closed, stop the loop
                    break;
                }
            }
        });

        *handle_guard = Some(handle);
        true
    }
}

impl Drop for LiveDanmakuConnection {
    fn drop(&mut self) {
        self.heartbeat_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.heartbeat_handle.get_mut().take() {
            handle.abort();
        }
    }
}

/// Result of a receive operation with reconnection support
#[derive(Debug)]
pub enum ReconnectResult {
    /// Successfully received messages
    Messages(Vec<DanmakuMessage>),
    /// Connection was lost and successfully reconnected
    Reconnected {
        /// Number of reconnection attempts made
        attempts: u32,
    },
    /// Reconnection failed after exhausting all retries
    Failed {
        /// Total number of reconnection attempts made
        attempts: u32,
        /// The final error that caused failure
        error: BilibiliError,
    },
}

/// Live danmaku WebSocket connection with automatic reconnection support
///
/// This wrapper around [`LiveDanmakuConnection`] provides automatic reconnection
/// when the WebSocket connection is lost. It uses exponential backoff for
/// reconnection attempts.
///
/// # Example
/// ```no_run
/// use std::sync::Arc;
/// use std::time::Duration;
/// use synctv_media_providers::BilibiliError;
/// use synctv_media_providers::bilibili::{
///     BilibiliClient, ReconnectConfig, ReconnectResult,
/// };
///
/// # async fn demo() -> Result<(), BilibiliError> {
/// let room_id = 123_u64;
/// let client = Arc::new(BilibiliClient::new()?);
/// let reconnect_config = ReconnectConfig {
///     max_retries: 5,
///     initial_delay: Duration::from_secs(1),
///     max_delay: Duration::from_secs(30),
///     backoff_multiplier: 2.0,
/// };
///
/// let mut conn = client
///     .connect_live_danmaku_with_reconnect(room_id, reconnect_config)
///     .await?;
///
/// let event = conn.recv().await?;
/// assert!(matches!(
///     event,
///     ReconnectResult::Messages(_)
///         | ReconnectResult::Reconnected { .. }
///         | ReconnectResult::Failed { .. }
/// ));
/// # Ok(())
/// # }
/// ```
pub struct ReconnectableLiveDanmakuConnection {
    /// Reference to the Bilibili client for reconnection
    client: Arc<BilibiliClient>,
    /// The current underlying connection
    connection: Option<Arc<LiveDanmakuConnection>>,
    /// Room ID for reconnection
    room_id: u64,
    /// Reconnection configuration
    config: ReconnectConfig,
    /// Current retry count (resets on successful receive)
    current_retry: u32,
    /// Heartbeat configuration to restart after reconnection
    heartbeat_config: Option<HeartbeatConfig>,
    /// Flag to signal the connection should stop
    stop_flag: Arc<AtomicBool>,
}

impl ReconnectableLiveDanmakuConnection {
    /// Receive next danmaku messages with automatic reconnection
    ///
    /// This method will attempt to reconnect if the connection is lost.
    /// Returns [`ReconnectResult`] indicating whether messages were received,
    /// reconnection occurred, or reconnection failed.
    pub async fn recv(&mut self) -> Result<ReconnectResult, BilibiliError> {
        // Check if we should stop
        if self.stop_flag.load(Ordering::SeqCst) {
            return Err(BilibiliError::Parse(
                "Connection stopped by user".to_string(),
            ));
        }

        // Get current connection or attempt initial connection
        let Some(conn) = self.connection.as_ref() else {
            // No connection yet, try to establish one
            match self.try_reconnect().await {
                Ok(()) => {
                    return Ok(ReconnectResult::Reconnected {
                        attempts: self.current_retry,
                    });
                }
                Err(e) => {
                    let attempts = self.current_retry;
                    self.current_retry = 0;
                    return Ok(ReconnectResult::Failed { attempts, error: e });
                }
            }
        };

        // Try to receive from current connection
        match conn.recv().await {
            Ok(messages) => {
                // Reset retry count on successful receive
                self.current_retry = 0;
                Ok(ReconnectResult::Messages(messages))
            }
            Err(e) => {
                // Connection error, try to reconnect
                tracing::warn!(
                    "Danmaku WebSocket error for room {}: {}, attempting reconnection",
                    self.room_id,
                    e
                );

                // Stop the heartbeat loop on the old connection
                conn.stop_heartbeat_loop().await;

                // Clear the old connection
                self.connection = None;

                // Try to reconnect
                match self.try_reconnect().await {
                    Ok(()) => Ok(ReconnectResult::Reconnected {
                        attempts: self.current_retry,
                    }),
                    Err(e) => {
                        let attempts = self.current_retry;
                        self.current_retry = 0;
                        Ok(ReconnectResult::Failed { attempts, error: e })
                    }
                }
            }
        }
    }

    /// Attempt to reconnect with exponential backoff
    async fn try_reconnect(&mut self) -> Result<(), BilibiliError> {
        while self.current_retry < self.config.max_retries {
            // Check if we should stop
            if self.stop_flag.load(Ordering::SeqCst) {
                return Err(BilibiliError::Parse(
                    "Connection stopped by user".to_string(),
                ));
            }

            // Calculate delay for this retry
            if self.current_retry > 0 {
                let delay = self.config.delay_for_retry(self.current_retry - 1);
                tracing::debug!(
                    "Waiting {:?} before reconnection attempt {} for room {}",
                    delay,
                    self.current_retry,
                    self.room_id
                );
                tokio::time::sleep(delay).await;
            }

            tracing::info!(
                "Attempting danmaku reconnection {} for room {}",
                self.current_retry + 1,
                self.room_id
            );

            match self.client.connect_live_danmaku(self.room_id).await {
                Ok(new_conn) => {
                    let new_conn = Arc::new(new_conn);

                    // Start heartbeat loop if configured
                    if let Some(ref heartbeat_config) = self.heartbeat_config {
                        new_conn.start_heartbeat_loop(*heartbeat_config).await;
                    }

                    self.connection = Some(new_conn);
                    let attempts = self.current_retry + 1;
                    self.current_retry = 0;

                    tracing::info!(
                        "Successfully reconnected danmaku for room {} after {} attempt(s)",
                        self.room_id,
                        attempts
                    );

                    return Ok(());
                }
                Err(e) => {
                    self.current_retry += 1;
                    tracing::warn!(
                        "Reconnection attempt {} failed for room {}: {}",
                        self.current_retry,
                        self.room_id,
                        e
                    );
                }
            }
        }

        // Exhausted all retries
        Err(BilibiliError::Parse(format!(
            "Failed to reconnect after {} attempts",
            self.config.max_retries
        )))
    }

    /// Get the room ID
    pub const fn room_id(&self) -> u64 {
        self.room_id
    }

    /// Check if the connection is currently active
    pub const fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Get the current retry count
    pub const fn current_retry(&self) -> u32 {
        self.current_retry
    }

    /// Stop the connection and prevent further reconnection attempts
    pub async fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);

        if let Some(ref conn) = self.connection {
            conn.stop_heartbeat_loop().await;
        }

        self.connection = None;
    }

    /// Set the heartbeat configuration for reconnected sessions
    pub const fn set_heartbeat_config(&mut self, config: HeartbeatConfig) {
        self.heartbeat_config = Some(config);
    }

    /// Get the underlying connection if available
    pub const fn connection(&self) -> Option<&Arc<LiveDanmakuConnection>> {
        self.connection.as_ref()
    }
}

/// Danmaku message types
#[derive(Debug, Clone)]
pub enum DanmakuMessage {
    /// Chat message
    Chat {
        user: String,
        message: String,
        timestamp: u64,
    },
    /// User entered room
    UserEnter { user: String },
    /// Gift sent
    Gift {
        user: String,
        gift_name: String,
        count: u32,
    },
    /// Heartbeat response (online viewer count)
    Heartbeat { online_count: u32 },
    /// Unknown message type
    Unknown,
}

/// Build authentication packet for danmaku WebSocket
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots", test))]
fn build_auth_packet(room_id: u64, token: &str) -> Result<Vec<u8>, BilibiliError> {
    // Bilibili danmaku protocol format:
    // Header (16 bytes):
    // - packet_length: u32 (big-endian)
    // - header_length: u16 (big-endian) = 16
    // - protocol_version: u16 (big-endian) = 1
    // - operation: u32 (big-endian) = 7 (auth)
    // - sequence: u32 (big-endian) = 1
    // Body: JSON auth data

    let auth_json = serde_json::json!({
        "roomid": room_id,
        "uid": 0,
        "protover": 3,
        "key": token,
        "platform": "web",
        "type": 2
    });
    let body = serde_json::to_vec(&auth_json)
        .map_err(|err| BilibiliError::Parse(format!("failed to serialize auth packet: {err}")))?;

    let packet_length = 16usize
        .checked_add(body.len())
        .ok_or_else(|| BilibiliError::Parse("auth packet length overflow".to_string()))?;
    let packet_length_u32 = u32::try_from(packet_length)
        .map_err(|_| BilibiliError::Parse("auth packet length exceeds u32".to_string()))?;
    let mut packet = Vec::with_capacity(packet_length);

    // Header
    packet.extend_from_slice(&packet_length_u32.to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&1u16.to_be_bytes()); // protocol version
    packet.extend_from_slice(&7u32.to_be_bytes()); // operation = auth
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence

    // Body
    packet.extend_from_slice(&body);

    Ok(packet)
}

/// Build heartbeat packet for danmaku WebSocket
fn build_heartbeat_packet() -> Vec<u8> {
    // Heartbeat packet: operation = 2, empty body
    let mut packet = Vec::with_capacity(16);

    packet.extend_from_slice(&16u32.to_be_bytes()); // packet length
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&1u16.to_be_bytes()); // protocol version
    packet.extend_from_slice(&2u32.to_be_bytes()); // operation = heartbeat
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence

    packet
}

/// Maximum decompressed size for danmaku packets (16 MB).
/// Prevents decompression bombs from exhausting memory.
const MAX_DANMAKU_DECOMPRESS_SIZE: u64 = 16 * 1024 * 1024;

fn read_limited_danmaku_decompressed<R: std::io::Read>(
    decoder: R,
    compression: &str,
    compressed_len: usize,
) -> Option<Vec<u8>> {
    use std::io::Read;

    let limit = MAX_DANMAKU_DECOMPRESS_SIZE.saturating_add(1);
    let mut limited = decoder.take(limit);
    let mut out = Vec::new();
    if let Err(e) = limited.read_to_end(&mut out) {
        tracing::warn!(
            "Danmaku packet {compression} decompression failed: {e} (body length: {compressed_len} bytes)"
        );
        return None;
    }

    if out.len() as u64 > MAX_DANMAKU_DECOMPRESS_SIZE {
        tracing::warn!(
            compression,
            compressed_len,
            decompressed_limit = MAX_DANMAKU_DECOMPRESS_SIZE,
            "Danmaku packet exceeded decompressed size limit"
        );
        return None;
    }

    Some(out)
}

/// Parse danmaku packet from binary data.
///
/// A single binary frame may contain multiple sub-packets (especially when
/// compressed with zlib/brotli). This function collects all parsed messages
/// instead of returning only the first one.
fn parse_danmaku_packet(data: &[u8]) -> std::vec::Vec<DanmakuMessage> {
    if data.len() < 16 {
        tracing::warn!(
            "Danmaku packet too short: {} bytes (minimum 16 required)",
            data.len()
        );
        return Vec::new();
    }

    // Parse header
    let _packet_length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let _header_length = u16::from_be_bytes([data[4], data[5]]);
    let protocol_version = u16::from_be_bytes([data[6], data[7]]);
    let operation = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let _sequence = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);

    let mut messages = Vec::new();

    match operation {
        3
            // Heartbeat response (online count)
            if data.len() >= 20 => {
                let online_count = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
                messages.push(DanmakuMessage::Heartbeat { online_count });
            }
        5 => {
            // Notification message
            let body = &data[16..];

            let compressed_body;
            let payload = match protocol_version {
                0 | 1 => body,
                2 => {
                    let decoder = flate2::read::ZlibDecoder::new(body);
                    compressed_body =
                        match read_limited_danmaku_decompressed(decoder, "zlib", body.len()) {
                            Some(out) => out,
                            None => return Vec::new(),
                        };
                    compressed_body.as_slice()
                }
                3 => {
                    let decoder = brotli::Decompressor::new(body, 4096);
                    compressed_body =
                        match read_limited_danmaku_decompressed(decoder, "brotli", body.len()) {
                            Some(out) => out,
                            None => return Vec::new(),
                        };
                    compressed_body.as_slice()
                }
                _ => {
                    tracing::warn!(
                        "Danmaku packet has unknown protocol version: {} (expected 0, 1, 2, or 3)",
                        protocol_version
                    );
                    return Vec::new();
                }
            };

            // Compressed data contains concatenated sub-packets with headers;
            // uncompressed (v0/v1) body is raw JSON.
            if protocol_version == 0 || protocol_version == 1 {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(payload) {
                    if let Some(cmd) = json.get("cmd").and_then(|v| v.as_str()) {
                        messages.push(parse_danmaku_cmd(cmd, &json));
                    }
                }
            } else {
                // Iterate over ALL sub-packets inside the decompressed buffer
                let mut offset = 0usize;
                while offset + 16 <= payload.len() {
                    let pkt_len = u32::from_be_bytes([
                        payload[offset],
                        payload[offset + 1],
                        payload[offset + 2],
                        payload[offset + 3],
                    ]) as usize;
                    let hdr_len = u16::from_be_bytes([
                        payload[offset + 4],
                        payload[offset + 5],
                    ]) as usize;
                    if pkt_len < hdr_len || offset + pkt_len > payload.len() {
                        break;
                    }
                    let sub_body = &payload[offset + hdr_len..offset + pkt_len];
                    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(sub_body) {
                        if let Some(cmd) = json.get("cmd").and_then(|v| v.as_str()) {
                            messages.push(parse_danmaku_cmd(cmd, &json));
                        }
                    }
                    offset += pkt_len;
                }
            }
        }
        8 => {
            // Auth response - connection established
            tracing::debug!("Danmaku authentication successful");
        }
        _ => {}
    }

    messages
}

/// Parse danmaku command from JSON
fn parse_danmaku_cmd(cmd: &str, json: &serde_json::Value) -> DanmakuMessage {
    let command = cmd.split_once(':').map_or(cmd, |(command, _)| command);
    match command {
        "DANMU_MSG" => {
            // Chat message
            let info = json.get("info").and_then(|v| v.as_array());
            if let Some(info) = info {
                let message = info
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let user_info = info.get(2).and_then(|v| v.as_array());
                let user = user_info
                    .and_then(|arr| arr.get(1))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let timestamp = unix_timestamp_secs();

                return DanmakuMessage::Chat {
                    user,
                    message,
                    timestamp,
                };
            }
        }
        "INTERACT_WORD" => {
            // User enter room
            let data = json.get("data");
            if let Some(data) = data {
                let user = data
                    .get("uname")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                return DanmakuMessage::UserEnter { user };
            }
        }
        "SEND_GIFT" => {
            // Gift sent
            let data = json.get("data");
            if let Some(data) = data {
                let user = data
                    .get("uname")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let gift_name = data
                    .get("giftName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let count = u32::try_from(
                    data.get("num")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1),
                );
                return DanmakuMessage::Gift {
                    user,
                    gift_name,
                    count: count.unwrap_or(u32::MAX),
                };
            }
        }
        _ => {}
    }

    DanmakuMessage::Unknown
}

/// Video page information
#[derive(Debug, Clone)]
pub struct VideoPageInfo {
    pub title: String,
    pub actors: Vec<String>,
    pub video_infos: Vec<VideoInfoItem>,
    pub season_id: u64,
    pub cover: String,
    pub collection: Option<BilibiliCollectionInfo>,
    pub live_started_at: Option<i64>,
    pub is_currently_live: bool,
}

#[derive(Debug, Clone)]
pub struct VideoInfoItem {
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub epid: u64,
    pub page: u32,
    pub name: String,
    pub cover_image: String,
    pub live: bool,
    pub duration_seconds: u64,
    pub width: u64,
    pub height: u64,
}

#[derive(Debug, Clone)]
pub struct BilibiliCollectionInfo {
    pub mid: u64,
    pub season_id: u64,
    pub title: String,
    pub cover: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedBilibiliResource {
    pub normalized_url: String,
    pub resource: BilibiliResource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BilibiliResource {
    Video { bvid: String, aid: u64, page: u32 },
    PgcEpisode { episode_id: u64 },
    PgcSeason { season_id: u64 },
    Live { room_id: u64 },
    LiveRecommended,
    LiveArea { parent_area_id: u64, area_id: u64 },
    UpVideos { mid: u64 },
    FavoriteVideos { media_id: u64 },
    CollectionVideos { mid: u64, season_id: u64 },
    SeriesVideos { mid: u64, series_id: u64 },
    WatchLater,
}

/// A single segment (durl) from Bilibili's multi-segment video response.
#[derive(Debug, Clone)]
pub struct VideoSegment {
    pub url: String,
    pub size: u64,
    pub duration_millis: u64,
    pub backup_urls: Vec<String>,
}

/// Video URL information
///
/// Bilibili may return multiple durl segments for a single video (common for
/// older videos or certain formats). The `url` field contains the first segment
/// as a convenience; `segments` contains ALL segments.
#[derive(Debug, Clone)]
pub struct VideoUrlInfo {
    pub accept_quality: Vec<u32>,
    pub accept_description: Vec<String>,
    pub current_quality: u32,
    /// First segment URL (convenience accessor for single-segment callers).
    pub url: String,
    /// All video segments. For single-segment videos this has one entry.
    pub segments: Vec<VideoSegment>,
}

/// User information
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub is_login: bool,
    pub user_id: u64,
    pub username: String,
    pub face: String,
    pub is_vip: bool,
}

#[derive(Debug, Clone)]
pub struct FavoriteFolder {
    pub media_id: u64,
    pub title: String,
    pub media_count: u64,
    pub private: bool,
    pub default_folder: bool,
}

#[derive(Debug, Clone)]
pub struct FollowedPgcSeason {
    pub season_id: u64,
    pub title: String,
    pub cover: String,
    pub description: String,
    pub latest_episode: String,
}

#[derive(Debug, Clone)]
pub struct FollowedPgcList {
    pub items: Vec<FollowedPgcSeason>,
    pub total: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    pub max: u64,
    pub view_at: i64,
    pub business: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryResource {
    Video { bvid: String, aid: u64, cid: u64 },
    Pgc { epid: u64, cid: u64 },
    Live { room_id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub resource: HistoryResource,
    pub title: String,
    pub subtitle: String,
    pub cover: String,
    pub author: String,
    pub viewed_at: i64,
    pub progress_seconds: i64,
    pub duration_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    pub cursor: Option<HistoryCursor>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgcTimelineItem {
    pub episode_id: u64,
    pub season_id: u64,
    pub cid: u64,
    pub title: String,
    pub episode_title: String,
    pub cover: String,
    pub episode_cover: String,
    pub publish_at: i64,
    pub published: bool,
    pub date: String,
    pub day_of_week: u32,
    pub delayed: bool,
    pub delay_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgcSeasonIndexItem {
    pub season_id: u64,
    pub media_id: u64,
    pub first_episode_id: u64,
    pub title: String,
    pub subtitle: String,
    pub cover: String,
    pub first_episode_cover: String,
    pub badge: String,
    pub progress: String,
    pub score: String,
    pub finished: bool,
    pub season_type: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgcSeasonIndexPage {
    pub items: Vec<PgcSeasonIndexItem>,
    pub total: u64,
    pub has_more: bool,
}

/// Live stream information
#[derive(Debug, Clone)]
pub struct LiveStream {
    pub quality: u32,
    pub urls: Vec<LiveStreamUrl>,
    pub quality_name: String,
    pub protocol: String,
    pub format: String,
    pub codec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStreamUrl {
    pub host: String,
    pub url: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRoomListItem {
    pub room_id: u64,
    pub title: String,
    pub cover: String,
    pub author: String,
    pub author_id: u64,
    pub author_avatar: String,
    pub parent_area_id: u64,
    pub parent_area_name: String,
    pub area_id: u64,
    pub area_name: String,
    pub online: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRoomList {
    pub items: Vec<LiveRoomListItem>,
    pub total: Option<u64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAreaItem {
    pub id: u64,
    pub parent_id: u64,
    pub name: String,
    pub parent_name: String,
    pub picture: String,
    pub hot: bool,
}

fn live_room_from_card(card: types::LiveRoomCard) -> Option<LiveRoomListItem> {
    let room_id = card.roomid.value();
    if room_id == 0 {
        return None;
    }
    let cover = [card.user_cover, card.keyframe, card.cover]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default();
    Some(LiveRoomListItem {
        room_id,
        title: card.title,
        cover,
        author: card.uname,
        author_id: card.uid.value(),
        author_avatar: card.face,
        parent_area_id: card.area_v2_parent_id.value(),
        parent_area_name: card.area_v2_parent_name,
        area_id: card.area_v2_id.value(),
        area_name: card.area_v2_name,
        online: card.online.value(),
    })
}

/// Live danmaku server information
#[derive(Debug, Clone)]
pub struct LiveDanmuInfo {
    pub token: String,
    pub host_list: Vec<DanmuHost>,
}

#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
async fn resolve_validated_danmaku_addr(
    hostname: &str,
    port: u32,
    guard: &SsrfGuard,
) -> Result<std::net::SocketAddr, BilibiliError> {
    let port = u16::try_from(port).map_err(|_| {
        BilibiliError::Parse(format!(
            "WebSocket port out of range for host {hostname}: {port}"
        ))
    })?;

    if guard.is_host_blocked(hostname) {
        return Err(BilibiliError::Network(format!(
            "WebSocket host is blocked by SSRF policy: {hostname}"
        )));
    }

    if let Ok(ip) = hostname.parse::<std::net::IpAddr>() {
        if guard.is_ip_blocked(&ip) {
            return Err(BilibiliError::Network(format!(
                "WebSocket host IP is blocked by SSRF policy: {ip}"
            )));
        }
        return Ok(std::net::SocketAddr::new(ip, port));
    }

    let resolved = tokio::net::lookup_host((hostname, port))
        .await
        .map_err(|e| {
            BilibiliError::Network(format!("Failed to resolve WebSocket host {hostname}: {e}"))
        })?
        .collect::<Vec<_>>();

    if resolved.is_empty() {
        return Err(BilibiliError::Network(format!(
            "WebSocket host resolved to no addresses: {hostname}"
        )));
    }

    if let Some(blocked_ip) = resolved
        .iter()
        .map(std::net::SocketAddr::ip)
        .find(|ip| guard.is_ip_blocked(ip))
    {
        return Err(BilibiliError::Network(format!(
            "WebSocket host resolves to blocked IP: {blocked_ip}"
        )));
    }

    Ok(resolved[0])
}

/// Danmaku server host
#[derive(Debug, Clone)]
pub struct DanmuHost {
    pub host: String,
    pub port: u32,
    pub wss_port: u32,
    pub ws_port: u32,
}

/// DASH stream data (structured for upper layer to generate MPD)
#[derive(Debug, Clone)]
pub struct DashData {
    pub duration: f64,
    pub min_buffer_time: f64,
    pub video_streams: Vec<VideoStreamData>,
    pub audio_streams: Vec<AudioStreamData>,
}

/// Video stream representation
#[derive(Debug, Clone)]
pub struct VideoStreamData {
    pub id: u64,
    pub quality_name: String,
    pub base_url: String,
    pub backup_urls: Vec<String>,
    pub mime_type: String,
    pub codecs: String,
    pub width: u64,
    pub height: u64,
    pub frame_rate: String,
    pub bandwidth: u64,
    pub codecid: u32,
    pub sar: String,
    pub start_with_sap: u64,
    pub segment_base: SegmentBaseData,
}

/// Audio stream representation
#[derive(Debug, Clone)]
pub struct AudioStreamData {
    pub id: u64,
    pub quality_name: String,
    pub base_url: String,
    pub backup_urls: Vec<String>,
    pub mime_type: String,
    pub codecs: String,
    pub bandwidth: u64,
    pub audio_sampling_rate: u32,
    pub start_with_sap: u64,
    pub segment_base: SegmentBaseData,
}

/// Segment base information
#[derive(Debug, Clone)]
pub struct SegmentBaseData {
    pub index_range: String,
    pub initialization_range: String,
}

// From trait implementations for proto conversion

impl From<&SegmentBaseData> for crate::transport_dto::bilibili::SegmentBase {
    fn from(data: &SegmentBaseData) -> Self {
        Self {
            index_range: data.index_range.clone(),
            initialization_range: data.initialization_range.clone(),
        }
    }
}

impl From<&VideoStreamData> for crate::transport_dto::bilibili::VideoStream {
    fn from(data: &VideoStreamData) -> Self {
        Self {
            id: data.id,
            base_url: data.base_url.clone(),
            mime_type: data.mime_type.clone(),
            codecs: data.codecs.clone(),
            width: data.width,
            height: data.height,
            frame_rate: data.frame_rate.clone(),
            bandwidth: data.bandwidth,
            codecid: data.codecid,
            start_with_sap: data.start_with_sap,
            segment_base: Some((&data.segment_base).into()),
            backup_urls: data.backup_urls.clone(),
            quality_name: data.quality_name.clone(),
            sar: data.sar.clone(),
        }
    }
}

impl From<&AudioStreamData> for crate::transport_dto::bilibili::AudioStream {
    fn from(data: &AudioStreamData) -> Self {
        Self {
            id: data.id,
            base_url: data.base_url.clone(),
            mime_type: data.mime_type.clone(),
            codecs: data.codecs.clone(),
            bandwidth: data.bandwidth,
            start_with_sap: data.start_with_sap,
            segment_base: Some((&data.segment_base).into()),
            audio_sampling_rate: data.audio_sampling_rate,
            backup_urls: data.backup_urls.clone(),
            quality_name: data.quality_name.clone(),
        }
    }
}

impl From<&DashData> for crate::transport_dto::bilibili::DashInfo {
    fn from(data: &DashData) -> Self {
        Self {
            duration: data.duration,
            min_buffer_time: data.min_buffer_time,
            video_streams: data
                .video_streams
                .iter()
                .map(std::convert::Into::into)
                .collect(),
            audio_streams: data
                .audio_streams
                .iter()
                .map(std::convert::Into::into)
                .collect(),
        }
    }
}

/// Parse DASH info into structured format
/// Returns (`regular_dash`, `hevc_dash`) where HEVC codecs are separated
fn parse_dash_info(
    dash_info: &types::DashInfo,
    support_formats: &[types::SupportFormat],
) -> (DashData, DashData) {
    let duration = dash_info.duration;
    let min_buffer_time = dash_info.min_buffer_time;

    // Build quality ID → name mapping from support_formats
    let quality_names: HashMap<u64, String> = support_formats
        .iter()
        .map(|f| (f.quality, f.new_description.clone()))
        .collect();

    // Parse audio streams (shared by both regular and HEVC)
    let parsed_audios: Vec<AudioStreamData> = dash_info
        .audio
        .iter()
        .chain(dash_info.dolby.iter().flat_map(|dolby| &dolby.audio))
        .chain(dash_info.flac.iter().filter_map(|flac| flac.audio.as_ref()))
        .map(|audio| AudioStreamData {
            id: audio.id,
            quality_name: match audio.id {
                30_251 => "Hi-Res FLAC".to_string(),
                30_250 => "Dolby Atmos".to_string(),
                30_280 => "192K".to_string(),
                30_232 => "132K".to_string(),
                30_216 => "64K".to_string(),
                id => format!("Audio {id}"),
            },
            base_url: audio.base_url.clone(),
            backup_urls: audio.backup_url.clone(),
            mime_type: audio.mime_type.clone(),
            codecs: audio.codecs.clone(),
            bandwidth: audio.bandwidth,
            audio_sampling_rate: audio.audio_sampling_rate,
            start_with_sap: audio.start_with_sap,
            segment_base: SegmentBaseData {
                index_range: audio.segment_base.index_range.clone(),
                initialization_range: audio.segment_base.initialization.clone(),
            },
        })
        .collect();

    // Separate videos into regular and HEVC
    let mut regular_videos = Vec::new();
    let mut hevc_videos = Vec::new();

    for video in &dash_info.video {
        let quality_name = quality_names
            .get(&video.id)
            .cloned()
            .unwrap_or_else(|| format!("{}P", video.height));

        let video_data = VideoStreamData {
            id: video.id,
            quality_name,
            base_url: video.base_url.clone(),
            backup_urls: video.backup_url.clone(),
            mime_type: video.mime_type.clone(),
            codecs: video.codecs.clone(),
            width: video.width,
            height: video.height,
            frame_rate: video.frame_rate.clone(),
            bandwidth: video.bandwidth,
            codecid: video.codecid,
            sar: video.sar.clone(),
            start_with_sap: video.start_with_sap,
            segment_base: SegmentBaseData {
                index_range: video.segment_base.index_range.clone(),
                initialization_range: video.segment_base.initialization.clone(),
            },
        };

        if video_data.codecs.starts_with("hev1") || video_data.codecs.starts_with("hvc1") {
            hevc_videos.push(video_data);
        } else {
            regular_videos.push(video_data);
        }
    }

    let regular_dash = DashData {
        duration,
        min_buffer_time,
        video_streams: regular_videos,
        audio_streams: parsed_audios.clone(),
    };

    let hevc_dash = DashData {
        duration,
        min_buffer_time,
        video_streams: hevc_videos,
        audio_streams: parsed_audios,
    };

    (regular_dash, hevc_dash)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
