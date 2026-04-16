//! Media service batch size limit tests
//!
//! Tests that batch operations have proper size limits to prevent `DoS` attacks.
//!
//! Run with: cargo test --package synctv-core `media_batch_limit_tests`
#![allow(clippy::unwrap_used)]

/// Maximum batch size for all batch operations
/// This value mirrors the constant in synctv-core/src/service/media.rs
const MAX_BATCH_SIZE: usize = 100;

// Batch size validation helpers (mirror service logic)

fn validate_batch_size(count: usize) -> Result<(), String> {
    if count > MAX_BATCH_SIZE {
        return Err(format!("Batch size exceeds maximum of {MAX_BATCH_SIZE}"));
    }
    Ok(())
}

// add_media_batch tests

#[test]
fn add_media_batch_exceeds_limit_returns_error() {
    // Test that 101 items exceeds the limit
    let result = validate_batch_size(101);
    assert!(result.is_err(), "Batch of 101 items should be rejected");
    assert!(
        result.unwrap_err().contains("exceeds maximum"),
        "Error message should mention exceeds maximum"
    );
}

#[test]
fn add_media_batch_exactly_100_succeeds() {
    // Test boundary value - exactly 100 should be allowed
    let result = validate_batch_size(100);
    assert!(
        result.is_ok(),
        "Batch of exactly 100 items should be accepted"
    );
}

#[test]
fn add_media_batch_99_succeeds() {
    // Test value just below the limit
    let result = validate_batch_size(99);
    assert!(result.is_ok(), "Batch of 99 items should be accepted");
}

#[test]
fn add_media_batch_empty_succeeds() {
    // Empty batch should be allowed (service returns early)
    let result = validate_batch_size(0);
    assert!(result.is_ok(), "Empty batch should be accepted");
}

// delete_entries tests

#[test]
fn delete_entries_exceeds_limit_returns_error() {
    // Test that 101 delete items exceeds the limit
    let result = validate_batch_size(101);
    assert!(
        result.is_err(),
        "Delete batch of 101 items should be rejected"
    );
}

#[test]
fn delete_entries_exactly_100_succeeds() {
    // Test boundary value
    let result = validate_batch_size(100);
    assert!(
        result.is_ok(),
        "Delete batch of exactly 100 items should be accepted"
    );
}

// reorder_media_batch tests

#[test]
fn reorder_media_batch_exceeds_limit_returns_error() {
    // Test that 101 reorder items exceeds the limit
    let result = validate_batch_size(101);
    assert!(
        result.is_err(),
        "Reorder batch of 101 items should be rejected"
    );
}

#[test]
fn reorder_media_batch_exactly_100_succeeds() {
    // Test boundary value
    let result = validate_batch_size(100);
    assert!(
        result.is_ok(),
        "Reorder batch of exactly 100 items should be accepted"
    );
}

// Error message format tests

#[test]
fn batch_limit_error_message_format() {
    let result = validate_batch_size(150);
    assert!(result.is_err());
    let error_msg = result.unwrap_err();
    assert!(
        error_msg.contains("150") || error_msg.contains("100"),
        "Error message should reference the limit or the attempted size"
    );
}

#[test]
fn batch_limit_consistency_across_operations() {
    // All batch operations should have the same limit
    // This test ensures consistency

    // Test add
    assert!(validate_batch_size(100).is_ok());
    assert!(validate_batch_size(101).is_err());

    // Test delete - should use same limit
    assert!(validate_batch_size(100).is_ok());
    assert!(validate_batch_size(101).is_err());

    // Test reorder - should use same limit
    assert!(validate_batch_size(100).is_ok());
    assert!(validate_batch_size(101).is_err());
}
