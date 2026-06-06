use std::sync::Arc;

use crate::{Error, RedisConnectionRuntime, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedStateMode {
    LocalOnly,
    SharedBestEffort,
    SharedRequired,
}

#[derive(Clone)]
pub struct SharedStateProfile {
    state_mode: SharedStateMode,
    shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    key_prefix: String,
}

impl std::fmt::Debug for SharedStateProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedStateProfile")
            .field("state_mode", &self.state_mode)
            .field(
                "shared_runtime",
                &self.shared_runtime.as_ref().map(|_| "<configured>"),
            )
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

impl SharedStateProfile {
    #[must_use]
    pub fn new(
        state_mode: SharedStateMode,
        shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            state_mode,
            shared_runtime,
            key_prefix: key_prefix.into(),
        }
    }

    #[must_use]
    pub fn local_only(key_prefix: impl Into<String>) -> Self {
        Self::new(SharedStateMode::LocalOnly, None, key_prefix)
    }

    #[must_use]
    pub fn best_effort(
        shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: impl Into<String>,
    ) -> Self {
        let state_mode = if shared_runtime.is_some() {
            SharedStateMode::SharedBestEffort
        } else {
            SharedStateMode::LocalOnly
        };
        Self::new(state_mode, shared_runtime, key_prefix)
    }

    #[must_use]
    pub fn required(
        shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self::new(SharedStateMode::SharedRequired, shared_runtime, key_prefix)
    }

    #[must_use]
    pub fn for_cluster_runtime(
        shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: impl Into<String>,
        cluster_runtime: bool,
    ) -> Self {
        if cluster_runtime {
            Self::required(shared_runtime, key_prefix)
        } else {
            Self::best_effort(shared_runtime, key_prefix)
        }
    }

    #[must_use]
    pub fn from_runtime(
        shared_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: impl Into<String>,
        shared_state_required: bool,
    ) -> Self {
        Self::for_cluster_runtime(shared_runtime, key_prefix, shared_state_required)
    }

    #[must_use]
    pub const fn state_mode(&self) -> SharedStateMode {
        self.state_mode
    }

    #[must_use]
    pub const fn shared_state_required(&self) -> bool {
        matches!(self.state_mode, SharedStateMode::SharedRequired)
    }

    #[must_use]
    pub fn key_prefix(&self) -> &str {
        &self.key_prefix
    }

    #[must_use]
    pub fn shared_runtime(&self) -> Option<Arc<dyn RedisConnectionRuntime>> {
        self.shared_runtime.clone()
    }

    pub fn require_shared_runtime(
        &self,
        capability_description: &str,
    ) -> Result<Arc<dyn RedisConnectionRuntime>> {
        self.shared_runtime.clone().ok_or_else(|| {
            Error::ServiceUnavailable(format!(
                "distributed runtime requires shared {capability_description}"
            ))
        })
    }

    pub fn best_effort_shared_runtime(
        &self,
        capability_description: &str,
    ) -> Result<Arc<dyn RedisConnectionRuntime>> {
        self.shared_runtime.clone().ok_or_else(|| {
            Error::ServiceUnavailable(format!(
                "shared best-effort mode requires configured {capability_description}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_effort_shared_runtime_rejects_inconsistent_profile_without_panic() {
        let profile = SharedStateProfile::new(SharedStateMode::SharedBestEffort, None, "test:");

        let Err(error) = profile.best_effort_shared_runtime("single-use test state") else {
            panic!("inconsistent best-effort profile should return an error");
        };

        assert!(
            matches!(error, Error::ServiceUnavailable(_)),
            "unexpected error: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("shared best-effort mode requires configured single-use test state"),
            "unexpected error: {error}"
        );
    }
}
