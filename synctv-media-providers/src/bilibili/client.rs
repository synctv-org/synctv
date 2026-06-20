//! Bilibili HTTP Client
#![allow(clippy::must_use_candidate)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use md5::{Digest, Md5};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use synctv_common::ssrf::SsrfGuard;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tokio_tungstenite::client_async_tls_with_config;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots", test))]
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use super::error::{check_response, json_with_limit, BilibiliError};
use super::types;
use crate::error::with_retry;

static RE_BVID: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"BV[a-zA-Z0-9]+"));
static RE_EPID: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| Regex::new(r"ep(\d+)"));
static RE_SSID: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| Regex::new(r"ss(\d+)"));
static RE_LIVE_ROOM: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?:/live)?/(\d+)(?:[/?#]|$)"));

use crate::PROVIDER_USER_AGENT as USER_AGENT;
const REFERER: &str = "https://www.bilibili.com";
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

fn quality_to_u32(quality: u64, endpoint: &'static str) -> Result<u32, BilibiliError> {
    u32::try_from(quality)
        .map_err(|_| BilibiliError::Parse(format!("{endpoint} quality {quality} exceeds u32")))
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
pub(crate) struct WbiState {
    key_cache: tokio::sync::Mutex<Option<WbiKeys>>,
    refresh_in_progress: AtomicUsize,
    refresh_notify: tokio::sync::Notify,
    consecutive_failures: AtomicUsize,
    #[cfg(test)]
    api_call_count: AtomicUsize,
}

impl Default for WbiState {
    fn default() -> Self {
        Self {
            key_cache: tokio::sync::Mutex::new(None),
            refresh_in_progress: AtomicUsize::new(0),
            refresh_notify: tokio::sync::Notify::new(),
            consecutive_failures: AtomicUsize::new(0),
            #[cfg(test)]
            api_call_count: AtomicUsize::new(0),
        }
    }
}

/// Maximum number of consecutive refresh failures before we give up and return an error.
/// This prevents infinite waiting when the WBI API is persistently unavailable.
const WBI_MAX_CONSECUTIVE_FAILURES: usize = 3;

/// Maximum time to wait for a refresh notification before timing out.
/// This prevents tasks from waiting indefinitely if the refreshing task fails silently.
const WBI_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// WBI key cache TTL (refresh keys every 30 minutes).
const WBI_KEY_TTL: Duration = Duration::from_mins(30);

impl WbiState {
    pub(crate) async fn get_valid_wbi_key(&self) -> Option<String> {
        let guard = self.key_cache.lock().await;
        guard
            .as_ref()
            .filter(|k| k.is_valid())
            .map(|k| k.mixin_key.clone())
    }

    pub(crate) async fn set_wbi_key(&self, mixin_key: String) {
        let mut guard = self.key_cache.lock().await;
        *guard = Some(WbiKeys {
            mixin_key,
            expires_at: std::time::Instant::now() + WBI_KEY_TTL,
        });
    }

    fn try_claim_refresh_lock(&self) -> bool {
        self.refresh_in_progress
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn release_refresh_lock_on_success_and_notify(&self) {
        self.consecutive_failures.store(0, Ordering::Release);
        self.refresh_in_progress.store(0, Ordering::Release);
        self.refresh_notify.notify_waiters();
    }

    fn release_refresh_lock_on_failure_and_notify(&self) -> bool {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
        self.refresh_in_progress.store(0, Ordering::Release);
        self.refresh_notify.notify_waiters();
        failures >= WBI_MAX_CONSECUTIVE_FAILURES
    }

    fn has_exceeded_max_failures(&self) -> bool {
        self.consecutive_failures.load(Ordering::Acquire) >= WBI_MAX_CONSECUTIVE_FAILURES
    }

    fn reset_consecutive_failures(&self) {
        self.consecutive_failures.store(0, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) async fn reset_for_tests(&self) {
        {
            let mut guard = self.key_cache.lock().await;
            *guard = None;
        }
        self.refresh_in_progress.store(0, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
        self.api_call_count.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn api_call_count(&self) -> usize {
        self.api_call_count.load(Ordering::Relaxed)
    }
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

    pub(crate) const fn new_with_transport(
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

    pub(crate) const fn with_cookies_and_transport(
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
            wbi_state,
            endpoints,
            ssrf_guard,
        }
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

    /// Get WBI mixin key, fetching and caching it if necessary.
    /// Internal method with optional force refresh.
    ///
    /// Fetches from Bilibili's nav API and caches in memory for 30 minutes.
    /// Uses coordinated refresh to prevent thundering herd when cache expires.
    ///
    /// # Failure Handling
    /// - Uses timeout to prevent indefinite waiting on notification
    /// - Tracks consecutive failures and returns error after max failures exceeded
    async fn get_wbi_mixin_key_internal(
        &self,
        force_refresh: bool,
    ) -> Result<String, BilibiliError> {
        // Check cache (unless force refresh)
        if !force_refresh {
            if let Some(key) = self.wbi_state.get_valid_wbi_key().await {
                // Reset failure counter on successful cache hit
                self.wbi_state.reset_consecutive_failures();
                return Ok(key);
            }
        }

        // Check if we've exceeded max consecutive failures - fail fast
        if self.wbi_state.has_exceeded_max_failures() {
            return Err(BilibiliError::Parse(
                "WBI key refresh unavailable: too many consecutive failures".to_string(),
            ));
        }

        // Try to claim the refresh lock. Only one task will succeed.
        if self.wbi_state.try_claim_refresh_lock() {
            // We got the lock - we are responsible for refreshing.
            let result = self.fetch_and_cache_wbi_key().await;
            match &result {
                Ok(_) => {
                    self.wbi_state.release_refresh_lock_on_success_and_notify();
                }
                Err(_) => {
                    self.wbi_state.release_refresh_lock_on_failure_and_notify();
                }
            }
            result
        } else {
            // Another task is refreshing. Wait for notification with timeout.
            // This prevents thundering herd and reduces unnecessary CPU usage.
            let notify_result = tokio::time::timeout(
                WBI_REFRESH_TIMEOUT,
                self.wbi_state.refresh_notify.notified(),
            )
            .await;

            if notify_result.is_err() {
                // Timeout waiting for notification - the refreshing task may have failed silently
                // or is taking too long. Return an error instead of waiting indefinitely.
                return Err(BilibiliError::Parse(
                    "WBI key refresh timeout: waited too long for refresh".to_string(),
                ));
            }

            // After being notified, check the cache again.
            if let Some(key) = self.wbi_state.get_valid_wbi_key().await {
                self.wbi_state.reset_consecutive_failures();
                return Ok(key);
            }

            // Check if we've exceeded max failures before retrying
            if self.wbi_state.has_exceeded_max_failures() {
                return Err(BilibiliError::Parse(
                    "WBI key refresh unavailable: too many consecutive failures".to_string(),
                ));
            }

            // If cache is still empty after notification (refresh failed),
            // try to refresh ourselves as a fallback.
            if self.wbi_state.try_claim_refresh_lock() {
                let result = self.fetch_and_cache_wbi_key().await;
                match &result {
                    Ok(_) => {
                        self.wbi_state.release_refresh_lock_on_success_and_notify();
                    }
                    Err(_) => {
                        self.wbi_state.release_refresh_lock_on_failure_and_notify();
                    }
                }
                result
            } else {
                // Another task beat us to it - wait again with timeout
                let notify_result = tokio::time::timeout(
                    WBI_REFRESH_TIMEOUT,
                    self.wbi_state.refresh_notify.notified(),
                )
                .await;

                if notify_result.is_err() {
                    return Err(BilibiliError::Parse(
                        "WBI key refresh timeout: waited too long for refresh".to_string(),
                    ));
                }

                // Check cache one more time
                if let Some(key) = self.wbi_state.get_valid_wbi_key().await {
                    self.wbi_state.reset_consecutive_failures();
                    return Ok(key);
                }

                // Check if we've exceeded max failures
                if self.wbi_state.has_exceeded_max_failures() {
                    return Err(BilibiliError::Parse(
                        "WBI key refresh unavailable: too many consecutive failures".to_string(),
                    ));
                }

                self.wbi_state
                    .get_valid_wbi_key()
                    .await
                    .ok_or_else(|| BilibiliError::Parse("WBI key refresh failed".to_string()))
            }
        }
    }

    /// Fetch WBI key from Bilibili API and cache it.
    async fn fetch_and_cache_wbi_key(&self) -> Result<String, BilibiliError> {
        #[cfg(test)]
        self.wbi_state
            .api_call_count
            .fetch_add(1, Ordering::Relaxed);

        let url = "https://api.bilibili.com/x/web-interface/nav";
        let req = self.add_cookies(self.client.get(url).header("Referer", REFERER));
        let resp = check_response(req.send().await?).await?;
        let json: types::NavResp = json_with_limit(resp).await?;

        let wbi_img = match json.data.wbi_img {
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
        self.wbi_state.set_wbi_key(mixin_key.clone()).await;

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
        self.cookies.as_ref().map(|cookies| {
            cookies
                .iter()
                .map(|(k, v)| sanitize_cookie_pair(k, v))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    /// Add cookies to request.
    /// Cookie values are sanitized to prevent header injection via \r\n.
    fn add_cookies(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.build_cookie_header() {
            Some(cookie_str) => req.header("Cookie", cookie_str),
            None => req,
        }
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

        let url = "https://api.bilibili.com/x/frontend/finger/spi";
        let req = self
            .client
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", "https://www.bilibili.com");

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

                let mut video_infos = Vec::new();
                for page in data.pages {
                    video_infos.push(VideoInfoItem {
                        bvid: data.bvid.clone(),
                        cid: page.cid,
                        epid: 0,
                        name: page.part,
                        cover_image: data.pic.clone(),
                        live: false,
                    });
                }

                Ok(VideoPageInfo {
                    title,
                    actors: vec![owner_name],
                    video_infos,
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

                let data = json.data;
                let accept_quality: Vec<u32> = data
                    .accept_quality
                    .iter()
                    .map(|&q| quality_to_u32(q, "video URL"))
                    .collect::<Result<_, _>>()?;
                let accept_description = data.accept_description;
                let current_quality = quality_to_u32(data.quality, "video URL")?;
                let segments: Vec<VideoSegment> = data
                    .durl
                    .iter()
                    .map(|d| VideoSegment {
                        url: d.url.clone(),
                        size: d.size,
                    })
                    .collect();
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
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let bvid = bvid.to_string();
        let cid_str = cid.to_string();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let bvid = bvid.clone();
            let cid_str = cid_str.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/x/player/v2");
                if bvid.is_empty() {
                    req = req.query(&[("aid", &aid.to_string()), ("cid", &cid_str)]);
                } else {
                    req = req.query(&[("bvid", &bvid), ("cid", &cid_str)]);
                }
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::PlayerV2InfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "subtitles"));
                }

                let mut subtitles = HashMap::new();
                for sub in json.data.subtitle.subtitles {
                    let name = sub.lan_doc;
                    let url = if sub.subtitle_url.starts_with("http") {
                        sub.subtitle_url
                    } else {
                        format!("https:{}", sub.subtitle_url)
                    };
                    if name.is_empty() || url.is_empty() {
                        continue;
                    }
                    subtitles.insert(name, url);
                }

                Ok(subtitles)
            }
        })
        .await
    }

    /// Get user information
    pub async fn user_info(&self) -> Result<UserInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client
                    .get("https://api.bilibili.com/x/web-interface/nav")
                    .header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::NavResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "user info"));
                }

                let data = json.data;
                Ok(UserInfo {
                    is_login: data.is_login,
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

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/pgc/view/web/season");
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
                let actors_str = result.actors;
                let actors = if actors_str.is_empty() {
                    vec![]
                } else {
                    vec![actors_str]
                };

                let mut video_infos = Vec::new();
                for ep in result.episodes {
                    video_infos.push(VideoInfoItem {
                        bvid: ep.bvid,
                        cid: ep.cid,
                        epid: ep.ep_id,
                        name: if ep.long_title.is_empty() {
                            ep.title
                        } else {
                            ep.long_title
                        },
                        cover_image: ep.cover,
                        live: false,
                    });
                }

                Ok(VideoPageInfo {
                    title,
                    actors,
                    video_infos,
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

                let result = json.result;
                let accept_quality: Vec<u32> = result
                    .accept_quality
                    .iter()
                    .map(|&q| quality_to_u32(q, "PGC URL"))
                    .collect::<Result<_, _>>()?;
                let accept_description = result.accept_description;
                let current_quality = quality_to_u32(result.quality, "PGC URL")?;
                let segments: Vec<VideoSegment> = result
                    .durl
                    .iter()
                    .map(|d| VideoSegment {
                        url: d.url.clone(),
                        size: d.size,
                    })
                    .collect();
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

                let dash_info = json.result.dash.ok_or_else(|| BilibiliError::Api {
                    code: i64::from(json.result.code),
                    message: "PGC playurl response did not include DASH streams".to_string(),
                })?;
                let (regular_dash, hevc_dash) =
                    parse_dash_info(&dash_info, &json.result.support_formats);

                Ok((regular_dash, hevc_dash))
            }
        })
        .await
    }

    /// Match URL to extract video type and ID
    pub fn match_url(url: &str) -> Result<(String, String), BilibiliError> {
        let bvid_regex = RE_BVID
            .as_ref()
            .map_err(|err| BilibiliError::Parse(format!("Invalid BVID regex: {err}")))?;
        let epid_regex = RE_EPID
            .as_ref()
            .map_err(|err| BilibiliError::Parse(format!("Invalid EPID regex: {err}")))?;
        let ssid_regex = RE_SSID
            .as_ref()
            .map_err(|err| BilibiliError::Parse(format!("Invalid SSID regex: {err}")))?;
        let live_room_regex = RE_LIVE_ROOM
            .as_ref()
            .map_err(|err| BilibiliError::Parse(format!("Invalid live room regex: {err}")))?;

        // Video: BV id
        if let Some(bvid) = bvid_regex.find(url).map(|m| m.as_str().to_string()) {
            return Ok(("bv".to_string(), bvid));
        }

        // Bangumi/Anime: ep id or ss id
        if url.contains("/bangumi/play/") {
            if let Some(ep_match) = epid_regex.captures(url) {
                if let Some(ep_id) = ep_match.get(1) {
                    return Ok(("ep".to_string(), ep_id.as_str().to_string()));
                }
            }
            if let Some(ss_match) = ssid_regex.captures(url) {
                if let Some(ss_id) = ss_match.get(1) {
                    return Ok(("ss".to_string(), ss_id.as_str().to_string()));
                }
            }
        }

        // Live: room id
        if url.contains("/live/") || url.contains("live.bilibili.com") {
            if let Some(room_match) = live_room_regex.captures(url) {
                if let Some(room_id) = room_match.get(1) {
                    return Ok(("live".to_string(), room_id.as_str().to_string()));
                }
            }
        }

        Err(BilibiliError::Parse("Cannot parse URL type".to_string()))
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

                let video_info = VideoInfoItem {
                    bvid: String::new(),
                    cid: room_id,
                    epid: 0,
                    name: title.clone(),
                    cover_image: data.user_cover,
                    live: true,
                };

                Ok(VideoPageInfo {
                    title,
                    actors: vec![uname],
                    video_infos: vec![video_info],
                })
            }
        })
        .await
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
                let mut req = client.get(play_info_url).query(&[
                    ("room_id", room_id_str.as_str()),
                    ("protocol", "0,1"),
                    ("format", "0,1,2"),
                    ("codec", "0,1"),
                    ("qn", "10000"),
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

                let mut streams = Vec::new();

                let stream_list = json
                    .data
                    .playurl_info
                    .as_ref()
                    .and_then(|info| info.playurl.as_ref())
                    .map(|playurl| &playurl.stream[..])
                    .unwrap_or_default();

                for stream in stream_list {
                    // Filter by protocol: when hls=true, only include HLS streams;
                    // when hls=false, only include HTTP-FLV streams.
                    let dominated = if hls { "http_hls" } else { "http_stream" };
                    if !stream.protocol_name.is_empty() && stream.protocol_name != dominated {
                        continue;
                    }
                    for format in &stream.format {
                        for codec in &format.codec {
                            let quality = quality_to_u32(codec.current_qn, "live stream URL")?;
                            let desc = codec
                                .accept_qn
                                .first()
                                .map_or_else(|| "Unknown".to_string(), |q| format!("{q}P"));

                            let urls: Vec<String> = codec
                                .url_info
                                .iter()
                                .filter(|info| !info.host.is_empty())
                                .map(|info| {
                                    format!("{}{}{}", info.host, codec.base_url, info.extra)
                                })
                                .collect();

                            if !urls.is_empty() {
                                streams.push(LiveStream {
                                    quality,
                                    urls,
                                    desc,
                                });
                            }
                        }
                    }
                }

                Ok(streams)
            }
        })
        .await
    }

    /// Get live danmaku server info
    pub async fn get_live_danmu_info(&self, room_id: u64) -> Result<LiveDanmuInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let danmu_info_url = self
            .endpoints
            .live_api_url("/xlive/web-room/v1/index/getDanmuInfo");

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let danmu_info_url = danmu_info_url.clone();
            async move {
                let mut req = client.get(danmu_info_url).query(&[("id", room_id)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::GetLiveDanmuInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(bilibili_api_error(i64::from(json.code), "live danmaku"));
                }

                let data = json.data;
                let token = data.token;
                let host_list: Vec<DanmuHost> = data
                    .host_list
                    .into_iter()
                    .map(|h| DanmuHost {
                        host: h.host,
                        port: h.port,
                        wss_port: h.wss_port,
                        ws_port: h.ws_port,
                    })
                    .collect();

                Ok(LiveDanmuInfo { token, host_list })
            }
        })
        .await
    }

    /// Connect to live danmaku WebSocket and return a message stream
    ///
    /// Returns a tuple of (sender, receiver) for bidirectional communication
    #[cfg_attr(
        not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")),
        allow(clippy::unused_async, clippy::unused_async_trait_impl)
    )]
    pub async fn connect_live_danmaku(
        &self,
        room_id: u64,
    ) -> Result<LiveDanmakuConnection, BilibiliError> {
        #[cfg(not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")))]
        {
            let _ = room_id;
            Err(BilibiliError::InvalidConfig(
                "Bilibili live danmaku requires a TLS root feature".to_string(),
            ))
        }

        #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
        {
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
        }
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
    /// After calling this, you can call [`start_heartbeat_loop_arc`](Self::start_heartbeat_loop_arc)
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
    /// Start automatic heartbeat loop with `Arc<Self>`.
    ///
    /// This is the recommended way to start the heartbeat loop when you have
    /// an `Arc<LiveDanmakuConnection>`.
    ///
    /// # Arguments
    /// * `self_ptr` - An `Arc` reference to this connection
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
    /// conn.start_heartbeat_loop_arc(Arc::clone(&conn), config).await;
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
    pub async fn start_heartbeat_loop_arc(
        self: &Arc<Self>,
        self_ptr: Arc<Self>,
        config: HeartbeatConfig,
    ) -> bool {
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

                // Send heartbeat
                if self_ptr.send_heartbeat().await.is_err() {
                    // Connection likely closed, stop the loop
                    break;
                }
            }
        });

        *handle_guard = Some(handle);
        true
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
                        new_conn
                            .start_heartbeat_loop_arc(Arc::clone(&new_conn), *heartbeat_config)
                            .await;
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
    match cmd {
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
}

#[derive(Debug, Clone)]
pub struct VideoInfoItem {
    pub bvid: String,
    pub cid: u64,
    pub epid: u64,
    pub name: String,
    pub cover_image: String,
    pub live: bool,
}

/// A single segment (durl) from Bilibili's multi-segment video response.
#[derive(Debug, Clone)]
pub struct VideoSegment {
    pub url: String,
    pub size: u64,
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
    pub username: String,
    pub face: String,
    pub is_vip: bool,
}

/// Live stream information
#[derive(Debug, Clone)]
pub struct LiveStream {
    pub quality: u32,
    pub urls: Vec<String>,
    pub desc: String,
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
    pub sar: String,
    pub start_with_sap: u64,
    pub segment_base: SegmentBaseData,
}

/// Audio stream representation
#[derive(Debug, Clone)]
pub struct AudioStreamData {
    pub id: u64,
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

impl From<&SegmentBaseData> for crate::grpc::bilibili::SegmentBase {
    fn from(data: &SegmentBaseData) -> Self {
        Self {
            index_range: data.index_range.clone(),
            initialization_range: data.initialization_range.clone(),
        }
    }
}

impl From<&VideoStreamData> for crate::grpc::bilibili::VideoStream {
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
            start_with_sap: data.start_with_sap,
            segment_base: Some((&data.segment_base).into()),
        }
    }
}

impl From<&AudioStreamData> for crate::grpc::bilibili::AudioStream {
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
        }
    }
}

impl From<&DashData> for crate::grpc::bilibili::DashInfo {
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
        .map(|audio| AudioStreamData {
            id: audio.id,
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
