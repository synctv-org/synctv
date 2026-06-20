//! Type-safe settings variables with automatic database persistence
//!
//! # Design
//!
//! - All settings share a single `Arc<RwLock<HashMap<String, String>>` for raw values
//! - Each setting has its own typed cache
//! - Type conversion via standard Rust traits (Display, `FromStr`)
//! - Reading returns cached value (synchronous, fast)
//! - Writing saves to storage + database (async)
//!
//! # Custom Validation
//!
//! Use `with_validator` to add custom validation logic:
//!
//! ```text
//! use synctv_core::service::settings_vars::*;
//!
//! let max_rooms = setting!(i64, "server.max_rooms", storage, 10)
//!     .with_validator(|v| {
//!         if *v > 0 && *v <= 1000 {
//!             Ok(())
//!         } else {
//!             Err(anyhow::anyhow!("max_rooms must be between 1 and 1000"))
//!         }
//!     });
//! ```

use parking_lot::RwLock;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use tokio::sync::broadcast;
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

/// Trait for setting operations (type-erased)
///
/// This trait provides a unified interface for working with a single setting
#[async_trait::async_trait]
pub trait SettingProvider: Send + Sync {
    /// Stable setting key in `group.name` format.
    fn key(&self) -> &str;

    /// Default raw value serialized using the setting's display representation.
    fn default_raw(&self) -> Result<String>;

    /// Get raw string value
    fn get_raw(&self) -> Option<String>;

    /// Set raw string value (persists to database)
    async fn set_raw(&self, value: String) -> Result<()>;

    /// Whether user/admin initiated settings updates may modify this key.
    fn user_writable(&self) -> bool {
        true
    }

    /// Whether the setting should be projected through user/admin settings APIs.
    fn user_visible(&self) -> bool {
        true
    }

    /// Validate a raw string value
    fn is_valid_raw(&self, value: &str) -> Result<()>;
}

/// Macro to create a Setting with any type
///
/// # Example
///
/// ```text
/// let enable_password_signup = setting!(bool, "user.enable_password_signup", storage, false);
/// let max_rooms = setting!(i64, "server.max_rooms", storage, 10);
/// let max_rooms_with_validator = setting!(i64, "server.max_rooms", storage, 10, |v| {
///     if *v > 0 && *v <= 1000 {
///         Ok(())
///     } else {
///         Err(anyhow::anyhow!("max_rooms must be between 1 and 1000"))
///     }
/// });
/// ```
#[macro_export]
macro_rules! setting {
    // Without validator
    ($type:ty, $key:expr, $storage:expr, $default:expr) => {
        $crate::service::settings_vars::Setting::new($key, $storage, $default)
    };
    // With validator
    ($type:ty, $key:expr, $storage:expr, $default:expr, $validator:expr) => {
        $crate::service::settings_vars::Setting::new($key, $storage, $default)
            .with_validator($validator)
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
    setting_providers: Arc<RwLock<HashMap<String, Arc<dyn SettingProvider>>>>,
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
        let setting_providers = settings_service.providers();
        Self {
            inner: Arc::new(RwLock::new(HashMap::default())),
            settings_service: Some(settings_service),
            setting_providers,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_provider_map(
        setting_providers: Arc<RwLock<HashMap<String, Arc<dyn SettingProvider>>>>,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::default())),
            settings_service: None,
            setting_providers,
        }
    }

    /// Register a setting provider for a key
    fn register_provider(&self, key: &'static str, provider: Arc<dyn SettingProvider>) {
        self.setting_providers
            .write()
            .insert(key.to_string(), provider);
    }

    /// Get a provider by key
    #[must_use]
    pub fn get_provider(&self, key: &str) -> Option<Arc<dyn SettingProvider>> {
        self.setting_providers.read().get(key).cloned()
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

        Ok(())
    }

    fn reload_all_from_service(&self) -> Result<()> {
        let all_values = self
            .settings_service()?
            .get_all_values()
            .map_err(|e| crate::Error::Internal(format!("Failed to reload settings: {e}")))?;

        let mut storage = self.inner.write();
        *storage = all_values.into_iter().collect();

        Ok(())
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
                    Ok((key, Some(value))) => {
                        storage.inner.write().insert(key.clone(), value);
                        debug!("SettingsStorage refreshed key '{}' from remote reload", key);
                    }
                    Ok((key, None)) => {
                        storage.inner.write().remove(&key);
                        debug!("SettingsStorage removed key '{}' from remote reload", key);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "SettingsStorage reload listener lagged by {} messages, forcing full snapshot refresh",
                            n
                        );
                        if let Err(error) = storage.reload_all_from_service() {
                            warn!(
                                error = %error,
                                "Failed to refresh SettingsStorage after lagged notifications"
                            );
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

    pub fn subscribe_changes<T>(&self, key: &'static str) -> Result<SettingChangeReceiver<T>>
    where
        T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
        <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
    {
        Ok(SettingChangeReceiver {
            key,
            receiver: self.settings_service()?.subscribe_reloads(),
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
    }

    /// Set raw string value for a key, persisting to database before updating cache.
    pub async fn set_raw(&self, key: &str, value: String) -> Result<()> {
        // Persist to database first — fail fast if the write fails.
        self.settings_service()?
            .update(key, value.clone())
            .await
            .map_err(|e| {
                crate::Error::Internal(format!("Failed to persist setting '{key}': {e}"))
            })?;

        // Only update in-memory cache after successful DB write
        self.inner.write().insert(key.to_string(), value);

        Ok(())
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
        Ok(())
    }

    /// Validate a setting value by key
    #[must_use]
    pub fn validate(&self, key: &str, value: &str) -> bool {
        self.get_provider(key)
            .is_some_and(|p| p.is_valid_raw(value).is_ok())
    }

    /// List all registered settings together with their default raw values.
    ///
    /// The returned vector is sorted by key to keep admin/API output stable.
    pub fn registered_defaults(&self) -> Result<Vec<(String, String)>> {
        let mut entries: Vec<(String, String)> = self
            .setting_providers
            .read()
            .values()
            .filter(|provider| provider.user_visible())
            .map(|provider| {
                provider
                    .default_raw()
                    .map(|raw| (provider.key().to_string(), raw))
            })
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(entries)
    }

    /// List all registered keys, including runtime-managed hidden settings.
    #[must_use]
    pub fn registered_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .setting_providers
            .read()
            .values()
            .map(|provider| provider.key().to_string())
            .collect();
        keys.sort();
        keys
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
    user_writable: Arc<RwLock<bool>>,
    user_visible: Arc<RwLock<bool>>,
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
            user_writable: self.user_writable.clone(),
            user_visible: self.user_visible.clone(),
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
        let setting = Self {
            key,
            storage,
            cache: Arc::new(RwLock::new(None)),
            raw_cache: Arc::new(RwLock::new(None)),
            default_value,
            validator: Arc::new(RwLock::new(None)),
            user_writable: Arc::new(RwLock::new(true)),
            user_visible: Arc::new(RwLock::new(true)),
        };

        // Auto-register provider
        setting
            .storage
            .register_provider(key, Arc::new(setting.clone()));

        setting
    }

    /// Set a custom validator for this setting
    ///
    /// # Example
    ///
    /// ```text
    /// let max_rooms = setting!(i64, "server.max_rooms", storage, 10)
    ///     .with_validator(|v| {
    ///         if *v > 0 && *v <= 1000 {
    ///             Ok(())
    ///         } else {
    ///             Err(anyhow::anyhow!("max_rooms must be between 1 and 1000"))
    ///         }
    ///     });
    /// ```
    #[must_use]
    pub fn with_validator<F>(self, validator: F) -> Self
    where
        F: Fn(&T) -> Result<()> + Send + Sync + 'static,
    {
        *self.validator.write() = Some(Arc::new(validator));
        self
    }

    /// Mark the setting as writable only by runtime internals.
    #[must_use]
    pub fn with_user_updates_disabled(self) -> Self {
        *self.user_writable.write() = false;
        self
    }

    /// Hide the setting from user/admin settings projection APIs.
    #[must_use]
    pub fn hidden_from_user_projection(self) -> Self {
        *self.user_visible.write() = false;
        self
    }

    /// Get the current value, checking for changes on every call
    pub fn get(&self) -> Result<T> {
        // Always fetch the latest raw value from storage
        let new_raw = self.storage.get_raw(self.key);

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

    /// Set a new value and persist to database
    pub async fn set(&self, value: T) -> Result<()> {
        // Validate if validator is set
        if let Some(validator) = self.validator.read().as_ref() {
            validator(&value)?;
        }
        // Convert to string using standard Display trait
        let str_value = SettingsStorage::serialize_setting_value(self.key, &value)?;
        self.storage.set_raw(self.key, str_value).await?;
        Ok(())
    }

    /// Return the setting value, creating and persisting it with `initializer` when absent.
    ///
    /// This bypasses `user_writable` because it is intended for runtime-owned
    /// settings that are initialized by the server itself.
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

    /// Subscribe to typed changes for this concrete setting.
    ///
    /// Call this before reading the initial value when the caller needs a
    /// race-free watch: the subscription receives every later write while the
    /// initial read establishes the current snapshot.
    pub fn subscribe_changes(&self) -> Result<SettingChangeReceiver<T>> {
        self.storage.subscribe_changes(self.key)
    }
}

#[async_trait::async_trait]
impl<T> SettingProvider for Setting<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    fn key(&self) -> &str {
        self.key
    }

    fn default_raw(&self) -> Result<String> {
        SettingsStorage::serialize_setting_value(self.key, &self.default_value)
    }

    fn get_raw(&self) -> Option<String> {
        self.storage.get_raw(self.key)
    }

    async fn set_raw(&self, value: String) -> Result<()> {
        // Validate before setting
        self.is_valid_raw(&value)?;
        self.storage.set_raw(self.key, value).await
    }

    fn user_writable(&self) -> bool {
        *self.user_writable.read()
    }

    fn user_visible(&self) -> bool {
        *self.user_visible.read()
    }

    fn is_valid_raw(&self, value: &str) -> Result<()> {
        let v = value
            .parse::<T>()
            .map_err(|e| crate::Error::InvalidInput(format!("Invalid setting value: {e}")))?;

        // Run custom validator if set
        if let Some(validator) = self.validator.read().as_ref() {
            validator(&v)?;
        }

        Ok(())
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
    fn test_bool_conversion() {
        assert!(ok("true".parse::<bool>(), "true should parse as bool"));
        assert!(!ok("false".parse::<bool>(), "false should parse as bool"));
        assert_eq!(true.to_string(), "true");
        assert_eq!(false.to_string(), "false");
    }

    #[test]
    fn test_i64_conversion() {
        assert_eq!(ok("42".parse::<i64>(), "integer should parse"), 42);
        assert_eq!(42.to_string(), "42");
    }

    #[test]
    fn test_string_conversion() {
        assert_eq!(
            ok("hello".parse::<String>(), "string should parse"),
            "hello"
        );
        assert_eq!("world".to_string(), "world");
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
        let harness = test_storage_harness();
        let storage = harness.storage;

        let validator_called_clone = Arc::clone(&validator_called);
        let _setting = Setting::<i64>::new("test.max_items", storage.clone(), 10).with_validator(
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

        assert!(storage.validate("test.max_items", "50"));
        assert!(validator_called.load(std::sync::atomic::Ordering::SeqCst));

        validator_called.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!storage.validate("test.max_items", "not_a_number"));
        assert!(!validator_called.load(std::sync::atomic::Ordering::SeqCst));
    }

    struct TestStorageHarness {
        storage: Arc<SettingsStorage>,
    }

    fn test_storage_harness() -> TestStorageHarness {
        let providers = Arc::new(RwLock::new(HashMap::default()));
        let storage = Arc::new(SettingsStorage::new_with_provider_map(providers));

        TestStorageHarness { storage }
    }

    #[test]
    fn test_storage_validate_bool_setting_accepts_valid() {
        let harness = test_storage_harness();
        let storage = harness.storage;
        let _setting = Setting::<bool>::new("test.enabled", storage.clone(), true);
        assert!(
            storage.validate("test.enabled", "true"),
            "Should accept 'true'"
        );
        assert!(
            storage.validate("test.enabled", "false"),
            "Should accept 'false'"
        );
    }

    #[test]
    fn test_storage_validate_bool_setting_rejects_invalid() {
        let harness = test_storage_harness();
        let storage = harness.storage;
        let _setting = Setting::<bool>::new("test.enabled", storage.clone(), true);
        assert!(
            !storage.validate("test.enabled", "not_a_bool"),
            "Should reject non-boolean"
        );
        assert!(!storage.validate("test.enabled", "1"), "Should reject '1'");
        assert!(
            !storage.validate("test.enabled", ""),
            "Should reject empty string"
        );
    }

    #[test]
    fn test_storage_validate_i64_setting_with_range_validator() {
        let harness = test_storage_harness();
        let storage = harness.storage;
        let _setting =
            Setting::<i64>::new("test.max_items", storage.clone(), 10).with_validator(|v: &i64| {
                if *v > 0 && *v <= 100 {
                    Ok(())
                } else {
                    Err(crate::Error::InvalidInput("Must be 1-100".into()))
                }
            });

        // Valid values
        assert!(
            storage.validate("test.max_items", "1"),
            "Should accept min bound"
        );
        assert!(
            storage.validate("test.max_items", "50"),
            "Should accept mid-range"
        );
        assert!(
            storage.validate("test.max_items", "100"),
            "Should accept max bound"
        );

        // Invalid values - out of range
        assert!(
            !storage.validate("test.max_items", "0"),
            "Should reject zero"
        );
        assert!(
            !storage.validate("test.max_items", "101"),
            "Should reject above max"
        );
        assert!(
            !storage.validate("test.max_items", "-5"),
            "Should reject negative"
        );

        // Invalid values - not a number
        assert!(
            !storage.validate("test.max_items", "abc"),
            "Should reject non-numeric"
        );
    }

    #[test]
    fn test_storage_validate_unknown_key_rejects() {
        let harness = test_storage_harness();
        let storage = harness.storage;
        assert!(
            !storage.validate("unknown.key", "anything"),
            "unknown keys should fail validation"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_reload_all_from_service_replaces_stale_snapshot() {
        let (_pg, pool) = synctv_core_testing::create_test_pool().await;
        let repo = crate::repository::SettingsRepository::new(pool.clone());
        let service = Arc::new(SettingsService::new(repo, pool.clone()));
        let storage = Arc::new(SettingsStorage::new(service.clone()));

        ok(
            sqlx::query!(
                "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) \
                 ON CONFLICT (key) DO NOTHING",
                "room.password_policy",
                "room",
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
        storage
            .inner
            .write()
            .insert("room.password_policy".to_string(), "optional".to_string());

        ok(
            storage.reload_all_from_service(),
            "settings storage should reload",
        );

        assert_eq!(
            storage.get_raw("room.password_policy").as_deref(),
            Some("required"),
            "full snapshot reload must overwrite stale in-memory values"
        );
    }
}
