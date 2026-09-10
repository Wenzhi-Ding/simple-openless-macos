//! Vocabulary entries and hit accounting.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;

use crate::errors::{BackendError, BackendErrorCode};
use crate::persistence::{atomic_write, persistence_error, read_or_default};
use crate::types::DictionaryEntry;

pub struct DictionaryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl DictionaryStore {
    pub fn at_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self::at_path(data_dir.as_ref().join("dictionary.json"))
    }

    pub fn at_path(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<DictionaryEntry>, BackendError> {
        let _guard = self.lock_store()?;
        read_or_default(&self.path)
    }

    pub fn add(
        &self,
        phrase: String,
        note: Option<String>,
    ) -> Result<DictionaryEntry, BackendError> {
        let _guard = self.lock_store()?;
        let mut entries = self.read_locked()?;
        let phrase = phrase.trim().to_string();
        if phrase.is_empty() {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "dictionary phrase is required",
            ));
        }
        if entries
            .iter()
            .any(|entry| entry.phrase.trim().eq_ignore_ascii_case(&phrase))
        {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "dictionary phrase already exists",
            ));
        }
        let entry = new_entry(phrase, note);
        entries.insert(0, entry.clone());
        self.write_locked(&entries)?;
        Ok(entry)
    }

    pub fn remove(&self, id: &str) -> Result<(), BackendError> {
        let _guard = self.lock_store()?;
        let mut entries = self.read_locked()?;
        let before = entries.len();
        entries.retain(|entry| entry.id != id);
        if entries.len() != before {
            self.write_locked(&entries)?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), BackendError> {
        let _guard = self.lock_store()?;
        let mut entries = self.read_locked()?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidArgument,
                    "dictionary entry not found",
                )
            })?;
        if entry.enabled != enabled {
            entry.enabled = enabled;
            self.write_locked(&entries)?;
        }
        Ok(())
    }

    /// Count case-insensitive, non-overlapping occurrences in final output.
    pub fn record_hits(&self, text: &str) -> Result<u64, BackendError> {
        if text.is_empty() {
            return Ok(0);
        }
        let _guard = self.lock_store()?;
        let mut entries = self.read_locked()?;
        let haystack = text.to_lowercase();
        let mut total = 0_u64;
        let mut changed = false;
        for entry in entries.iter_mut().filter(|entry| entry.enabled) {
            let needle = entry.phrase.trim().to_lowercase();
            let count = count_occurrences(&haystack, &needle);
            if count > 0 {
                entry.hits = entry.hits.saturating_add(count);
                total = total.saturating_add(count);
                changed = true;
            }
        }
        if changed {
            self.write_locked(&entries)?;
        }
        Ok(total)
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, ()>, BackendError> {
        self.lock.lock().map_err(|_| {
            BackendError::new(BackendErrorCode::Internal, "dictionary store lock poisoned")
        })
    }

    fn read_locked(&self) -> Result<Vec<DictionaryEntry>, BackendError> {
        read_or_default(&self.path)
    }

    fn write_locked(&self, entries: &[DictionaryEntry]) -> Result<(), BackendError> {
        let json = serde_json::to_vec_pretty(entries)
            .map_err(|_| persistence_error("encode dictionary entries"))?;
        atomic_write(&self.path, &json)
    }
}

fn new_entry(phrase: String, note: Option<String>) -> DictionaryEntry {
    DictionaryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        phrase,
        note,
        enabled: true,
        hits: 0,
        created_at: Utc::now().to_rfc3339(),
    }
}

fn count_occurrences(haystack: &str, needle: &str) -> u64 {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    let mut count = 0_u64;
    let mut start = 0_usize;
    while let Some(position) = haystack[start..].find(needle) {
        count = count.saturating_add(1);
        start += position + needle.len();
        if start >= haystack.len() {
            break;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (DictionaryStore, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "openless-core-vocab-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        (DictionaryStore::at_path(path.clone()), path)
    }

    #[test]
    fn explicit_add_trims_rejects_empty_and_deduplicates_case_insensitively() {
        let (store, path) = temp_store();
        let entry = store
            .add("  OpenLess  ".into(), Some("note".into()))
            .unwrap();
        assert_eq!(entry.phrase, "OpenLess");
        assert!(store.add("openless".into(), None).is_err());
        assert!(store.add("   ".into(), None).is_err());
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_learned_note_loads_without_affecting_asr_order() {
        let (store, path) = temp_store();
        std::fs::write(&path, r#"[{"id":"legacy","phrase":"Legacy","note":"从手改中自动收集","enabled":true,"hits":0,"createdAt":""}]"#).unwrap();
        let entries = store.list().unwrap();
        assert_eq!(entries[0].note.as_deref(), Some("从手改中自动收集"));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.phrase.as_str())
                .collect::<Vec<_>>(),
            vec!["Legacy"]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn list_preserves_persisted_order_regardless_of_hits() {
        let (store, path) = temp_store();
        std::fs::write(&path, r#"[{"id":"first","phrase":"First","enabled":true,"hits":1,"createdAt":""},{"id":"second","phrase":"Second","enabled":true,"hits":999,"createdAt":""}]"#).unwrap();
        let entries = store.list().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.phrase.as_str())
                .collect::<Vec<_>>(),
            vec!["First", "Second"]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn records_hits_only_for_enabled_entries() {
        let (store, path) = temp_store();
        let enabled = store.add("Codex".into(), None).unwrap();
        let disabled = store.add("Rust".into(), None).unwrap();
        store.set_enabled(&disabled.id, false).unwrap();
        assert_eq!(store.record_hits("codex CODEX Rust").unwrap(), 2);
        let entries = store.list().unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == enabled.id)
                .unwrap()
                .hits,
            2
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == disabled.id)
                .unwrap()
                .hits,
            0
        );
        let _ = std::fs::remove_file(path);
    }
}
