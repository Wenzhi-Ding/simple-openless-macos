//! Newest-first dictation history with retention and count caps.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::errors::{BackendError, BackendErrorCode};
use crate::persistence::{atomic_write, persistence_error, read_or_default};
use crate::types::DictationSession;

pub const HISTORY_CAP: usize = 200;

pub struct HistoryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl HistoryStore {
    pub fn at_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self::at_path(data_dir.as_ref().join("history.json"))
    }

    pub fn at_path(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<DictationSession>, BackendError> {
        let _guard = self.lock_store()?;
        self.read_locked()
    }

    pub fn append_with_retention(
        &self,
        session: DictationSession,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<(), BackendError> {
        let _guard = self.lock_store()?;
        let mut sessions = self.read_locked()?;
        sessions.insert(0, session);
        if retention_days > 0 {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days));
            sessions.retain(|session| {
                chrono::DateTime::parse_from_rfc3339(&session.created_at)
                    .map(|time| time.with_timezone(&chrono::Utc) >= cutoff)
                    .unwrap_or(true)
            });
        }
        let cap = max_entries
            .map(|count| (count as usize).clamp(5, HISTORY_CAP))
            .unwrap_or(HISTORY_CAP);
        sessions.truncate(cap);
        self.write_locked(&sessions)
    }

    pub fn recent_within_minutes(
        &self,
        minutes: u32,
    ) -> Result<Vec<DictationSession>, BackendError> {
        if minutes == 0 {
            return Ok(Vec::new());
        }
        let _guard = self.lock_store()?;
        let sessions = self.read_locked()?;
        let cutoff = chrono::Utc::now() - chrono::Duration::minutes(i64::from(minutes));
        Ok(sessions
            .into_iter()
            .take_while(|session| {
                chrono::DateTime::parse_from_rfc3339(&session.created_at)
                    .map(|time| time.with_timezone(&chrono::Utc) >= cutoff)
                    .unwrap_or(true)
            })
            .collect())
    }

    pub fn delete(&self, id: &str) -> Result<(), BackendError> {
        let _guard = self.lock_store()?;
        let mut sessions = self.read_locked()?;
        let before = sessions.len();
        sessions.retain(|session| session.id != id);
        if sessions.len() != before {
            self.write_locked(&sessions)?;
        }
        Ok(())
    }

    pub fn update_entry(&self, updated: DictationSession) -> Result<bool, BackendError> {
        let _guard = self.lock_store()?;
        let mut sessions = self.read_locked()?;
        let Some(slot) = sessions.iter_mut().find(|session| session.id == updated.id) else {
            return Ok(false);
        };
        *slot = updated;
        self.write_locked(&sessions)?;
        Ok(true)
    }

    pub fn clear(&self) -> Result<(), BackendError> {
        let _guard = self.lock_store()?;
        self.write_locked(&[])
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, ()>, BackendError> {
        self.lock.lock().map_err(|_| {
            BackendError::new(BackendErrorCode::Internal, "history store lock poisoned")
        })
    }

    fn read_locked(&self) -> Result<Vec<DictationSession>, BackendError> {
        read_or_default(&self.path)
    }

    fn write_locked(&self, sessions: &[DictationSession]) -> Result<(), BackendError> {
        let json = serde_json::to_vec_pretty(sessions)
            .map_err(|_| persistence_error("encode history entries"))?;
        atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HistoryInsertStatus, HistorySource};

    fn session(id: &str, created_at: String) -> DictationSession {
        DictationSession {
            id: id.into(),
            created_at,
            source: HistorySource::Voice,
            raw_transcript: "raw".into(),
            asr_transcript: None,
            final_text: "final".into(),
            polish_source: None,
            app_bundle_id: None,
            app_name: None,
            insert_status: HistoryInsertStatus::Inserted,
            error_code: None,
            duration_ms: Some(1000),
            dictionary_entry_count: None,
            has_audio_recording: None,
            asr_provider: None,
            asr_model: None,
            llm_provider: None,
            llm_model: None,
            pipeline_mode: None,
            asr_ms: None,
            polish_ms: None,
            polish_diagnostics: None,
        }
    }

    #[test]
    fn legacy_styles_are_ignored_without_losing_history_evidence() {
        let mut original =
            serde_json::to_value(session("legacy", chrono::Utc::now().to_rfc3339())).unwrap();
        original["rawTranscript"] = serde_json::json!("原始转写 😀\n第二行");
        original["finalText"] = serde_json::json!("过去的整理结果");
        original["hasAudioRecording"] = serde_json::json!(true);
        original["llmModel"] = serde_json::json!("legacy-E2B");
        original["polishMs"] = serde_json::json!(456);
        original["polishDiagnostics"] = serde_json::json!({
            "stage": "succeeded", "outcome": "succeeded",
            "prompt": "确切过去请求：轻度润色 / 正式表达",
            "messages": [{"role": "user", "content": "保留正文与 OCR"}]
        });
        for mode in [
            Some("raw"),
            Some("light"),
            Some("structured"),
            Some("formal"),
            Some("future-style"),
            None,
        ] {
            let mut legacy = original.clone();
            if let Some(mode) = mode {
                legacy["mode"] = serde_json::json!(mode);
                legacy["stylePackId"] = serde_json::json!(format!("old.{mode}"));
            }
            // 旧记录里的风格标签被静默跳过，其余字段一个不丢。
            let loaded: DictationSession = serde_json::from_value(legacy).unwrap();
            let normalized = serde_json::to_value(&loaded).unwrap();
            assert!(normalized.get("mode").is_none());
            assert!(normalized.get("stylePackId").is_none());
            let baseline: DictationSession = serde_json::from_value(original.clone()).unwrap();
            assert_eq!(
                loaded, baseline,
                "style removal must preserve every other field"
            );
            assert_eq!(
                normalized["polishDiagnostics"]["prompt"],
                original["polishDiagnostics"]["prompt"]
            );
            // Exercise the real storage path using synthetic data only. A normal
            // update gradually normalizes labels; reading alone never rewrites.
            let path =
                std::env::temp_dir().join(format!("openless-legacy-{}.json", uuid::Uuid::new_v4()));
            let mut old_record = original.clone();
            old_record["mode"] = serde_json::json!(mode);
            old_record["stylePackId"] = serde_json::json!("old.custom");
            let old_bytes = serde_json::to_vec(&vec![old_record]).unwrap();
            std::fs::write(&path, &old_bytes).unwrap();
            let store = HistoryStore::at_path(path.clone());
            assert_eq!(store.list().unwrap(), vec![loaded.clone()]);
            assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
            assert!(store.update_entry(loaded.clone()).unwrap());
            assert_eq!(store.list().unwrap(), vec![loaded]);
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved[0], normalized);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn context_accepts_old_and_unlabelled_history_without_style_filtering() {
        let first = session("first", chrono::Utc::now().to_rfc3339());
        let second = session("second", chrono::Utc::now().to_rfc3339());
        let mut failed = first.clone();
        failed.error_code = Some("failed".into());
        let turns =
            crate::dictation_context::eligible_polish_context_turns(vec![failed, first, second]);
        assert_eq!(turns.len(), 2);
        assert!(turns
            .iter()
            .all(|turn| turn.raw_text == "raw" && turn.polished_text == "final"));
    }

    #[test]
    fn append_orders_caps_and_filters_retention() {
        let path = std::env::temp_dir().join(format!(
            "openless-core-history-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let store = HistoryStore::at_path(path.clone());
        let old = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        store
            .append_with_retention(session("old", old), 0, None)
            .unwrap();
        for index in 0..7 {
            store
                .append_with_retention(
                    session(&format!("new-{index}"), chrono::Utc::now().to_rfc3339()),
                    7,
                    Some(5),
                )
                .unwrap();
        }
        let sessions = store.list().unwrap();
        assert_eq!(sessions.len(), 5);
        assert_eq!(sessions[0].id, "new-6");
        assert!(sessions.iter().all(|session| session.id != "old"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn update_delete_clear_and_recent_queries_are_stable() {
        let path = std::env::temp_dir().join(format!(
            "openless-core-history-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let store = HistoryStore::at_path(path.clone());
        let mut entry = session("one", chrono::Utc::now().to_rfc3339());
        store.append_with_retention(entry.clone(), 0, None).unwrap();
        assert_eq!(store.recent_within_minutes(5).unwrap(), vec![entry.clone()]);
        entry.final_text = "updated".into();
        assert!(store.update_entry(entry.clone()).unwrap());
        assert_eq!(store.list().unwrap(), vec![entry]);
        store.delete("one").unwrap();
        store.delete("missing").unwrap();
        assert!(store.list().unwrap().is_empty());
        store.clear().unwrap();
        assert!(store.recent_within_minutes(0).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }
}
