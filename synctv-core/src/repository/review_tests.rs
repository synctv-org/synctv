use super::required_count;
use crate::test_helpers::{err, ok};
use crate::Error;

#[test]
fn required_count_rejects_missing_count_result() {
    let error = err(
        required_count(None, "review total"),
        "missing COUNT must fail",
    );

    assert!(matches!(
        error,
        Error::Internal(message) if message.contains("review total")
    ));
}

#[test]
fn required_count_accepts_count_result() {
    assert_eq!(
        ok(required_count(Some(7), "review total"), "review total"),
        7
    );
}
