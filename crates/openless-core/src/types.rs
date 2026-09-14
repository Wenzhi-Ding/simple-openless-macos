use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque identifier for one dictation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    #[default]
    Voice,
    #[serde(alias = "selection_voice_edit")]
    SelectionPolish,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HistoryInsertStatus {
    Inserted,
    PasteSent,
    CopiedFallback,
    Failed,
    NotRequested,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DictationSession {
    pub id: String,
    pub created_at: String,
    #[serde(default)]
    pub source: HistorySource,
    pub raw_transcript: String,
    #[serde(default)]
    pub asr_transcript: Option<String>,
    pub final_text: String,
    #[serde(default)]
    pub polish_source: Option<String>,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub insert_status: HistoryInsertStatus,
    pub error_code: Option<String>,
    pub duration_ms: Option<u64>,
    pub dictionary_entry_count: Option<u32>,
    #[serde(default)]
    pub has_audio_recording: Option<bool>,
    #[serde(default)]
    pub asr_provider: Option<String>,
    #[serde(default)]
    pub asr_model: Option<String>,
    #[serde(default)]
    pub llm_provider: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub pipeline_mode: Option<String>,
    #[serde(default)]
    pub asr_ms: Option<u64>,
    #[serde(default)]
    pub polish_ms: Option<u64>,
    /// 润色调用诊断快照（用户已授权随本机历史保存完整 prompt；见
    /// polish_diagnostics 模块）。旧记录无此字段 → None。重新转写覆盖
    /// raw/final 时清除，避免误对应。
    #[serde(default)]
    pub polish_diagnostics: Option<crate::polish_diagnostics::PolishDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntry {
    pub id: String,
    pub phrase: String,
    #[serde(default, alias = "notes")]
    pub note: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, alias = "hitCount")]
    pub hits: u64,
    #[serde(default)]
    pub created_at: String,
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DictationPhase {
    Idle,
    Starting,
    Recording,
    Transcribing,
    Polishing,
    Inserting,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationStateSnapshot {
    pub phase: DictationPhase,
    pub session_id: Option<SessionId>,
    pub elapsed_ms: u64,
    pub level: f32,
    pub message: Option<String>,
    /// Native capture startup can return before its first PCM callback. Hosts
    /// must keep their warming presentation until that callback is observed.
    #[serde(default)]
    pub recording_ready: bool,
}

impl Default for DictationStateSnapshot {
    fn default() -> Self {
        Self {
            phase: DictationPhase::Idle,
            session_id: None,
            elapsed_ms: 0,
            level: 0.0,
            message: None,
            recording_ready: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationResult {
    pub session_id: SessionId,
    pub raw_text: String,
    pub polished_text: String,
    #[serde(default)]
    pub polish_source: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    pub inserted: InsertStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InsertStatus {
    Inserted,
    CopiedFallback,
    NotRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptDelta {
    pub text: String,
    pub offset: u64,
    pub is_final: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptAccumulator {
    text: String,
}

impl TranscriptAccumulator {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn apply(&mut self, delta: &TranscriptDelta) -> Result<(), crate::errors::BackendError> {
        let offset = usize::try_from(delta.offset).map_err(|_| {
            crate::errors::BackendError::new(
                crate::errors::BackendErrorCode::InvalidArgument,
                "transcript offset exceeds this platform's address space",
            )
        })?;
        if offset > self.text.chars().count() {
            return Err(crate::errors::BackendError::new(
                crate::errors::BackendErrorCode::InvalidArgument,
                "transcript delta starts after the current text",
            ));
        }
        let mut next = self.text.chars().take(offset).collect::<String>();
        next.push_str(&delta.text);
        self.text = next;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertFallbackPayload {
    pub reason: String,
    pub copied_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesChange {
    /// Monotonic revision used to invalidate host-side caches.  The payload
    /// intentionally contains no arbitrary JSON or credential values.
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSnapshot {
    pub microphone: PermissionState,
    pub accessibility: PermissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Unknown,
    Granted,
    Denied,
    Restricted,
    NoDevice,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPayload {
    pub level: NotificationLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryChange {
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyChange {
    pub revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_types::PlatformCapabilities;

    #[test]
    fn transcript_accumulator_applies_unicode_replace_from_offsets() {
        let mut transcript = TranscriptAccumulator::default();
        transcript
            .apply(&TranscriptDelta {
                text: "你".into(),
                offset: 0,
                is_final: false,
            })
            .unwrap();
        transcript
            .apply(&TranscriptDelta {
                text: "你好🙂".into(),
                offset: 0,
                is_final: true,
            })
            .unwrap();
        assert_eq!(transcript.text(), "你好🙂");
        transcript
            .apply(&TranscriptDelta {
                text: "们".into(),
                offset: 1,
                is_final: true,
            })
            .unwrap();
        assert_eq!(transcript.text(), "你们");
        assert!(transcript
            .apply(&TranscriptDelta {
                text: "gap".into(),
                offset: 3,
                is_final: false,
            })
            .is_err());
    }

    #[test]
    fn host_dto_serialization_names_and_units_are_stable() {
        let session = SessionId::new();
        let snapshot = DictationStateSnapshot {
            phase: DictationPhase::Transcribing,
            session_id: Some(session),
            elapsed_ms: 1500,
            level: 0.5,
            message: None,
            recording_ready: true,
        };
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["phase"], "transcribing");
        assert_eq!(value["elapsedMs"], 1500);
        assert_eq!(value["level"], 0.5);
        assert!(value.get("sessionId").is_some());
        assert!(value.get("elapsed_ms").is_none());

        let result = serde_json::to_value(DictationResult {
            session_id: session,
            raw_text: "raw".to_string(),
            polished_text: "polished".to_string(),
            polish_source: None,
            duration_ms: 1200,
            inserted: InsertStatus::CopiedFallback,
        })
        .unwrap();
        assert_eq!(result["inserted"], "copiedFallback");
        assert_eq!(result["rawText"], "raw");
        assert_eq!(result["polishedText"], "polished");
        assert_eq!(result["durationMs"], 1200);
    }

    #[test]
    fn dictation_result_accepts_the_pre_v1_json_fixture() {
        let session = SessionId::new();
        let fixture = serde_json::json!({
            "sessionId": session,
            "rawText": "raw",
            "polishedText": "polished",
            "inserted": "inserted"
        });

        let result: DictationResult = serde_json::from_value(fixture).unwrap();

        assert_eq!(result.session_id, session);
        assert_eq!(result.polish_source, None);
        assert_eq!(result.duration_ms, 0);
    }

    #[test]
    fn capability_dto_uses_host_facing_camel_case_fields() {
        let value = serde_json::to_value(PlatformCapabilities {
            platform: "linux".to_string(),
            supports_desktop_hotkey: true,
            supports_tray: false,
            supports_overlay: false,
            supports_ime_input: true,
            supports_local_asr: true,
            supports_local_qwen3_mlx: false,
            supports_in_app_dictation: false,
            supports_auto_update: false,
        })
        .unwrap();
        assert_eq!(value["supportsDesktopHotkey"], true);
        assert_eq!(value["supportsImeInput"], true);
        assert!(value.get("supports_desktop_hotkey").is_none());
    }
}
