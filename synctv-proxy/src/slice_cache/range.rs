//! Range header parsing, Content-Range response parsing, and slice alignment.
//!
//! Mirrors nginx's slice module approach:
//! - Request `Range` header parsing (single byte-range only)
//! - Response `Content-Range` parsing modeled after
//!   `ngx_http_slice_parse_content_range`
//! - Slice-aligned range computation

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientRangePlan {
    Explicit { start: u64, end: u64 },
    OpenEnded { start: u64 },
    Suffix { suffix_len: u64 },
    MultiRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRangeError {
    InvalidRequest(String),
    Unsatisfiable { message: String, total_size: u64 },
}

impl ClientRangeError {
    fn message(&self) -> &str {
        match self {
            Self::InvalidRequest(message) | Self::Unsatisfiable { message, .. } => message,
        }
    }
}

impl fmt::Display for ClientRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ClientRangeError {}

pub fn parse_client_range_plan(range: &str) -> Result<ClientRangePlan, ClientRangeError> {
    let range = range.trim();
    if !range.starts_with("bytes=") {
        return Err(ClientRangeError::InvalidRequest(
            "Invalid range format: must start with 'bytes='".to_string(),
        ));
    }

    let spec = &range["bytes=".len()..];
    if spec.contains(',') {
        return Ok(ClientRangePlan::MultiRange);
    }

    let Some((start_text, end_text)) = spec.split_once('-') else {
        return Err(ClientRangeError::InvalidRequest(
            "Invalid range format".to_string(),
        ));
    };

    if start_text.is_empty() {
        let suffix_len = end_text
            .parse::<u64>()
            .map_err(|_| ClientRangeError::InvalidRequest("Invalid suffix range".to_string()))?;
        if suffix_len == 0 {
            return Err(ClientRangeError::InvalidRequest(
                "Invalid suffix range".to_string(),
            ));
        }
        return Ok(ClientRangePlan::Suffix { suffix_len });
    }

    let start = start_text
        .parse::<u64>()
        .map_err(|_| ClientRangeError::InvalidRequest("Invalid range start".to_string()))?;

    if end_text.is_empty() {
        return Ok(ClientRangePlan::OpenEnded { start });
    }

    let end = end_text
        .parse::<u64>()
        .map_err(|_| ClientRangeError::InvalidRequest("Invalid range end".to_string()))?;
    if start > end {
        return Err(ClientRangeError::InvalidRequest(
            "Range start must not exceed range end".to_string(),
        ));
    }

    Ok(ClientRangePlan::Explicit { start, end })
}

pub fn range_bounds_for_total(
    plan: ClientRangePlan,
    total_size: u64,
) -> Result<(u64, u64), ClientRangeError> {
    let unsatisfiable = |message: &str| ClientRangeError::Unsatisfiable {
        message: message.to_string(),
        total_size,
    };

    match plan {
        ClientRangePlan::Explicit { start, mut end } => {
            if start >= total_size {
                return Err(unsatisfiable("Range start beyond total size"));
            }
            if end >= total_size {
                end = total_size - 1;
            }
            Ok((start, end))
        }
        ClientRangePlan::OpenEnded { start } => {
            if start >= total_size {
                return Err(unsatisfiable("Range start beyond total size"));
            }
            Ok((start, total_size - 1))
        }
        ClientRangePlan::Suffix { suffix_len } => {
            if suffix_len == 0 {
                return Err(unsatisfiable("Suffix range out of bounds"));
            }
            Ok((total_size.saturating_sub(suffix_len), total_size - 1))
        }
        ClientRangePlan::MultiRange => Err(ClientRangeError::InvalidRequest(
            "Multi-range requests are not supported by the slice cache".to_string(),
        )),
    }
}

#[must_use]
pub fn slice_index_for_byte(byte: u64, slice_size: usize) -> u64 {
    byte / slice_size as u64
}

// Request Range header parsing

/// Parse a single HTTP Range header value.
///
/// Only supports a single byte range (multi-range is rejected, following
/// nginx's pattern of passing multi-range through without slicing).
/// Returns `(start, end)` where both are inclusive.
pub fn parse_range_header(range: &str, total_size: u64) -> Result<(u64, u64), anyhow::Error> {
    let range = range.trim();
    if !range.starts_with("bytes=") {
        return Err(anyhow::anyhow!(
            "Invalid range format: must start with 'bytes='"
        ));
    }

    let spec = &range["bytes=".len()..];

    // Reject multi-range (nginx: comma in Range -> passthrough).
    if spec.contains(',') {
        return Err(anyhow::anyhow!("Multi-range requests are not supported"));
    }

    let parts: Vec<&str> = spec.splitn(2, '-').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!("Invalid range format"));
    }

    let (start, end) = if parts[0].is_empty() {
        // Suffix range: bytes=-N (last N bytes)
        let suffix_len: u64 = parts[1]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid suffix range"))?;
        if suffix_len == 0 {
            return Err(anyhow::anyhow!("Suffix range out of bounds"));
        }
        (total_size.saturating_sub(suffix_len), total_size - 1)
    } else if parts[1].is_empty() {
        // Open-ended: bytes=N-
        let start: u64 = parts[0]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid range start"))?;
        if start >= total_size {
            return Err(anyhow::anyhow!("Range start beyond total size"));
        }
        (start, total_size - 1)
    } else {
        // Explicit range: bytes=N-M
        let start: u64 = parts[0]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid range start"))?;
        let mut end: u64 = parts[1]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid range end"))?;
        if start >= total_size {
            return Err(anyhow::anyhow!("Range start beyond total size"));
        }
        if start > end {
            return Err(anyhow::anyhow!("Range start beyond range end"));
        }
        if end >= total_size {
            end = total_size - 1;
        }
        (start, end)
    };

    Ok((start, end))
}

// Response Content-Range parsing (modeled after nginx)

/// Parsed Content-Range response header: `bytes START-END/TOTAL`
///
/// Modeled after nginx's `ngx_http_slice_content_range_t`.
/// Note: `end` is exclusive (nginx does `end++` after parsing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    /// Exclusive end (one past the last byte), matching nginx's convention.
    pub end: u64,
    /// Total resource length, or `None` when the server responds with `*`.
    pub complete_length: Option<u64>,
}

/// Parse a Content-Range response header value.
///
/// Accepts: `bytes START-END/TOTAL` or `bytes START-END/*`
/// Rejects: missing prefix, non-numeric values, overflow, trailing garbage.
///
/// Based on nginx's `ngx_http_slice_parse_content_range()`.
pub fn parse_content_range(value: &str) -> Result<ContentRange, anyhow::Error> {
    let value = value.trim();

    // Must start with "bytes " (case-sensitive, matching nginx).
    if !value.starts_with("bytes ") {
        return Err(anyhow::anyhow!("Content-Range must start with 'bytes '"));
    }

    let rest = value["bytes ".len()..].trim_start();

    // Parse start.
    let (start, rest) = parse_u64_prefix(rest)
        .ok_or_else(|| anyhow::anyhow!("Invalid or missing start in Content-Range"))?;

    // Expect '-'.
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix('-')
        .ok_or_else(|| anyhow::anyhow!("Missing '-' separator in Content-Range"))?;
    let rest = rest.trim_start();

    // Parse end (inclusive in the header, we convert to exclusive like nginx).
    let (end_inclusive, rest) = parse_u64_prefix(rest)
        .ok_or_else(|| anyhow::anyhow!("Invalid or missing end in Content-Range"))?;

    // Validate start <= end_inclusive (e.g., "bytes 500-100/1000" is invalid).
    if start > end_inclusive {
        return Err(anyhow::anyhow!(
            "Invalid Content-Range: start ({start}) > end ({end_inclusive})"
        ));
    }

    let end = end_inclusive
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("Content-Range end overflow"))?;

    // Expect '/'.
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix('/')
        .ok_or_else(|| anyhow::anyhow!("Missing '/' separator in Content-Range"))?;
    let rest = rest.trim_start();

    // Parse complete_length or '*'.
    let (complete_length, rest) = if let Some(after_star) = rest.strip_prefix('*') {
        (None, after_star)
    } else {
        let (len, r) = parse_u64_prefix(rest).ok_or_else(|| {
            anyhow::anyhow!("Invalid or missing complete length in Content-Range")
        })?;
        (Some(len), r)
    };

    // Reject trailing garbage (nginx checks `*p != '\0'`).
    let rest = rest.trim();
    if !rest.is_empty() {
        return Err(anyhow::anyhow!(
            "Trailing characters in Content-Range: '{rest}'"
        ));
    }

    Ok(ContentRange {
        start,
        end,
        complete_length,
    })
}

/// Parse a decimal u64 from the beginning of `s`, returning the parsed value
/// and the remaining string.  Returns `None` if `s` does not start with a
/// digit or if the number overflows u64.
///
/// Modeled after nginx's digit-by-digit parsing with overflow protection
/// (`cutoff`/`cutlim` pattern).
fn parse_u64_prefix(s: &str) -> Option<(u64, &str)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() {
        return None;
    }

    let mut value: u64 = 0;
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        let digit = u64::from(bytes[i] - b'0');
        value = value.checked_mul(10)?.checked_add(digit)?;
        i += 1;
    }

    Some((value, &s[i..]))
}

// Slice alignment helpers

/// Compute which slice indices are needed to serve the given byte range.
#[must_use]
pub fn compute_needed_slices(range_start: u64, range_end: u64, slice_size: usize) -> Vec<u64> {
    let ss = slice_size as u64;
    let first = range_start / ss;
    let last = range_end / ss;
    (first..=last).collect()
}

/// Compute the aligned byte range `(start, end)` for a given slice index.
/// Both `start` and `end` are inclusive.
///
/// Returns an error if `total_size` is zero, since there are no valid byte
/// ranges for an empty resource.
pub fn aligned_range_for_slice(
    slice_index: u64,
    slice_size: usize,
    total_size: u64,
) -> Result<(u64, u64), anyhow::Error> {
    if total_size == 0 {
        return Err(anyhow::anyhow!(
            "Cannot compute slice range for zero-size resource"
        ));
    }
    let ss = slice_size as u64;
    let start = slice_index * ss;
    let end = std::cmp::min(start + ss, total_size) - 1;
    Ok((start, end))
}

// Unit tests

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
