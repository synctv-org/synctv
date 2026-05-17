#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyErrorKind {
    Cancelled,
    Timeout,
    Connection,
    BodyTooLarge,
    Ssrf,
    InvalidRequest,
    RangeNotSatisfiable,
    Upstream,
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
            Self::RangeNotSatisfiable => "range_not_satisfiable",
            Self::Upstream => "upstream",
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
    RangeNotSatisfiable { message: String, total_size: u64 },
    Upstream(String),
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
            Self::RangeNotSatisfiable { .. } => ProxyErrorKind::RangeNotSatisfiable,
            Self::Upstream(_) => ProxyErrorKind::Upstream,
        }
    }

    pub(crate) const fn range_not_satisfiable_total_size(&self) -> Option<u64> {
        match self {
            Self::RangeNotSatisfiable { total_size, .. } => Some(*total_size),
            _ => None,
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
            Self::RangeNotSatisfiable { message, .. } => {
                write!(f, "Range not satisfiable: {message}")
            }
            Self::Upstream(message) => write!(f, "Upstream rejected request: {message}"),
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
pub fn proxy_range_not_satisfiable_total_size(err: &anyhow::Error) -> Option<u64> {
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<ProxyError>()
            .and_then(ProxyError::range_not_satisfiable_total_size)
    })
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
    } else if reqwest_error_message_indicates_connection_failure(&message) {
        ProxyError::Connection(message)
    } else {
        ProxyError::Upstream(message)
    }
}

pub(crate) fn reqwest_error_message_indicates_connection_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("connection")
        || lower.contains("closed")
        || lower.contains("eof")
        || lower.contains("reset")
        || lower.contains("broken pipe")
        || lower.contains("incomplete")
}
