use super::DistributedLock;
use crate::{Error, Result};

#[must_use = "lock guard must be explicitly released via .release() for reliable unlock"]
pub struct LockGuard {
    lock: DistributedLock,
    key: String,
    value: Option<String>,
    fencing_token: Option<u64>,
    drop_tx: Option<tokio::sync::oneshot::Sender<(String, String)>>,
}

impl LockGuard {
    fn spawn_drop_task(lock: DistributedLock) -> tokio::sync::oneshot::Sender<(String, String)> {
        let (tx, rx) = tokio::sync::oneshot::channel::<(String, String)>();
        tokio::spawn(async move {
            if let Ok((key, value)) = rx.await {
                if let Err(error) = lock.release(&key, &value).await {
                    tracing::error!(
                        key = %key,
                        error = %error,
                        "Background task failed to release lock"
                    );
                }
            }
        });
        tx
    }

    pub async fn new(lock: DistributedLock, key: String, ttl_seconds: u64) -> Result<Self> {
        let value = lock
            .acquire(&key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

        let drop_tx = Some(Self::spawn_drop_task(lock.clone()));

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token: None,
            drop_tx,
        })
    }

    pub async fn new_with_token(
        lock: DistributedLock,
        key: String,
        ttl_seconds: u64,
    ) -> Result<Self> {
        let (value, fencing_token) = lock
            .acquire_with_token(&key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

        let drop_tx = Some(Self::spawn_drop_task(lock.clone()));

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token: Some(fencing_token),
            drop_tx,
        })
    }

    #[must_use]
    pub const fn fencing_token(&self) -> u64 {
        match self.fencing_token {
            Some(token) => token,
            None => 0,
        }
    }

    #[must_use]
    pub const fn fencing_token_opt(&self) -> Option<u64> {
        self.fencing_token
    }

    pub async fn extend(&self, ttl_seconds: u64) -> Result<bool> {
        if let Some(ref value) = self.value {
            self.lock.extend(&self.key, value, ttl_seconds).await
        } else {
            Ok(false)
        }
    }

    pub async fn release(mut self) -> Result<bool> {
        let _ = self.drop_tx.take();

        if let Some(value) = self.value.take() {
            self.lock.release(&self.key, &value).await
        } else {
            Ok(false)
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            if let Some(tx) = self.drop_tx.take() {
                let key = self.key.clone();
                if tx.send((key.clone(), value)).is_err() {
                    tracing::warn!(
                        key = %key,
                        "Lock drop task exited before receiving unlock signal; lock will expire after TTL"
                    );
                }
            }
        }
    }
}
