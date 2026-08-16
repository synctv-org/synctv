use std::error::Error;
use std::future::Future;
use std::time::Duration;

use synctv_common::ExecutionControl;

use crate::{
    reqwest_error_message_indicates_connection_failure, run_with_proxy_cancellation, ProxyError,
};

/// Maximum number of redirects to follow manually.
const MAX_REDIRECTS: usize = 10;

/// Headers that should be preserved across redirect hops.
///
/// Provider-controlled headers and cache-generated headers are re-applied on
/// each redirect to avoid breaking providers that require them on the final
/// CDN request.
pub(crate) const REDIRECT_PRESERVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "origin",
    "proxy-authorization",
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
/// Credential headers stay within one origin. Cross-origin `Referer` values
/// are reduced to their origin below, matching `strict-origin-when-cross-origin`.
const CROSS_ORIGIN_DROP_HEADERS: &[&str] = &["authorization", "cookie", "proxy-authorization"];

fn preserve_redirect_header(name: &str, is_cross_origin: bool) -> bool {
    REDIRECT_PRESERVE_HEADERS.contains(&name)
        && !(is_cross_origin && CROSS_ORIGIN_DROP_HEADERS.contains(&name))
}

fn redirect_header_value(
    name: &str,
    value: &reqwest::header::HeaderValue,
    is_cross_origin: bool,
) -> Option<reqwest::header::HeaderValue> {
    if !preserve_redirect_header(name, is_cross_origin) {
        return None;
    }
    if !is_cross_origin || name != "referer" {
        return Some(value.clone());
    }
    let referer = url::Url::parse(value.to_str().ok()?).ok()?;
    let origin = referer.origin().ascii_serialization();
    (origin != "null")
        .then(|| reqwest::header::HeaderValue::from_str(&format!("{origin}/")).ok())
        .flatten()
}

/// Result of `send_with_redirect_validation`.
pub struct ProxyResponse {
    /// The final HTTP response after following any redirects.
    pub response: reqwest::Response,
    /// `true` if at least one redirect was followed.
    ///
    /// When redirects occurred the response body has been fully consumed and
    /// re-requested at the final URL, so `Content-Encoding` must be stripped
    /// from the forwarded headers regardless of the encoding value (the body
    /// is already decoded by reqwest).
    pub followed_redirects: bool,
}

pub(crate) async fn send_head_with_redirect_validation_with_control_and_timeout(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    request_control: Option<&ExecutionControl>,
    header_timeout: Option<Duration>,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_inner(
        client,
        request,
        reqwest::Method::HEAD,
        ssrf_guard,
        request_control,
        header_timeout,
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
#[cfg(test)]
pub(crate) async fn send_with_redirect_validation(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_with_control(client, request, ssrf_guard, None).await
}

#[cfg(test)]
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

pub async fn send_with_redirect_validation_with_control_and_timeout(
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

    let preserved: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> = built
        .headers()
        .iter()
        .filter(|(name, _)| preserve_redirect_header(name.as_str(), false))
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

        let is_cross_origin =
            location.origin().ascii_serialization() != current_url.origin().ascii_serialization();
        validate_target_url_against_ssrf(&location, ssrf_guard)?;

        let mut redirect_req = client.request(redirect_method.clone(), location.clone());
        for (name, value) in &preserved {
            let Some(value) = redirect_header_value(name.as_str(), value, is_cross_origin) else {
                continue;
            };
            redirect_req = redirect_req.header(name.clone(), value);
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
    let port = url.port_or_known_default().ok_or_else(|| {
        ProxyError::InvalidRequest("URL port could not be determined".to_string())
    })?;

    guard
        .validate_url_target(host, port)
        .map_err(|error| ProxyError::Ssrf(error.to_string()))?;
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
    let proxy_error = if reqwest_error_has_ssrf_resolution_block(error) {
        ProxyError::Ssrf(message)
    } else if error.is_timeout() {
        ProxyError::Timeout(message)
    } else if error.is_connect() || reqwest_error_message_indicates_connection_failure(&message) {
        ProxyError::Connection(message)
    } else {
        ProxyError::Upstream(message)
    };
    proxy_error.into()
}

fn reqwest_error_has_ssrf_resolution_block(error: &reqwest::Error) -> bool {
    let mut source = error.source();
    while let Some(error) = source {
        if error
            .downcast_ref::<synctv_common::ssrf::SsrfResolutionBlocked>()
            .is_some()
        {
            return true;
        }
        source = error.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{preserve_redirect_header, redirect_header_value};

    #[test]
    fn sensitive_headers_only_survive_same_origin_redirects() {
        for header in ["authorization", "cookie", "proxy-authorization"] {
            assert!(preserve_redirect_header(header, false));
            assert!(!preserve_redirect_header(header, true));
        }
        assert!(preserve_redirect_header("origin", false));
        assert!(preserve_redirect_header("origin", true));
        assert!(!preserve_redirect_header("x-private-token", false));
    }

    #[test]
    fn cross_origin_referer_is_reduced_to_its_origin() {
        let referer = reqwest::header::HeaderValue::from_static(
            "https://live.example/private/path?token=secret",
        );
        assert_eq!(
            redirect_header_value("referer", &referer, true)
                .expect("valid referer should survive")
                .to_str()
                .expect("sanitized referer should be ASCII"),
            "https://live.example/"
        );
        assert_eq!(
            redirect_header_value("referer", &referer, false)
                .expect("same-origin referer should survive"),
            referer
        );
    }
}
