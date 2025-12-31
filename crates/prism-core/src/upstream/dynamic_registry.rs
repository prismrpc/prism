//! Dynamic upstream registry for tracking dynamically-added upstreams.
//!
//! This module provides a registry to distinguish between upstreams loaded from config
//! and those added via the admin API at runtime. Dynamic upstreams are persisted to disk
//! for restart survival.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::PathBuf};
use tracing::{debug, error, info};

/// Configuration for a dynamically-added upstream.
///
/// This is persisted to disk and contains all information needed to reconstruct
/// the upstream after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicUpstreamConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    pub ws_url: Option<String>,
    pub weight: u32,
    pub chain_id: u64,
    pub timeout_seconds: u64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Source of an upstream configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamSource {
    /// Loaded from TOML configuration file.
    Config,
    /// Added via admin API at runtime (dynamic).
    Dynamic,
}

/// Registry for tracking dynamically-added upstreams.
///
/// Maintains separation between config-based and dynamically-added upstreams,
/// providing persistence for dynamic upstreams.
pub struct DynamicUpstreamRegistry {
    /// Dynamically-added upstreams indexed by ID.
    dynamic_upstreams: RwLock<HashMap<String, DynamicUpstreamConfig>>,
    /// Names of upstreams from config (for source checking).
    config_upstream_names: RwLock<Vec<String>>,
    /// Optional file path for persistence.
    storage_path: Option<PathBuf>,
}

impl DynamicUpstreamRegistry {
    /// Creates a new dynamic upstream registry.
    ///
    /// # Arguments
    ///
    /// * `storage_path` - Optional path to JSON file for persisting dynamic upstreams
    #[must_use]
    pub fn new(storage_path: Option<PathBuf>) -> Self {
        Self {
            dynamic_upstreams: RwLock::new(HashMap::new()),
            config_upstream_names: RwLock::new(Vec::new()),
            storage_path,
        }
    }

    /// Initializes the registry with config upstream names.
    ///
    /// This should be called during startup after loading config upstreams.
    pub fn initialize_config_upstreams(&self, names: Vec<String>) {
        let mut config_names = self.config_upstream_names.write();
        *config_names = names;
        debug!(count = config_names.len(), "initialized config upstream names");
    }

    /// Adds a dynamic upstream to the registry.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - An upstream with the same name already exists
    /// - The upstream ID conflicts with an existing dynamic upstream
    /// - Persistence fails
    pub fn add(&self, config: &DynamicUpstreamConfig) -> Result<String, String> {
        // Check if name conflicts with config upstreams
        if self.is_config_upstream(&config.name) {
            return Err(format!(
                "Cannot add dynamic upstream '{}': name conflicts with config upstream",
                config.name
            ));
        }

        // Check if name conflicts with existing dynamic upstreams
        {
            let upstreams = self.dynamic_upstreams.read();
            if upstreams.values().any(|u| u.name == config.name) {
                return Err(format!(
                    "Cannot add dynamic upstream '{}': name already exists",
                    config.name
                ));
            }
        }

        let id = config.id.clone();

        // Add to registry
        {
            let mut upstreams = self.dynamic_upstreams.write();
            upstreams.insert(id.clone(), config.clone());
        }

        // Persist to disk
        if let Err(e) = self.save_to_disk() {
            error!(error = %e, "failed to persist dynamic upstreams");
            // Don't fail the operation, but log the error
        }

        info!(id = %id, name = %config.name, "added dynamic upstream");
        Ok(id)
    }

    /// Updates a dynamic upstream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The upstream ID doesn't exist
    /// - The new name conflicts with another upstream
    /// - Persistence fails
    pub fn update(&self, id: &str, updates: DynamicUpstreamUpdate) -> Result<(), String> {
        // Check for name conflicts first, before acquiring write lock
        if let Some(ref new_name) = updates.name {
            let upstreams = self.dynamic_upstreams.read();

            // Get current name
            let current_name = upstreams
                .get(id)
                .map(|u| u.name.clone())
                .ok_or_else(|| format!("Dynamic upstream '{id}' not found"))?;

            if new_name != &current_name {
                // Check config upstreams
                if self.is_config_upstream(new_name) {
                    return Err(format!(
                        "Cannot rename to '{new_name}': conflicts with config upstream"
                    ));
                }

                // Check other dynamic upstreams
                if upstreams.values().any(|u| u.id != id && &u.name == new_name) {
                    return Err(format!("Cannot rename to '{new_name}': name already exists"));
                }
            }
        }

        // Now acquire write lock and apply updates
        let mut upstreams = self.dynamic_upstreams.write();

        let config = upstreams
            .get_mut(id)
            .ok_or_else(|| format!("Dynamic upstream '{id}' not found"))?;

        // Apply updates
        if let Some(name) = updates.name {
            config.name = name;
        }
        if let Some(url) = updates.url {
            config.url = url;
        }
        if let Some(ws_url) = updates.ws_url {
            config.ws_url = ws_url;
        }
        if let Some(weight) = updates.weight {
            config.weight = weight;
        }
        if let Some(enabled) = updates.enabled {
            config.enabled = enabled;
        }

        // Update timestamp
        config.updated_at = chrono::Utc::now().to_rfc3339();

        drop(upstreams);

        // Persist to disk
        if let Err(e) = self.save_to_disk() {
            error!(error = %e, "failed to persist dynamic upstreams");
        }

        info!(id = %id, "updated dynamic upstream");
        Ok(())
    }

    /// Removes a dynamic upstream from the registry.
    ///
    /// # Errors
    ///
    /// Returns an error if the upstream ID doesn't exist.
    pub fn remove(&self, id: &str) -> Result<DynamicUpstreamConfig, String> {
        let mut upstreams = self.dynamic_upstreams.write();

        let config = upstreams
            .remove(id)
            .ok_or_else(|| format!("Dynamic upstream '{id}' not found"))?;

        drop(upstreams);

        // Persist to disk
        if let Err(e) = self.save_to_disk() {
            error!(error = %e, "failed to persist dynamic upstreams");
        }

        info!(id = %id, name = %config.name, "removed dynamic upstream");
        Ok(config)
    }

    /// Gets a dynamic upstream by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<DynamicUpstreamConfig> {
        let upstreams = self.dynamic_upstreams.read();
        upstreams.get(id).cloned()
    }

    /// Lists all dynamic upstreams.
    #[must_use]
    pub fn list_all(&self) -> Vec<DynamicUpstreamConfig> {
        let upstreams = self.dynamic_upstreams.read();
        upstreams.values().cloned().collect()
    }

    /// Checks if an upstream name belongs to a config upstream.
    #[must_use]
    pub fn is_config_upstream(&self, name: &str) -> bool {
        let config_names = self.config_upstream_names.read();
        config_names.contains(&name.to_string())
    }

    /// Determines the source of an upstream by name.
    #[must_use]
    pub fn get_source(&self, name: &str) -> Option<UpstreamSource> {
        if self.is_config_upstream(name) {
            Some(UpstreamSource::Config)
        } else {
            let upstreams = self.dynamic_upstreams.read();
            if upstreams.values().any(|u| u.name == name) {
                Some(UpstreamSource::Dynamic)
            } else {
                None
            }
        }
    }

    /// Loads dynamic upstreams from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if file reading or JSON parsing fails.
    pub fn load_from_disk(&self) -> Result<(), std::io::Error> {
        let Some(ref path) = self.storage_path else {
            return Ok(()); // No storage path configured
        };

        if !path.exists() {
            debug!("no dynamic upstreams file found, starting fresh");
            return Ok(());
        }

        let contents = fs::read_to_string(path)?;
        let configs: Vec<DynamicUpstreamConfig> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut upstreams = self.dynamic_upstreams.write();
        upstreams.clear();
        for config in configs {
            upstreams.insert(config.id.clone(), config);
        }

        info!(
            count = upstreams.len(),
            path = %path.display(),
            "loaded dynamic upstreams from disk"
        );

        Ok(())
    }

    /// Saves dynamic upstreams to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if file writing or JSON serialization fails.
    pub fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let Some(ref path) = self.storage_path else {
            return Ok(()); // No storage path configured
        };

        let upstreams = self.dynamic_upstreams.read();
        let configs: Vec<DynamicUpstreamConfig> = upstreams.values().cloned().collect();
        drop(upstreams);

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&configs)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        fs::write(path, contents)?;

        debug!(
            count = configs.len(),
            path = %path.display(),
            "saved dynamic upstreams to disk"
        );

        Ok(())
    }
}

/// Updates to apply to a dynamic upstream.
#[derive(Debug, Clone, Default)]
pub struct DynamicUpstreamUpdate {
    pub name: Option<String>,
    pub url: Option<String>,
    pub ws_url: Option<Option<String>>,
    pub weight: Option<u32>,
    pub enabled: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_config(id: &str, name: &str) -> DynamicUpstreamConfig {
        DynamicUpstreamConfig {
            id: id.to_string(),
            name: name.to_string(),
            url: format!("https://{name}.example.com"),
            ws_url: None,
            weight: 100,
            chain_id: 1,
            timeout_seconds: 30,
            enabled: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_add_dynamic_upstream() {
        let registry = DynamicUpstreamRegistry::new(None);
        let config = create_test_config("1", "test-upstream");

        let result = registry.add(&config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "1");

        let retrieved = registry.get("1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test-upstream");
    }

    #[test]
    fn test_duplicate_name_rejection() {
        let registry = DynamicUpstreamRegistry::new(None);

        let config1 = create_test_config("1", "test-upstream");
        let config2 = create_test_config("2", "test-upstream");

        assert!(registry.add(&config1).is_ok());
        assert!(registry.add(&config2).is_err());
    }

    #[test]
    fn test_config_upstream_conflict() {
        let registry = DynamicUpstreamRegistry::new(None);
        registry.initialize_config_upstreams(vec!["config-upstream".to_string()]);

        let config = create_test_config("1", "config-upstream");
        let result = registry.add(&config);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("conflicts with config upstream"));
    }

    #[test]
    fn test_update_upstream() {
        let registry = DynamicUpstreamRegistry::new(None);
        let config = create_test_config("1", "test-upstream");

        registry.add(&config).unwrap();

        let updates = DynamicUpstreamUpdate {
            name: Some("updated-name".to_string()),
            weight: Some(200),
            ..Default::default()
        };

        let result = registry.update("1", updates);
        assert!(result.is_ok());

        let retrieved = registry.get("1").unwrap();
        assert_eq!(retrieved.name, "updated-name");
        assert_eq!(retrieved.weight, 200);
    }

    #[test]
    fn test_remove_upstream() {
        let registry = DynamicUpstreamRegistry::new(None);
        let config = create_test_config("1", "test-upstream");

        registry.add(&config).unwrap();
        assert!(registry.get("1").is_some());

        let result = registry.remove("1");
        assert!(result.is_ok());
        assert!(registry.get("1").is_none());
    }

    #[test]
    fn test_list_all() {
        let registry = DynamicUpstreamRegistry::new(None);

        let config1 = create_test_config("1", "upstream-1");
        let config2 = create_test_config("2", "upstream-2");

        registry.add(&config1).unwrap();
        registry.add(&config2).unwrap();

        let all = registry.list_all();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_get_source() {
        let registry = DynamicUpstreamRegistry::new(None);
        registry.initialize_config_upstreams(vec!["config-upstream".to_string()]);

        let config = create_test_config("1", "dynamic-upstream");
        registry.add(&config).unwrap();

        assert_eq!(registry.get_source("config-upstream"), Some(UpstreamSource::Config));
        assert_eq!(registry.get_source("dynamic-upstream"), Some(UpstreamSource::Dynamic));
        assert_eq!(registry.get_source("non-existent"), None);
    }

    #[test]
    fn test_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let storage_path = temp_dir.path().join("dynamic_upstreams.json");

        // Create registry and add upstreams
        {
            let registry = DynamicUpstreamRegistry::new(Some(storage_path.clone()));
            let config = create_test_config("1", "test-upstream");
            registry.add(&config).unwrap();
        }

        // Load from disk in new registry
        {
            let registry = DynamicUpstreamRegistry::new(Some(storage_path));
            registry.load_from_disk().unwrap();

            let upstreams = registry.list_all();
            assert_eq!(upstreams.len(), 1);
            assert_eq!(upstreams[0].name, "test-upstream");
        }
    }
}
