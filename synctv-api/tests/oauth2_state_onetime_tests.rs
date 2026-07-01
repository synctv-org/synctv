//! OAuth2 State One-Time Use Validation Tests
//!
//! Tests that OAuth2 state parameters can only be used once to prevent replay attacks.
//!
//! State tokens must not be reusable; a captured state from browser history,
//! logs, or MITM could otherwise be replayed to authenticate.
//! State tokens must be consumed atomically (GET + DEL) so that:
//! 1. First use succeeds
//! 2. Second use fails with "Invalid or expired OAuth2 state"
//! 3. Expired states fail even if somehow still in storage
//!
//! These tests validate the InMemoryOAuthStateStore's atomic single-use semantics
//! which are used by OAuth2Service::verify_state() to prevent replay attacks.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::UserId;
use synctv_core::service::{
    local_oauth_state_store, OAuth2Operation, OAuth2State, OAuthStateStore,
};

// Helper functions

/// Create a test state store
fn create_state_store() -> Arc<dyn OAuthStateStore> {
    local_oauth_state_store()
}

/// Create a test OAuth2 state
fn create_test_state(instance_name: &str) -> OAuth2State {
    OAuth2State {
        instance_name: instance_name.to_string(),
        redirect_url: Some("/dashboard".to_string()),
        created_at: chrono::Utc::now(),
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "test_verifier_abc123".to_string(),
        nonce: None,
    }
}

#[tokio::test]
async fn test_oauth2_state_first_use_succeeds() {
    let state_store = create_state_store();
    let state = create_test_state("github");
    let state_token = "test_state_token_abc123";

    // Store the state
    state_store
        .store(state_token, &state, Duration::from_mins(5))
        .await
        .expect("Failed to store state");

    // First consume should succeed
    let result = state_store
        .consume(state_token)
        .await
        .expect("Consume should not error");

    assert!(
        result.is_some(),
        "First use of valid state should return Some(state)"
    );

    let retrieved = result.unwrap();
    assert_eq!(retrieved.instance_name, "github");
    assert_eq!(retrieved.pkce_verifier, "test_verifier_abc123");
}

#[tokio::test]
async fn test_oauth2_state_second_use_fails_prevents_replay() {
    let state_store = create_state_store();
    let state = create_test_state("github");
    let state_token = "test_state_token_replay";

    // Store the state
    state_store
        .store(state_token, &state, Duration::from_mins(5))
        .await
        .expect("Failed to store state");

    // First consume should succeed
    let first_result = state_store
        .consume(state_token)
        .await
        .expect("First consume should not error");

    assert!(first_result.is_some(), "First use should succeed");

    // Second consume should return None (state already consumed)
    let second_result = state_store
        .consume(state_token)
        .await
        .expect("Second consume should not error");

    assert!(
        second_result.is_none(),
        "Second use of same state should fail (return None) - this prevents replay attacks!"
    );
}

#[tokio::test]
async fn test_oauth2_state_expired_fails() {
    let state_store = create_state_store();

    let expired_time = chrono::Utc::now() - chrono::Duration::seconds(600);
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: None,
        created_at: expired_time,
        operation: OAuth2Operation::Login,
        target_user_id: None,
        pkce_verifier: "expired_verifier".to_string(),
        nonce: None,
    };
    let state_token = "test_state_token_expired";

    // Store with a very short TTL (1 nanosecond - will expire immediately)
    state_store
        .store(state_token, &state, Duration::from_nanos(1))
        .await
        .expect("Failed to store state");

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Consuming expired state should return None
    let result = state_store
        .consume(state_token)
        .await
        .expect("Consume should not error");

    assert!(result.is_none(), "Expired state should return None");
}

#[tokio::test]
async fn test_oauth2_state_unknown_token_fails() {
    let state_store = create_state_store();

    // Try to consume a state that was never stored
    let result = state_store
        .consume("nonexistent_token_xyz")
        .await
        .expect("Consume should not error");

    assert!(result.is_none(), "Unknown state token should return None");
}

#[tokio::test]
async fn test_oauth2_state_different_tokens_independent() {
    let state_store = create_state_store();

    let state1 = create_test_state("github");
    let state2 = OAuth2State {
        instance_name: "logto1".to_string(),
        redirect_url: Some("/profile".to_string()),
        created_at: chrono::Utc::now(),
        operation: OAuth2Operation::Bind,
        target_user_id: Some(UserId::new()),
        pkce_verifier: "verifier_logto".to_string(),
        nonce: None,
    };

    let token1 = "token_github_123";
    let token2 = "token_logto_456";

    // Store both states
    state_store
        .store(token1, &state1, Duration::from_mins(5))
        .await
        .expect("Failed to store state1");
    state_store
        .store(token2, &state2, Duration::from_mins(5))
        .await
        .expect("Failed to store state2");

    // Consume token1
    let result1 = state_store
        .consume(token1)
        .await
        .expect("Consume should not error");
    assert!(result1.is_some(), "Token1 should be found");
    assert_eq!(result1.unwrap().instance_name, "github");

    // Token2 should still be available
    let result2 = state_store
        .consume(token2)
        .await
        .expect("Consume should not error");
    assert!(result2.is_some(), "Token2 should still be available");
    assert_eq!(result2.unwrap().instance_name, "logto1");

    // Token1 should now be gone
    let result1_again = state_store
        .consume(token1)
        .await
        .expect("Consume should not error");
    assert!(result1_again.is_none(), "Token1 should be consumed");
}

#[tokio::test]
async fn test_oauth2_state_concurrent_only_one_succeeds() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let state_store = create_state_store();
    let state = create_test_state("concurrent_test");
    let state_token = "concurrent_token_xyz";

    // Store the state
    state_store
        .store(state_token, &state, Duration::from_mins(5))
        .await
        .expect("Failed to store state");

    let success_count = Arc::new(AtomicUsize::new(0));
    let none_count = Arc::new(AtomicUsize::new(0));

    // Spawn 10 concurrent consumers
    let mut handles = Vec::new();
    for _ in 0..10 {
        let ss = state_store.clone();
        let sc = success_count.clone();
        let nc = none_count.clone();
        let token = state_token.to_string();

        handles.push(tokio::spawn(async move {
            let result = ss.consume(&token).await.expect("Consume should not error");
            match result {
                Some(_) => sc.fetch_add(1, Ordering::SeqCst),
                None => nc.fetch_add(1, Ordering::SeqCst),
            }
        }));
    }

    for handle in handles {
        handle.await.expect("Task should not panic");
    }

    // Exactly one should have succeeded
    assert_eq!(
        success_count.load(Ordering::SeqCst),
        1,
        "Exactly one concurrent consume should succeed (atomic single-use)"
    );
    assert_eq!(
        none_count.load(Ordering::SeqCst),
        9,
        "9 concurrent consumes should return None"
    );
}

#[tokio::test]
async fn test_oauth2_state_with_target_user_id_consumed_correctly() {
    let state_store = create_state_store();

    let target_user_id = UserId::new();
    let state = OAuth2State {
        instance_name: "github".to_string(),
        redirect_url: Some("/settings".to_string()),
        created_at: chrono::Utc::now(),
        operation: OAuth2Operation::Bind,
        target_user_id: Some(target_user_id),
        pkce_verifier: "bind_verifier".to_string(),
        nonce: None,
    };
    let state_token = "bind_state_token";

    // Store the state
    state_store
        .store(state_token, &state, Duration::from_mins(5))
        .await
        .expect("Failed to store state");

    // First consume
    let result = state_store
        .consume(state_token)
        .await
        .expect("Consume should succeed");

    assert!(result.is_some());
    let retrieved = result.unwrap();
    assert_eq!(retrieved.instance_name, "github");
    assert_eq!(retrieved.target_user_id, Some(target_user_id));

    // Second use should fail
    let second = state_store
        .consume(state_token)
        .await
        .expect("Consume should not error");
    assert!(second.is_none(), "Second use of bind state should fail");
}

#[tokio::test]
async fn test_in_memory_state_store_reports_single_node_scope() {
    let state_store = create_state_store();
    assert!(
        !state_store.supports_cross_node_single_use(),
        "InMemoryOAuthStateStore should remain single-node scoped"
    );
}

#[tokio::test]
async fn test_state_store_sweeps_expired_entries_on_store() {
    let state_store = create_state_store();

    // Store a state with a very short TTL
    let state = create_test_state("shortlived");
    state_store
        .store("short_token", &state, Duration::from_nanos(1))
        .await
        .expect("Failed to store state");

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Store another state - this should trigger sweep of expired entries
    let state2 = create_test_state("longlived");
    state_store
        .store("long_token", &state2, Duration::from_mins(5))
        .await
        .expect("Failed to store state2");

    // The short-lived token should have been swept
    let result = state_store
        .consume("short_token")
        .await
        .expect("Consume should not error");
    assert!(
        result.is_none(),
        "Short-lived token should be gone after sweep"
    );

    // The long-lived token should still be there
    let result = state_store
        .consume("long_token")
        .await
        .expect("Consume should not error");
    assert!(
        result.is_some(),
        "Long-lived token should still be available"
    );
}

#[tokio::test]
async fn test_oauth2_replay_attack_prevented() {
    let state_store = create_state_store();

    // Simulate the OAuth2 flow:
    let state = create_test_state("github");
    let state_token = "user_oauth_state_12345";

    state_store
        .store(state_token, &state, Duration::from_mins(5))
        .await
        .expect("Failed to store state");

    let first_validation = state_store
        .consume(state_token)
        .await
        .expect("Consume should not error");
    assert!(
        first_validation.is_some(),
        "Legitimate OAuth2 callback should succeed"
    );

    //    and tries to replay it for another login attempt
    let replay_attempt = state_store
        .consume(state_token)
        .await
        .expect("Consume should not error");

    assert!(
        replay_attempt.is_none(),
        "REPLAY ATTACK PREVENTED: Attacker should not be able to reuse the state token!"
    );
}
