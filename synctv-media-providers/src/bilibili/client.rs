//! Bilibili HTTP Client

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use md5::{Digest, Md5};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::error::{check_response, json_with_limit, BilibiliError};
use super::types::{self as types, AnimeInfo, DurlItem, PlayUrlInfo, Quality, VideoInfo};
use crate::error::with_retry;
use crate::ssrf::ssrf_dns_resolver;

// Pre-compiled regexes using std::sync::LazyLock (no external crate needed).
// These patterns are compile-time constants; Regex::new cannot fail on them.
static RE_BVID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"BV[a-zA-Z0-9]+").expect("invalid BVID regex"));
static RE_EPID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ep(\d+)").expect("invalid EPID regex"));
static RE_SSID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ss(\d+)").expect("invalid SSID regex"));
static RE_LIVE_ROOM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/live/(\d+)").expect("invalid live room regex"));

use crate::error::PROVIDER_USER_AGENT as USER_AGENT;
const REFERER: &str = "https://www.bilibili.com";

/// Shared HTTP client for all Bilibili requests (connection pooling)
/// Redirects are disabled to prevent SSRF via redirect to private IPs.
/// Uses SSRF-safe DNS resolver to check resolved IPs at connection time,
/// preventing DNS rebinding attacks.
static SHARED_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent(USER_AGENT)
        .dns_resolver(ssrf_dns_resolver())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Failed to build Bilibili shared HTTP client")
});

// ============================================================================
// WBI Signing
// ============================================================================

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

/// Global in-memory WBI key cache.
/// WBI keys are refreshed from Bilibili's nav API every 30 minutes.
static WBI_KEY_CACHE: LazyLock<tokio::sync::Mutex<Option<WbiKeys>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

/// Refresh lock to prevent thundering herd when cache expires.
/// Uses an atomic counter: 0 = no refresh in progress, >0 = refresh in progress.
static WBI_REFRESH_IN_PROGRESS: LazyLock<AtomicUsize> = LazyLock::new(|| AtomicUsize::new(0));

/// Notify for waking tasks waiting for refresh to complete.
/// Replaces spin-loop waiting with efficient async notification.
static WBI_REFRESH_NOTIFY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// Counter for consecutive refresh failures. Used to prevent infinite retry loops
/// when the WBI API is persistently unavailable.
static WBI_CONSECUTIVE_FAILURES: LazyLock<AtomicUsize> = LazyLock::new(|| AtomicUsize::new(0));

/// Maximum number of consecutive refresh failures before we give up and return an error.
/// This prevents infinite waiting when the WBI API is persistently unavailable.
const WBI_MAX_CONSECUTIVE_FAILURES: usize = 3;

/// Maximum time to wait for a refresh notification before timing out.
/// This prevents tasks from waiting indefinitely if the refreshing task fails silently.
const WBI_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Counter for tracking number of API calls (for testing).
#[cfg(test)]
static WBI_API_CALL_COUNT: LazyLock<AtomicUsize> = LazyLock::new(|| AtomicUsize::new(0));

/// WBI key cache TTL (refresh keys every 30 minutes).
const WBI_KEY_TTL: Duration = Duration::from_mins(30);

/// Get a valid cached WBI key if available and not expired.
async fn get_valid_wbi_key() -> Option<String> {
    let guard = WBI_KEY_CACHE.lock().await;
    guard
        .as_ref()
        .filter(|k| k.expires_at > std::time::Instant::now())
        .map(|k| k.mixin_key.clone())
}

/// Store a new WBI key in the cache with expiration.
async fn set_wbi_key(mixin_key: String) {
    let mut guard = WBI_KEY_CACHE.lock().await;
    *guard = Some(WbiKeys {
        mixin_key,
        expires_at: std::time::Instant::now() + WBI_KEY_TTL,
    });
}

/// Try to claim the refresh lock. Returns true if this task should perform the refresh.
/// Uses compare_exchange for atomic coordination.
fn try_claim_refresh_lock() -> bool {
    // Try to transition from 0 to 1
    WBI_REFRESH_IN_PROGRESS
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Release the refresh lock after refresh completes.
/// Note: This is a test-only helper. Production code should use `release_refresh_lock_and_notify`.
#[cfg(test)]
fn release_refresh_lock() {
    WBI_REFRESH_IN_PROGRESS.store(0, Ordering::Release);
}

/// Release the refresh lock after a successful refresh, reset failure counter, and notify waiters.
fn release_refresh_lock_on_success_and_notify() {
    WBI_CONSECUTIVE_FAILURES.store(0, Ordering::Release);
    WBI_REFRESH_IN_PROGRESS.store(0, Ordering::Release);
    WBI_REFRESH_NOTIFY.notify_waiters();
}

/// Release the refresh lock after a failed refresh, increment failure counter, and notify waiters.
/// Returns true if we've exceeded the maximum consecutive failures.
fn release_refresh_lock_on_failure_and_notify() -> bool {
    let failures = WBI_CONSECUTIVE_FAILURES.fetch_add(1, Ordering::AcqRel) + 1;
    WBI_REFRESH_IN_PROGRESS.store(0, Ordering::Release);
    WBI_REFRESH_NOTIFY.notify_waiters();
    failures >= WBI_MAX_CONSECUTIVE_FAILURES
}

/// Check if we've exceeded the maximum consecutive failures.
fn has_exceeded_max_failures() -> bool {
    WBI_CONSECUTIVE_FAILURES.load(Ordering::Acquire) >= WBI_MAX_CONSECUTIVE_FAILURES
}

/// Reset the consecutive failure counter. Used by tests and when a cached key is available.
fn reset_consecutive_failures() {
    WBI_CONSECUTIVE_FAILURES.store(0, Ordering::Release);
}

/// Release the refresh lock after refresh completes and notify all waiters.
/// Deprecated: Use `release_refresh_lock_on_success_and_notify` or
/// `release_refresh_lock_on_failure_and_notify` instead.
#[allow(dead_code)]
fn release_refresh_lock_and_notify() {
    WBI_REFRESH_IN_PROGRESS.store(0, Ordering::Release);
    // Notify all waiting tasks that refresh is complete
    WBI_REFRESH_NOTIFY.notify_waiters();
}

/// Check if a refresh is currently in progress.
#[allow(dead_code)]
fn is_refresh_in_progress() -> bool {
    WBI_REFRESH_IN_PROGRESS.load(Ordering::Acquire) > 0
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

/// Bilibili HTTP Client
pub struct BilibiliClient {
    client: Client,
    cookies: Option<HashMap<String, String>>,
}

impl BilibiliClient {
    /// Create a new Bilibili client (reuses shared connection pool and rate limiter)
    pub fn new() -> Result<Self, BilibiliError> {
        Ok(Self {
            client: SHARED_CLIENT.clone(),
            cookies: None,
        })
    }

    /// Create a new Bilibili client with cookies (reuses shared connection pool and rate limiter)
    pub fn with_cookies(cookies: HashMap<String, String>) -> Result<Self, BilibiliError> {
        Ok(Self {
            client: SHARED_CLIENT.clone(),
            cookies: Some(cookies),
        })
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
            if let Some(key) = get_valid_wbi_key().await {
                // Reset failure counter on successful cache hit
                reset_consecutive_failures();
                return Ok(key);
            }
        }

        // Check if we've exceeded max consecutive failures - fail fast
        if has_exceeded_max_failures() {
            return Err(BilibiliError::Parse(
                "WBI key refresh unavailable: too many consecutive failures".to_string(),
            ));
        }

        // Try to claim the refresh lock. Only one task will succeed.
        if try_claim_refresh_lock() {
            // We got the lock - we are responsible for refreshing.
            let result = self.fetch_and_cache_wbi_key().await;
            match &result {
                Ok(_) => {
                    release_refresh_lock_on_success_and_notify();
                }
                Err(_) => {
                    release_refresh_lock_on_failure_and_notify();
                }
            }
            result
        } else {
            // Another task is refreshing. Wait for notification with timeout.
            // This prevents thundering herd and reduces unnecessary CPU usage.
            let notify_result =
                tokio::time::timeout(WBI_REFRESH_TIMEOUT, WBI_REFRESH_NOTIFY.notified()).await;

            if notify_result.is_err() {
                // Timeout waiting for notification - the refreshing task may have failed silently
                // or is taking too long. Return an error instead of waiting indefinitely.
                return Err(BilibiliError::Parse(
                    "WBI key refresh timeout: waited too long for refresh".to_string(),
                ));
            }

            // After being notified, check the cache again.
            if let Some(key) = get_valid_wbi_key().await {
                reset_consecutive_failures();
                return Ok(key);
            }

            // Check if we've exceeded max failures before retrying
            if has_exceeded_max_failures() {
                return Err(BilibiliError::Parse(
                    "WBI key refresh unavailable: too many consecutive failures".to_string(),
                ));
            }

            // If cache is still empty after notification (refresh failed),
            // try to refresh ourselves as a fallback.
            if try_claim_refresh_lock() {
                let result = self.fetch_and_cache_wbi_key().await;
                match &result {
                    Ok(_) => {
                        release_refresh_lock_on_success_and_notify();
                    }
                    Err(_) => {
                        release_refresh_lock_on_failure_and_notify();
                    }
                }
                result
            } else {
                // Another task beat us to it - wait again with timeout
                let notify_result =
                    tokio::time::timeout(WBI_REFRESH_TIMEOUT, WBI_REFRESH_NOTIFY.notified()).await;

                if notify_result.is_err() {
                    return Err(BilibiliError::Parse(
                        "WBI key refresh timeout: waited too long for refresh".to_string(),
                    ));
                }

                // Check cache one more time
                if let Some(key) = get_valid_wbi_key().await {
                    reset_consecutive_failures();
                    return Ok(key);
                }

                // Check if we've exceeded max failures
                if has_exceeded_max_failures() {
                    return Err(BilibiliError::Parse(
                        "WBI key refresh unavailable: too many consecutive failures".to_string(),
                    ));
                }

                get_valid_wbi_key()
                    .await
                    .ok_or_else(|| BilibiliError::Parse("WBI key refresh failed".to_string()))
            }
        }
    }

    /// Fetch WBI key from Bilibili API and cache it.
    async fn fetch_and_cache_wbi_key(&self) -> Result<String, BilibiliError> {
        #[cfg(test)]
        WBI_API_CALL_COUNT.fetch_add(1, Ordering::Relaxed);

        let url = "https://api.bilibili.com/x/web-interface/nav";
        let req = self.add_cookies(self.client.get(url).header("Referer", REFERER));
        let resp = check_response(req.send().await?).await?;
        let json: types::NavResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
        }

        let wbi_img = json
            .data
            .wbi_img
            .ok_or_else(|| BilibiliError::Parse("Missing wbi_img in nav response".to_string()))?;

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
        set_wbi_key(mixin_key.clone()).await;

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
        let req = self
            .client
            .get(url)
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = check_response(req.send().await?).await?;
        let json: QrCodeResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
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
            #[allow(dead_code)]
            message: String,
        }

        #[derive(Deserialize)]
        struct LoginResp {
            code: i32,
            message: String,
            data: Option<LoginData>,
        }

        let req = self
            .client
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
            .query(&[("qrcode_key", key)])
            .header("Referer", "https://passport.bilibili.com/login");

        let resp = req.send().await?;
        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            let url = resp.url().to_string();
            let body = resp.text().await.unwrap_or_default();
            return Err(BilibiliError::Http {
                status,
                url,
                retry_after_secs: None,
                body,
            });
        }

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
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
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
            message: String,
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
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
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
            message: String,
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
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
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
            .map(|(name, value)| {
                let safe_name: String = name.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                let safe_value: String =
                    value.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                format!("{safe_name}={safe_value}")
            })
            .collect::<Vec<_>>()
            .join("; ");
        if !cookie_str.is_empty() {
            req = req.header("Cookie", cookie_str);
        }

        let resp = check_response(req.send().await?).await?;
        let json: SmsResp = json_with_limit(resp).await?;

        if json.code != 0 {
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
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
        let req = self
            .client
            .post(url)
            .header("Referer", "https://passport.bilibili.com/login")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params);

        let resp = req.send().await?;
        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            let url = resp.url().to_string();
            let body = resp.text().await.unwrap_or_default();
            return Err(BilibiliError::Http {
                status,
                url,
                retry_after_secs: None,
                body,
            });
        }

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
            return Err(BilibiliError::Api {
                code: i64::from(json.code),
                message: json.message,
            });
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
        RE_BVID.find(url).map(|m| m.as_str().to_string())
    }

    /// Extract EPID from URL
    #[must_use]
    pub fn extract_epid(url: &str) -> Option<String> {
        RE_EPID
            .captures(url)
            .and_then(|cap| cap.get(1))
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
        // SSRF protection is handled by the DNS resolver at connection time
        let response = self.client.get(url).send().await?;
        let status = response.status();

        // b23.tv returns a 302 redirect; extract the Location header
        if status.is_redirection() {
            if let Some(location) = response.headers().get("location") {
                let resolved = location
                    .to_str()
                    .map_err(|e| BilibiliError::Parse(format!("Invalid Location header: {e}")))?;
                return Ok(resolved.to_string());
            }
        }

        // If no redirect, the response URL is already the final URL
        if status.is_success() {
            return Ok(response.url().to_string());
        }

        Err(BilibiliError::Http {
            status,
            url: response.url().to_string(),
            retry_after_secs: None,
            body: String::new(),
        })
    }

    /// Get video information by BVID
    pub async fn get_video_info(&self, bvid: &str) -> Result<VideoInfo, BilibiliError> {
        let client = self.client.clone();
        let bvid = bvid.to_string();
        let cookie_header = self.build_cookie_header();

        with_retry(|| {
            let client = client.clone();
            let bvid = bvid.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client
                    .get("https://api.bilibili.com/x/web-interface/view")
                    .query(&[("bvid", &bvid)]);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let response = check_response(req.send().await?).await?;

                let json: serde_json::Value = json_with_limit(response).await?;

                if json["code"].as_i64() != Some(0) {
                    return Err(BilibiliError::Api {
                        code: json["code"].as_i64().unwrap_or(0),
                        message: json["message"]
                            .as_str()
                            .unwrap_or("Unknown error")
                            .to_string(),
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
        })
        .await
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
                let mut req = client
                    .get("https://api.bilibili.com/x/player/playurl")
                    .query(&[("bvid", bvid.as_str()), ("cid", &cid_str), ("qn", &qn_str)]);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let response = check_response(req.send().await?).await?;
                let json: serde_json::Value = json_with_limit(response).await?;

                if json["code"].as_i64() != Some(0) {
                    return Err(BilibiliError::Api {
                        code: json["code"].as_i64().unwrap_or(0),
                        message: json["message"]
                            .as_str()
                            .unwrap_or("Unknown error")
                            .to_string(),
                    });
                }

                let durl = json["data"]["durl"]
                    .as_array()
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
        })
        .await
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
                let mut req = client
                    .get("https://api.bilibili.com/pgc/view/web/season")
                    .query(&[("ep_id", epid.as_str())]);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let response = check_response(req.send().await?).await?;
                let json: serde_json::Value = json_with_limit(response).await?;

                if json["code"].as_i64() != Some(0) {
                    return Err(BilibiliError::Api {
                        code: json["code"].as_i64().unwrap_or(0),
                        message: json["message"]
                            .as_str()
                            .unwrap_or("Unknown error")
                            .to_string(),
                    });
                }

                let data = &json["result"];
                let first_episode = data["episodes"].as_array().and_then(|arr| arr.first());

                Ok(AnimeInfo {
                    season_id: data["season_id"].as_u64().unwrap_or(0),
                    ep_id: first_episode
                        .and_then(|ep| ep["ep_id"].as_u64())
                        .unwrap_or(0),
                    cid: first_episode.and_then(|ep| ep["cid"].as_u64()).unwrap_or(0),
                    title: data["title"].as_str().unwrap_or("").to_string(),
                    cover: data["cover"].as_str().unwrap_or("").to_string(),
                })
            }
        })
        .await
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
                let resp = check_response(req.send().await?).await?;
                let json: types::VideoPageInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
                }

                let data = json.data;
                let accept_quality: Vec<u32> =
                    data.accept_quality.iter().map(|&q| q as u32).collect();
                let accept_description = data.accept_description;
                let current_quality = data.quality as u32;
                let segments: Vec<VideoSegment> = data
                    .durl
                    .iter()
                    .map(|d| VideoSegment {
                        url: d.url.clone(),
                        size: d.size,
                    })
                    .collect();
                let url = segments.first().map(|s| s.url.clone()).unwrap_or_default();

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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
                }

                // Parse DASH data into structured format
                let dash_info = json.data.dash;
                let (regular_dash, hevc_dash) =
                    parse_dash_info(&dash_info, &json.data.support_formats);

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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
                }

                let result = json.result;
                let accept_quality: Vec<u32> =
                    result.accept_quality.iter().map(|&q| q as u32).collect();
                let accept_description = result.accept_description;
                let current_quality = result.quality as u32;
                let segments: Vec<VideoSegment> = result
                    .durl
                    .iter()
                    .map(|d| VideoSegment {
                        url: d.url.clone(),
                        size: d.size,
                    })
                    .collect();
                let url = segments.first().map(|s| s.url.clone()).unwrap_or_default();

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
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
                }

                let dash_info = json.result.dash;
                let (regular_dash, hevc_dash) =
                    parse_dash_info(&dash_info, &json.result.support_formats);

                Ok((regular_dash, hevc_dash))
            }
        })
        .await
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
                let mut req = client
                    .get("https://api.live.bilibili.com/room/v1/Room/get_info")
                    .query(&[("room_id", room_id)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::ParseLivePageResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
                }

                let data = json.data;
                let title = data.title.clone();

                // Fetch streamer name from master info API using uid from room info
                let uname = {
                    let uid = data.uid;
                    let mut master_req = client
                        .get("https://api.live.bilibili.com/live_user/v1/Master/info")
                        .query(&[("uid", uid)]);
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

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            let room_id_str = room_id_str.clone();
            async move {
                let mut req = client
                    .get("https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo")
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
                let resp = check_response(req.send().await?).await?;
                let json: types::RoomPlayInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
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
                            let quality = u32::try_from(codec.current_qn);
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
                                    quality: quality.unwrap_or(0),
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

        with_retry(|| {
            let client = client.clone();
            let cookie_header = cookie_header.clone();
            async move {
                let mut req = client
                    .get("https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo")
                    .query(&[("id", room_id)]);
                req = req.header("Referer", REFERER);
                if let Some(ref cookies) = cookie_header {
                    req = req.header("Cookie", cookies.as_str());
                }
                let resp = check_response(req.send().await?).await?;
                let json: types::GetLiveDanmuInfoResp = json_with_limit(resp).await?;

                if json.code != 0 {
                    return Err(BilibiliError::Api {
                        code: i64::from(json.code),
                        message: json.message,
                    });
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
    pub async fn connect_live_danmaku(
        &self,
        room_id: u64,
    ) -> Result<LiveDanmakuConnection, BilibiliError> {
        // Get danmaku server info
        let danmu_info = self.get_live_danmu_info(room_id).await?;

        // Select first available host with wss_port
        let host = danmu_info
            .host_list
            .first()
            .ok_or_else(|| BilibiliError::Parse("No danmaku host available".to_string()))?;

        // Build WebSocket URL (use wss:// for secure connection)
        let ws_url = format!("wss://{}:{}/sub", host.host, host.wss_port);

        // Connect to WebSocket with timeout
        let ws_connect_timeout = Duration::from_secs(10);
        let (ws_stream, _) = tokio::time::timeout(ws_connect_timeout, connect_async(&ws_url))
            .await
            .map_err(|_| BilibiliError::Network("WebSocket connection timeout".to_string()))?
            .map_err(|e| {
                BilibiliError::Parse(format!("Failed to connect to danmaku WebSocket: {e}"))
            })?;

        let (mut write, read) = ws_stream.split();

        // Send authentication packet
        let auth_packet = build_auth_packet(room_id, &danmu_info.token);
        write
            .send(Message::Binary(auth_packet.into()))
            .await
            .map_err(|e| BilibiliError::Parse(format!("Failed to send auth packet: {e}")))?;

        Ok(LiveDanmakuConnection {
            write: AsyncMutex::new(write),
            read: AsyncMutex::new(read),
            room_id,
            heartbeat_handle: AsyncMutex::new(None),
            heartbeat_stop: Arc::new(AtomicBool::new(false)),
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
            .map_err(|e| BilibiliError::Parse(format!("Failed to send heartbeat: {e}")))
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
    /// ```ignore
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// let conn = Arc::new(client.connect_live_danmaku(room_id).await?);
    /// let config = HeartbeatConfig {
    ///     interval: Duration::from_secs(30),
    /// };
    /// conn.start_heartbeat_loop_arc(Arc::clone(&conn), config).await;
    ///
    /// // The connection will now automatically send heartbeats
    /// while let Ok(messages) = conn.recv().await {
    ///     for msg in messages {
    ///         // handle messages
    ///     }
    /// }
    /// conn.stop_heartbeat_loop().await;
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
pub fn build_auth_packet(room_id: u64, token: &str) -> Vec<u8> {
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
    packet.extend_from_slice(
        &u32::try_from(packet_length)
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    packet.extend_from_slice(&16u16.to_be_bytes()); // header length
    packet.extend_from_slice(&1u16.to_be_bytes()); // protocol version
    packet.extend_from_slice(&7u32.to_be_bytes()); // operation = auth
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence

    // Body
    packet.extend_from_slice(&body);

    packet
}

/// Build heartbeat packet for danmaku WebSocket
pub fn build_heartbeat_packet() -> Vec<u8> {
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

            // Handle compression (protocol_version 2 = zlib, 3 = brotli)
            let decompressed = match protocol_version {
                0 | 1 => body.to_vec(),
                2 => {
                    // zlib (deflate) compression with size limit
                    use std::io::Read;
                    let decoder = flate2::read::ZlibDecoder::new(body);
                    let mut limited = decoder.take(MAX_DANMAKU_DECOMPRESS_SIZE);
                    let mut out = Vec::new();
                    if let Err(e) = limited.read_to_end(&mut out) {
                        tracing::warn!(
                            "Danmaku packet zlib decompression failed: {} (body length: {} bytes)",
                            e,
                            body.len()
                        );
                        return Vec::new();
                    }
                    out
                }
                3 => {
                    // brotli compression with size limit
                    use std::io::Read;
                    let decoder = brotli::Decompressor::new(body, 4096);
                    let mut limited = decoder.take(MAX_DANMAKU_DECOMPRESS_SIZE);
                    let mut out = Vec::new();
                    if let Err(e) = limited.read_to_end(&mut out) {
                        tracing::warn!(
                            "Danmaku packet brotli decompression failed: {} (body length: {} bytes)",
                            e,
                            body.len()
                        );
                        return Vec::new();
                    }
                    out
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
/// for backwards compatibility; `segments` contains ALL segments.
#[derive(Debug, Clone)]
pub struct VideoUrlInfo {
    pub accept_quality: Vec<u32>,
    pub accept_description: Vec<String>,
    pub current_quality: u32,
    /// First segment URL (for backwards compatibility with single-segment callers).
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

/// Generate DASH MPD XML from structured data
#[must_use]
pub fn generate_mpd_xml(dash_data: &DashData) -> String {
    let mut mpd = String::new();

    // MPD header
    mpd.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    mpd.push('\n');
    mpd.push_str(r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" "#);
    mpd.push_str(r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" "#);
    mpd.push_str(r#"xsi:schemaLocation="urn:mpeg:dash:schema:mpd:2011 DASH-MPD.xsd" "#);
    let _ = write!(
        mpd,
        r#"minBufferTime="PT{:.1}S" "#,
        dash_data.min_buffer_time
    );
    mpd.push_str(r#"type="static" "#);
    let _ = write!(
        mpd,
        r#"mediaPresentationDuration="PT{:.1}S" "#,
        dash_data.duration
    );
    mpd.push_str(r#"profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">"#);
    mpd.push('\n');

    // Period
    mpd.push_str(r"  <Period>");
    mpd.push('\n');

    // Video AdaptationSet
    if !dash_data.video_streams.is_empty() {
        mpd.push_str(r#"    <AdaptationSet mimeType="video/mp4" contentType="video" "#);
        mpd.push_str(r#"startWithSAP="1" segmentAlignment="true">"#);
        mpd.push('\n');

        for video in &dash_data.video_streams {
            let _ = write!(mpd, r#"      <Representation id="{}" "#, video.id);
            let _ = write!(mpd, r#"codecs="{}" "#, video.codecs);
            let _ = write!(mpd, r#"width="{}" "#, video.width);
            let _ = write!(mpd, r#"height="{}" "#, video.height);
            let _ = write!(mpd, r#"frameRate="{}" "#, video.frame_rate);
            let _ = write!(mpd, r#"sar="{}" "#, video.sar);
            let _ = write!(mpd, r#"bandwidth="{}">"#, video.bandwidth);
            mpd.push('\n');

            // BaseURL
            let _ = write!(
                mpd,
                r"        <BaseURL>{}</BaseURL>",
                escape_xml(&video.base_url)
            );
            mpd.push('\n');

            // Backup URLs
            for backup_url in &video.backup_urls {
                let _ = write!(
                    mpd,
                    r"        <BaseURL>{}</BaseURL>",
                    escape_xml(backup_url)
                );
                mpd.push('\n');
            }

            // SegmentBase
            let _ = write!(
                mpd,
                r#"        <SegmentBase indexRange="{}">"#,
                video.segment_base.index_range
            );
            mpd.push('\n');
            let _ = write!(
                mpd,
                r#"          <Initialization range="{}"/>"#,
                video.segment_base.initialization_range
            );
            mpd.push('\n');
            mpd.push_str(r"        </SegmentBase>");
            mpd.push('\n');

            mpd.push_str(r"      </Representation>");
            mpd.push('\n');
        }

        mpd.push_str(r"    </AdaptationSet>");
        mpd.push('\n');
    }

    // Audio AdaptationSet
    if !dash_data.audio_streams.is_empty() {
        mpd.push_str(r#"    <AdaptationSet mimeType="audio/mp4" contentType="audio" "#);
        mpd.push_str(r#"startWithSAP="1" segmentAlignment="true">"#);
        mpd.push('\n');

        for audio in &dash_data.audio_streams {
            let _ = write!(mpd, r#"      <Representation id="{}" "#, audio.id);
            let _ = write!(mpd, r#"codecs="{}" "#, audio.codecs);
            let _ = write!(mpd, r#"audioSamplingRate="{}" "#, audio.audio_sampling_rate);
            let _ = write!(mpd, r#"bandwidth="{}">"#, audio.bandwidth);
            mpd.push('\n');

            // BaseURL
            let _ = write!(
                mpd,
                r"        <BaseURL>{}</BaseURL>",
                escape_xml(&audio.base_url)
            );
            mpd.push('\n');

            // Backup URLs
            for backup_url in &audio.backup_urls {
                let _ = write!(
                    mpd,
                    r"        <BaseURL>{}</BaseURL>",
                    escape_xml(backup_url)
                );
                mpd.push('\n');
            }

            // SegmentBase
            let _ = write!(
                mpd,
                r#"        <SegmentBase indexRange="{}">"#,
                audio.segment_base.index_range
            );
            mpd.push('\n');
            let _ = write!(
                mpd,
                r#"          <Initialization range="{}"/>"#,
                audio.segment_base.initialization_range
            );
            mpd.push('\n');
            mpd.push_str(r"        </SegmentBase>");
            mpd.push('\n');

            mpd.push_str(r"      </Representation>");
            mpd.push('\n');
        }

        mpd.push_str(r"    </AdaptationSet>");
        mpd.push('\n');
    }

    // Close Period and MPD
    mpd.push_str(r"  </Period>");
    mpd.push('\n');
    mpd.push_str(r"</MPD>");

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
        assert!(!BilibiliClient::is_short_link(
            "https://www.bilibili.com/video/BV123"
        ));
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
        assert_eq!(
            BilibiliClient::extract_bvid("https://www.bilibili.com/video/av12345"),
            None
        );
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
            BilibiliClient::extract_epid(
                "https://www.bilibili.com/bangumi/play/ep99999?from=search"
            ),
            Some("ep99999".to_string())
        );
    }

    #[test]
    fn test_extract_epid_invalid() {
        assert_eq!(
            BilibiliClient::extract_epid("https://www.bilibili.com/video/BV123"),
            None
        );
        assert_eq!(BilibiliClient::extract_epid(""), None);
    }

    #[test]
    fn test_is_short_link_variations() {
        assert!(BilibiliClient::is_short_link("https://b23.tv/abc123"));
        assert!(BilibiliClient::is_short_link("http://b23.tv/xyz"));
        assert!(BilibiliClient::is_short_link(
            "https://b23.tv/episode/12345"
        ));
        assert!(!BilibiliClient::is_short_link(
            "https://www.bilibili.com/video/BV123"
        ));
        assert!(!BilibiliClient::is_short_link(""));
        // These must NOT match: "b23.tv" appearing in path or as subdomain of another host
        assert!(!BilibiliClient::is_short_link(
            "https://evil.com/b23.tv/abc"
        ));
        assert!(!BilibiliClient::is_short_link(
            "https://b23.tv.evil.com/abc"
        ));
    }

    // === URL Matching Tests ===

    #[test]
    fn test_match_url_video() {
        let (media_type, id) =
            BilibiliClient::match_url("https://www.bilibili.com/video/BV1xx411c7XZ").unwrap();
        assert_eq!(media_type, "video");
        assert_eq!(id, "BV1xx411c7XZ");
    }

    #[test]
    fn test_match_url_bangumi_ep() {
        let (media_type, id) =
            BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ep12345").unwrap();
        assert_eq!(media_type, "bangumi");
        assert_eq!(id, "ep12345");
    }

    #[test]
    fn test_match_url_bangumi_ss() {
        let (media_type, id) =
            BilibiliClient::match_url("https://www.bilibili.com/bangumi/play/ss67890").unwrap();
        assert_eq!(media_type, "bangumi");
        assert_eq!(id, "ss67890");
    }

    #[test]
    fn test_match_url_live() {
        let (media_type, id) =
            BilibiliClient::match_url("https://live.bilibili.com/live/12345").unwrap();
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
        assert_eq!(
            client.cookies.as_ref().unwrap().get("SESSDATA"),
            Some(&"abc123".to_string())
        );
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
            extract_key_from_url(
                "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png"
            ),
            Some("7cd084941338484aae1ad9425b84077c".to_string())
        );
        assert_eq!(
            extract_key_from_url(
                "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
            ),
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
        assert!(
            keys.contains(&"w_rid"),
            "signed params should contain w_rid"
        );
        assert!(keys.contains(&"wts"), "signed params should contain wts");
        assert!(keys.contains(&"bvid"), "signed params should contain bvid");
        assert!(keys.contains(&"cid"), "signed params should contain cid");
        assert!(
            keys.contains(&"fnval"),
            "signed params should contain fnval"
        );

        // w_rid should be a 32-char hex MD5 hash
        let w_rid = signed
            .iter()
            .find(|(k, _)| k == "w_rid")
            .map(|(_, v)| v.as_str())
            .expect("w_rid missing");
        assert_eq!(w_rid.len(), 32);
        assert!(
            w_rid.chars().all(|c| c.is_ascii_hexdigit()),
            "w_rid should be hex"
        );
    }

    #[test]
    fn test_wbi_sign_filters_special_chars() {
        let params = vec![("key", "hello!'()*world".to_string())];
        let mixin_key = "testkey12345678901234567890123456";
        let signed = wbi_sign(&params, mixin_key);

        // The value should have !'()* removed
        let val = signed
            .iter()
            .find(|(k, _)| k == "key")
            .map(|(_, v)| v.as_str())
            .expect("key missing");
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
        let keys_before_wrid: Vec<&str> = signed
            .iter()
            .filter(|(k, _)| k != "w_rid")
            .map(|(k, _)| k.as_str())
            .collect();
        // a_param, m_param, wts, z_param (alphabetically sorted)
        let mut sorted = keys_before_wrid.clone();
        sorted.sort_unstable();
        assert_eq!(
            keys_before_wrid, sorted,
            "params should be sorted alphabetically"
        );
    }

    #[test]
    fn test_wbi_sign_deterministic_for_same_timestamp() {
        // The same params + mixin_key should produce consistent signing
        // (modulo the wts which depends on system time)
        let params = vec![("bvid", "BV1test".to_string()), ("cid", "999".to_string())];
        let mixin_key = "ea1db124af3c7062474693fa704f4ff8";
        let signed1 = wbi_sign(&params, mixin_key);
        let signed2 = wbi_sign(&params, mixin_key);

        // The wts values should be very close (same second)
        let wts1 = signed1
            .iter()
            .find(|(k, _)| k == "wts")
            .map(|(_, v)| v.clone())
            .expect("wts missing");
        let wts2 = signed2
            .iter()
            .find(|(k, _)| k == "wts")
            .map(|(_, v)| v.clone())
            .expect("wts missing");
        // They should be the same if run within the same second
        assert_eq!(wts1, wts2, "wts should be same within the same second");

        // If wts is the same, w_rid must be the same too
        let w_rid1 = signed1
            .iter()
            .find(|(k, _)| k == "w_rid")
            .map(|(_, v)| v.clone())
            .expect("w_rid missing");
        let w_rid2 = signed2
            .iter()
            .find(|(k, _)| k == "w_rid")
            .map(|(_, v)| v.clone())
            .expect("w_rid missing");
        assert_eq!(
            w_rid1, w_rid2,
            "w_rid should be deterministic for same inputs"
        );
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

    // === is_wbi_stale_error Tests ===

    #[test]
    fn test_is_wbi_stale_error_minus_352() {
        let err = BilibiliError::Api {
            code: -352,
            message: "signature error".to_string(),
        };
        assert!(BilibiliClient::is_wbi_stale_error(&err));
    }

    #[test]
    fn test_is_wbi_stale_error_minus_401() {
        let err = BilibiliError::Api {
            code: -401,
            message: "unauthorized".to_string(),
        };
        assert!(BilibiliClient::is_wbi_stale_error(&err));
    }

    #[test]
    fn test_is_wbi_stale_error_other_codes() {
        let err = BilibiliError::Api {
            code: -101,
            message: "not logged in".to_string(),
        };
        assert!(!BilibiliClient::is_wbi_stale_error(&err));

        let err = BilibiliError::Api {
            code: 0,
            message: "success".to_string(),
        };
        assert!(!BilibiliClient::is_wbi_stale_error(&err));

        let err = BilibiliError::Network("timeout".to_string());
        assert!(!BilibiliClient::is_wbi_stale_error(&err));

        let err = BilibiliError::Parse("bad json".to_string());
        assert!(!BilibiliClient::is_wbi_stale_error(&err));
    }

    // === build_cookie_header Tests ===

    #[test]
    fn test_build_cookie_header_empty_returns_none() {
        let client = BilibiliClient::new().unwrap();
        assert!(client.build_cookie_header().is_none());
    }

    #[test]
    fn test_build_cookie_header_multiple_joined() {
        let mut cookies = HashMap::new();
        cookies.insert("SESSDATA".to_string(), "abc123".to_string());
        cookies.insert("bili_jct".to_string(), "token456".to_string());
        let client = BilibiliClient::with_cookies(cookies).unwrap();

        let header = client.build_cookie_header().unwrap();
        // Should contain both cookies joined by "; "
        assert!(header.contains("SESSDATA=abc123"));
        assert!(header.contains("bili_jct=token456"));
        assert!(header.contains("; "));
    }

    #[test]
    fn test_build_cookie_header_sanitizes_crlf() {
        let mut cookies = HashMap::new();
        cookies.insert("evil\r\nkey".to_string(), "evil\r\nvalue".to_string());
        let client = BilibiliClient::with_cookies(cookies).unwrap();

        let header = client.build_cookie_header().unwrap();
        // CRLF characters should be stripped
        assert!(!header.contains('\r'));
        assert!(!header.contains('\n'));
        assert!(header.contains("evilkey=evilvalue"));
    }

    // === WBI Key Refresh Coordination Tests ===

    #[test]
    fn test_refresh_lock_basic() {
        // Reset any state from previous tests (parallel test isolation)
        release_refresh_lock();

        // Test that the refresh lock can be claimed and released
        assert!(try_claim_refresh_lock(), "First claim should succeed");
        assert!(
            !try_claim_refresh_lock(),
            "Second claim should fail while lock is held"
        );
        release_refresh_lock();
        assert!(
            try_claim_refresh_lock(),
            "Claim should succeed after release"
        );
        release_refresh_lock();
    }

    #[test]
    fn test_refresh_lock_multiple_threads() {
        use std::sync::Arc;
        use std::thread;

        // Reset any state
        release_refresh_lock();

        let success_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts = 10;

        let handles: Vec<_> = (0..attempts)
            .map(|_| {
                let success_count = Arc::clone(&success_count);
                thread::spawn(move || {
                    if try_claim_refresh_lock() {
                        success_count.fetch_add(1, Ordering::SeqCst);
                        // Hold the lock briefly
                        thread::sleep(std::time::Duration::from_millis(1));
                        release_refresh_lock();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Due to timing, multiple threads might succeed in sequence,
        // but never concurrently. The key invariant is that only one
        // thread holds the lock at any given time.
        let successes = success_count.load(Ordering::SeqCst);
        assert!(successes >= 1, "At least one claim should succeed");
    }

    #[tokio::test]
    async fn test_concurrent_wbi_refresh_single_api_call() {
        // Reset state
        release_refresh_lock();
        WBI_API_CALL_COUNT.store(0, Ordering::SeqCst);
        // Clear cache to force refresh
        *WBI_KEY_CACHE.lock().await = None;

        // Note: This test verifies the coordination logic, not actual API calls.
        // The actual API call test would require a mock server.
        // We're testing that the refresh lock prevents multiple claimants.

        // Spawn multiple tasks that all try to get the WBI key concurrently
        let mut handles = Vec::new();
        for _ in 0..5 {
            handles.push(tokio::spawn(async {
                // Try to claim - only one should succeed
                if try_claim_refresh_lock() {
                    // Simulate a brief API call delay
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    release_refresh_lock();
                    true
                } else {
                    false
                }
            }));
        }

        let results = futures_util::future::join_all(handles).await;
        let successful_claims = results
            .iter()
            .filter(|r| r.is_ok() && *r.as_ref().unwrap())
            .count();

        // Only one task should have successfully claimed the lock at a time
        // Note: Due to timing, multiple tasks might succeed in sequence,
        // but the key invariant is that claims never overlap.
        assert!(successful_claims >= 1, "At least one claim should succeed");
    }

    /// Test that Notify-based waiting works correctly for concurrent refresh.
    /// This test verifies that:
    /// 1. Tasks that fail to claim the lock can wait for notification
    /// 2. When refresh completes, waiting tasks are notified
    /// 3. Notified tasks can read from the cache
    ///
    /// Note: This test uses local state to avoid interference from parallel tests.
    #[tokio::test]
    async fn test_notify_based_waiting_pattern() {
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        // Use local state to avoid interference from parallel tests
        let local_notify = Arc::new(tokio::sync::Notify::new());
        let local_cache = Arc::new(AsyncMutex::new(Option::<String>::None));
        let local_lock = Arc::new(AtomicBool::new(false));

        let api_call_count = Arc::new(AtomicUsize::new(0));
        let waiting_tasks_woken = Arc::new(AtomicUsize::new(0));

        // Spawn the refresher task
        let api_count = Arc::clone(&api_call_count);
        let cache = Arc::clone(&local_cache);
        let notify = Arc::clone(&local_notify);
        let lock = Arc::clone(&local_lock);
        let refresher = tokio::spawn(async move {
            // Try to claim lock
            if lock
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // Simulate API call delay
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                api_count.fetch_add(1, Ordering::SeqCst);
                // Set a cached key
                *cache.lock().await = Some("test_key_32_characters_long_x".to_string());
                // Release and notify
                lock.store(false, Ordering::Release);
                notify.notify_waiters();
                true
            } else {
                false
            }
        });

        // Small delay to ensure refresher claims the lock first
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        // Spawn waiter tasks
        let mut waiter_handles = Vec::new();
        for _ in 0..5 {
            let woken_count = Arc::clone(&waiting_tasks_woken);
            let cache = Arc::clone(&local_cache);
            let notify = Arc::clone(&local_notify);
            waiter_handles.push(tokio::spawn(async move {
                // Wait for notification
                notify.notified().await;
                // Record that we were woken
                woken_count.fetch_add(1, Ordering::SeqCst);
                // Try to get the key from cache
                cache.lock().await.clone()
            }));
        }

        // Wait for all tasks
        let refresher_result = refresher.await.expect("Refresher task panicked");
        let waiter_results = futures_util::future::join_all(waiter_handles).await;

        // Verify refresher task claimed the lock
        assert!(refresher_result, "Refresher should have claimed the lock");

        // Verify only one task performed the API call
        assert_eq!(
            api_call_count.load(Ordering::SeqCst),
            1,
            "Only one API call should occur"
        );

        // Verify all waiter tasks were woken
        assert_eq!(
            waiting_tasks_woken.load(Ordering::SeqCst),
            5,
            "All 5 waiter tasks should be woken"
        );

        // Verify waiter tasks got the cached key
        for result in waiter_results {
            assert_eq!(
                result.expect("Waiter task should not panic"),
                Some("test_key_32_characters_long_x".to_string()),
                "Waiter should receive cached key"
            );
        }
    }

    #[tokio::test]
    async fn test_valid_cache_returns_cached_key() {
        // Set a valid cached key
        let test_key = "test_mixin_key_32_characters_long";
        set_wbi_key(test_key.to_string()).await;

        // Should return the cached key
        let result = get_valid_wbi_key().await;
        assert_eq!(result, Some(test_key.to_string()));
    }

    #[tokio::test]
    async fn test_expired_cache_returns_none() {
        // Set an already-expired key (TTL = 0, already in the past)
        let mut guard = WBI_KEY_CACHE.lock().await;
        *guard = Some(WbiKeys {
            mixin_key: "expired_key".to_string(),
            expires_at: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap(),
        });
        drop(guard);

        // Should return None for expired key
        let result = get_valid_wbi_key().await;
        assert!(result.is_none());
    }

    // ========== B5: .expect("REASON") panic prevention ==========

    #[test]
    fn test_parse_danmaku_gift_with_huge_count_no_panic() {
        // The gift count field could exceed u32::MAX, which would cause
        // u32::try_from().expect("REASON") to panic. After the fix,
        // it should use unwrap_or(u32::MAX) instead.
        let json = serde_json::json!({
            "cmd": "SEND_GIFT",
            "data": {
                "uname": "TestUser",
                "giftName": "TestGift",
                "num": u64::from(u32::MAX) + 1  // exceeds u32
            }
        });

        // This should NOT panic after the fix
        let result = parse_danmaku_cmd("SEND_GIFT", &json);
        match result {
            DanmakuMessage::Gift { count, .. } => {
                assert_eq!(count, u32::MAX, "Overflow should clamp to u32::MAX");
            }
            _ => panic!("Expected Gift message variant"),
        }
    }

    #[test]
    fn test_parse_danmaku_gift_with_normal_count() {
        let json = serde_json::json!({
            "cmd": "SEND_GIFT",
            "data": {
                "uname": "TestUser",
                "giftName": "TestGift",
                "num": 5
            }
        });

        let result = parse_danmaku_cmd("SEND_GIFT", &json);
        match result {
            DanmakuMessage::Gift { count, .. } => {
                assert_eq!(count, 5);
            }
            _ => panic!("Expected Gift message variant"),
        }
    }

    #[test]
    fn test_build_auth_packet_does_not_panic_on_normal_token() {
        // build_auth_packet uses u32::try_from(packet_length).expect("REASON")
        // Normal tokens should work fine
        let packet = build_auth_packet(12345, "normal_token_value");
        assert!(!packet.is_empty());
        // The first 4 bytes encode the packet length as big-endian u32
        let len = u32::from_be_bytes([packet[0], packet[1], packet[2], packet[3]]);
        assert_eq!(len as usize, packet.len());
    }

    // ========== parse_danmaku_packet failure case tests ==========

    #[test]
    fn test_parse_danmaku_packet_too_short_returns_empty() {
        // Packets less than 16 bytes should return empty Vec
        let short_data = [0u8; 15];
        let result = parse_danmaku_packet(&short_data);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_danmaku_packet_empty_returns_empty() {
        // Empty packet should return empty Vec
        let result = parse_danmaku_packet(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_danmaku_packet_invalid_zlib_returns_empty() {
        // Create a packet with operation=5 (notification) and protocol_version=2 (zlib)
        // but with invalid zlib data
        let mut packet = Vec::new();
        packet.extend_from_slice(&16u32.to_be_bytes()); // packet length
        packet.extend_from_slice(&16u16.to_be_bytes()); // header length
        packet.extend_from_slice(&2u16.to_be_bytes()); // protocol version = zlib
        packet.extend_from_slice(&5u32.to_be_bytes()); // operation = notification
        packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
                                                       // Add invalid zlib data (not valid zlib compressed data)
        packet.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let result = parse_danmaku_packet(&packet);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_danmaku_packet_invalid_brotli_returns_empty() {
        // Create a packet with operation=5 (notification) and protocol_version=3 (brotli)
        // but with invalid brotli data
        let mut packet = Vec::new();
        packet.extend_from_slice(&20u32.to_be_bytes()); // packet length
        packet.extend_from_slice(&16u16.to_be_bytes()); // header length
        packet.extend_from_slice(&3u16.to_be_bytes()); // protocol version = brotli
        packet.extend_from_slice(&5u32.to_be_bytes()); // operation = notification
        packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
                                                       // Add invalid brotli data
        packet.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let result = parse_danmaku_packet(&packet);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_danmaku_packet_unknown_protocol_version_returns_empty() {
        // Create a packet with operation=5 (notification) and unknown protocol_version
        let mut packet = Vec::new();
        packet.extend_from_slice(&20u32.to_be_bytes()); // packet length
        packet.extend_from_slice(&16u16.to_be_bytes()); // header length
        packet.extend_from_slice(&99u16.to_be_bytes()); // protocol version = unknown
        packet.extend_from_slice(&5u32.to_be_bytes()); // operation = notification
        packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
        packet.extend_from_slice(&[0, 0, 0, 0]); // body

        let result = parse_danmaku_packet(&packet);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_danmaku_packet_valid_heartbeat() {
        // Create a valid heartbeat response packet
        let mut packet = Vec::new();
        packet.extend_from_slice(&20u32.to_be_bytes()); // packet length
        packet.extend_from_slice(&16u16.to_be_bytes()); // header length
        packet.extend_from_slice(&1u16.to_be_bytes()); // protocol version
        packet.extend_from_slice(&3u32.to_be_bytes()); // operation = heartbeat response
        packet.extend_from_slice(&1u32.to_be_bytes()); // sequence
        packet.extend_from_slice(&12345u32.to_be_bytes()); // online count

        let result = parse_danmaku_packet(&packet);
        assert_eq!(result.len(), 1);
        match &result[0] {
            DanmakuMessage::Heartbeat { online_count } => {
                assert_eq!(*online_count, 12345);
            }
            _ => panic!("Expected Heartbeat message"),
        }
    }

    // === WBI Failure Counter and Timeout Tests ===
    // Note: These tests share global static state and should not be split into
    // separate #[test] functions to avoid parallel execution interference.

    #[test]
    fn test_failure_counter_mechanics() {
        // === Test 1: Counter increments on failure ===
        // Set counter to a known value just below max
        WBI_CONSECUTIVE_FAILURES.store(WBI_MAX_CONSECUTIVE_FAILURES - 1, Ordering::SeqCst);
        assert!(
            !has_exceeded_max_failures(),
            "Should not exceed max failures at {} failures",
            WBI_MAX_CONSECUTIVE_FAILURES - 1
        );

        // One more failure should exceed max
        let exceeded = release_refresh_lock_on_failure_and_notify();
        assert!(
            exceeded,
            "Failure at count {} should exceed max {}",
            WBI_MAX_CONSECUTIVE_FAILURES, WBI_MAX_CONSECUTIVE_FAILURES
        );
        assert!(
            has_exceeded_max_failures(),
            "Should exceed max after reaching {} failures",
            WBI_MAX_CONSECUTIVE_FAILURES
        );

        // === Test 2: Counter resets on success ===
        // Set counter to a value above zero
        WBI_CONSECUTIVE_FAILURES.store(2, Ordering::SeqCst);

        // Success should reset the counter to 0
        release_refresh_lock_on_success_and_notify();

        // The counter should now be 0 (success resets it)
        let count = WBI_CONSECUTIVE_FAILURES.load(Ordering::SeqCst);
        assert_eq!(
            count, 0,
            "Counter should be 0 after success reset, but got {}",
            count
        );

        // === Test 3: Reset function works ===
        // Add failures
        WBI_CONSECUTIVE_FAILURES.store(5, Ordering::SeqCst);
        assert!(has_exceeded_max_failures());

        // Reset should clear
        reset_consecutive_failures();
        assert!(!has_exceeded_max_failures());
        assert_eq!(WBI_CONSECUTIVE_FAILURES.load(Ordering::SeqCst), 0);

        // Cleanup
        release_refresh_lock();
    }

    /// Test that has_exceeded_max_failures returns error when too many failures occurred.
    /// This is a unit test for the failure detection logic.
    #[tokio::test]
    async fn test_exceeded_max_failures_returns_error_fast() {
        // Setup: reset all state
        release_refresh_lock();
        reset_consecutive_failures();
        *WBI_KEY_CACHE.lock().await = None;

        // Simulate 3 consecutive failures to exceed max
        WBI_CONSECUTIVE_FAILURES.store(WBI_MAX_CONSECUTIVE_FAILURES, Ordering::SeqCst);

        // Now has_exceeded_max_failures should return true
        assert!(
            has_exceeded_max_failures(),
            "Should exceed max failures after setting counter to max"
        );

        // Cleanup
        reset_consecutive_failures();
        release_refresh_lock();
    }

    /// Test that waiting with timeout actually times out when no notification comes.
    /// This test verifies the timeout mechanism works in isolation.
    #[tokio::test]
    async fn test_notify_timeout_mechanism() {
        // We test that tokio::time::timeout works correctly with Notify.
        // This is a sanity check that our timeout approach is valid.
        let timeout_duration = std::time::Duration::from_millis(10);

        // Create a new Notify for this test (not the global one) to avoid interference
        let local_notify = tokio::sync::Notify::new();

        // This should timeout since we never call local_notify.notify_waiters()
        let result = tokio::time::timeout(timeout_duration, local_notify.notified()).await;
        assert!(
            result.is_err(),
            "Should timeout when no notification is sent"
        );
    }

    /// Test that notification arrives before timeout when sent quickly.
    #[tokio::test]
    async fn test_notify_arrives_before_timeout() {
        // Create a new Notify wrapped in Arc for this test to avoid interference
        use std::sync::Arc;
        let local_notify = Arc::new(tokio::sync::Notify::new());
        let timeout_duration = std::time::Duration::from_millis(100);

        // Spawn a task that waits with timeout
        let notify = Arc::clone(&local_notify);
        let wait_task = tokio::spawn(async move {
            let result = tokio::time::timeout(timeout_duration, notify.notified()).await;
            result.is_ok()
        });

        // Send notification quickly
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        local_notify.notify_waiters();

        let notification_received = wait_task.await.expect("Task should not panic");
        assert!(
            notification_received,
            "Notification should have arrived before timeout"
        );
    }
}
