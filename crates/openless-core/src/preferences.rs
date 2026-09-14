//! Framework-independent user preferences persistence.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::errors::{BackendError, BackendErrorCode};
use crate::persistence::atomic_write;
use crate::shared_types::UserPreferences;

fn persistence_error(operation: impl Into<String>) -> BackendError {
    BackendError::new(BackendErrorCode::Persistence, operation)
}

fn read_preferences(path: &Path) -> Result<UserPreferences, BackendError> {
    if !path.exists() {
        return Ok(UserPreferences::default());
    }
    let bytes = fs::read(path).map_err(|_| persistence_error("read preferences"))?;
    if bytes.is_empty() {
        return Ok(UserPreferences::default());
    }

    let preferences = match serde_json::from_slice::<UserPreferences>(&bytes) {
        Ok(preferences) => preferences,
        Err(error) => {
            log::error!(
                "[prefs] strict decode of {} failed: {error}; backing up and salvaging",
                path.display()
            );
            let backup = backup_unparseable_preferences(path, &bytes)?;
            log::info!(
                "[prefs] original unparseable preferences backed up to {}",
                backup.display()
            );
            let salvaged = UserPreferences::salvage_from_json_bytes(&bytes);
            match serde_json::to_vec_pretty(&salvaged)
                .map_err(|_| persistence_error("encode salvaged preferences"))
                .and_then(|json| atomic_write(path, &json))
            {
                Ok(()) => log::info!(
                    "[prefs] salvaged preferences written back to {}",
                    path.display()
                ),
                Err(error) => log::warn!(
                    "[prefs] failed to persist salvaged preferences to {}: {error}",
                    path.display()
                ),
            }
            return Ok(salvaged);
        }
    };

    Ok(preferences)
}

fn backup_unparseable_preferences(path: &Path, bytes: &[u8]) -> Result<PathBuf, BackendError> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let backup = path.with_file_name(format!(
        "preferences.corrupt-{timestamp}-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .map_err(|_| persistence_error("create corrupt preferences backup"))?;
    file.write_all(bytes)
        .map_err(|_| persistence_error("write corrupt preferences backup"))?;
    file.sync_all()
        .map_err(|_| persistence_error("flush corrupt preferences backup"))?;
    Ok(backup)
}

pub struct PreferencesStore {
    path: PathBuf,
    state: Mutex<UserPreferences>,
}

impl PreferencesStore {
    /// Opens a preferences document at a host-selected path.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, BackendError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(persistence_error("preferences path is empty"));
        }
        let preferences = read_preferences(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(preferences),
        })
    }

    /// Creates an in-memory fallback. Mutating calls deliberately fail instead
    /// of writing to an implicit temporary or platform directory.
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            state: Mutex::new(UserPreferences::default()),
        }
    }

    /// Creates a default-valued fallback at a path selected by the host.
    /// An empty path retains the memory-only, fail-on-write behavior.
    pub fn fallback(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            state: Mutex::new(UserPreferences::default()),
        }
    }

    pub fn get(&self) -> UserPreferences {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, preferences: UserPreferences) -> Result<(), BackendError> {
        let json = serde_json::to_vec_pretty(&preferences)
            .map_err(|_| persistence_error("encode preferences"))?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        atomic_write(&self.path, &json)?;
        *state = preferences;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_preferences_path() -> (PathBuf, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("openless-core-prefs-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create temp dir");
        let path = directory.join("preferences.json");
        (directory, path)
    }


    #[test]
    fn corrupt_preferences_are_backed_up_before_salvage() {
        let (directory, path) = temporary_preferences_path();
        let original = br#"{
            "historyRetentionDays": "not-a-number",
            "activeAsrProvider": "preserved-provider"
        }"#;
        fs::write(&path, original).expect("write corrupt preferences");

        let preferences = read_preferences(&path).expect("salvage preferences");
        assert_eq!(preferences.active_asr_provider, "preserved-provider");

        let backups = fs::read_dir(&directory)
            .expect("read temp dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("preferences.corrupt-"))
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).expect("read backup"), original);
        assert!(serde_json::from_slice::<UserPreferences>(
            &fs::read(&path).expect("read salvaged preferences")
        )
        .is_ok());

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn store_round_trips_settings_updates() {
        let (directory, path) = temporary_preferences_path();
        let store = PreferencesStore::open(&path).expect("open preferences");
        store
            .set(UserPreferences {
                microphone_device_name: "External Mic".to_string(),
                ..UserPreferences::default()
            })
            .expect("update preferences");

        let saved = store.get();
        assert_eq!(saved.microphone_device_name, "External Mic");

        let _ = fs::remove_dir_all(directory);
    }

    /// 旧 preferences.json 残留已删除的风格/翻译/市场字段时必须照常加载，
    /// 其余有效设置不丢（serde 跳过未知字段）。
    #[test]
    fn legacy_preferences_with_removed_feature_fields_still_load() {
        let (directory, path) = temporary_preferences_path();
        fs::write(
            &path,
            r#"{
                "microphoneDeviceName": "Legacy Mic",
                "polishMode": "light",
                "stylePackId": "builtin.light",
                "translationTargetLanguage": "English",
                "translationHotkey": { "primary": "Shift", "modifiers": [] },
                "chineseScriptPreference": "traditional",
                "outputLanguagePreference": "en",
                "marketplaceBaseUrl": "https://api.example.com",
                "marketplaceDevLogin": "octocat"
            }"#,
        )
        .expect("write legacy preferences");

        let preferences = read_preferences(&path).expect("read legacy preferences");
        assert_eq!(preferences.microphone_device_name, "Legacy Mic");

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn in_memory_store_refuses_implicit_persistence() {
        let store = PreferencesStore::in_memory();
        let error = store.set(UserPreferences::default()).unwrap_err();
        assert_eq!(error.code, BackendErrorCode::Persistence);
    }
}
