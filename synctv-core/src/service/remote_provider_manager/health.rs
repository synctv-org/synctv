use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::stream::{self, StreamExt};
use synctv_common::ExecutionControl;

use super::RemoteProviderManager;
use crate::models::ProviderInstance;
use synctv_media_providers::remote_transport::{
    execute_health_check, validate_auth_secret, RemoteProviderConnection,
};

const REMOTE_HEALTH_CHECK_CONCURRENCY: usize = 16;

impl RemoteProviderManager {
    pub(super) fn probe_execution_control(
        control: Option<&ExecutionControl>,
        timeout: Duration,
    ) -> ExecutionControl {
        let probe_deadline = std::time::Instant::now() + timeout;
        match control {
            Some(control) => {
                let deadline = control
                    .deadline()
                    .map_or(probe_deadline, |deadline| deadline.min(probe_deadline));
                ExecutionControl::from_parts(Some(deadline), control.cancellation_token())
            }
            None => ExecutionControl::from_timeout(Some(timeout)),
        }
    }

    pub(super) async fn build_management_validated_remote_connection_with_control(
        &self,
        config: &ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<RemoteProviderConnection> {
        let connection = self.create_remote_connection(config)?;
        self.validate_management_connection_with_control(config, &connection, control)
            .await?;
        Ok(connection)
    }

    async fn validate_management_connection_with_control(
        &self,
        config: &ProviderInstance,
        connection: &RemoteProviderConnection,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let timeout = Duration::from_secs(5);
        let control = Self::probe_execution_control(control, timeout);

        let status = execute_health_check(&config.name, connection, &control, timeout)
            .await
            .map_err(|error| Self::map_remote_transport_validation_error(&error))?;
        if status != 1 {
            return Err(crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' is not serving (health status: {status})",
                config.name
            )));
        }

        Ok(())
    }

    /// Health check all remote instances
    ///
    /// Returns a map of instance name to health status.
    /// Uses the remote provider health-check protocol with a 5-second timeout per instance.
    ///
    /// Loads the full list from DB to check all instances, not just cached ones.
    pub async fn health_check(&self) -> HashMap<String, bool> {
        let configs = match self.repository.get_all_enabled().await {
            Ok(configs) => configs,
            Err(e) => {
                tracing::error!("Failed to load instances for health check: {e}");
                return HashMap::new();
            }
        };

        self.health_check_instances(&configs).await
    }

    /// Health check a selected set of provider instances.
    ///
    /// This avoids probing every enabled instance when a caller only needs
    /// status for a filtered or paginated subset.
    pub async fn health_check_instances(
        &self,
        configs: &[ProviderInstance],
    ) -> HashMap<String, bool> {
        let mut results = HashMap::new();

        for config in configs {
            if !Self::requires_remote_connection(config) {
                continue;
            }

            if validate_auth_secret(config.jwt_secret.as_deref()).is_err() {
                tracing::warn!(
                    "Health check reporting provider instance '{}' unhealthy: missing or invalid jwt_secret for remote-capable configuration",
                    config.name
                );
                results.insert(config.name.clone(), false);
                continue;
            }

            let Some(connection) = self.get(&config.name).await else {
                results.insert(config.name.clone(), false);
                continue;
            };

            let is_healthy = self
                .check_instance_health(&config.name, config, &connection)
                .await;
            results.insert(config.name.clone(), is_healthy);
        }

        results
    }

    pub async fn health_check_instances_owned(
        self: Arc<Self>,
        configs: Vec<ProviderInstance>,
    ) -> HashMap<String, bool> {
        let mut results = HashMap::new();
        let mut remote_configs = Vec::new();

        for config in configs {
            if !Self::requires_remote_connection(&config) {
                continue;
            }

            if validate_auth_secret(config.jwt_secret.as_deref()).is_err() {
                tracing::warn!(
                    "Health check reporting provider instance '{}' unhealthy: missing or invalid jwt_secret for remote-capable configuration",
                    config.name
                );
                results.insert(config.name.clone(), false);
                continue;
            }

            remote_configs.push(config);
        }

        let manager = Arc::clone(&self);
        let remote_results = stream::iter(remote_configs)
            .map(move |config| {
                let manager = Arc::clone(&manager);
                let name = config.name.clone();
                async move {
                    let is_healthy = manager.health_check_instance(&config).await;
                    (name, is_healthy)
                }
            })
            .buffer_unordered(REMOTE_HEALTH_CHECK_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        for (name, is_healthy) in remote_results {
            results.insert(name, is_healthy);
        }

        results
    }

    async fn health_check_instance(self: Arc<Self>, config: &ProviderInstance) -> bool {
        let Some(connection) = self.get(&config.name).await else {
            return false;
        };

        self.check_instance_health(&config.name, config, &connection)
            .await
    }

    /// Check health of a single remote instance
    ///
    /// Calls the remote provider health-check endpoint with a 5-second timeout.
    async fn check_instance_health(
        &self,
        name: &str,
        config: &ProviderInstance,
        connection: &RemoteProviderConnection,
    ) -> bool {
        match self
            .validate_management_connection_with_control(config, connection, None)
            .await
        {
            Ok(()) => {
                tracing::debug!("Provider instance '{}' is healthy", name);
                true
            }
            Err(error) => {
                tracing::error!("Health check failed for instance '{}': {}", name, error);
                false
            }
        }
    }
}
