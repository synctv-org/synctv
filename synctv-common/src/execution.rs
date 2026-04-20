use std::future::Future;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Minimal cooperative execution control shared across layers.
///
/// This carries only lifecycle control data:
/// - an optional absolute deadline
/// - a cancellation token
///
/// Request metadata such as auth, IP, and user-agent must stay out of this type.
#[derive(Clone, Debug)]
pub struct ExecutionControl {
    deadline: Option<Instant>,
    cancellation: CancellationToken,
}

impl Default for ExecutionControl {
    fn default() -> Self {
        Self {
            deadline: None,
            cancellation: CancellationToken::new(),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ExecutionControlError {
    #[error("Request cancelled")]
    Cancelled,
    #[error("Request timed out")]
    DeadlineExceeded,
}

impl ExecutionControl {
    #[must_use]
    pub fn from_parts(deadline: Option<Instant>, cancellation: CancellationToken) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

    #[must_use]
    pub fn from_timeout(timeout: Option<Duration>) -> Self {
        let deadline = timeout.and_then(|budget| Instant::now().checked_add(budget));
        Self::from_parts(deadline, CancellationToken::new())
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    #[must_use]
    pub fn remaining_timeout(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            deadline: self.deadline,
            cancellation: self.cancellation.child_token(),
        }
    }

    #[must_use]
    pub fn without_deadline(mut self) -> Self {
        self.deadline = None;
        self
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub fn check_deadline(&self) -> Result<(), ExecutionControlError> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ExecutionControlError::DeadlineExceeded);
        }

        Ok(())
    }

    pub fn check_active(&self) -> Result<(), ExecutionControlError> {
        if self.is_cancelled() {
            return Err(ExecutionControlError::Cancelled);
        }

        self.check_deadline()
    }

    pub async fn run<F, T>(&self, future: F) -> Result<T, ExecutionControlError>
    where
        F: Future<Output = T>,
    {
        self.check_active()?;

        let cancellation = self.cancellation_token();
        tokio::pin!(future);

        if let Some(deadline) = self.deadline {
            let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
            tokio::pin!(sleep);

            tokio::select! {
                () = cancellation.cancelled() => Err(ExecutionControlError::Cancelled),
                () = &mut sleep => Err(ExecutionControlError::DeadlineExceeded),
                output = &mut future => Ok(output),
            }
        } else {
            tokio::select! {
                () = cancellation.cancelled() => Err(ExecutionControlError::Cancelled),
                output = &mut future => Ok(output),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_returns_cancelled_before_future_completes() {
        let control = ExecutionControl::default();
        control.cancel();

        let result = control.run(async { 42 }).await;

        assert_eq!(result, Err(ExecutionControlError::Cancelled));
    }

    #[tokio::test]
    async fn run_returns_deadline_exceeded_when_budget_elapsed() {
        let control = ExecutionControl::from_timeout(Some(Duration::ZERO));

        let result = control.run(async { 42 }).await;

        assert_eq!(result, Err(ExecutionControlError::DeadlineExceeded));
    }
}
