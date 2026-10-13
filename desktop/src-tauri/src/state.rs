//! Application state shared across Tauri commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nash_core::config::AppConfig;
use nash_core::engine::Engine;
use nash_agent::ApprovalResponse;
use nash_store::{KeyringSecretStore, Store};
use tokio_util::sync::CancellationToken;

/// Service name grouping this app's credentials in the OS keyring.
pub const SERVICE: &str = "nash-ai-engine";

/// Resolve the directory holding the config file and database.
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NASH_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("nash");
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("nash");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("nash");
    }
    PathBuf::from(".nash")
}

pub fn default_config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn default_db_path() -> PathBuf {
    config_dir().join("nash.sqlite")
}

/// Read configuration from disk, defaulting when absent.
pub fn read_config(path: &Path) -> anyhow::Result<AppConfig> {
    if path.exists() {
        let text = std::fs::read_to_string(path)?;
        Ok(AppConfig::from_json(&text)?)
    } else {
        Ok(AppConfig::starter())
    }
}

/// Write configuration to disk, creating parent directories.
pub fn write_config(path: &Path, cfg: &AppConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, cfg.to_json_pretty()?)?;
    Ok(())
}

/// Process-wide state: config, database, and in-flight cancellations.
pub struct AppState {
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    pub store: Store,
    pub config: Mutex<AppConfig>,
    pub cancels: Mutex<HashMap<String, CancellationToken>>,
    pub approvals: Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApprovalResponse>>>,
}

impl AppState {
    /// Load state from the default (or env-overridden) locations.
    pub fn load() -> anyhow::Result<std::sync::Arc<Self>> {
        let config_path = std::env::var_os("NASH_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(default_config_path);
        let db_path = std::env::var_os("NASH_DB")
            .map(PathBuf::from)
            .unwrap_or_else(default_db_path);

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let config = read_config(&config_path)?;
        let store = Store::open(&db_path)?;
        Ok(std::sync::Arc::new(Self {
            config_path,
            db_path,
            store,
            config: Mutex::new(config),
            cancels: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
        }))
    }

    /// A snapshot of the current configuration.
    pub fn snapshot(&self) -> AppConfig {
        self.config.lock().unwrap().clone()
    }

    /// Build an engine from the current configuration.
    pub fn engine(&self) -> Result<Engine, String> {
        let cfg = self.config.lock().unwrap().clone();
        let secrets = KeyringSecretStore::new(SERVICE);
        Engine::new(cfg, Box::new(secrets)).map_err(|e| e.to_string())
    }
}
