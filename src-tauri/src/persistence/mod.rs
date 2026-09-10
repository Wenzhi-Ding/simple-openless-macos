//! Local persistence: history JSON, user preferences JSON, vocab JSON, and
//! platform-backed credentials vault.
//!
//! Storage roots:
//! - macOS:   `~/Library/Application Support/OpenLess`
//! - Windows: `%APPDATA%\OpenLess`
//! - Linux:   `$XDG_DATA_HOME/OpenLess` or `~/.local/share/OpenLess`
//!
//! Credential storage policy: provider credentials are stored in the OS
//! credential vault (macOS Keychain, Windows Credential Manager, Linux keyring).
//! A legacy plaintext JSON file is read once as a migration source and removed
//! after a successful vault write; new writes never persist plaintext secrets.
//!
//! This module is split into focused submodules; everything that was previously
//! reachable as `crate::persistence::*` stays reachable via the glob re-exports
//! below. The shared filesystem helpers and the two cross-cutting constants live
//! here so every submodule can reach them through `super::`.
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
mod activity;
mod dictionary;
mod history;
mod paths;
mod preferences;
pub use activity::*;
pub use dictionary::*;
pub use history::*;
pub use paths::*;
pub use preferences::*;
const HISTORY_CAP: usize = 200;
const PREFERENCES_FILE: &str = "preferences.json";
pub(crate) fn data_dir() -> Result<PathBuf> {
    {
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("OpenLess"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("OpenLess"));
            }
        }
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("OpenLess"))
    }
}
/// Fallback store path when `data_dir()` is unavailable.
fn fallback_store_path(file_name: &str) -> PathBuf {
    std::env::temp_dir().join(file_name)
}
fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create dir failed: {}", dir.display()))?;
    Ok(())
}
