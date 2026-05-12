#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyErrorKind {
    Cancelled,
    Timeout,
    Connection,
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
    err.downcast_ref::<ProxyError>().map(ProxyError::kind)
}
