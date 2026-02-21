//! `OAuth2` configuration loader (`LoadModuleConfig` pattern)
//!
//! Similar to Go's `ModuleConfigLoader` from sealos-state-metric

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

/// Configuration loader
///
/// Implements the Go `LoadModuleConfig` pattern:
/// 1. Parse full JSON config
/// 2. Navigate to section (e.g., "oauth2.github")
/// 3. Decode section directly into provider's config struct
///
/// Similar to `ModuleConfigLoader.LoadModuleConfig()` in sealos-state-metric
pub struct ConfigLoader {
    /// Full parsed YAML config
    raw_config: HashMap<String, serde_json::Value>,
}

impl ConfigLoader {
    /// Create a new empty loader
    #[must_use] 
    pub fn new() -> Self {
        Self {
            raw_config: HashMap::new(),
        }
    }

    /// Load configuration from JSON file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .context("Failed to read config file")?;

        let raw_config: HashMap<String, serde_json::Value> = serde_json::from_str(&content)
            .context("Failed to parse JSON config")?;

        info!("Loaded OAuth2 configuration from file");

        Ok(Self { raw_config })
    }

    /// Load provider section from config
    ///
    /// This is the core method that implements the `LoadModuleConfig` pattern:
    /// - Navigate to provider section (e.g., "oauth2.github")
    /// - Decode directly into provider's config struct
    ///
    /// Similar to Go's:
    /// ```go
    /// func (l *ModuleConfigLoader) LoadModuleConfig(moduleKey string, target any) error
    /// ```
    ///
    /// # Example
    ///
    /// ```text
    /// let github_config: GitHubConfig = loader.load_section("oauth2.github")?;
    /// ```
    pub fn load_section<T: DeserializeOwned>(&self, section_key: &str) -> Result<T> {
        let value = self.navigate_to_section(section_key)?;
        serde_json::from_value(value.clone())
            .context(format!("Failed to decode section '{section_key}' into target type"))
    }

    /// Navigate to a section key (e.g., "oauth2.github")
    ///
    /// Similar to Go's `navigateToKey()` function in `module_loader.go`
    fn navigate_to_section(&self, key: &str) -> Result<&serde_json::Value> {
        let parts: Vec<&str> = key.split('.').collect();
        let mut current: Option<&serde_json::Value> = None;

        for part in parts {
            if current.is_none() {
                current = self.raw_config.get(part);
            } else {
                current = current.and_then(|v| v.get(part));
            }
        }

        current.ok_or_else(|| anyhow::anyhow!("Section '{key}' not found in config"))
    }

    /// Get all provider instance names from oauth2 section
    #[must_use]
    pub fn provider_instances(&self) -> Vec<String> {
        let mut instances = Vec::new();

        if let Some(oauth2_section) = self.raw_config.get("oauth2") {
            if let Some(map) = oauth2_section.as_object() {
                for key in map.keys() {
                    instances.push(key.clone());
                }
            }
        }

        instances
    }

    /// Get provider type from a section
    ///
    /// Determines the provider type by:
    /// 1. Checking for explicit `type` field in the section
    /// 2. Using the instance name if no `type` field is present
    pub fn get_provider_type(&self, instance_name: &str) -> Result<String> {
        let section_key = format!("oauth2.{instance_name}");
        let value = self.navigate_to_section(&section_key)?;

        // Check for explicit `type` field
        if let Some(type_str) = value.get("type").and_then(|v| v.as_str()) {
            return Ok(type_str.to_string());
        }

        // Default to instance name
        Ok(instance_name.to_string())
    }
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}
