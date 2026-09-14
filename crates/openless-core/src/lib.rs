//! Framework-independent OpenLess application core.
//!
//! This crate deliberately has no dependency on Tauri, WebView, egui, or eframe.
//! Host applications provide platform integrations through [`ports`] and consume
//! semantic state changes through [`events`].

pub mod activity;
pub mod api;
pub mod asr;
pub mod audio;
pub mod auxiliary;
pub mod cli;
pub mod config;
pub mod dictation_context;
pub mod dictation_engine;
pub mod domains;
pub mod errors;
pub mod events;
pub mod history;
mod hotkey_interpreter;
pub mod insertion_boundary;
pub mod output_cleaning;
mod persistence;
pub mod polish;
pub mod polish_diagnostics;
pub mod ports;
pub mod preferences;
pub mod prompt_compose;
pub mod prompts;
pub mod provider_registry;
pub mod screen_ocr;
pub mod settings;
pub mod shared_types;
pub mod silence_auto_stop;
#[cfg(test)]
mod test_support;
pub mod types;
pub mod vocabulary;

mod cancellation;
pub mod local_asr_catalog;
mod local_asr_service;
mod shortcut_types;
mod voice_session;

/// Version of the public host-facing DTO/event/lifecycle contract.
///
/// This is independent from the application release version.  Increment the
/// major component for a breaking host contract change and update the canonical fixture.
pub const BACKEND_CONTRACT_VERSION: &str = "4.0.0";

pub fn require_backend_contract_version(version: &str) -> Result<(), errors::BackendError> {
    if version == BACKEND_CONTRACT_VERSION {
        Ok(())
    } else {
        Err(errors::BackendError::new(
            errors::BackendErrorCode::InvalidArgument,
            format!("unsupported backend contract version: {version}"),
        ))
    }
}

pub use activity::{ActivityDay, ActivityStore, DayStats};
pub use api::{
    BackendRepositories, BackendSnapshot, CliDispatchOutcome, DictationHotkeyDispatchOptions,
    DictationHotkeyEdge, OpenLessBackend, StartupSnapshot,
};
pub use audio::{encode_dictation_wav, NormalizedPcmChunk, PcmNormalizer, DICTATION_SAMPLE_RATE};
pub use auxiliary::{
    AsrCallLabel, AuxiliaryApi, RepolishRequest, RetranscriptionFailure, RetranscriptionResult,
};
pub use cancellation::CancellationToken;
pub use cli::{
    decode_launch_intent, encode_launch_intent, parse_cli_intent, CliIntent, LaunchIntent,
};

pub use config::{
    BackendConfig, BackendDependencies, Clock, SystemClock, TaskSpawner, TokioTaskSpawner,
};
pub use dictation_context::{
    build_asr_prompt, eligible_polish_context_turns, DictationAudioSource, DictationContext,
    DictationInsertionContext, DictationPolishContext, DictationStartOptions, PolishHistoryTurn,
    ProviderInvocation, RecordingPlan, ASR_PROMPT_CHAR_BUDGET,
};
pub use dictation_engine::{PipelineDictationEngine, PolishFailurePolicy};
pub use domains::*;
pub use errors::{BackendError, BackendErrorCode};
pub use events::{
    BackendEvent, BackendEventKind, BackendEventPublisher, EventRecvError, EventReplay,
    EventSubscription, LocalAsrPreparePhase, LocalAsrPrepareProgress, LocalAsrRuntimeKind,
    RecordingControlAction, RecordingControlRequest,
};
pub use history::{HistoryStore, HISTORY_CAP};
pub use local_asr_catalog::{
    LocalAsrModelId, LocalAsrRuntime, LocalAsrTarget, LOCAL_QWEN3_MLX_PROVIDER_ID,
    LOCAL_QWEN3_MODEL_ID,
};
pub use local_asr_service::{
    directory_has_model_files, local_model_dir, ModelPrepareProgressSink, ModelRuntimeAdapter,
};
pub use polish_diagnostics::{
    context_diagnostics, cursor_trace_from_capture, screen_ocr_not_started_trace,
    screen_ocr_outcome_trace, screen_ocr_pending_trace, PolishContextDiagnostics,
    PolishContextStatus, PolishContextTrace, PolishDiagnosticError, PolishDiagnosticMessage,
    PolishDiagnostics, PolishDiagnosticsOutcome, PolishDiagnosticsStage, PolishTemplateKind,
    ADD_BOS_NOTE,
};
pub use ports::{
    ActiveRecording, AudioConsumer, AudioRecorder, DictationEngine, DirectoryResourceResolver,
    EngineFailure, EngineFailureStage, EngineProgress, EngineProgressSink, EngineResult,
    EngineStage, HostAction, HostActions, HostContextAdapter, HostContextCapture, InsertOutcome,
    NoopHostActions, NoopHostContextAdapter, PolishAttempt, PolishOutput,
    RecordingArchive, RecordingControlSink, RecordingEvent, RecordingProgressSink,
    ResourceResolver, ScreenOcrNotStarted, ScreenOcrOutcome, ScreenOcrSlot, ScreenOcrStart,
    TextInserter, TextInsertionSession, TextPolisher, TextStreamChunk, TextStreamSink,
    TranscriptOutput, TranscriptionEngine, TranscriptionSession, UnsupportedTextInserter,
    VoiceCapture,
};
pub use preferences::PreferencesStore;
pub use prompt_compose::{
    build_hotword_block, compose_polish_prompts, compose_system_prompt, context_premise,
};
pub use provider_registry::{DictationEngineRouter, TextPolisherRouter, TranscriptionRouter};
pub use screen_ocr::{
    normalize_screen_ocr, screen_ocr_block, screen_ocr_injection_defense, ScreenOcrPending,
    SCREEN_OCR_CHAR_BUDGET,
};
pub use settings::*;
pub use shared_types::{
    CapsulePayload, CapsuleState, CapsuleStyle, HotkeyAdapterKind, HotkeyStatus,
    HotkeyStatusState, PlatformCapabilities, UserPreferences,
};
pub use shortcut_types::{
    binding_from_legacy_trigger, binding_requires_side_aware_hook, bindings_overlap,
    is_side_specific_modifier_tag, legacy_modifier_trigger, normalize_side_modifier_tag,
    reject_bare_shift_dictation_shortcut, reject_side_specific_non_dictation,
    sync_dictation_hotkey_legacy_fields, validate_shortcut_binding, ShortcutBindingError,
    SIDE_SPECIFIC_NON_DICTATION_MSG,
};
pub use silence_auto_stop::{SilenceAutoStop, SilenceDecision};
pub use types::InsertStatus as DictationInsertStatus;
pub use types::{
    DictationPhase, DictationResult, DictationSession, DictationStateSnapshot, DictionaryEntry,
    HistoryChange, HistoryInsertStatus, HistorySource, InsertFallbackPayload, NotificationLevel,
    NotificationPayload, PermissionSnapshot, PermissionState, PreferencesChange,
    SessionId, TranscriptAccumulator, TranscriptDelta, VocabularyChange,
};
pub use vocabulary::DictionaryStore;
