use super::RemoteProviderManager;
use crate::models::ProviderInstance;
use crate::provider::provider_client::RemoteProviderConnection;
use synctv_common::ExecutionControl;

impl RemoteProviderManager {
    async fn restore_cached_connection(
        &self,
        instance_name: &str,
        previous_connection: Option<RemoteProviderConnection>,
    ) {
        if let Some(connection) = previous_connection {
            self.connection_cache
                .insert(instance_name.to_string(), connection)
                .await;
        } else {
            self.connection_cache.invalidate(instance_name).await;
        }
    }

    fn rollback_failure(
        operation: &'static str,
        instance_name: &str,
        notify_error: &crate::Error,
        rollback_error: &crate::Error,
    ) -> crate::Error {
        crate::Error::Internal(format!(
            "Failed to roll back provider instance {operation} for '{instance_name}' after invalidation publish failure. publish_error: {notify_error}; rollback_error: {rollback_error}"
        ))
    }

    pub async fn add(&self, config: ProviderInstance) -> crate::Result<()> {
        Box::pin(self.add_with_control(config, None)).await
    }

    pub async fn add_with_control(
        &self,
        config: ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        Self::validate_config_with_ssrf_guard(&config, &self.ssrf_guard)?;

        let connection = if config.enabled && Self::requires_remote_connection(&config) {
            Some(
                Box::pin(
                    self.build_management_validated_remote_connection_with_control(
                        &config, control,
                    ),
                )
                .await?,
            )
        } else {
            None
        };

        self.repository.create(&config).await?;

        if let Some(connection) = connection {
            self.connection_cache
                .insert(config.name.clone(), connection)
                .await;
        } else {
            self.connection_cache.invalidate(&config.name).await;
        }

        if let Err(notify_error) = self.notify_change("add", &config.name).await {
            if let Err(rollback_error) = self.repository.delete(&config.name).await {
                return Err(Self::rollback_failure(
                    "add",
                    &config.name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.connection_cache.invalidate(&config.name).await;
            return Err(notify_error);
        }

        tracing::info!("Added provider instance: {}", config.name);
        Ok(())
    }

    pub async fn update(&self, config: ProviderInstance) -> crate::Result<()> {
        Box::pin(self.update_with_control(config, None)).await
    }

    pub async fn update_with_control(
        &self,
        config: ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let previous_config = self
            .repository
            .get_by_name(&config.name)
            .await?
            .ok_or_else(|| {
                crate::Error::NotFound(format!("Instance '{}' not found", config.name))
            })?;
        let previous_connection = self.connection_cache.get(&config.name).await;

        Self::validate_config_with_ssrf_guard(&config, &self.ssrf_guard)?;

        let connection = if config.enabled && Self::requires_remote_connection(&config) {
            Some(
                Box::pin(
                    self.build_management_validated_remote_connection_with_control(
                        &config, control,
                    ),
                )
                .await?,
            )
        } else {
            None
        };

        self.repository.update(&config).await?;

        if let Some(connection) = connection {
            self.connection_cache
                .insert(config.name.clone(), connection)
                .await;
        } else {
            self.connection_cache.invalidate(&config.name).await;
        }

        if let Err(notify_error) = self.notify_change("update", &config.name).await {
            if let Err(rollback_error) = self.repository.update(&previous_config).await {
                return Err(Self::rollback_failure(
                    "update",
                    &config.name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.restore_cached_connection(&config.name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Updated provider instance: {}", config.name);
        Ok(())
    }

    pub async fn delete(&self, name: &str) -> crate::Result<()> {
        let previous_config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;
        let previous_connection = self.connection_cache.get(name).await;

        self.repository.delete(name).await?;
        self.connection_cache.invalidate(name).await;

        if let Err(notify_error) = self.notify_change("delete", name).await {
            if let Err(rollback_error) = self.repository.create(&previous_config).await {
                return Err(Self::rollback_failure(
                    "delete",
                    name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.restore_cached_connection(name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Deleted provider instance: {}", name);
        Ok(())
    }

    pub async fn enable(&self, name: &str) -> crate::Result<()> {
        Box::pin(self.enable_with_control(name, None)).await
    }

    pub async fn enable_with_control(
        &self,
        name: &str,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let mut config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;
        let previous_connection = self.connection_cache.get(name).await;

        if config.enabled {
            if Self::requires_remote_connection(&config) {
                let connection = self
                    .build_management_validated_remote_connection_with_control(&config, control);
                let connection = Box::pin(connection).await?;
                self.connection_cache
                    .insert(config.name.clone(), connection)
                    .await;
            } else {
                self.connection_cache.invalidate(&config.name).await;
            }
            if let Err(notify_error) = self.notify_change("enable", name).await {
                self.restore_cached_connection(name, previous_connection)
                    .await;
                return Err(notify_error);
            }
            tracing::info!("Enabled provider instance: {}", name);
            return Ok(());
        }

        Self::validate_config_with_ssrf_guard(&config, &self.ssrf_guard)?;

        config.enabled = true;
        if Self::requires_remote_connection(&config) {
            let connection =
                self.build_management_validated_remote_connection_with_control(&config, control);
            let connection = Box::pin(connection).await?;

            self.repository.enable(name).await?;
            self.connection_cache
                .insert(config.name.clone(), connection)
                .await;
        } else {
            self.repository.enable(name).await?;
            self.connection_cache.invalidate(&config.name).await;
        }

        if let Err(notify_error) = self.notify_change("enable", name).await {
            if let Err(rollback_error) = self.repository.disable(name).await {
                return Err(Self::rollback_failure(
                    "enable",
                    name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.restore_cached_connection(name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Enabled provider instance: {}", name);
        Ok(())
    }

    pub async fn disable(&self, name: &str) -> crate::Result<()> {
        let previous_config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;
        let previous_connection = self.connection_cache.get(name).await;

        self.repository.disable(name).await?;
        self.connection_cache.invalidate(name).await;

        if let Err(notify_error) = self.notify_change("disable", name).await {
            if let Err(rollback_error) = self.repository.enable(name).await {
                return Err(Self::rollback_failure(
                    "disable",
                    name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            if previous_config.enabled {
                self.restore_cached_connection(name, previous_connection)
                    .await;
            } else {
                self.connection_cache.invalidate(name).await;
            }
            return Err(notify_error);
        }

        tracing::info!("Disabled provider instance: {}", name);
        Ok(())
    }

    pub async fn reconnect(&self, name: &str) -> crate::Result<()> {
        Box::pin(self.reconnect_with_control(name, None)).await
    }

    pub async fn reconnect_with_control(
        &self,
        name: &str,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let previous_connection = self.connection_cache.get(name).await;
        self.connection_cache.invalidate(name).await;

        let config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;

        if !config.enabled {
            return Err(crate::Error::InvalidInput(format!(
                "Instance '{name}' is disabled; enable it before reconnecting"
            )));
        }

        if !Self::requires_remote_connection(&config) {
            return Err(crate::Error::InvalidInput(format!(
                "Instance '{name}' is local-only and does not support remote reconnect"
            )));
        }

        let connection =
            self.build_management_validated_remote_connection_with_control(&config, control);
        let connection = Box::pin(connection).await?;
        self.connection_cache
            .insert(config.name.clone(), connection)
            .await;

        if let Err(notify_error) = self.notify_change("reconnect", name).await {
            self.restore_cached_connection(name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Reconnected provider instance: {}", name);
        Ok(())
    }
}
