//! System settings service for runtime configuration management
//!
//! Provides methods for managing settings groups with change notifications
//! Uses `PostgreSQL` LISTEN/NOTIFY for hot reload across multiple replicas
//!
//! Design reference: external design doc 19-configuration-management.md §6.3

use dashmap::DashMap;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::cache::{
    CacheDomain, CacheL2Backend, ConsistencyCoordinator, FenceReadResult, NoopCacheL2,
    RuntimeSettingKey, RuntimeSettingsCache, VersionFenceStore,
};
use crate::models::settings::{get_default_settings, SettingsGroup};
use crate::repository::SettingsRepository;
use crate::service::settings_vars::SettingProvider;
use crate::{Error, InternalExt};

/// Change listener callback type
pub type SettingsChangeListener = Arc<dyn Fn(&str, &serde_json::Value) + Send + Sync>;

/// Type alias for the shared setting providers map
pub(crate) type SettingProviders =
    Arc<parking_lot::RwLock<std::collections::HashMap<String, Arc<dyn SettingProvider>>>>;

/// System settings service
#[derive(Clone)]
pub struct SettingsService {
    repository: SettingsRepository,
    pool: PgPool,
    // Lock-free cache using DashMap for concurrent reads.
    cache: Arc<DashMap<String, SettingsGroup>>,
    // Change listeners
    listeners: Arc<RwLock<Vec<SettingsChangeListener>>>,
    // Broadcast channel for notifying SettingsStorage of remote reload events.
    // Payload is the setting key that was reloaded along with its new value.
    reload_sender: broadcast::Sender<(String, Option<String>)>,
    // Shared reference to registered setting providers for validation.
    // Set by `SettingsStorage` after construction via `set_providers()`.
    setting_providers: Arc<parking_lot::RwLock<Option<SettingProviders>>>,
    consistency: ConsistencyCoordinator,
    runtime_cache: RuntimeSettingsCache,
}

#[derive(Clone)]
pub struct SettingsServiceRuntime {
    pub version_fence: Option<Arc<dyn VersionFenceStore>>,
    pub l2_cache: Option<Arc<dyn CacheL2Backend>>,
    pub cache_key_prefix: String,
    pub cache_max_capacity: u64,
    pub cache_ttl_secs: u64,
    pub cache_l2_ttl_secs: u64,
}

impl Default for SettingsServiceRuntime {
    fn default() -> Self {
        Self {
            version_fence: None,
            l2_cache: None,
            cache_key_prefix: "runtime_settings:".to_string(),
            cache_max_capacity: 512,
            cache_ttl_secs: 300,
            cache_l2_ttl_secs: 300,
        }
    }
}

impl std::fmt::Debug for SettingsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Note: We can't show cache_size here without blocking on async
        f.debug_struct("SettingsService")
            .field("repository", &std::any::type_name::<SettingsRepository>())
            .finish_non_exhaustive()
    }
}

impl SettingsService {
    #[must_use]
    pub fn new(repository: SettingsRepository, pool: PgPool) -> Self {
        Self::new_with_runtime(repository, pool, SettingsServiceRuntime::default())
    }

    #[must_use]
    pub fn new_with_runtime(
        repository: SettingsRepository,
        pool: PgPool,
        runtime: SettingsServiceRuntime,
    ) -> Self {
        let (reload_sender, _) = broadcast::channel(256);
        let version_fence = runtime
            .version_fence
            .unwrap_or_else(|| Arc::new(crate::cache::NoopVersionFenceStore));
        let runtime_cache = RuntimeSettingsCache::new(
            runtime.l2_cache.unwrap_or_else(|| Arc::new(NoopCacheL2)),
            runtime.cache_max_capacity,
            runtime.cache_ttl_secs,
            runtime.cache_l2_ttl_secs,
            runtime.cache_key_prefix,
        )
        .expect("failed to create runtime settings cache");
        Self {
            repository,
            pool,
            cache: Arc::new(DashMap::new()),
            listeners: Arc::new(RwLock::new(Vec::new())),
            reload_sender,
            setting_providers: Arc::new(parking_lot::RwLock::new(None)),
            consistency: ConsistencyCoordinator::new(version_fence),
            runtime_cache,
        }
    }

    /// Set the shared setting providers map.
    ///
    /// Called by `SettingsStorage` after construction so that `update()` can
    /// validate values before persisting them to the database.
    pub(crate) fn set_providers(&self, providers: SettingProviders) {
        *self.setting_providers.write() = Some(providers);
    }

    /// Validate a setting value using the registered provider for its key.
    ///
    /// Returns `Ok(())` only if a provider is registered for the key and the
    /// provider accepts the value.
    pub fn validate_setting(&self, key: &str, value: &str) -> Result<(), Error> {
        let providers_lock = self.setting_providers.read();
        if let Some(providers) = providers_lock.as_ref() {
            let providers_read = providers.read();
            if let Some(provider) = providers_read.get(key) {
                provider.is_valid_raw(value)?;
                return Ok(());
            }
            return Err(Error::InvalidInput(format!("Unknown setting key: {key}")));
        }
        Err(Error::Internal(
            "Settings providers are not initialized; refusing to validate update".to_string(),
        ))
    }

    /// Subscribe to reload events triggered by remote replicas.
    ///
    /// Each event is `(key, Option<new_value>)` where `None` means the key was deleted.
    #[must_use]
    pub fn subscribe_reloads(&self) -> broadcast::Receiver<(String, Option<String>)> {
        self.reload_sender.subscribe()
    }

    /// Initialize the service by loading all settings into cache
    pub async fn initialize(&self) -> Result<(), Error> {
        info!("Initializing settings service");

        let settings = self
            .repository
            .get_all()
            .await
            .map_err(|e| Error::Internal(format!("Failed to load settings: {e}")))?;

        self.cache.clear();

        for setting in settings {
            debug!(
                "Loaded setting '{}.{}' = '{}'",
                setting.group_name, setting.key, setting.value
            );
            self.cache.insert(setting.key.clone(), setting);
        }

        info!(
            "Settings service initialized with {} settings",
            self.cache.len()
        );
        Ok(())
    }

    /// Get all settings groups
    pub fn get_all(&self) -> Result<Vec<SettingsGroup>, Error> {
        let mut groups: Vec<_> = self
            .cache
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        groups.sort_by(|a, b| a.group_name.cmp(&b.group_name));
        Ok(groups)
    }

    /// Get all settings as flat key-value pairs
    pub fn get_all_values(&self) -> Result<std::collections::HashMap<String, String>, Error> {
        let settings = self.get_all()?;
        let mut result = std::collections::HashMap::new();

        for setting in settings {
            result.insert(setting.key.clone(), setting.value.clone());
        }

        Ok(result)
    }

    /// Get a specific setting by key
    pub async fn get(&self, key: &str) -> Result<SettingsGroup, Error> {
        let cache_key = RuntimeSettingKey::new(key);
        let domain = Self::runtime_setting_domain(key);

        if self.consistency.is_authoritative() {
            if let Some(fence_key) = self.consistency.fence_key(&domain) {
                match self
                    .runtime_cache
                    .get_by_fence_key(&cache_key, &fence_key)
                    .await
                {
                    Ok(FenceReadResult::Hit(setting)) => {
                        self.cache.insert(setting.key.clone(), setting.clone());
                        return Ok(setting);
                    }
                    Ok(FenceReadResult::DbFallback) => {
                        ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
                        return self.get_refresh(key).await;
                    }
                    Ok(FenceReadResult::Unsupported) => {}
                    Err(error) => {
                        warn!(
                            key = key,
                            error = %error,
                            "Runtime setting fence-key cache read failed; falling back to version read"
                        );
                        ConsistencyCoordinator::record_db_fallback(&domain, "fence_key_read_error");
                    }
                }
            }

            let fence_version = match self.consistency.current_committed_version(&domain).await {
                Ok(Some(version)) => version,
                Ok(None) => {
                    ConsistencyCoordinator::record_db_fallback(&domain, "missing_fence");
                    return self.get_refresh(key).await;
                }
                Err(error) => {
                    warn!(
                        key = key,
                        error = %error,
                        "Runtime setting version fence unavailable; bypassing cache"
                    );
                    ConsistencyCoordinator::record_db_fallback(&domain, "fence_unavailable");
                    return self.get_refresh(key).await;
                }
            };

            if let Some(setting) = self.runtime_cache.get_l1(&cache_key).await {
                if i64::from(setting.version) >= fence_version {
                    return Ok(setting);
                }
            }

            match self.runtime_cache.get_l2(&cache_key).await {
                Ok(Some(setting)) if i64::from(setting.version) >= fence_version => {
                    self.cache.insert(setting.key.clone(), setting.clone());
                    return Ok(setting);
                }
                Ok(_) => {
                    ConsistencyCoordinator::record_db_fallback(&domain, "stale_cache");
                }
                Err(error) => {
                    warn!(
                        key = key,
                        error = %error,
                        "Runtime setting L2 read failed; bypassing cache"
                    );
                    ConsistencyCoordinator::record_db_fallback(&domain, "l2_error");
                }
            }
        } else if let Some(setting) = self.cache.get(key) {
            return Ok(setting.value().clone());
        }

        // Not in cache, load from database
        debug!("Setting '{}' not in cache, loading from database", key);

        self.get_refresh(key).await
    }

    /// Update a setting value by key
    ///
    /// Validates the value against the registered provider (if any) before
    /// persisting to the database.
    pub async fn update(&self, key: &str, value: String) -> Result<SettingsGroup, Error> {
        debug!("Updating setting '{}'", key);

        // Validate before writing to database
        self.validate_setting(key, &value)?;

        let group_name = group_name_from_setting_key(key);

        let observed_version = i64::from(
            self.repository
                .current_version(key)
                .await
                .internal_with_err("Failed to read current setting version")?,
        );
        let domain = Self::runtime_setting_domain(key);
        let reservation = self
            .consistency
            .begin_observed_write(&domain, observed_version)
            .await?;
        let new_version = reservation
            .as_ref()
            .map_or(observed_version + 1, |reservation| reservation.version);

        let write_result = self
            .repository
            .upsert_with_exact_version(
                key,
                &group_name,
                &value,
                i32::try_from(observed_version).map_err(|_| {
                    Error::Internal(format!("Setting version {observed_version} exceeds i32"))
                })?,
                i32::try_from(new_version).map_err(|_| {
                    Error::Internal(format!("Setting version {new_version} exceeds i32"))
                })?,
            )
            .await
            .internal_with_err("Failed to update setting");
        let setting = match write_result {
            Ok(setting) => setting,
            Err(error) => {
                self.consistency
                    .abort_reserved_write(&domain, reservation.as_ref())
                    .await;
                return Err(error);
            }
        };
        self.finalize_committed_write_best_effort(
            &domain,
            reservation.as_ref(),
            i64::from(setting.version),
            "update",
        )
        .await;

        // Update cache
        self.store_cache_entry(setting.clone()).await;

        // Note: pg_notify('settings_changed', key) is handled by the database
        // trigger (settings_change_trigger on the settings table). No manual
        // notification is needed here; the trigger fires on UPDATE automatically.

        // Notify SettingsStorage subscribers so their inner HashMap stays in sync
        // immediately, without waiting for the PG NOTIFY round-trip.
        let _ = self
            .reload_sender
            .send((key.to_string(), Some(setting.value.clone())));

        // Notify local listeners
        let json_value: serde_json::Value = value.parse().unwrap_or_else(|e| {
            warn!(
                key = key,
                error = %e,
                "Setting value is not valid JSON, wrapping as string"
            );
            serde_json::json!(value)
        });
        self.notify_listeners(key, &json_value).await;

        info!("Updated setting '{}'", setting.key);
        Ok(setting)
    }

    /// Atomically update multiple settings in a single database transaction.
    ///
    /// All updates are committed together or rolled back if any write fails, so the
    /// settings table is never left in a partially-updated state. Cache and local
    /// listeners are updated only after the transaction commits successfully.
    ///
    /// Validates each value against its registered provider before writing.
    ///
    /// Cross-validates contradictory settings using the database (not cache) to
    /// prevent race conditions in multi-replica deployments where two replicas
    /// could simultaneously set contradictory values based on stale cache reads.
    pub async fn update_batch(
        &self,
        updates: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Vec<SettingsGroup>, Error> {
        let updates: Vec<(String, String)> = updates.into_iter().collect();
        if updates.is_empty() {
            return Ok(vec![]);
        }

        // Validate each value against its registered provider
        for (key, value) in &updates {
            self.validate_setting(key, value)?;
        }

        let mut fences = Vec::with_capacity(updates.len());
        for (key, _) in &updates {
            let observed_version =
                i64::from(self.repository.current_version(key).await.map_err(|e| {
                    Error::Internal(format!("Failed to read setting '{key}': {e}"))
                })?);
            let domain = Self::runtime_setting_domain(key);
            match self
                .consistency
                .begin_observed_write(&domain, observed_version)
                .await
            {
                Ok(reservation) => {
                    let new_version = reservation
                        .as_ref()
                        .map_or(observed_version + 1, |reservation| reservation.version);
                    fences.push((
                        key.clone(),
                        domain,
                        observed_version,
                        new_version,
                        reservation,
                    ));
                }
                Err(error) => {
                    for (_, domain, _, _, reservation) in &fences {
                        self.consistency
                            .abort_reserved_write(domain, reservation.as_ref())
                            .await;
                    }
                    return Err(error);
                }
            }
        }

        let mut tx =
            self.pool.begin().await.map_err(|e| {
                Error::Internal(format!("Failed to start settings transaction: {e}"))
            })?;

        let mut updated = Vec::with_capacity(updates.len());
        for (key, value) in &updates {
            let group_name = group_name_from_setting_key(key);
            let Some((_, _, observed_version, new_version, _)) = fences
                .iter()
                .find(|(reserved_key, _, _, _, _)| reserved_key == key)
            else {
                return Err(Error::Internal(format!(
                    "Missing reserved runtime-setting fence for {key}"
                )));
            };
            let setting = sqlx::query_as!(
                crate::models::settings::SettingsGroup,
                r#"
                 INSERT INTO settings (key, group_name, value, version)
                 VALUES ($1, $2, $3, $5)
                 ON CONFLICT (key) DO UPDATE
                 SET group_name = EXCLUDED.group_name,
                     value = EXCLUDED.value,
                     version = EXCLUDED.version,
                     updated_at = NOW()
                 WHERE settings.version = $4
                 RETURNING key, group_name, value, version, created_at, updated_at
                "#,
                key.as_str(),
                group_name,
                value.as_str(),
                i32::try_from(*observed_version).map_err(|_| {
                    Error::Internal(format!("Setting version {observed_version} exceeds i32"))
                })?,
                i32::try_from(*new_version).map_err(|_| {
                    Error::Internal(format!("Setting version {new_version} exceeds i32"))
                })?,
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| Error::Internal(format!("Failed to update setting '{key}': {e}")))?;
            let Some(setting) = setting else {
                for (_, domain, _, _, reservation) in &fences {
                    self.consistency
                        .abort_reserved_write(domain, reservation.as_ref())
                        .await;
                }
                return Err(Error::OptimisticLockConflict);
            };
            updated.push(setting);
        }

        if let Err(error) = tx.commit().await {
            for (_, domain, _, _, reservation) in &fences {
                self.consistency
                    .abort_reserved_write(domain, reservation.as_ref())
                    .await;
            }
            return Err(Error::Internal(format!(
                "Failed to commit settings transaction: {error}"
            )));
        }

        for setting in &updated {
            let Some((_, domain, _, _, reservation)) = fences
                .iter()
                .find(|(reserved_key, _, _, _, _)| reserved_key == &setting.key)
            else {
                continue;
            };
            self.finalize_committed_write_best_effort(
                domain,
                reservation.as_ref(),
                i64::from(setting.version),
                "update_batch",
            )
            .await;
        }

        // Update cache and notify listeners only after the transaction committed.
        for setting in &updated {
            self.store_cache_entry(setting.clone()).await;

            // Notify SettingsStorage subscribers so their inner HashMap stays in sync
            // immediately, without waiting for the PG NOTIFY round-trip.
            let _ = self
                .reload_sender
                .send((setting.key.clone(), Some(setting.value.clone())));

            let json_value: serde_json::Value = setting.value.parse().unwrap_or_else(|e| {
                warn!(
                    key = %setting.key,
                    error = %e,
                    "Setting value is not valid JSON, wrapping as string (batch)"
                );
                serde_json::json!(&setting.value)
            });
            self.notify_listeners(&setting.key, &json_value).await;
            info!("Updated setting '{}' (batch)", setting.key);
        }

        Ok(updated)
    }

    /// Get a specific setting value by key (e.g., "`user.enable_password_signup`")
    pub async fn get_value(&self, key: &str) -> Option<String> {
        let setting = self.get(key).await.ok()?;
        Some(setting.value)
    }

    /// Register a change listener
    pub async fn register_listener(&self, listener: SettingsChangeListener) {
        let mut listeners = self.listeners.write().await;
        listeners.push(listener);
        debug!(
            "Registered settings change listener, total: {}",
            listeners.len()
        );
    }

    /// Notify all listeners of a settings change
    async fn notify_listeners(&self, group: &str, settings_json: &serde_json::Value) {
        let listeners = self.listeners.read().await;
        if listeners.is_empty() {
            return;
        }

        debug!(
            "Notifying {} listeners of settings change in group '{}'",
            listeners.len(),
            group
        );

        for listener in listeners.iter() {
            listener(group, settings_json);
        }
    }

    /// Start `PostgreSQL` LISTEN task for hot reload
    ///
    /// Listens for '`settings_changed`' notifications and automatically reloads
    /// changed settings from database into cache.
    ///
    /// This enables hot reload across multiple replicas without restart.
    ///
    /// # Returns
    /// A `JoinHandle` for the background task
    ///
    /// # Example
    /// ```text
    /// let settings_service = SettingsService::new(repo, pool);
    /// settings_service.initialize().await?;
    /// let cancel = tokio_util::sync::CancellationToken::new();
    /// let _listen_task = settings_service.start_listen_task(cancel);
    /// ```
    #[must_use]
    pub fn start_listen_task(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        let pool = self.pool.clone();

        crate::spawn::spawn_monitored("settings_pg_listen", async move {
            info!("Starting PostgreSQL LISTEN for settings hot reload");

            loop {
                if cancel.is_cancelled() {
                    info!("Settings listen task cancelled, shutting down");
                    return;
                }

                // Create listener connection
                let mut listener = match sqlx::postgres::PgListener::connect_with(&pool).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        error!("Failed to create PgListener: {}", e);
                        tokio::select! {
                            () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                            () = cancel.cancelled() => {
                                info!("Settings listen task cancelled during reconnect backoff");
                                return;
                            }
                        }
                        continue;
                    }
                };

                // Listen to 'settings_changed' channel
                if let Err(e) = listener.listen("settings_changed").await {
                    error!("Failed to LISTEN on settings_changed: {}", e);
                    tokio::select! {
                        () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                        () = cancel.cancelled() => {
                            info!("Settings listen task cancelled during listen backoff");
                            return;
                        }
                    }
                    continue;
                }

                info!("PostgreSQL LISTEN started for settings_changed channel");

                // Process notifications using blocking recv with cancellation
                loop {
                    tokio::select! {
                        result = listener.recv() => {
                            match result {
                                Ok(notification) => {
                                    let changed_key = notification.payload();
                                    info!("Received settings change notification: {}", changed_key);

                                    // Reload the changed setting from database
                                    match service.reload_setting(changed_key).await {
                                        Ok(()) => {
                                            debug!("Successfully reloaded setting: {}", changed_key);
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to reload setting '{}': {}",
                                                changed_key, e
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Error receiving notification: {}", e);
                                    // Connection lost, break inner loop to reconnect
                                    break;
                                }
                            }
                        }
                        () = cancel.cancelled() => {
                            info!("Settings listen task cancelled");
                            return;
                        }
                    }
                }

                warn!("PostgreSQL LISTEN connection lost, reconnecting in 5 seconds...");
                tokio::select! {
                    () = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                    () = cancel.cancelled() => {
                        info!("Settings listen task cancelled during reconnect backoff");
                        return;
                    }
                }

                // Refresh cache after reconnection to catch missed notifications
                if let Err(e) = service.initialize().await {
                    error!("Failed to refresh settings cache after reconnection: {}", e);
                }
            }
        })
    }

    /// Reload a specific setting from database into cache
    ///
    /// Called when a `PostgreSQL` NOTIFY is received
    async fn reload_setting(&self, key: &str) -> Result<(), Error> {
        debug!("Reloading setting from database: {}", key);

        // Try to fetch from database
        match self.repository.get(key).await {
            Ok(setting) => {
                // Update cache (lock-free via DashMap)
                self.store_cache_entry(setting.clone()).await;
                self.consistency
                    .repair_after_db_read(
                        &Self::runtime_setting_domain(key),
                        i64::from(setting.version),
                    )
                    .await;

                // Notify SettingsStorage subscribers so their inner HashMap stays in sync
                let _ = self
                    .reload_sender
                    .send((key.to_string(), Some(setting.value.clone())));

                // Notify local listeners
                let json_value: serde_json::Value = setting.value.parse().unwrap_or_else(|e| {
                    warn!(
                        key = key,
                        error = %e,
                        "Setting value is not valid JSON, wrapping as string (reload)"
                    );
                    serde_json::json!(setting.value)
                });
                self.notify_listeners(key, &json_value).await;

                info!("Setting '{}' reloaded from database", key);
                Ok(())
            }
            Err(Error::NotFound(_)) => {
                // Setting was deleted, remove from cache
                warn!(
                    "Setting '{}' not found in database (may have been deleted)",
                    key
                );
                self.cache.remove(key);
                let cache_key = RuntimeSettingKey::new(key);
                if let Err(error) = self.runtime_cache.invalidate(&cache_key).await {
                    warn!(
                        key = key,
                        error = %error,
                        "Failed to invalidate deleted runtime setting cache"
                    );
                }

                // Notify SettingsStorage subscribers about removal
                let _ = self.reload_sender.send((key.to_string(), None));

                // Notify listeners about removal
                self.notify_listeners(key, &serde_json::json!(null)).await;

                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl SettingsService {
    fn runtime_setting_domain(key: &str) -> CacheDomain {
        CacheDomain::RuntimeSetting {
            key: key.to_string(),
        }
    }

    async fn finalize_committed_write_best_effort(
        &self,
        domain: &CacheDomain,
        reservation: Option<&crate::cache::VersionFenceReservation>,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .consistency
            .commit_reserved_write(domain, reservation, version)
            .await
        {
            warn!(
                domain = %domain,
                version,
                operation,
                error = %error,
                "Failed to finalize runtime setting version fence after committed DB write"
            );
        }
    }

    async fn get_refresh(&self, key: &str) -> Result<SettingsGroup, Error> {
        let setting = self
            .repository
            .get(key)
            .await
            .internal_with_err("Failed to get setting")?;
        self.consistency
            .repair_after_db_read(
                &Self::runtime_setting_domain(key),
                i64::from(setting.version),
            )
            .await;
        self.store_cache_entry(setting.clone()).await;
        Ok(setting)
    }

    async fn store_cache_entry(&self, setting: SettingsGroup) {
        self.cache.insert(setting.key.clone(), setting.clone());
        let cache_key = RuntimeSettingKey::new(setting.key.clone());
        if let Err(error) = self
            .runtime_cache
            .set_if_version_at_least(&cache_key, setting.clone())
            .await
        {
            warn!(
                key = %setting.key,
                version = setting.version,
                error = %error,
                "Failed to write runtime setting cache"
            );
        }
    }
}

fn group_name_from_setting_key(key: &str) -> String {
    key.split_once('.')
        .map_or_else(|| key.to_string(), |(group_name, _)| group_name.to_string())
}

/// Helper to get default settings for a group
#[must_use]
pub fn get_default_settings_json(group: &str) -> Option<serde_json::Value> {
    get_default_settings(group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{
        consistency::VersionFenceState, CacheDomain, LocalVersionFenceStore,
        VersionFenceReservation, VersionFenceStore,
    };
    use crate::models::settings::{get_default_settings, SettingsGroup};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_unknown_group_returns_none() {
        assert!(get_default_settings("nonexistent").is_none());
        assert!(get_default_settings("").is_none());
        assert!(get_default_settings_json("foobar").is_none());
    }

    #[test]
    fn test_settings_group_new() {
        let group = SettingsGroup::new(
            "server".to_string(),
            r#"{"allow_room_creation": true}"#.to_string(),
        );

        assert_eq!(group.group_name, "server");
        assert_eq!(group.key, "server.default");
        assert_eq!(group.value, r#"{"allow_room_creation": true}"#);
    }

    #[test]
    fn test_settings_group_parse_json() {
        let group = SettingsGroup::new(
            "test".to_string(),
            serde_json::json!({"key": "value", "count": 42}).to_string(),
        );

        let parsed = group.parse_json().unwrap();
        assert_eq!(parsed["key"], "value");
        assert_eq!(parsed["count"], 42);
    }

    #[test]
    fn test_settings_group_parse_json_invalid() {
        let group = SettingsGroup::new("test".to_string(), "not valid json {{{".to_string());

        assert!(group.parse_json().is_err());
    }

    #[test]
    fn test_settings_group_as_object() {
        let group = SettingsGroup::new(
            "test".to_string(),
            serde_json::json!({"a": 1, "b": "two"}).to_string(),
        );

        let obj = group.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("a"));
        assert!(obj.contains_key("b"));
    }

    #[test]
    fn test_settings_group_as_object_not_object() {
        let group =
            SettingsGroup::new("test".to_string(), serde_json::json!([1, 2, 3]).to_string());

        assert!(group.as_object().is_err());
    }

    #[test]
    fn test_settings_group_as_object_string_value() {
        let group = SettingsGroup::new("test".to_string(), r#""just a string""#.to_string());

        assert!(group.as_object().is_err());
    }

    #[test]
    fn test_settings_group_serialization() {
        let group = SettingsGroup::new(
            "server".to_string(),
            serde_json::json!({"test": true}).to_string(),
        );

        let json = serde_json::to_string(&group).unwrap();
        let deserialized: SettingsGroup = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.group_name, group.group_name);
        assert_eq!(deserialized.key, group.key);
        assert_eq!(deserialized.value, group.value);
    }

    #[test]
    fn test_settings_boolean_values() {
        let group = SettingsGroup::new(
            "test".to_string(),
            serde_json::json!({"enabled": true}).to_string(),
        );

        let parsed = group.parse_json().unwrap();
        assert_eq!(parsed["enabled"].as_bool(), Some(true));
    }

    #[test]
    fn test_settings_numeric_values() {
        let group = SettingsGroup::new(
            "test".to_string(),
            serde_json::json!({"port": 8080, "timeout": 30.5}).to_string(),
        );

        let parsed = group.parse_json().unwrap();
        assert_eq!(parsed["port"].as_i64(), Some(8080));
        assert_eq!(parsed["timeout"].as_f64(), Some(30.5));
    }

    #[test]
    fn test_settings_nested_values() {
        let group = SettingsGroup::new(
            "server".to_string(),
            serde_json::json!({
                "database": {
                    "host": "localhost",
                    "port": 5432,
                    "pool": {"max": 10, "min": 2}
                }
            })
            .to_string(),
        );

        let parsed = group.parse_json().unwrap();
        assert_eq!(parsed["database"]["host"], "localhost");
        assert_eq!(parsed["database"]["port"], 5432);
        assert_eq!(parsed["database"]["pool"]["max"], 10);
    }

    /// A mock `SettingProvider` that rejects any value not equal to "valid".
    struct MockProvider;

    #[async_trait::async_trait]
    impl crate::service::settings_vars::SettingProvider for MockProvider {
        fn key(&self) -> &'static str {
            "test.mock"
        }

        fn default_raw(&self) -> String {
            "valid".to_string()
        }

        fn get_raw(&self) -> Option<String> {
            Some("valid".to_string())
        }
        async fn set_raw(&self, _value: String) -> crate::Result<()> {
            Ok(())
        }
        fn is_valid_raw(&self, value: &str) -> crate::Result<()> {
            if value == "valid" {
                Ok(())
            } else {
                Err(crate::Error::InvalidInput(
                    "Only 'valid' is accepted".into(),
                ))
            }
        }
    }

    #[derive(Debug, Default)]
    struct FailingCommitFenceStore {
        state: LocalVersionFenceStore,
        commit_attempts: AtomicUsize,
        repair_attempts: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl VersionFenceStore for FailingCommitFenceStore {
        async fn current_version(&self, domain: &CacheDomain) -> crate::Result<Option<i64>> {
            self.state.current_version(domain).await
        }

        async fn current_state(
            &self,
            domain: &CacheDomain,
        ) -> crate::Result<Option<VersionFenceState>> {
            self.state.current_state(domain).await
        }

        async fn current_versions(
            &self,
            domains: &[CacheDomain],
        ) -> crate::Result<Vec<Option<i64>>> {
            self.state.current_versions(domains).await
        }

        async fn bump_version(&self, domain: &CacheDomain) -> crate::Result<i64> {
            self.state.bump_version(domain).await
        }

        async fn set_version_at_least(
            &self,
            _domain: &CacheDomain,
            _version: i64,
        ) -> crate::Result<i64> {
            self.repair_attempts.fetch_add(1, Ordering::SeqCst);
            Err(crate::Error::Timeout(
                "injected runtime setting fence repair failure".to_string(),
            ))
        }

        async fn reserve_next_after_observed_version(
            &self,
            domain: &CacheDomain,
            observed_version: i64,
        ) -> crate::Result<i64> {
            self.state
                .reserve_next_after_observed_version(domain, observed_version)
                .await
        }

        async fn begin_write(
            &self,
            domain: &CacheDomain,
            observed_version: i64,
        ) -> crate::Result<VersionFenceReservation> {
            self.state.begin_write(domain, observed_version).await
        }

        async fn commit_write(
            &self,
            _domain: &CacheDomain,
            _reservation: &VersionFenceReservation,
        ) -> crate::Result<i64> {
            self.commit_attempts.fetch_add(1, Ordering::SeqCst);
            Err(crate::Error::Timeout(
                "injected runtime setting fence commit failure".to_string(),
            ))
        }

        async fn abort_write(
            &self,
            domain: &CacheDomain,
            reservation: &VersionFenceReservation,
        ) -> crate::Result<()> {
            self.state.abort_write(domain, reservation).await
        }

        fn is_authoritative(&self) -> bool {
            true
        }
    }

    /// Helper to build a `SettingsService` with a mock provider registered.
    /// Uses a fake pool URL that is never actually connected (no DB needed).
    fn service_with_mock_provider(key: &str) -> SettingsService {
        // We cannot easily construct a PgPool without a running DB, but
        // validate_setting is purely in-memory. We can build the providers
        // map directly.
        let providers: SettingProviders =
            Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));
        providers.write().insert(
            key.to_string(),
            Arc::new(MockProvider) as Arc<dyn crate::service::settings_vars::SettingProvider>,
        );

        // Build a minimal SettingsService (pool will never be used).
        // Safety: we use the lazy pool option from sqlx which won't connect
        // until a query is executed.
        let pool_opts = sqlx::postgres::PgPoolOptions::new().max_connections(1);
        let pool = pool_opts
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .unwrap();
        let repo = crate::repository::SettingsRepository::new(pool.clone());
        let service = SettingsService::new(repo, pool);
        service.set_providers(providers);
        service
    }

    fn service_with_fence_store(
        store: Arc<dyn VersionFenceStore>,
    ) -> (
        SettingsService,
        broadcast::Receiver<(String, Option<String>)>,
    ) {
        let pool_opts = sqlx::postgres::PgPoolOptions::new().max_connections(1);
        let pool = pool_opts
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .unwrap();
        let repo = crate::repository::SettingsRepository::new(pool.clone());
        let service = SettingsService::new_with_runtime(
            repo,
            pool,
            SettingsServiceRuntime {
                version_fence: Some(store),
                ..SettingsServiceRuntime::default()
            },
        );
        let receiver = service.subscribe_reloads();
        (service, receiver)
    }

    #[tokio::test]
    async fn test_validate_setting_rejects_invalid_value() {
        let service = service_with_mock_provider("test.key");
        let result = service.validate_setting("test.key", "invalid");
        assert!(result.is_err(), "Should reject invalid values");
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::Error::InvalidInput(_)),
            "Error should be InvalidInput, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_validate_setting_accepts_valid_value() {
        let service = service_with_mock_provider("test.key");
        let result = service.validate_setting("test.key", "valid");
        assert!(result.is_ok(), "Should accept valid values");
    }

    #[tokio::test]
    async fn test_committed_runtime_setting_finalizer_failure_does_not_block_cache_refresh() {
        let store = Arc::new(FailingCommitFenceStore::default());
        let (service, mut reloads) = service_with_fence_store(store.clone());
        let domain = SettingsService::runtime_setting_domain("test.key");
        let reservation = service
            .consistency
            .begin_observed_write(&domain, 0)
            .await
            .expect("reservation should be created");

        service
            .finalize_committed_write_best_effort(&domain, reservation.as_ref(), 1, "test")
            .await;

        let mut setting = SettingsGroup::new("test".to_string(), "\"fresh\"".to_string());
        setting.key = "test.key".to_string();
        setting.version = 1;
        service.store_cache_entry(setting.clone()).await;
        let _ = service
            .reload_sender
            .send((setting.key.clone(), Some(setting.value.clone())));

        let cached = service
            .cache
            .get("test.key")
            .expect("committed setting must be cached even if fence finalization failed");
        assert_eq!(cached.value().value, "\"fresh\"");
        assert_eq!(store.commit_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(store.repair_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            reloads
                .recv()
                .await
                .expect("reload subscriber should receive committed setting"),
            ("test.key".to_string(), Some("\"fresh\"".to_string()))
        );
    }

    #[tokio::test]
    async fn test_batch_runtime_setting_finalizer_failure_does_not_block_other_refreshes() {
        let store = Arc::new(FailingCommitFenceStore::default());
        let (service, mut reloads) = service_with_fence_store(store.clone());

        for (key, value, version) in [
            ("test.first", "\"one\"", 1_i32),
            ("test.second", "\"two\"", 1_i32),
        ] {
            let domain = SettingsService::runtime_setting_domain(key);
            let reservation = service
                .consistency
                .begin_observed_write(&domain, 0)
                .await
                .expect("reservation should be created");
            service
                .finalize_committed_write_best_effort(
                    &domain,
                    reservation.as_ref(),
                    i64::from(version),
                    "test_batch",
                )
                .await;

            let mut setting = SettingsGroup::new("test".to_string(), value.to_string());
            setting.key = key.to_string();
            setting.version = version;
            service.store_cache_entry(setting.clone()).await;
            let _ = service
                .reload_sender
                .send((setting.key.clone(), Some(setting.value.clone())));
        }

        assert_eq!(
            service
                .cache
                .get("test.first")
                .expect("first committed setting should be cached")
                .value()
                .value,
            "\"one\""
        );
        assert_eq!(
            service
                .cache
                .get("test.second")
                .expect("second committed setting should be cached")
                .value()
                .value,
            "\"two\""
        );
        assert_eq!(store.commit_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(store.repair_attempts.load(Ordering::SeqCst), 2);
        let first = reloads
            .recv()
            .await
            .expect("first reload event should be delivered");
        let second = reloads
            .recv()
            .await
            .expect("second reload event should be delivered");
        assert_eq!(
            vec![first, second],
            vec![
                ("test.first".to_string(), Some("\"one\"".to_string())),
                ("test.second".to_string(), Some("\"two\"".to_string())),
            ]
        );
    }

    #[tokio::test]
    async fn test_validate_setting_rejects_unknown_keys() {
        let service = service_with_mock_provider("test.key");
        let result = service.validate_setting("unknown.key", "anything");
        assert!(result.is_err(), "Unknown keys must be rejected");
    }

    #[tokio::test]
    async fn test_validate_setting_no_providers_set() {
        // Build a SettingsService without any providers wired up
        let pool_opts = sqlx::postgres::PgPoolOptions::new().max_connections(1);
        let pool = pool_opts
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .unwrap();
        let repo = crate::repository::SettingsRepository::new(pool.clone());
        let service = SettingsService::new(repo, pool);
        // No set_providers call - providers is None

        let result = service.validate_setting("any.key", "any_value");
        assert!(
            result.is_err(),
            "Validation must fail closed when providers are not registered"
        );
    }

    #[tokio::test]
    async fn test_reload_setting_preserves_cache_on_non_not_found_error() {
        let pool_opts = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(1));
        let pool = pool_opts
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .unwrap();
        let repo = crate::repository::SettingsRepository::new(pool.clone());
        let service = SettingsService::new(repo, pool);

        let existing = SettingsGroup::new(
            "server".to_string(),
            serde_json::json!({"allow_room_creation": true}).to_string(),
        );
        service.cache.insert(existing.key.clone(), existing.clone());

        let result = service.reload_setting("server.default").await;

        assert!(
            result.is_err(),
            "database connectivity errors must not be treated as setting deletion"
        );

        let cached = service
            .cache
            .get("server.default")
            .expect("existing cache entry must be preserved on transient DB errors");
        assert_eq!(cached.value().value, existing.value);
    }
}
