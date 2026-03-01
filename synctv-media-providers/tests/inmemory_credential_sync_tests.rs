//! InMemoryCredentialStorage Multi-Replica Synchronization Tests
//!
//! These tests verify and document the behavior of InMemoryCredentialStorage
//! in multi-replica deployment scenarios.
//!
//! ## Key Finding
//!
//! InMemoryCredentialStorage uses a local HashMap, meaning each replica has
//! independent memory. In multi-replica deployments, credential state will
//! NOT be synchronized across replicas.
//!
//! ## Recommended Usage
//!
//! - **Testing**: Ideal for unit tests and integration tests
//! - **Single-replica deployments**: Works correctly when only one instance exists
//! - **Development**: Convenient for local development
//!
//! ## Production Recommendation
//!
//! For production deployments with multiple replicas, use `PostgresCredentialStorage`
//! which provides persistent and synchronized credential storage across all replicas.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use std::sync::Arc;

use synctv_media_providers::{
    CredentialData, CredentialStorage, InMemoryCredentialStorage, ProviderType,
};

// ========== Single Instance Tests (Expected Behavior) ==========

/// Verifies that a single InMemoryCredentialStorage instance works correctly
/// for basic CRUD operations.
#[tokio::test]
async fn test_single_instance_basic_crud_works() {
    let storage = InMemoryCredentialStorage::new();

    // Create
    let cred = storage
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();
    assert_eq!(cred.user_id, "user1");

    // Read
    let found = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(found.is_some());

    // Delete
    let deleted = storage
        .delete("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(deleted);

    // Verify deleted
    let not_found = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(not_found.is_none());
}

/// Verifies that concurrent operations within a single instance work correctly.
#[tokio::test]
async fn test_single_instance_concurrent_access_works() {
    let storage = Arc::new(InMemoryCredentialStorage::new());
    let mut handles = vec![];

    // Spawn 10 concurrent write operations
    for i in 0..10 {
        let s = storage.clone();
        handles.push(tokio::spawn(async move {
            let user_id = format!("user{i}");
            s.set(&user_id, None, CredentialData::bilibili(HashMap::new()))
                .await
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // All 10 credentials should be stored
    for i in 0..10 {
        let user_id = format!("user{i}");
        let found = storage
            .get(&user_id, ProviderType::Bilibili, "bilibili")
            .await
            .unwrap();
        assert!(found.is_some(), "Expected credential for {user_id}");
    }
}

// ========== Multi-Instance Tests (Documented Limitation) ==========

/// Demonstrates that multiple InMemoryCredentialStorage instances DO NOT
/// share state. This is the core limitation in multi-replica deployments.
///
/// In a real multi-replica deployment:
/// - Replica A stores a credential
/// - Replica B cannot access that credential
/// - This leads to inconsistent user experience
#[tokio::test]
async fn test_multiple_instances_do_not_share_state() {
    // Simulate two replicas with independent memory
    let replica_a = InMemoryCredentialStorage::new();
    let replica_b = InMemoryCredentialStorage::new();

    // Store credential on replica A
    let cred_on_a = replica_a
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();
    assert_eq!(cred_on_a.user_id, "user1");

    // Verify it's accessible on replica A
    let found_on_a = replica_a
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(
        found_on_a.is_some(),
        "Credential should be accessible on replica A"
    );

    // Verify it's NOT accessible on replica B (this is the limitation)
    let found_on_b = replica_b
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(
        found_on_b.is_none(),
        "Credential should NOT be accessible on replica B - this demonstrates \
         the multi-replica synchronization limitation"
    );
}

/// Demonstrates that updates on one instance are not visible on another.
#[tokio::test]
async fn test_updates_not_synchronized_across_instances() {
    let replica_a = InMemoryCredentialStorage::new();
    let replica_b = InMemoryCredentialStorage::new();

    // Both replicas start empty
    assert!(replica_a.list_by_user("user1").await.unwrap().is_empty());
    assert!(replica_b.list_by_user("user1").await.unwrap().is_empty());

    // Store on replica A
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "session_v1".to_string());
    replica_a
        .set("user1", None, CredentialData::bilibili(cookies))
        .await
        .unwrap();

    // Replica A has the credential
    let creds_a = replica_a.list_by_user("user1").await.unwrap();
    assert_eq!(creds_a.len(), 1);

    // Replica B still has nothing
    let creds_b = replica_b.list_by_user("user1").await.unwrap();
    assert!(
        creds_b.is_empty(),
        "Replica B should not see credentials stored on replica A"
    );

    // Update on replica A
    let mut new_cookies = HashMap::new();
    new_cookies.insert("SESSDATA".to_string(), "session_v2".to_string());
    replica_a
        .set("user1", None, CredentialData::bilibili(new_cookies))
        .await
        .unwrap();

    // Replica A has the update
    let cred_a = replica_a
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap()
        .unwrap();
    let cookies_a = cred_a.data.as_bilibili().unwrap();
    assert_eq!(
        cookies_a.get("SESSDATA"),
        Some(&"session_v2".to_string()),
        "Replica A should have the updated credential"
    );

    // Replica B still has nothing
    let cred_b = replica_b
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(
        cred_b.is_none(),
        "Replica B should still not have any credential"
    );
}

/// Demonstrates that deletions are also not synchronized.
#[tokio::test]
async fn test_deletions_not_synchronized_across_instances() {
    // Scenario: User stores credential on replica A, load balancer
    // routes delete request to replica B - delete appears to succeed
    // but credential still exists on replica A

    let replica_a = InMemoryCredentialStorage::new();
    let replica_b = InMemoryCredentialStorage::new();

    // Store on both replicas (simulating initial synchronized state)
    replica_a
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();
    replica_b
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    // Both have the credential
    assert!(replica_a
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap()
        .is_some());
    assert!(replica_b
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap()
        .is_some());

    // Delete on replica B only
    let deleted = replica_b
        .delete("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(deleted, "Delete on replica B should succeed");

    // Replica B no longer has it
    assert!(replica_b
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap()
        .is_none());

    // But replica A still has it! (inconsistent state)
    assert!(
        replica_a
            .get("user1", ProviderType::Bilibili, "bilibili")
            .await
            .unwrap()
            .is_some(),
        "Replica A still has the credential - this is the inconsistent state problem"
    );
}

/// Demonstrates that Arc wrapping only shares within a single process.
/// Even with Arc, separate processes (replicas) have separate memory.
#[tokio::test]
async fn test_arc_only_shares_within_same_process() {
    // Within the same process, Arc shares the storage
    let storage = Arc::new(InMemoryCredentialStorage::new());
    let storage_clone = storage.clone();

    // Store using the original
    storage
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    // Access using the clone - this works because they share the same HashMap
    let found = storage_clone
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(
        found.is_some(),
        "Arc clone within the same process shares state"
    );

    // But this is NOT the same as multi-replica deployment!
    // In Kubernetes/Docker, each replica is a separate process with separate memory.
    // Arc cannot share memory across process boundaries.
}

// ========== Documentation/Contract Tests ==========

/// This test documents the expected use case for InMemoryCredentialStorage.
/// It should ONLY be used for testing and single-replica deployments.
#[tokio::test]
async fn test_documented_use_case_testing() {
    // Use case 1: Unit testing - quick setup without external dependencies
    let storage = InMemoryCredentialStorage::new();

    // Easy to create, no database connection needed
    storage
        .set("test_user", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    // Works perfectly for isolated tests
    assert!(storage
        .exists("test_user", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap());
}

/// Documents that InMemoryCredentialStorage supports encryption
/// which is important for testing encrypted credential flows.
#[tokio::test]
async fn test_documented_feature_encryption_support() {
    let key = vec![0u8; 32]; // Test encryption key
    let storage = InMemoryCredentialStorage::with_encryption(&key);

    // Store with encryption
    let cred = storage
        .set(
            "user1",
            Some("my_alist"),
            CredentialData::alist(
                "https://alist.example.com".to_string(),
                "admin".to_string(),
                "secret_password".to_string(),
            ),
        )
        .await
        .unwrap();

    // Retrieved credential has decrypted password
    let (_, _, password) = cred.data.as_alist().unwrap();
    assert_eq!(password, "secret_password");
}

/// Documents the default() behavior.
#[tokio::test]
async fn test_documented_default_creates_unencrypted_storage() {
    let storage = InMemoryCredentialStorage::default();

    // Default creates unencrypted storage
    storage
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    let found = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(found.is_some());
}
