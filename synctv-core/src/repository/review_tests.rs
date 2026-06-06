use super::*;

#[test]
fn count_value_rejects_missing_count_result() {
    let error = count_value(None, "review total").expect_err("missing COUNT must fail");

    assert!(matches!(
        error,
        Error::Internal(message) if message.contains("review total")
    ));
}

#[test]
fn count_value_accepts_count_result() {
    assert_eq!(count_value(Some(7), "review total").unwrap(), 7);
}
