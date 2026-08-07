//! Type-safe settings variables with automatic database persistence
//!
//! # Design
//!
//! - All settings share a single `Arc<RwLock<HashMap<String, String>>` for raw values
//! - Each setting has its own typed cache
//! - Type conversion via standard Rust traits (Display, `FromStr`)
//! - Reading returns cached value (synchronous, fast)
//! - Persistence is performed through complete typed `RuntimeSettings` snapshots
//!
//! # Custom Validation
//!
//! Use the `setting!` macro to define concrete setting types with their
//! key, default value, and validation rules kept in one place:
//!
//! ```text
//! use synctv_core::service::settings_vars::*;
//!
//! setting!(MaxRoomsSetting, i64, "room_creation.max_rooms_per_user", 10, |v: &i64| {
//!     if *v > 0 && *v <= 1000 {
//!         Ok(())
//!     } else {
//!         Err(anyhow::anyhow!("max_rooms must be between 1 and 1000"))
//!     }
//! });
//!
//! let max_rooms = MaxRoomsSetting::new(storage);
//! ```

use parking_lot::RwLock;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

use super::SettingsService;
use crate::Result;

/// Type alias for validator function to reduce type complexity
type ValidatorFn<T> = Arc<dyn Fn(&T) -> Result<()> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange<T> {
    pub key: &'static str,
    pub raw_value: Option<String>,
    pub value: Option<T>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingChangeError {
    #[error("setting change subscription lagged by {0} messages")]
    Lagged(u64),
    #[error("setting change subscription closed")]
    Closed,
    #[error("failed to parse setting '{key}' value: {error}")]
    Parse { key: &'static str, error: String },
}

pub struct SettingChangeReceiver<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    key: &'static str,
    receiver: broadcast::Receiver<(String, Option<String>)>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> SettingChangeReceiver<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    pub async fn recv(&mut self) -> std::result::Result<SettingChange<T>, SettingChangeError> {
        loop {
            match self.receiver.recv().await {
                Ok((key, raw_value)) if key == self.key => {
                    let value = raw_value
                        .as_deref()
                        .map(str::parse::<T>)
                        .transpose()
                        .map_err(|error| SettingChangeError::Parse {
                            key: self.key,
                            error: error.to_string(),
                        })?;
                    return Ok(SettingChange {
                        key: self.key,
                        raw_value,
                        value,
                    });
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    return Err(SettingChangeError::Lagged(count));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(SettingChangeError::Closed);
                }
            }
        }
    }
}

/// Macro to define a concrete setting type.
///
/// # Example
///
/// ```text
/// setting!(EnablePasswordSignupSetting, bool, "user.enable_password_signup", false);
/// setting!(MaxRoomsSetting, i64, "room_creation.max_rooms_per_user", 10, |v| {
///     if *v > 0 && *v <= 1000 {
///         Ok(())
///     } else {
///         Err(anyhow::anyhow!("max_rooms must be between 1 and 1000"))
///     }
/// });
/// ```
#[macro_export]
macro_rules! setting {
    ($name:ident, $type:ty, $key:literal, $default:expr) => {
        #[derive(Clone)]
        pub struct $name($crate::service::settings_vars::Setting<$type>);

        impl $name {
            pub const KEY: &'static str = $key;

            #[must_use]
            pub fn new(
                storage: std::sync::Arc<$crate::service::settings_vars::SettingsStorage>,
            ) -> Self {
                Self($crate::service::settings_vars::Setting::new(
                    Self::KEY,
                    storage,
                    $default,
                ))
            }
        }

        impl std::ops::Deref for $name {
            type Target = $crate::service::settings_vars::Setting<$type>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
    ($name:ident, $type:ty, $key:literal, $default:expr, $validator:expr) => {
        #[derive(Clone)]
        pub struct $name($crate::service::settings_vars::Setting<$type>);

        impl $name {
            pub const KEY: &'static str = $key;

            #[must_use]
            pub fn new(
                storage: std::sync::Arc<$crate::service::settings_vars::SettingsStorage>,
            ) -> Self {
                Self(
                    $crate::service::settings_vars::Setting::new(Self::KEY, storage, $default)
                        .with_validator($validator),
                )
            }
        }

        impl std::ops::Deref for $name {
            type Target = $crate::service::settings_vars::Setting<$type>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
}

/// Raw settings storage - shared across all settings
///
/// Uses `parking_lot::RwLock` (not `tokio::sync::RwLock`) because all lock-guarded
/// operations are fast, synchronous `HashMap` lookups/inserts with no `.await` points.
/// `parking_lot::RwLock` does not poison on panic, avoiding lock-poisoning errors.
#[derive(Clone)]
pub struct SettingsStorage {
    inner: Arc<RwLock<HashMap<String, String, BuildHasherDefault<DefaultHasher>>>>,
    settings_service: Option<Arc<SettingsService>>,
    change_sender: broadcast::Sender<(String, Option<String>)>,
    reload_lock: Arc<Mutex<()>>,
    mutation_revision: Arc<AtomicU64>,
}

pub(crate) struct SettingsSnapshot {
    values: HashMap<String, String, BuildHasherDefault<DefaultHasher>>,
}

impl SettingsSnapshot {
    fn get_raw(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

impl SettingsStorage {
    fn serialize_setting_value<T>(key: &str, value: &T) -> Result<String>
    where
        T: Display,
    {
        let mut output = String::new();
        fmt::write(&mut output, format_args!("{value}"))
            .map_err(|_| crate::Error::Internal(format!("Failed to serialize setting '{key}'")))?;
        Ok(output)
    }

    #[must_use]
    pub fn new(settings_service: Arc<SettingsService>) -> Self {
        let (change_sender, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(RwLock::new(HashMap::default())),
            settings_service: Some(settings_service),
            change_sender,
            reload_lock: Arc::new(Mutex::new(())),
            mutation_revision: Arc::new(AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests() -> Self {
        let (change_sender, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(RwLock::new(HashMap::default())),
            settings_service: None,
            change_sender,
            reload_lock: Arc::new(Mutex::new(())),
            mutation_revision: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn settings_service(&self) -> Result<&Arc<SettingsService>> {
        self.settings_service.as_ref().ok_or_else(|| {
            crate::Error::Internal("Settings storage has no service backend".to_string())
        })
    }

    /// Initialize all settings from database
    pub fn init(&self) -> Result<()> {
        // Load all settings as flat key-value pairs
        let all_values = self
            .settings_service()?
            .get_all_values()
            .map_err(|e| crate::Error::Internal(format!("Failed to load settings: {e}")))?;

        let mut storage = self.inner.write();
        *storage = all_values.into_iter().collect();
        self.mutation_revision.fetch_add(1, Ordering::Release);

        Ok(())
    }

    async fn reload_all_from_database(&self) -> Result<Vec<(String, Option<String>)>> {
        let _reload_guard = self.reload_lock.lock().await;
        loop {
            let observed_revision = self.mutation_revision.load(Ordering::Acquire);
            let settings = self
                .settings_service()?
                .reload_all_from_database()
                .await
                .map_err(|e| crate::Error::Internal(format!("Failed to reload settings: {e}")))?;
            let all_values: HashMap<_, _, BuildHasherDefault<DefaultHasher>> = settings
                .into_iter()
                .map(|setting| (setting.key, setting.value))
                .collect();

            if let Some(changed) = self.apply_reloaded_values(all_values, observed_revision) {
                return Ok(changed);
            }
            debug!(
                observed_revision,
                current_revision = self.mutation_revision.load(Ordering::Acquire),
                "Discarded stale runtime settings snapshot after a local commit"
            );
        }
    }

    fn apply_reloaded_values(
        &self,
        all_values: HashMap<String, String, BuildHasherDefault<DefaultHasher>>,
        observed_revision: u64,
    ) -> Option<Vec<(String, Option<String>)>> {
        let mut storage = self.inner.write();
        if self.mutation_revision.load(Ordering::Acquire) != observed_revision {
            return None;
        }

        let mut changed = Vec::new();
        for (key, value) in storage.iter() {
            if all_values.get(key) != Some(value) {
                changed.push((key.clone(), all_values.get(key).cloned()));
            }
        }
        for (key, value) in &all_values {
            if !storage.contains_key(key) {
                changed.push((key.clone(), Some(value.clone())));
            }
        }
        *storage = all_values;
        self.mutation_revision.fetch_add(1, Ordering::Release);

        Some(changed)
    }

    /// Start a background task that listens for reload events from `SettingsService`
    /// and updates the inner `HashMap` accordingly.
    ///
    /// This keeps the `SettingsStorage` in sync with remote replica changes
    /// that are propagated via `PostgreSQL` LISTEN/NOTIFY.
    ///
    /// The `cancel` token must be provided so the task can be stopped during
    /// graceful shutdown. When the token is cancelled the listener exits its
    /// loop cleanly without leaking the background task.
    pub fn start_reload_listener(&self, cancel: tokio_util::sync::CancellationToken) {
        let storage = self.clone();
        let Ok(settings_service) = self.settings_service() else {
            tracing::warn!("SettingsStorage reload listener not started: no service backend");
            return;
        };
        let mut receiver = settings_service.subscribe_reloads();

        crate::spawn::spawn_monitored("settings_reload_listener", async move {
            loop {
                let recv_result = tokio::select! {
                    () = cancel.cancelled() => {
                        debug!("SettingsStorage reload listener cancelled");
                        break;
                    }
                    result = receiver.recv() => result,
                };

                match recv_result {
                    Ok(event) => {
                        if let Err(error) = storage.reload_all_from_database().await {
                            warn!(
                                keys = ?event.keys,
                                error = %error,
                                "Failed to refresh SettingsStorage generation"
                            );
                            continue;
                        }
                        for key in event.keys {
                            let value = storage.get_raw(&key);
                            let _ = storage.change_sender.send((key, value));
                        }
                        debug!("SettingsStorage refreshed a committed settings generation");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "SettingsStorage reload listener lagged by {} messages, forcing full snapshot refresh",
                            n
                        );
                        match storage.reload_all_from_database().await {
                            Ok(changed) => {
                                for change in changed {
                                    let _ = storage.change_sender.send(change);
                                }
                            }
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    "Failed to refresh SettingsStorage after lagged notifications"
                                );
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        debug!("SettingsStorage reload channel closed, stopping listener");
                        break;
                    }
                }
            }
        });
    }

    /// Get raw string value for a key
    #[must_use]
    pub fn get_raw(&self, key: &str) -> Option<String> {
        self.inner.read().get(key).cloned()
    }

    pub(crate) fn snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            values: self.inner.read().clone(),
        }
    }

    pub fn subscribe_changes<T>(&self, key: &'static str) -> Result<SettingChangeReceiver<T>>
    where
        T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
        <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
    {
        Ok(SettingChangeReceiver {
            key,
            receiver: self.change_sender.subscribe(),
            _phantom: std::marker::PhantomData,
        })
    }

    /// Set a raw string value in memory without persisting.
    ///
    /// This is intended for tests that need to seed settings without a live
    /// database connection.
    #[cfg(test)]
    pub(crate) fn set_raw_for_test(&self, key: &str, value: String) {
        self.inner.write().insert(key.to_string(), value);
        self.mutation_revision.fetch_add(1, Ordering::Release);
    }

    /// Apply settings that were already committed by `SettingsService`.
    pub fn apply_persisted_updates(
        &self,
        updates: impl IntoIterator<Item = crate::models::settings::RuntimeSetting>,
    ) {
        let mut storage = self.inner.write();
        let mut applied = false;
        for setting in updates {
            storage.insert(setting.key, setting.value);
            applied = true;
        }
        if applied {
            self.mutation_revision.fetch_add(1, Ordering::Release);
        }
    }

    pub(crate) async fn set_raw_internal_if_missing(&self, key: &str, value: String) -> Result<()> {
        let setting = self
            .settings_service()?
            .upsert_internal_if_missing(key, value)
            .await
            .map_err(|e| {
                crate::Error::Internal(format!("Failed to initialize runtime setting '{key}': {e}"))
            })?;
        self.inner.write().insert(setting.key, setting.value);
        self.mutation_revision.fetch_add(1, Ordering::Release);
        Ok(())
    }
}

/// Type-safe setting variable with lazy loading
///
/// Generic over any type that implements:
/// - `Clone` - for copying values
/// - `Display` - for formatting to string (via `to_string()`)
/// - `std::str::FromStr` - for parsing from string
///
/// Uses `parking_lot::RwLock` for cache fields because `get()` is synchronous
/// and only performs fast in-memory operations (no `.await` while lock is held).
/// `parking_lot::RwLock` does not poison on panic.
pub struct Setting<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    key: &'static str,
    storage: Arc<SettingsStorage>,
    cache: Arc<RwLock<Option<T>>>,
    raw_cache: Arc<RwLock<Option<String>>>,
    default_value: T,
    validator: Arc<RwLock<Option<ValidatorFn<T>>>>,
}

impl<T> Clone for Setting<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            storage: self.storage.clone(),
            cache: self.cache.clone(),
            raw_cache: self.raw_cache.clone(),
            default_value: self.default_value.clone(),
            validator: self.validator.clone(),
        }
    }
}

impl<T> Setting<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    /// Create a new setting variable
    ///
    /// # Arguments
    ///
    /// * `key` - Setting key in format "group.name" (e.g., "`user.enable_password_signup`")
    /// * `storage` - Shared settings storage
    /// * `default_value` - Default value if setting doesn't exist
    pub fn new(key: &'static str, storage: Arc<SettingsStorage>, default_value: T) -> Self {
        Self {
            key,
            storage,
            cache: Arc::new(RwLock::new(None)),
            raw_cache: Arc::new(RwLock::new(None)),
            default_value,
            validator: Arc::new(RwLock::new(None)),
        }
    }

    /// Set a custom validator for this setting
    ///
    /// # Example
    ///
    /// ```text
    /// setting!(MaxRoomsSetting, i64, "room_creation.max_rooms_per_user", 10, |v: &i64| {
    ///     if *v > 0 && *v <= 1000 {
    ///         Ok(())
    ///     } else {
    ///         Err(anyhow::anyhow!("max_rooms must be between 1 and 1000"))
    ///     }
    /// });
    ///
    /// let max_rooms = MaxRoomsSetting::new(storage);
    /// ```
    #[must_use]
    pub fn with_validator<F>(self, validator: F) -> Self
    where
        F: Fn(&T) -> Result<()> + Send + Sync + 'static,
    {
        *self.validator.write() = Some(Arc::new(validator));
        self
    }

    /// Get the current value, checking for changes on every call
    pub fn get(&self) -> Result<T> {
        // Always fetch the latest raw value from storage
        let new_raw = self.storage.get_raw(self.key);
        self.get_from_raw(new_raw)
    }

    pub(crate) fn get_from_snapshot(&self, snapshot: &SettingsSnapshot) -> Result<T> {
        self.get_from_raw(snapshot.get_raw(self.key))
    }

    fn get_from_raw(&self, new_raw: Option<String>) -> Result<T> {
        // Check if we need to update cache
        let needs_update = {
            let raw_cache = self.raw_cache.read();
            match (&*raw_cache, &new_raw) {
                (Some(cached), Some(new)) => cached != new,
                (None, None) => false,
                _ => true, // One is None, one is Some
            }
        };

        if needs_update {
            // Raw value changed (or first load), re-parse
            let value = match new_raw.as_ref() {
                Some(raw) => raw.parse().map_err(|error| {
                    crate::Error::InvalidInput(format!(
                        "Invalid persisted value for setting '{}': {error}",
                        self.key
                    ))
                })?,
                None => self.default_value.clone(),
            };

            // Update both caches
            *self.cache.write() = Some(value.clone());
            *self.raw_cache.write() = new_raw;

            Ok(value)
        } else {
            if let Some(value) = self.cache.read().as_ref().cloned() {
                return Ok(value);
            }

            let value = match new_raw.as_ref() {
                Some(raw) => raw.parse().map_err(|error| {
                    crate::Error::InvalidInput(format!(
                        "Invalid persisted value for setting '{}': {error}",
                        self.key
                    ))
                })?,
                None => self.default_value.clone(),
            };
            *self.cache.write() = Some(value.clone());
            Ok(value)
        }
    }

    /// Return the setting value, creating and persisting it with `initializer` when absent.
    pub async fn get_or_initialize_with<F>(&self, initializer: F) -> Result<T>
    where
        F: FnOnce() -> T,
    {
        if self.storage.get_raw(self.key).is_some() {
            let current = self.get()?;
            if let Some(validator) = self.validator.read().as_ref() {
                validator(&current)?;
            }
            return Ok(current);
        }

        let value = initializer();
        if let Some(validator) = self.validator.read().as_ref() {
            validator(&value)?;
        }
        let str_value = SettingsStorage::serialize_setting_value(self.key, &value)?;
        self.storage
            .set_raw_internal_if_missing(self.key, str_value)
            .await?;
        self.get()
    }

    /// Set a value in memory without persisting.
    ///
    /// This is intended for tests that need to seed settings without a live
    /// database connection.
    #[cfg(test)]
    pub(crate) fn set_for_test(&self, value: &T) -> Result<()> {
        if let Some(validator) = self.validator.read().as_ref() {
            validator(value)?;
        }
        let str_value = SettingsStorage::serialize_setting_value(self.key, value)?;
        self.storage.set_raw_for_test(self.key, str_value);
        Ok(())
    }

    /// Validate a raw string value (for API input validation)
    pub fn is_valid_raw(&self, str_value: &str) -> Result<()> {
        let v = str_value.parse::<T>().map_err(|_| {
            crate::Error::InvalidInput(format!("Invalid value for setting '{}'", self.key))
        })?;

        // Run custom validator if set
        if let Some(validator) = self.validator.read().as_ref() {
            validator(&v)?;
        }

        Ok(())
    }

    /// Get the setting key
    pub const fn key(&self) -> &str {
        self.key
    }

    /// Serialize and validate a typed setting value for batch persistence.
    pub fn update_entry(&self, value: &T) -> Result<(String, String)> {
        if let Some(validator) = self.validator.read().as_ref() {
            validator(value)?;
        }
        Ok((
            self.key.to_string(),
            SettingsStorage::serialize_setting_value(self.key, value)?,
        ))
    }

    /// Subscribe to typed changes for this concrete setting.
    ///
    /// Call this before reading the initial value when the caller needs a
    /// race-free watch: the subscription receives every later write while the
    /// initial read establishes the current snapshot.
    pub fn subscribe_changes(&self) -> Result<SettingChangeReceiver<T>> {
        self.storage.subscribe_changes(self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_invalid_bool_parse() {
        // Valid bool values
        assert!("true".parse::<bool>().is_ok());
        assert!("false".parse::<bool>().is_ok());

        // Invalid bool values
        assert!("invalid".parse::<bool>().is_err());
        assert!("1".parse::<bool>().is_err()); // FromStr is strict for bool
    }

    #[test]
    fn test_invalid_i64_parse() {
        // Valid i64 values
        assert!("42".parse::<i64>().is_ok());
        assert!("-100".parse::<i64>().is_ok());

        // Invalid i64 values
        assert!("abc".parse::<i64>().is_err());
        assert!("12.34".parse::<i64>().is_err());
    }

    #[test]
    fn test_custom_validator() {
        let validator_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let storage = test_storage();

        let validator_called_clone = Arc::clone(&validator_called);
        let setting = Setting::<i64>::new("test.max_items", storage.clone(), 10).with_validator(
            move |v: &i64| -> Result<()> {
                validator_called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                if *v > 0 && *v <= 100 {
                    Ok(())
                } else {
                    Err(crate::Error::InvalidInput(
                        "Value must be between 1 and 100".into(),
                    ))
                }
            },
        );

        assert!(setting.is_valid_raw("50").is_ok());
        assert!(validator_called.load(std::sync::atomic::Ordering::SeqCst));

        validator_called.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(setting.is_valid_raw("not_a_number").is_err());
        assert!(!validator_called.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn test_storage() -> Arc<SettingsStorage> {
        Arc::new(SettingsStorage::new_for_tests())
    }

    #[test]
    fn test_bool_setting_accepts_valid_raw_values() {
        let storage = test_storage();
        let setting = Setting::<bool>::new("test.enabled", storage.clone(), true);
        assert!(setting.is_valid_raw("true").is_ok(), "Should accept 'true'");
        assert!(
            setting.is_valid_raw("false").is_ok(),
            "Should accept 'false'"
        );
    }

    #[test]
    fn test_bool_setting_rejects_invalid_raw_values() {
        let storage = test_storage();
        let setting = Setting::<bool>::new("test.enabled", storage.clone(), true);
        assert!(
            setting.is_valid_raw("not_a_bool").is_err(),
            "Should reject non-boolean"
        );
        assert!(setting.is_valid_raw("1").is_err(), "Should reject '1'");
        assert!(
            setting.is_valid_raw("").is_err(),
            "Should reject empty string"
        );
    }

    #[test]
    fn test_i64_setting_raw_validation_with_range_validator() {
        let storage = test_storage();
        let setting =
            Setting::<i64>::new("test.max_items", storage.clone(), 10).with_validator(|v: &i64| {
                if *v > 0 && *v <= 100 {
                    Ok(())
                } else {
                    Err(crate::Error::InvalidInput("Must be 1-100".into()))
                }
            });

        assert!(setting.is_valid_raw("1").is_ok(), "Should accept min bound");
        assert!(
            setting.is_valid_raw("50").is_ok(),
            "Should accept mid-range"
        );
        assert!(
            setting.is_valid_raw("100").is_ok(),
            "Should accept max bound"
        );
        assert!(setting.is_valid_raw("0").is_err(), "Should reject zero");
        assert!(
            setting.is_valid_raw("101").is_err(),
            "Should reject above max"
        );
        assert!(
            setting.is_valid_raw("-5").is_err(),
            "Should reject negative"
        );
        assert!(
            setting.is_valid_raw("abc").is_err(),
            "Should reject non-numeric"
        );
    }

    #[test]
    fn test_stale_reload_snapshot_cannot_overwrite_committed_update() {
        let storage = SettingsStorage::new_for_tests();
        let key = "room_creation.approval_required";
        storage.set_raw_for_test(key, "false".to_string());

        let observed_revision = storage.mutation_revision.load(Ordering::Acquire);
        let stale_snapshot = HashMap::from_iter([(key.to_string(), "false".to_string())]);

        storage.set_raw_for_test(key, "true".to_string());

        assert!(
            storage
                .apply_reloaded_values(stale_snapshot, observed_revision)
                .is_none(),
            "a snapshot read before the committed update must be retried"
        );
        assert_eq!(storage.get_raw(key).as_deref(), Some("true"));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_reload_all_from_database_replaces_stale_snapshot() {
        let (_pg, pool) = synctv_core_testing::create_test_pool().await;
        let repo = crate::repository::SettingsRepository::new(pool.clone());
        let service = Arc::new(SettingsService::new(repo, pool.clone()));
        let storage = Arc::new(SettingsStorage::new(service.clone()));

        ok(
            sqlx::query!(
                "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) \
                 ON CONFLICT (key) DO NOTHING",
                crate::service::RoomCreationPasswordPolicySetting::KEY,
                "room_creation",
                "required"
            )
            .execute(&pool)
            .await
            .map_err(|error| error.to_string()),
            "settings row should insert",
        );

        ok(
            service.initialize().await,
            "settings service should initialize",
        );
        ok(storage.init(), "settings storage should initialize");
        storage.inner.write().insert(
            crate::service::RoomCreationPasswordPolicySetting::KEY.to_string(),
            "optional".to_string(),
        );

        ok(
            storage.reload_all_from_database().await,
            "settings storage should reload",
        );

        assert_eq!(
            storage
                .get_raw(crate::service::RoomCreationPasswordPolicySetting::KEY)
                .as_deref(),
            Some("required"),
            "full snapshot reload must overwrite stale in-memory values"
        );
    }
}
