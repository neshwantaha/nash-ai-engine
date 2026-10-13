//! Platform paths for configuration, the database and app data.

use std::path::PathBuf;

/// The user's home directory, best-effort.
pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory holding Nash configuration and data.
///
/// Honours `NASH_CONFIG_DIR`; otherwise uses `%APPDATA%\nash` on Windows and
/// `$XDG_CONFIG_HOME/nash` (or `~/.config/nash`) elsewhere.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NASH_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("nash");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("nash");
    }
    home_dir().join(".config").join("nash")
}

/// Default configuration file path.
pub fn default_config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Default database file path.
pub fn default_db_path() -> PathBuf {
    config_dir().join("nash.sqlite")
}
