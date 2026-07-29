use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use synctv_common::ExecutionControl;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::{
    repository::UserOAuthProviderRepository,
    service::oauth2::{
        OAuth2Service, OAuth2ServiceRuntime, OAuth2State, OAuthStateStore,
        OAUTH2_STATE_TTL_SECONDS, OAUTH2_STATE_TTL_SECONDS_I64,
    },
    Error, Result,
};

impl OAuth2Service {
    pub(crate) async fn run_with_control<T, F>(
        control: Option<&ExecutionControl>,
        future: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        match control {
            Some(control) => control.run(future).await.map_err(Error::from)?,
            None => future.await,
        }
    }

    pub(crate) fn validate_cluster_state_store(
        cluster_mode: bool,
        state_store: &dyn OAuthStateStore,
    ) -> Result<()> {
        if cluster_mode && !state_store.supports_cross_node_single_use() {
            return Err(Error::Internal(
                "distributed runtime requires shared single-use OAuth2 state storage. \
                 Local-only state is only visible on the replica that created it, \
                 causing authentication failures when the callback hits a different replica. \
                 Configure a shared state backend to fix this."
                    .to_string(),
            ));
        }

        Ok(())
    }

    pub fn new(
        repository: UserOAuthProviderRepository,
        state_store: Arc<dyn OAuthStateStore>,
        provider_registry: crate::oauth2::ProviderRegistry,
        cluster_mode: bool,
    ) -> Result<Self> {
        Self::new_with_ssrf_guard(
            repository,
            state_store,
            provider_registry,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
            cluster_mode,
        )
    }

    pub fn new_with_ssrf_guard(
        repository: UserOAuthProviderRepository,
        state_store: Arc<dyn OAuthStateStore>,
        provider_registry: crate::oauth2::ProviderRegistry,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
        cluster_mode: bool,
    ) -> Result<Self> {
        Self::new_with_runtime(
            repository,
            state_store,
            provider_registry,
            ssrf_guard,
            cluster_mode,
            OAuth2ServiceRuntime::default(),
        )
    }

    pub fn new_with_runtime(
        repository: UserOAuthProviderRepository,
        state_store: Arc<dyn OAuthStateStore>,
        provider_registry: crate::oauth2::ProviderRegistry,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
        cluster_mode: bool,
        runtime: OAuth2ServiceRuntime,
    ) -> Result<Self> {
        Self::validate_cluster_state_store(cluster_mode, state_store.as_ref())?;

        info!(
            cross_node_single_use = state_store.supports_cross_node_single_use(),
            "OAuth2 service initialized"
        );

        Ok(Self {
            repository: Some(repository),
            providers: Arc::new(RwLock::new(HashMap::new())),
            state_store,
            provider_registry,
            ssrf_guard,
            runtime_settings_store: runtime.runtime_settings_store,
            user_service: runtime.user_service,
            providers_fingerprint: Arc::new(RwLock::new(None)),
        })
    }

    #[cfg(test)]
    pub(crate) fn new_without_repository_for_tests(
        state_store: Arc<dyn OAuthStateStore>,
        provider_registry: crate::oauth2::ProviderRegistry,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
        cluster_mode: bool,
        runtime: OAuth2ServiceRuntime,
    ) -> Result<Self> {
        Self::validate_cluster_state_store(cluster_mode, state_store.as_ref())?;

        Ok(Self {
            repository: None,
            providers: Arc::new(RwLock::new(HashMap::new())),
            state_store,
            provider_registry,
            ssrf_guard,
            runtime_settings_store: runtime.runtime_settings_store,
            user_service: runtime.user_service,
            providers_fingerprint: Arc::new(RwLock::new(None)),
        })
    }

    #[must_use]
    pub const fn provider_registry(&self) -> &crate::oauth2::ProviderRegistry {
        &self.provider_registry
    }

    #[cfg(test)]
    pub(crate) async fn store_state(&self, state_token: &str, state: &OAuth2State) -> Result<()> {
        self.store_state_with_control(state_token, state, None)
            .await
    }

    pub(crate) async fn store_state_with_control(
        &self,
        state_token: &str,
        state: &OAuth2State,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::run_with_control(
            control,
            self.state_store.store(
                state_token,
                state,
                std::time::Duration::from_secs(OAUTH2_STATE_TTL_SECONDS),
            ),
        )
        .await?;
        debug!(
            "Stored OAuth2 state for token {}",
            &state_token[..8.min(state_token.len())]
        );
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn consume_state(&self, state_token: &str) -> Result<OAuth2State> {
        self.consume_state_with_control(state_token, None).await
    }

    pub(crate) async fn consume_state_with_control(
        &self,
        state_token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2State> {
        match Self::run_with_control(control, self.state_store.consume(state_token)).await? {
            Some(state) => {
                let age = crate::SystemClock
                    .now()
                    .signed_duration_since(state.created_at);
                if age.num_seconds() > OAUTH2_STATE_TTL_SECONDS_I64 {
                    debug!(
                        "OAuth2 state expired based on created_at (age: {}s, max: {}s)",
                        age.num_seconds(),
                        OAUTH2_STATE_TTL_SECONDS
                    );
                    return Err(Error::Authentication(
                        "Invalid or expired OAuth2 state".to_string(),
                    ));
                }

                debug!(
                    "Retrieved OAuth2 state for token {}",
                    &state_token[..8.min(state_token.len())]
                );
                Ok(state)
            }
            None => Err(Error::Authentication(
                "Invalid or expired OAuth2 state".to_string(),
            )),
        }
    }
}
