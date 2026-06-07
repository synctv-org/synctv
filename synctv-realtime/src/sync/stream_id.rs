/// Parse a Redis Stream ID (`"{timestamp_ms}-{seq}"`) into numeric parts.
pub(crate) fn parse_stream_id(id: &str) -> Option<(u64, u64)> {
    let (ts_str, seq_str) = id.split_once('-')?;
    let ts = ts_str.parse::<u64>().ok()?;
    let seq = seq_str.parse::<u64>().ok()?;
    Some((ts, seq))
}

/// Compare Redis Stream IDs using numeric timestamp and sequence fields.
pub(crate) fn stream_id_gt(a: &str, b: &str) -> bool {
    match (parse_stream_id(a), parse_stream_id(b)) {
        (Some(a_parsed), Some(b_parsed)) => a_parsed > b_parsed,
        _ => a > b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_stream_ids() {
        assert_eq!(parse_stream_id("10-2"), Some((10, 2)));
        assert_eq!(parse_stream_id("$"), None);
        assert_eq!(parse_stream_id("0"), None);
    }

    #[test]
    fn compares_numeric_fields() {
        assert!(stream_id_gt("10-0", "9-99"));
        assert!(stream_id_gt("10-2", "10-1"));
        assert!(!stream_id_gt("9-99", "10-0"));
    }
}
