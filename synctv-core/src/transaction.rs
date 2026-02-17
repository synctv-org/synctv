//! Unit of Work implementation
//!
//! Provides transactional scope for multi-repository operations.

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
/// Wraps a database transaction and provides access to repositories
/// that work within the same transactional context.
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
