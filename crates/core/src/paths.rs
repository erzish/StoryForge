//! Where StoryForge keeps its files.
//!
//! Resolution order for the database:
//! 1. `STORYFORGE_DB` environment variable (absolute path to a `story.db`).
//! 2. `db_path` inside `<config dir>/config.json`.
//! 3. `<config dir>/data/story.db` — created with the bundled schema on first run.
//!
//! `<config dir>` is `%APPDATA%\StoryForge` on Windows, `$XDG_CONFIG_HOME/storyforge`
//! elsewhere. Keeping the database outside the install directory means an
//! upgrade never touches the author's books.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SfError};

pub const APP_DIR_NAME: &str = "StoryForge";
pub const CONFIG_FILE: &str = "config.json";
pub const DB_FILE: &str = "story.db";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    /// Absolute path to the SQLite database. Missing means the default location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_path: Option<String>,
    /// Preferred HTTP port for the local API/MCP endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Last library folder offered in the import dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_root: Option<String>,
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STORYFORGE_HOME") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Path::new(&appdata).join(APP_DIR_NAME);
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return Path::new(&xdg).join("storyforge");
        }
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(".config").join("storyforge");
        }
    }
    PathBuf::from(".").join(".storyforge")
}

pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

pub fn load_config() -> AppConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), %err, "config unreadable, using defaults");
            AppConfig::default()
        }),
        Err(_) => AppConfig::default(),
    }
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| SfError::io(&dir, e))?;
    let path = config_path();
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, text).map_err(|e| SfError::io(&path, e))
}

/// Resolve the database path, without creating anything.
pub fn resolve_db_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("STORYFORGE_DB")
        && !explicit.trim().is_empty()
    {
        return PathBuf::from(explicit);
    }
    let config = load_config();
    if let Some(custom) = config.db_path.as_deref()
        && !custom.trim().is_empty()
    {
        return PathBuf::from(custom);
    }
    config_dir().join("data").join(DB_FILE)
}

/// Locate an existing StoryForge database that is not the default one,
/// e.g. the JavaScript edition's `data/story.db` next to its `server/` folder.
/// Returns the first candidate that exists.
pub fn find_legacy_database(start: &Path) -> Option<PathBuf> {
    let library = start.join("data").join(DB_FILE);
    if library.is_file() {
        return Some(library);
    }
    let repo = start.join("StoryForge").join("data").join(DB_FILE);
    if repo.is_file() {
        return Some(repo);
    }
    None
}
