use super::*;
use crate::test_helpers::{err, ok};

#[test]
fn count_value_rejects_missing_count_result() {
    let error = err(count_value(None, "review total"), "missing COUNT must fail");

    assert!(matches!(
        error,
        Error::Internal(message) if message.contains("review total")
    ));
}

#[test]
fn count_value_accepts_count_result() {
    assert_eq!(ok(count_value(Some(7), "review total"), "review total"), 7);
}
