use std::future::Future;
use std::time::Duration;

use synctv_common::ExecutionControl;

use crate::{run_with_proxy_cancellation, ProxyError};

/// Maximum number of redirects to follow manually.
const MAX_REDIRECTS: usize = 10;

/// Headers that should be preserved across redirect hops.
///
/// Provider-controlled headers and cache-generated headers are re-applied on
/// each redirect to avoid breaking providers that require them on the final
/// CDN request.
pub(crate) const REDIRECT_PRESERVE_HEADERS: &[&str] = &[
    "referer",
    "user-agent",
    "range",
    "accept",
    "accept-language",
    "if-none-match",
    "if-modified-since",
];

/// Headers to drop when a redirect crosses origin boundaries.
///
/// The `Referer` header can leak the original request URL (including signed
/// query parameters) to a third-party host. Dropping it on cross-origin
/// redirects follows browser `strict-origin-when-cross-origin` behaviour.
const CROSS_ORIGIN_DROP_HEADERS: &[&str] = &["referer"];

/// Result of `send_with_redirect_validation`.
pub(crate) struct ProxyResponse {
    /// The final HTTP response after following any redirects.
    pub(crate) response: reqwest::Response,
    /// `true` if at least one redirect was followed.
    ///
    /// When redirects occurred the response body has been fully consumed and
    /// re-requested at the final URL, so `Content-Encoding` must be stripped
    /// from the forwarded headers regardless of the encoding value (the body
    /// is already decoded by reqwest).
    pub(crate) followed_redirects: bool,
}

pub(crate) async fn send_head_with_redirect_validation_with_control(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    request_control: Option<&ExecutionControl>,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_inner(
        client,
        request,
        reqwest::Method::HEAD,
        ssrf_guard,
        request_control,
        None,
    )
    .await
}

/// Send a request via the proxy client, manually following redirects with
/// full async DNS validation on every hop.
///
/// Automatic redirects are disabled on the injected proxy client, so 3xx responses
/// are handled here. Each redirect target gets both static URL validation
/// and async DNS resolution checks to prevent DNS-rebinding SSRF.
///
/// Headers matching [`REDIRECT_PRESERVE_HEADERS`] are captured from the
/// initial request and re-applied on every redirect hop so that
/// provider-controlled headers are not lost.
pub(crate) async fn send_with_redirect_validation(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_with_control(client, request, ssrf_guard, None).await
}

pub(crate) async fn send_with_redirect_validation_with_control(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    request_control: Option<&ExecutionControl>,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_with_control_and_timeout(
        client,
        request,
        ssrf_guard,
        request_control,
        None,
    )
    .await
}

pub(crate) async fn send_with_redirect_validation_with_control_and_timeout(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    request_control: Option<&ExecutionControl>,
    header_timeout: Option<Duration>,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_inner(
        client,
        request,
        reqwest::Method::GET,
        ssrf_guard,
        request_control,
        header_timeout,
    )
    .await
}

async fn send_with_redirect_validation_inner(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    redirect_method: reqwest::Method,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    request_control: Option<&ExecutionControl>,
    header_timeout: Option<Duration>,
) -> Result<ProxyResponse, anyhow::Error> {
    let built = request
        .build()
        .map_err(|e| ProxyError::InvalidRequest(format!("failed to build proxy request: {e}")))?;
    validate_target_url_against_ssrf(built.url(), ssrf_guard)?;

    let original_origin = built.url().origin().ascii_serialization();

    let preserved: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> = built
        .headers()
        .iter()
        .filter(|(name, _)| REDIRECT_PRESERVE_HEADERS.contains(&name.as_str()))
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();

    let mut response = run_with_proxy_header_control(
        "upstream proxy request headers",
        request_control,
        header_timeout,
        async move { client.execute(built).await },
    )
    .await?
    .map_err(|error| classify_reqwest_error(&error))?;

    let mut hops = 0usize;
    while response.status().is_redirection()
        && response.status() != reqwest::StatusCode::NOT_MODIFIED
    {
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(
                ProxyError::Upstream(format!("too many redirects ({MAX_REDIRECTS} max)")).into(),
            );
        }

        let current_url = response.url().clone();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| ProxyError::Upstream("redirect without Location header".to_string()))?
            .to_str()
            .map_err(|_| ProxyError::Upstream("invalid Location header".to_string()))?
            .to_string();

        let location = current_url.join(&location).map_err(|e| {
            ProxyError::Upstream(format!("invalid redirect target `{location}`: {e}"))
        })?;

        let scheme = location.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(
                ProxyError::Ssrf(format!("redirect to disallowed scheme: {scheme}")).into(),
            );
        }

        let is_cross_origin = location.origin().ascii_serialization() != original_origin;
        if is_cross_origin {
            validate_target_url_against_ssrf(&location, ssrf_guard)?;
        }

        let mut redirect_req = client.request(redirect_method.clone(), location.clone());
        for (name, value) in &preserved {
            if is_cross_origin && CROSS_ORIGIN_DROP_HEADERS.contains(&name.as_str()) {
                continue;
            }
            redirect_req = redirect_req.header(name.clone(), value.clone());
        }

        drop(response);
        response = run_with_proxy_header_control(
            "upstream redirect request headers",
            request_control,
            header_timeout,
            async move { redirect_req.send().await },
        )
        .await?
        .map_err(|error| classify_reqwest_error(&error))?;
    }

    Ok(ProxyResponse {
        response,
        followed_redirects: hops > 0,
    })
}

pub(crate) fn validate_target_url_against_ssrf(
    url: &url::Url,
    guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<(), ProxyError> {
    let host = url
        .host_str()
        .ok_or_else(|| ProxyError::InvalidRequest("URL host is required".to_string()))?;

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if guard.is_ip_blocked(&ip) {
            return Err(ProxyError::Ssrf(format!(
                "target host `{host}` is blocked by SSRF policy"
            )));
        }
    } else if guard.is_host_blocked(host) {
        return Err(ProxyError::Ssrf(format!(
            "target host `{host}` is blocked by SSRF policy"
        )));
    }

    Ok(())
}

async fn run_with_proxy_header_control<T, F>(
    context: &str,
    request_control: Option<&ExecutionControl>,
    header_timeout: Option<Duration>,
    future: F,
) -> Result<T, anyhow::Error>
where
    F: Future<Output = T>,
{
    match header_timeout {
        Some(timeout) => run_with_proxy_cancellation(context, request_control, async move {
            tokio::time::timeout(timeout, future).await
        })
        .await?
        .map_err(|_| ProxyError::Timeout(context.to_string()).into()),
        None => run_with_proxy_cancellation(context, request_control, future).await,
    }
}

fn classify_reqwest_error(error: &reqwest::Error) -> anyhow::Error {
    let message = error.to_string();
    let proxy_error = if error.is_timeout() {
        ProxyError::Timeout(message)
    } else if error.is_connect() {
        ProxyError::Connection(message)
    } else {
        let lower = message.to_ascii_lowercase();
        if lower.contains("private")
            || lower.contains("loopback")
            || lower.contains("disallowed")
            || lower.contains("blocked")
        {
            ProxyError::Ssrf(message)
        } else {
            ProxyError::Other(message)
        }
    };
    proxy_error.into()
}
