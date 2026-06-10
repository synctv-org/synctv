use super::DistributedLock;
use crate::{Error, Result};

#[must_use = "lock guard must be explicitly released via .release() for reliable unlock"]
pub struct LockGuard {
    lock: DistributedLock,
    key: String,
    value: Option<String>,
    fencing_token: Option<u64>,
}

impl LockGuard {
    pub async fn new(lock: DistributedLock, key: String, ttl_seconds: u64) -> Result<Self> {
        let value = lock
            .acquire(&key, ttl_seconds)
            .await?
            .ok_or_else(|| Error::LockConflict(format!("Lock already held: {key}")))?;

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token: None,
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

        Ok(Self {
            lock,
            key,
            value: Some(value),
            fencing_token: Some(fencing_token),
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
            let key = self.key.clone();
            let lock = self.lock.clone();

            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    if let Err(error) = lock.release(&key, &value).await {
                        tracing::error!(
                            key = %key,
                            error = %error,
                            "Background task failed to release lock"
                        );
                    }
                });
            } else {
                tracing::warn!(
                    key = %key,
                    "Lock dropped without a runtime handle; lock will expire after TTL"
                );
            }
        }
    }
}
