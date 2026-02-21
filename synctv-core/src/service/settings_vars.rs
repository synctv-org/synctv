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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::Display;
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::{debug, warn};

use super::SettingsService;
use crate::Result;

/// Type alias for validator function to reduce type complexity
type ValidatorFn<T> = Arc<dyn Fn(&T) -> Result<()> + Send + Sync>;

/// Trait for setting operations (type-erased)
///
/// This trait provides a unified interface for working with a single setting
#[async_trait::async_trait]
pub trait SettingProvider: Send + Sync {
    /// Get raw string value
    fn get_raw(&self) -> Option<String>;

    /// Set raw string value (persists to database)
    async fn set_raw(&self, value: String) -> Result<()>;

    /// Validate a raw string value
    fn is_valid_raw(&self, value: &str) -> Result<()>;
}

/// Macro to create a Setting with any type
///
/// # Example
///
/// ```text
/// let signup_enabled = setting!(bool, "server.signup_enabled", storage, true);
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
    settings_service: Arc<SettingsService>,
    setting_providers: Arc<RwLock<HashMap<String, Arc<dyn SettingProvider>>>>,
}

impl SettingsStorage {
    #[must_use] 
    pub fn new(settings_service: Arc<SettingsService>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::default())),
            settings_service,
            setting_providers: Arc::new(RwLock::new(HashMap::default())),
        }
    }

    /// Register a setting provider for a key
    fn register_provider(&self, key: &'static str, provider: Arc<dyn SettingProvider>) {
        self.setting_providers.write().insert(key.to_string(), provider);
    }

    /// Get a provider by key
    #[must_use]
    pub fn get_provider(&self, key: &str) -> Option<Arc<dyn SettingProvider>> {
        self.setting_providers.read().get(key).cloned()
    }

    /// Initialize all settings from database
    pub async fn init(&self) -> Result<()> {
        // Load all settings as flat key-value pairs
        let all_values = self
            .settings_service
            .get_all_values()
            .await
            .map_err(|e| crate::Error::Internal(format!("Failed to load settings: {e}")))?;

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
        let inner = self.inner.clone();
        let mut receiver = self.settings_service.subscribe_reloads();

        crate::spawn::spawn_monitored("settings_reload_listener", async move {
            loop {
                let recv_result = tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!("SettingsStorage reload listener cancelled");
                        break;
                    }
                    result = receiver.recv() => result,
                };

                match recv_result {
                    Ok((key, Some(value))) => {
                        inner.write().insert(key.clone(), value);
                        debug!("SettingsStorage refreshed key '{}' from remote reload", key);
                    }
                    Ok((key, None)) => {
                        inner.write().remove(&key);
                        debug!("SettingsStorage removed key '{}' from remote reload", key);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "SettingsStorage reload listener lagged by {} messages, some settings may be stale until next access",
                            n
                        );
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

    /// Set raw string value for a key, persisting to database before updating cache.
    pub async fn set_raw(&self, key: &str, value: String) -> Result<()> {
        // Persist to database first — fail fast if the write fails
        self.settings_service
            .update(key, value.clone())
            .await
            .map_err(|e| crate::Error::Internal(format!("Failed to persist setting '{key}': {e}")))?;

        // Only update in-memory cache after successful DB write
        self.inner.write().insert(key.to_string(), value);

        Ok(())
    }

    /// Validate a setting value by key
    #[must_use] 
    pub fn validate(&self, key: &str, value: &str) -> bool {
        self.get_provider(key)
            .is_none_or(|p| p.is_valid_raw(value).is_ok())
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
    _phantom: std::marker::PhantomData<T>,
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
            _phantom: std::marker::PhantomData,
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
    /// * `key` - Setting key in format "group.name" (e.g., "`server.signup_enabled`")
    /// * `storage` - Shared settings storage
    /// * `default_value` - Default value if setting doesn't exist
    pub fn new(key: &'static str, storage: Arc<SettingsStorage>, default_value: T) -> Self {
        let setting = Self {
            key,
            storage: storage.clone(),
            cache: Arc::new(RwLock::new(None)),
            raw_cache: Arc::new(RwLock::new(None)),
            default_value,
            validator: Arc::new(RwLock::new(None)),
            _phantom: std::marker::PhantomData,
        };

        // Auto-register provider
        storage.register_provider(key, Arc::new(setting.clone()));

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
            let value = new_raw
                .as_ref()
                .map_or_else(|| self.default_value.clone(), |raw| {
                    raw.parse().unwrap_or_else(|_| self.default_value.clone())
                });

            // Update both caches
            *self.cache.write() = Some(value.clone());
            *self.raw_cache.write() = new_raw;

            Ok(value)
        } else {
            // Raw value unchanged, return cached value
            let cache = self.cache.read();
            Ok(cache.as_ref().cloned().unwrap_or_else(|| self.default_value.clone()))
        }
    }

    /// Set a new value and persist to database
    pub async fn set(&self, value: T) -> Result<()> {
        // Validate if validator is set
        if let Some(validator) = self.validator.read().as_ref() {
            validator(&value)?;
        }
        // Convert to string using standard Display trait
        let str_value = value.to_string();
        self.storage.set_raw(self.key, str_value).await?;
        Ok(())
    }

    /// Validate a raw string value (for API input validation)
    pub fn is_valid_raw(&self, str_value: &str) -> Result<()> {
        let v = str_value
            .parse::<T>()
            .map_err(|_| crate::Error::InvalidInput(format!("Invalid value for setting '{}'", self.key)))?;

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
}

#[async_trait::async_trait]
impl<T> SettingProvider for Setting<T>
where
    T: Clone + Display + std::str::FromStr + Send + Sync + 'static,
    <T as std::str::FromStr>::Err: std::error::Error + Send + Sync,
{
    fn get_raw(&self) -> Option<String> {
        self.storage.get_raw(self.key)
    }

    async fn set_raw(&self, value: String) -> Result<()> {
        // Validate before setting
        self.is_valid_raw(&value)?;
        self.storage.set_raw(self.key, value).await
    }

    fn is_valid_raw(&self, value: &str) -> Result<()> {
        let v = value.parse::<T>()
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

    #[test]
    fn test_bool_conversion() {
        assert!("true".parse::<bool>().unwrap());
        assert!(!"false".parse::<bool>().unwrap());
        assert_eq!(true.to_string(), "true");
        assert_eq!(false.to_string(), "false");
    }

    #[test]
    fn test_i64_conversion() {
        assert_eq!("42".parse::<i64>().unwrap(), 42);
        assert_eq!(42.to_string(), "42");
    }

    #[test]
    fn test_string_conversion() {
        assert_eq!("hello".parse::<String>().unwrap(), "hello");
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
        // This test demonstrates using with_validator
        // In real usage, the validator would be stored with the setting
        let validator_called = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let _validator = |v: &i64| -> Result<()> {
            validator_called.store(true, std::sync::atomic::Ordering::SeqCst);
            if *v > 0 && *v <= 100 {
                Ok(())
            } else {
                Err(crate::Error::InvalidInput("Value must be between 1 and 100".into()))
            }
        };

        // The validator would be set via with_validator in actual usage
        // This is just to demonstrate the API
        assert!(!validator_called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
