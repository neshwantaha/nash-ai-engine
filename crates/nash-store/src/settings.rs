//! Key/value settings, including the serialised [`AppConfig`].

use crate::db::Store;
use nash_core::config::AppConfig;
use nash_core::error::{EngineError, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

/// Settings key under which the whole [`AppConfig`] JSON is stored.
pub const KEY_CONFIG: &str = "app_config";
/// Settings key for the last-used provider id.
pub const KEY_LAST_PROVIDER: &str = "last_provider";

impl Store {
    /// Read a JSON setting.
    pub fn get_setting(&self, key: &str) -> Result<Option<Value>> {
        let raw: Option<String> = self.with_conn(|c| {
            c.query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| EngineError::Other(format!("database error: {e}")))
        })?;
        match raw {
            Some(text) => Ok(Some(serde_json::from_str(&text)?)),
            None => Ok(None),
        }
    }

    /// Write a JSON setting (upsert).
    pub fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        let text = serde_json::to_string(value)?;
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=?2",
                params![key, text],
            )
            .map_err(|e| EngineError::Other(format!("database error: {e}")))?;
            Ok(())
        })
    }

    /// Remove a setting. Returns whether a row was deleted.
    pub fn delete_setting(&self, key: &str) -> Result<bool> {
        self.with_conn(|c| {
            let n = c
                .execute("DELETE FROM settings WHERE key=?1", params![key])
                .map_err(|e| EngineError::Other(format!("database error: {e}")))?;
            Ok(n > 0)
        })
    }

    /// Persist an [`AppConfig`]. The config never contains raw secrets.
    pub fn save_config(&self, config: &AppConfig) -> Result<()> {
        let value = serde_json::to_value(config)?;
        self.set_setting(KEY_CONFIG, &value)
    }

    /// Load the saved [`AppConfig`], if any. The config is re-validated.
    pub fn load_config(&self) -> Result<Option<AppConfig>> {
        match self.get_setting(KEY_CONFIG)? {
            Some(value) => {
                let cfg: AppConfig = serde_json::from_value(value)?;
                cfg.validate()?;
                Ok(Some(cfg))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_setting("missing").unwrap(), None);
        store
            .set_setting(KEY_LAST_PROVIDER, &serde_json::json!("openai"))
            .unwrap();
        assert_eq!(
            store.get_setting(KEY_LAST_PROVIDER).unwrap(),
            Some(serde_json::json!("openai"))
        );
        assert!(store.delete_setting(KEY_LAST_PROVIDER).unwrap());
        assert!(!store.delete_setting(KEY_LAST_PROVIDER).unwrap());
    }

    #[test]
    fn config_roundtrips() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.load_config().unwrap().is_none());
        let cfg = AppConfig::default();
        store.save_config(&cfg).unwrap();
        let back = store.load_config().unwrap().unwrap();
        assert_eq!(back, cfg);
    }
}
