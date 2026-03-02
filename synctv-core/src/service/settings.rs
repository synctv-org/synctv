//! System settings service for runtime configuration management
//!
//! Provides methods for managing settings groups with change notifications
//! Uses `PostgreSQL` LISTEN/NOTIFY for hot reload across multiple replicas
//!
//! Design reference: /Volumes/workspace/rust/synctv-rs-design/19-配置管理系统.md §6.3

use dashmap::DashMap;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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
    // M-02: Lock-free cache using DashMap for concurrent reads
    cache: Arc<DashMap<String, SettingsGroup>>,
    // Change listeners
    listeners: Arc<RwLock<Vec<SettingsChangeListener>>>,
    // Broadcast channel for notifying SettingsStorage of remote reload events.
    // Payload is the setting key that was reloaded along with its new value.
    reload_sender: broadcast::Sender<(String, Option<String>)>,
    // Shared reference to registered setting providers for validation.
    // Set by `SettingsStorage` after construction via `set_providers()`.
    setting_providers: Arc<parking_lot::RwLock<Option<SettingProviders>>>,
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
        let (reload_sender, _) = broadcast::channel(256);
        Self {
            repository,
            pool,
            cache: Arc::new(DashMap::new()),
            listeners: Arc::new(RwLock::new(Vec::new())),
            reload_sender,
            setting_providers: Arc::new(parking_lot::RwLock::new(None)),
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
    /// Returns `Ok(())` if no provider is registered for the key (unknown keys
    /// are allowed through) or if the provider accepts the value.
    pub fn validate_setting(&self, key: &str, value: &str) -> Result<(), Error> {
        let providers_lock = self.setting_providers.read();
        if let Some(providers) = providers_lock.as_ref() {
            let providers_read = providers.read();
            if let Some(provider) = providers_read.get(key) {
                provider.is_valid_raw(value)?;
            }
        }
        Ok(())
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
    pub async fn get_all(&self) -> Result<Vec<SettingsGroup>, Error> {
        let mut groups: Vec<_> = self
            .cache
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        groups.sort_by(|a, b| a.group_name.cmp(&b.group_name));
        Ok(groups)
    }

    /// Get all settings as flat key-value pairs
    pub async fn get_all_values(&self) -> Result<std::collections::HashMap<String, String>, Error> {
        let settings = self.get_all().await?;
        let mut result = std::collections::HashMap::new();

        for setting in settings {
            result.insert(setting.key.clone(), setting.value.clone());
        }

        Ok(result)
    }

    /// Get a specific setting by key
    pub async fn get(&self, key: &str) -> Result<SettingsGroup, Error> {
        // Try cache first (lock-free read via DashMap)
        if let Some(setting) = self.cache.get(key) {
            return Ok(setting.value().clone());
        }

        // Not in cache, load from database
        debug!("Setting '{}' not in cache, loading from database", key);

        let setting = self
            .repository
            .get(key)
            .await
            .internal_with_err("Failed to get setting")?;

        // Update cache
        self.cache.insert(setting.key.clone(), setting.clone());

        Ok(setting)
    }

    /// Update a setting value by key
    ///
    /// Validates the value against the registered provider (if any) before
    /// persisting to the database.
    pub async fn update(&self, key: &str, value: String) -> Result<SettingsGroup, Error> {
        debug!("Updating setting '{}'", key);

        // Validate before writing to database
        self.validate_setting(key, &value)?;

        // Update in database
        let setting = self
            .repository
            .update(key, &value)
            .await
            .internal_with_err("Failed to update setting")?;

        // Update cache
        self.cache.insert(setting.key.clone(), setting.clone());

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

        let mut tx =
            self.pool.begin().await.map_err(|e| {
                Error::Internal(format!("Failed to start settings transaction: {e}"))
            })?;

        // Cross-validate contradictory room password settings.
        // Build effective values: use the batch value if present, otherwise read
        // fresh from the database **within the transaction** to prevent stale-cache
        // race conditions in multi-replica deployments.
        {
            let batch_map: std::collections::HashMap<&str, &str> = updates
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            let must_need_pwd = if let Some(v) = batch_map.get("room.room_must_need_pwd") {
                *v == "true"
            } else {
                // Read from DB within the transaction for consistency
                sqlx::query_scalar::<_, String>(
                    "SELECT value FROM settings WHERE key = $1 FOR UPDATE",
                )
                .bind("room.room_must_need_pwd")
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| Error::Internal(format!("Failed to read room_must_need_pwd: {e}")))?
                .is_some_and(|v| v == "true")
            };

            let must_no_need_pwd = if let Some(v) = batch_map.get("room.room_must_no_need_pwd") {
                *v == "true"
            } else {
                // Read from DB within the transaction for consistency
                sqlx::query_scalar::<_, String>(
                    "SELECT value FROM settings WHERE key = $1 FOR UPDATE",
                )
                .bind("room.room_must_no_need_pwd")
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| Error::Internal(format!("Failed to read room_must_no_need_pwd: {e}")))?
                .is_some_and(|v| v == "true")
            };

            if must_need_pwd && must_no_need_pwd {
                return Err(Error::InvalidInput(
                    "room_must_need_pwd and room_must_no_need_pwd cannot both be true".into(),
                ));
            }
        }

        let mut updated = Vec::with_capacity(updates.len());
        for (key, value) in &updates {
            let row = sqlx::query(
                "UPDATE settings
                 SET value = $1, version = version + 1, updated_at = NOW()
                 WHERE key = $2
                 RETURNING key, group_name, value, version, created_at, updated_at",
            )
            .bind(value.as_str())
            .bind(key.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| Error::Internal(format!("Failed to update setting '{key}': {e}")))?;

            let setting = crate::models::settings::SettingsGroup {
                key: row
                    .try_get("key")
                    .map_err(|e| Error::Internal(format!("Failed to read setting key: {e}")))?,
                group_name: row
                    .try_get("group_name")
                    .map_err(|e| Error::Internal(format!("Failed to read setting group: {e}")))?,
                value: row
                    .try_get("value")
                    .map_err(|e| Error::Internal(format!("Failed to read setting value: {e}")))?,
                version: row
                    .try_get("version")
                    .map_err(|e| Error::Internal(format!("Failed to read setting version: {e}")))?,
                created_at: row.try_get("created_at").map_err(|e| {
                    Error::Internal(format!("Failed to read setting created_at: {e}"))
                })?,
                updated_at: row.try_get("updated_at").map_err(|e| {
                    Error::Internal(format!("Failed to read setting updated_at: {e}"))
                })?,
            };
            updated.push(setting);
        }

        tx.commit()
            .await
            .map_err(|e| Error::Internal(format!("Failed to commit settings transaction: {e}")))?;

        // Update cache and notify listeners only after the transaction committed.
        for setting in &updated {
            self.cache.insert(setting.key.clone(), setting.clone());

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

    /// Get a specific setting value by key (e.g., "`server.allow_registration`")
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
                self.cache.insert(setting.key.clone(), setting.clone());

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
            Err(e) => {
                // Setting was deleted, remove from cache
                warn!(
                    "Setting '{}' not found in database (may have been deleted): {}",
                    key, e
                );
                self.cache.remove(key);

                // Notify SettingsStorage subscribers about removal
                let _ = self.reload_sender.send((key.to_string(), None));

                // Notify listeners about removal
                self.notify_listeners(key, &serde_json::json!(null)).await;

                Ok(())
            }
        }
    }
}

/// Helper to get default settings for a group
#[must_use]
pub fn get_default_settings_json(group: &str) -> Option<serde_json::Value> {
    get_default_settings(group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::settings::{
        default_email_settings, default_oauth_settings, default_server_settings,
        get_default_settings, SettingsGroup,
    };

    #[tokio::test]
    async fn test_get_default_values() {
        let allow_reg = get_default_settings_json("server")
            .and_then(|v| v.get("allow_registration").cloned())
            .and_then(|v| v.as_bool());

        assert_eq!(allow_reg, Some(true));
    }

    // ========== Default Settings Groups ==========

    #[test]
    fn test_default_server_settings() {
        let settings = default_server_settings();
        assert_eq!(settings["allow_registration"], true);
        assert_eq!(settings["allow_room_creation"], true);
        assert_eq!(settings["max_rooms_per_user"], 10);
        assert_eq!(settings["max_members_per_room"], 100);
        assert!(settings["default_room_settings"].is_object());
        assert_eq!(settings["default_room_settings"]["require_password"], false);
        assert_eq!(settings["default_room_settings"]["allow_guest"], true);
    }

    #[test]
    fn test_default_email_settings() {
        let settings = default_email_settings();
        assert_eq!(settings["enabled"], false);
        assert_eq!(settings["smtp_port"], 587);
        assert_eq!(settings["use_tls"], true);
        assert_eq!(settings["from_name"], "SyncTV");
        assert_eq!(settings["smtp_host"], "");
        assert_eq!(settings["smtp_username"], "");
    }

    #[test]
    fn test_default_oauth_settings() {
        let settings = default_oauth_settings();
        assert_eq!(settings["github_enabled"], false);
        assert_eq!(settings["google_enabled"], false);
        assert_eq!(settings["microsoft_enabled"], false);
        assert_eq!(settings["discord_enabled"], false);
    }

    #[test]
    fn test_default_rate_limit_settings() {
        let settings = get_default_settings("rate_limit").unwrap();
        assert_eq!(settings["enabled"], true);
        assert_eq!(settings["api_rate_limit"], 100);
        assert_eq!(settings["api_rate_window"], 60);
        assert_eq!(settings["ws_rate_limit"], 50);
        assert_eq!(settings["ws_rate_window"], 60);
    }

    #[test]
    fn test_default_content_moderation_settings() {
        let settings = get_default_settings("content_moderation").unwrap();
        assert_eq!(settings["enabled"], false);
        assert_eq!(settings["filter_profanity"], false);
        assert_eq!(settings["max_message_length"], 1000);
        assert_eq!(settings["link_filter_enabled"], false);
    }

    #[test]
    fn test_unknown_group_returns_none() {
        assert!(get_default_settings("nonexistent").is_none());
        assert!(get_default_settings("").is_none());
        assert!(get_default_settings_json("foobar").is_none());
    }

    // ========== SettingsGroup Model ==========

    #[test]
    fn test_settings_group_new() {
        let group = SettingsGroup::new(
            "server".to_string(),
            r#"{"allow_registration": true}"#.to_string(),
        );

        assert_eq!(group.group_name, "server");
        assert_eq!(group.key, "server.default");
        assert_eq!(group.value, r#"{"allow_registration": true}"#);
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

    // ========== Settings Serialization Round-Trip ==========

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

    // ========== Settings Value Types ==========

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

    // ========== Helper Function ==========

    #[test]
    fn test_get_default_settings_json_returns_same_as_get_default_settings() {
        for group_name in &[
            "server",
            "email",
            "oauth",
            "rate_limit",
            "content_moderation",
        ] {
            let from_helper = get_default_settings_json(group_name);
            let from_model = get_default_settings(group_name);
            assert_eq!(from_helper, from_model, "Mismatch for group: {group_name}");
        }
    }

    // ========== validate_setting tests ==========

    /// A mock `SettingProvider` that rejects any value not equal to "valid".
    struct MockProvider;

    #[async_trait::async_trait]
    impl crate::service::settings_vars::SettingProvider for MockProvider {
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
    async fn test_validate_setting_passes_unknown_keys() {
        let service = service_with_mock_provider("test.key");
        // A key with no registered provider should pass validation
        let result = service.validate_setting("unknown.key", "anything");
        assert!(result.is_ok(), "Unknown keys should pass validation");
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
            result.is_ok(),
            "Should pass when no providers are registered"
        );
    }
}
