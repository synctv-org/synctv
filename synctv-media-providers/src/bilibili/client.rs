//! Bilibili HTTP Client

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::LazyLock;
use std::time::Duration;

use governor::{Quota, RateLimiter};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use md5::{Md5, Digest};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};

use super::error::{BilibiliError, check_response, json_with_limit};
use super::types::{self as types, VideoInfo, Quality, PlayUrlInfo, DurlItem, AnimeInfo};
use crate::error::with_retry;

/// Default Bilibili API rate limit: 5 requests per second.
const DEFAULT_RATE_LIMIT_PER_SECOND: u32 = 5;

type BilibiliRateLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

// Pre-compiled regexes using std::sync::LazyLock (no external crate needed).
// These patterns are compile-time constants; Regex::new cannot fail on them.
static RE_BVID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"BV[a-zA-Z0-9]+").expect("invalid BVID regex"));
static RE_EPID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ep(\d+)").expect("invalid EPID regex"));
static RE_SSID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ss(\d+)").expect("invalid SSID regex"));
static RE_LIVE_ROOM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/live/(\d+)").expect("invalid live room regex"));

use crate::error::PROVIDER_USER_AGENT as USER_AGENT;
const REFERER: &str = "https://www.bilibili.com";

/// Shared HTTP client for all Bilibili requests (connection pooling)
/// Redirects are disabled to prevent SSRF via redirect to private IPs.
static SHARED_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Failed to build Bilibili shared HTTP client")
});

/// Shared rate limiter for all Bilibili requests.
/// This must be global so that the token bucket is not reset when a new
/// `BilibiliClient` is created per-request in the service layer.
///
/// **Known limitation**: This is an in-memory rate limiter, so in multi-replica
/// deployments the effective rate is `N * DEFAULT_RATE_LIMIT_PER_SECOND` where N
/// is the number of running instances. For Bilibili's API this is acceptable
/// because the limit is conservative (5 req/s) and replicas are typically few.
/// A Redis-backed limiter (e.g. via `redis-cell` or a Lua script) would be
/// needed if the deployment scales to many replicas.
static SHARED_RATE_LIMITER: LazyLock<std::sync::Arc<BilibiliRateLimiter>> = LazyLock::new(|| {
    let quota = Quota::per_second(
        NonZeroU32::new(DEFAULT_RATE_LIMIT_PER_SECOND).expect("rate limit must be > 0"),
    );
    std::sync::Arc::new(RateLimiter::direct(quota))
});

// ============================================================================
// WBI Signing
// ============================================================================

/// Predefined character-index table for generating the WBI mixin key.
/// This table is derived from Bilibili's frontend JavaScript and maps
/// positions in the concatenated `img_key + sub_key` string to positions
/// in the resulting mixin key.
const MIXIN_KEY_ENC_TAB: [u8; 64] = [
    46, 47, 18,  2, 53,  8, 23, 32, 15, 50, 10, 31, 58,  3, 45, 35,
    27, 43,  5, 49, 33,  9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13,
    37, 48,  7, 16, 24, 55, 40, 61, 26, 17,  0,  1, 60, 51, 30,  4,
    22, 25, 54, 21, 56, 59,  6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// Cached WBI keys with expiration timestamp.
struct WbiKeys {
    mixin_key: String,
    expires_at: std::time::Instant,
}

/// Global WBI key cache. Uses Mutex instead of RwLock so that when multiple
/// tasks hit a cache miss simultaneously, only the first one refreshes from
/// the API while the rest wait and then read the updated value.
static WBI_KEY_CACHE: LazyLock<tokio::sync::Mutex<Option<WbiKeys>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

/// WBI key cache TTL (refresh keys every 30 minutes).
const WBI_KEY_TTL: Duration = Duration::from_secs(30 * 60);

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
    let wts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

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

    // Build query string for hashing
    let query_str: String = all_params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    // Compute MD5 hash of query_string + mixin_key
    let to_hash = format!("{query_str}{mixin_key}");
    let mut hasher = Md5::new();
    hasher.update(to_hash.as_bytes());
    let w_rid = format!("{:x}", hasher.finalize());

    // Add w_rid to the params
    all_params.push(("w_rid".to_string(), w_rid));

    all_params
}

/// Cookie storage with expiration tracking
#[derive(Clone)]
struct CookieStore {
    cookies: HashMap<String, String>,
    expires_at: std::time::Instant,
}

impl CookieStore {
    /// Check if cookies are expired (30 days TTL)
    fn is_expired(&self) -> bool {
        std::time::Instant::now() >= self.expires_at
    }

    /// Create new cookie store with default TTL of 30 days
    fn new(cookies: HashMap<String, String>) -> Self {
        Self {
            cookies,
            expires_at: std::time::Instant::now() + Duration::from_secs(30 * 24 * 60 * 60),
        }
    }

    /// Refresh the expiration time
    fn refresh(&mut self) {
        self.expires_at = std::time::Instant::now() + Duration::from_secs(30 * 24 * 60 * 60);
    }
}

/// Per-user cookie cache for authenticated sessions.
/// Keyed by user identifier to prevent cross-user cookie leakage.
static COOKIE_CACHE: LazyLock<tokio::sync::RwLock<HashMap<String, CookieStore>>> =
    LazyLock::new(|| tokio::sync::RwLock::new(HashMap::new()));

/// Bilibili HTTP Client
pub struct BilibiliClient {
    client: Client,
    cookies: Option<HashMap<String, String>>,
    rate_limiter: std::sync::Arc<BilibiliRateLimiter>,
}

impl BilibiliClient {
    /// Create a new Bilibili client (reuses shared connection pool and rate limiter)
    pub fn new() -> Result<Self, BilibiliError> {
        Ok(Self {
            client: SHARED_CLIENT.clone(),
            cookies: None,
            rate_limiter: SHARED_RATE_LIMITER.clone(),
        })
    }

    /// Create a new Bilibili client with cookies (reuses shared connection pool and rate limiter)
    pub fn with_cookies(cookies: HashMap<String, String>) -> Result<Self, BilibiliError> {
        Ok(Self {
            client: SHARED_CLIENT.clone(),
            cookies: Some(cookies),
            rate_limiter: SHARED_RATE_LIMITER.clone(),
        })
    }

    /// Store cookies in per-user cache for persistence.
    ///
    /// Each user's cookies are stored independently to prevent cross-user leakage.
    pub async fn store_cookies(user_id: &str, cookies: HashMap<String, String>) {
        let mut cache = COOKIE_CACHE.write().await;
        cache.insert(user_id.to_string(), CookieStore::new(cookies));
    }

    /// Load cookies from per-user cache if not expired.
    ///
    /// Returns `None` if the user has no cached cookies or they have expired.
    pub async fn load_cookies(user_id: &str) -> Option<HashMap<String, String>> {
        let mut cache = COOKIE_CACHE.write().await;
        if let Some(store) = cache.get_mut(user_id) {
            if !store.is_expired() {
                store.refresh(); // Refresh on successful use
                return Some(store.cookies.clone());
            }
            // Expired, remove entry
            cache.remove(user_id);
        }
        None
    }

    /// Refresh cookies by validating them with a lightweight API call
    /// Returns true if cookies are still valid
    pub async fn refresh_cookies(&self) -> Result<bool, BilibiliError> {
        // Try to fetch user info to validate cookies
        match self.user_info().await {
            Ok(info) => Ok(info.is_login),
            Err(BilibiliError::Api { code, .. }) if code == -101 => {
                // -101 is "not logged in" error code
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Wait for the rate limiter before making an API call.
    /// This prevents concurrent users from triggering IP bans.
    async fn wait_for_rate_limit(&self) {
        self.rate_limiter.until_ready().await;
    }

    /// Get the WBI mixin key, fetching and caching it if necessary.
    ///
    /// The mixin key is derived from `img_key` and `sub_key` obtained from
    /// the Bilibili nav API. It is cached globally for `WBI_KEY_TTL` to
    /// avoid excessive API calls.
    async fn get_wbi_mixin_key(&self) -> Result<String, BilibiliError> {
        self.get_wbi_mixin_key_internal(false).await
    }

    /// Internal method to get WBI mixin key with optional force refresh.
    ///
    /// Uses a single Mutex to coordinate access: when the cache is expired (or
    /// force_refresh is set), only the first caller fetches from the API while
    /// all others wait on the lock and then read the freshly-cached value.
    async fn get_wbi_mixin_key_internal(&self, force_refresh: bool) -> Result<String, BilibiliError> {
        let mut cache = WBI_KEY_CACHE.lock().await;

        // Check cache under the lock (unless force refresh)
        if !force_refresh {
            if let Some(ref keys) = *cache {
                if std::time::Instant::now() < keys.expires_at {
                    return Ok(keys.mixin_key.clone());
                }
            }
        }

        // Cache miss or force refresh: fetch from API while holding the lock
        // so concurrent callers wait rather than all hitting the API.
        self.wait_for_rate_limit().await;
        let url = "https://api.bilibili.com/x/web-interface/nav";
        let req = self.add_cookies(self.client.get(url).header("Referer", REFERER));
        let resp = check_response(req.send().await?)?;
        let json: types::NavResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        let wbi_img = json.data.wbi_img.ok_or_else(|| {
            BilibiliError::Parse("Missing wbi_img in nav response".to_string())
        })?;

        let img_key = extract_key_from_url(&wbi_img.img_url).ok_or_else(|| {
            BilibiliError::Parse(format!("Cannot extract img_key from URL: {}", wbi_img.img_url))
        })?;
        let sub_key = extract_key_from_url(&wbi_img.sub_url).ok_or_else(|| {
            BilibiliError::Parse(format!("Cannot extract sub_key from URL: {}", wbi_img.sub_url))
        })?;

        let mixin_key = gen_mixin_key(&img_key, &sub_key);
        if mixin_key.is_empty() {
            return Err(BilibiliError::Parse("Generated empty mixin key".to_string()));
        }

        // Update cache (lock is already held)
        *cache = Some(WbiKeys {
            mixin_key: mixin_key.clone(),
            expires_at: std::time::Instant::now() + WBI_KEY_TTL,
        });

        Ok(mixin_key)
    }

    /// Detect if WBI signature is stale based on error response
    fn is_wbi_stale_error(error: &BilibiliError) -> bool {
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
                .map(|(k, v)| {
                    let safe_k: String = k.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                    let safe_v: String = v.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                    format!("{safe_k}={safe_v}")
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    /// Add cookies to request.
    /// Cookie values are sanitized to prevent header injection via \r\n.
    fn add_cookies(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(cookies) = &self.cookies {
            let cookie_str = cookies
                .iter()
                .map(|(k, v)| {
                    let safe_k: String = k.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                    let safe_v: String = v.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                    format!("{safe_k}={safe_v}")
                })
                .collect::<Vec<_>>()
                .join("; ");
            req.header("Cookie", cookie_str)
        } else {
            req
        }
    }

    /// Generate QR code for login
    pub async fn new_qr_code(&self) -> Result<(String, String), BilibiliError> {
        self.wait_for_rate_limit().await;
        #[derive(Deserialize)]
        struct QrCodeData {
            url: String,
            qrcode_key: String,
        }

        #[derive(Deserialize)]
        struct QrCodeResp {
            code: i32,
            message: String,
            data: Option<QrCodeData>,
        }

        let url = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
        let req = self.client
            .get(url)
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = check_response(req.send().await?)?;
        let json: QrCodeResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        let data = json.data.ok_or_else(|| BilibiliError::Parse("Missing QR code data".to_string()))?;
        Ok((data.url, data.qrcode_key))
    }

    /// Check QR code login status
    pub async fn login_with_qr_code(&self, key: &str) -> Result<(u32, Option<HashMap<String, String>>), BilibiliError> {
        self.wait_for_rate_limit().await;
        #[derive(Deserialize)]
        struct LoginData {
            code: u32,
            #[allow(dead_code)]
            message: String,
        }

        #[derive(Deserialize)]
        struct LoginResp {
            code: i32,
            message: String,
            data: Option<LoginData>,
        }

        let req = self.client
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
            .query(&[("qrcode_key", key)])
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = req.send().await?;
        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(BilibiliError::Http { status, url: resp.url().to_string(), retry_after_secs: None });
        }

        // Extract ALL relevant cookies (SESSDATA, bili_jct, DedeUserID, DedeUserID__ckMd5)
        let cookies = {
            let relevant: HashMap<String, String> = resp.cookies()
                .filter(|c| matches!(c.name(), "SESSDATA" | "bili_jct" | "DedeUserID" | "DedeUserID__ckMd5"))
                .map(|c| (c.name().to_string(), c.value().to_string()))
                .collect();
            if relevant.is_empty() { None } else { Some(relevant) }
        };

        let json: LoginResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        let data = json.data.ok_or_else(|| BilibiliError::Parse("Missing login data".to_string()))?;

        // QR code status codes:
        // 0: success
        // 86038: expired
        // 86090: scanned
        // 86101: not scanned
        Ok((data.code, cookies))
    }

    /// Get new captcha for SMS login
    pub async fn new_captcha(&self) -> Result<(String, String, String), BilibiliError> {
        self.wait_for_rate_limit().await;
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
            message: String,
            data: Option<CaptchaData>,
        }

        let url = "https://passport.bilibili.com/x/passport-login/captcha";
        let req = self.client
            .get(url)
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = check_response(req.send().await?)?;
        let json: CaptchaResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        let data = json.data.ok_or_else(|| BilibiliError::Parse("Missing captcha data".to_string()))?;
        Ok((data.token, data.geetest.gt, data.geetest.challenge))
    }

    /// Get BUVID cookies for SMS operations
    async fn get_buvid_cookies(&self) -> Result<HashMap<String, String>, BilibiliError> {
        self.wait_for_rate_limit().await;
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
            message: String,
            data: Option<SpiData>,
        }

        let url = "https://api.bilibili.com/x/frontend/finger/spi";
        let req = self.client
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", "https://www.bilibili.com");

        let resp = check_response(req.send().await?)?;
        let json: SpiResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        let data = json.data.ok_or_else(|| BilibiliError::Parse("Missing BUVID data".to_string()))?;
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
        self.wait_for_rate_limit().await;
        #[derive(Deserialize)]
        struct SmsData {
            captcha_key: String,
        }

        #[derive(Deserialize)]
        struct SmsResp {
            code: i32,
            message: String,
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
        let mut req = self.client
            .post(url)
            .header("Referer", "https://passport.bilibili.com/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params);

        // Add BUVID cookies as single Cookie header.
        // Sanitize \r\n to prevent header injection, consistent with add_cookies().
        let cookie_str: String = buvid_cookies
            .iter()
            .map(|(name, value)| {
                let safe_name: String = name.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                let safe_value: String = value.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                format!("{safe_name}={safe_value}")
            })
            .collect::<Vec<_>>()
            .join("; ");
        if !cookie_str.is_empty() {
            req = req.header("Cookie", cookie_str);
        }

        let resp = check_response(req.send().await?)?;
        let json: SmsResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        let data = json.data.ok_or_else(|| BilibiliError::Parse("Missing SMS data".to_string()))?;
        Ok(data.captcha_key)
    }

    /// Login with SMS verification code
    pub async fn login_with_sms(
        &self,
        phone: &str,
        code: &str,
        captcha_key: &str,
    ) -> Result<HashMap<String, String>, BilibiliError> {
        self.wait_for_rate_limit().await;
        #[derive(Deserialize)]
        struct LoginSmsData {
            status: i32,
        }

        #[derive(Deserialize)]
        struct LoginSmsResp {
            code: i32,
            message: String,
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
        let req = self.client
            .post(url)
            .header("Referer", "https://passport.bilibili.com/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params);

        let resp = req.send().await?;
        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(BilibiliError::Http { status, url: resp.url().to_string(), retry_after_secs: None });
        }

        // Extract cookies from headers BEFORE consuming body.
        // Cookies are in Set-Cookie headers, so we must read them before json_with_limit.
        let cookies: HashMap<String, String> = resp.cookies()
            .filter(|c| matches!(c.name(), "SESSDATA" | "bili_jct" | "DedeUserID" | "DedeUserID__ckMd5"))
            .map(|c| (c.name().to_string(), c.value().to_string()))
            .collect();

        let json: LoginSmsResp = json_with_limit(resp).await?;

        // Check API-level status before trusting the cookies
        if json.code != 0 {
            return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
        }

        // Check data.status field -- non-zero indicates SMS login failure
        if let Some(data) = &json.data {
            if data.status != 0 {
                return Err(BilibiliError::Api {
                    code: data.status as i64,
                    message: format!("SMS login failed with status: {}", data.status),
                });
            }
        }

        if cookies.is_empty() {
            return Err(BilibiliError::Parse("No auth cookies found in response".to_string()));
        }

        Ok(cookies)
    }

    /// Extract BVID from URL
    #[must_use]
    pub fn extract_bvid(url: &str) -> Option<String> {
        RE_BVID.find(url).map(|m| m.as_str().to_string())
    }

    /// Extract EPID from URL
    #[must_use]
    pub fn extract_epid(url: &str) -> Option<String> {
        RE_EPID.captures(url).and_then(|cap| cap.get(1))
            .map(|m| format!("ep{}", m.as_str()))
    }

    /// Check if URL is a short link (b23.tv)
    ///
    /// Uses proper URL host parsing to avoid false positives from URLs like
    /// `evil.com/b23.tv` or `b23.tv.evil.com`.
    #[must_use]
    pub fn is_short_link(url: &str) -> bool {
        url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .is_some_and(|host| host == "b23.tv" || host.ends_with(".b23.tv"))
    }

    /// Resolve short link to full URL.
    ///
    /// The shared client has `redirect(Policy::none())`, so we manually follow
    /// the `Location` header from b23.tv to get the resolved URL.
    /// The resolved URL is validated against SSRF rules before returning.
    pub async fn resolve_short_link(&self, url: &str) -> Result<String, BilibiliError> {
        self.wait_for_rate_limit().await;
        let response = self.client.get(url).send().await?;
        let status = response.status();

        // b23.tv returns a 302 redirect; extract the Location header
        if status.is_redirection() {
            if let Some(location) = response.headers().get("location") {
                let resolved = location.to_str().map_err(|e| {
                    BilibiliError::Parse(format!("Invalid Location header: {e}"))
                })?;
                // Validate resolved URL against SSRF rules (with DNS resolution to prevent rebinding)
                crate::grpc::validation::validate_host_with_dns(resolved).await
                    .map_err(|e| BilibiliError::InvalidConfig(format!("Resolved URL blocked by SSRF check: {e}")))?;
                return Ok(resolved.to_string());
            }
        }

        // If no redirect, the response URL is already the final URL
        if status.is_success() {
            return Ok(response.url().to_string());
        }

        Err(BilibiliError::Http { status, url: response.url().to_string(), retry_after_secs: None })
    }

    /// Get video information by BVID
    pub async fn get_video_info(&self, bvid: &str) -> Result<VideoInfo, BilibiliError> {
        self.wait_for_rate_limit().await;
        let client = self.client.clone();
        let bvid = bvid.to_string();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let bvid = bvid.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/x/web-interface/view")
                    .query(&[("bvid", &bvid)]);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let response = check_response(req.send().await?)?;

                let json: serde_json::Value = json_with_limit(response).await?;

                if json["code"].as_i64() != Some(0) {
                    return Err(BilibiliError::Api {
                        code: json["code"].as_i64().unwrap_or(0),
                        message: json["message"].as_str().unwrap_or("Unknown error").to_string(),
                    });
                }

                let data = &json["data"];
                Ok(VideoInfo {
                    bvid: data["bvid"].as_str().unwrap_or("").to_string(),
                    aid: data["aid"].as_u64().unwrap_or(0),
                    cid: data["cid"].as_u64().unwrap_or(0),
                    title: data["title"].as_str().unwrap_or("").to_string(),
                    desc: data["desc"].as_str().unwrap_or("").to_string(),
                    pic: data["pic"].as_str().unwrap_or("").to_string(),
                    duration: data["duration"].as_u64().unwrap_or(0),
                })
            }
        }).await
    }

    /// Get playback URL
    pub async fn get_play_url(
        &self,
        bvid: &str,
        cid: u64,
        quality: Quality,
    ) -> Result<PlayUrlInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let bvid = bvid.to_string();
        let cid_str = cid.to_string();
        let qn_str = quality.to_qn().to_string();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let bvid = bvid.clone();
            let cid_str = cid_str.clone();
            let qn_str = qn_str.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/x/player/playurl")
                    .query(&[("bvid", bvid.as_str()), ("cid", &cid_str), ("qn", &qn_str)]);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let response = check_response(req.send().await?)?;
                let json: serde_json::Value = json_with_limit(response).await?;

                if json["code"].as_i64() != Some(0) {
                    return Err(BilibiliError::Api {
                        code: json["code"].as_i64().unwrap_or(0),
                        message: json["message"].as_str().unwrap_or("Unknown error").to_string(),
                    });
                }

                let durl = json["data"]["durl"].as_array()
                    .ok_or_else(|| BilibiliError::Parse("Missing durl array".to_string()))?
                    .iter()
                    .filter_map(|item| {
                        Some(DurlItem {
                            url: item["url"].as_str()?.to_string(),
                            size: item["size"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect();

                Ok(PlayUrlInfo { durl })
            }
        }).await
    }

    /// Get anime information by EPID
    pub async fn get_anime_info(&self, epid: &str) -> Result<AnimeInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let epid = epid.to_string();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let epid = epid.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/pgc/view/web/season")
                    .query(&[("ep_id", epid.as_str())]);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let response = check_response(req.send().await?)?;
                let json: serde_json::Value = json_with_limit(response).await?;

                if json["code"].as_i64() != Some(0) {
                    return Err(BilibiliError::Api {
                        code: json["code"].as_i64().unwrap_or(0),
                        message: json["message"].as_str().unwrap_or("Unknown error").to_string(),
                    });
                }

                let data = &json["result"];
                let first_episode = data["episodes"].as_array().and_then(|arr| arr.first());

                Ok(AnimeInfo {
                    season_id: data["season_id"].as_u64().unwrap_or(0),
                    ep_id: first_episode.and_then(|ep| ep["ep_id"].as_u64()).unwrap_or(0),
                    cid: first_episode.and_then(|ep| ep["cid"].as_u64()).unwrap_or(0),
                    title: data["title"].as_str().unwrap_or("").to_string(),
                    cover: data["cover"].as_str().unwrap_or("").to_string(),
                })
            }
        }).await
    }

    /// Parse video page to get video information
    pub async fn parse_video_page(&self, aid: u64, bvid: &str) -> Result<VideoPageInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let bvid = bvid.to_string();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let bvid = bvid.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/x/web-interface/view");
                if bvid.is_empty() {
                    req = req.query(&[("aid", &aid.to_string())]);
                } else {
                    req = req.query(&[("bvid", &bvid)]);
                }
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::VideoPageInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let data = json.data;
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
        }).await
    }

    /// Get video playback URL (normal video, not DASH)
    pub async fn get_video_url(&self, aid: u64, bvid: &str, cid: u64, quality: Option<u32>) -> Result<VideoUrlInfo, BilibiliError> {
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
                    req = req.query(&[("aid", &aid.to_string()), ("cid", &cid_str), ("qn", &qn_str)]);
                } else {
                    req = req.query(&[("bvid", &bvid), ("cid", &cid_str), ("qn", &qn_str)]);
                }
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::VideoUrlResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let data = json.data;
                let accept_quality: Vec<u32> = data.accept_quality.iter().map(|&q| q as u32).collect();
                let accept_description = data.accept_description;
                let current_quality = data.quality as u32;
                let url = data.durl.first()
                    .map(|d| d.url.clone())
                    .unwrap_or_default();

                Ok(VideoUrlInfo {
                    accept_quality,
                    accept_description,
                    current_quality,
                    url,
                })
            }
        }).await
    }

    /// Get DASH video URL - returns structured DASH data for upper layer to generate MPD.
    ///
    /// This endpoint (`/x/player/wbi/playurl`) requires WBI parameter signing.
    /// Query parameters are signed with the WBI mixin key before sending.
    /// Automatically detects and retries on stale WBI key errors.
    pub async fn get_dash_video_url(&self, aid: u64, bvid: &str, cid: u64) -> Result<(DashData, DashData), BilibiliError> {
        // First attempt with cached key
        let result = self.get_dash_video_url_internal(aid, bvid, cid, false).await;

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
    async fn get_dash_video_url_internal(&self, aid: u64, bvid: &str, cid: u64, force_key_refresh: bool) -> Result<(DashData, DashData), BilibiliError> {
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
                let mut params: Vec<(&str, String)> = vec![
                    ("cid", cid.to_string()),
                    ("fnval", "4048".to_string()),
                ];
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
                let resp = check_response(req.send().await?)?;
                let json: types::DashVideoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                // Parse DASH data into structured format
                let dash_info = json.data.dash;
                let (regular_dash, hevc_dash) = parse_dash_info(&dash_info, &json.data.support_formats)?;

                Ok((regular_dash, hevc_dash))
            }
        }).await
    }

    /// Get subtitles for a video
    pub async fn get_subtitles(&self, aid: u64, bvid: &str, cid: u64) -> Result<HashMap<String, String>, BilibiliError> {
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
                let resp = check_response(req.send().await?)?;
                let json: types::PlayerV2InfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
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
                    // Validate subtitle URL against SSRF
                    if let Err(e) = crate::grpc::validation::validate_host(&url) {
                        tracing::warn!("Skipping subtitle with blocked URL: {} ({})", url, e.message());
                        continue;
                    }
                    subtitles.insert(name, url);
                }

                Ok(subtitles)
            }
        }).await
    }

    /// Get user information
    pub async fn user_info(&self) -> Result<UserInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/x/web-interface/nav")
                    .header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::NavResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let data = json.data;
                Ok(UserInfo {
                    is_login: data.is_login,
                    username: data.uname,
                    face: data.face,
                    is_vip: data.vip_status == 1,
                })
            }
        }).await
    }

    /// Parse PGC (anime/bangumi) page
    pub async fn parse_pgc_page(&self, epid: u64, ssid: u64) -> Result<VideoPageInfo, BilibiliError> {
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
                let resp = check_response(req.send().await?)?;
                let json: types::SeasonInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
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
                        name: if ep.long_title.is_empty() { ep.title } else { ep.long_title },
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
        }).await
    }

    /// Get PGC playback URL
    pub async fn get_pgc_url(&self, epid: u64, cid: u64, quality: Option<u32>) -> Result<VideoUrlInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let qn = quality.unwrap_or(80);

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/pgc/player/web/playurl")
                    .query(&[("ep_id", epid), ("cid", cid), ("qn", qn as u64)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::PgcUrlResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let result = json.result;
                let accept_quality: Vec<u32> = result.accept_quality.iter().map(|&q| q as u32).collect();
                let accept_description = result.accept_description;
                let current_quality = result.quality as u32;
                let url = result.durl.first()
                    .map(|d| d.url.clone())
                    .unwrap_or_default();

                Ok(VideoUrlInfo {
                    accept_quality,
                    accept_description,
                    current_quality,
                    url,
                })
            }
        }).await
    }

    /// Get DASH PGC URL - returns structured DASH data for upper layer to generate MPD
    pub async fn get_dash_pgc_url(&self, epid: u64, cid: u64) -> Result<(DashData, DashData), BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.bilibili.com/pgc/player/web/playurl")
                    .query(&[("ep_id", epid), ("cid", cid), ("fnval", 4048u64)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::DashPgcResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let dash_info = json.result.dash;
                let (regular_dash, hevc_dash) = parse_dash_info(&dash_info, &json.result.support_formats)?;

                Ok((regular_dash, hevc_dash))
            }
        }).await
    }

    /// Match URL to extract video type and ID
    pub fn match_url(url: &str) -> Result<(String, String), BilibiliError> {
        // Video: BV id
        if let Some(bvid) = Self::extract_bvid(url) {
            return Ok(("video".to_string(), bvid));
        }

        // Bangumi/Anime: ep id or ss id
        if url.contains("/bangumi/play/") {
            if let Some(ep_match) = RE_EPID.captures(url) {
                if let Some(ep_id) = ep_match.get(1) {
                    return Ok(("bangumi".to_string(), format!("ep{}", ep_id.as_str())));
                }
            }
            if let Some(ss_match) = RE_SSID.captures(url) {
                if let Some(ss_id) = ss_match.get(1) {
                    return Ok(("bangumi".to_string(), format!("ss{}", ss_id.as_str())));
                }
            }
        }

        // Live: room id
        if url.contains("/live/") || url.contains("live.bilibili.com") {
            if let Some(room_match) = RE_LIVE_ROOM.captures(url) {
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

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.live.bilibili.com/room/v1/Room/get_info")
                    .query(&[("room_id", room_id)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::ParseLivePageResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let data = json.data;
                let title = data.title.clone();
                let uname = String::new();

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
        }).await
    }

    /// Get live streams
    pub async fn get_live_streams(&self, room_id: u64, _hls: bool) -> Result<Vec<LiveStream>, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();
        let room_id_str = room_id.to_string();

        // Note: `_hls` parameter is currently unused; the API always requests both protocols (0,1).
        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let room_id_str = room_id_str.clone();
            async move {
                let mut req = client.get("https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo")
                    .query(&[
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
                let resp = check_response(req.send().await?)?;
                let json: types::RoomPlayInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api {
                        code: json.code as i64,
                        message: json.message,
                    });
                }

                let mut streams = Vec::new();

                let stream_list = json.data.playurl_info
                    .as_ref()
                    .and_then(|info| info.playurl.as_ref())
                    .map(|playurl| &playurl.stream[..])
                    .unwrap_or_default();

                for stream in stream_list {
                    for format in &stream.format {
                        for codec in &format.codec {
                            let quality = codec.current_qn as u32;
                            let desc = codec.accept_qn.first()
                                .map_or_else(|| "Unknown".to_string(), |q| format!("{q}P"));

                            let urls: Vec<String> = codec.url_info.iter()
                                .filter(|info| !info.host.is_empty())
                                .map(|info| format!("{}{}{}", info.host, codec.base_url, info.extra))
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
        }).await
    }

    /// Get live danmaku server info
    pub async fn get_live_danmu_info(&self, room_id: u64) -> Result<LiveDanmuInfo, BilibiliError> {
        let client = self.client.clone();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client.get("https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo")
                    .query(&[("id", room_id)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?)?;
                let json: types::GetLiveDanmuInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api { code: json.code as i64, message: json.message });
                }

                let data = json.data;
                let token = data.token;
                let host_list: Vec<DanmuHost> = data.host_list
                    .into_iter()
                    .map(|h| DanmuHost {
                        host: h.host,
                        port: h.port,
                        wss_port: h.wss_port,
                        ws_port: h.ws_port,
                    })
                    .collect();

                Ok(LiveDanmuInfo {
                    token,
                    host_list,
                })
            }
        }).await
    }

    /// Connect to live danmaku WebSocket and return a message stream
    ///
    /// Returns a tuple of (sender, receiver) for bidirectional communication
    pub async fn connect_live_danmaku(
        &self,
        room_id: u64,
    ) -> Result<LiveDanmakuConnection, BilibiliError> {
        // Get danmaku server info
        let danmu_info = self.get_live_danmu_info(room_id).await?;

        // Select first available host with wss_port
        let host = danmu_info.host_list.first().ok_or_else(|| {
            BilibiliError::Parse("No danmaku host available".to_string())
        })?;

        // Build WebSocket URL (use wss:// for secure connection)
        let ws_url = format!("wss://{}:{}/sub", host.host, host.wss_port);

        // Validate WebSocket URL against SSRF (convert wss to https for validation)
        let validation_url = format!("https://{}:{}/sub", host.host, host.wss_port);
        crate::grpc::validation::validate_host_with_dns(&validation_url).await
            .map_err(|e| BilibiliError::InvalidConfig(format!("Danmaku WebSocket URL blocked by SSRF check: {}", e.message())))?;

        // Connect to WebSocket
        let (ws_stream, _) = connect_async(&ws_url).await.map_err(|e| {
            BilibiliError::Parse(format!("Failed to connect to danmaku WebSocket: {}", e))
        })?;

        let (mut write, read) = ws_stream.split();

        // Send authentication packet
        let auth_packet = build_auth_packet(room_id, &danmu_info.token);
        write.send(Message::Binary(auth_packet.into())).await.map_err(|e| {
            BilibiliError::Parse(format!("Failed to send auth packet: {}", e))
        })?;

        Ok(LiveDanmakuConnection {
            write: tokio::sync::Mutex::new(write),
            read: tokio::sync::Mutex::new(read),
            room_id,
        })
    }
}

/// Live danmaku WebSocket connection
pub struct LiveDanmakuConnection {
    write: tokio::sync::Mutex<futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message
    >>,
    read: tokio::sync::Mutex<futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    >>,
    room_id: u64,
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
            Some(Ok(Message::Binary(data))) => {
                parse_danmaku_packet(&data)
            }
            Some(Ok(_)) => Ok(Vec::new()), // Ignore non-binary messages
            Some(Err(e)) => Err(BilibiliError::Parse(format!("WebSocket error: {}", e))),
            None => Ok(Vec::new()), // Connection closed
        }
    }

    /// Send heartbeat to keep connection alive
    pub async fn send_heartbeat(&self) -> Result<(), BilibiliError> {
        let mut write = self.write.lock().await;
        let heartbeat_packet = build_heartbeat_packet();
        write.send(Message::Binary(heartbeat_packet.into())).await.map_err(|e| {
            BilibiliError::Parse(format!("Failed to send heartbeat: {}", e))
        })
    }

    /// Get room ID
    pub fn room_id(&self) -> u64 {
        self.room_id
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
    UserEnter {
        user: String,
    },
    /// Gift sent
    Gift {
        user: String,
        gift_name: String,
        count: u32,
    },
    /// Heartbeat response (online viewer count)
    Heartbeat {
        online_count: u32,
    },
    /// Unknown message type
    Unknown,
}

/// Build authentication packet for danmaku WebSocket
fn build_auth_packet(room_id: u64, token: &str) -> Vec<u8> {
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
    let body = serde_json::to_vec(&auth_json).unwrap_or_default();

    let packet_length = 16 + body.len();
    let mut packet = Vec::with_capacity(packet_length);

    // Header
    packet.extend_from_slice(&(packet_length as u32).to_be_bytes());
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&1u16.to_be_bytes());  // protocol version
    packet.extend_from_slice(&7u32.to_be_bytes());  // operation = auth
    packet.extend_from_slice(&1u32.to_be_bytes());  // sequence

    // Body
    packet.extend_from_slice(&body);

    packet
}

/// Build heartbeat packet for danmaku WebSocket
fn build_heartbeat_packet() -> Vec<u8> {
    // Heartbeat packet: operation = 2, empty body
    let mut packet = Vec::with_capacity(16);

    packet.extend_from_slice(&16u32.to_be_bytes());  // packet length
    packet.extend_from_slice(&16u16.to_be_bytes());  // header length
    packet.extend_from_slice(&1u16.to_be_bytes());   // protocol version
    packet.extend_from_slice(&2u32.to_be_bytes());   // operation = heartbeat
    packet.extend_from_slice(&1u32.to_be_bytes());   // sequence

    packet
}

/// Parse danmaku packet from binary data.
///
/// A single binary frame may contain multiple sub-packets (especially when
/// compressed with zlib/brotli). This function collects all parsed messages
/// instead of returning only the first one.
fn parse_danmaku_packet(data: &[u8]) -> Result<Vec<DanmakuMessage>, BilibiliError> {
    if data.len() < 16 {
        return Ok(Vec::new());
    }

    // Parse header
    let _packet_length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let _header_length = u16::from_be_bytes([data[4], data[5]]);
    let protocol_version = u16::from_be_bytes([data[6], data[7]]);
    let operation = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let _sequence = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);

    let mut messages = Vec::new();

    match operation {
        3 => {
            // Heartbeat response (online count)
            if data.len() >= 20 {
                let online_count = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
                messages.push(DanmakuMessage::Heartbeat { online_count });
            }
        }
        5 => {
            // Notification message
            let body = &data[16..];

            // Handle compression (protocol_version 2 = zlib, 3 = brotli)
            let decompressed = match protocol_version {
                0 | 1 => body.to_vec(),
                2 => {
                    // zlib (deflate) compression
                    use std::io::Read;
                    let mut decoder = flate2::read::ZlibDecoder::new(body);
                    let mut out = Vec::new();
                    if decoder.read_to_end(&mut out).is_err() {
                        return Ok(Vec::new());
                    }
                    out
                }
                3 => {
                    // brotli compression
                    use std::io::Read;
                    let mut decoder = brotli::Decompressor::new(body, 4096);
                    let mut out = Vec::new();
                    if decoder.read_to_end(&mut out).is_err() {
                        return Ok(Vec::new());
                    }
                    out
                }
                _ => return Ok(Vec::new()),
            };

            // Compressed data contains concatenated sub-packets with headers;
            // uncompressed (v0/v1) body is raw JSON.
            if protocol_version == 0 || protocol_version == 1 {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&decompressed) {
                    if let Some(cmd) = json.get("cmd").and_then(|v| v.as_str()) {
                        messages.push(parse_danmaku_cmd(cmd, &json));
                    }
                }
            } else {
                // Iterate over ALL sub-packets inside the decompressed buffer
                let mut offset = 0usize;
                while offset + 16 <= decompressed.len() {
                    let pkt_len = u32::from_be_bytes([
                        decompressed[offset],
                        decompressed[offset + 1],
                        decompressed[offset + 2],
                        decompressed[offset + 3],
                    ]) as usize;
                    let hdr_len = u16::from_be_bytes([
                        decompressed[offset + 4],
                        decompressed[offset + 5],
                    ]) as usize;
                    if pkt_len < hdr_len || offset + pkt_len > decompressed.len() {
                        break;
                    }
                    let sub_body = &decompressed[offset + hdr_len..offset + pkt_len];
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

    Ok(messages)
}

/// Parse danmaku command from JSON
fn parse_danmaku_cmd(cmd: &str, json: &serde_json::Value) -> DanmakuMessage {
    match cmd {
        "DANMU_MSG" => {
            // Chat message
            let info = json.get("info").and_then(|v| v.as_array());
            if let Some(info) = info {
                let message = info.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let user_info = info.get(2).and_then(|v| v.as_array());
                let user = user_info
                    .and_then(|arr| arr.get(1))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

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
                let user = data.get("uname").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
                return DanmakuMessage::UserEnter { user };
            }
        }
        "SEND_GIFT" => {
            // Gift sent
            let data = json.get("data");
            if let Some(data) = data {
                let user = data.get("uname").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
                let gift_name = data.get("giftName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let count = data.get("num").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                return DanmakuMessage::Gift {
                    user,
                    gift_name,
                    count,
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

/// Video URL information
#[derive(Debug, Clone)]
pub struct VideoUrlInfo {
    pub accept_quality: Vec<u32>,
    pub accept_description: Vec<String>,
    pub current_quality: u32,
    pub url: String,
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

/// Danmaku server host
#[derive(Debug, Clone)]
pub struct DanmuHost {
    pub host: String,
    pub port: u32,
    pub wss_port: u32,
    pub ws_port: u32,
}

// Note: Default impl intentionally removed. BilibiliClient::new() returns
// Result and callers should handle the error. Use BilibiliClient::new() directly.

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

// ============================================================================
// From trait implementations for proto conversion
// ============================================================================

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
            video_streams: data.video_streams.iter().map(std::convert::Into::into).collect(),
            audio_streams: data.audio_streams.iter().map(std::convert::Into::into).collect(),
        }
    }
}

/// Parse DASH info into structured format
/// Returns (`regular_dash`, `hevc_dash`) where HEVC codecs are separated
fn parse_dash_info(
    dash_info: &types::DashInfo,
    support_formats: &[types::SupportFormat],
) -> Result<(DashData, DashData), BilibiliError> {
    let duration = dash_info.duration;
    let min_buffer_time = dash_info.min_buffer_time;

    // Build quality ID → name mapping from support_formats
    let quality_names: HashMap<u64, String> = support_formats
        .iter()
        .map(|f| (f.quality, f.new_description.clone()))
        .collect();

    // Parse audio streams (shared by both regular and HEVC)
    let parsed_audios: Vec<AudioStreamData> = dash_info.audio
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

    Ok((regular_dash, hevc_dash))
}

/// Generate DASH MPD XML from structured data
pub fn generate_mpd_xml(dash_data: &DashData) -> String {
    let mut mpd = String::new();

    // MPD header
    mpd.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    mpd.push('\n');
    mpd.push_str(r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" "#);
    mpd.push_str(r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" "#);
    mpd.push_str(r#"xsi:schemaLocation="urn:mpeg:dash:schema:mpd:2011 DASH-MPD.xsd" "#);
    mpd.push_str(&format!(r#"minBufferTime="PT{:.1}S" "#, dash_data.min_buffer_time));
    mpd.push_str(r#"type="static" "#);
    mpd.push_str(&format!(r#"mediaPresentationDuration="PT{:.1}S" "#, dash_data.duration));
    mpd.push_str(r#"profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">"#);
    mpd.push('\n');

    // Period
    mpd.push_str(r#"  <Period>"#);
    mpd.push('\n');

    // Video AdaptationSet
    if !dash_data.video_streams.is_empty() {
        mpd.push_str(r#"    <AdaptationSet mimeType="video/mp4" contentType="video" "#);
        mpd.push_str(r#"startWithSAP="1" segmentAlignment="true">"#);
        mpd.push('\n');

        for video in &dash_data.video_streams {
            mpd.push_str(&format!(
                r#"      <Representation id="{}" "#,
                video.id
            ));
            mpd.push_str(&format!(r#"codecs="{}" "#, video.codecs));
            mpd.push_str(&format!(r#"width="{}" "#, video.width));
            mpd.push_str(&format!(r#"height="{}" "#, video.height));
            mpd.push_str(&format!(r#"frameRate="{}" "#, video.frame_rate));
            mpd.push_str(&format!(r#"sar="{}" "#, video.sar));
            mpd.push_str(&format!(r#"bandwidth="{}">"#, video.bandwidth));
            mpd.push('\n');

            // BaseURL
            mpd.push_str(&format!(r#"        <BaseURL>{}</BaseURL>"#, escape_xml(&video.base_url)));
            mpd.push('\n');

            // Backup URLs
            for backup_url in &video.backup_urls {
                mpd.push_str(&format!(r#"        <BaseURL>{}</BaseURL>"#, escape_xml(backup_url)));
                mpd.push('\n');
            }

            // SegmentBase
            mpd.push_str(&format!(
                r#"        <SegmentBase indexRange="{}">"#,
                video.segment_base.index_range
            ));
            mpd.push('\n');
            mpd.push_str(&format!(
                r#"          <Initialization range="{}"/>"#,
                video.segment_base.initialization_range
            ));
            mpd.push('\n');
            mpd.push_str(r#"        </SegmentBase>"#);
            mpd.push('\n');

            mpd.push_str(r#"      </Representation>"#);
            mpd.push('\n');
        }

        mpd.push_str(r#"    </AdaptationSet>"#);
        mpd.push('\n');
    }

    // Audio AdaptationSet
    if !dash_data.audio_streams.is_empty() {
        mpd.push_str(r#"    <AdaptationSet mimeType="audio/mp4" contentType="audio" "#);
        mpd.push_str(r#"startWithSAP="1" segmentAlignment="true">"#);
        mpd.push('\n');

        for audio in &dash_data.audio_streams {
            mpd.push_str(&format!(
                r#"      <Representation id="{}" "#,
                audio.id
            ));
            mpd.push_str(&format!(r#"codecs="{}" "#, audio.codecs));
            mpd.push_str(&format!(r#"audioSamplingRate="{}" "#, audio.audio_sampling_rate));
            mpd.push_str(&format!(r#"bandwidth="{}">"#, audio.bandwidth));
            mpd.push('\n');

            // BaseURL
            mpd.push_str(&format!(r#"        <BaseURL>{}</BaseURL>"#, escape_xml(&audio.base_url)));
            mpd.push('\n');

            // Backup URLs
            for backup_url in &audio.backup_urls {
                mpd.push_str(&format!(r#"        <BaseURL>{}</BaseURL>"#, escape_xml(backup_url)));
                mpd.push('\n');
            }

            // SegmentBase
            mpd.push_str(&format!(
                r#"        <SegmentBase indexRange="{}">"#,
                audio.segment_base.index_range
            ));
            mpd.push('\n');
            mpd.push_str(&format!(
                r#"          <Initialization range="{}"/>"#,
                audio.segment_base.initialization_range
            ));
            mpd.push('\n');
            mpd.push_str(r#"        </SegmentBase>"#);
            mpd.push('\n');

            mpd.push_str(r#"      </Representation>"#);
            mpd.push('\n');
        }

        mpd.push_str(r#"    </AdaptationSet>"#);
        mpd.push('\n');
    }

    // Close Period and MPD
    mpd.push_str(r#"  </Period>"#);
    mpd.push('\n');
    mpd.push_str(r#"</MPD>"#);

    mpd
}

/// Escape XML special characters
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_bvid() {
        assert_eq!(
            BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7XZ"),
            Some("BV1xx411c7XZ".to_string())
        );
    }

    #[test]
    fn test_extract_epid() {
        assert_eq!(
            BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep12345"),
            Some("ep12345".to_string())
        );
    }

    #[test]
    fn test_is_short_link() {
        assert!(BilibiliClient::is_short_link("https://b23.tv/abc123"));
        assert!(!BilibiliClient::is_short_link("https://www.bilibili.com/video/BV123"));
    }

    #[test]
    fn test_quality_conversion() {
        assert_eq!(Quality::P1080.to_qn(), 80);
        assert_eq!(Quality::from_qn(64), Quality::P720);
        assert_eq!(Quality::P480.as_str(), "480P");
    }

    // === Extended URL Extraction Tests ===

    #[test]
    fn test_extract_bvid_various_formats() {
        // Standard video URL
        assert_eq!(
            BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7XZ"),
            Some("BV1xx411c7XZ".to_string())
        );
        // With query params
        assert_eq!(
            BilibiliClient::extract_bvid("https://www.bilibili.com/video/BV1xx411c7XZ?p=2"),
            Some("BV1xx411c7XZ".to_string())
        );
        // Mobile URL
        assert_eq!(
            BilibiliClient::extract_bvid("https://m.bilibili.com/video/BV1xx411c7XZ"),
            Some("BV1xx411c7XZ".to_string())
        );
        // Just the BV id
        assert_eq!(
            BilibiliClient::extract_bvid("BV1xx411c7XZ"),
            Some("BV1xx411c7XZ".to_string())
        );
    }

    #[test]
    fn test_extract_bvid_invalid() {
        assert_eq!(BilibiliClient::extract_bvid("https://www.bilibili.com/video/av12345"), None);
        assert_eq!(BilibiliClient::extract_bvid("not-a-url"), None);
        assert_eq!(BilibiliClient::extract_bvid(""), None);
    }

    #[test]
    fn test_extract_epid_various_formats() {
        assert_eq!(
            BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep12345"),
            Some("ep12345".to_string())
        );
        assert_eq!(
            BilibiliClient::extract_epid("https://www.bilibili.com/bangumi/play/ep99999?from=search"),
            Some("ep99999".to_string())
        );
    }

    #[test]
    fn test_extract_epid_invalid() {
        assert_eq!(BilibiliClient::extract_epid("https://www.bilibili.com/video/BV123"), None);
        assert_eq!(BilibiliClient::extract_epid(""), None);
    }

    #[test]
    fn test_is_short_link_variations() {
        assert!(BilibiliClient::is_short_link("https://b23.tv/abc123"));
        assert!(BilibiliClient::is_short_link("http://b23.tv/xyz"));
        assert!(BilibiliClient::is_short_link("https://b23.tv/episode/12345"));
        assert!(!BilibiliClient::is_short_link("https://www.bilibili.com/video/BV123"));
        assert!(!BilibiliClient::is_short_link(""));
        // These must NOT match: "b23.tv" appearing in path or as subdomain of another host
        assert!(!BilibiliClient::is_short_link("https://evil.com/b23.tv/abc"));
        assert!(!BilibiliClient::is_short_link("https://b23.tv.evil.com/abc"));
    }

    // === URL Matching Tests ===

    #[test]
    fn test_match_url_video() {
        let (media_type, id) = BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7XZ").unwrap();
        assert_eq!(media_type, "video");
        assert_eq!(id, "BV1xx411c7XZ");
    }

    #[test]
    fn test_match_url_bangumi_ep() {
        let (media_type, id) = BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep12345").unwrap();
        assert_eq!(media_type, "bangumi");
        assert_eq!(id, "ep12345");
    }

    #[test]
    fn test_match_url_bangumi_ss() {
        let (media_type, id) = BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss67890").unwrap();
        assert_eq!(media_type, "bangumi");
        assert_eq!(id, "ss67890");
    }

    #[test]
    fn test_match_url_live() {
        let (media_type, id) = BilibiliClient::match_url("https://live.bilibili.com/live/12345").unwrap();
        assert_eq!(media_type, "live");
        assert_eq!(id, "12345");
    }

    #[test]
    fn test_match_url_unknown() {
        let result = BilibiliClient::match_url("https://example.com/unknown");
        assert!(result.is_err());
    }

    // === Quality Tests ===

    #[test]
    fn test_quality_all_variants() {
        assert_eq!(Quality::P1080.to_qn(), 80);
        assert_eq!(Quality::P720.to_qn(), 64);
        assert_eq!(Quality::P480.to_qn(), 32);
        assert_eq!(Quality::P360.to_qn(), 16);
    }

    #[test]
    fn test_quality_from_qn_all() {
        assert_eq!(Quality::from_qn(80), Quality::P1080);
        assert_eq!(Quality::from_qn(64), Quality::P720);
        assert_eq!(Quality::from_qn(32), Quality::P480);
        assert_eq!(Quality::from_qn(16), Quality::P360);
    }

    #[test]
    fn test_quality_from_qn_unknown_defaults() {
        assert_eq!(Quality::from_qn(0), Quality::P360);
        assert_eq!(Quality::from_qn(999), Quality::P360);
    }

    #[test]
    fn test_quality_as_str_all() {
        assert_eq!(Quality::P1080.as_str(), "1080P");
        assert_eq!(Quality::P720.as_str(), "720P");
        assert_eq!(Quality::P480.as_str(), "480P");
        assert_eq!(Quality::P360.as_str(), "360P");
    }

    #[test]
    fn test_quality_roundtrip() {
        for q in [Quality::P1080, Quality::P720, Quality::P480, Quality::P360] {
            assert_eq!(Quality::from_qn(q.to_qn()), q);
        }
    }

    // === Client Creation Tests ===

    #[test]
    fn test_client_creation_no_cookies() {
        let client = BilibiliClient::new().unwrap();
        assert!(client.cookies.is_none());
    }

    #[test]
    fn test_client_creation_with_cookies() {
        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "abc123".to_string());
        let client = BilibiliClient::with_cookies(cookies.clone()).unwrap();
        assert!(client.cookies.is_some());
        assert_eq!(client.cookies.as_ref().unwrap().get("SESSDATA"), Some(&"abc123".to_string()));
    }

    // === Type Deserialization Tests ===

    #[test]
    fn test_video_page_info_deserialize() {
        let json = r#"{
            "data": {
                "title": "Test Video",
                "pic": "https://example.com/pic.jpg",
                "bvid": "BV1xx411c7XZ",
                "aid": 12345,
                "cid": 67890,
                "owner": {"name": "TestUser", "face": "https://example.com/face.jpg", "mid": 111},
                "pages": [{"cid": 67890, "page": 1, "part": "P1", "duration": 120, "dimension": {"width": 1920, "height": 1080, "rotate": 0}}]
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: types::VideoPageInfoResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.title, "Test Video");
        assert_eq!(resp.data.bvid, "BV1xx411c7XZ");
        assert_eq!(resp.data.aid, 12345);
        assert_eq!(resp.data.pages.len(), 1);
        assert_eq!(resp.data.pages[0].duration, 120);
        assert_eq!(resp.code, 0);
    }

    #[test]
    fn test_nav_resp_deserialize() {
        let json = r#"{
            "data": {"isLogin": true, "uname": "TestUser", "face": "https://example.com/face.jpg", "vipStatus": 1, "mid": 12345},
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: types::NavResp = serde_json::from_str(json).unwrap();
        assert!(resp.data.is_login);
        assert_eq!(resp.data.uname, "TestUser");
        assert_eq!(resp.data.mid, 12345);
    }

    #[test]
    fn test_video_url_resp_deserialize() {
        let json = r#"{
            "data": {
                "accept_quality": [80, 64, 32],
                "accept_description": ["1080P", "720P", "480P"],
                "quality": 80,
                "durl": [{"url": "https://cdn.bilibili.com/video.flv", "size": 1000000, "length": 120}]
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: types::VideoUrlResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.quality, 80);
        assert_eq!(resp.data.durl.len(), 1);
        assert_eq!(resp.data.accept_quality, vec![80, 64, 32]);
    }

    #[test]
    fn test_qrcode_resp_deserialize() {
        let json = r#"{
            "data": {"url": "https://passport.bilibili.com/qrcode", "qrcode_key": "abc123"},
            "message": "0",
            "code": 0,
            "ttl": 180
        }"#;
        let resp: types::QrcodeResp = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.qrcode_key, "abc123");
        assert_eq!(resp.ttl, 180);
    }

    // === WBI Signing Tests ===

    #[test]
    fn test_extract_key_from_url() {
        assert_eq!(
            extract_key_from_url("https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png"),
            Some("7cd084941338484aae1ad9425b84077c".to_string())
        );
        assert_eq!(
            extract_key_from_url("https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"),
            Some("4932caff0ff746eab6f01bf08b70ac45".to_string())
        );
        assert_eq!(extract_key_from_url(""), None);
        assert_eq!(extract_key_from_url("no-slash"), None);
    }

    #[test]
    fn test_gen_mixin_key() {
        // Use known img_key and sub_key values to test the mixin key generation.
        let img_key = "7cd084941338484aae1ad9425b84077c";
        let sub_key = "4932caff0ff746eab6f01bf08b70ac45";
        let mixin = gen_mixin_key(img_key, sub_key);
        // The mixin key should be exactly 32 characters
        assert_eq!(mixin.len(), 32);
        // Verify the key is deterministic
        assert_eq!(mixin, gen_mixin_key(img_key, sub_key));
        // Verify first few characters from the known encoding table:
        // MIXIN_KEY_ENC_TAB[0] = 46 → combined[46] (sub_key[14] = 'f')
        let combined = format!("{img_key}{sub_key}");
        let combined_bytes: Vec<u8> = combined.bytes().collect();
        assert_eq!(mixin.as_bytes()[0], combined_bytes[46]);
    }

    #[test]
    fn test_gen_mixin_key_empty() {
        let mixin = gen_mixin_key("", "");
        assert!(mixin.is_empty());
    }

    #[test]
    fn test_wbi_sign_produces_w_rid_and_wts() {
        let params = vec![
            ("bvid", "BV1xx411c7XZ".to_string()),
            ("cid", "12345".to_string()),
            ("fnval", "4048".to_string()),
        ];
        let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
        let signed = wbi_sign(&params, mixin_key);

        // Should contain w_rid and wts in addition to original params
        let keys: Vec<&str> = signed.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"w_rid"), "signed params should contain w_rid");
        assert!(keys.contains(&"wts"), "signed params should contain wts");
        assert!(keys.contains(&"bvid"), "signed params should contain bvid");
        assert!(keys.contains(&"cid"), "signed params should contain cid");
        assert!(keys.contains(&"fnval"), "signed params should contain fnval");

        // w_rid should be a 32-char hex MD5 hash
        let w_rid = signed.iter().find(|(k, _)| k == "w_rid").map(|(_, v)| v.as_str()).expect("w_rid missing");
        assert_eq!(w_rid.len(), 32);
        assert!(w_rid.chars().all(|c| c.is_ascii_hexdigit()), "w_rid should be hex");
    }

    #[test]
    fn test_wbi_sign_filters_special_chars() {
        let params = vec![
            ("key", "hello!'()*world".to_string()),
        ];
        let mixin_key = "testkey12345678901234567890123456";
        let signed = wbi_sign(&params, mixin_key);

        // The value should have !'()* removed
        let val = signed.iter().find(|(k, _)| k == "key").map(|(_, v)| v.as_str()).expect("key missing");
        assert_eq!(val, "helloworld");
    }

    #[test]
    fn test_wbi_sign_sorted_params() {
        let params = vec![
            ("z_param", "z".to_string()),
            ("a_param", "a".to_string()),
            ("m_param", "m".to_string()),
        ];
        let mixin_key = "testkey12345678901234567890123456";
        let signed = wbi_sign(&params, mixin_key);

        // Params before w_rid should be sorted alphabetically
        let keys_before_wrid: Vec<&str> = signed.iter()
            .filter(|(k, _)| k != "w_rid")
            .map(|(k, _)| k.as_str())
            .collect();
        // a_param, m_param, wts, z_param (alphabetically sorted)
        let mut sorted = keys_before_wrid.clone();
        sorted.sort();
        assert_eq!(keys_before_wrid, sorted, "params should be sorted alphabetically");
    }

    #[test]
    fn test_wbi_sign_deterministic_for_same_timestamp() {
        // The same params + mixin_key should produce consistent signing
        // (modulo the wts which depends on system time)
        let params = vec![
            ("bvid", "BV1test".to_string()),
            ("cid", "999".to_string()),
        ];
        let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
        let signed1 = wbi_sign(&params, mixin_key);
        let signed2 = wbi_sign(&params, mixin_key);

        // The wts values should be very close (same second)
        let wts1 = signed1.iter().find(|(k, _)| k == "wts").map(|(_, v)| v.clone()).expect("wts missing");
        let wts2 = signed2.iter().find(|(k, _)| k == "wts").map(|(_, v)| v.clone()).expect("wts missing");
        // They should be the same if run within the same second
        assert_eq!(wts1, wts2, "wts should be same within the same second");

        // If wts is the same, w_rid must be the same too
        let w_rid1 = signed1.iter().find(|(k, _)| k == "w_rid").map(|(_, v)| v.clone()).expect("w_rid missing");
        let w_rid2 = signed2.iter().find(|(k, _)| k == "w_rid").map(|(_, v)| v.clone()).expect("w_rid missing");
        assert_eq!(w_rid1, w_rid2, "w_rid should be deterministic for same inputs");
    }

    #[test]
    fn test_nav_resp_with_wbi_img_deserialize() {
        let json = r#"{
            "data": {
                "isLogin": true,
                "uname": "TestUser",
                "face": "https://example.com/face.jpg",
                "vipStatus": 1,
                "mid": 12345,
                "wbi_img": {
                    "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                    "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
                }
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: types::NavResp = serde_json::from_str(json).unwrap();
        assert!(resp.data.wbi_img.is_some());
        let wbi_img = resp.data.wbi_img.unwrap();
        assert!(wbi_img.img_url.contains("7cd084941338484aae1ad9425b84077c"));
        assert!(wbi_img.sub_url.contains("4932caff0ff746eab6f01bf08b70ac45"));
    }

    #[test]
    fn test_nav_resp_without_wbi_img_deserialize() {
        let json = r#"{
            "data": {
                "isLogin": false,
                "uname": "",
                "face": "",
                "vipStatus": 0,
                "mid": 0
            },
            "message": "0",
            "code": 0,
            "ttl": 1
        }"#;
        let resp: types::NavResp = serde_json::from_str(json).unwrap();
        assert!(resp.data.wbi_img.is_none());
    }
}
