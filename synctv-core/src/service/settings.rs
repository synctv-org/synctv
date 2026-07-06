//! System settings service for runtime configuration management
//!
//! Provides methods for managing runtime settings with change notifications
//! Uses `PostgreSQL` LISTEN/NOTIFY for hot reload across multiple replicas
//!
//! Design reference: external design doc 19-configuration-management.md §6.3

use dashmap::DashMap;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::cache::{
    CacheDomain, CacheL2Backend, ConsistencyCoordinator, FenceReadResult, NoopCacheL2,
    RuntimeSettingKey, RuntimeSettingsCache, VersionFenceStore,
};
use crate::models::settings::RuntimeSetting;
use crate::repository::SettingsRepository;
use crate::{Error, InternalExt};

/// System settings service
#[derive(Clone)]
pub struct SettingsService {
    repository: Option<SettingsRepository>,
    pool: Option<PgPool>,
    // Lock-free cache using DashMap for concurrent reads.
    cache: Arc<DashMap<String, RuntimeSetting>>,
    // Broadcast channel for notifying SettingsStorage of remote reload events.
    // Payload is the setting key that was reloaded along with its new value.
    reload_sender: broadcast::Sender<(String, Option<String>)>,
    consistency: ConsistencyCoordinator,
    runtime_cache: RuntimeSettingsCache,
}

#[derive(Clone)]
pub struct SettingsServiceRuntime {
    pub version_fence: Arc<dyn VersionFenceStore>,
    pub l2_cache: Arc<dyn CacheL2Backend>,
    pub cache_key_prefix: String,
    pub cache_max_capacity: u64,
    pub cache_ttl_secs: u64,
    pub cache_l2_ttl_secs: u64,
}

impl SettingsServiceRuntime {
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            l2_cache: Arc::new(NoopCacheL2),
            cache_key_prefix: "runtime_settings:".to_string(),
            cache_max_capacity: 512,
            cache_ttl_secs: 300,
            cache_l2_ttl_secs: 300,
        }
    }
}

fn normalize_cache_capacity(capacity: u64) -> u64 {
    capacity.max(1)
}

fn normalize_cache_ttl(ttl_seconds: u64) -> u64 {
    ttl_seconds.max(1)
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
        Self::new_with_runtime(repository, pool, SettingsServiceRuntime::local_only())
    }

    #[must_use]
    pub fn new_with_runtime(
        repository: SettingsRepository,
        pool: PgPool,
        runtime: SettingsServiceRuntime,
    ) -> Self {
        let (reload_sender, _) = broadcast::channel(256);
        let runtime_cache = RuntimeSettingsCache::new(
            runtime.l2_cache,
            normalize_cache_capacity(runtime.cache_max_capacity),
            normalize_cache_ttl(runtime.cache_ttl_secs),
            normalize_cache_ttl(runtime.cache_l2_ttl_secs),
            runtime.cache_key_prefix,
        );
        Self {
            repository: Some(repository),
            pool: Some(pool),
            cache: Arc::new(DashMap::new()),
            reload_sender,
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
            runtime_cache,
        }
    }

    #[cfg(test)]
    fn new_without_backend_for_tests(runtime: SettingsServiceRuntime) -> Self {
        let (reload_sender, _) = broadcast::channel(256);
        let runtime_cache = RuntimeSettingsCache::new(
            runtime.l2_cache,
            normalize_cache_capacity(runtime.cache_max_capacity),
            normalize_cache_ttl(runtime.cache_ttl_secs),
            normalize_cache_ttl(runtime.cache_l2_ttl_secs),
            runtime.cache_key_prefix,
        );
        Self {
            repository: None,
            pool: None,
            cache: Arc::new(DashMap::new()),
            reload_sender,
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
            runtime_cache,
        }
    }

    fn repository(&self) -> Result<&SettingsRepository, Error> {
        self.repository
            .as_ref()
            .ok_or_else(|| Error::Internal("Settings service has no repository backend".into()))
    }

    fn pool(&self) -> Result<&PgPool, Error> {
        self.pool
            .as_ref()
            .ok_or_else(|| Error::Internal("Settings service has no database pool backend".into()))
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
            .repository()?
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

    /// Get all runtime settings
    pub fn get_all(&self) -> Result<Vec<RuntimeSetting>, Error> {
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
    pub async fn get(&self, key: &str) -> Result<RuntimeSetting, Error> {
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

    /// Atomically update multiple settings in a single database transaction.
    ///
    /// All updates are committed together or rolled back if any write fails, so the
    /// settings table is never left in a partially-updated state. Cache and reload
    /// subscribers are updated only after the transaction commits successfully.
    ///
    pub(crate) async fn persist_raw_settings_batch_internal(
        &self,
        updates: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Vec<RuntimeSetting>, Error> {
        let updates: Vec<(String, String)> = updates.into_iter().collect();
        if updates.is_empty() {
            return Ok(vec![]);
        }

        let mut fences = Vec::with_capacity(updates.len());
        for (key, _) in &updates {
            let observed_version =
                i64::from(self.repository()?.current_version(key).await.map_err(|e| {
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
            self.pool()?.begin().await.map_err(|e| {
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
                crate::models::settings::RuntimeSetting,
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

        // Update cache and notify reload subscribers only after the transaction committed.
        for setting in &updated {
            self.store_cache_entry(setting.clone()).await;

            self.notify_reload_subscribers(&setting.key, Some(setting.value.clone()));

            info!("Updated setting '{}' (batch)", setting.key);
        }

        Ok(updated)
    }

    pub(crate) async fn upsert_internal_if_missing(
        &self,
        key: &str,
        value: String,
    ) -> Result<RuntimeSetting, Error> {
        let group_name = group_name_from_setting_key(key);
        let setting = sqlx::query_as!(
            RuntimeSetting,
            r#"
            INSERT INTO settings (key, group_name, value, version)
            VALUES ($1, $2, $3, 0)
            ON CONFLICT (key) DO UPDATE
            SET updated_at = settings.updated_at
            RETURNING key AS "key!",
                      group_name AS "group_name!",
                      value AS "value!",
                      version AS "version!",
                      created_at AS "created_at!",
                      updated_at AS "updated_at!"
            "#,
            key,
            group_name,
            value
        )
        .fetch_one(self.pool()?)
        .await
        .map_err(|error| {
            Error::Internal(format!("Failed to initialize setting '{key}': {error}"))
        })?;

        self.store_cache_entry(setting.clone()).await;
        self.notify_reload_subscribers(&setting.key, Some(setting.value.clone()));

        Ok(setting)
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
            let Some(pool) = pool else {
                error!("Settings listen task cannot start without a PostgreSQL pool");
                return;
            };

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

        let result = self.repository()?.get(key).await;
        self.apply_reload_result(key, result).await
    }

    async fn apply_reload_result(
        &self,
        key: &str,
        result: Result<RuntimeSetting, Error>,
    ) -> Result<(), Error> {
        match result {
            Ok(setting) => {
                // Update cache (lock-free via DashMap)
                self.store_cache_entry(setting.clone()).await;
                self.consistency
                    .repair_after_db_read(
                        &Self::runtime_setting_domain(key),
                        i64::from(setting.version),
                    )
                    .await;

                self.notify_reload_subscribers(key, Some(setting.value.clone()));

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

                match self.reload_sender.send((key.to_string(), None)) {
                    Ok(subscriber_count) => tracing::debug!(
                        key,
                        subscriber_count,
                        "Runtime setting removal notified SettingsStorage subscribers"
                    ),
                    Err(error) => {
                        tracing::debug!(
                            key,
                            error = %error,
                            "Runtime setting removal had no SettingsStorage subscribers"
                        );
                    }
                }

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

    async fn get_refresh(&self, key: &str) -> Result<RuntimeSetting, Error> {
        let setting = self
            .repository()?
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

    async fn store_cache_entry(&self, setting: RuntimeSetting) {
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

    fn notify_reload_subscribers(&self, key: &str, value: Option<String>) {
        match self.reload_sender.send((key.to_string(), value)) {
            Ok(subscriber_count) => debug!(
                key = %key,
                subscriber_count,
                "Runtime setting reload notified SettingsStorage subscribers"
            ),
            Err(error) => debug!(
                key = %key,
                error = %error,
                "Runtime setting reload had no active SettingsStorage subscribers"
            ),
        }
    }
}

fn group_name_from_setting_key(key: &str) -> String {
    key.split_once('.')
        .map_or_else(|| key.to_string(), |(group_name, _)| group_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{
        consistency::VersionFenceState, CacheDomain, LocalVersionFenceStore,
        VersionFenceReservation, VersionFenceStore,
    };
    use crate::models::settings::RuntimeSetting;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => std::panic::panic_any(context.to_string()),
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

    fn service_with_fence_store(
        store: Arc<dyn VersionFenceStore>,
    ) -> (
        SettingsService,
        broadcast::Receiver<(String, Option<String>)>,
    ) {
        let service = SettingsService::new_without_backend_for_tests(SettingsServiceRuntime {
            version_fence: store,
            ..SettingsServiceRuntime::local_only()
        });
        let receiver = service.subscribe_reloads();
        (service, receiver)
    }

    #[tokio::test]
    async fn test_committed_runtime_setting_finalizer_failure_does_not_block_cache_refresh() {
        let store = Arc::new(FailingCommitFenceStore::default());
        let (service, mut reloads) = service_with_fence_store(store.clone());
        let domain = SettingsService::runtime_setting_domain("test.key");
        let reservation = service.consistency.begin_observed_write(&domain, 0).await;
        let reservation = ok(reservation, "reservation should be created");

        service
            .finalize_committed_write_best_effort(&domain, reservation.as_ref(), 1, "test")
            .await;

        let mut setting = RuntimeSetting::new("test".to_string(), "\"fresh\"".to_string());
        setting.key = "test.key".to_string();
        setting.version = 1;
        service.store_cache_entry(setting.clone()).await;
        service.notify_reload_subscribers(&setting.key, Some(setting.value.clone()));

        let cached = some(
            service.cache.get("test.key"),
            "committed setting must be cached even if fence finalization failed",
        );
        assert_eq!(cached.value().value, "\"fresh\"");
        assert_eq!(store.commit_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(store.repair_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            ok(
                reloads.recv().await,
                "reload subscriber should receive committed setting",
            ),
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
            let reservation = service.consistency.begin_observed_write(&domain, 0).await;
            let reservation = ok(reservation, "reservation should be created");
            service
                .finalize_committed_write_best_effort(
                    &domain,
                    reservation.as_ref(),
                    i64::from(version),
                    "test_batch",
                )
                .await;

            let mut setting = RuntimeSetting::new("test".to_string(), value.to_string());
            setting.key = key.to_string();
            setting.version = version;
            service.store_cache_entry(setting.clone()).await;
            service.notify_reload_subscribers(&setting.key, Some(setting.value.clone()));
        }

        assert_eq!(
            some(
                service.cache.get("test.first"),
                "first committed setting should be cached",
            )
            .value()
            .value,
            "\"one\""
        );
        assert_eq!(
            some(
                service.cache.get("test.second"),
                "second committed setting should be cached",
            )
            .value()
            .value,
            "\"two\""
        );
        assert_eq!(store.commit_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(store.repair_attempts.load(Ordering::SeqCst), 2);
        let first = ok(
            reloads.recv().await,
            "first reload event should be delivered",
        );
        let second = ok(
            reloads.recv().await,
            "second reload event should be delivered",
        );
        assert_eq!(
            vec![first, second],
            vec![
                ("test.first".to_string(), Some("\"one\"".to_string())),
                ("test.second".to_string(), Some("\"two\"".to_string())),
            ]
        );
    }

    #[tokio::test]
    async fn test_reload_setting_preserves_cache_on_non_not_found_error() {
        let service =
            SettingsService::new_without_backend_for_tests(SettingsServiceRuntime::local_only());

        let existing = RuntimeSetting {
            key: crate::service::DefaultMaxMembersSetting::KEY.to_string(),
            group_name: group_name_from_setting_key(crate::service::DefaultMaxMembersSetting::KEY),
            value: "100".to_string(),
            version: 0,
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
        };
        service.cache.insert(existing.key.clone(), existing.clone());

        let result = service
            .apply_reload_result(
                crate::service::DefaultMaxMembersSetting::KEY,
                Err(Error::Internal(
                    "injected transient database failure".to_string(),
                )),
            )
            .await;

        assert!(
            result.is_err(),
            "database connectivity errors must not be treated as setting deletion"
        );

        let cached = some(
            service
                .cache
                .get(crate::service::DefaultMaxMembersSetting::KEY),
            "existing cache entry must be preserved on transient DB errors",
        );
        assert_eq!(cached.value().value, existing.value);
    }
}
