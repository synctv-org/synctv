//! Database transaction helpers.

use sqlx::{PgPool, Postgres, Transaction};

use crate::Result;

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
