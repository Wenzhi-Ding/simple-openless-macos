//! Framework-independent OpenLess application core.
//!
//! This crate deliberately has no dependency on Tauri, WebView, egui, or eframe.
//! Host applications provide platform integrations through [`ports`] and consume
//! semantic state changes through [`events`].

pub mod activity;
pub mod android_types;
pub mod api;
pub mod asr;
pub mod audio;
pub mod auxiliary;
pub mod cli;
pub mod config;
pub mod correction;
pub mod credentials;
pub mod dictation_context;
pub mod dictation_engine;
pub mod domains;
pub mod edit_plan;
pub mod errors;
pub mod events;
pub mod history;
pub mod host_document;
mod hotkey_interpreter;
pub mod output_cleaning;
mod persistence;
pub mod polish;
pub mod polish_diagnostics;
pub mod ports;
pub mod preferences;
pub mod provider_registry;
pub mod prompt_compose;
pub mod prompts;
mod selection_service;
pub mod settings;
pub mod screen_ocr;
pub mod shared_types;
pub mod silence_auto_stop;
pub mod streaming_insert;
mod style_pack_archive;
pub mod style_pack_store;
pub mod style_packs;
pub mod testing;
pub mod types;
pub mod vocabulary;

pub mod local_asr_catalog;
mod cancellation;
mod voice_session;
mod local_asr_service;
mod shortcut_types;

/// Version of the public host-facing DTO/event/lifecycle contract.
///
/// This is independent from the application release version.  Increment the
/// major component for a breaking host contract change and document the
/// migration in `docs/linux-egui-backend-contract.md`.
pub const BACKEND_CONTRACT_VERSION: &str = "2.0.0";

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

pub use cancellation::CancellationToken;
pub use activity::{ActivityDay, ActivityStore, DayStats};
pub use api::{
    BackendRepositories, BackendSnapshot, CliDispatchOutcome, DictationHotkeyDispatchOptions,
    DictationHotkeyEdge, OpenLessBackend, StartupSnapshot,
};
pub use audio::{encode_dictation_wav, NormalizedPcmChunk, PcmNormalizer, DICTATION_SAMPLE_RATE};
pub use auxiliary::{
    AsrCallLabel, AuxiliaryApi, RepolishRequest, RetranscriptionFailure, RetranscriptionResult,
};
pub use cli::{
    decode_launch_intent, encode_launch_intent, parse_cli_intent, CliIntent, LaunchIntent,
};

pub use config::{
    BackendConfig, BackendDependencies, Clock, SystemClock, TaskSpawner, TokioTaskSpawner,
};
pub use correction::{apply_correction_rules, CorrectionRuleStore};
pub use credentials::{
    ChannelKind, ChannelMutation, ChannelMutationResult, ChannelSummary, ChannelTestSummary,
    CredentialDirectory, CredentialKey, CredentialMetadata, CredentialMetadataStore,
    CredentialNamespace, CredentialStore, InMemoryCredentialStore, ProviderChannelId, ProviderSlot,
    ProviderType, SecretValue, UnsupportedCredentialStore,
};
pub use dictation_context::{
    build_asr_prompt, eligible_polish_context_turns, DictationAudioSource, DictationContext,
    DictationInsertionContext, DictationPolishContext, DictationStartOptions, DictationStopOptions,
    PolishHistoryTurn, ProviderInvocation, RecordingPlan, ASR_PROMPT_CHAR_BUDGET,
};
pub use dictation_engine::{PipelineDictationEngine, PolishFailurePolicy};
pub use domains::*;
pub use edit_plan::{
    apply_edit_plan, parse_edit_plan, parse_edit_plan_json, parse_edit_plan_xml, EditApplyError,
    EditOperation, EditPlan, RegexFlags,
};
pub use errors::{BackendError, BackendErrorCode};
pub use events::{
    BackendEvent, BackendEventKind, BackendEventPublisher, EventRecvError, EventReplay,
    EventSubscription, LocalAsrPreparePhase, LocalAsrPrepareProgress, LocalAsrRuntimeKind,
    RecordingControlAction, RecordingControlRequest,
};
pub use history::{HistoryStore, HISTORY_CAP};
pub use local_asr_catalog::{
    normalize_foundry_language_hint, normalize_sherpa_language_hint, FoundryRuntimeSource,
    LocalAsrExecutionMode, LocalAsrMirror, LocalAsrModelId, LocalAsrRuntime, LocalAsrTarget,
    SherpaModelFamily,
};
pub use local_asr_service::{
    directory_has_model_files, local_model_dir, LocalAsrRuntimeLease, ModelPrepareProgressSink,
    ModelRuntimeAdapter, NativeModelState, StorageRebind,
};
pub use ports::{
    ActiveRecording, AudioConsumer, AudioRecorder, DictationEngine, DirectoryResourceResolver,
    EditObservationAdapter, EditObservationSink, EngineFailure, EngineFailureStage, EngineProgress,
    EngineProgressSink, EngineResult, EngineStage, HostAction, HostActions, HostContextAdapter,
    HostContextCapture, InsertOutcome, InsertWriteResult, NoopEditObservationAdapter,
    NoopHostActions, NoopHostContextAdapter, PolishAttempt, PolishOutput, RecordingArchive,
    RecordingControlSink, RecordingEvent, RecordingProgressSink, ResourceResolver,
    ScreenOcrNotStarted, ScreenOcrOutcome, ScreenOcrSlot, ScreenOcrStart, TextInserter,
    TextInsertionSession,
    TextPolisher, TextStreamChunk, TextStreamSink, TranscriptOutput, TranscriptionEngine,
    TranscriptionSession, UnsupportedTextInserter, VoiceCapture,
};
pub use polish_diagnostics::{
    context_diagnostics, cursor_trace_from_capture, PolishContextDiagnostics,
    PolishContextStatus, PolishContextTrace, PolishDiagnosticError, PolishDiagnosticMessage,
    PolishDiagnostics, PolishDiagnosticsOutcome, PolishDiagnosticsStage, PolishTemplateKind,
    screen_ocr_not_started_trace, screen_ocr_outcome_trace, screen_ocr_pending_trace,
    ADD_BOS_NOTE,
};
pub use preferences::PreferencesStore;
pub use screen_ocr::{
    normalize_screen_ocr, screen_ocr_block, screen_ocr_injection_defense, ScreenOcrPending,
    SCREEN_OCR_CHAR_BUDGET,
};
pub use prompt_compose::{
    assemble_polish_system_prompt, build_hotword_block, build_polish_translate_system_prompt,
    compose_hotword_block_preview, compose_polish_prompts, compose_qa_system_prompt,
    compose_system_prompt, compose_translate_prompts, context_premise,
    split_polish_translate_output, PolishSystemPromptAssembly, POLISH_TRANSLATE_SRC_MARKER,
    POLISH_TRANSLATE_TGT_MARKER,
};
pub use provider_registry::{DictationEngineRouter, TextPolisherRouter, TranscriptionRouter};
pub use settings::*;
pub use shared_types::{
    CapsulePayload, CapsuleState, CapsuleStyle, CredentialsStatus, HotkeyMode, HotkeyStatus,
    PendingCorrection, PlatformCapabilities, SelectionPolishOutputMode, UserPreferences,
};
pub use shortcut_types::{
    binding_from_legacy_trigger, binding_requires_side_aware_hook, bindings_overlap,
    is_side_specific_modifier_tag, legacy_modifier_trigger, normalize_side_modifier_tag,
    reconcile_hotkey_collisions, reject_bare_shift_dictation_shortcut,
    reject_dictation_qa_hotkey_overlap, reject_dictation_translation_hotkey_overlap,
    reject_hotkey_collisions, reject_modifier_only_action_shortcut,
    reject_non_dictation_side_specific_shortcuts, reject_qa_less_computer_hotkey_overlap,
    reject_qa_open_app_hotkey_overlap, reject_qa_switch_style_hotkey_overlap,
    reject_qa_translation_hotkey_overlap, reject_selection_polish_hotkey_collisions,
    reject_side_specific_non_dictation, reject_style_pack_hotkey_conflicts,
    sync_dictation_hotkey_legacy_fields, validate_shortcut_binding, ShortcutBindingError,
    SIDE_SPECIFIC_NON_DICTATION_MSG,
};
pub use silence_auto_stop::{SilenceAutoStop, SilenceDecision};
pub use streaming_insert::{
    append_typed_prefix, streaming_insert_eligible, StreamingInsertState,
    STREAMING_FLUSH_INTERVAL_MS,
};
pub use style_pack_archive::{
    validate_style_pack_archive_bytes, STYLE_PACK_ARCHIVE_MAX_COMPRESSED_BYTES,
};
pub use style_pack_store::{
    enabled_modes_from_style_packs, migrate_style_packs_from_preferences,
    sync_style_pack_preferences, StylePackStore,
};
pub use style_packs::*;
pub use types::InsertStatus as DictationInsertStatus;
pub use types::{
    CorrectionRule, DictationPhase, DictationResult, DictationSession, DictationStateSnapshot,
    DictionaryEntry, DownloadProgress, HistoryChange, HistoryInsertStatus, HistorySource,
    InsertFallbackPayload, NotificationLevel, NotificationPayload, PermissionSnapshot,
    PermissionState, PolishDelta, PolishMode, PreferencesChange, RuleSource,
    SelectionVoiceIntentMode, SelectionVoiceManualIntent, SessionId, StylePackChange,
    TranscriptAccumulator, TranscriptDelta, VocabPreset, VocabPresetStore, VocabularyChange,
};
pub use vocabulary::{list_vocab_presets, save_vocab_presets, DictionaryStore};
