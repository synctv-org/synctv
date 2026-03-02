//! Admin Force Logout Token Blacklist Validation Tests (TDD)
//!
//! Tests that admin-initiated force logout properly blacklists the user's tokens
//! so they cannot be used even if still valid by signature/expiration.
//!
//! Security Issue: If admin force logout doesn't invalidate existing tokens,
//! a banned/deleted user could continue using their valid JWT until it expires.
//!
//! Fix: Force logout must add the user's token JTI to the blacklist, and the
//! SecurityPipeline must reject blacklisted JTIs even if the token is otherwise valid.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::models::UserId;
use synctv_core::service::auth::token_blacklist::{InMemoryTokenBlacklistStore, TokenBlacklistStore};

// ============================================================================
// Helper functions
// ============================================================================

/// Create a test blacklist store
fn create_blacklist_store() -> Arc<InMemoryTokenBlacklistStore> {
    Arc::new(InMemoryTokenBlacklistStore::new(10000, 3600, 3600))
}

/// Generate a test JTI (JWT ID) for testing
fn generate_test_jti() -> String {
    format!("jti_{}", uuid::Uuid::new_v4())
}

// ============================================================================
// TDD Test 1: Token blacklist prevents replay of force-logged-out token
// ============================================================================

#[tokio::test]
async fn test_force_logout_blacklist_prevents_token_reuse() {
    let store = create_blacklist_store();
    let _user_id = UserId::new();
    let jti = generate_test_jti();

    // Simulate admin force logout: add token JTI to blacklist
    let ttl_secs = 3600; // 1 hour
    store.blacklist(&jti, ttl_secs).await.expect("Blacklist should succeed");

    // Verify the token is now blacklisted
    let is_blacklisted = store.is_blacklisted(&jti).await;
    assert!(
        is_blacklisted,
        "Token JTI should be blacklisted after force logout"
    );
}

// ============================================================================
// TDD Test 2: Blacklisted token is rejected even if otherwise valid
// ============================================================================

#[tokio::test]
async fn test_blacklisted_token_rejected_even_if_valid_signature() {
    let store = create_blacklist_store();
    let jti = generate_test_jti();

    // Add JTI to blacklist (simulating force logout)
    store.blacklist(&jti, 3600).await.expect("Blacklist should succeed");

    // The SecurityPipeline should check is_blacklisted_checked before accepting token
    let result = store.is_blacklisted_checked(&jti).await;
    assert!(
        result.is_ok(),
        "is_blacklisted_checked should not error"
    );
    assert!(
        result.unwrap(),
        "Blacklisted token should return true from is_blacklisted_checked"
    );
}

// ============================================================================
// TDD Test 3: Force logout blacklists all user's tokens by family
// ============================================================================

#[tokio::test]
async fn test_force_logout_revokes_token_family() {
    let store = create_blacklist_store();
    let user_id = UserId::new();
    let family_key = format!("family:{}", user_id.as_str());

    // Simulate admin force logout: revoke entire token family
    let revoked_at = chrono::Utc::now().timestamp();
    let ttl_secs = 3600;
    store.set_family_revoked(&family_key, revoked_at, ttl_secs).await;

    // Verify the family is revoked
    let family_revoked_at = store.get_family_revoked_at(&family_key).await;
    assert!(
        family_revoked_at.is_some(),
        "Token family should be revoked after force logout"
    );
    assert!(
        family_revoked_at.unwrap() <= chrono::Utc::now().timestamp(),
        "Revocation timestamp should be in the past or now"
    );
}

// ============================================================================
// TDD Test 4: Non-blacklisted token passes check
// ============================================================================

#[tokio::test]
async fn test_non_blacklisted_token_passes_check() {
    let store = create_blacklist_store();
    let jti = generate_test_jti();

    // Token NOT in blacklist should pass
    let is_blacklisted = store.is_blacklisted(&jti).await;
    assert!(
        !is_blacklisted,
        "Non-blacklisted token should not be flagged"
    );

    let result = store.is_blacklisted_checked(&jti).await;
    assert!(result.is_ok(), "is_blacklisted_checked should not error");
    assert!(
        !result.unwrap(),
        "Non-blacklisted token should return false"
    );
}

// ============================================================================
// TDD Test 5: Blacklist entry expires after TTL
// ============================================================================

#[tokio::test]
async fn test_blacklist_entry_expires_after_ttl() {
    let store = create_blacklist_store();
    let jti = generate_test_jti();

    // Add to blacklist with very short TTL
    store.blacklist(&jti, 1).await.expect("Blacklist should succeed");

    // Immediately should be blacklisted
    assert!(store.is_blacklisted(&jti).await, "Should be blacklisted immediately");

    // Wait for TTL to expire
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    // After TTL, should NOT be blacklisted
    let is_blacklisted = store.is_blacklisted(&jti).await;
    assert!(
        !is_blacklisted,
        "Blacklist entry should expire after TTL"
    );
}

// ============================================================================
// TDD Test 6: Concurrent blacklist operations are atomic
// ============================================================================

#[tokio::test]
async fn test_concurrent_blacklist_operations_atomic() {
    let store = create_blacklist_store();
    let jti = generate_test_jti();

    // Spawn multiple concurrent blacklist_if_not_exists operations
    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = store.clone();
        let j = jti.clone();
        handles.push(tokio::spawn(async move {
            s.blacklist_if_not_exists(&j, 3600).await
        }));
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // Exactly one should return Ok(false) (first insert)
    // All others should return Ok(true) (already existed)
    let (first_use_count, replay_count): (usize, usize) = results.iter().fold((0, 0), |(first, replay), r| {
        match r.as_ref().unwrap().as_ref() {
            Ok(false) => (first + 1, replay), // First use
            Ok(true) => (first, replay + 1),  // Replay detected
            Err(_) => (first, replay),         // Error (shouldn't happen)
        }
    });

    assert_eq!(
        first_use_count, 1,
        "Exactly one operation should succeed as first insert"
    );
    assert_eq!(
        replay_count, 9,
        "All other operations should detect replay"
    );
}

// ============================================================================
// TDD Test 7: Force logout for multiple tokens (batch blacklist)
// ============================================================================

#[tokio::test]
async fn test_batch_blacklist_for_force_logout() {
    let store = create_blacklist_store();

    // Simulate force logout for user with multiple active sessions
    let jti1 = generate_test_jti();
    let jti2 = generate_test_jti();
    let jti3 = generate_test_jti();

    // Blacklist all tokens
    store.blacklist(&jti1, 3600).await.expect("Should succeed");
    store.blacklist(&jti2, 3600).await.expect("Should succeed");
    store.blacklist(&jti3, 3600).await.expect("Should succeed");

    // All should be blacklisted
    assert!(store.is_blacklisted(&jti1).await, "JTI1 should be blacklisted");
    assert!(store.is_blacklisted(&jti2).await, "JTI2 should be blacklisted");
    assert!(store.is_blacklisted(&jti3).await, "JTI3 should be blacklisted");
}

// ============================================================================
// TDD Test 8: Token issued after family revocation is NOT blacklisted
// ============================================================================

#[tokio::test]
async fn test_token_issued_after_family_revocation_not_blacklisted() {
    let store = create_blacklist_store();
    let user_id = UserId::new();
    let family_key = format!("family:{}", user_id.as_str());

    // Revoke family at time T1
    let revoked_at = chrono::Utc::now().timestamp();
    store.set_family_revoked(&family_key, revoked_at, 3600).await;

    // Token issued after revocation should have iat > revoked_at
    // This is checked by SecurityPipeline, not the blacklist store
    // Here we just verify the revocation timestamp is accessible
    let family_revoked_at = store.get_family_revoked_at(&family_key).await;
    assert!(
        family_revoked_at.is_some(),
        "Family revocation timestamp should be available"
    );

    // A new token with iat > revoked_at would pass the pipeline check
    // (this logic is in SecurityPipeline, not tested here)
}

// ============================================================================
// TDD Test 9: Blacklist store backend name for observability
// ============================================================================

#[tokio::test]
async fn test_blacklist_store_can_be_created() {
    // Verify store can be created with expected parameters
    let store = InMemoryTokenBlacklistStore::new(
        5000,  // max_jti_capacity
        7200,  // jti_ttl_secs
        86400, // family_ttl_secs
    );

    // Basic sanity check - store should be functional
    let jti = generate_test_jti();
    store.blacklist(&jti, 3600).await.expect("Should succeed");
    assert!(store.is_blacklisted(&jti).await);
}

// ============================================================================
// TDD Test 10: Force logout simulation with JWT claims
// ============================================================================

#[tokio::test]
async fn test_force_logout_simulation_with_jwt_claims() {
    let store = create_blacklist_store();

    // Simulate a JWT's JTI claim
    let jti = format!("{:x}", uuid::Uuid::new_v4().to_u128_le());

    // 1. Initially token is NOT blacklisted
    assert!(
        !store.is_blacklisted(&jti).await,
        "Token should not be blacklisted initially"
    );

    // 2. Admin performs force logout - blacklist the token
    store.blacklist(&jti, 3600).await.expect("Should succeed");

    // 3. Now token IS blacklisted
    assert!(
        store.is_blacklisted(&jti).await,
        "Token should be blacklisted after force logout"
    );

    // 4. SecurityPipeline would check this and reject the token
    // (simulated here by direct check)
    let check_result = store.is_blacklisted_checked(&jti).await;
    assert!(
        check_result.is_ok() && check_result.unwrap(),
        "SecurityPipeline should reject blacklisted token"
    );
}
