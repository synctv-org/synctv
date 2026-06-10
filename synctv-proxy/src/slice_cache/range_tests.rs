use super::*;

type ClientRangeTestResult = Result<(), ClientRangeError>;
type ContentRangeTestResult = Result<(), anyhow::Error>;

#[test]
fn parse_client_range_plan_supports_single_range_forms() -> ClientRangeTestResult {
    assert_eq!(
        parse_client_range_plan("bytes=10-20")?,
        ClientRangePlan::Explicit { start: 10, end: 20 }
    );
    assert_eq!(
        parse_client_range_plan("bytes=10-")?,
        ClientRangePlan::OpenEnded { start: 10 }
    );
    assert_eq!(
        parse_client_range_plan("bytes=-50")?,
        ClientRangePlan::Suffix { suffix_len: 50 }
    );
    Ok(())
}

#[test]
fn parse_client_range_plan_detects_multi_range_passthrough() -> ClientRangeTestResult {
    assert_eq!(
        parse_client_range_plan("bytes=0-10,20-30")?,
        ClientRangePlan::MultiRange
    );
    Ok(())
}

#[test]
fn parse_client_range_plan_rejects_invalid_ranges() {
    assert!(matches!(
        parse_client_range_plan("items=0-10"),
        Err(ClientRangeError::InvalidRequest(_))
    ));
    assert!(matches!(
        parse_client_range_plan("bytes=20-10"),
        Err(ClientRangeError::InvalidRequest(_))
    ));
    assert!(matches!(
        parse_client_range_plan("bytes=-0"),
        Err(ClientRangeError::InvalidRequest(_))
    ));
}

#[test]
fn range_bounds_for_total_clamps_and_resolves_forms() -> ClientRangeTestResult {
    assert_eq!(
        range_bounds_for_total(
            ClientRangePlan::Explicit {
                start: 10,
                end: 999
            },
            100
        )?,
        (10, 99)
    );
    assert_eq!(
        range_bounds_for_total(ClientRangePlan::OpenEnded { start: 90 }, 100)?,
        (90, 99)
    );
    assert_eq!(
        range_bounds_for_total(ClientRangePlan::Suffix { suffix_len: 20 }, 100)?,
        (80, 99)
    );
    assert_eq!(
        range_bounds_for_total(ClientRangePlan::Suffix { suffix_len: 200 }, 100)?,
        (0, 99)
    );
    Ok(())
}

#[test]
fn range_bounds_for_total_reports_unsatisfiable_ranges() {
    assert!(matches!(
        range_bounds_for_total(
            ClientRangePlan::Explicit {
                start: 100,
                end: 120
            },
            100
        ),
        Err(ClientRangeError::Unsatisfiable {
            total_size: 100,
            ..
        })
    ));
}

#[test]
fn slice_index_for_byte_uses_zero_based_slice_numbers() {
    assert_eq!(slice_index_for_byte(0, 100), 0);
    assert_eq!(slice_index_for_byte(99, 100), 0);
    assert_eq!(slice_index_for_byte(100, 100), 1);
}

#[test]
fn test_parse_content_range_basic() -> ContentRangeTestResult {
    let cr = parse_content_range("bytes 0-499/1000")?;
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 500);
    assert_eq!(cr.complete_length, Some(1000));
    Ok(())
}

#[test]
fn test_parse_content_range_large_values() -> ContentRangeTestResult {
    let cr = parse_content_range("bytes 0-2097151/10485760")?;
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 2_097_152);
    assert_eq!(cr.complete_length, Some(10_485_760));
    Ok(())
}

#[test]
fn test_parse_content_range_wildcard_length() -> ContentRangeTestResult {
    let cr = parse_content_range("bytes 100-199/*")?;
    assert_eq!(cr.start, 100);
    assert_eq!(cr.end, 200);
    assert_eq!(cr.complete_length, None);
    Ok(())
}

#[test]
fn test_parse_content_range_with_spaces() -> ContentRangeTestResult {
    let cr = parse_content_range("bytes  0 - 499 / 1000")?;
    assert_eq!(cr.start, 0);
    assert_eq!(cr.end, 500);
    assert_eq!(cr.complete_length, Some(1000));
    Ok(())
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
    assert!(parse_content_range("bytes 0-18446744073709551615/999").is_err());
}

#[test]
fn test_parse_content_range_u64_max_start() {
    assert!(parse_content_range("bytes 18446744073709551615-18446744073709551615/999").is_err());
}

#[test]
fn test_parse_content_range_start_greater_than_end() {
    assert!(parse_content_range("bytes 500-100/1000").is_err());
}

#[test]
fn test_parse_content_range_start_equals_end_is_valid() -> ContentRangeTestResult {
    let cr = parse_content_range("bytes 100-100/1000")?;
    assert_eq!(cr.start, 100);
    assert_eq!(cr.end, 101);
    assert_eq!(cr.complete_length, Some(1000));
    Ok(())
}
