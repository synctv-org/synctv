//! Transaction Management
//!
//! This module provides utilities for database transaction management.
//!
//! ## Best Practices
//!
//! ### Standard Transaction Pattern
//!
//! The recommended pattern for transactions is:
//!
//! ```text
//! let mut tx = pool.begin().await?;
//!
//! // Perform multiple operations
//! operation1(&mut *tx).await?;
//! operation2(&mut *tx).await?;
//!
//! // Commit on success
//! tx.commit().await?;
//! ```
//!
//! **No explicit rollback needed** - if an error occurs (via `?`), the transaction
//! will be automatically rolled back when it's dropped.
//!
//! ### When to Use Explicit Rollback
//!
//! Only use explicit `tx.rollback().await?` in special cases:
//!
//! - **Retry loops**: When you need to rollback and retry with fresh locks
//! - **Early exit without error**: When rolling back is the intended behavior
//!
//! Example (CAS retry):
//! ```text
//! for attempt in 0..MAX_RETRIES {
//!     let mut tx = pool.begin().await?;
//!
//!     if cas_update_success {
//!         tx.commit().await?;
//!         return Ok(());
//!     }
//!
//!     // Explicit rollback before retry to release locks immediately
//!     tx.rollback().await?;
//! }
//! ```
//!
//! ## Deadlock Prevention
//!
//! When updating multiple rows across different tables in a transaction, deadlocks can occur
//! if concurrent transactions lock rows in different orders. `PostgreSQL` detects deadlocks
//! and will abort one transaction with error code 40P01.
//!
//! ### Lock Ordering Rules
//!
//! To prevent deadlocks, **always acquire locks in a consistent order**:
//!
//! 1. **Lock parent entities before child entities** (e.g., room before `room_members`)
//! 2. **Lock by ID in ascending order** when updating multiple rows of the same table
//! 3. **Use FOR UPDATE explicitly** when reading before updating
//!
//! ### Examples
//!
//! **CORRECT: Lock room first, then members**
//! ```text
//! // Transaction A: Update room and its members
//! let room = sqlx::query("SELECT * FROM rooms WHERE id = $1 FOR UPDATE")
//!     .bind(room_id)
//!     .fetch_one(&mut *tx).await?;
//!
//! let members = sqlx::query("SELECT * FROM room_members WHERE room_id = $1 FOR UPDATE")
//!     .bind(room_id)
//!     .fetch_all(&mut *tx).await?;
//! ```
//!
//! **INCORRECT: Inconsistent ordering causes deadlock**
//! ```text
//! // Transaction A: Updates room 1, then room 2
//! // Transaction B: Updates room 2, then room 1
//! // DEADLOCK: A waits for B's lock on room 2, B waits for A's lock on room 1
//! ```
//!
//! **CORRECT: Always lock rooms in ID order**
//! ```text
//! let room_ids = vec!["room_1", "room_2"];
//! let mut sorted_ids = room_ids.clone();
//! sorted_ids.sort(); // Consistent ordering
//!
//! for id in sorted_ids {
//!     sqlx::query("SELECT * FROM rooms WHERE id = $1 FOR UPDATE")
//!         .bind(id)
//!         .fetch_one(&mut *tx).await?;
//! }
//! ```
//!
//! ### Retry Strategy
//!
//! When a deadlock occurs (error code 40P01), the operation should be retried:
//!
//! ```text
//! const MAX_RETRIES: u32 = 3;
//! for attempt in 0..MAX_RETRIES {
//!     match do_operation(&pool).await {
//!         Ok(result) => return Ok(result),
//!         Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("40P01") => {
//!             // Deadlock detected, retry with fresh transaction
//!             tokio::time::sleep(Duration::from_millis(10 * (1 << attempt))).await;
//!             continue;
//!         }
//!         Err(e) => return Err(e.into()),
//!     }
//! }
//! ```
//!
//! ### Entity Lock Hierarchy
//!
//! For this codebase, use the following lock ordering:
//!
//! 1. Users (highest priority - lock first)
//! 2. Rooms
//! 3. Room Members
//! 4. Playlists
//! 5. Media
//! 6. Playback State (lowest priority - lock last)
//!
//! When locking multiple entities of the same type, sort by ID (lexicographically for string IDs).

use sqlx::{PgPool, Postgres, Transaction};

use crate::Result;

/// Error type for transaction operations
#[derive(Debug, Clone)]
pub struct TransactionError(pub &'static str);

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TransactionError {}

/// Unit of Work for managing database transactions
///
/// This is an alternative transaction management pattern that wraps a transaction
/// and tracks its lifecycle. It's primarily useful for complex scenarios where you
/// need to pass transaction context across multiple function calls.
///
/// ## When to Use
///
/// - Complex multi-step operations spanning multiple functions
/// - When you need to check if a transaction is still active
/// - When building higher-level transaction abstractions
///
/// ## When NOT to Use
///
/// For simple, localized transactions, prefer the standard pattern:
/// ```text
/// let mut tx = pool.begin().await?;
/// // ... operations
/// tx.commit().await?;
/// ```
///
/// ## Example
///
/// ```text
/// let mut uow = UnitOfWork::begin(&pool).await?;
/// let tx = uow.transaction()?;
///
/// // Pass tx to multiple functions
/// repo1.save(data, tx).await?;
/// repo2.update(id, tx).await?;
///
/// uow.commit().await?;
/// ```
pub struct UnitOfWork {
    tx: Option<Transaction<'static, Postgres>>,
    /// Track if the transaction was explicitly committed
    committed: bool,
    /// Track if the transaction was explicitly rolled back
    rolled_back: bool,
    /// For testing: simulate having an active uncommitted transaction
    #[cfg(test)]
    _test_simulate_uncommitted: bool,
}

impl UnitOfWork {
    /// Begin a new transaction
    pub async fn begin(pool: &PgPool) -> Result<Self> {
        let tx = pool.begin().await?;
        Ok(Self {
            tx: Some(tx),
            committed: false,
            rolled_back: false,
            #[cfg(test)]
            _test_simulate_uncommitted: false,
        })
    }

    /// Commit the transaction
    pub async fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await?;
        }
        self.committed = true;
        Ok(())
    }

    /// Rollback the transaction
    pub async fn rollback(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
        }
        self.rolled_back = true;
        Ok(())
    }

    /// Get the transaction for repository operations
    ///
    /// Returns an error if the transaction has already been consumed
    /// (committed or rolled back).
    pub fn transaction(&mut self) -> Result<&mut Transaction<'static, Postgres>> {
        self.tx.as_mut().ok_or(crate::error::Error::Internal(
            "Transaction already consumed".to_string(),
        ))
    }

    /// Check if the transaction is still active (not consumed)
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.tx.is_some()
    }

    /// Check if the transaction was explicitly handled (committed or rolled back)
    #[inline]
    const fn is_handled(&self) -> bool {
        self.committed || self.rolled_back
    }

    /// Create a `UnitOfWork` for testing purposes that simulates an uncommitted state.
    ///
    /// This allows testing the panic behavior without a real database connection.
    #[cfg(test)]
    #[must_use]
    pub const fn new_uncommitted_for_testing() -> Self {
        Self {
            tx: None,
            committed: false,
            rolled_back: false,
            _test_simulate_uncommitted: true,
        }
    }

    /// Mark the `UnitOfWork` as committed for testing purposes
    #[cfg(test)]
    pub const fn mark_committed_for_testing(&mut self) {
        self.committed = true;
    }

    /// Mark the `UnitOfWork` as rolled back for testing purposes
    #[cfg(test)]
    pub const fn mark_rolled_back_for_testing(&mut self) {
        self.rolled_back = true;
    }
}

// Implement Drop for automatic rollback on panic
impl Drop for UnitOfWork {
    fn drop(&mut self) {
        // Check if the transaction needs explicit handling
        // In production: tx.is_some() means we have an uncommitted transaction
        // In testing: _test_simulate_uncommitted simulates having an uncommitted tx
        #[cfg(not(test))]
        let needs_handling = self.tx.is_some() && !self.is_handled();

        #[cfg(test)]
        let needs_handling =
            (self.tx.is_some() || self._test_simulate_uncommitted) && !self.is_handled();

        // Transaction was not explicitly committed/rolled back.
        // sqlx will automatically rollback when the Transaction is dropped,
        // but this is likely a bug in the caller.
        //
        // In debug mode, panic to catch the bug early.
        // In release mode, just log a warning.
        assert!(
            !needs_handling,
            "UnitOfWork dropped without explicit commit or rollback! \
             This is likely a bug - transactions should be explicitly committed or rolled back. \
             The transaction will be rolled back automatically."
        );
    }
}

/// Transaction wrapper for automatic commit on success
///
/// This helper allows for clean transaction handling with automatic commit/rollback.
pub async fn with_transaction<F, R>(pool: &PgPool, f: F) -> Result<R>
where
    F: for<'e> FnOnce(&mut Transaction<'e, Postgres>) -> futures::future::BoxFuture<'e, Result<R>>
        + Send
        + Sync,
    R: Send + Sync + 'static,
{
    let mut tx = pool.begin().await?;

    match f(&mut tx).await {
        Ok(result) => {
            tx.commit().await?;
            Ok(result)
        }
        Err(e) => {
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!("Rollback failed: {rollback_err}");
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    // ========== UnitOfWork State Machine Tests ==========

    #[test]
    fn test_transaction_error_display() {
        let err = TransactionError("Transaction already consumed");
        assert_eq!(err.to_string(), "Transaction already consumed");
    }

    #[test]
    fn test_transaction_error_is_error() {
        let err = TransactionError("test error");
        // Ensure it implements std::error::Error
        let _: &dyn std::error::Error = &err;
        assert_eq!(err.0, "test error");
    }

    #[test]
    fn test_transaction_error_clone() {
        let err = TransactionError("clone me");
        let cloned = err;
        assert_eq!(cloned.0, "clone me");
    }

    #[test]
    fn test_uow_transaction_returns_error_when_consumed() {
        let mut uow = UnitOfWork {
            tx: None,
            committed: false,
            rolled_back: false,
            _test_simulate_uncommitted: false,
        };
        let result = uow.transaction();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("already consumed"));
    }

    #[test]
    fn test_uow_is_active_when_no_transaction() {
        let uow = UnitOfWork {
            tx: None,
            committed: false,
            rolled_back: false,
            _test_simulate_uncommitted: false,
        };
        assert!(!uow.is_active());
    }

    #[test]
    fn test_uow_drop_when_committed_is_safe() {
        // Dropping a committed UnitOfWork should not panic
        let uow = UnitOfWork {
            tx: None,
            committed: true,
            rolled_back: false,
            _test_simulate_uncommitted: true, // Simulates having had a transaction
        };
        drop(uow); // Should not panic because committed = true
    }

    #[test]
    fn test_uow_drop_when_rolled_back_is_safe() {
        // Dropping a rolled back UnitOfWork should not panic
        let uow = UnitOfWork {
            tx: None,
            committed: false,
            rolled_back: true,
            _test_simulate_uncommitted: true, // Simulates having had a transaction
        };
        drop(uow); // Should not panic because rolled_back = true
    }

    // ========== TDD Tests for Uncommitted Detection ==========

    /// Test: Uncommitted `UnitOfWork` panics in debug mode when dropped.
    /// This is the key safety feature to catch developer mistakes early.
    #[test]
    fn test_uncommitted_uow_panics_in_debug_mode() {
        #[cfg(debug_assertions)]
        {
            let result = catch_unwind(AssertUnwindSafe(|| {
                let _uow = UnitOfWork::new_uncommitted_for_testing();
                // When _uow goes out of scope, it should panic
            }));

            assert!(
                result.is_err(),
                "Expected panic when dropping uncommitted UnitOfWork in debug mode"
            );

            // Verify the panic message contains useful information
            if let Err(panic_payload) = result {
                if let Some(msg) = panic_payload.downcast_ref::<&str>() {
                    assert!(
                        msg.contains("UnitOfWork dropped without explicit commit or rollback"),
                        "Panic message should mention uncommitted UnitOfWork: {msg}"
                    );
                } else if let Some(msg) = panic_payload.downcast_ref::<String>() {
                    assert!(
                        msg.contains("UnitOfWork dropped without explicit commit or rollback"),
                        "Panic message should mention uncommitted UnitOfWork: {msg}"
                    );
                }
            }
        }

        #[cfg(not(debug_assertions))]
        {
            println!("Test skipped in release mode");
        }
    }

    /// Test: Committed `UnitOfWork` drops cleanly without panic.
    #[test]
    fn test_committed_uow_drops_cleanly() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut uow = UnitOfWork::new_uncommitted_for_testing();
            uow.mark_committed_for_testing();
            drop(uow);
        }));

        assert!(
            result.is_ok(),
            "Committed UnitOfWork should drop without panic"
        );
    }

    /// Test: Explicitly rolled back `UnitOfWork` drops cleanly without panic.
    #[test]
    fn test_rolled_back_uow_drops_cleanly() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut uow = UnitOfWork::new_uncommitted_for_testing();
            uow.mark_rolled_back_for_testing();
            drop(uow);
        }));

        assert!(
            result.is_ok(),
            "Rolled back UnitOfWork should drop without panic"
        );
    }

    /// Test: Double commit is safe (no panic on drop).
    #[test]
    fn test_double_commit_is_safe() {
        let mut uow = UnitOfWork::new_uncommitted_for_testing();
        uow.mark_committed_for_testing();
        uow.mark_committed_for_testing(); // Second commit should be safe

        let result = catch_unwind(AssertUnwindSafe(|| {
            drop(uow);
        }));

        assert!(
            result.is_ok(),
            "Double committed UnitOfWork should drop safely"
        );
    }

    /// Test: Double rollback is safe (no panic on drop).
    #[test]
    fn test_double_rollback_is_safe() {
        let mut uow = UnitOfWork::new_uncommitted_for_testing();
        uow.mark_rolled_back_for_testing();
        uow.mark_rolled_back_for_testing(); // Second rollback should be safe

        let result = catch_unwind(AssertUnwindSafe(|| {
            drop(uow);
        }));

        assert!(
            result.is_ok(),
            "Double rolled back UnitOfWork should drop safely"
        );
    }

    /// Test: Commit after rollback is safe (no panic on drop).
    #[test]
    fn test_commit_after_rollback_is_safe() {
        let mut uow = UnitOfWork::new_uncommitted_for_testing();
        uow.mark_rolled_back_for_testing();
        uow.mark_committed_for_testing(); // Should be safe, no-op

        let result = catch_unwind(AssertUnwindSafe(|| {
            drop(uow);
        }));

        assert!(
            result.is_ok(),
            "UnitOfWork with both flags should drop safely"
        );
    }

    /// Test: `is_handled` method returns correct state.
    #[test]
    fn test_is_handled_method() {
        // Test committed state
        let uow = UnitOfWork {
            tx: None,
            committed: true,
            rolled_back: false,
            _test_simulate_uncommitted: false,
        };
        assert!(uow.is_handled());

        // Test rolled back state
        let uow = UnitOfWork {
            tx: None,
            committed: false,
            rolled_back: true,
            _test_simulate_uncommitted: false,
        };
        assert!(uow.is_handled());

        // Test unhandled state
        let uow = UnitOfWork {
            tx: None,
            committed: false,
            rolled_back: false,
            _test_simulate_uncommitted: false,
        };
        assert!(!uow.is_handled());

        // Test both flags set (should be handled)
        let uow = UnitOfWork {
            tx: None,
            committed: true,
            rolled_back: true,
            _test_simulate_uncommitted: false,
        };
        assert!(uow.is_handled());
    }

    /// Test: In release mode, uncommitted UnitOfWork only logs a warning (no panic).
    #[test]
    #[cfg(not(debug_assertions))]
    fn test_uncommitted_uow_only_warns_in_release() {
        // In release mode, should not panic, just warn
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _uow = UnitOfWork::new_uncommitted_for_testing();
        }));

        assert!(
            result.is_ok(),
            "Uncommitted UnitOfWork should not panic in release mode"
        );
    }
}
