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
//! ```rust,ignore
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
//! ```rust,ignore
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
/// ```rust,ignore
/// let mut tx = pool.begin().await?;
/// // ... operations
/// tx.commit().await?;
/// ```
///
/// ## Example
///
/// ```rust,ignore
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
}

impl UnitOfWork {
    /// Begin a new transaction
    pub async fn begin(pool: &PgPool) -> Result<Self> {
        let tx = pool.begin().await?;
        Ok(Self { tx: Some(tx) })
    }

    /// Commit the transaction
    pub async fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await?;
        }
        Ok(())
    }

    /// Rollback the transaction
    pub async fn rollback(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
        }
        Ok(())
    }

    /// Get the transaction for repository operations
    ///
    /// Returns an error if the transaction has already been consumed
    /// (committed or rolled back).
    pub fn transaction(&mut self) -> Result<&mut Transaction<'static, Postgres>> {
        self.tx.as_mut().ok_or(crate::error::Error::Internal("Transaction already consumed".to_string()))
    }

    /// Check if the transaction is still active (not consumed)
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.tx.is_some()
    }
}

// Implement Drop for automatic rollback on panic
impl Drop for UnitOfWork {
    fn drop(&mut self) {
        if self.tx.is_some() {
            // Transaction was not explicitly committed/rolled back.
            // sqlx will automatically rollback when the Transaction is dropped,
            // but this is likely a bug in the caller.
            tracing::warn!(
                "UnitOfWork dropped without explicit commit or rollback; \
                 transaction will be rolled back automatically"
            );
        }
    }
}

/// Transaction wrapper for automatic commit on success
///
/// This helper allows for clean transaction handling with automatic commit/rollback.
pub async fn with_transaction<F, R>(pool: &PgPool, f: F) -> Result<R>
where
    F: for<'e> FnOnce(&mut Transaction<'e, Postgres>) -> futures::future::BoxFuture<'e, Result<R>> + Send + Sync,
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
        let cloned = err.clone();
        assert_eq!(cloned.0, "clone me");
    }

    #[test]
    fn test_uow_transaction_returns_error_when_consumed() {
        let mut uow = UnitOfWork { tx: None };
        let result = uow.transaction();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("already consumed"));
    }

    #[test]
    fn test_uow_is_active_when_no_transaction() {
        let uow = UnitOfWork { tx: None };
        assert!(!uow.is_active());
    }

    #[test]
    fn test_uow_drop_when_consumed_is_safe() {
        // Dropping a consumed UnitOfWork should not panic
        let uow = UnitOfWork { tx: None };
        drop(uow); // Should not panic
    }

    // ========== Integration test placeholders ==========

    #[tokio::test]
    #[ignore = "Requires database"]
    async fn test_transaction_commit() {
        // Integration test placeholder
    }

    #[tokio::test]
    #[ignore = "Requires database"]
    async fn test_transaction_rollback() {
        // Integration test placeholder
    }
}
