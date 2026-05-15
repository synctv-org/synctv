#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyErrorKind {
    Cancelled,
    Timeout,
    Connection,
    BodyTooLarge,
    Ssrf,
    InvalidRequest,
    Upstream,
    Other,
}

impl ProxyErrorKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::BodyTooLarge => "body_too_large",
            Self::Ssrf => "ssrf",
            Self::InvalidRequest => "invalid_request",
            Self::Upstream => "upstream",
            Self::Other => "other",
        }
    }
}

#[derive(Debug)]
pub(crate) enum ProxyError {
    Cancelled(String),
    Timeout(String),
    Connection(String),
    BodyTooLarge(String),
    Ssrf(String),
    InvalidRequest(String),
    Upstream(String),
    Other(String),
}

impl ProxyError {
    pub(crate) const fn kind(&self) -> ProxyErrorKind {
        match self {
            Self::Cancelled(_) => ProxyErrorKind::Cancelled,
            Self::Timeout(_) => ProxyErrorKind::Timeout,
            Self::Connection(_) => ProxyErrorKind::Connection,
            Self::BodyTooLarge(_) => ProxyErrorKind::BodyTooLarge,
            Self::Ssrf(_) => ProxyErrorKind::Ssrf,
            Self::InvalidRequest(_) => ProxyErrorKind::InvalidRequest,
            Self::Upstream(_) => ProxyErrorKind::Upstream,
            Self::Other(_) => ProxyErrorKind::Other,
        }
    }
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled(message) => write!(f, "Request cancelled: {message}"),
            Self::Timeout(message) => write!(f, "Request timed out: {message}"),
            Self::Connection(message) => write!(f, "Connection failed: {message}"),
            Self::BodyTooLarge(message) => write!(f, "Proxy response body too large: {message}"),
            Self::Ssrf(message) => write!(f, "SSRF protection blocked request: {message}"),
            Self::InvalidRequest(message) => write!(f, "Invalid proxy request: {message}"),
            Self::Upstream(message) => write!(f, "Upstream rejected request: {message}"),
            Self::Other(message) => write!(f, "Proxy request failed: {message}"),
        }
    }
}

impl std::error::Error for ProxyError {}

#[must_use]
pub fn proxy_error_kind(err: &anyhow::Error) -> Option<ProxyErrorKind> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<ProxyError>().map(ProxyError::kind))
}

#[must_use]
pub fn proxy_error_kind_from_std_error(
    err: &(dyn std::error::Error + 'static),
) -> Option<ProxyErrorKind> {
    let mut current = Some(err);
    while let Some(cause) = current {
        if let Some(proxy_error) = cause.downcast_ref::<ProxyError>() {
            return Some(proxy_error.kind());
        }
        current = cause.source();
    }
    None
}

pub(crate) fn classify_reqwest_body_error(error: &reqwest::Error) -> ProxyError {
    let message = error.to_string();
    if error.is_timeout() {
        ProxyError::Timeout(message)
    } else if error.is_connect() {
        ProxyError::Connection(message)
    } else {
        let lower = message.to_ascii_lowercase();
        if lower.contains("connection")
            || lower.contains("closed")
            || lower.contains("eof")
            || lower.contains("reset")
            || lower.contains("broken pipe")
            || lower.contains("incomplete")
        {
            ProxyError::Connection(message)
        } else {
            ProxyError::Upstream(message)
        }
    }
}
