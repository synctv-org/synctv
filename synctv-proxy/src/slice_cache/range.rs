//! Range header parsing, Content-Range response parsing, and slice alignment.
//!
//! Mirrors nginx's slice module approach:
//! - Request `Range` header parsing (single byte-range only, multi-range rejected)
//! - Response `Content-Range` parsing modeled after
//!   `ngx_http_slice_parse_content_range`
//! - Slice-aligned range computation

// ------------------------------------------------------------------
// Request Range header parsing
// ------------------------------------------------------------------

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
        if suffix_len == 0 || suffix_len > total_size {
            return Err(anyhow::anyhow!("Suffix range out of bounds"));
        }
        (total_size - suffix_len, total_size - 1)
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
        if end >= total_size {
            end = total_size - 1;
        }
        (start, end)
    };

    Ok((start, end))
}

// ------------------------------------------------------------------
// Response Content-Range parsing (modeled after nginx)
// ------------------------------------------------------------------

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
        return Err(anyhow::anyhow!(
            "Content-Range must start with 'bytes '"
        ));
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

// ------------------------------------------------------------------
// Slice alignment helpers
// ------------------------------------------------------------------

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
#[must_use]
pub fn aligned_range_for_slice(
    slice_index: u64,
    slice_size: usize,
    total_size: u64,
) -> (u64, u64) {
    let ss = slice_size as u64;
    let start = slice_index * ss;
    let end = std::cmp::min(start + ss, total_size) - 1;
    (start, end)
}

// ------------------------------------------------------------------
// Unit tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_content_range tests ---

    #[test]
    fn test_parse_content_range_basic() {
        let cr = parse_content_range("bytes 0-499/1000").unwrap();
        assert_eq!(cr.start, 0);
        assert_eq!(cr.end, 500); // exclusive
        assert_eq!(cr.complete_length, Some(1000));
    }

    #[test]
    fn test_parse_content_range_large_values() {
        let cr = parse_content_range("bytes 0-2097151/10485760").unwrap();
        assert_eq!(cr.start, 0);
        assert_eq!(cr.end, 2_097_152);
        assert_eq!(cr.complete_length, Some(10_485_760));
    }

    #[test]
    fn test_parse_content_range_wildcard_length() {
        let cr = parse_content_range("bytes 100-199/*").unwrap();
        assert_eq!(cr.start, 100);
        assert_eq!(cr.end, 200);
        assert_eq!(cr.complete_length, None);
    }

    #[test]
    fn test_parse_content_range_with_spaces() {
        // nginx tolerates spaces between tokens
        let cr = parse_content_range("bytes  0 - 499 / 1000").unwrap();
        assert_eq!(cr.start, 0);
        assert_eq!(cr.end, 500);
        assert_eq!(cr.complete_length, Some(1000));
    }

    #[test]
    fn test_parse_content_range_missing_prefix() {
        assert!(parse_content_range("0-499/1000").is_err());
    }

    #[test]
    fn test_parse_content_range_missing_dash() {
        assert!(parse_content_range("bytes 0 499/1000").is_err());
    }

    #[test]
    fn test_parse_content_range_missing_slash() {
        assert!(parse_content_range("bytes 0-499 1000").is_err());
    }

    #[test]
    fn test_parse_content_range_non_numeric_start() {
        assert!(parse_content_range("bytes abc-499/1000").is_err());
    }

    #[test]
    fn test_parse_content_range_non_numeric_end() {
        assert!(parse_content_range("bytes 0-xyz/1000").is_err());
    }

    #[test]
    fn test_parse_content_range_trailing_garbage() {
        assert!(parse_content_range("bytes 0-499/1000 extra").is_err());
    }

    #[test]
    fn test_parse_content_range_empty() {
        assert!(parse_content_range("").is_err());
    }

    #[test]
    fn test_parse_content_range_overflow() {
        // u64::MAX = 18446744073709551615, adding 1 for exclusive end overflows
        assert!(parse_content_range("bytes 0-18446744073709551615/999").is_err());
    }

    #[test]
    fn test_parse_content_range_u64_max_start() {
        // Start value at u64::MAX is fine as long as end doesn't overflow
        assert!(
            parse_content_range("bytes 18446744073709551615-18446744073709551615/999").is_err()
        );
    }
}
