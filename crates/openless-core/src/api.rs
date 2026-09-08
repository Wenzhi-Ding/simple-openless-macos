use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::activity::ActivityDay;
use crate::config::{BackendConfig, BackendDependencies, Clock, SystemClock, TaskSpawner};
use crate::correction::apply_correction_rules;
use crate::credentials::{CredentialKey, SecretValue};
use crate::dictation_context::{
    DictationContext, DictationProviderInvocations, DictationStartOptions, DictationStopOptions,
};
use crate::errors::{BackendError, BackendErrorCode};
use crate::events::{
    BackendEventKind, BackendEventPublisher, EventBus, EventReplay, EventSubscription,
};
use crate::ports::{
    EditObservationSink, EngineFailureStage, EngineProgress, EngineProgressSink, EngineStage,
    HostAction, InsertOutcome, TextInserter, TextInsertionSession, TextStreamSink,
    TranscriptionSession,
};
use crate::shared_types::{
    CredentialsStatus, PendingCorrection, UserPreferences, LEARNED_VOCAB_NOTE,
    MAX_PENDING_CORRECTIONS,
};
use crate::style_pack_store::sync_style_pack_preferences;
use crate::style_packs::StylePack;
use crate::types::{
    CorrectionRule, DictationPhase, DictationResult, DictationSession, DictationStateSnapshot,
    DictionaryEntry, HistoryChange, HistoryInsertStatus, HistorySource, PreferencesChange,
    SessionId, StylePackChange, VocabPresetStore, VocabularyChange,
};
use crate::vocabulary::DictionaryStore;
use crate::{ActivityStore, CorrectionRuleStore, HistoryStore, PreferencesStore, StylePackStore};

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSnapshot {
    pub running: bool,
    pub dictation: DictationStateSnapshot,
    #[serde(default)]
    pub vocabulary_revision: u64,
    #[serde(default)]
    pub history_revision: u64,
    #[serde(default)]
    pub style_pack_revision: u64,
    #[serde(default)]
    pub preferences_revision: u64,
    #[serde(default)]
    pub credentials: CredentialsStatus,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupSnapshot {
    pub contract_version: String,
    pub backend: BackendSnapshot,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum CliDispatchOutcome {
    DictationStarted(SessionId),
    DictationCompleted(DictationResult),
    DictationCancelled,
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationHotkeyEdge {
    Pressed {
        press_id: u64,
        at: std::time::Instant,
    },
    Released {
        press_id: u64,
        at: std::time::Instant,
    },
    Combined {
        press_id: u64,
        at: std::time::Instant,
    },
}


#[derive(Debug, Clone, Default, PartialEq)]
pub struct DictationHotkeyDispatchOptions {
    pub start: DictationStartOptions,
    pub stop: DictationStopOptions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictationContextPurpose {
    Dictation,
    AsrOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum VoiceCapturePhase {
    #[default]
    Recording,
    Finishing,
    Completed,
    Cancelled,
}

/// Stopping the microphone begins provider finalization; it does not close
/// the cancellation window. Dictation retains this shared state until the
/// provider result is committed or cancellation wins.
#[derive(Default)]
struct VoiceCaptureLifecycle {
    phase: std::sync::Mutex<VoiceCapturePhase>,
    resources: std::sync::Mutex<Option<Arc<crate::voice_session::VoiceResourceHold>>>,
}

impl VoiceCaptureLifecycle {
    fn with_resources(resources: Arc<crate::voice_session::VoiceResourceHold>) -> Self {
        Self {
            phase: std::sync::Mutex::default(),
            resources: std::sync::Mutex::new(Some(resources)),
        }
    }

    fn resources(&self) -> Option<Arc<crate::voice_session::VoiceResourceHold>> {
        self.resources
            .lock()
            .expect("voice resource lock poisoned")
            .clone()
    }

    fn release_resources(&self) {
        self.resources
            .lock()
            .expect("voice resource lock poisoned")
            .take();
    }

    fn begin_finish(&self) -> Result<(), BackendError> {
        let mut phase = self
            .phase
            .lock()
            .expect("voice capture lifecycle lock poisoned");
        match *phase {
            VoiceCapturePhase::Recording => {
                *phase = VoiceCapturePhase::Finishing;
                Ok(())
            }
            VoiceCapturePhase::Cancelled => Err(Self::cancelled_error()),
            _ => Err(BackendError::new(
                BackendErrorCode::InvalidState,
                "voice capture is already finishing or completed",
            )),
        }
    }

    fn is_cancelled(&self) -> bool {
        *self
            .phase
            .lock()
            .expect("voice capture lifecycle lock poisoned")
            == VoiceCapturePhase::Cancelled
    }

    /// The winner alone owns provider cancellation. Recorder ownership is
    /// independently transferred with take(), so concurrent finish/cancel can
    /// never stop the native recorder or discard its archive twice.
    fn claim_cancel(&self) -> bool {
        let mut phase = self
            .phase
            .lock()
            .expect("voice capture lifecycle lock poisoned");
        if matches!(
            *phase,
            VoiceCapturePhase::Completed | VoiceCapturePhase::Cancelled
        ) {
            return false;
        }
        *phase = VoiceCapturePhase::Cancelled;
        true
    }

    fn settle<T>(&self, result: Result<T, BackendError>) -> (Result<T, BackendError>, bool) {
        let mut phase = self
            .phase
            .lock()
            .expect("voice capture lifecycle lock poisoned");
        if *phase == VoiceCapturePhase::Cancelled {
            return (Err(Self::cancelled_error()), false);
        }
        let cancel_provider = result.is_err();
        *phase = if cancel_provider {
            VoiceCapturePhase::Cancelled
        } else {
            VoiceCapturePhase::Completed
        };
        (result, cancel_provider)
    }

    fn cancelled_error() -> BackendError {
        BackendError::new(BackendErrorCode::Cancelled, "voice capture was cancelled")
    }
}

/// Committed native effects outlive the caller waiting on their reply. In
/// particular, dropping an IPC future cannot release a hold while a native
/// spawn_blocking recorder stop or decoder is still running.
fn own_voice_effect<T: Send + 'static>(
    spawner: &Arc<dyn TaskSpawner>,
    effect: futures_util::future::BoxFuture<'static, Result<T, BackendError>>,
) -> futures_util::future::BoxFuture<'static, Result<T, BackendError>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    spawner.spawn(Box::pin(async move {
        let _ = sender.send(effect.await);
    }));
    Box::pin(async move {
        receiver.await.map_err(|_| {
            BackendError::new(
                BackendErrorCode::Internal,
                "voice task did not return a result",
            )
        })?
    })
}

struct DictationReservation {
    session_id: SessionId,
    resources: Arc<crate::voice_session::VoiceResourceHold>,
    inserter: Arc<dyn TextInserter>,
}


/// Repository bundle shared by every host-facing facade in one process.
///
/// Tauri's compatibility coordinator and the new core facade must use the same
/// instances; opening the same JSON files twice would create divergent
/// in-memory snapshots even though the paths are identical.
#[derive(Clone)]
pub struct BackendRepositories {
    pub preferences: Arc<PreferencesStore>,
    pub history: Arc<HistoryStore>,
    pub activity: Arc<ActivityStore>,
    pub vocabulary: Arc<DictionaryStore>,
    pub correction_rules: Arc<CorrectionRuleStore>,
    pub style_packs: Arc<StylePackStore>,
}

impl BackendRepositories {
    pub fn open(data_dir: &std::path::Path) -> Result<Self, BackendError> {
        let preferences = Arc::new(PreferencesStore::open(data_dir.join("preferences.json"))?);
        let style_packs = Arc::new(
            StylePackStore::at_data_dir_with_preferences(data_dir, &preferences.get())
                .unwrap_or_else(|_| StylePackStore::in_memory()),
        );
        let mut preference_snapshot = preferences.get();
        if sync_style_pack_preferences(&mut preference_snapshot, &style_packs.list()?) {
            preferences.set(preference_snapshot)?;
        }
        Ok(Self {
            preferences,
            history: Arc::new(HistoryStore::at_data_dir(data_dir)),
            activity: Arc::new(
                ActivityStore::at_data_dir(data_dir).unwrap_or_else(|_| ActivityStore::in_memory()),
            ),
            vocabulary: Arc::new(DictionaryStore::at_data_dir(data_dir)),
            correction_rules: Arc::new(CorrectionRuleStore::at_data_dir(data_dir)),
            style_packs,
        })
    }
}

struct MutableState {
    running: bool,
    dictation: DictationStateSnapshot,
    dictation_context: Option<Arc<DictationContext>>,
    credentials: CredentialsStatus,
    transcripts: HashMap<SessionId, crate::types::TranscriptAccumulator>,
    silence_monitor: Option<SilenceMonitor>,
}

struct SilenceMonitor {
    session_id: SessionId,
    started_at: std::time::Instant,
    detector: crate::silence_auto_stop::SilenceAutoStop,
}

// Preparation itself can switch a native input source. Register one shared
// future before polling it so cancellation can join and restore even when the
// original start caller is dropped. Synchronous progress only sees ready values.
type TextInsertionPreparation = futures_util::future::Shared<
    futures_util::future::BoxFuture<'static, Result<Arc<ActiveTextInsertion>, BackendError>>,
>;

struct ActiveTextInsertion {
    platform: Arc<dyn TextInsertionSession>,
    streaming: bool,
    script: crate::shared_types::ChineseScriptPreference,
    save_streamed_text_to_clipboard: bool,
    state: Mutex<ActiveTextInsertionState>,
    drained: tokio::sync::Notify,
    task_spawner: Arc<dyn TaskSpawner>,
    _resources: Arc<crate::voice_session::VoiceResourceHold>,
    cancel_result: std::sync::OnceLock<Result<(), BackendError>>,
    // 0: open, 1: native finalization in flight, 2: cancellation owns cleanup,
    // 3: native finalization settled, 4: cancellation cleanup settled.
    // Cancellation must join state 1 or 2 rather
    // than release the next voice session while its committed input continues.
    terminal: AtomicU8,
}

#[derive(Default)]
struct ActiveTextInsertionState {
    stream: crate::streaming_insert::StreamingInsertState,
    scheduled: bool,
}

impl ActiveTextInsertion {
    fn new(
        platform: Arc<dyn TextInsertionSession>,
        context: &DictationContext,
        task_spawner: Arc<dyn TaskSpawner>,
        resources: Arc<crate::voice_session::VoiceResourceHold>,
    ) -> Arc<Self> {
        let windows_non_streaming = cfg!(target_os = "windows")
            && context.insertion.windows_insertion_mode
                != crate::shared_types::WindowsInsertionMode::SendInput;
        let platform_streaming = platform.supports_streaming();
        Arc::new(Self {
            platform,
            streaming: context.uses_llm_polisher()
                && platform_streaming
                && crate::streaming_insert::streaming_insert_eligible(
                    context.insertion.streaming,
                    context.polish.translation_active,
                    context.polish.chinese_script_preference
                        == crate::shared_types::ChineseScriptPreference::Traditional,
                    windows_non_streaming,
                ),
            script: context.polish.chinese_script_preference,
            save_streamed_text_to_clipboard: context.insertion.save_streamed_text_to_clipboard,
            state: Mutex::new(ActiveTextInsertionState::default()),
            drained: tokio::sync::Notify::new(),
            task_spawner,
            _resources: resources,
            cancel_result: std::sync::OnceLock::new(),
            terminal: AtomicU8::new(0),
        })
    }

    fn push(self: &Arc<Self>, delta: &crate::types::PolishDelta) {
        if !self.streaming || delta.is_final {
            return;
        }
        let text =
            crate::streaming_insert::apply_chinese_script_preference(&delta.text, self.script);
        let should_spawn = {
            let mut state = self.state.lock().expect("text insertion lock poisoned");
            state.stream.push_delta(delta.offset, &text);
            if state.scheduled || state.stream.pending.is_empty() || state.stream.failed.is_some() {
                false
            } else {
                state.scheduled = true;
                true
            }
        };
        if should_spawn {
            let insertion = Arc::clone(self);
            self.task_spawner
                .spawn(Box::pin(async move { insertion.flush_loop().await }));
        }
    }

    fn has_written_text(&self) -> bool {
        !self
            .state
            .lock()
            .expect("text insertion lock poisoned")
            .stream
            .typed_text
            .is_empty()
    }

    async fn flush_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(
                crate::streaming_insert::STREAMING_FLUSH_INTERVAL_MS,
            ))
            .await;
            let delta = {
                let mut state = self.state.lock().expect("text insertion lock poisoned");
                if state.stream.failed.is_some() {
                    state.scheduled = false;
                    self.drained.notify_waiters();
                    return;
                }
                std::mem::take(&mut state.stream.pending)
            };
            let expected = delta.chars().count();
            let result = if delta.is_empty() {
                Ok(crate::ports::InsertWriteResult { written_chars: 0 })
            } else {
                self.platform.write(delta.clone()).await
            };
            let mut state = self.state.lock().expect("text insertion lock poisoned");
            match result {
                Ok(result) if result.written_chars >= expected => {
                    state.stream.typed_text.push_str(&delta);
                }
                Ok(result) => {
                    let written = crate::streaming_insert::append_typed_prefix(
                        &mut state.stream.typed_text,
                        &delta,
                        result.written_chars,
                    );
                    state.stream.failed = Some(format!(
                        "host inserted only {written}/{expected} characters"
                    ));
                }
                Err(error) => state.stream.failed = Some(error.to_string()),
            }
            if state.stream.failed.is_some() || state.stream.pending.is_empty() {
                state.scheduled = false;
                self.drained.notify_waiters();
                return;
            }
        }
    }

    async fn wait_for_stream_drain(&self) {
        loop {
            let notified = self.drained.notified();
            if !self
                .state
                .lock()
                .expect("text insertion lock poisoned")
                .scheduled
            {
                break;
            }
            notified.await;
        }
    }

    async fn finish(self: &Arc<Self>, final_text: String) -> Result<InsertOutcome, BackendError> {
        self.wait_for_stream_drain().await;
        self.terminal
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                BackendError::new(
                    BackendErrorCode::Cancelled,
                    "text insertion session was cancelled before completion",
                )
            })?;
        // Once native finalization is committed, a dropped IPC/stop future
        // must not abandon its input-source restoration or leave terminal=1
        // forever. The host executor owns this effect until it settles; the
        // caller only owns its response, and cancel() joins the same operation.
        let insertion = Arc::clone(self);
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.task_spawner.spawn(Box::pin(async move {
            let result = insertion.finish_committed(final_text).await;
            insertion.terminal.store(3, Ordering::Release);
            insertion.drained.notify_waiters();
            let _ = result_tx.send(result);
        }));
        result_rx.await.map_err(|_| {
            BackendError::new(
                BackendErrorCode::Internal,
                "native insertion finalization task did not return a result",
            )
        })?
    }

    async fn finish_committed(&self, final_text: String) -> Result<InsertOutcome, BackendError> {
        let reconciliation = self
            .state
            .lock()
            .expect("text insertion lock poisoned")
            .stream
            .reconcile_final(&final_text);
        use crate::streaming_insert::FinalReconciliation;
        match reconciliation {
            FinalReconciliation::InsertFinal(text) => self.platform.finish(text).await,
            FinalReconciliation::WriteTail(tail) => {
                let expected = tail.chars().count();
                let written = self.platform.write(tail).await;
                let complete = matches!(written, Ok(result) if result.written_chars >= expected);
                if complete {
                    if self.save_streamed_text_to_clipboard {
                        if let Err(error) = self.platform.copy(final_text).await {
                            log::warn!("failed to preserve streamed text on clipboard: {error}");
                        }
                    }
                    self.platform.finish(String::new()).await
                } else {
                    self.finish_with_fallback(final_text).await
                }
            }
            FinalReconciliation::Complete => {
                if self.save_streamed_text_to_clipboard {
                    if let Err(error) = self.platform.copy(final_text).await {
                        log::warn!("failed to preserve streamed text on clipboard: {error}");
                    }
                }
                self.platform.finish(String::new()).await
            }
            FinalReconciliation::CopyFallback(text) => self.finish_with_fallback(text).await,
        }
    }

    async fn finish_with_fallback(&self, text: String) -> Result<InsertOutcome, BackendError> {
        let copied = self.platform.copy(text).await;
        let closed = self.platform.finish(String::new()).await;
        copied?;
        closed?;
        Ok(InsertOutcome::CopiedFallback)
    }

    async fn cancel(self: &Arc<Self>) -> Result<(), BackendError> {
        {
            let mut state = self.state.lock().expect("text insertion lock poisoned");
            state.stream.pending.clear();
            state.stream.failed = Some("text insertion session was cancelled".to_string());
        }
        if self
            .terminal
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // A native write may already be sending an indivisible chunk.
            // Discarding queued text cannot stop that effect. Drain it
            // before restoring TIS/focus, as the 1.x typer join did; otherwise
            // the remaining keys can be interpreted by the restored IME.
            let insertion = Arc::clone(self);
            self.task_spawner.spawn(Box::pin(async move {
                insertion.wait_for_stream_drain().await;
                let result = insertion.platform.cancel().await;
                let _ = insertion.cancel_result.set(result);
                insertion.terminal.store(4, Ordering::Release);
                insertion.drained.notify_waiters();
            }));
        }
        // A late starter and an explicit cancel can reach this same session.
        // Both must await the single cleanup; treating the second call as an
        // immediate success would admit B while A still restores its source.
        loop {
            let notified = self.drained.notified();
            match self.terminal.load(Ordering::Acquire) {
                1 | 2 => notified.await,
                4 => {
                    return self
                        .cancel_result
                        .get()
                        .expect("cleanup result precedes terminal")
                        .clone()
                }
                _ => return Ok(()),
            }
        }
    }
}

struct BackendEngineProgress {
    events: Arc<EventBus>,
    state: Arc<RwLock<MutableState>>,
    phase_changed: Arc<tokio::sync::Notify>,
    text_insertions: Arc<Mutex<HashMap<SessionId, TextInsertionPreparation>>>,
}

impl EngineProgressSink for BackendEngineProgress {
    fn publish(&self, session_id: SessionId, progress: EngineProgress) -> Result<(), BackendError> {
        match progress {
            EngineProgress::RecordingLevel { elapsed_ms, level } => {
                let mut state = self.state.write().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                if !matches!(
                    state.dictation.phase,
                    DictationPhase::Starting | DictationPhase::Recording
                ) {
                    return Err(BackendError::new(
                        BackendErrorCode::InvalidState,
                        "recording progress arrived after recording stopped",
                    ));
                }
                let level = if level.is_finite() {
                    level.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let silence_decision = state
                    .silence_monitor
                    .as_mut()
                    .filter(|monitor| monitor.session_id == session_id)
                    .and_then(|monitor| {
                        monitor.detector.on_level(
                            level,
                            monitor.started_at + std::time::Duration::from_millis(elapsed_ms),
                        )
                    });
                if !state.dictation.recording_ready
                    || state.dictation.elapsed_ms != elapsed_ms
                    || state.dictation.level != level
                {
                    // A silent first frame at 0 ms is still proof that capture
                    // is live. Do not deduplicate it against initial zeroes.
                    state.dictation.recording_ready = true;
                    state.dictation.elapsed_ms = elapsed_ms;
                    state.dictation.level = level;
                    self.events.publish(
                        Some(session_id),
                        BackendEventKind::DictationStateChanged(state.dictation.clone()),
                    );
                }
                drop(state);
                if let Some(decision) = silence_decision {
                    let action = match decision {
                        crate::silence_auto_stop::SilenceDecision::Stop => {
                            crate::events::RecordingControlAction::Stop
                        }
                        crate::silence_auto_stop::SilenceDecision::Cancel => {
                            crate::events::RecordingControlAction::Cancel
                        }
                    };
                    self.events.publish(
                        Some(session_id),
                        BackendEventKind::RecordingControlRequested(
                            crate::events::RecordingControlRequest { session_id, action },
                        ),
                    );
                }
            }
            EngineProgress::RecordingFault(error) => {
                let mut state = self.state.write().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                state.dictation.phase = DictationPhase::Failed;
                state.dictation.message = Some(error.message);
                self.events.publish(
                    Some(session_id),
                    BackendEventKind::DictationStateChanged(state.dictation.clone()),
                );
                self.phase_changed.notify_waiters();
            }
            EngineProgress::Notification(notification) => {
                let state = self.state.read().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                drop(state);
                self.events.publish(
                    Some(session_id),
                    BackendEventKind::Notification(notification),
                );
            }
            EngineProgress::Stage(stage) => {
                let mut state = self.state.write().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                let phase = match stage {
                    EngineStage::Transcribing => DictationPhase::Transcribing,
                    EngineStage::Polishing => DictationPhase::Polishing,
                };
                if state.dictation.phase != phase {
                    state.dictation.phase = phase;
                    self.events.publish(
                        Some(session_id),
                        BackendEventKind::DictationStateChanged(state.dictation.clone()),
                    );
                    self.phase_changed.notify_waiters();
                }
            }
            EngineProgress::TranscriptDelta(delta) => {
                let mut state = self.state.write().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                if !matches!(
                    state.dictation.phase,
                    DictationPhase::Starting
                        | DictationPhase::Recording
                        | DictationPhase::Transcribing
                ) {
                    return Err(BackendError::new(
                        BackendErrorCode::InvalidState,
                        "transcript delta arrived after transcription completed",
                    ));
                }
                state
                    .transcripts
                    .entry(session_id)
                    .or_default()
                    .apply(&delta)?;
                drop(state);
                self.events
                    .publish(Some(session_id), BackendEventKind::TranscriptDelta(delta));
            }
            EngineProgress::PolishDelta(delta) => {
                let state = self.state.read().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                if state.dictation.phase != DictationPhase::Polishing {
                    return Err(BackendError::new(
                        BackendErrorCode::InvalidState,
                        "polish delta arrived outside the polishing phase",
                    ));
                }
                drop(state);
                if let Some(insertion) = self
                    .text_insertions
                    .lock()
                    .expect("text insertion registry lock poisoned")
                    .get(&session_id)
                    .and_then(|preparation| preparation.peek())
                    .and_then(|result| result.as_ref().ok())
                    .cloned()
                {
                    insertion.push(&delta);
                }
                self.events
                    .publish(Some(session_id), BackendEventKind::PolishDelta(delta));
            }
        }
        Ok(())
    }
}

pub struct OpenLessBackend {
    config: BackendConfig,
    deps: BackendDependencies,
    clock: Arc<dyn Clock>,
    events: Arc<EventBus>,
    state: Arc<RwLock<MutableState>>,
    phase_changed: Arc<tokio::sync::Notify>,
    hotkey: Mutex<crate::hotkey_interpreter::HotkeyInterpreter>,
    hotkey_dispatch_gate: tokio::sync::Mutex<()>,
    vocabulary: Arc<DictionaryStore>,
    correction_rules: Arc<CorrectionRuleStore>,
    vocabulary_revision: Arc<AtomicU64>,
    history: Arc<HistoryStore>,
    history_revision: Arc<AtomicU64>,
    activity: Arc<ActivityStore>,
    style_packs: Arc<StylePackStore>,
    style_pack_revision: Arc<AtomicU64>,
    preferences: Arc<PreferencesStore>,
    preferences_revision: Arc<AtomicU64>,
    settings_write_gate: Mutex<()>,
    pending_corrections: Arc<Mutex<Vec<PendingCorrection>>>,
    edit_observation_generation: Arc<AtomicU64>,
    text_insertions: Arc<Mutex<HashMap<SessionId, TextInsertionPreparation>>>,
    voice_sessions: Arc<crate::voice_session::VoiceSessionGate>,
}

struct HistoryProviderAttribution {
    asr_provider: Option<String>,
    asr_model: Option<String>,
    llm_provider: Option<String>,
    llm_model: Option<String>,
    asr_ms: Option<u64>,
    polish_ms: Option<u64>,
}

struct CoreEditObservationSink {
    expected_generation: u64,
    generation: Arc<AtomicU64>,
    typed_text: String,
    pending: Arc<Mutex<Vec<PendingCorrection>>>,
    events: Arc<EventBus>,
}

impl EditObservationSink for CoreEditObservationSink {
    fn publish(&self, edit: crate::host_document::EditPair) -> bool {
        // Dropping the native watcher is asynchronous on macOS: a queued AX
        // callback may still arrive after the next session starts. The Core
        // generation is therefore the authoritative stale-report barrier.
        if self.generation.load(Ordering::Acquire) != self.expected_generation {
            return false;
        }
        if !crate::host_document::edit_is_within_typed_text(&edit, &self.typed_text) {
            return false;
        }
        let Some(rule) = crate::host_document::learned_rule(&edit) else {
            return false;
        };
        if let Err(error) = queue_pending_correction_state(
            &self.pending,
            &self.events,
            rule.pattern,
            rule.replacement,
        ) {
            log::warn!("failed to queue observed correction: {error}");
        }
        true
    }
}

fn queue_pending_correction_state(
    pending: &Arc<Mutex<Vec<PendingCorrection>>>,
    events: &Arc<EventBus>,
    pattern: String,
    replacement: String,
) -> Result<Option<PendingCorrection>, BackendError> {
    if pattern.trim().is_empty() || replacement.trim().is_empty() {
        return Err(BackendError::new(
            BackendErrorCode::InvalidArgument,
            "correction pattern and replacement are required",
        ));
    }
    let (suggestion, snapshot) = {
        let mut pending = pending.lock().expect("pending correction lock poisoned");
        if pending
            .iter()
            .any(|item| item.pattern == pattern && item.replacement == replacement)
        {
            return Ok(None);
        }
        if pending.len() >= MAX_PENDING_CORRECTIONS {
            pending.remove(0);
        }
        let suggestion = PendingCorrection {
            id: uuid::Uuid::new_v4().to_string(),
            pattern,
            replacement,
        };
        pending.push(suggestion.clone());
        (suggestion, pending.clone())
    };
    events.publish(
        None,
        BackendEventKind::VocabularySuggestionsChanged(snapshot),
    );
    Ok(Some(suggestion))
}

fn settings_transaction_error(
    mut primary: BackendError,
    compensation_errors: Vec<BackendError>,
) -> BackendError {
    if compensation_errors.is_empty() {
        return primary;
    }
    primary.details = Some(serde_json::json!({
        "primaryError": primary.clone(),
        "compensationErrors": compensation_errors,
    }));
    primary
}

impl HistoryProviderAttribution {
    fn from_context(
        context: &DictationContext,
        llm_used: bool,
        asr_ms: Option<u64>,
        polish_ms: Option<u64>,
        asr_call_label: Option<&crate::auxiliary::AsrCallLabel>,
        llm_call_label: Option<&crate::polish::LlmCallLabel>,
    ) -> Self {
        match context.pipeline_mode {
            crate::shared_types::PipelineMode::Traditional => Self {
                asr_provider: Some(
                    asr_call_label
                        .map(|label| label.provider.clone())
                        .unwrap_or_else(|| context.asr.provider_id.clone()),
                ),
                asr_model: asr_call_label
                    .and_then(|label| label.model.clone())
                    .or_else(|| context.asr.model.clone()),
                llm_provider: llm_used.then(|| {
                    llm_call_label
                        .map(|label| label.provider.clone())
                        .unwrap_or_else(|| context.llm.provider_id.clone())
                }),
                llm_model: llm_used
                    .then(|| {
                        llm_call_label
                            .map(|label| label.model.clone())
                            .or_else(|| context.llm.model.clone())
                    })
                    .flatten(),
                asr_ms,
                polish_ms,
            },
            crate::shared_types::PipelineMode::Multimodal => Self {
                asr_provider: None,
                asr_model: None,
                llm_provider: Some(context.omni.provider_id.clone()),
                llm_model: context.omni.model.clone(),
                asr_ms: None,
                polish_ms,
            },
        }
    }
}

impl OpenLessBackend {
    pub fn new(config: BackendConfig, deps: BackendDependencies) -> Result<Self, BackendError> {
        if config.data_dir.as_os_str().is_empty() {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "data directory is required",
            ));
        }
        let repositories = BackendRepositories::open(&config.data_dir)?;
        Self::new_with_repositories_and_clock(config, deps, repositories, Arc::new(SystemClock))
    }

    pub fn new_with_clock(
        config: BackendConfig,
        deps: BackendDependencies,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, BackendError> {
        if config.data_dir.as_os_str().is_empty() {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "data directory is required",
            ));
        }
        let repositories = BackendRepositories::open(&config.data_dir)?;
        Self::new_with_repositories_and_clock(config, deps, repositories, clock)
    }

    pub fn new_with_repositories(
        config: BackendConfig,
        deps: BackendDependencies,
        repositories: BackendRepositories,
    ) -> Result<Self, BackendError> {
        Self::new_with_repositories_and_clock(config, deps, repositories, Arc::new(SystemClock))
    }

    pub fn new_with_repositories_and_clock(
        config: BackendConfig,
        mut deps: BackendDependencies,
        repositories: BackendRepositories,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, BackendError> {
        if config.data_dir.as_os_str().is_empty() {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "data directory is required",
            ));
        }
        let events = Arc::new(EventBus::new(256));
        let preferences_revision = Arc::new(AtomicU64::new(0));
        let style_pack_revision = Arc::new(AtomicU64::new(0));
        let history_revision = Arc::new(AtomicU64::new(0));
        let vocabulary_revision = Arc::new(AtomicU64::new(0));
        if let Some(runtime) = deps.local_asr_runtime.take() {
            deps.services.local_asr = Arc::new(crate::local_asr_service::LocalAsrService::new(
                Arc::clone(&repositories.preferences),
                runtime,
                config.data_dir.join("models"),
                BackendEventPublisher::new(Arc::clone(&events)),
                Arc::clone(&preferences_revision),
                Arc::clone(&deps.credential_store),
            ));
        }
        match (
            deps.selection_runtime.take(),
            deps.selection_polisher.take(),
        ) {
            (Some(runtime), Some(polisher)) => {
                deps.services.selection =
                    Arc::new(crate::selection_service::SelectionService::new(
                        crate::selection_service::SelectionServiceDependencies {
                            preferences: Arc::clone(&repositories.preferences),
                            style_packs: Arc::clone(&repositories.style_packs),
                            runtime,
                            polisher,
                            host_actions: Arc::clone(&deps.host_actions),
                            events: BackendEventPublisher::new(Arc::clone(&events)),
                            history: Arc::clone(&repositories.history),
                            history_revision: Arc::clone(&history_revision),
                            clock: Arc::clone(&clock),
                            vocabulary: Arc::clone(&repositories.vocabulary),
                            vocabulary_revision: Arc::clone(&vocabulary_revision),
                            correction_rules: Arc::clone(&repositories.correction_rules),
                            activity: Arc::clone(&repositories.activity),
                            credential_store: Arc::clone(&deps.credential_store),
                        },
                    ));
            }
            (None, None) => {}
            _ => {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidArgument,
                    "selection runtime and polisher must be configured together",
                ));
            }
        }
        if let Some((polisher, transcription)) = deps.services.take_auxiliary_runtime() {
            deps.services.auxiliary = Arc::new(crate::auxiliary::AuxiliaryService::new(
                Arc::clone(&repositories.preferences),
                Arc::clone(&repositories.style_packs),
                Arc::clone(&repositories.vocabulary),
                Arc::clone(&deps.credential_store),
                polisher,
                transcription,
                Arc::clone(&deps.task_spawner),
            ));
        }
        Ok(Self {
            config,
            deps,
            clock,
            events,
            state: Arc::new(RwLock::new(MutableState {
                running: false,
                dictation: DictationStateSnapshot::default(),
                dictation_context: None,
                credentials: CredentialsStatus::default(),
                transcripts: HashMap::new(),
                silence_monitor: None,
            })),
            phase_changed: Arc::new(tokio::sync::Notify::new()),
            hotkey: Mutex::new(crate::hotkey_interpreter::HotkeyInterpreter::default()),
            hotkey_dispatch_gate: tokio::sync::Mutex::new(()),
            vocabulary: repositories.vocabulary,
            correction_rules: repositories.correction_rules,
            vocabulary_revision,
            history: repositories.history,
            history_revision,
            activity: repositories.activity,
            style_packs: repositories.style_packs,
            style_pack_revision,
            preferences: repositories.preferences,
            preferences_revision,
            settings_write_gate: Mutex::new(()),
            pending_corrections: Arc::new(Mutex::new(Vec::new())),
            edit_observation_generation: Arc::new(AtomicU64::new(0)),
            text_insertions: Arc::new(Mutex::new(HashMap::new())),
            voice_sessions: Arc::new(crate::voice_session::VoiceSessionGate::default()),
        })
    }

    pub fn repositories(&self) -> BackendRepositories {
        BackendRepositories {
            preferences: Arc::clone(&self.preferences),
            history: Arc::clone(&self.history),
            activity: Arc::clone(&self.activity),
            vocabulary: Arc::clone(&self.vocabulary),
            correction_rules: Arc::clone(&self.correction_rules),
            style_packs: Arc::clone(&self.style_packs),
        }
    }

    pub fn config(&self) -> &BackendConfig {
        &self.config
    }

    /// Return the versioned domain interfaces used by non-Tauri hosts.
    ///
    /// Each service is an independently replaceable port.  A missing adapter
    /// returns `BackendErrorCode::Unsupported`; callers never need to inspect
    /// the concrete implementation.
    pub fn services(&self) -> &crate::domains::BackendServices {
        &self.deps.services
    }


    /// Capture the immutable response-provider/preferences snapshot for a QA
    /// text turn. ASR is not required; QA voice captures both stages through
    /// `start_qa_voice_capture`. This is an adapter seam, not a UI use-case.
    #[doc(hidden)]
    pub async fn capture_host_dictation_context(
        &self,
        options: DictationStartOptions,
    ) -> Result<Arc<DictationContext>, BackendError> {
        Ok(Arc::new(
            self.capture_dictation_context(&options, DictationContextPurpose::AsrOnly)
                .await?,
        ))
    }

    pub fn subscribe(&self) -> EventSubscription {
        self.events.subscribe()
    }

    /// Replay the bounded instance-local event tail after `sequence`.
    ///
    /// Hosts use this after a cold UI mount or a lag notification. A truncated
    /// result means the caller must rebuild its complete view model from the
    /// current snapshots before applying the returned tail.
    pub fn replay_events_after(&self, sequence: u64) -> EventReplay {
        self.events.replay_after(sequence)
    }

    /// Return a typed publisher for platform/transport Adapters that need to
    /// report progress or capability changes on the backend event stream.
    pub fn event_publisher(&self) -> BackendEventPublisher {
        BackendEventPublisher::new(Arc::clone(&self.events))
    }

    pub fn request_host_action(&self, action: HostAction) -> Result<(), BackendError> {
        self.deps.host_actions.request(action)
    }

    fn engine_progress_sink(&self) -> Arc<dyn EngineProgressSink> {
        Arc::new(BackendEngineProgress {
            events: Arc::clone(&self.events),
            state: Arc::clone(&self.state),
            phase_changed: Arc::clone(&self.phase_changed),
            text_insertions: Arc::clone(&self.text_insertions),
        })
    }

    pub async fn start(&self) -> Result<StartupSnapshot, BackendError> {
        let credentials = self
            .deps
            .credential_store
            .status(self.get_preferences())
            .await?;
        let mut state = self.state.write().expect("backend state lock poisoned");
        state.credentials = credentials;
        if state.running {
            return Ok(StartupSnapshot {
                contract_version: crate::BACKEND_CONTRACT_VERSION.to_string(),
                backend: BackendSnapshot {
                    running: true,
                    dictation: state.dictation.clone(),
                    vocabulary_revision: self.vocabulary_revision.load(Ordering::Acquire),
                    history_revision: self.history_revision.load(Ordering::Acquire),
                    style_pack_revision: self.style_pack_revision.load(Ordering::Acquire),
                    preferences_revision: self.preferences_revision.load(Ordering::Acquire),
                    credentials: state.credentials.clone(),
                },
            });
        }
        state.running = true;
        self.events.publish(None, BackendEventKind::BackendStarted);
        Ok(StartupSnapshot {
            contract_version: crate::BACKEND_CONTRACT_VERSION.to_string(),
            backend: BackendSnapshot {
                running: true,
                dictation: state.dictation.clone(),
                vocabulary_revision: self.vocabulary_revision.load(Ordering::Acquire),
                history_revision: self.history_revision.load(Ordering::Acquire),
                style_pack_revision: self.style_pack_revision.load(Ordering::Acquire),
                preferences_revision: self.preferences_revision.load(Ordering::Acquire),
                credentials: state.credentials.clone(),
            },
        })
    }

    pub async fn shutdown(&self) -> Result<(), BackendError> {
        let active_session = {
            let mut state = self.state.write().expect("backend state lock poisoned");
            if !state.running {
                return Ok(());
            }
            let active_session = state.dictation.session_id;
            self.events.publish(None, BackendEventKind::BackendStopping);
            if active_session.is_some() {
                state.dictation.phase = DictationPhase::Cancelled;
                self.events.publish(
                    state.dictation.session_id,
                    BackendEventKind::DictationStateChanged(state.dictation.clone()),
                );
            }
            state.running = false;
            state.dictation = DictationStateSnapshot::default();
            if let Some(context) = state.dictation_context.as_ref() {
                context.discard_pending_screen_ocr();
            }
            state.dictation_context = None;
            state.silence_monitor = None;
            state.transcripts.clear();
            self.phase_changed.notify_waiters();
            active_session
        };
        self.disarm_edit_observation();
        // Start native teardown before awaiting unrelated domain shutdown.
        // This also retains a cleanup hold if the shutdown caller disappears.
        let dictation_cleanup =
            active_session.map(|session_id| self.cancel_session_adapters(session_id));
        let selection_result = match self.deps.services.selection.snapshot().await {
            Ok(snapshot)
                if matches!(
                    snapshot.phase,
                    crate::domains::SelectionPhase::Capturing
                        | crate::domains::SelectionPhase::Preview
                        | crate::domains::SelectionPhase::Applying
                ) =>
            {
                self.deps
                    .services
                    .selection
                    .cancel(snapshot.session_id)
                    .await
            }
            Ok(_) => Ok(()),
            Err(error) if error.code == BackendErrorCode::Unsupported => Ok(()),
            Err(error) => Err(error),
        };
        if let Some(cleanup) = dictation_cleanup {
            let cancel_result = cleanup.await;
            let host_result = active_session
                .map(|session_id| self.hide_dictation_feedback(session_id))
                .unwrap_or(Ok(()));
            cancel_result?;
            host_result?;
        }
        selection_result?;
        self.dismiss_pending_corrections();
        Ok(())
    }

    pub fn snapshot(&self) -> BackendSnapshot {
        let state = self.state.read().expect("backend state lock poisoned");
        BackendSnapshot {
            running: state.running,
            dictation: state.dictation.clone(),
            vocabulary_revision: self.vocabulary_revision.load(Ordering::Acquire),
            history_revision: self.history_revision.load(Ordering::Acquire),
            style_pack_revision: self.style_pack_revision.load(Ordering::Acquire),
            preferences_revision: self.preferences_revision.load(Ordering::Acquire),
            credentials: state.credentials.clone(),
        }
    }

    /// Dispatch a launcher/single-instance intent through the same state
    /// machine and domain Interfaces used by normal host calls.
    pub async fn dispatch_cli_intent(
        &self,
        intent: crate::cli::CliIntent,
    ) -> Result<CliDispatchOutcome, BackendError> {
        match intent {
            crate::cli::CliIntent::ToggleDictation => match self.snapshot().dictation.phase {
                DictationPhase::Idle => self
                    .start_dictation()
                    .await
                    .map(CliDispatchOutcome::DictationStarted),
                DictationPhase::Starting | DictationPhase::Recording => self
                    .stop_dictation()
                    .await
                    .map(CliDispatchOutcome::DictationCompleted),
                DictationPhase::Transcribing
                | DictationPhase::Polishing
                | DictationPhase::Inserting
                | DictationPhase::Completed
                | DictationPhase::Cancelled
                | DictationPhase::Failed => Ok(CliDispatchOutcome::Noop),
            },
            crate::cli::CliIntent::CancelDictation => {
                if let Some(session_id) = self.snapshot().dictation.session_id {
                    self.cancel_dictation(Some(session_id)).await?;
                } else {
                    return Ok(CliDispatchOutcome::Noop);
                }
                Ok(CliDispatchOutcome::DictationCancelled)
            }
        }
    }

    /// Apply physical dictation-key edges using the shared hotkey-mode rules.
    ///
    /// Native listeners provide a stable physical-press id plus monotonic event
    /// timestamps; Core owns deduplication, arbitration, duration and cooldown.
    pub async fn dispatch_dictation_hotkey_edge(
        &self,
        edge: DictationHotkeyEdge,
    ) -> Result<CliDispatchOutcome, BackendError> {
        self.dispatch_dictation_hotkey_edge_with_session_options(
            edge,
            DictationHotkeyDispatchOptions::default(),
        )
        .await
    }

    /// Apply a physical dictation-key edge with host-captured start options.
    ///
    /// This is primarily used by native hotkey adapters that receive a
    /// translation modifier before the dictation press. The options are only
    /// consumed when this edge actually starts a new session.
    pub async fn dispatch_dictation_hotkey_edge_with_options(
        &self,
        edge: DictationHotkeyEdge,
        options: DictationStartOptions,
    ) -> Result<CliDispatchOutcome, BackendError> {
        self.dispatch_dictation_hotkey_edge_with_session_options(
            edge,
            DictationHotkeyDispatchOptions {
                start: options,
                stop: DictationStopOptions::default(),
            },
        )
        .await
    }

    /// Apply a physical hotkey edge with host-captured start and stop options.
    ///
    /// Start options are consumed only when the edge creates a session. Stop
    /// options are consumed only when it finalizes one. This preserves desktop
    /// translation-modifier semantics without making the host duplicate the
    /// Toggle/Hold/Auto state machine.
    pub async fn dispatch_dictation_hotkey_edge_with_session_options(
        &self,
        edge: DictationHotkeyEdge,
        options: DictationHotkeyDispatchOptions,
    ) -> Result<CliDispatchOutcome, BackendError> {
        use crate::hotkey_interpreter::HotkeyIntent;

        // Pressed and Released stay FIFO even though start/finalize await native
        // work. Combined deliberately bypasses this gate: it has a dedicated
        // low-latency host bridge and must be able to cancel a start in flight.
        // `press_id` plus `start_finished` closes the resulting race safely.
        let _dispatch_guard = if matches!(edge, DictationHotkeyEdge::Combined { .. }) {
            None
        } else {
            Some(self.hotkey_dispatch_gate.lock().await)
        };
        let preferences = self.get_preferences();
        let mode = preferences.hotkey.mode;
        let modifier_only =
            crate::shortcut_types::legacy_modifier_trigger(&preferences.dictation_hotkey).is_some();
        let (intent, reservation) = {
            let mut hotkey = self
                .hotkey
                .lock()
                .expect("hotkey interpreter lock poisoned");
            let phase = self.snapshot().dictation.phase;
            let intent = match edge {
                DictationHotkeyEdge::Pressed { press_id, at } => {
                    hotkey.press(press_id, at, mode, phase, modifier_only)
                }
                DictationHotkeyEdge::Released { press_id, at } => {
                    hotkey.release(press_id, at, mode, phase)
                }
                DictationHotkeyEdge::Combined { press_id, at: _ } => hotkey.combined(press_id),
            };
            // Bind an accepted physical press to its actual Starting session
            // before releasing the interpreter lock. An older CLI/button stop
            // must not clear this press between its Start decision and claim.
            let reservation = matches!(intent, HotkeyIntent::Start { .. })
                .then(|| self.reserve_dictation_session(options.start.insert_text));
            (intent, reservation)
        };
        let (intent, reservation) = if let HotkeyIntent::WaitForModifierGrace { press_id } = intent
        {
            // Only modifier-only triggers pay this delay. The separate Combined
            // bridge can mark the same generation while this task is sleeping.
            tokio::time::sleep(
                crate::hotkey_interpreter::HotkeyInterpreter::MODIFIER_ARBITRATION_GRACE,
            )
            .await;
            let mut hotkey = self
                .hotkey
                .lock()
                .expect("hotkey interpreter lock poisoned");
            let intent = hotkey.after_modifier_grace(press_id, self.snapshot().dictation.phase);
            let reservation = matches!(intent, HotkeyIntent::Start { .. })
                .then(|| self.reserve_dictation_session(options.start.insert_text));
            (intent, reservation)
        } else {
            (intent, reservation)
        };

        match intent {
            HotkeyIntent::Noop | HotkeyIntent::WaitForModifierGrace { .. } => {
                Ok(CliDispatchOutcome::Noop)
            }
            HotkeyIntent::Start { press_id } => {
                let result = match reservation.expect("Start intent must claim its session") {
                    Ok(reservation) => {
                        self.start_reserved_dictation(reservation, options.start)
                            .await
                    }
                    Err(error) => Err(error),
                };
                // Microphone and ASR startup can take long enough for Combined
                // to overtake this task. Re-check before reporting a live session.
                let combined = self
                    .hotkey
                    .lock()
                    .expect("hotkey interpreter lock poisoned")
                    .start_finished(press_id, result.is_ok());
                match result {
                    Ok(session_id) if combined => {
                        self.cancel_dictation(Some(session_id)).await?;
                        self.hotkey
                            .lock()
                            .expect("hotkey interpreter lock poisoned")
                            .combo_cancelled(press_id);
                        Ok(CliDispatchOutcome::DictationCancelled)
                    }
                    Ok(session_id) => Ok(CliDispatchOutcome::DictationStarted(session_id)),
                    Err(error) => {
                        if combined {
                            self.hotkey
                                .lock()
                                .expect("hotkey interpreter lock poisoned")
                                .combo_cancelled(press_id);
                        }
                        Err(error)
                    }
                }
            }
            HotkeyIntent::Stop => self
                .stop_dictation_with_options(options.stop)
                .await
                .map(CliDispatchOutcome::DictationCompleted),
            HotkeyIntent::Cancel { press_id } => {
                let active = self.snapshot().dictation.session_id;
                if active.is_none() {
                    return Ok(CliDispatchOutcome::Noop);
                }
                self.cancel_dictation(active).await?;
                self.hotkey
                    .lock()
                    .expect("hotkey interpreter lock poisoned")
                    .combo_cancelled(press_id);
                Ok(CliDispatchOutcome::DictationCancelled)
            }
        }
    }

    pub async fn get_credentials_status(&self) -> Result<CredentialsStatus, BackendError> {
        let status = self
            .deps
            .credential_store
            .status(self.get_preferences())
            .await?;
        self.state
            .write()
            .expect("backend state lock poisoned")
            .credentials = status.clone();
        Ok(status)
    }

    pub async fn read_credential(
        &self,
        key: CredentialKey,
    ) -> Result<Option<SecretValue>, BackendError> {
        self.deps.credential_store.read(key).await
    }

    pub async fn set_credential(
        &self,
        key: CredentialKey,
        value: SecretValue,
    ) -> Result<CredentialsStatus, BackendError> {
        self.deps.credential_store.write(key, value).await?;
        self.refresh_and_publish_credentials().await
    }

    pub async fn remove_credential(
        &self,
        key: CredentialKey,
    ) -> Result<CredentialsStatus, BackendError> {
        self.deps.credential_store.remove(key).await?;
        self.refresh_and_publish_credentials().await
    }

    pub async fn activate_local_asr(
        &self,
        request: crate::domains::LocalAsrActivationRequest,
    ) -> Result<crate::domains::LocalAsrActivationResult, BackendError> {
        let result = self.deps.services.local_asr.activate(request).await?;
        self.refresh_and_publish_credentials().await?;
        Ok(result)
    }


    async fn refresh_and_publish_credentials(&self) -> Result<CredentialsStatus, BackendError> {
        let status = self
            .deps
            .credential_store
            .status(self.get_preferences())
            .await?;
        self.state
            .write()
            .expect("backend state lock poisoned")
            .credentials = status.clone();
        self.events
            .publish(None, BackendEventKind::CredentialsChanged(status.clone()));
        Ok(status)
    }

    pub fn get_preferences(&self) -> UserPreferences {
        self.preferences.get()
    }

    #[cfg(test)]
    pub(crate) fn set_preferences(
        &self,
        mut preferences: UserPreferences,
    ) -> Result<(), BackendError> {
        sync_style_pack_preferences(&mut preferences, &self.style_packs.list()?);
        self.discard_active_screen_ocr_on_disable(&self.get_preferences(), &preferences);
        self.preferences.set(preferences)?;
        self.hotkey
            .lock()
            .expect("hotkey interpreter lock poisoned")
            .reset();
        self.publish_preferences_changed();
        Ok(())
    }

    /// 屏幕 OCR 开关在会话进行中被关掉时，立刻作废当前会话的待定观测——
    /// 已经识别完、还躺在槽位里的结果也不许再被润色消费（开关关闭 = 功能不存在）。
    fn discard_active_screen_ocr_on_disable(
        &self,
        previous: &UserPreferences,
        next: &UserPreferences,
    ) {
        if previous.screen_ocr_context_enabled && !next.screen_ocr_context_enabled {
            let state = self.state.read().expect("backend state lock poisoned");
            if let Some(context) = state.dictation_context.as_ref() {
                context.discard_pending_screen_ocr();
            }
        }
    }
    /// Persist a host-facing settings document after applying the shared
    /// shortcut compatibility and collision rules.
    #[cfg(test)]
    pub(crate) fn set_preferences_validated(
        &self,
        mut preferences: UserPreferences,
    ) -> Result<(), BackendError> {
        crate::sync_dictation_hotkey_legacy_fields(&mut preferences);
        crate::reject_hotkey_collisions(&preferences).map_err(|message| {
            BackendError::new(crate::BackendErrorCode::InvalidArgument, message)
        })?;
        self.set_preferences(preferences)
    }

    #[cfg(test)]
    pub(crate) fn set_preferences_preserving_style(
        &self,
        preferences: UserPreferences,
    ) -> Result<(), BackendError> {
        self.preferences
            .set_preserving_current_style_preferences(preferences)?;
        self.hotkey
            .lock()
            .expect("hotkey interpreter lock poisoned")
            .reset();
        self.publish_preferences_changed();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_preferences_preserving_style_validated(
        &self,
        mut preferences: UserPreferences,
    ) -> Result<(), BackendError> {
        crate::sync_dictation_hotkey_legacy_fields(&mut preferences);
        crate::reject_hotkey_collisions(&preferences).map_err(|message| {
            BackendError::new(crate::BackendErrorCode::InvalidArgument, message)
        })?;
        self.set_preferences_preserving_style(preferences)
    }

    pub fn update_settings<R: crate::SettingsRuntime + ?Sized>(
        &self,
        mut preferences: UserPreferences,
        options: crate::SettingsUpdateOptions,
        runtime: &R,
    ) -> Result<crate::SettingsUpdateOutcome, BackendError> {
        let _write_guard = self
            .settings_write_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(expected) = options.expected_preferences_revision {
            let actual = self.preferences_revision.load(Ordering::Acquire);
            if actual != expected {
                return Err(BackendError {
                    code: BackendErrorCode::Busy,
                    message: "settings changed since the submitted document was read".into(),
                    retryable: true,
                    details: Some(serde_json::json!({
                        "expectedPreferencesRevision": expected,
                        "actualPreferencesRevision": actual,
                    })),
                });
            }
        }
        let mut previous = self.preferences.get();
        crate::sync_dictation_hotkey_legacy_fields(&mut previous);
        crate::sync_dictation_hotkey_legacy_fields(&mut preferences);
        if options.preserve_current_style {
            preferences.preserve_style_preferences_from(&previous);
        }
        sync_style_pack_preferences(&mut preferences, &self.style_packs.list()?);

        let reconciled_hotkey_count = match crate::reject_hotkey_collisions(&preferences) {
            Ok(()) => 0,
            Err(message)
                if options.collision_policy == crate::SettingsCollisionPolicy::Reconcile =>
            {
                let adjusted = crate::reconcile_hotkey_collisions(&mut preferences, &previous);
                crate::reject_hotkey_collisions(&preferences).map_err(|leftover| {
                    BackendError::new(
                        BackendErrorCode::InvalidArgument,
                        format!(
                            "{message}; reconciled {adjusted} shortcuts but validation still failed: {leftover}"
                        ),
                    )
                })?;
                adjusted
            }
            Err(message) => {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidArgument,
                    message,
                ));
            }
        };

        let effects = crate::SettingsEffectPlan::between(&previous, &preferences);
        let mut receipt = match runtime.prepare(&effects) {
            Ok(receipt) => receipt,
            Err(failure) => {
                let compensation = runtime.restore(&effects, &failure.receipt).err();
                return Err(settings_transaction_error(
                    failure.error,
                    compensation.into_iter().collect(),
                ));
            }
        };

        if let Err(failure) = runtime.commit(&effects, &mut receipt) {
            for effect in failure.receipt.applied {
                if !receipt.applied.contains(&effect) {
                    receipt.applied.push(effect);
                }
            }
            let compensation_errors = runtime
                .restore(&effects, &receipt)
                .err()
                .into_iter()
                .collect();
            return Err(settings_transaction_error(
                failure.error,
                compensation_errors,
            ));
        }

        self.discard_active_screen_ocr_on_disable(&previous, &preferences);
        if let Err(error) = self.preferences.set(preferences.clone()) {
            let compensation = runtime.restore(&effects, &receipt).err();
            return Err(settings_transaction_error(
                error,
                compensation.into_iter().collect(),
            ));
        }

        if effects.hotkeys.is_some() {
            self.hotkey
                .lock()
                .expect("hotkey interpreter lock poisoned")
                .reset();
        }

        if previous.cursor_context_enabled && !preferences.cursor_context_enabled {
            self.disarm_edit_observation();
        }
        self.publish_preferences_changed();
        Ok(crate::SettingsUpdateOutcome {
            preferences,
            reconciled_hotkey_count,
            effects,
        })
    }

    fn publish_preferences_changed(&self) {
        let revision = self.preferences_revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.events.publish(
            None,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision }),
        );
    }

    pub fn list_style_packs(&self, active_id: &str) -> Result<Vec<StylePack>, BackendError> {
        self.style_packs.list_with_active(active_id)
    }

    /// Return settings-page prompt diagnostics assembled by Core. The DTO is
    /// owned and safe for any host to render; hosts must not duplicate prompt
    /// composition or hotword filtering.
    pub fn preview_style_pack_runtime(
        &self,
        style_pack: &StylePack,
    ) -> crate::style_packs::StylePackRuntimeDiagnostics {
        let preferences = self.get_preferences();
        let hotwords = self.enabled_vocabulary_phrases();
        crate::style_packs::build_style_pack_runtime_diagnostics(style_pack, &preferences, hotwords)
    }

    /// Persist the microphone selected by a host-owned menu or device picker.
    ///
    /// This focused use-case keeps callers away from whole-document writes and
    /// shares the settings write gate with validated settings transactions.
    pub fn select_microphone_device(&self, device_name: String) -> Result<(), BackendError> {
        let _write_guard = self
            .settings_write_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut preferences = self.preferences.get();
        preferences.microphone_device_name = device_name;
        self.preferences.set(preferences)?;
        self.publish_preferences_changed();
        Ok(())
    }

    /// Select the previous enabled style pack in the stable store order.
    ///
    /// Returns `None` when cycling is not meaningful (zero or one enabled pack).
    /// Window feedback and tray refresh remain host responsibilities.
    pub fn activate_previous_style_pack(&self) -> Result<Option<StylePack>, BackendError> {
        let _write_guard = self
            .settings_write_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut preferences = self.preferences.get();
        let packs = self
            .style_packs
            .list_with_active(&preferences.active_style_pack_id)?;
        let enabled = packs
            .into_iter()
            .filter(|pack| pack.enabled)
            .collect::<Vec<_>>();
        if enabled.len() <= 1 {
            return Ok(None);
        }
        let current_index = enabled
            .iter()
            .position(|pack| pack.id == preferences.active_style_pack_id)
            .unwrap_or(0);
        let next_index = if current_index == 0 {
            enabled.len() - 1
        } else {
            current_index - 1
        };
        let mut selected = enabled[next_index].clone();
        preferences.active_style_pack_id = selected.id.clone();
        sync_style_pack_preferences(&mut preferences, &enabled);
        self.preferences.set(preferences)?;
        self.publish_preferences_changed();
        selected.active = true;
        Ok(Some(selected))
    }

    pub fn get_style_pack(&self, id: &str) -> Result<StylePack, BackendError> {
        self.style_packs.get(id)
    }

    pub fn get_active_style_pack(&self, active_id: &str) -> Result<StylePack, BackendError> {
        self.style_packs.get_or_default_active(active_id)
    }

    pub fn activate_style_pack(&self, id: &str) -> Result<StylePack, BackendError> {
        let mut pack = self.style_packs.get(id)?;
        if !pack.enabled {
            pack = self.style_packs.set_enabled(id, true)?;
        }
        let mut preferences = self.preferences.get();
        preferences.active_style_pack_id = id.to_string();
        sync_style_pack_preferences(&mut preferences, &self.style_packs.list()?);
        self.preferences.set(preferences)?;
        self.publish_preferences_changed();
        self.publish_style_packs_changed();
        pack.active = true;
        Ok(pack)
    }

    pub fn create_style_pack(&self, pack: StylePack) -> Result<StylePack, BackendError> {
        let pack = self.style_packs.create(pack)?;
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn update_style_pack(&self, pack: StylePack) -> Result<StylePack, BackendError> {
        let pack = self.style_packs.update(pack)?;
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn set_style_pack_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<StylePack, BackendError> {
        let pack = self.style_packs.set_enabled(id, enabled)?;
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn set_style_pack_origin(
        &self,
        id: &str,
        origin_pack_id: Option<String>,
        origin_author_login: Option<String>,
    ) -> Result<StylePack, BackendError> {
        let pack = self
            .style_packs
            .set_origin(id, origin_pack_id, origin_author_login)?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn reset_builtin_style_pack(&self, id: &str) -> Result<StylePack, BackendError> {
        let pack = self.style_packs.reset_builtin(id)?;
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn remove_style_pack(
        &self,
        id: &str,
    ) -> Result<crate::StylePackRemovalOutcome, BackendError> {
        let _write_guard = self
            .settings_write_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = self.preferences.get();
        self.style_packs.remove_imported(id)?;
        let mut preferences = previous.clone();
        preferences
            .style_pack_hotkeys
            .retain(|entry| entry.pack_id != id);
        if preferences.active_style_pack_id == id {
            preferences.active_style_pack_id = crate::style_packs::default_active_style_pack_id();
        }
        sync_style_pack_preferences(&mut preferences, &self.style_packs.list()?);
        let effects = crate::SettingsEffectPlan::between(&previous, &preferences);
        self.preferences.set(preferences)?;
        self.publish_preferences_changed();
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(crate::StylePackRemovalOutcome { effects })
    }

    pub fn import_style_pack_bytes(&self, bytes: &[u8]) -> Result<StylePack, BackendError> {
        let pack = self.style_packs.import_from_zip_bytes(bytes)?;
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn import_style_pack_path(
        &self,
        path: &std::path::Path,
    ) -> Result<StylePack, BackendError> {
        let pack = self.style_packs.import_from_zip(path)?;
        self.sync_preferences_after_style_pack_change()?;
        self.publish_style_packs_changed();
        Ok(pack)
    }

    pub fn export_style_pack_bytes(&self, id: &str) -> Result<Vec<u8>, BackendError> {
        self.style_packs.export_zip_bytes(id)
    }

    pub fn export_style_pack_path(
        &self,
        id: &str,
        path: &std::path::Path,
    ) -> Result<(), BackendError> {
        self.style_packs.export_to_zip(id, path)
    }

    fn sync_preferences_after_style_pack_change(&self) -> Result<(), BackendError> {
        let mut preferences = self.preferences.get();
        if sync_style_pack_preferences(&mut preferences, &self.style_packs.list()?) {
            self.preferences.set(preferences)?;
            self.publish_preferences_changed();
        }
        Ok(())
    }

    fn publish_style_packs_changed(&self) {
        let revision = self.style_pack_revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.events.publish(
            None,
            BackendEventKind::StylePacksChanged(StylePackChange { revision }),
        );
    }

    pub fn list_history(&self) -> Result<Vec<DictationSession>, BackendError> {
        self.history.list()
    }

    pub fn recent_history_within_minutes(
        &self,
        minutes: u32,
    ) -> Result<Vec<DictationSession>, BackendError> {
        self.history.recent_within_minutes(minutes)
    }

    pub fn list_activity(&self) -> Result<Vec<ActivityDay>, BackendError> {
        self.activity.snapshot()
    }

    pub fn record_activity(
        &self,
        date: &str,
        chars: u64,
        duration_ms: u64,
    ) -> Result<(), BackendError> {
        self.activity.bump(date, chars, duration_ms)?;
        self.publish_history_changed();
        Ok(())
    }

    pub fn append_history(
        &self,
        session: DictationSession,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<(), BackendError> {
        self.history
            .append_with_retention(session, retention_days, max_entries)?;
        self.publish_history_changed();
        Ok(())
    }

    pub fn delete_history(&self, id: &str) -> Result<(), BackendError> {
        self.history.delete(id)?;
        self.publish_history_changed();
        Ok(())
    }

    pub fn update_history_entry(&self, session: DictationSession) -> Result<bool, BackendError> {
        let updated = self.history.update_entry(session)?;
        if updated {
            self.publish_history_changed();
        }
        Ok(updated)
    }

    pub fn apply_history_retranscription(
        &self,
        session_id: &str,
        text: String,
        asr_call_label: &crate::auxiliary::AsrCallLabel,
        asr_ms: u64,
    ) -> Result<DictationSession, BackendError> {
        // The host supplies only bytes and the measured provider result. All
        // record attribution/mutation stays here so Tauri and future hosts
        // cannot retain different meanings for a successful retranscription.
        if text.trim().is_empty() {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "retranscription text is empty",
            ));
        }
        let mut entry = self
            .list_history()?
            .into_iter()
            .find(|entry| entry.id == session_id)
            .ok_or_else(|| {
                BackendError::new(BackendErrorCode::InvalidArgument, "history entry not found")
            })?;
        entry.raw_transcript = text.clone();
        entry.final_text = text;
        entry.error_code = None;
        // 重新转写覆盖了 raw/final：旧诊断快照里的 prompt 对应的是被覆盖前的
        // 输入，保留会误导「这就是本次记录实际送入模型的内容」，一并清除。
        entry.polish_diagnostics = None;
        entry.asr_provider = Some(asr_call_label.provider.clone());
        entry.asr_model = asr_call_label.model.clone();
        entry.asr_ms = Some(asr_ms);
        entry.llm_provider = None;
        entry.llm_model = None;
        entry.polish_ms = None;
        if !self.update_history_entry(entry.clone())? {
            return Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "history entry not found",
            ));
        }
        Ok(entry)
    }

    pub fn clear_history(&self) -> Result<(), BackendError> {
        self.history.clear()?;
        self.publish_history_changed();
        Ok(())
    }

    fn publish_history_changed(&self) {
        let revision = self.history_revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.events.publish(
            None,
            BackendEventKind::HistoryChanged(HistoryChange { revision }),
        );
    }

    pub fn list_vocabulary(&self) -> Result<Vec<DictionaryEntry>, BackendError> {
        self.vocabulary.list()
    }

    /// Return enabled vocabulary phrases in persisted order. Hosts reuse this
    /// owned projection instead of duplicating the filtering rule.
    pub fn enabled_vocabulary_phrases(&self) -> Vec<String> {
        self.list_vocabulary()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.phrase)
            .collect()
    }

    /// Return enabled vocabulary phrases in the Core ASR priority order.
    pub fn asr_vocabulary_phrases(&self) -> Vec<String> {
        let entries = self
            .list_vocabulary()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.enabled)
            .collect();
        crate::vocabulary::prioritize_vocabulary_for_asr(entries)
    }

    /// Return the instance-local correction suggestions awaiting a user
    /// decision. The returned value is owned and safe to render on any host.
    pub fn pending_corrections(&self) -> Vec<PendingCorrection> {
        self.pending_corrections
            .lock()
            .expect("pending correction lock poisoned")
            .clone()
    }

    /// Queue one observed manual correction. Duplicate pairs are ignored and
    /// the oldest item is dropped when the bounded card capacity is reached.
    pub fn queue_pending_correction(
        &self,
        pattern: String,
        replacement: String,
    ) -> Result<Option<PendingCorrection>, BackendError> {
        queue_pending_correction_state(
            &self.pending_corrections,
            &self.events,
            pattern,
            replacement,
        )
    }

    fn disarm_edit_observation(&self) {
        self.edit_observation_generation
            .fetch_add(1, Ordering::AcqRel);
        self.deps.services.edit_observation.disarm();
    }

    fn arm_edit_observation(
        &self,
        session_id: SessionId,
        enabled: bool,
        insert_outcome: Option<InsertOutcome>,
        typed_text: &str,
    ) {
        // Privacy changes are not a frozen dictation option. Serialize this
        // native registration with settings updates so a mid-session opt-out
        // cannot be undone by an older dictation completing afterwards.
        let _settings_guard = self
            .settings_write_gate
            .lock()
            .expect("settings write gate poisoned");
        // Native settings can hold this gate while A completes, is cancelled,
        // and B starts. Recheck ownership after the wait, before disarming or
        // reading any document for an obsolete completion.
        {
            let state = self.state.read().expect("backend state lock poisoned");
            if state.dictation.session_id != Some(session_id)
                || state.dictation.phase != DictationPhase::Completed
            {
                return;
            }
        }
        self.disarm_edit_observation();
        if !enabled
            || !self.get_preferences().cursor_context_enabled
            || !matches!(insert_outcome, Some(InsertOutcome::Inserted))
            || typed_text.trim().is_empty()
        {
            return;
        }
        let expected_generation = self.edit_observation_generation.load(Ordering::Acquire);
        let sink = Arc::new(CoreEditObservationSink {
            expected_generation,
            generation: Arc::clone(&self.edit_observation_generation),
            typed_text: typed_text.to_string(),
            pending: Arc::clone(&self.pending_corrections),
            events: Arc::clone(&self.events),
        });
        if let Err(error) = self
            .deps
            .services
            .edit_observation
            .arm(typed_text.to_string(), sink)
        {
            log::warn!("failed to arm edit observation: {error}");
        }
        if self.edit_observation_generation.load(Ordering::Acquire) != expected_generation {
            // A new start may disarm during the synchronous Host call. Another
            // completion cannot arm while this settings guard is held, so only
            // the obsolete registration is removed; preserve B's generation.
            self.deps.services.edit_observation.disarm();
        }
    }

    /// Accept one suggestion and atomically remove it only after the shared
    /// vocabulary mutation succeeds. Repeated or stale ids are idempotent.
    pub fn accept_pending_correction(
        &self,
        id: &str,
    ) -> Result<Option<PendingCorrection>, BackendError> {
        let (suggestion, added, snapshot) = {
            let mut pending = self
                .pending_corrections
                .lock()
                .expect("pending correction lock poisoned");
            let Some(index) = pending.iter().position(|item| item.id == id) else {
                return Ok(None);
            };
            let suggestion = pending[index].clone();
            let added = self.vocabulary.add_if_absent(
                suggestion.replacement.clone(),
                Some(LEARNED_VOCAB_NOTE.to_string()),
            )?;
            pending.remove(index);
            (suggestion, added.is_some(), pending.clone())
        };
        if added {
            self.publish_vocabulary_changed();
        }
        self.events.publish(
            None,
            BackendEventKind::VocabularySuggestionsChanged(snapshot),
        );
        Ok(Some(suggestion))
    }

    /// Reject one suggestion without creating a hidden deny-list.
    pub fn reject_pending_correction(&self, id: &str) -> bool {
        let snapshot = {
            let mut pending = self
                .pending_corrections
                .lock()
                .expect("pending correction lock poisoned");
            let Some(index) = pending.iter().position(|item| item.id == id) else {
                return false;
            };
            pending.remove(index);
            pending.clone()
        };
        self.events.publish(
            None,
            BackendEventKind::VocabularySuggestionsChanged(snapshot),
        );
        true
    }

    /// Dismiss the complete card. Empty dismissals are idempotent and do not
    /// publish redundant events.
    pub fn dismiss_pending_corrections(&self) {
        let changed = {
            let mut pending = self
                .pending_corrections
                .lock()
                .expect("pending correction lock poisoned");
            if pending.is_empty() {
                false
            } else {
                pending.clear();
                true
            }
        };
        if changed {
            self.events.publish(
                None,
                BackendEventKind::VocabularySuggestionsChanged(Vec::new()),
            );
        }
    }

    pub fn add_vocabulary(
        &self,
        phrase: String,
        note: Option<String>,
    ) -> Result<DictionaryEntry, BackendError> {
        let entry = self.vocabulary.add(phrase, note)?;
        self.publish_vocabulary_changed();
        Ok(entry)
    }

    pub fn add_vocabulary_if_absent(
        &self,
        phrase: String,
        note: Option<String>,
    ) -> Result<Option<DictionaryEntry>, BackendError> {
        let entry = self.vocabulary.add_if_absent(phrase, note)?;
        if entry.is_some() {
            self.publish_vocabulary_changed();
        }
        Ok(entry)
    }

    pub fn record_vocabulary_hits(&self, text: &str) -> Result<u64, BackendError> {
        let hits = self.vocabulary.record_hits(text)?;
        if hits > 0 {
            self.publish_vocabulary_changed();
        }
        Ok(hits)
    }

    pub fn remove_vocabulary(&self, id: &str) -> Result<(), BackendError> {
        self.vocabulary.remove(id)?;
        self.publish_vocabulary_changed();
        Ok(())
    }

    pub fn set_vocabulary_enabled(&self, id: &str, enabled: bool) -> Result<(), BackendError> {
        self.vocabulary.set_enabled(id, enabled)?;
        self.publish_vocabulary_changed();
        Ok(())
    }

    pub fn list_correction_rules(&self) -> Result<Vec<CorrectionRule>, BackendError> {
        self.correction_rules.list()
    }

    pub fn add_correction_rule(
        &self,
        pattern: String,
        replacement: String,
    ) -> Result<CorrectionRule, BackendError> {
        let rule = self.correction_rules.add(pattern, replacement)?;
        self.publish_vocabulary_changed();
        Ok(rule)
    }

    pub fn remove_correction_rule(&self, id: &str) -> Result<(), BackendError> {
        self.correction_rules.remove(id)?;
        self.publish_vocabulary_changed();
        Ok(())
    }

    pub fn set_correction_rule_enabled(&self, id: &str, enabled: bool) -> Result<(), BackendError> {
        self.correction_rules.set_enabled(id, enabled)?;
        self.publish_vocabulary_changed();
        Ok(())
    }

    pub fn list_vocabulary_presets(&self) -> Result<VocabPresetStore, BackendError> {
        crate::vocabulary::list_vocab_presets(&self.config.data_dir)
    }

    pub fn save_vocabulary_presets(&self, store: &VocabPresetStore) -> Result<(), BackendError> {
        crate::vocabulary::save_vocab_presets(&self.config.data_dir, store)?;
        self.publish_vocabulary_changed();
        Ok(())
    }

    fn publish_vocabulary_changed(&self) {
        let revision = self.vocabulary_revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.events.publish(
            None,
            BackendEventKind::VocabularyChanged(VocabularyChange { revision }),
        );
    }

    pub async fn start_dictation(&self) -> Result<SessionId, BackendError> {
        self.start_dictation_with_options(DictationStartOptions::default())
            .await
    }




    fn reserve_dictation_session(
        &self,
        insert_text: bool,
    ) -> Result<DictationReservation, BackendError> {
        {
            let state = self.state.read().expect("backend state lock poisoned");
            ensure_running(&state)?;
            if state.dictation.session_id.is_some() {
                return Err(BackendError::new(
                    BackendErrorCode::Busy,
                    "a dictation session is already active",
                ));
            }
        }
        let session_id = SessionId::new();
        self.voice_sessions.acquire(
            session_id,
            crate::voice_session::VoiceSessionKind::Dictation,
        )?;
        let starting_resources = self.voice_sessions.hold_resources(session_id)?;
        // Freeze the destination before context/credential awaits or feedback
        // can change foreground focus. No native preparation happens yet.
        let inserter = insert_text
            .then(|| self.deps.text_inserter.capture_target())
            .flatten()
            .unwrap_or_else(|| Arc::clone(&self.deps.text_inserter));
        self.disarm_edit_observation();
        // Context capture can await AX, a keyring, or another host service.
        // Publish ownership before that first await so Esc/stop see Starting
        // instead of an invisible lease which would begin recording later.
        {
            let mut state = self.state.write().expect("backend state lock poisoned");
            if let Err(error) = ensure_running(&state) {
                self.voice_sessions.release(session_id);
                return Err(error);
            }
            state.dictation = DictationStateSnapshot {
                phase: DictationPhase::Starting,
                session_id: Some(session_id),
                ..DictationStateSnapshot::default()
            };
            self.events.publish(
                Some(session_id),
                BackendEventKind::DictationStateChanged(state.dictation.clone()),
            );
            self.phase_changed.notify_waiters();
        }
        Ok(DictationReservation {
            session_id,
            resources: starting_resources,
            inserter,
        })
    }

    pub async fn start_dictation_with_options(
        &self,
        options: DictationStartOptions,
    ) -> Result<SessionId, BackendError> {
        let reservation = self.reserve_dictation_session(options.insert_text)?;
        self.start_reserved_dictation(reservation, options).await
    }

    async fn start_reserved_dictation(
        &self,
        reservation: DictationReservation,
        options: DictationStartOptions,
    ) -> Result<SessionId, BackendError> {
        let DictationReservation {
            session_id,
            resources: starting_resources,
            inserter,
        } = reservation;
        let context = match self
            .capture_dictation_context(&options, DictationContextPurpose::Dictation)
            .await
        {
            Ok(context) => Arc::new(context),
            Err(error) => {
                self.mark_dictation_failed(session_id, &error);
                self.reset_dictation_session(session_id);
                return Err(error);
            }
        };
        let context = {
            let mut state = self.state.write().expect("backend state lock poisoned");
            if let Err(error) = ensure_running(&state) {
                self.voice_sessions.release(session_id);
                return Err(error);
            }
            if state.dictation.session_id != Some(session_id)
                || state.dictation.phase != DictationPhase::Starting
            {
                self.voice_sessions.release(session_id);
                return Err(BackendError::new(
                    BackendErrorCode::Cancelled,
                    "dictation was cancelled while capturing its context",
                ));
            }
            state.silence_monitor = context.recording.silence_after_ms.map(|silence_ms| {
                let started_at = std::time::Instant::now();
                SilenceMonitor {
                    session_id,
                    started_at,
                    detector: crate::silence_auto_stop::SilenceAutoStop::new(
                        std::time::Duration::from_millis(silence_ms),
                        started_at,
                    ),
                }
            });
            state.dictation.translation_active = context.polish.translation_active;
            // 新会话取代旧上下文时，作废旧会话可能残留的屏幕 OCR 观测（无跨会话缓存）。
            if let Some(previous) = state.dictation_context.take() {
                previous.discard_pending_screen_ocr();
            }
            state.dictation_context = Some(Arc::clone(&context));
            context
        };

        if let Err(error) = self
            .deps
            .host_actions
            .request(HostAction::ShowDictationFeedback)
        {
            self.mark_dictation_failed(session_id, &error);
            self.reset_dictation_session(session_id);
            return Err(error);
        }
        if context.insertion.enabled {
            let insertion_context = Arc::clone(&context);
            let task_spawner = Arc::clone(&self.deps.task_spawner);
            let resources = Arc::clone(&starting_resources);
            let preparing: futures_util::future::BoxFuture<
                'static,
                Result<Arc<ActiveTextInsertion>, BackendError>,
            > = Box::pin(async move {
                let platform = inserter
                    .begin(session_id, Arc::clone(&insertion_context))
                    .await?;
                Ok(ActiveTextInsertion::new(
                    platform,
                    &insertion_context,
                    task_spawner,
                    resources,
                ))
            });
            let preparation = futures_util::FutureExt::shared(preparing);
            {
                // Publish preparation while ownership is still checked under
                // the same state lock cancellation uses. No native effect is
                // polled before its cancellable registry entry exists.
                let state = self.state.read().expect("backend state lock poisoned");
                ensure_active_session(&state, session_id)?;
                self.text_insertions
                    .lock()
                    .expect("text insertion registry lock poisoned")
                    .insert(session_id, preparation.clone());
            }
            let prepared = preparation.await;
            let still_active = self
                .state
                .read()
                .expect("backend state lock poisoned")
                .dictation
                .session_id
                == Some(session_id);
            if !still_active {
                if let Ok(insertion) = prepared {
                    let _ = insertion.cancel().await;
                }
                self.text_insertions
                    .lock()
                    .expect("text insertion registry lock poisoned")
                    .remove(&session_id);
                return Err(BackendError::new(
                    BackendErrorCode::Cancelled,
                    "dictation session was cancelled while insertion was starting",
                ));
            }
            match prepared {
                Ok(_) => {}
                Err(error) => {
                    self.text_insertions
                        .lock()
                        .expect("text insertion registry lock poisoned")
                        .remove(&session_id);
                    self.mark_dictation_failed(session_id, &error);
                    self.persist_failed_dictation(
                        &context,
                        session_id,
                        "insertFailed",
                        String::new(),
                        String::new(),
                        None,
                        None,
                        None,
                        None,
                        None,
                        false,
                        None,
                        None,
                        Some(crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                            &context,
                            "insertion failed before recording started",
                        )),
                    );
                    let _ = self.hide_dictation_feedback(session_id);
                    self.reset_dictation_session(session_id);
                    return Err(error);
                }
            }
        }
        let engine = Arc::clone(&self.deps.dictation_engine);
        let engine_context = Arc::clone(&context);
        let progress = self.engine_progress_sink();
        let resources = Arc::clone(&starting_resources);
        // Recorder startup can own a blocking native operation even when the
        // start caller disappears. Keep its hold in the executor, and perform
        // late cancellation here rather than relying on that caller to resume.
        let starting = own_voice_effect(
            &self.deps.task_spawner,
            Box::pin(async move {
                let _resources = resources;
                if _resources.cancel.is_cancelled() {
                    return Err(VoiceCaptureLifecycle::cancelled_error());
                }
                let result = engine.start(session_id, engine_context, progress).await;
                if _resources.cancel.is_cancelled() {
                    let _ = engine.cancel(session_id).await;
                    return Err(VoiceCaptureLifecycle::cancelled_error());
                }
                result
            }),
        );
        if let Err(error) = starting.await {
            if error.code != BackendErrorCode::Cancelled {
                self.mark_dictation_failed(session_id, &error);
                self.persist_failed_dictation(
                    &context,
                    session_id,
                    "transcribeFailed",
                    String::new(),
                    String::new(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                    Some(crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                        &context,
                        "recording failed before transcription started",
                    )),
                );
            }
            let _ = self.cancel_session_adapters(session_id).await;
            let _ = self.hide_dictation_feedback(session_id);
            self.reset_dictation_session(session_id);
            return Err(error);
        }
        let started = {
            let mut state = self.state.write().expect("backend state lock poisoned");
            if state.dictation.session_id == Some(session_id)
                && state.dictation.phase == DictationPhase::Starting
            {
                state.dictation.phase = DictationPhase::Recording;
                self.events.publish(
                    Some(session_id),
                    BackendEventKind::DictationStateChanged(state.dictation.clone()),
                );
                self.phase_changed.notify_waiters();
                true
            } else {
                false
            }
        };
        if !started {
            let _ = self.cancel_session_adapters(session_id).await;
            return Err(BackendError::new(
                BackendErrorCode::Cancelled,
                "dictation session was cancelled while the engine was starting",
            ));
        }
        Ok(session_id)
    }

    pub async fn stop_dictation(&self) -> Result<DictationResult, BackendError> {
        self.stop_dictation_session_with_options(None, DictationStopOptions::default())
            .await
    }

    pub async fn stop_dictation_with_options(
        &self,
        options: DictationStopOptions,
    ) -> Result<DictationResult, BackendError> {
        self.stop_dictation_session_with_options(None, options)
            .await
    }

    pub async fn stop_dictation_session(
        &self,
        session_id: SessionId,
    ) -> Result<DictationResult, BackendError> {
        self.stop_dictation_session_with_options(Some(session_id), DictationStopOptions::default())
            .await
    }

    async fn stop_dictation_session_with_options(
        &self,
        mut expected_session_id: Option<SessionId>,
        options: DictationStopOptions,
    ) -> Result<DictationResult, BackendError> {
        let (session_id, context, context_changed) = loop {
            // Register before inspecting the phase so a Starting -> Recording
            // transition cannot be lost between the state read and await.
            let changed = self.phase_changed.notified();
            let ready = {
                let mut state = self.state.write().expect("backend state lock poisoned");
                ensure_running(&state)?;
                let session_id = state.dictation.session_id.ok_or_else(|| {
                    BackendError::new(
                        BackendErrorCode::InvalidState,
                        "no active dictation session",
                    )
                })?;
                // A caller without an explicit ID means "stop this session",
                // not "stop whichever session exists after Starting wakes".
                // Bind under the state lock before the first await: cancelling
                // A and starting B must never retarget A's queued stop to B.
                if *expected_session_id.get_or_insert(session_id) != session_id {
                    return Err(BackendError::new(
                        BackendErrorCode::InvalidState,
                        "dictation stop targets a different session",
                    ));
                }
                match state.dictation.phase {
                    DictationPhase::Starting => None,
                    DictationPhase::Recording => {
                        let captured = state.dictation_context.clone().ok_or_else(|| {
                            BackendError::new(
                                BackendErrorCode::Internal,
                                "active dictation session has no captured context",
                            )
                        })?;
                        let context = match options.translation_requested {
                            Some(requested) => {
                                Arc::new(captured.with_translation_requested(requested))
                            }
                            None => captured,
                        };
                        let context_changed =
                            state.dictation_context.as_ref().is_some_and(|previous| {
                                    previous.polish.translation_active
                                        != context.polish.translation_active
                                });
                        state.dictation_context = Some(Arc::clone(&context));
                        state.dictation.translation_active = context.polish.translation_active;
                        state.dictation.phase = DictationPhase::Transcribing;
                        self.events.publish(
                            Some(session_id),
                            BackendEventKind::DictationStateChanged(state.dictation.clone()),
                        );
                        self.phase_changed.notify_waiters();
                        Some((session_id, context, context_changed))
                    }
                    _ => {
                        return Err(BackendError::new(
                            BackendErrorCode::Busy,
                            "dictation session is already being finalized",
                        ));
                    }
                }
            };
            if let Some(session) = ready {
                break session;
            }
            changed.await;
        };

        if context_changed {
            if let Err(error) = self
                .deps
                .dictation_engine
                .update_context(session_id, Arc::clone(&context))
                .await
            {
                self.mark_dictation_failed(session_id, &error);
                self.persist_failed_dictation(
                    &context,
                    session_id,
                    "polishFailed",
                    String::new(),
                    String::new(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                    Some(crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                        &context,
                        "stop-time context update failed before polish",
                    )),
                );
                let _ = self.cancel_session_adapters(session_id).await;
                let _ = self.hide_dictation_feedback(session_id);
                self.reset_dictation_session(session_id);
                return Err(error);
            }
        }

        let progress = self.engine_progress_sink();
        let resources = self.voice_sessions.hold_resources(session_id)?;
        let finishing = self.deps.dictation_engine.finish(session_id, progress);
        // finish() has taken the recorder out of the engine's registry. Cancel
        // can no longer join that stop through engine.cancel(), so its original
        // owner must retain the shared lease even without a text insertion.
        let mut engine_result = match own_voice_effect(
            &self.deps.task_spawner,
            Box::pin(async move {
                if resources.cancel.is_cancelled() {
                    return Ok(Err(crate::ports::EngineFailure::from(
                        VoiceCaptureLifecycle::cancelled_error(),
                    )));
                }
                let result = finishing.await;
                Ok(if resources.cancel.is_cancelled() {
                    Err(crate::ports::EngineFailure::from(
                        VoiceCaptureLifecycle::cancelled_error(),
                    ))
                } else {
                    result
                })
            }),
        )
        .await
        .map_err(crate::ports::EngineFailure::from)
        .and_then(std::convert::identity)
        {
            Ok(result) => result,
            Err(failure) => {
                let asr_call_label = failure.asr_call_label.clone();
                let llm_call_label = failure.llm_call_label.clone();
                let error = failure.error;
                if error.code != BackendErrorCode::Cancelled {
                    self.mark_dictation_failed(session_id, &error);
                    let raw_text = failure.raw_text.unwrap_or_default();
                    let (error_code, final_text, llm_used) = match failure.stage {
                        EngineFailureStage::Transcribing => {
                            ("transcribeFailed", String::new(), false)
                        }
                        EngineFailureStage::Polishing => ("polishFailed", raw_text.clone(), true),
                    };
                    self.persist_failed_dictation(
                        &context,
                        session_id,
                        error_code,
                        raw_text,
                        final_text,
                        None,
                        failure.duration_ms,
                        failure.asr_ms,
                        failure.polish_ms,
                        failure.has_audio_recording,
                        llm_used,
                        asr_call_label,
                        llm_call_label,
                        failure.polish_diagnostics.clone().or_else(|| {
                            Some(
                                crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                                    &context,
                                    "the dictation failed before the polish model was called",
                                ),
                            )
                        }),
                    );
                }
                // A failed finish can leave provider/recorder state in the engine.
                // Close both adapters before releasing this session's voice lease.
                let _ = self.cancel_session_adapters(session_id).await;
                let _ = self.hide_dictation_feedback(session_id);
                self.reset_dictation_session(session_id);
                return Err(error);
            }
        };

        if engine_result.raw_text.trim().is_empty() {
            let error = BackendError::new(
                BackendErrorCode::Provider,
                "transcription provider returned an empty transcript",
            );
            self.mark_dictation_failed(session_id, &error);
            self.persist_failed_dictation(
                &context,
                session_id,
                "emptyTranscript",
                engine_result.raw_text.clone(),
                String::new(),
                None,
                Some(engine_result.duration_ms),
                engine_result.asr_ms,
                None,
                engine_result.has_audio_recording,
                false,
                engine_result.asr_call_label.clone(),
                engine_result.llm_call_label.clone(),
                Some(crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                    &context,
                    "transcription returned an empty transcript",
                )),
            );
            let _ = self.cancel_text_insertion(session_id).await;
            let _ = self.hide_dictation_feedback(session_id);
            self.reset_dictation_session(session_id);
            return Err(error);
        }

        engine_result.polished_text = crate::streaming_insert::apply_chinese_script_preference(
            &engine_result.polished_text,
            context.polish.chinese_script_preference,
        );
        let correction_rules = match self.correction_rules.list() {
            Ok(rules) => rules,
            Err(error) => {
                log::warn!(
                    "failed to load correction rules for completed dictation: {error}; continuing without correction"
                );
                Vec::new()
            }
        };
        let streamed_text_is_visible = self
            .text_insertions
            .lock()
            .expect("text insertion registry lock poisoned")
            .get(&session_id)
            .and_then(|preparation| preparation.peek())
            .and_then(|result| result.as_ref().ok())
            .is_some_and(|insertion| insertion.has_written_text());
        if !correction_rules.is_empty() && !streamed_text_is_visible {
            engine_result.polished_text =
                apply_correction_rules(&engine_result.polished_text, &correction_rules);
        }

        // Cancellation may happen while ASR/LLM work is in flight. Never
        // insert a result after the session has been cancelled or replaced.
        {
            let state = self.state.read().expect("backend state lock poisoned");
            if state.dictation.session_id != Some(session_id)
                || !matches!(
                    state.dictation.phase,
                    DictationPhase::Transcribing | DictationPhase::Polishing
                )
            {
                return Err(BackendError::new(
                    BackendErrorCode::Cancelled,
                    "dictation session was cancelled before insertion",
                ));
            }
        }

        {
            let mut state = self.state.write().expect("backend state lock poisoned");
            ensure_active_session(&state, session_id)?;
            state.dictation.phase = if context.insertion.enabled {
                DictationPhase::Inserting
            } else {
                DictationPhase::Completed
            };
            self.events.publish(
                Some(session_id),
                BackendEventKind::DictationStateChanged(state.dictation.clone()),
            );
            self.phase_changed.notify_waiters();
        }

        let insert_outcome = if context.insertion.enabled {
            match self
                .finish_text_insertion(session_id, engine_result.polished_text.clone())
                .await
            {
                Ok(outcome) => Some(outcome),
                Err(error) => {
                    self.mark_dictation_failed(session_id, &error);
                    self.persist_failed_dictation(
                        &context,
                        session_id,
                        "insertFailed",
                        engine_result.raw_text.clone(),
                        engine_result.polished_text.clone(),
                        engine_result.polish_source.clone(),
                        Some(engine_result.duration_ms),
                        engine_result.asr_ms,
                        engine_result.polish_ms,
                        engine_result.has_audio_recording,
                        context.uses_llm_polisher(),
                        engine_result.asr_call_label.clone(),
                        engine_result.llm_call_label.clone(),
                        // 模型调用本身可能已成功：保留引擎快照（含成功 prompt），
                        // 失败的是后续插入。
                        engine_result.polish_diagnostics.clone(),
                    );
                    let _ = self.cancel_text_insertion(session_id).await;
                    let _ = self.hide_dictation_feedback(session_id);
                    self.reset_dictation_session(session_id);
                    return Err(error);
                }
            }
        } else {
            None
        };
        let result = DictationResult {
            session_id,
            raw_text: engine_result.raw_text.clone(),
            polished_text: engine_result.polished_text.clone(),
            polish_source: engine_result.polish_source.clone(),
            duration_ms: engine_result.duration_ms,
            inserted: insert_outcome
                .map(InsertOutcome::into_status)
                .unwrap_or(crate::types::InsertStatus::NotRequested),
        };

        let mut state = self.state.write().expect("backend state lock poisoned");
        if state.dictation.session_id != Some(session_id)
            || state.dictation.phase
                != if context.insertion.enabled {
                    DictationPhase::Inserting
                } else {
                    DictationPhase::Completed
                }
        {
            return Err(BackendError::new(
                BackendErrorCode::Cancelled,
                "dictation session was replaced before completion",
            ));
        }
        state.dictation.phase = DictationPhase::Completed;
        state.dictation.message = Some(match insert_outcome {
            Some(InsertOutcome::Inserted) => "inserted".to_string(),
            Some(InsertOutcome::PasteSent) => "paste_sent".to_string(),
            Some(InsertOutcome::CopiedFallback) => "copied_fallback".to_string(),
            None => "insertion_not_requested".to_string(),
        });
        if matches!(insert_outcome, Some(InsertOutcome::CopiedFallback)) {
            self.events.publish(
                Some(session_id),
                BackendEventKind::InsertFallback(crate::types::InsertFallbackPayload {
                    reason: "clipboard_fallback".to_string(),
                    copied_text: Some(engine_result.polished_text.clone()),
                }),
            );
        }
        self.events.publish(
            Some(session_id),
            BackendEventKind::DictationCompleted(result.clone()),
        );
        self.events.publish(
            Some(session_id),
            BackendEventKind::DictationStateChanged(state.dictation.clone()),
        );
        self.phase_changed.notify_waiters();
        drop(state);
        self.arm_edit_observation(
            session_id,
            context.insertion.observe_edits,
            insert_outcome,
            &result.polished_text,
        );
        let host_result = self.hide_dictation_feedback(session_id);
        // Reuse the same identity-checked reset as failure/cancellation. A
        // delayed successful completion must not clear its successor's state,
        // transcript, silence detector or physical-hotkey generation.
        self.reset_dictation_session(session_id);
        self.persist_completed_dictation(&context, &result, insert_outcome, &engine_result);
        host_result?;
        Ok(result)
    }

    fn persist_completed_dictation(
        &self,
        context: &DictationContext,
        result: &DictationResult,
        insert_outcome: Option<InsertOutcome>,
        engine_result: &crate::ports::EngineResult,
    ) {
        let preferences = self.get_preferences();
        let dictionary_entry_count = match self.record_vocabulary_hits(&result.polished_text) {
            Ok(hits) => Some(hits.min(u32::MAX as u64) as u32),
            Err(error) => {
                log::warn!("failed to record vocabulary hits for completed dictation: {error}");
                None
            }
        };
        let front_app =
            crate::shared_types::split_front_app_opt(context.polish.front_app.as_deref());
        let insert_status = match insert_outcome {
            Some(InsertOutcome::Inserted) => HistoryInsertStatus::Inserted,
            Some(InsertOutcome::PasteSent) => HistoryInsertStatus::PasteSent,
            Some(InsertOutcome::CopiedFallback) => HistoryInsertStatus::CopiedFallback,
            None => HistoryInsertStatus::NotRequested,
        };
        let pipeline_mode = match context.pipeline_mode {
            crate::shared_types::PipelineMode::Traditional => "traditional",
            crate::shared_types::PipelineMode::Multimodal => "multimodal",
        };
        let llm_used = context.uses_llm_polisher();
        let attribution = HistoryProviderAttribution::from_context(
            context,
            llm_used,
            engine_result.asr_ms,
            engine_result.polish_ms,
            engine_result.asr_call_label.as_ref(),
            engine_result.llm_call_label.as_ref(),
        );
        let session = DictationSession {
            id: result.session_id.to_string(),
            created_at: self.clock.now_utc().to_rfc3339(),
            source: HistorySource::Voice,
            raw_transcript: result.raw_text.clone(),
            asr_transcript: engine_result.asr_transcript.clone(),
            final_text: result.polished_text.clone(),
            mode: context.polish.mode,
            style_pack_id: Some(context.polish.style_pack_id.clone()),
            translation_active: context.polish.translation_active,
            polish_source: result.polish_source.clone(),
            app_bundle_id: front_app.bundle_id,
            app_name: front_app.name,
            insert_status,
            error_code: engine_result
                .polish_failed
                .then(|| "polishFailed".to_string()),
            duration_ms: Some(result.duration_ms),
            dictionary_entry_count,
            has_audio_recording: engine_result.has_audio_recording,
            asr_provider: attribution.asr_provider,
            asr_model: attribution.asr_model,
            llm_provider: attribution.llm_provider,
            llm_model: attribution.llm_model,
            pipeline_mode: Some(pipeline_mode.to_string()),
            asr_ms: attribution.asr_ms,
            polish_ms: attribution.polish_ms,
            // 诊断快照随记录内嵌：成功 / 回退 raw（polish_failed）都携带引擎快照；
            // Raw 直通没有引擎快照时补一条 NotCalled（无 prompt 可拼，不伪造）；
            // llm_used 但引擎未提供诊断（测试桩）保持 None，UI 显示「未保存诊断」。
            polish_diagnostics: engine_result.polish_diagnostics.clone().or_else(|| {
                (!llm_used).then(|| {
                    crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                        context,
                        "raw passthrough: the polisher was bypassed",
                    )
                })
            }),
        };
        if let Err(error) = self.append_history(
            session,
            preferences.history_retention_days,
            preferences.history_max_entries,
        ) {
            log::warn!("failed to persist completed dictation history: {error}");
        }
        if let Err(error) = self.record_activity(
            &self.clock.today_local().format("%Y-%m-%d").to_string(),
            result.polished_text.chars().count() as u64,
            result.duration_ms,
        ) {
            log::warn!("failed to persist completed dictation activity: {error}");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_failed_dictation(
        &self,
        context: &DictationContext,
        session_id: SessionId,
        error_code: &str,
        raw_text: String,
        final_text: String,
        polish_source: Option<String>,
        duration_ms: Option<u64>,
        asr_ms: Option<u64>,
        polish_ms: Option<u64>,
        has_audio_recording: Option<bool>,
        llm_used: bool,
        asr_call_label: Option<crate::auxiliary::AsrCallLabel>,
        llm_call_label: Option<crate::polish::LlmCallLabel>,
        polish_diagnostics: Option<crate::polish_diagnostics::PolishDiagnostics>,
    ) {
        let preferences = self.get_preferences();
        let front_app =
            crate::shared_types::split_front_app_opt(context.polish.front_app.as_deref());
        let pipeline_mode = match context.pipeline_mode {
            crate::shared_types::PipelineMode::Traditional => "traditional",
            crate::shared_types::PipelineMode::Multimodal => "multimodal",
        };
        let attribution = HistoryProviderAttribution::from_context(
            context,
            llm_used,
            asr_ms,
            polish_ms,
            asr_call_label.as_ref(),
            llm_call_label.as_ref(),
        );
        let session = DictationSession {
            id: session_id.to_string(),
            created_at: self.clock.now_utc().to_rfc3339(),
            source: HistorySource::Voice,
            raw_transcript: raw_text.clone(),
            asr_transcript: Some(raw_text),
            final_text,
            mode: context.polish.mode,
            style_pack_id: Some(context.polish.style_pack_id.clone()),
            translation_active: context.polish.translation_active,
            polish_source,
            app_bundle_id: front_app.bundle_id,
            app_name: front_app.name,
            insert_status: HistoryInsertStatus::Failed,
            error_code: Some(error_code.to_string()),
            duration_ms,
            dictionary_entry_count: None,
            has_audio_recording,
            asr_provider: attribution.asr_provider,
            asr_model: attribution.asr_model,
            llm_provider: attribution.llm_provider,
            llm_model: attribution.llm_model,
            pipeline_mode: Some(pipeline_mode.to_string()),
            asr_ms: attribution.asr_ms,
            polish_ms: attribution.polish_ms,
            // 失败也携带快照：润色阶段失败/回退用引擎快照（含渲染出的 prompt）；
            // 更早失败（转写/插入启动）由调用方给 NotCalled 原因。
            polish_diagnostics,
        };
        if let Err(error) = self.append_history(
            session,
            preferences.history_retention_days,
            preferences.history_max_entries,
        ) {
            log::warn!("failed to persist dictation failure history: {error}");
        }
    }

    fn hide_dictation_feedback(&self, session_id: SessionId) -> Result<(), BackendError> {
        let state = self.state.read().expect("backend state lock poisoned");
        if state
            .dictation
            .session_id
            .is_some_and(|current| current != session_id)
        {
            return Ok(());
        }
        // Cancellation resets its snapshot before awaiting native cleanup, so
        // Idle still permits its Hide. Any live successor suppresses it. Keep
        // the check and synchronous Host enqueue under one read guard so the
        // successor's Show cannot overtake an old terminal action. All success,
        // failure, cancellation and shutdown exits share this same boundary.
        self.deps
            .host_actions
            .request(HostAction::HideDictationFeedback)
    }

    fn reset_dictation_session(&self, session_id: SessionId) {
        // Match modifier-grace dispatch's hotkey -> state lock order. Clearing
        // the physical generation must precede exposing Idle/releasing audio;
        // otherwise an accepted successor press can be erased by this terminal.
        let mut hotkey = self
            .hotkey
            .lock()
            .expect("hotkey interpreter lock poisoned");
        let mut state = self.state.write().expect("backend state lock poisoned");
        if state.dictation.session_id != Some(session_id) {
            return;
        }
        state.dictation = DictationStateSnapshot::default();
        if let Some(context) = state.dictation_context.as_ref() {
            context.discard_pending_screen_ocr();
        }
        state.dictation_context = None;
        state.silence_monitor = None;
        state.transcripts.remove(&session_id);
        hotkey.terminal(std::time::Instant::now());
        self.phase_changed.notify_waiters();
        drop(state);
        self.voice_sessions.release(session_id);
    }

    fn cancel_session_adapters(
        &self,
        session_id: SessionId,
    ) -> futures_util::future::BoxFuture<'static, Result<(), BackendError>> {
        let engine = Arc::clone(&self.deps.dictation_engine);
        let resources = self.voice_sessions.hold_resources(session_id).ok();
        let insertion = self.cancel_text_insertion(session_id);
        // Revoke startup before scheduling cleanup. A queued engine.start must
        // observe cancellation even when its original caller has disappeared.
        // The owned cleanup hold keeps Busy until native stop/restore settles.
        self.voice_sessions.release(session_id);
        own_voice_effect(
            &self.deps.task_spawner,
            Box::pin(async move {
                let _resources = resources;
                let engine_result = engine.cancel(session_id).await;
                let inserter_result = insertion.await;
                engine_result?;
                inserter_result
            }),
        )
    }

    async fn finish_text_insertion(
        &self,
        session_id: SessionId,
        final_text: String,
    ) -> Result<InsertOutcome, BackendError> {
        let insertion = self
            .text_insertions
            .lock()
            .expect("text insertion registry lock poisoned")
            .get(&session_id)
            .cloned()
            .ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "text insertion session is not active",
                )
            })?;
        let insertion = insertion.await?;
        let result = insertion.finish(final_text).await;
        let mut insertions = self
            .text_insertions
            .lock()
            .expect("text insertion registry lock poisoned");
        if insertions
            .get(&session_id)
            .and_then(|preparation| preparation.peek())
            .is_some_and(
                |current| matches!(current, Ok(current) if Arc::ptr_eq(current, &insertion)),
            )
        {
            insertions.remove(&session_id);
        }
        result
    }

    fn cancel_text_insertion(
        &self,
        session_id: SessionId,
    ) -> futures_util::future::BoxFuture<'static, Result<(), BackendError>> {
        let insertion = self
            .text_insertions
            .lock()
            .expect("text insertion registry lock poisoned")
            .remove(&session_id);
        Box::pin(async move {
            match insertion {
                Some(preparation) => preparation.await?.cancel().await,
                None => Ok(()),
            }
        })
    }

    pub async fn report_recording_fault(
        &self,
        session_id: SessionId,
        error: BackendError,
    ) -> Result<(), BackendError> {
        let (context, duration_ms, already_failed) = {
            let state = self.state.read().expect("backend state lock poisoned");
            ensure_running(&state)?;
            if state.dictation.session_id != Some(session_id) {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "recording fault belongs to an inactive session",
                ));
            }
            (
                state.dictation_context.clone().ok_or_else(|| {
                    BackendError::new(
                        BackendErrorCode::Internal,
                        "active recording fault has no captured context",
                    )
                })?,
                state.dictation.elapsed_ms,
                state.dictation.phase == DictationPhase::Failed,
            )
        };
        // RecordingProgressForwarder marks the state Failed immediately so UI
        // feedback is real-time. The adapter then calls this method to perform
        // async cancellation/persistence; accepting that already-Failed phase
        // closes the two halves without emitting a second terminal event.
        if !already_failed {
            self.mark_dictation_failed(session_id, &error);
        }
        self.persist_failed_dictation(
            &context,
            session_id,
            "recordingFailed",
            String::new(),
            String::new(),
            None,
            Some(duration_ms),
            None,
            None,
            None,
            false,
            None,
            None,
            Some(crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                &context,
                "recording failed before transcription started",
            )),
        );
        let cancel_result = self.cancel_session_adapters(session_id).await;
        let _ = self.hide_dictation_feedback(session_id);
        self.reset_dictation_session(session_id);
        cancel_result
    }

    pub async fn cancel_dictation(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<(), BackendError> {
        let active = {
            let mut state = self.state.write().expect("backend state lock poisoned");
            ensure_running(&state)?;
            let active = state.dictation.session_id.ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "no active dictation session",
                )
            })?;
            if session_id.is_some() && session_id != Some(active) {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidArgument,
                    "session id does not match the active session",
                ));
            }
            state.dictation.phase = DictationPhase::Cancelled;
            self.events.publish(
                Some(active),
                BackendEventKind::DictationStateChanged(state.dictation.clone()),
            );
            state.dictation = DictationStateSnapshot::default();
            if let Some(context) = state.dictation_context.as_ref() {
                context.discard_pending_screen_ocr();
            }
            state.dictation_context = None;
            state.silence_monitor = None;
            state.transcripts.remove(&active);
            self.phase_changed.notify_waiters();
            active
        };
        let cancel_result = self.cancel_session_adapters(active).await;
        // The state can already display cancellation, but native audio/input
        // cleanup still owns the shared resource. Reject new capture until that
        // cleanup finishes, including on its error path.
        self.voice_sessions.release(active);
        let host_result = self.hide_dictation_feedback(active);
        cancel_result?;
        host_result?;
        Ok(())
    }

    async fn capture_dictation_context(
        &self,
        options: &DictationStartOptions,
        purpose: DictationContextPurpose,
    ) -> Result<DictationContext, BackendError> {
        let preferences = self.get_preferences();
        // 屏幕上下文：在任何可能耗时的宿主等待（AX 光标读取等）之前，同步冻结
        // 前台目标并把截图+OCR 派发到 host 的后台线程；录音 start 不为 OCR 等待。
        // 开关关闭时 start_screen_ocr 一次都不会被调用（host 不产生任何截图）。
        // 启动失败也记录轻量原因（NotStarted），诊断里不再静默。
        let (screen_ocr_pending, screen_ocr_trace) = if preferences.screen_ocr_context_enabled {
            match self.deps.services.host_context.start_screen_ocr() {
                crate::ports::ScreenOcrStart::Started(slot) => (
                    Some(crate::screen_ocr::ScreenOcrPending::new(slot)),
                    crate::polish_diagnostics::screen_ocr_pending_trace(),
                ),
                crate::ports::ScreenOcrStart::NotStarted(reason) => (
                    None,
                    crate::polish_diagnostics::screen_ocr_not_started_trace(reason),
                ),
            }
        } else {
            (
                None,
                crate::polish_diagnostics::PolishContextTrace::new(
                    crate::polish_diagnostics::PolishContextStatus::Disabled,
                    Some("screen context disabled".to_string()),
                ),
            )
        };
        let mut captured_options = options.clone();
        // 光标上下文轨迹：开关关闭 = Disabled；打开则记录宿主 probe 的真实结局
        // （blocked/unavailable/timeout/unsupported 都要有状态，见 HostContextCapture）。
        let mut cursor_trace = crate::polish_diagnostics::PolishContextTrace::new(
            crate::polish_diagnostics::PolishContextStatus::Disabled,
            Some("cursor context disabled".to_string()),
        );
        if !preferences.cursor_context_enabled {
            // This switch controls document text, including text supplied by
            // callers. Front-application metadata is still needed for history,
            // application-aware polish, and macOS Auto newline selection.
            captured_options.cursor_context = None;
        }
        if captured_options.front_app.is_none()
            || (preferences.cursor_context_enabled && captured_options.cursor_context.is_none())
        {
            match self
                .deps
                .services
                .host_context
                .capture(preferences.cursor_context_enabled)
                .await
            {
                Ok(capture) => {
                    captured_options.front_app = captured_options.front_app.or(capture.front_app);
                    if preferences.cursor_context_enabled {
                        captured_options.cursor_context =
                            captured_options.cursor_context.or(capture.cursor_context);
                        if captured_options.cursor_context.is_none() {
                            // 只有宿主真的被询问过光标读取时才用它的 probe 结局覆盖；
                            // 调用方已自带正文时保持 Included 候选轨迹。
                            cursor_trace = crate::polish_diagnostics::cursor_trace_from_capture(
                                capture.cursor_status.as_deref(),
                                capture.cursor_reason.clone(),
                                false,
                            );
                        }
                    }
                }
                Err(error) => {
                    log::warn!("host context capture failed: {error}");
                    if preferences.cursor_context_enabled
                        && captured_options.cursor_context.is_none()
                    {
                        cursor_trace = crate::polish_diagnostics::PolishContextTrace::new(
                            crate::polish_diagnostics::PolishContextStatus::Failed,
                            Some("host context capture failed".to_string()),
                        );
                    }
                }
            }
        }
        let options = &captured_options;
        let style_pack_id = options
            .style_pack_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&preferences.active_style_pack_id);
        let style_pack = self.style_packs.get_or_default_active(style_pack_id)?;
        let hotwords = self
            .vocabulary
            .list()?
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.phrase)
            .collect();
        // 纯本地改造:provider 只由 preferences 决定,不再经过凭据 channel
        // 解析(云端 channel 体系已移除)。传统管线是唯一路径;多模态/Omni
        // 偏好字段保留但永远不可达。
        let pipeline_mode = crate::shared_types::PipelineMode::Traditional;
        let _ = purpose;
        let active_asr_provider =
            crate::dictation_context::ProviderInvocation::for_provider(&preferences.active_asr_provider);
        let active_llm_provider =
            crate::dictation_context::ProviderInvocation::for_provider(&preferences.active_llm_provider);
        let active_omni_provider =
            crate::dictation_context::ProviderInvocation::for_provider(&preferences.active_omni_provider);
        let recent_history = if preferences.polish_context_window_minutes == 0 {
            Vec::new()
        } else {
            match self
                .history
                .recent_within_minutes(preferences.polish_context_window_minutes)
            {
                Ok(sessions) => sessions,
                Err(error) => {
                    log::warn!(
                        "failed to capture polish history context; using a single turn: {error}"
                    );
                    Vec::new()
                }
            }
        };
        let mut context = DictationContext::capture(
            &preferences,
            &style_pack,
            DictationProviderInvocations::new(
                active_asr_provider,
                active_llm_provider,
                active_omni_provider,
            ),
            hotwords,
            recent_history,
            options,
        );
        context.screen_ocr_pending = screen_ocr_pending.unwrap_or_default();
        context.polish.screen_ocr_trace = screen_ocr_trace;
        // 调用方自带正文（如 options.cursor_context 直传）保持 Included 候选；
        // 宿主刚读取成功也一样（Included），失败路径已在上面覆盖轨迹。
        if context.polish.cursor_context.is_some() {
            context.polish.cursor_context_trace = crate::polish_diagnostics::PolishContextTrace::included();
        } else if preferences.cursor_context_enabled {
            context.polish.cursor_context_trace = cursor_trace;
        }
        context.pipeline_mode = pipeline_mode;
        context.correction_rules = self
            .correction_rules
            .list()?
            .into_iter()
            .filter(|rule| rule.enabled)
            .collect();
        Ok(context)
    }

    fn mark_dictation_failed(&self, session_id: SessionId, error: &BackendError) {
        let mut state = self.state.write().expect("backend state lock poisoned");
        if state.dictation.session_id != Some(session_id) {
            return;
        }
        state.dictation.phase = DictationPhase::Failed;
        state.dictation.message = Some(format!("{:?}", error.code));
        let snapshot = state.dictation.clone();
        self.events.publish(
            Some(session_id),
            BackendEventKind::DictationStateChanged(snapshot),
        );
        self.phase_changed.notify_waiters();
    }
}

fn ensure_running(state: &MutableState) -> Result<(), BackendError> {
    if state.running {
        Ok(())
    } else {
        Err(BackendError::new(
            BackendErrorCode::InvalidState,
            "backend is not started",
        ))
    }
}

fn ensure_active_session(state: &MutableState, session_id: SessionId) -> Result<(), BackendError> {
    ensure_running(state)?;
    if state.dictation.session_id == Some(session_id)
        && !matches!(
            state.dictation.phase,
            DictationPhase::Idle
                | DictationPhase::Completed
                | DictationPhase::Cancelled
                | DictationPhase::Failed
        )
    {
        Ok(())
    } else {
        Err(BackendError::new(
            BackendErrorCode::Cancelled,
            "dictation progress belongs to an inactive session",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures_util::future::BoxFuture;

    use super::*;
    use crate::config::{BackendConfig, TokioTaskSpawner};
    use crate::errors::BackendError;
    use crate::ports::{
        boxed, DictationEngine, EditObservationAdapter, EditObservationSink, EngineFailure,
        EngineProgressSink, EngineResult, HostAction, HostActions, HostContextAdapter,
        HostContextCapture, InsertOutcome, InsertWriteResult, TextInserter, TextInsertionSession,
    };

    fn assert_send_sync<T: Send + Sync>() {}

    struct TestDataDir {
        path: std::path::PathBuf,
    }

    impl TestDataDir {
        fn new(label: &str) -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "openless-core-{label}-{}",
                    uuid::Uuid::new_v4().simple()
                )),
            }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TestDataDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct TestBackend {
        backend: OpenLessBackend,
        _data_dir: TestDataDir,
    }

    impl std::ops::Deref for TestBackend {
        type Target = OpenLessBackend;

        fn deref(&self) -> &Self::Target {
            &self.backend
        }
    }

    #[test]
    fn backend_is_safe_to_share_between_host_and_ui_tasks() {
        assert_send_sync::<OpenLessBackend>();
    }

    #[derive(Default)]
    struct FakeHost(Mutex<Vec<HostAction>>);

    impl HostActions for FakeHost {
        fn request(&self, action: HostAction) -> Result<(), BackendError> {
            self.0.lock().unwrap().push(action);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeHostContext(
        std::sync::atomic::AtomicUsize,
        std::sync::atomic::AtomicUsize,
    );

    impl HostContextAdapter for FakeHostContext {
        fn capture(
            &self,
            include_cursor: bool,
        ) -> BoxFuture<'static, Result<HostContextCapture, BackendError>> {
            self.0.fetch_add(1, Ordering::AcqRel);
            if include_cursor {
                self.1.fetch_add(1, Ordering::AcqRel);
            }
            boxed(async move {
                Ok(HostContextCapture {
                    front_app: Some("Terminal (com.apple.Terminal)".into()),
                    cursor_context: include_cursor.then(|| "before <OPENLESS_CURSOR> after".into()),
                    cursor_status: include_cursor.then(|| "ok".to_string()),
                    cursor_reason: None,
                })
            })
        }
    }

    #[derive(Default)]
    struct FakeEditObservation {
        typed_texts: Mutex<Vec<String>>,
        sinks: Mutex<Vec<Arc<dyn EditObservationSink>>>,
    }

    impl FakeEditObservation {
        fn publish(&self, index: usize, edit: crate::host_document::EditPair) {
            let _ = self.sinks.lock().unwrap()[index].publish(edit);
        }
    }

    impl EditObservationAdapter for FakeEditObservation {
        fn arm(
            &self,
            typed_text: String,
            sink: Arc<dyn EditObservationSink>,
        ) -> Result<(), BackendError> {
            self.typed_texts.lock().unwrap().push(typed_text);
            self.sinks.lock().unwrap().push(sink);
            Ok(())
        }

        fn disarm(&self) {}
    }

    struct FakeEngine;

    impl DictationEngine for FakeEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }

        fn finish(
            &self,
            _session_id: SessionId,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
            boxed(async {
                Ok(EngineResult {
                    raw_text: "raw".to_string(),
                    asr_transcript: None,
                    polished_text: "polished".to_string(),
                    polish_source: None,
                    duration_ms: 1000,
                    polish_failed: false,
                    asr_ms: None,
                    polish_ms: None,
                    has_audio_recording: None,
                    asr_call_label: None,
                    llm_call_label: None,
                    polish_diagnostics: None,
                })
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }
    }

    struct FakeInserter;

    impl TextInserter for FakeInserter {
        fn begin(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
        ) -> BoxFuture<'static, Result<Arc<dyn TextInsertionSession>, BackendError>> {
            boxed(async { Ok(Arc::new(FakeInsertionSession) as Arc<dyn TextInsertionSession>) })
        }
    }

    struct FakeInsertionSession;

    impl TextInsertionSession for FakeInsertionSession {
        fn write(
            &self,
            text: String,
        ) -> BoxFuture<'static, Result<InsertWriteResult, BackendError>> {
            boxed(async move {
                Ok(InsertWriteResult {
                    written_chars: text.chars().count(),
                })
            })
        }

        fn copy(&self, _text: String) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }

        fn finish(
            &self,
            _final_text: String,
        ) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
            boxed(async { Ok(InsertOutcome::Inserted) })
        }

        fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }
    }

    struct FailingEngine(std::sync::atomic::AtomicUsize);

    impl DictationEngine for FailingEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }

        fn finish(
            &self,
            _session_id: SessionId,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
            boxed(async {
                Err(EngineFailure::from(BackendError::new(
                    BackendErrorCode::Provider,
                    "fixture provider failure",
                )))
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            self.0.fetch_add(1, Ordering::AcqRel);
            boxed(async { Ok(()) })
        }
    }

    struct PolishMetadataFailingEngine;

    impl DictationEngine for PolishMetadataFailingEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }

        fn finish(
            &self,
            _session_id: SessionId,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
            boxed(async {
                Err(EngineFailure {
                    error: BackendError::new(BackendErrorCode::Provider, "fixture omni failure"),
                    stage: EngineFailureStage::Polishing,
                    raw_text: Some("omni raw".to_string()),
                    duration_ms: Some(900),
                    asr_ms: Some(300),
                    polish_ms: Some(600),
                    has_audio_recording: Some(true),
                    asr_call_label: None,
                    llm_call_label: None,
                    polish_diagnostics: None,
                })
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }
    }

    struct StartFailingEngine;

    impl DictationEngine for StartFailingEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async {
                Err(BackendError::new(
                    BackendErrorCode::Platform,
                    "fixture recorder start failure",
                ))
            })
        }

        fn finish(
            &self,
            _session_id: SessionId,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
            boxed(async {
                Err(EngineFailure::from(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "fixture start never completed",
                )))
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }
    }

    struct BlockingStartEngine {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl DictationEngine for BlockingStartEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<(), BackendError>> {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            boxed(async move {
                entered.notify_one();
                release.notified().await;
                Ok(())
            })
        }

        fn finish(
            &self,
            _session_id: SessionId,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
            boxed(async {
                Ok(EngineResult {
                    raw_text: "raw".to_string(),
                    asr_transcript: None,
                    polished_text: "polished".to_string(),
                    polish_source: None,
                    duration_ms: 1000,
                    polish_failed: false,
                    asr_ms: None,
                    polish_ms: None,
                    has_audio_recording: None,
                    asr_call_label: None,
                    llm_call_label: None,
                    polish_diagnostics: None,
                })
            })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }
    }

    fn backend() -> (TestBackend, Arc<FakeHost>) {
        let host = Arc::new(FakeHost::default());
        let data_dir = TestDataDir::new("facade");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: host.clone(),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        (
            TestBackend {
                backend,
                _data_dir: data_dir,
            },
            host,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn old_cli_completion_cannot_erase_an_accepted_physical_hold_press() {
        use crate::shared_types::{HotkeyMode, ShortcutBinding};

        // Hold the interpreter while the public key entry and a CLI-style stop
        // queue on separate threads. Mutex wake order is not a contract, so
        // repeat the overlap and check every new press that was accepted.
        for iteration in 0..32 {
            let (backend, _) = backend();
            let backend = Arc::new(backend);
            let mut preferences = backend.get_preferences();
            preferences.hotkey.mode = HotkeyMode::Hold;
            preferences.dictation_hotkey = ShortcutBinding {
                primary: "F11".into(),
                modifiers: vec!["ctrl".into()],
            };
            backend
                .update_settings(
                    preferences,
                    crate::SettingsUpdateOptions::STRICT,
                    &crate::NoopSettingsRuntime,
                )
                .unwrap();
            backend.start().await.unwrap();
            let (locked, lock_ready) = tokio::sync::oneshot::channel();
            let (unlock, unlock_rx) = std::sync::mpsc::channel();
            let locking = std::thread::spawn({
                let backend = backend.clone();
                move || {
                    let _interpreter = backend.hotkey.lock().unwrap();
                    let _ = locked.send(());
                    let _ = unlock_rx.recv();
                }
            });
            lock_ready.await.unwrap();
            let pressed_at = std::time::Instant::now();
            let press_id = 1;
            let pressing = tokio::spawn({
                let backend = backend.clone();
                async move {
                    backend
                        .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                            press_id,
                            at: pressed_at,
                        })
                        .await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while backend.hotkey_dispatch_gate.try_lock().is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // The key entry owns the FIFO dispatch gate and is waiting for the
            // interpreter. Starting/stopping by another public entry is legal.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            let first = backend.start_dictation().await.unwrap();
            let stopping = tokio::spawn({
                let backend = backend.clone();
                async move { backend.stop_dictation_session(first).await }
            });
            let _ = tokio::time::timeout(std::time::Duration::from_millis(30), async {
                while backend.snapshot().dictation.phase != DictationPhase::Idle {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            unlock.send(()).unwrap();
            tokio::task::spawn_blocking(move || locking.join().unwrap())
                .await
                .unwrap();
            let press = pressing.await.unwrap();
            stopping.await.unwrap().unwrap();

            match press {
                Ok(CliDispatchOutcome::DictationStarted(second)) => {
                    let released = backend
                        .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Released {
                            press_id,
                            at: pressed_at + std::time::Duration::from_secs(1),
                        })
                        .await
                        .unwrap();
                    assert!(matches!(released, CliDispatchOutcome::DictationCompleted(ref result)
                        if result.session_id == second),
                        "old stop erased accepted Hold generation in iteration {iteration}: {released:?}");
                }
                Ok(CliDispatchOutcome::Noop) => {}
                Err(error) if error.code == BackendErrorCode::Busy => {}
                result => panic!("unexpected key result: {result:?}"),
            }
            assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
            backend.shutdown().await.unwrap();
        }
    }






    #[derive(Default)]
    struct VoiceTranscription {
        pcm: Mutex<Vec<u8>>,
        cancelled: std::sync::atomic::AtomicBool,
    }

    impl crate::ports::AudioConsumer for VoiceTranscription {
        fn consume_pcm_chunk(&self, pcm: &[u8]) {
            self.pcm.lock().unwrap().extend_from_slice(pcm);
        }
    }

    impl crate::ports::TranscriptionSession for VoiceTranscription {
        fn finish(&self) -> BoxFuture<'static, Result<crate::TranscriptOutput, BackendError>> {
            boxed(async {
                Ok(crate::TranscriptOutput {
                    text: "执行语音任务".into(),
                    duration_ms: 100,
                })
            })
        }

        fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
            self.cancelled
                .store(true, std::sync::atomic::Ordering::Release);
            boxed(async { Ok(()) })
        }
    }

    struct VoiceOnlyEngine(Arc<VoiceTranscription>);

    impl DictationEngine for VoiceOnlyEngine {
        fn start(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }

        fn start_transcription(
            &self,
            _session_id: SessionId,
            _context: Arc<DictationContext>,
            _partials: Arc<dyn TextStreamSink>,
        ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
            let session: Arc<dyn TranscriptionSession> = self.0.clone();
            boxed(async move { Ok(session) })
        }

        fn finish(
            &self,
            _session_id: SessionId,
            _progress: Arc<dyn EngineProgressSink>,
        ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
            boxed(async { unreachable!("voice-only engine does not run dictation") })
        }

        fn cancel(&self, _session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
            boxed(async { Ok(()) })
        }
    }





    #[derive(Default)]
    struct FakeRecordingControl {
        requests: Mutex<Vec<(SessionId, crate::events::RecordingControlAction)>>,
    }

    impl crate::ports::RecordingControlSink for FakeRecordingControl {
        fn request(
            &self,
            session_id: SessionId,
            action: crate::events::RecordingControlAction,
        ) -> Result<(), BackendError> {
            self.requests.lock().unwrap().push((session_id, action));
            Ok(())
        }
    }





    fn backend_with_dictation_engine(
        data_dir: std::path::PathBuf,
        dictation_engine: Arc<dyn DictationEngine>,
    ) -> OpenLessBackend {
        OpenLessBackend::new(
            BackendConfig {
                data_dir,
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine,
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap()
    }

    fn history_session(id: &str) -> DictationSession {
        DictationSession {
            id: id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source: crate::types::HistorySource::Voice,
            raw_transcript: "raw".to_string(),
            asr_transcript: None,
            final_text: "final".to_string(),
            mode: crate::types::PolishMode::Light,
            style_pack_id: None,
            translation_active: false,
            polish_source: None,
            app_bundle_id: None,
            app_name: None,
            insert_status: crate::types::HistoryInsertStatus::Inserted,
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
    fn vocabulary_facade_persists_shared_types_and_publishes_revisions() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-backend-vocab-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut events = backend.subscribe();

        let entry = backend.add_vocabulary("OpenLess".into(), None).unwrap();
        let rule = backend
            .add_correction_rule("几粒".into(), "几例".into())
            .unwrap();
        backend.set_vocabulary_enabled(&entry.id, false).unwrap();
        backend.remove_correction_rule(&rule.id).unwrap();

        assert!(!backend.list_vocabulary().unwrap()[0].enabled);
        assert!(backend.list_correction_rules().unwrap().is_empty());
        assert_eq!(backend.snapshot().vocabulary_revision, 4);
        for expected_revision in 1..=4 {
            let event = events.try_recv().unwrap();
            assert_eq!(
                event.kind,
                BackendEventKind::VocabularyChanged(VocabularyChange {
                    revision: expected_revision,
                })
            );
        }
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn correction_suggestions_are_bounded_idempotent_and_committed_by_core() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-correction-suggestions-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies::unsupported(),
        )
        .unwrap();
        let mut events = backend.subscribe();

        let first = backend
            .queue_pending_correction("扣的爱思".into(), "Codex".into())
            .unwrap()
            .unwrap();
        assert!(backend
            .queue_pending_correction("扣的爱思".into(), "Codex".into())
            .unwrap()
            .is_none());
        for index in 0..MAX_PENDING_CORRECTIONS {
            backend
                .queue_pending_correction(format!("old-{index}"), format!("new-{index}"))
                .unwrap();
        }
        let pending = backend.pending_corrections();
        assert_eq!(pending.len(), MAX_PENDING_CORRECTIONS);
        assert!(pending.iter().all(|item| item.id != first.id));

        let accepted = pending[0].clone();
        assert_eq!(
            backend
                .accept_pending_correction(&accepted.id)
                .unwrap()
                .unwrap(),
            accepted
        );
        assert!(backend
            .accept_pending_correction(&accepted.id)
            .unwrap()
            .is_none());
        let learned = backend
            .list_vocabulary()
            .unwrap()
            .into_iter()
            .find(|entry| entry.phrase == accepted.replacement)
            .unwrap();
        assert_eq!(learned.note.as_deref(), Some("从手改中自动收集"));

        let rejected = backend.pending_corrections()[0].id.clone();
        assert!(backend.reject_pending_correction(&rejected));
        assert!(!backend.reject_pending_correction(&rejected));
        backend.dismiss_pending_corrections();
        backend.dismiss_pending_corrections();
        assert!(backend.pending_corrections().is_empty());

        let mut suggestion_events = 0;
        let mut vocabulary_events = 0;
        while let Ok(event) = events.try_recv() {
            match event.kind {
                BackendEventKind::VocabularySuggestionsChanged(_) => suggestion_events += 1,
                BackendEventKind::VocabularyChanged(_) => vocabulary_events += 1,
                _ => {}
            }
        }
        assert_eq!(suggestion_events, MAX_PENDING_CORRECTIONS + 4);
        assert_eq!(vocabulary_events, 1);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn history_facade_persists_shared_types_and_publishes_revisions() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-backend-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut events = backend.subscribe();

        let mut entry = history_session("one");
        // 预置一条诊断快照：重新转写覆盖 raw/final 后必须清除，否则旧 prompt
        // 会被误当成「这条记录实际送入模型的内容」。
        entry.polish_diagnostics = Some(crate::polish_diagnostics::PolishDiagnostics {
            stage: crate::polish_diagnostics::PolishDiagnosticsStage::Succeeded,
            outcome: crate::polish_diagnostics::PolishDiagnosticsOutcome::Succeeded,
            provider: Some("local-llama".into()),
            model: Some("stale.gguf".into()),
            template: None,
            add_bos: None,
            add_bos_note: None,
            prompt: Some("stale prompt".into()),
            messages: None,
            prompt_tokens: None,
            context_tokens: None,
            generation_budget: None,
            error: None,
            not_called_reason: None,
            cursor: None,
            screen_ocr: None,
        });
        backend.append_history(entry.clone(), 30, Some(20)).unwrap();
        entry.final_text = "updated".to_string();
        assert!(backend.update_history_entry(entry.clone()).unwrap());
        assert!(!backend
            .update_history_entry(history_session("missing"))
            .unwrap());
        let retranscribed = backend
            .apply_history_retranscription(
                &entry.id,
                "retranscribed".into(),
                &crate::auxiliary::AsrCallLabel {
                    provider: "channel-b".into(),
                    model: Some("model-b".into()),
                },
                480,
            )
            .unwrap();
        assert_eq!(retranscribed.raw_transcript, "retranscribed");
        assert_eq!(retranscribed.final_text, "retranscribed");
        assert_eq!(retranscribed.asr_provider.as_deref(), Some("channel-b"));
        assert_eq!(retranscribed.asr_model.as_deref(), Some("model-b"));
        assert_eq!(retranscribed.asr_ms, Some(480));
        assert!(retranscribed.error_code.is_none());
        assert!(retranscribed.llm_provider.is_none());
        assert!(retranscribed.llm_model.is_none());
        assert!(retranscribed.polish_ms.is_none());
        assert!(
            retranscribed.polish_diagnostics.is_none(),
            "重新转写覆盖 raw/final 后，旧诊断快照必须一并清除"
        );
        backend.delete_history(&entry.id).unwrap();
        backend.clear_history().unwrap();
        backend.record_activity("2026-08-27", 42, 1000).unwrap();

        assert!(backend.list_history().unwrap().is_empty());
        assert_eq!(backend.list_activity().unwrap()[0].chars, 42);
        assert_eq!(backend.snapshot().history_revision, 6);
        for expected_revision in 1..=6 {
            assert_eq!(
                events.try_recv().unwrap().kind,
                BackendEventKind::HistoryChanged(HistoryChange {
                    revision: expected_revision,
                })
            );
        }
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn focused_microphone_selection_persists_and_publishes_once() {
        let (backend, _) = backend();
        let mut events = backend.subscribe();

        backend
            .select_microphone_device("Studio microphone".to_string())
            .unwrap();

        assert_eq!(
            backend.get_preferences().microphone_device_name,
            "Studio microphone"
        );
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        ));
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));
    }

    #[test]
    fn previous_style_use_case_owns_cycle_order_and_preferences_event() {
        let (backend, _) = backend();
        let before = backend.get_preferences();
        let mut events = backend.subscribe();

        let selected = backend
            .activate_previous_style_pack()
            .unwrap()
            .expect("default store has multiple enabled packs");

        assert_ne!(selected.id, before.active_style_pack_id);
        assert!(selected.active);
        assert_eq!(backend.get_preferences().active_style_pack_id, selected.id);
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert_eq!(backend.snapshot().style_pack_revision, 0);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        ));
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));
    }

    #[test]
    fn style_pack_facade_owns_mutations_and_publishes_revisions() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-backend-style-packs-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut events = backend.subscribe();
        let pack = backend
            .create_style_pack(StylePack {
                name: "Linux contract".to_string(),
                prompt: "prompt".to_string(),
                ..StylePack::default()
            })
            .unwrap();
        backend.set_style_pack_enabled(&pack.id, false).unwrap();
        backend.remove_style_pack(&pack.id).unwrap();

        assert_eq!(backend.snapshot().style_pack_revision, 3);
        let mut style_revisions = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let BackendEventKind::StylePacksChanged(change) = event.kind {
                style_revisions.push(change.revision);
            }
        }
        assert_eq!(style_revisions, vec![1, 2, 3]);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn activating_style_pack_publishes_preferences_and_style_revisions() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-activate-style-pack-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies::unsupported(),
        )
        .unwrap();
        let pack = backend
            .create_style_pack(StylePack {
                name: "Activate me".to_string(),
                prompt: "prompt".to_string(),
                ..StylePack::default()
            })
            .unwrap();
        let mut events = backend.subscribe();
        let before = backend.snapshot();

        let active = backend.activate_style_pack(&pack.id).unwrap();

        assert!(active.active);
        assert_eq!(backend.get_preferences().active_style_pack_id, pack.id);
        let after = backend.snapshot();
        assert_eq!(after.preferences_revision, before.preferences_revision + 1);
        assert_eq!(after.style_pack_revision, before.style_pack_revision + 1);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(_)
        ));
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::StylePacksChanged(_)
        ));
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn removing_style_pack_cleans_active_id_and_orphan_hotkey() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-remove-style-pack-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies::unsupported(),
        )
        .unwrap();
        let pack = backend
            .create_style_pack(StylePack {
                name: "Temporary pack".to_string(),
                prompt: "prompt".to_string(),
                ..StylePack::default()
            })
            .unwrap();
        let mut preferences = backend.get_preferences();
        preferences.active_style_pack_id = pack.id.clone();
        preferences
            .style_pack_hotkeys
            .push(crate::shared_types::StylePackHotkey {
                pack_id: pack.id.clone(),
                binding: crate::shared_types::ShortcutBinding {
                    primary: "K".to_string(),
                    modifiers: vec!["ctrl".to_string()],
                },
            });
        backend.set_preferences(preferences).unwrap();

        let outcome = backend.remove_style_pack(&pack.id).unwrap();

        let preferences = backend.get_preferences();
        assert_ne!(preferences.active_style_pack_id, pack.id);
        assert!(preferences
            .style_pack_hotkeys
            .iter()
            .all(|entry| entry.pack_id != pack.id));
        let hotkey_change = outcome
            .effects
            .hotkeys
            .expect("removal must expose the host hotkey effect");
        assert!(hotkey_change
            .previous
            .style_packs
            .iter()
            .any(|entry| entry.pack_id == pack.id));
        assert!(hotkey_change
            .next
            .style_packs
            .iter()
            .all(|entry| entry.pack_id != pack.id));
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn preferences_facade_persists_shared_contract_and_publishes_revisions() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-backend-preferences-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut events = backend.subscribe();

        let mut preferences = backend.get_preferences();
        preferences.microphone_device_name = "Shared microphone".to_string();
        backend.set_preferences(preferences).unwrap();

        assert_eq!(
            backend.get_preferences().microphone_device_name,
            "Shared microphone"
        );
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert_eq!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        );
        assert!(data_dir.join("preferences.json").is_file());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn validated_preferences_write_syncs_legacy_fields_and_publishes_once() {
        let (backend, _) = backend();
        let mut events = backend.subscribe();
        let mut preferences = backend.get_preferences();
        preferences.dictation_hotkey = crate::shared_types::ShortcutBinding {
            primary: "RightControl".to_string(),
            modifiers: Vec::new(),
        };

        backend.set_preferences_validated(preferences).unwrap();

        let saved = backend.get_preferences();
        assert_eq!(
            saved.hotkey.trigger,
            crate::shared_types::HotkeyTrigger::RightControl
        );
        assert_eq!(saved.custom_combo_hotkey, None);
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert_eq!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        );
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));
    }

    #[test]
    fn validated_preferences_write_rejects_conflicts_without_mutation_or_event() {
        let (backend, _) = backend();
        let before = backend.get_preferences();
        let before_json = serde_json::to_value(&before).unwrap();
        let mut events = backend.subscribe();
        let mut conflicting = before.clone();
        conflicting.translation_hotkey = conflicting.dictation_hotkey.clone();

        let error = backend
            .set_preferences_validated(conflicting)
            .expect_err("conflicting shortcut must be rejected");

        assert_eq!(error.code, BackendErrorCode::InvalidArgument);
        assert_eq!(
            serde_json::to_value(backend.get_preferences()).unwrap(),
            before_json
        );
        assert_eq!(backend.snapshot().preferences_revision, 0);
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));
    }

    #[test]
    fn validated_preferences_write_can_preserve_current_style_fields() {
        let (backend, _) = backend();
        let before = backend.get_preferences();
        let mut events = backend.subscribe();
        let mut incoming = before.clone();
        incoming.microphone_device_name = "Updated microphone".to_string();
        incoming.default_mode = crate::types::PolishMode::Raw;
        incoming.enabled_modes = vec![crate::types::PolishMode::Raw];
        incoming.active_style_pack_id = "incoming.style".to_string();
        incoming.custom_style_prompts.raw = "incoming prompt".to_string();

        backend
            .set_preferences_preserving_style_validated(incoming)
            .unwrap();

        let saved = backend.get_preferences();
        assert_eq!(saved.microphone_device_name, "Updated microphone");
        assert_eq!(saved.default_mode, before.default_mode);
        assert_eq!(saved.enabled_modes, before.enabled_modes);
        assert_eq!(saved.active_style_pack_id, before.active_style_pack_id);
        assert_eq!(saved.style_system_prompts, before.style_system_prompts);
        assert_eq!(saved.custom_style_prompts, before.custom_style_prompts);
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert_eq!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        );
        assert_eq!(events.try_recv(), Err(crate::events::EventRecvError::Empty));
    }

    #[derive(Default)]
    struct FailingSettingsRuntime {
        restore_error: Option<BackendError>,
    }

    impl crate::SettingsRuntime for FailingSettingsRuntime {
        fn prepare(
            &self,
            _plan: &crate::SettingsEffectPlan,
        ) -> Result<crate::SettingsEffectReceipt, crate::SettingsEffectFailure> {
            Err(crate::SettingsEffectFailure::after_side_effect(
                BackendError::new(BackendErrorCode::Platform, "runtime apply failed"),
                crate::SettingsEffectReceipt {
                    applied: vec![crate::SettingsEffectKind::ActiveAsrProvider],
                },
            ))
        }

        fn restore(
            &self,
            _plan: &crate::SettingsEffectPlan,
            _receipt: &crate::SettingsEffectReceipt,
        ) -> Result<(), BackendError> {
            self.restore_error.clone().map_or(Ok(()), Err)
        }
    }

    #[derive(Default)]
    struct RecordingSettingsRuntime {
        actions: Mutex<Vec<&'static str>>,
        fail_commit: bool,
    }

    impl crate::SettingsRuntime for RecordingSettingsRuntime {
        fn prepare(
            &self,
            plan: &crate::SettingsEffectPlan,
        ) -> Result<crate::SettingsEffectReceipt, crate::SettingsEffectFailure> {
            self.actions.lock().unwrap().push("prepare");
            Ok(crate::SettingsEffectReceipt {
                applied: plan
                    .active_asr_provider
                    .as_ref()
                    .map(|_| vec![crate::SettingsEffectKind::ActiveAsrProvider])
                    .unwrap_or_default(),
            })
        }

        fn commit(
            &self,
            _plan: &crate::SettingsEffectPlan,
            receipt: &mut crate::SettingsEffectReceipt,
        ) -> Result<(), crate::SettingsEffectFailure> {
            self.actions.lock().unwrap().push("commit");
            receipt.applied.push(crate::SettingsEffectKind::Hotkeys);
            if self.fail_commit {
                Err(crate::SettingsEffectFailure::after_side_effect(
                    BackendError::new(BackendErrorCode::Platform, "listener registration failed"),
                    receipt.clone(),
                ))
            } else {
                Ok(())
            }
        }

        fn restore(
            &self,
            _plan: &crate::SettingsEffectPlan,
            _receipt: &crate::SettingsEffectReceipt,
        ) -> Result<(), BackendError> {
            self.actions.lock().unwrap().push("restore");
            Ok(())
        }
    }

    #[test]
    fn settings_transaction_success_persists_and_publishes_once() {
        let (backend, _) = backend();
        let mut events = backend.subscribe();
        let mut next = backend.get_preferences();
        next.dictation_hotkey = crate::shared_types::ShortcutBinding {
            primary: "RightControl".into(),
            modifiers: vec![],
        };

        let outcome = backend
            .update_settings(
                next,
                crate::SettingsUpdateOptions::STRICT,
                &crate::NoopSettingsRuntime,
            )
            .unwrap();

        assert_eq!(
            outcome.preferences.hotkey.trigger,
            crate::shared_types::HotkeyTrigger::RightControl
        );
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        ));
        assert!(matches!(
            events.try_recv(),
            Err(crate::EventRecvError::Empty)
        ));
    }

    #[test]
    fn settings_runtime_failure_preserves_preferences_revision_and_events() {
        let (backend, _) = backend();
        let previous = backend.get_preferences();
        let mut events = backend.subscribe();
        let mut next = previous.clone();
        next.active_asr_provider = "fixture-asr".into();

        let error = backend
            .update_settings(
                next,
                crate::SettingsUpdateOptions::STRICT,
                &FailingSettingsRuntime::default(),
            )
            .unwrap_err();

        assert_eq!(error.code, BackendErrorCode::Platform);
        assert_eq!(
            serde_json::to_value(backend.get_preferences()).unwrap(),
            serde_json::to_value(previous).unwrap()
        );
        assert_eq!(backend.snapshot().preferences_revision, 0);
        assert!(matches!(
            events.try_recv(),
            Err(crate::EventRecvError::Empty)
        ));
    }

    #[test]
    fn settings_transaction_reports_primary_and_compensation_errors() {
        let (backend, _) = backend();
        let mut next = backend.get_preferences();
        next.active_asr_provider = "fixture-asr".into();
        let runtime = FailingSettingsRuntime {
            restore_error: Some(BackendError::new(
                BackendErrorCode::Platform,
                "runtime restore failed",
            )),
        };

        let error = backend
            .update_settings(next, crate::SettingsUpdateOptions::STRICT, &runtime)
            .unwrap_err();

        assert_eq!(error.message, "runtime apply failed");
        let details = error.details.expect("structured transaction details");
        assert_eq!(details["primaryError"]["message"], "runtime apply failed");
        assert_eq!(
            details["compensationErrors"][0]["message"],
            "runtime restore failed"
        );
    }

    #[test]
    fn settings_commit_failure_never_persists_or_publishes() {
        let (backend, _) = backend();
        let previous = backend.get_preferences();
        let mut next = previous.clone();
        next.dictation_hotkey = crate::shared_types::ShortcutBinding {
            primary: "F9".into(),
            modifiers: vec!["ctrl".into()],
        };
        let mut events = backend.subscribe();
        let runtime = RecordingSettingsRuntime {
            fail_commit: true,
            ..RecordingSettingsRuntime::default()
        };

        let error = backend
            .update_settings(next, crate::SettingsUpdateOptions::STRICT, &runtime)
            .unwrap_err();

        assert_eq!(error.code, BackendErrorCode::Platform);
        assert_eq!(
            serde_json::to_value(backend.get_preferences()).unwrap(),
            serde_json::to_value(&previous).unwrap()
        );
        assert_eq!(backend.snapshot().preferences_revision, 0);
        assert!(matches!(
            events.try_recv(),
            Err(crate::EventRecvError::Empty)
        ));
        assert_eq!(
            runtime.actions.lock().unwrap().as_slice(),
            ["prepare", "commit", "restore"]
        );
    }

    #[test]
    fn settings_persistence_failure_restores_prepared_effects() {
        let host = Arc::new(FakeHost::default());
        let data_dir = TestDataDir::new("settings-persistence-failure");
        let mut repositories = BackendRepositories::open(data_dir.path()).unwrap();
        repositories.preferences = Arc::new(crate::PreferencesStore::in_memory());
        let backend = OpenLessBackend::new_with_repositories(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: host,
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
            repositories,
        )
        .unwrap();
        let previous = backend.get_preferences();
        let mut next = previous.clone();
        next.active_asr_provider = "fixture-asr".into();
        let runtime = RecordingSettingsRuntime::default();
        let mut events = backend.subscribe();

        let error = backend
            .update_settings(next, crate::SettingsUpdateOptions::STRICT, &runtime)
            .unwrap_err();

        assert_eq!(error.code, BackendErrorCode::Persistence);
        assert_eq!(
            serde_json::to_value(backend.get_preferences()).unwrap(),
            serde_json::to_value(previous).unwrap()
        );
        assert_eq!(backend.snapshot().preferences_revision, 0);
        assert!(matches!(
            events.try_recv(),
            Err(crate::EventRecvError::Empty)
        ));
        assert_eq!(
            runtime.actions.lock().unwrap().as_slice(),
            ["prepare", "commit", "restore"]
        );
    }

    #[test]
    fn settings_document_reconciles_conflicts_and_preserves_current_style() {
        let (backend, _) = backend();
        let previous = backend.get_preferences();
        let mut next = previous.clone();
        next.microphone_device_name = "updated microphone".into();
        next.default_mode = crate::types::PolishMode::Raw;
        next.enabled_modes = vec![crate::types::PolishMode::Raw];
        next.active_style_pack_id = "stale-style".into();
        next.translation_hotkey = next.dictation_hotkey.clone();
        let mut events = backend.subscribe();

        let outcome = backend
            .update_settings(
                next,
                crate::SettingsUpdateOptions::SETTINGS_DOCUMENT,
                &crate::NoopSettingsRuntime,
            )
            .unwrap();

        assert!(outcome.reconciled_hotkey_count > 0);
        assert_eq!(
            outcome.preferences.microphone_device_name,
            "updated microphone"
        );
        assert_eq!(outcome.preferences.default_mode, previous.default_mode);
        assert_eq!(outcome.preferences.enabled_modes, previous.enabled_modes);
        assert_eq!(
            outcome.preferences.active_style_pack_id,
            previous.active_style_pack_id
        );
        crate::reject_hotkey_collisions(&outcome.preferences).unwrap();
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        ));
        assert!(matches!(
            events.try_recv(),
            Err(crate::EventRecvError::Empty)
        ));
    }

    #[test]
    fn settings_revision_guard_rejects_one_of_two_concurrent_stale_documents() {
        let (backend, _) = backend();
        let backend = Arc::new(backend);
        let expected_revision = backend.snapshot().preferences_revision;
        let mut microphone_update = backend.get_preferences();
        microphone_update.microphone_device_name = "concurrent microphone".into();
        let mut theme_update = backend.get_preferences();
        theme_update.theme_mode = crate::shared_types::ThemeMode::Light;
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let spawn = |preferences| {
            let backend = Arc::clone(&backend);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                backend.update_settings(
                    preferences,
                    crate::SettingsUpdateOptions::STRICT.at_revision(expected_revision),
                    &crate::NoopSettingsRuntime,
                )
            })
        };
        let first = spawn(microphone_update);
        let second = spawn(theme_update);
        let mut events = backend.subscribe();
        barrier.wait();

        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let stale = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one stale settings document");
        assert_eq!(stale.code, BackendErrorCode::Busy);
        assert!(stale.retryable);
        assert_eq!(backend.snapshot().preferences_revision, 1);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision: 1 })
        ));
        assert!(matches!(
            events.try_recv(),
            Err(crate::EventRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn credentials_facade_keeps_secrets_out_of_snapshots_and_publishes_status() {
        let credential_store = Arc::new(crate::credentials::InMemoryCredentialStore::default());
        credential_store.set_status(CredentialsStatus {
            active_asr_provider: "fixture-asr".to_string(),
            active_llm_provider: "fixture-llm".to_string(),
            asr_configured: true,
            ..CredentialsStatus::default()
        });
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: std::env::temp_dir().join(format!(
                    "openless-core-credentials-{}",
                    uuid::Uuid::new_v4().simple()
                )),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: credential_store.clone(),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut events = backend.subscribe();
        let startup = backend.start().await.unwrap();
        assert_eq!(
            startup.backend.credentials.active_asr_provider,
            "fixture-asr"
        );
        assert!(startup.backend.credentials.asr_configured);
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::BackendStarted
        ));

        let key = crate::credentials::CredentialKey::new(
            crate::credentials::CredentialNamespace::Asr,
            Some("fixture-asr".to_string()),
            "api_key",
        )
        .unwrap();
        backend
            .set_credential(
                key.clone(),
                crate::credentials::SecretValue::new("not-in-the-snapshot"),
            )
            .await
            .unwrap();
        assert_eq!(
            backend
                .read_credential(key)
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "not-in-the-snapshot"
        );
        assert!(matches!(
            events.try_recv().unwrap().kind,
            BackendEventKind::CredentialsChanged(_)
        ));
        let snapshot_json = serde_json::to_string(&backend.snapshot()).unwrap();
        assert!(!snapshot_json.contains("not-in-the-snapshot"));
        assert!(!snapshot_json.contains("api_key"));
    }


    #[tokio::test]
    async fn lifecycle_is_idempotent_and_emits_started_once_per_transition() {
        let (backend, _) = backend();
        let mut events = backend.subscribe();
        backend.start().await.unwrap();
        backend.start().await.unwrap();
        assert_eq!(
            events.recv().await.unwrap().kind,
            BackendEventKind::BackendStarted
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), events.recv())
                .await
                .is_err()
        );
        backend.shutdown().await.unwrap();
        backend.shutdown().await.unwrap();
        assert_eq!(
            events.recv().await.unwrap().kind,
            BackendEventKind::BackendStopping
        );
    }

    #[tokio::test]
    async fn front_app_is_captured_without_reading_documents_when_cursor_context_is_disabled() {
        let data_dir = TestDataDir::new("host-context-privacy");
        let host_context = Arc::new(FakeHostContext::default());
        let mut services = crate::domains::BackendServices::unsupported();
        services.host_context = host_context.clone();
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                services,
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                credential_store: Arc::new(crate::InMemoryCredentialStore::default()),
                ..BackendDependencies::unsupported()
            },
        )
        .unwrap();

        let context = backend
            .capture_host_dictation_context(DictationStartOptions::default())
            .await
            .unwrap();
        assert_eq!(host_context.0.load(Ordering::Acquire), 1);
        assert_eq!(host_context.1.load(Ordering::Acquire), 0);
        assert!(context.polish.cursor_context.is_none());
        assert_eq!(
            context.polish.front_app.as_deref(),
            Some("Terminal (com.apple.Terminal)")
        );
        assert_eq!(
            context.insertion.macos_newline_mode,
            crate::shared_types::MacosNewlineMode::LineFeed
        );

        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = true;
        backend.set_preferences(preferences).unwrap();
        let context = backend
            .capture_host_dictation_context(DictationStartOptions::default())
            .await
            .unwrap();
        assert_eq!(host_context.0.load(Ordering::Acquire), 2);
        assert_eq!(host_context.1.load(Ordering::Acquire), 1);
        assert_eq!(
            context.polish.front_app.as_deref(),
            Some("Terminal (com.apple.Terminal)")
        );
        assert!(context.polish.cursor_context.is_some());

        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = false;
        backend.set_preferences(preferences).unwrap();
        let context = backend
            .capture_host_dictation_context(DictationStartOptions {
                cursor_context: Some("must not survive a disabled privacy switch".into()),
                ..DictationStartOptions::default()
            })
            .await
            .unwrap();
        assert!(context.polish.cursor_context.is_none());
        backend.start().await.unwrap();
        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();
        let history = backend.history.list().unwrap();
        assert_eq!(
            history[0].app_bundle_id.as_deref(),
            cfg!(target_os = "macos").then_some("com.apple.Terminal")
        );
        assert_eq!(
            history[0].app_name.as_deref(),
            Some(if cfg!(target_os = "macos") {
                "Terminal"
            } else {
                "Terminal (com.apple.Terminal)"
            })
        );
        assert_eq!(
            host_context.1.load(Ordering::Acquire),
            1,
            "only the explicitly enabled capture may read a document"
        );
    }

    #[tokio::test]
    async fn realtime_recording_fault_finishes_once_and_rejects_stale_reports() {
        let (backend, _) = backend();
        backend.start().await.unwrap();
        let session_id = backend.start_dictation().await.unwrap();
        let error = BackendError::new(BackendErrorCode::Platform, "device disconnected");
        backend
            .engine_progress_sink()
            .publish(session_id, EngineProgress::RecordingFault(error.clone()))
            .unwrap();
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Failed);

        backend
            .report_recording_fault(session_id, error.clone())
            .await
            .unwrap();
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].error_code.as_deref(), Some("recordingFailed"));
        // 更早失败的记录也带 NotCalled 快照（无 prompt，绝不事后拼）。
        let diagnostics = history[0]
            .polish_diagnostics
            .as_ref()
            .expect("early failure carries not-called diagnostics");
        assert_eq!(
            diagnostics.stage,
            crate::polish_diagnostics::PolishDiagnosticsStage::NotCalled
        );
        assert_eq!(
            diagnostics.outcome,
            crate::polish_diagnostics::PolishDiagnosticsOutcome::NotCalled
        );
        assert!(diagnostics.prompt.is_none());
        assert!(diagnostics.not_called_reason.is_some());
        assert_eq!(
            backend
                .report_recording_fault(session_id, error)
                .await
                .unwrap_err()
                .code,
            BackendErrorCode::InvalidState
        );
        assert_eq!(backend.list_history().unwrap().len(), 1);
    }

    /// 成功记录原样内嵌引擎诊断（含 prompt）；Raw 直通（builtin.raw 默认包）没有
    /// 引擎快照时补 NotCalled，且 prompt 必须为 None（不事后重拼）。
    #[tokio::test]
    async fn history_embeds_polish_diagnostics_for_success_and_raw_bypass() {
        struct DiagnosticsEngine {
            calls: std::sync::atomic::AtomicUsize,
        }

        impl DictationEngine for DiagnosticsEngine {
            fn start(
                &self,
                _session_id: SessionId,
                _context: Arc<DictationContext>,
                _progress: Arc<dyn EngineProgressSink>,
            ) -> BoxFuture<'static, Result<(), BackendError>> {
                boxed(async { Ok(()) })
            }

            fn finish(
                &self,
                _session_id: SessionId,
                _progress: Arc<dyn EngineProgressSink>,
            ) -> BoxFuture<'static, Result<EngineResult, EngineFailure>> {
                let first_call = self
                    .calls
                    .fetch_add(1, Ordering::SeqCst)
                    == 0;
                boxed(async move {
                    Ok(EngineResult {
                        raw_text: "raw".to_string(),
                        asr_transcript: None,
                        polished_text: "polished".to_string(),
                        polish_source: None,
                        duration_ms: 1000,
                        polish_failed: false,
                        asr_ms: None,
                        polish_ms: None,
                        has_audio_recording: None,
                        asr_call_label: None,
                        llm_call_label: None,
                        // 只在第一次 finish 提供引擎快照；第二次（Raw 直通）返回
                        // None，模拟真实 dictation_engine 在 raw 路径交给历史层补
                        // NotCalled 的行为。
                        polish_diagnostics: first_call.then(|| {
                            crate::polish_diagnostics::PolishDiagnostics {
                            stage: crate::polish_diagnostics::PolishDiagnosticsStage::Succeeded,
                            outcome:
                                crate::polish_diagnostics::PolishDiagnosticsOutcome::Succeeded,
                            provider: Some("local-llama".to_string()),
                            model: Some("gemma-4-E2B-it-Q4_0.gguf".to_string()),
                            template: Some(
                                crate::polish_diagnostics::PolishTemplateKind::Gemma4Canonical,
                            ),
                            add_bos: Some(true),
                            add_bos_note: Some(crate::polish_diagnostics::ADD_BOS_NOTE.to_string()),
                            prompt: Some("<|turn>system\n…<|turn>model\n".to_string()),
                            messages: Some(vec![
                                crate::polish_diagnostics::PolishDiagnosticMessage {
                                    role: "system".to_string(),
                                    content: "SYS".to_string(),
                                },
                                crate::polish_diagnostics::PolishDiagnosticMessage {
                                    role: "user".to_string(),
                                    content: "raw".to_string(),
                                },
                            ]),
                            prompt_tokens: Some(42),
                            context_tokens: Some(16_384),
                            generation_budget: Some(1_024),
                            error: None,
                            not_called_reason: None,
                            cursor: None,
                            screen_ocr: None,
                            }
                        }),
                    })
                })
            }

            fn cancel(
                &self,
                _session_id: SessionId,
            ) -> BoxFuture<'static, Result<(), BackendError>> {
                boxed(async { Ok(()) })
            }
        }

        let data_dir = TestDataDir::new("history-polish-diagnostics");
        let backend = backend_with_dictation_engine(
            data_dir.path().to_path_buf(),
            Arc::new(DiagnosticsEngine {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }),
        );
        backend.start().await.unwrap();
        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        let diagnostics = history[0]
            .polish_diagnostics
            .as_ref()
            .expect("success record embeds the engine snapshot");
        assert_eq!(
            diagnostics.stage,
            crate::polish_diagnostics::PolishDiagnosticsStage::Succeeded
        );
        assert_eq!(
            diagnostics.prompt.as_deref(),
            Some("<|turn>system\n…<|turn>model\n")
        );
        assert_eq!(diagnostics.messages.as_ref().map(|m| m.len()), Some(2));
        assert_eq!(diagnostics.prompt_tokens, Some(42));
        assert_eq!(
            diagnostics.template,
            Some(crate::polish_diagnostics::PolishTemplateKind::Gemma4Canonical)
        );
        assert_eq!(diagnostics.add_bos, Some(true));

        // Raw 直通：builtin.raw 是真 passthrough（不进润色器），引擎没有快照时
        // 历史层补 NotCalled；prompt/messages 保持 None。
        let mut preferences = backend.get_preferences();
        preferences.active_style_pack_id =
            crate::style_packs::BUILTIN_STYLE_PACK_RAW_ID.to_string();
        backend.set_preferences(preferences).unwrap();
        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 2);
        let bypass = history[0]
            .polish_diagnostics
            .as_ref()
            .expect("raw bypass records a not-called snapshot");
        assert_eq!(
            bypass.stage,
            crate::polish_diagnostics::PolishDiagnosticsStage::NotCalled
        );
        assert_eq!(
            bypass.outcome,
            crate::polish_diagnostics::PolishDiagnosticsOutcome::NotCalled
        );
        assert!(bypass.prompt.is_none(), "不事后重拼 prompt");
        assert!(bypass.messages.is_none());
        assert!(bypass
            .not_called_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("raw passthrough")));
    }

    #[tokio::test]
    async fn edit_observation_is_core_gated_and_rejects_stale_reports() {
        let data_dir = TestDataDir::new("edit-observation");
        let observation = Arc::new(FakeEditObservation::default());
        let mut services = crate::domains::BackendServices::unsupported();
        services.edit_observation = observation.clone();
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services,
                ..BackendDependencies::unsupported()
            },
        )
        .unwrap();
        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = true;
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();
        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        assert_eq!(
            observation.typed_texts.lock().unwrap().as_slice(),
            ["polished"]
        );
        let edit = crate::host_document::EditPair {
            source: "polished".into(),
            target: "Polished".into(),
            before: String::new(),
            after: String::new(),
        };
        observation.publish(0, edit.clone());
        assert_eq!(backend.pending_corrections().len(), 1);

        backend.dismiss_pending_corrections();
        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = false;
        backend
            .update_settings(
                preferences,
                crate::SettingsUpdateOptions::STRICT,
                &crate::NoopSettingsRuntime,
            )
            .unwrap();
        observation.publish(0, edit);
        assert!(backend.pending_corrections().is_empty());
    }

    #[tokio::test]
    async fn disabled_cursor_context_is_not_rearmed_by_an_older_dictation() {
        for streaming in [false, true] {
            let data_dir = TestDataDir::new("privacy-disabled-during-dictation");
            let observation = Arc::new(FakeEditObservation::default());
            let mut services = crate::domains::BackendServices::unsupported();
            services.edit_observation = observation.clone();
            let backend = OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                BackendDependencies {
                    host_actions: Arc::new(FakeHost::default()),
                    text_inserter: Arc::new(FakeInserter),
                    dictation_engine: Arc::new(FakeEngine),
                    credential_store: Arc::new(
                        crate::credentials::InMemoryCredentialStore::default(),
                    ),
                    task_spawner: Arc::new(TokioTaskSpawner),
                    services,
                    ..BackendDependencies::unsupported()
                },
            )
            .unwrap();
            let mut preferences = backend.get_preferences();
            preferences.cursor_context_enabled = true;
            preferences.streaming_insert = streaming;
            backend.set_preferences(preferences).unwrap();
            backend.start().await.unwrap();
            backend.start_dictation().await.unwrap();
            let mut preferences = backend.get_preferences();
            preferences.cursor_context_enabled = false;
            backend
                .update_settings(
                    preferences,
                    crate::SettingsUpdateOptions::STRICT,
                    &crate::NoopSettingsRuntime,
                )
                .unwrap();
            backend.stop_dictation().await.unwrap();
            assert!(
                observation.typed_texts.lock().unwrap().is_empty(),
                "a frozen true preference cannot restart native document observation"
            );
        }
    }

    #[tokio::test]
    async fn delayed_cancel_reply_cannot_hide_a_successor_session() {
        let (backend, host) = backend();
        backend.start().await.unwrap();
        let first = backend.start_dictation().await.unwrap();
        let mut cancelling = Box::pin(backend.cancel_dictation(Some(first)));
        assert!(futures_util::poll!(cancelling.as_mut()).is_pending());
        // Run executor-owned native cleanup, but intentionally do not resume
        // its caller. Releasing resources must not authorize a late Hide of B.
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match backend.start_dictation().await {
                    Ok(id) => break id,
                    Err(error) if error.code == BackendErrorCode::Busy => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("unexpected start failure: {error}"),
                }
            }
        })
        .await
        .unwrap();
        cancelling.await.unwrap();
        assert_eq!(backend.snapshot().dictation.session_id, Some(second));
        assert_eq!(
            host.0.lock().unwrap().last(),
            Some(&HostAction::ShowDictationFeedback)
        );
        backend.cancel_dictation(Some(second)).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn delayed_completed_callback_cannot_reset_or_observe_a_new_session() {
        struct BlockingCommit {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        }
        impl crate::SettingsRuntime for BlockingCommit {
            fn prepare(
                &self,
                _: &crate::SettingsEffectPlan,
            ) -> Result<crate::SettingsEffectReceipt, crate::SettingsEffectFailure> {
                Ok(crate::SettingsEffectReceipt::default())
            }
            fn commit(
                &self,
                _: &crate::SettingsEffectPlan,
                _: &mut crate::SettingsEffectReceipt,
            ) -> Result<(), crate::SettingsEffectFailure> {
                self.entered.notify_one();
                let mut released = self.release.0.lock().unwrap();
                while !*released {
                    released = self.release.1.wait(released).unwrap();
                }
                Ok(())
            }
            fn restore(
                &self,
                _: &crate::SettingsEffectPlan,
                _: &crate::SettingsEffectReceipt,
            ) -> Result<(), BackendError> {
                Ok(())
            }
        }
        struct ReleaseCommit(Arc<(Mutex<bool>, std::sync::Condvar)>);
        impl ReleaseCommit {
            fn release(&self) {
                *self
                    .0
                     .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                self.0 .1.notify_all();
            }
        }
        impl Drop for ReleaseCommit {
            fn drop(&mut self) {
                self.release();
            }
        }

        // Settings effects may synchronously wait for native hotkey registration.
        // Leave executor workers available while stop waits on that native
        // gate, so cancellation and a successor can make progress normally.
        let data_dir = TestDataDir::new("completed-callback-owner");
        let observation = Arc::new(FakeEditObservation::default());
        let host = Arc::new(FakeHost::default());
        let mut services = crate::domains::BackendServices::unsupported();
        services.edit_observation = observation.clone();
        let backend = Arc::new(
            OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                BackendDependencies {
                    host_actions: host.clone(),
                    text_inserter: Arc::new(FakeInserter),
                    dictation_engine: Arc::new(FakeEngine),
                    credential_store: Arc::new(
                        crate::credentials::InMemoryCredentialStore::default(),
                    ),
                    task_spawner: Arc::new(TokioTaskSpawner),
                    services,
                    ..BackendDependencies::unsupported()
                },
            )
            .unwrap(),
        );
        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = true;
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();
        let first = backend.start_dictation().await.unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let release_guard = ReleaseCommit(release.clone());
        let settings_backend = backend.clone();
        let settings = std::thread::spawn({
            let entered = entered.clone();
            move || {
                let mut preferences = settings_backend.get_preferences();
                preferences.microphone_device_name = "pending settings microphone".into();
                settings_backend.update_settings(
                    preferences,
                    crate::SettingsUpdateOptions::STRICT,
                    &BlockingCommit { entered, release },
                )
            }
        });
        entered.notified().await;
        let mut events = backend.subscribe();
        let stopping_backend = backend.clone();
        let stopping = tokio::spawn(async move { stopping_backend.stop_dictation().await });
        loop {
            let event = events.recv().await.unwrap();
            if event.session_id == Some(first)
                && matches!(
                    event.kind,
                    BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                        phase: DictationPhase::Completed,
                        ..
                    })
                )
            {
                break;
            }
        }
        backend.cancel_dictation(Some(first)).await.unwrap();
        let second = backend.start_dictation().await.unwrap();
        release_guard.release();
        settings.join().unwrap().unwrap();
        stopping.await.unwrap().unwrap();
        assert_eq!(backend.snapshot().dictation.session_id, Some(second));
        assert_eq!(
            backend.snapshot().dictation.phase,
            DictationPhase::Recording
        );
        assert!(
            observation.typed_texts.lock().unwrap().is_empty(),
            "old completion cannot arm observation for the successor"
        );
        assert_eq!(
            host.0.lock().unwrap().last(),
            Some(&HostAction::ShowDictationFeedback)
        );
        backend.cancel_dictation(Some(second)).await.unwrap();
    }

    #[tokio::test]
    async fn dictation_captures_preferences_style_and_vocabulary_once_per_session() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-context-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let engine = crate::testing::FixtureDictationEngine::successful("raw", "polished");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: Arc::new(engine.clone()),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();
        let mut preferences = backend.get_preferences();
        preferences.microphone_device_name = "Session microphone".to_string();
        preferences.active_asr_provider = "local-qwen3".to_string();
        preferences.active_llm_provider = "openai-compatible".to_string();
        preferences.local_asr_active_model = "qwen3-asr-1.7b".to_string();
        preferences.translation_target_language = "English".to_string();
        preferences.working_languages = vec!["简体中文".to_string()];
        backend.set_preferences(preferences).unwrap();
        backend
            .add_vocabulary("OpenLess".to_string(), None)
            .unwrap();

        backend
            .start_dictation_with_options(DictationStartOptions {
                translation_requested: true,
                style_pack_id: None,
                ..DictationStartOptions::default()
            })
            .await
            .unwrap();
        assert!(backend.snapshot().dictation.translation_active);
        let mut changed = backend.get_preferences();
        changed.microphone_device_name = "Changed microphone".to_string();
        changed.active_asr_provider = "changed-provider".to_string();
        changed.translation_target_language = "日本語".to_string();
        backend.set_preferences(changed).unwrap();
        backend.stop_dictation().await.unwrap();

        let contexts = engine.contexts();
        assert_eq!(contexts.len(), 1);
        let context = &contexts[0];
        assert_eq!(
            context.recording.microphone_device_name.as_deref(),
            Some("Session microphone")
        );
        assert_eq!(context.asr.provider_id, "local-qwen3");
        assert_eq!(context.asr.model.as_deref(), Some("qwen3-asr-1.7b"));
        assert_eq!(context.asr.prompt.as_deref(), Some("OpenLess."));
        assert_eq!(context.polish.translation_target_language, "English");
        assert!(context.polish.translation_active);
        backend.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(data_dir);
    }





    #[tokio::test]
    async fn recording_readiness_waits_for_the_first_pcm_even_when_its_meter_is_zero() {
        let (backend, _) = backend();
        backend.start().await.unwrap();
        let session_id = backend.start_dictation().await.unwrap();
        assert_eq!(
            backend.snapshot().dictation.phase,
            DictationPhase::Recording
        );
        assert_eq!(
            serde_json::to_value(backend.snapshot().dictation).unwrap()["recordingReady"],
            false
        );
        let before = backend.replay_events_after(0).latest_sequence;
        backend
            .engine_progress_sink()
            .publish(
                session_id,
                EngineProgress::RecordingLevel {
                    elapsed_ms: 0,
                    level: 0.0,
                },
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(backend.snapshot().dictation).unwrap()["recordingReady"],
            true
        );
        assert!(
            backend.replay_events_after(0).latest_sequence > before,
            "zero first meter must still publish readiness"
        );
        backend.cancel_dictation(None).await.unwrap();
        backend.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stop_time_context_update_failure_cancels_and_resets_the_session() {
        use crate::testing::FixtureEngineAction;

        let data_dir = std::env::temp_dir().join(format!(
            "openless-stop-time-translation-failure-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let engine = crate::testing::FixtureDictationEngine::failing_context_update(
            "raw",
            "polished",
            BackendError::new(BackendErrorCode::Platform, "fixture context update failure"),
        );
        let backend = backend_with_dictation_engine(data_dir.clone(), Arc::new(engine.clone()));
        backend.start().await.unwrap();
        let mut preferences = backend.get_preferences();
        preferences.translation_target_language = "English".to_string();
        preferences.working_languages = vec!["简体中文".to_string()];
        backend.set_preferences(preferences).unwrap();

        let failed_session = backend.start_dictation().await.unwrap();
        let error = backend
            .stop_dictation_with_options(DictationStopOptions {
                translation_requested: Some(true),
            })
            .await
            .expect_err("context update failure must abort finalization");
        assert_eq!(error.code, BackendErrorCode::Platform);
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        assert_eq!(backend.snapshot().dictation.session_id, None);
        assert_eq!(
            engine.actions(),
            vec![
                FixtureEngineAction::Start(failed_session),
                FixtureEngineAction::UpdateContext(failed_session),
                FixtureEngineAction::Cancel(failed_session),
            ]
        );
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].error_code.as_deref(), Some("polishFailed"));
        assert!(history[0].translation_active);

        backend.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(data_dir);
    }



    #[tokio::test]
    async fn completed_dictation_persists_history_and_activity_from_the_session_snapshot() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-completed-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let engine = crate::testing::FixtureDictationEngine::successful_with_metadata(
            "raw voice",
            "translated output",
            Some("polished source".to_string()),
            1250,
        );
        let fixed_clock = Arc::new(crate::testing::FixedClock::new(
            chrono::DateTime::parse_from_rfc3339("2026-08-28T12:34:56Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            chrono::NaiveDate::from_ymd_opt(2026, 8, 28).unwrap(),
        ));
        let backend = OpenLessBackend::new_with_clock(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
            fixed_clock,
        )
        .unwrap();
        let mut preferences = backend.get_preferences();
        preferences.translation_target_language = "English".to_string();
        preferences.working_languages = vec!["简体中文".to_string()];
        preferences.history_retention_days = 30;
        preferences.history_max_entries = Some(20);
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();

        let session_id = backend
            .start_dictation_with_options(DictationStartOptions {
                translation_requested: true,
                front_app: Some("Visual Studio Code".to_string()),
                ..DictationStartOptions::default()
            })
            .await
            .unwrap();
        let result = backend.stop_dictation().await.unwrap();

        assert_eq!(result.session_id, session_id);
        assert_eq!(result.polish_source.as_deref(), Some("polished source"));
        assert_eq!(result.duration_ms, 1250);
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        let entry = &history[0];
        assert_eq!(entry.id, session_id.to_string());
        assert_eq!(entry.created_at, "2026-08-28T12:34:56+00:00");
        assert_eq!(entry.raw_transcript, "raw voice");
        assert_eq!(entry.final_text, "translated output");
        assert_eq!(entry.polish_source.as_deref(), Some("polished source"));
        assert!(entry.translation_active);
        assert_eq!(entry.duration_ms, Some(1250));
        assert_eq!(
            entry.insert_status,
            crate::types::HistoryInsertStatus::Inserted
        );
        let activity = backend.list_activity().unwrap();
        assert_eq!(activity.len(), 1);
        assert_eq!(activity[0].date, "2026-08-28");
        assert_eq!(
            activity[0].chars,
            "translated output".chars().count() as u64
        );
        assert_eq!(activity[0].duration_ms, 1250);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn completed_dictation_applies_enabled_correction_rules_before_insert_and_history() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-completed-correction-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let inserter = crate::testing::FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(inserter.clone()),
                dictation_engine: Arc::new(crate::testing::FixtureDictationEngine::successful(
                    "10粒样品和禁用词",
                    "10粒样品和禁用词",
                )),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend
            .add_correction_rule("{num}粒".to_string(), "{num}例".to_string())
            .unwrap();
        let disabled = backend
            .add_correction_rule("禁用词".to_string(), "不应出现".to_string())
            .unwrap();
        backend
            .set_correction_rule_enabled(&disabled.id, false)
            .unwrap();
        backend.start().await.unwrap();

        backend.start_dictation().await.unwrap();
        let result = backend.stop_dictation().await.unwrap();

        assert_eq!(result.polished_text, "10例样品和禁用词");
        assert!(inserter.actions().iter().any(|action| matches!(
            action,
            crate::testing::FixtureInsertionAction::Insert { text, .. }
                if text == "10例样品和禁用词"
        )));
        assert_eq!(
            backend.list_history().unwrap()[0].final_text,
            result.polished_text
        );

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn pipeline_passes_corrected_asr_text_to_the_polisher_and_preserves_original_history() {
        let data_dir = TestDataDir::new("correction-before-polish");
        let polisher = Arc::new(crate::testing::FixtureTextPolisher::successful("10例"));
        let engine = crate::PipelineDictationEngine::new(
            Arc::new(crate::testing::FixtureAudioRecorder::new(
                vec![vec![1, 0, 2, 0]],
                vec![],
            )),
            Arc::new(crate::testing::FixtureTranscriptionEngine::successful(
                "10粒", 10,
            )),
            polisher.clone(),
        );
        let backend =
            backend_with_dictation_engine(data_dir.path().to_path_buf(), Arc::new(engine));
        backend
            .add_correction_rule("{num}粒".to_string(), "{num}例".to_string())
            .unwrap();
        backend.start().await.unwrap();

        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        assert_eq!(polisher.inputs(), vec!["10例"]);
        let history = backend.list_history().unwrap();
        assert_eq!(history[0].raw_transcript, "10例");
        assert_eq!(history[0].asr_transcript.as_deref(), Some("10粒"));
    }



    #[tokio::test]
    async fn polish_fallback_persists_the_polish_failed_history_code() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-polish-fallback-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let engine = crate::dictation_engine::PipelineDictationEngine::new(
            Arc::new(
                crate::testing::FixtureAudioRecorder::new(vec![vec![1, 0, 2, 0]], vec![(25, 0.5)])
                    .with_archived_recording(true),
            ),
            Arc::new(crate::testing::FixtureTranscriptionEngine::successful(
                "raw fallback",
                25,
            )),
            Arc::new(crate::testing::FixtureTextPolisher::failing(
                BackendError::new(BackendErrorCode::Provider, "fixture polish failure"),
            )),
        );
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        backend.start_dictation().await.unwrap();
        let result = backend.stop_dictation().await.unwrap();

        assert_eq!(result.polished_text, "raw fallback");
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].error_code.as_deref(), Some("polishFailed"));
        assert!(history[0].asr_ms.is_some());
        assert!(history[0].polish_ms.is_some());
        assert_eq!(history[0].has_audio_recording, Some(false));

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn empty_transcript_persists_a_failed_history_entry_without_activity() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-empty-transcript-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let engine = crate::dictation_engine::PipelineDictationEngine::new(
            Arc::new(crate::testing::FixtureAudioRecorder::new(
                vec![vec![1, 0, 2, 0]],
                vec![(40, 0.25)],
            )),
            Arc::new(crate::testing::FixtureTranscriptionEngine::successful(
                "   ", 40,
            )),
            Arc::new(crate::testing::FixtureTextPolisher::successful(
                "must not be inserted",
            )),
        );
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        let session_id = backend.start_dictation().await.unwrap();
        let error = backend
            .stop_dictation()
            .await
            .expect_err("empty transcript must fail");

        assert_eq!(error.code, BackendErrorCode::Provider);
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, session_id.to_string());
        assert_eq!(history[0].error_code.as_deref(), Some("emptyTranscript"));
        assert_eq!(
            history[0].insert_status,
            crate::types::HistoryInsertStatus::Failed
        );
        assert!(backend.list_activity().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn asr_finish_failure_persists_history_and_releases_the_session() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-asr-failure-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: Arc::new(crate::testing::FixtureDictationEngine::failing(
                    BackendError::new(BackendErrorCode::Provider, "fixture ASR failure"),
                )),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        let session_id = backend.start_dictation().await.unwrap();
        let error = backend.stop_dictation().await.expect_err("ASR must fail");

        assert_eq!(error.code, BackendErrorCode::Provider);
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, session_id.to_string());
        assert_eq!(history[0].error_code.as_deref(), Some("transcribeFailed"));
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        assert!(backend.start_dictation().await.is_ok());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn raw_style_runs_through_the_real_pipeline_without_a_polishing_stage() {
        let data_dir = TestDataDir::new("raw-real-pipeline");
        let polisher = crate::testing::FixtureTextPolisher::successful("must not run");
        let recorder = Arc::new(crate::testing::FixtureAudioRecorder::default());
        let engine = Arc::new(crate::PipelineDictationEngine::new(
            recorder.clone(),
            Arc::new(crate::testing::FixtureTranscriptionEngine::successful(
                "raw words",
                80,
            )),
            Arc::new(polisher.clone()),
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..Default::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: engine,
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                ..BackendDependencies::unsupported()
            },
        )
        .unwrap();
        backend.activate_style_pack("builtin.raw").unwrap();
        backend.start().await.unwrap();
        // Exercise the production backend progress validator, not the loose
        // sink used by engine-only tests; Raw never enters the LLM stage.
        for _ in 0..2 {
            backend.start_dictation().await.unwrap();
            let result = backend
                .stop_dictation()
                .await
                .expect("Raw ASR must remain usable");
            assert_eq!(result.raw_text, "raw words");
            assert_eq!(result.polished_text, "raw words");
            assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        }
        assert!(polisher.inputs().is_empty(), "Raw must not call the LLM");
        assert_eq!(recorder.stop_count(), 2);
        assert!(backend
            .list_history()
            .unwrap()
            .iter()
            .all(|entry| entry.error_code.is_none()));
        backend.shutdown().await.unwrap();
    }



    #[tokio::test]
    async fn pipeline_asr_failure_preserves_archive_and_timing_diagnostics() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-asr-diagnostics-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let engine = crate::dictation_engine::PipelineDictationEngine::new(
            Arc::new(
                crate::testing::FixtureAudioRecorder::new(vec![vec![1, 0, 2, 0]], vec![(80, 0.5)])
                    .with_archived_recording(true),
            ),
            Arc::new(crate::testing::FixtureTranscriptionEngine::failing(
                BackendError::new(BackendErrorCode::Provider, "fixture ASR failure"),
            )),
            Arc::new(crate::testing::FixtureTextPolisher::successful("unused")),
        );
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                    InsertOutcome::Inserted,
                )),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.expect_err("ASR must fail");

        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].error_code.as_deref(), Some("transcribeFailed"));
        assert_eq!(history[0].has_audio_recording, Some(true));
        assert!(history[0].asr_ms.is_some());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn engine_start_failure_persists_history_and_releases_the_session() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-start-failure-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let inserter = crate::testing::FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(inserter.clone()),
                dictation_engine: Arc::new(StartFailingEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        let error = backend
            .start_dictation()
            .await
            .expect_err("engine start must fail");

        assert_eq!(error.code, BackendErrorCode::Platform);
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].error_code.as_deref(), Some("transcribeFailed"));
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        let actions = inserter.actions();
        assert_eq!(actions.len(), 2);
        let prepared_session = match &actions[0] {
            crate::testing::FixtureInsertionAction::Prepare(session_id) => *session_id,
            action => panic!("unexpected first insertion action: {action:?}"),
        };
        assert_eq!(
            actions[1],
            crate::testing::FixtureInsertionAction::Cancel(prepared_session)
        );
        assert!(backend.start_dictation().await.is_err());
        assert_eq!(backend.list_history().unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn insertion_failure_persists_generated_text_and_releases_the_session() {
        let data_dir = std::env::temp_dir().join(format!(
            "openless-core-insert-failure-history-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.clone(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(crate::testing::FixtureTextInserter::failing(
                    BackendError::new(BackendErrorCode::Platform, "fixture insertion failure"),
                )),
                dictation_engine: Arc::new(
                    crate::testing::FixtureDictationEngine::successful_with_metadata(
                        "raw voice",
                        "generated text",
                        Some("polished source".to_string()),
                        600,
                    ),
                ),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        let session_id = backend.start_dictation().await.unwrap();
        let error = backend
            .stop_dictation()
            .await
            .expect_err("insertion must fail");

        assert_eq!(error.code, BackendErrorCode::Platform);
        let history = backend.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, session_id.to_string());
        assert_eq!(history[0].raw_transcript, "raw voice");
        assert_eq!(history[0].final_text, "generated text");
        assert_eq!(history[0].polish_source.as_deref(), Some("polished source"));
        assert_eq!(history[0].error_code.as_deref(), Some("insertFailed"));
        assert_eq!(
            history[0].insert_status,
            crate::types::HistoryInsertStatus::Failed
        );
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        assert!(backend.list_activity().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn dictation_runs_through_engine_inserter_and_host_actions() {
        let (backend, host) = backend();
        let mut events = backend.subscribe();
        backend.start().await.unwrap();
        let session = backend.start_dictation().await.unwrap();
        assert_eq!(
            backend.stop_dictation().await.unwrap().polished_text,
            "polished"
        );
        let mut emitted = Vec::new();
        while let Ok(event) = events.try_recv() {
            emitted.push(event);
        }
        assert!(matches!(emitted[0].kind, BackendEventKind::BackendStarted));
        assert_eq!(
            emitted
                .iter()
                .filter(|event| event.session_id == Some(session))
                .filter_map(|event| match &event.kind {
                    BackendEventKind::DictationStateChanged(snapshot) => Some(snapshot.phase),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                DictationPhase::Starting,
                DictationPhase::Recording,
                DictationPhase::Transcribing,
                DictationPhase::Inserting,
                DictationPhase::Completed,
            ]
        );
        assert!(emitted.iter().any(|event| {
            event.session_id == Some(session)
                && matches!(event.kind, BackendEventKind::DictationCompleted(_))
        }));
        let actions = host.0.lock().unwrap();
        assert_eq!(
            actions.as_slice(),
            &[
                HostAction::ShowDictationFeedback,
                HostAction::HideDictationFeedback
            ]
        );
    }

    #[tokio::test]
    async fn streaming_polish_deltas_flush_before_the_final_insert() {
        use crate::testing::{FixtureDictationEngine, FixtureInsertionAction, FixtureTextInserter};

        let data_dir = TestDataDir::new("streaming-insert");
        let engine = FixtureDictationEngine::successful("raw", "你好").with_polish_deltas(vec![
            crate::types::PolishDelta {
                text: "你".into(),
                offset: 0,
                is_final: false,
            },
            crate::types::PolishDelta {
                text: "好".into(),
                offset: 1,
                is_final: false,
            },
        ]);
        let inserter = FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(inserter.clone()),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut preferences = backend.get_preferences();
        preferences.streaming_insert = true;
        preferences.streaming_insert_save_clipboard = false;
        preferences.windows_insertion_mode = crate::shared_types::WindowsInsertionMode::SendInput;
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();
        let session_id = backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        assert_eq!(
            inserter.actions(),
            vec![
                FixtureInsertionAction::Prepare(session_id),
                FixtureInsertionAction::Write {
                    session_id,
                    text: "你好".into(),
                },
                FixtureInsertionAction::Insert {
                    session_id,
                    text: String::new(),
                },
            ]
        );
    }

    async fn assert_cancel_drains_native_insertion(streaming: bool, drop_stop: bool) {
        struct BlockingInsertion {
            actions: Arc<Mutex<Vec<&'static str>>>,
            started: Arc<tokio::sync::Semaphore>,
            release: Arc<tokio::sync::Semaphore>,
        }
        impl TextInsertionSession for BlockingInsertion {
            fn write(
                &self,
                text: String,
            ) -> BoxFuture<'static, Result<InsertWriteResult, BackendError>> {
                let actions = Arc::clone(&self.actions);
                let started = Arc::clone(&self.started);
                let release = Arc::clone(&self.release);
                boxed(async move {
                    actions.lock().unwrap().push("write started");
                    started.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    actions.lock().unwrap().push("write finished");
                    Ok(InsertWriteResult {
                        written_chars: text.chars().count(),
                    })
                })
            }
            fn copy(&self, _: String) -> BoxFuture<'static, Result<(), BackendError>> {
                boxed(async { Ok(()) })
            }
            fn finish(
                &self,
                text: String,
            ) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
                let writing = self.write(text);
                let actions = Arc::clone(&self.actions);
                boxed(async move {
                    writing.await?;
                    actions.lock().unwrap().push("input source restored");
                    Ok(InsertOutcome::Inserted)
                })
            }
            fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
                self.actions.lock().unwrap().push("input source restored");
                boxed(async { Ok(()) })
            }
        }
        struct BlockingInserter(Arc<BlockingInsertion>);
        impl TextInserter for BlockingInserter {
            fn begin(
                &self,
                _: SessionId,
                _: Arc<DictationContext>,
            ) -> BoxFuture<'static, Result<Arc<dyn TextInsertionSession>, BackendError>>
            {
                let insertion: Arc<dyn TextInsertionSession> = self.0.clone();
                boxed(async move { Ok(insertion) })
            }
        }

        let actions = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let data_dir = TestDataDir::new("stream-cancel-drain");
        let mut deps = BackendDependencies::unsupported();
        deps.credential_store = Arc::new(crate::credentials::InMemoryCredentialStore::default());
        deps.text_inserter = Arc::new(BlockingInserter(Arc::new(BlockingInsertion {
            actions: Arc::clone(&actions),
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        })));
        deps.dictation_engine = Arc::new(
            crate::testing::FixtureDictationEngine::successful("raw", "streamed")
                .with_polish_deltas(if streaming {
                    vec![crate::types::PolishDelta {
                        text: "streamed".into(),
                        offset: 0,
                        is_final: false,
                    }]
                } else {
                    Vec::new()
                }),
        );
        deps.task_spawner = Arc::new(TokioTaskSpawner);
        let backend = Arc::new(
            OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                deps,
            )
            .unwrap(),
        );
        let mut preferences = backend.get_preferences();
        preferences.streaming_insert = streaming;
        preferences.windows_insertion_mode = crate::shared_types::WindowsInsertionMode::SendInput;
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();
        let session_id = backend.start_dictation().await.unwrap();
        let stopping_backend = Arc::clone(&backend);
        let stop = tokio::spawn(async move { stopping_backend.stop_dictation().await });
        started.acquire().await.unwrap().forget();
        if drop_stop {
            stop.abort();
            tokio::task::yield_now().await;
        }

        // The write represents a native, non-interruptible CGEvent/SendInput
        // chunk. Cancelling may discard queued text but must not restore TIS or
        // admit another voice session before this in-flight effect has drained.
        let mut cancel = std::pin::pin!(backend.cancel_dictation(Some(session_id)));
        assert!(futures_util::poll!(cancel.as_mut()).is_pending());
        assert_eq!(*actions.lock().unwrap(), vec!["write started"]);
        assert_eq!(
            backend.start_dictation().await.unwrap_err().code,
            BackendErrorCode::Busy
        );
        release.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(2), cancel)
            .await
            .expect("native cleanup must outlive a dropped stop caller")
            .unwrap();
        if drop_stop {
            assert!(stop.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(
                stop.await.unwrap().unwrap_err().code,
                BackendErrorCode::Cancelled
            );
        }
        assert_eq!(
            *actions.lock().unwrap(),
            vec!["write started", "write finished", "input source restored"]
        );
        let next = backend.start_dictation().await.unwrap();
        backend.cancel_dictation(Some(next)).await.unwrap();
    }

    #[tokio::test]
    async fn streaming_cancel_drains_native_write_before_restoring_and_releasing_voice() {
        assert_cancel_drains_native_insertion(true, false).await;
    }

    #[tokio::test]
    async fn final_insert_cancel_waits_for_the_committed_native_effect() {
        assert_cancel_drains_native_insertion(false, false).await;
    }

    #[tokio::test]
    async fn final_insert_cancellation_survives_a_dropped_stop_caller() {
        assert_cancel_drains_native_insertion(false, true).await;
    }

    #[tokio::test]
    async fn streaming_final_writes_only_the_missing_tail() {
        use crate::testing::{FixtureDictationEngine, FixtureInsertionAction, FixtureTextInserter};

        let data_dir = TestDataDir::new("streaming-final-tail");
        let engine = FixtureDictationEngine::successful("raw", "你好").with_polish_deltas(vec![
            crate::types::PolishDelta {
                text: "你".into(),
                offset: 0,
                is_final: false,
            },
        ]);
        let inserter = FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(inserter.clone()),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut preferences = backend.get_preferences();
        preferences.streaming_insert = true;
        preferences.streaming_insert_save_clipboard = false;
        preferences.windows_insertion_mode = crate::shared_types::WindowsInsertionMode::SendInput;
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();
        let session_id = backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        assert_eq!(
            inserter.actions(),
            vec![
                FixtureInsertionAction::Prepare(session_id),
                FixtureInsertionAction::Write {
                    session_id,
                    text: "你".into(),
                },
                FixtureInsertionAction::Write {
                    session_id,
                    text: "好".into(),
                },
                FixtureInsertionAction::Insert {
                    session_id,
                    text: String::new(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn native_streaming_unavailable_keeps_one_final_delivery() {
        struct FinalOnlySession(Arc<Mutex<Vec<String>>>);
        impl TextInsertionSession for FinalOnlySession {
            fn supports_streaming(&self) -> bool {
                false
            }
            fn write(
                &self,
                _: String,
            ) -> BoxFuture<'static, Result<InsertWriteResult, BackendError>> {
                panic!("an unavailable native stream must never receive a chunk");
            }
            fn copy(&self, _: String) -> BoxFuture<'static, Result<(), BackendError>> {
                panic!("native preparation failure still allows final insertion");
            }
            fn finish(
                &self,
                text: String,
            ) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
                self.0.lock().unwrap().push(text);
                boxed(async { Ok(InsertOutcome::Inserted) })
            }
            fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
                boxed(async { Ok(()) })
            }
        }
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let mut context = DictationContext::default();
        context.insertion.streaming = true;
        context.insertion.windows_insertion_mode =
            crate::shared_types::WindowsInsertionMode::SendInput;
        let gate = Arc::new(crate::voice_session::VoiceSessionGate::default());
        let session_id = SessionId::new();
        gate.acquire(
            session_id,
            crate::voice_session::VoiceSessionKind::Dictation,
        )
        .unwrap();
        let insertion = ActiveTextInsertion::new(
            Arc::new(FinalOnlySession(delivered.clone())),
            &context,
            Arc::new(TokioTaskSpawner),
            gate.hold_resources(session_id).unwrap(),
        );
        insertion.push(&crate::types::PolishDelta {
            offset: 0,
            text: "partial".into(),
            is_final: false,
        });
        assert_eq!(
            insertion.finish("final text".into()).await.unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(*delivered.lock().unwrap(), ["final text"]);
    }

    #[tokio::test]
    async fn context_capture_is_already_a_cancellable_dictation_session() {
        struct SlowContext {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        impl HostContextAdapter for SlowContext {
            fn capture(
                &self,
                include_cursor: bool,
            ) -> BoxFuture<'static, Result<HostContextCapture, BackendError>> {
                let entered = self.entered.clone();
                let release = self.release.clone();
                boxed(async move {
                    if include_cursor {
                        entered.notify_one();
                        release.notified().await;
                    }
                    Ok(HostContextCapture::default())
                })
            }
        }
        let data_dir = TestDataDir::new("cancel-context-capture");
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut services = crate::domains::BackendServices::unsupported();
        services.host_context = Arc::new(SlowContext {
            entered: entered.clone(),
            release: release.clone(),
        });
        let backend = Arc::new(
            OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                BackendDependencies {
                    host_actions: Arc::new(FakeHost::default()),
                    text_inserter: Arc::new(FakeInserter),
                    dictation_engine: Arc::new(FakeEngine),
                    services,
                    ..BackendDependencies::unsupported()
                },
            )
            .unwrap(),
        );
        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = true;
        backend.set_preferences(preferences).unwrap();
        backend.start().await.unwrap();
        let start = tokio::spawn({
            let backend = backend.clone();
            async move { backend.start_dictation().await }
        });
        entered.notified().await;
        let cancelled = match backend.snapshot().dictation.session_id {
            Some(session_id) => backend.cancel_dictation(Some(session_id)).await.map(|_| ()),
            None => Ok(()),
        };
        release.notify_one();
        let result = start.await.unwrap();
        assert!(
            cancelled.is_ok(),
            "Esc must own the session even while AX/credentials are being captured"
        );
        assert_eq!(result.unwrap_err().code, BackendErrorCode::Cancelled);
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
        let mut preferences = backend.get_preferences();
        preferences.cursor_context_enabled = false;
        backend.set_preferences(preferences).unwrap();
        backend.start_dictation().await.unwrap();
        backend.cancel_dictation(None).await.unwrap();
    }

    #[tokio::test]
    async fn cli_dictation_intents_use_the_same_core_state_machine() {
        let (backend, _) = backend();
        backend.start().await.unwrap();

        let started = backend
            .dispatch_cli_intent(crate::cli::CliIntent::ToggleDictation)
            .await
            .unwrap();
        let session_id = match started {
            CliDispatchOutcome::DictationStarted(session_id) => session_id,
            other => panic!("unexpected start outcome: {other:?}"),
        };
        assert_eq!(
            backend.snapshot().dictation.phase,
            DictationPhase::Recording
        );

        let completed = backend
            .dispatch_cli_intent(crate::cli::CliIntent::ToggleDictation)
            .await
            .unwrap();
        assert!(matches!(
            completed,
            CliDispatchOutcome::DictationCompleted(DictationResult {
                session_id: completed_session,
                ..
            }) if completed_session == session_id
        ));
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);

        assert_eq!(
            backend
                .dispatch_cli_intent(crate::cli::CliIntent::CancelDictation)
                .await
                .unwrap(),
            CliDispatchOutcome::Noop
        );
    }

    #[tokio::test]
    async fn cancelling_wrong_session_is_rejected_without_mutating_state() {
        let (backend, _) = backend();
        backend.start().await.unwrap();
        let active = backend.start_dictation().await.unwrap();
        let wrong = SessionId::new();
        let error = backend.cancel_dictation(Some(wrong)).await.unwrap_err();
        assert_eq!(error.code, BackendErrorCode::InvalidArgument);
        assert_eq!(backend.snapshot().dictation.session_id, Some(active));
    }

    #[tokio::test]
    async fn engine_failure_publishes_failed_state_and_preserves_session_identity() {
        let host = Arc::new(FakeHost::default());
        let data_dir = TestDataDir::new("engine-failure");
        let engine = Arc::new(FailingEngine(std::sync::atomic::AtomicUsize::new(0)));
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: host,
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: engine.clone(),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();

        let mut events = backend.subscribe();
        backend.start().await.unwrap();
        let session = backend.start_dictation().await.unwrap();
        let error = backend.stop_dictation().await.unwrap_err();

        assert_eq!(error.code, BackendErrorCode::Provider);
        let snapshot = backend.snapshot();
        assert_eq!(snapshot.dictation.session_id, None);
        assert_eq!(snapshot.dictation.phase, DictationPhase::Idle);
        let mut failed_session = None;
        while let Ok(event) = events.try_recv() {
            if let BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                session_id,
                phase: DictationPhase::Failed,
                ..
            }) = event.kind
            {
                failed_session = session_id;
            }
        }
        assert_eq!(failed_session, Some(session));
        assert_eq!(
            engine.0.load(Ordering::Acquire),
            1,
            "finish errors must cancel remaining engine resources"
        );
    }

    #[tokio::test]
    async fn stop_is_rejected_after_the_session_has_reached_idle() {
        let (backend, _) = backend();
        backend.start().await.unwrap();
        backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        let error = backend.stop_dictation().await.unwrap_err();
        assert_eq!(error.code, BackendErrorCode::InvalidState);
    }

    #[tokio::test]
    async fn clipboard_fallback_is_an_explicit_event() {
        for outcome in [InsertOutcome::CopiedFallback] {
            let host = Arc::new(FakeHost::default());
            let data_dir = TestDataDir::new("insert-outcome");
            let backend = OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                BackendDependencies {
                    host_actions: host,
                    text_inserter: Arc::new(crate::testing::FixtureTextInserter::with_outcome(
                        outcome,
                    )),
                    dictation_engine: Arc::new(FakeEngine),
                    task_spawner: Arc::new(TokioTaskSpawner),
                    credential_store: Arc::new(
                        crate::credentials::InMemoryCredentialStore::default(),
                    ),
                    services: crate::domains::BackendServices::unsupported(),
                    local_asr_runtime: None,
                    selection_runtime: None,
                    selection_polisher: None,
                },
            )
            .unwrap();
            let mut events = backend.subscribe();
            backend.start().await.unwrap();
            backend.start_dictation().await.unwrap();
            let result = backend.stop_dictation().await.unwrap();
            assert_eq!(result.inserted, outcome.into_status());

            let mut fallback_payload = None;
            loop {
                match events.try_recv() {
                    Ok(event) => {
                        if let BackendEventKind::InsertFallback(payload) = event.kind {
                            fallback_payload = Some(payload);
                        }
                    }
                    Err(crate::events::EventRecvError::Empty) => break,
                    Err(error) => panic!("unexpected event error: {error}"),
                }
            }
            assert_eq!(
                fallback_payload
                    .expect("fallback outcome must be visible to both hosts")
                    .copied_text
                    .as_deref(),
                Some("polished")
            );
        }
    }

    #[tokio::test]
    async fn cancellation_emits_cancelled_state_and_clears_the_session() {
        let (backend, host) = backend();
        let mut events = backend.subscribe();
        backend.start().await.unwrap();
        let session = backend.start_dictation().await.unwrap();
        backend.cancel_dictation(Some(session)).await.unwrap();

        assert_eq!(
            backend.snapshot().dictation,
            DictationStateSnapshot::default()
        );
        assert_eq!(
            *host.0.lock().unwrap(),
            vec![
                HostAction::ShowDictationFeedback,
                HostAction::HideDictationFeedback
            ]
        );
        let mut saw_cancelled = false;
        while let Ok(event) = events.try_recv() {
            if matches!(
                event.kind,
                BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                    phase: DictationPhase::Cancelled,
                    ..
                })
            ) {
                saw_cancelled = true;
            }
        }
        assert!(saw_cancelled);
        assert_eq!(
            backend.stop_dictation().await.unwrap_err().code,
            BackendErrorCode::InvalidState
        );
    }

    #[tokio::test]
    async fn engine_receives_start_finish_and_cancel_lifecycle_calls() {
        use crate::testing::{
            FixtureDictationEngine, FixtureEngineAction, FixtureInsertionAction,
            FixtureTextInserter,
        };

        let completed_engine = FixtureDictationEngine::successful("raw", "polished");
        let completed_inserter = FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let completed_data_dir = TestDataDir::new("engine-lifecycle-completed");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: completed_data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(completed_inserter.clone()),
                dictation_engine: Arc::new(completed_engine.clone()),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();
        let completed = backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();
        assert_eq!(
            completed_engine.actions(),
            vec![
                FixtureEngineAction::Start(completed),
                FixtureEngineAction::Finish(completed),
            ]
        );
        assert_eq!(
            completed_inserter.actions(),
            vec![
                FixtureInsertionAction::Prepare(completed),
                FixtureInsertionAction::Insert {
                    session_id: completed,
                    text: "polished".to_string(),
                },
            ]
        );

        let cancelled_engine = FixtureDictationEngine::successful("raw", "polished");
        let cancelled_inserter = FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let cancelled_data_dir = TestDataDir::new("engine-lifecycle-cancelled");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: cancelled_data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(cancelled_inserter.clone()),
                dictation_engine: Arc::new(cancelled_engine.clone()),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();
        let cancelled = backend.start_dictation().await.unwrap();
        backend.cancel_dictation(Some(cancelled)).await.unwrap();
        assert_eq!(
            cancelled_engine.actions(),
            vec![
                FixtureEngineAction::Start(cancelled),
                FixtureEngineAction::Cancel(cancelled),
            ]
        );
        assert_eq!(
            cancelled_inserter.actions(),
            vec![
                FixtureInsertionAction::Prepare(cancelled),
                FixtureInsertionAction::Cancel(cancelled),
            ]
        );

        let shutdown_engine = FixtureDictationEngine::successful("raw", "polished");
        let shutdown_inserter = FixtureTextInserter::with_outcome(InsertOutcome::Inserted);
        let shutdown_data_dir = TestDataDir::new("engine-lifecycle-shutdown");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: shutdown_data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(shutdown_inserter.clone()),
                dictation_engine: Arc::new(shutdown_engine.clone()),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();
        let interrupted = backend.start_dictation().await.unwrap();
        backend.shutdown().await.unwrap();
        assert_eq!(
            shutdown_engine.actions(),
            vec![
                FixtureEngineAction::Start(interrupted),
                FixtureEngineAction::Cancel(interrupted),
            ]
        );
        assert_eq!(
            shutdown_inserter.actions(),
            vec![
                FixtureInsertionAction::Prepare(interrupted),
                FixtureInsertionAction::Cancel(interrupted),
            ]
        );
    }

    #[tokio::test]
    async fn engine_progress_is_session_scoped_and_orders_stage_delta_and_terminal_events() {
        let engine = crate::testing::FixtureDictationEngine::successful("raw", "polished");
        let data_dir = TestDataDir::new("engine-progress");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(engine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        let mut events = backend.subscribe();
        backend.start().await.unwrap();
        let session = backend.start_dictation().await.unwrap();
        backend.stop_dictation().await.unwrap();

        let mut session_events = Vec::new();
        while let Ok(event) = events.try_recv() {
            if event.session_id == Some(session) {
                session_events.push(event);
            }
        }
        assert!(session_events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence));
        assert!(matches!(
            session_events[0].kind,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Starting,
                ..
            })
        ));
        assert!(session_events.iter().any(|event| matches!(
            event.kind,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Recording,
                ..
            })
        )));
        assert!(session_events.iter().any(|event| matches!(
            event.kind,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Transcribing,
                ..
            })
        )));
        assert!(session_events
            .iter()
            .any(|event| matches!(event.kind, BackendEventKind::TranscriptDelta(_))));
        assert!(session_events.iter().any(|event| matches!(
            event.kind,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Polishing,
                ..
            })
        )));
        assert!(session_events
            .iter()
            .any(|event| matches!(event.kind, BackendEventKind::PolishDelta(_))));
        assert!(session_events.iter().any(|event| matches!(
            event.kind,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Inserting,
                ..
            })
        )));
        assert!(matches!(
            session_events[session_events.len() - 2].kind,
            BackendEventKind::DictationCompleted(_)
        ));
        assert!(matches!(
            session_events.last().unwrap().kind,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Completed,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn late_engine_progress_is_rejected_after_session_cancellation() {
        let (backend, _) = backend();
        backend.start().await.unwrap();
        let session = backend.start_dictation().await.unwrap();
        let progress = BackendEngineProgress {
            events: Arc::clone(&backend.events),
            state: Arc::clone(&backend.state),
            phase_changed: Arc::clone(&backend.phase_changed),
            text_insertions: Arc::clone(&backend.text_insertions),
        };
        backend.cancel_dictation(Some(session)).await.unwrap();

        let error = progress
            .publish(
                session,
                EngineProgress::TranscriptDelta(crate::types::TranscriptDelta {
                    text: "late".to_string(),
                    offset: 0,
                    is_final: false,
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, BackendErrorCode::Cancelled);
    }

    #[tokio::test]
    async fn stop_requested_while_engine_starts_waits_and_finishes_same_session() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let data_dir = TestDataDir::new("stop-during-start");
        let backend = Arc::new(
            OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                BackendDependencies {
                    host_actions: Arc::new(FakeHost::default()),
                    text_inserter: Arc::new(FakeInserter),
                    dictation_engine: Arc::new(BlockingStartEngine {
                        entered: Arc::clone(&entered),
                        release: Arc::clone(&release),
                    }),
                    task_spawner: Arc::new(TokioTaskSpawner),
                    credential_store: Arc::new(
                        crate::credentials::InMemoryCredentialStore::default(),
                    ),
                    services: crate::domains::BackendServices::unsupported(),
                    local_asr_runtime: None,
                    selection_runtime: None,
                    selection_polisher: None,
                },
            )
            .unwrap(),
        );
        backend.start().await.unwrap();
        let starting_backend = Arc::clone(&backend);
        let start_task = tokio::spawn(async move { starting_backend.start_dictation().await });
        entered.notified().await;
        let expected_session = backend.snapshot().dictation.session_id.unwrap();

        let stopping_backend = Arc::clone(&backend);
        let stop_task = tokio::spawn(async move { stopping_backend.stop_dictation().await });
        tokio::task::yield_now().await;
        assert!(!stop_task.is_finished());

        release.notify_one();
        assert_eq!(start_task.await.unwrap().unwrap(), expected_session);
        let result = stop_task.await.unwrap().unwrap();
        assert_eq!(result.session_id, expected_session);
        assert_eq!(result.polished_text, "polished");
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
    }

    async fn assert_pending_insertion_cleanup(drop_start: bool) {
        struct DelayedSession {
            restoring: Arc<tokio::sync::Semaphore>,
            restored: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl TextInsertionSession for DelayedSession {
            fn write(
                &self,
                _: String,
            ) -> BoxFuture<'static, Result<InsertWriteResult, BackendError>> {
                boxed(async { unreachable!() })
            }
            fn finish(&self, _: String) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
                boxed(async { unreachable!() })
            }
            fn copy(&self, _: String) -> BoxFuture<'static, Result<(), BackendError>> {
                boxed(async { unreachable!() })
            }
            fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
                let restoring = self.restoring.clone();
                let restored = self.restored.clone();
                boxed(async move {
                    restoring.acquire().await.unwrap().forget();
                    restored.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
            }
        }
        struct DelayedInserter {
            preparing: Arc<tokio::sync::Semaphore>,
            session: Arc<DelayedSession>,
        }
        impl TextInserter for DelayedInserter {
            fn begin(
                &self,
                _: SessionId,
                _: Arc<DictationContext>,
            ) -> BoxFuture<'static, Result<Arc<dyn TextInsertionSession>, BackendError>>
            {
                let preparing = self.preparing.clone();
                let session: Arc<dyn TextInsertionSession> = self.session.clone();
                boxed(async move {
                    preparing.acquire().await.unwrap().forget();
                    Ok(session)
                })
            }
        }
        let preparing = Arc::new(tokio::sync::Semaphore::new(0));
        let restoring = Arc::new(tokio::sync::Semaphore::new(0));
        let restored = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let data_dir = TestDataDir::new("cancel-during-insertion-prepare");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(DelayedInserter {
                    preparing: preparing.clone(),
                    session: Arc::new(DelayedSession {
                        restoring: restoring.clone(),
                        restored: restored.clone(),
                    }),
                }),
                dictation_engine: Arc::new(FakeEngine),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                task_spawner: Arc::new(TokioTaskSpawner),
                ..BackendDependencies::unsupported()
            },
        )
        .unwrap();
        backend.start().await.unwrap();
        let mut starting = Some(Box::pin(backend.start_dictation()));
        assert!(futures_util::poll!(starting.as_mut().unwrap().as_mut()).is_pending());
        let session_id = backend.snapshot().dictation.session_id.unwrap();
        let mut cancel = std::pin::pin!(backend.cancel_dictation(Some(session_id)));
        assert!(
            futures_util::poll!(cancel.as_mut()).is_pending(),
            "cancel must still own the pending native preparation"
        );
        assert_eq!(
            backend.start_dictation().await.unwrap_err().code,
            BackendErrorCode::Busy
        );
        if drop_start {
            // Cancellation still owns the shared preparation even if the IPC
            // caller disappears; dropping its response cannot abandon TIS.
            drop(starting.take());
        }
        preparing.add_permits(1);
        // The late starter and explicit cancel both reach the same prepared
        // session. Exactly one restores it; the other must join that cleanup.
        if let Some(starting) = starting.as_mut() {
            assert!(futures_util::poll!(starting.as_mut()).is_pending());
        }
        assert!(futures_util::poll!(cancel.as_mut()).is_pending());
        assert_eq!(
            backend.start_dictation().await.unwrap_err().code,
            BackendErrorCode::Busy
        );
        restoring.add_permits(1);
        if let Some(starting) = starting {
            let (start_result, cancel_result) = tokio::join!(starting, cancel);
            assert_eq!(start_result.unwrap_err().code, BackendErrorCode::Cancelled);
            cancel_result.unwrap();
        } else {
            cancel.await.unwrap();
        }
        assert_eq!(restored.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn cancelling_pending_insertion_joins_preparation_and_native_restore() {
        assert_pending_insertion_cleanup(false).await;
    }

    #[tokio::test]
    async fn pending_insertion_cleanup_survives_a_dropped_start_caller() {
        assert_pending_insertion_cleanup(true).await;
    }

    #[tokio::test]
    async fn pending_stop_does_not_follow_a_replacement_dictation_session() {
        let release = Arc::new(tokio::sync::Notify::new());
        let data_dir = TestDataDir::new("stop-does-not-follow-replacement");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(BlockingStartEngine {
                    entered: Arc::new(tokio::sync::Notify::new()),
                    release: Arc::clone(&release),
                }),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: crate::domains::BackendServices::unsupported(),
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        // Poll the public requests explicitly: the old stop has already seen
        // Starting, but cannot resume until after cancellation and replacement.
        // This fixes the interleaving without relying on scheduler timing.
        let mut first_start = std::pin::pin!(backend.start_dictation());
        assert!(futures_util::poll!(first_start.as_mut()).is_pending());
        let first_session = backend.snapshot().dictation.session_id.unwrap();
        let mut pending_stop = std::pin::pin!(backend.stop_dictation());
        assert!(futures_util::poll!(pending_stop.as_mut()).is_pending());
        backend.cancel_dictation(Some(first_session)).await.unwrap();
        assert_eq!(
            backend.start_dictation().await.unwrap_err().code,
            BackendErrorCode::Busy
        );
        // Native startup still owns resources after logical cancellation. Let
        // it clean up before B starts, while deliberately not polling A's old
        // stop request: that request must remain bound to A after B is ready.
        release.notify_one();
        assert_eq!(
            first_start.await.unwrap_err().code,
            BackendErrorCode::Cancelled
        );

        let mut second_start = std::pin::pin!(backend.start_dictation());
        assert!(futures_util::poll!(second_start.as_mut()).is_pending());
        let second_session = backend.snapshot().dictation.session_id.unwrap();
        assert_ne!(first_session, second_session);
        // Owned startup may not have polled the engine yet. Keep the permit
        // instead of sending a broadcast which only wakes existing waiters.
        release.notify_one();
        assert_eq!(second_start.await.unwrap(), second_session);

        let error = pending_stop
            .await
            .expect_err("old stop must not finalize the new recording");
        assert_eq!(error.code, BackendErrorCode::InvalidState);
        assert_eq!(
            backend.snapshot().dictation.session_id,
            Some(second_session)
        );
        assert_eq!(
            backend.snapshot().dictation.phase,
            DictationPhase::Recording
        );
        backend
            .cancel_dictation(Some(second_session))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancellation_while_engine_starts_never_reenters_recording() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let data_dir = TestDataDir::new("cancel-during-start");
        let backend = Arc::new(
            OpenLessBackend::new(
                BackendConfig {
                    data_dir: data_dir.path().to_path_buf(),
                    ..BackendConfig::default()
                },
                BackendDependencies {
                    host_actions: Arc::new(FakeHost::default()),
                    text_inserter: Arc::new(FakeInserter),
                    dictation_engine: Arc::new(BlockingStartEngine {
                        entered: Arc::clone(&entered),
                        release: Arc::clone(&release),
                    }),
                    task_spawner: Arc::new(TokioTaskSpawner),
                    credential_store: Arc::new(
                        crate::credentials::InMemoryCredentialStore::default(),
                    ),
                    services: crate::domains::BackendServices::unsupported(),
                    local_asr_runtime: None,
                    selection_runtime: None,
                    selection_polisher: None,
                },
            )
            .unwrap(),
        );
        backend.start().await.unwrap();
        let mut events = backend.subscribe();
        let starting_backend = Arc::clone(&backend);
        let start_task = tokio::spawn(async move { starting_backend.start_dictation().await });
        entered.notified().await;

        let session = backend.snapshot().dictation.session_id.unwrap();
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Starting);
        backend.cancel_dictation(Some(session)).await.unwrap();
        release.notify_one();
        assert_eq!(
            start_task.await.unwrap().unwrap_err().code,
            BackendErrorCode::Cancelled
        );
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);

        let mut saw_recording_after_cancel = false;
        let mut cancelled_sequence = None;
        while let Ok(event) = events.try_recv() {
            if matches!(
                event.kind,
                BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                    phase: DictationPhase::Cancelled,
                    ..
                })
            ) {
                cancelled_sequence = Some(event.sequence);
            }
            if cancelled_sequence.is_some_and(|sequence| event.sequence > sequence)
                && matches!(
                    event.kind,
                    BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                        phase: DictationPhase::Recording,
                        ..
                    })
                )
            {
                saw_recording_after_cancel = true;
            }
        }
        assert!(!saw_recording_after_cancel);
    }

    #[tokio::test]
    async fn hotkey_combined_edge_cancels_the_same_generation_during_start_await() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let data_dir = TestDataDir::new("hotkey-combined-during-start");
        let backend = Arc::new(backend_with_dictation_engine(
            data_dir.path().to_path_buf(),
            Arc::new(BlockingStartEngine {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
        ));
        backend.start().await.unwrap();
        let mut preferences = backend.get_preferences();
        preferences.hotkey.mode = crate::shared_types::HotkeyMode::Hold;
        // An explicit combo has no modifier-only ambiguity, so this test
        // isolates the start/Combined race instead of waiting for grace.
        preferences.dictation_hotkey = crate::shared_types::ShortcutBinding {
            primary: "F9".into(),
            modifiers: vec!["ctrl".into()],
        };
        backend.set_preferences(preferences).unwrap();

        let pressed_at = std::time::Instant::now();
        let starting_backend = Arc::clone(&backend);
        let start_task = tokio::spawn(async move {
            starting_backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                    press_id: 41,
                    at: pressed_at,
                })
                .await
        });
        entered.notified().await;

        // Combined bypasses the serialized Pressed/Released gate. The stable
        // press id lets it cancel only the session opened by this physical key.
        assert_eq!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Combined {
                    press_id: 41,
                    at: pressed_at + std::time::Duration::from_millis(1),
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationCancelled
        );
        release.notify_one();
        assert_eq!(
            start_task.await.unwrap().unwrap_err().code,
            BackendErrorCode::Cancelled
        );
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
    }

    #[tokio::test]
    async fn shared_hotkey_edges_own_hold_auto_and_combo_abort_semantics() {
        let (backend, _) = backend();
        backend.start().await.unwrap();

        let mut preferences = backend.get_preferences();
        preferences.hotkey.mode = crate::shared_types::HotkeyMode::Hold;
        backend.set_preferences(preferences).unwrap();
        let pressed_at = std::time::Instant::now();
        assert!(matches!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                    press_id: 1,
                    at: pressed_at,
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationStarted(_)
        ));
        assert!(matches!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Released {
                    press_id: 1,
                    at: pressed_at + std::time::Duration::from_millis(50),
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationCompleted(_)
        ));

        let mut preferences = backend.get_preferences();
        preferences.hotkey.mode = crate::shared_types::HotkeyMode::Auto;
        backend.set_preferences(preferences).unwrap();
        let short_press = std::time::Instant::now() + std::time::Duration::from_secs(1);
        backend
            .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                press_id: 2,
                at: short_press,
            })
            .await
            .unwrap();
        assert_eq!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Released {
                    press_id: 2,
                    at: short_press + std::time::Duration::from_millis(100),
                })
                .await
                .unwrap(),
            CliDispatchOutcome::Noop
        );
        assert_eq!(
            backend.snapshot().dictation.phase,
            DictationPhase::Recording
        );
        assert!(matches!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                    press_id: 3,
                    at: short_press + std::time::Duration::from_millis(500),
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationCompleted(_)
        ));

        let long_press = short_press + std::time::Duration::from_secs(2);
        assert!(matches!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                    press_id: 4,
                    at: long_press,
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationStarted(_)
        ));
        assert!(matches!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Released {
                    press_id: 4,
                    at: long_press + std::time::Duration::from_millis(500),
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationCompleted(_)
        ));

        backend
            .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Pressed {
                press_id: 5,
                at: long_press + std::time::Duration::from_secs(2),
            })
            .await
            .unwrap();
        assert_eq!(
            backend
                .dispatch_dictation_hotkey_edge(DictationHotkeyEdge::Combined {
                    press_id: 5,
                    at: long_press + std::time::Duration::from_secs(2),
                })
                .await
                .unwrap(),
            CliDispatchOutcome::DictationCancelled
        );
        assert_eq!(backend.snapshot().dictation.phase, DictationPhase::Idle);
    }

    /// 屏幕 OCR 的可控假槽位：记录探测与作废次数；discard 后结果永久不可取。
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct FakeScreenOcrSlot {
        ready: Mutex<Option<String>>,
        discarded: std::sync::atomic::AtomicBool,
        takes: AtomicUsize,
        discards: AtomicUsize,
    }

    impl crate::ports::ScreenOcrSlot for FakeScreenOcrSlot {
        fn try_take(&self) -> Option<String> {
            self.takes.fetch_add(1, Ordering::SeqCst);
            if self.discarded.load(Ordering::SeqCst) {
                return None;
            }
            self.ready.lock().unwrap().take()
        }

        fn discard(&self) {
            self.discards.fetch_add(1, Ordering::SeqCst);
            self.discarded.store(true, Ordering::SeqCst);
            *self.ready.lock().unwrap() = None;
        }
    }

    /// 带屏幕 OCR 计数的 HostContext：start_screen_ocr 只在开关打开时被 core 调用。
    #[derive(Default)]
    struct ScreenOcrHostContext {
        capture_calls: AtomicUsize,
        screen_starts: AtomicUsize,
        slots: Mutex<Vec<Arc<FakeScreenOcrSlot>>>,
    }

    impl HostContextAdapter for ScreenOcrHostContext {
        fn capture(
            &self,
            _include_cursor: bool,
        ) -> BoxFuture<'static, Result<HostContextCapture, BackendError>> {
            self.capture_calls.fetch_add(1, Ordering::SeqCst);
            boxed(async { Ok(HostContextCapture::default()) })
        }

        fn start_screen_ocr(&self) -> crate::ports::ScreenOcrStart {
            self.screen_starts.fetch_add(1, Ordering::SeqCst);
            let slot = Arc::new(FakeScreenOcrSlot::default());
            self.slots.lock().unwrap().push(Arc::clone(&slot));
            crate::ports::ScreenOcrStart::started(slot)
        }

    }

    #[tokio::test]
    async fn screen_ocr_starts_only_when_enabled_once_per_session_and_discards_on_cancel() {
        let host = Arc::new(ScreenOcrHostContext::default());
        let data_dir = TestDataDir::new("screen-ocr-lifecycle");
        let backend = OpenLessBackend::new(
            BackendConfig {
                data_dir: data_dir.path().to_path_buf(),
                ..BackendConfig::default()
            },
            BackendDependencies {
                host_actions: Arc::new(FakeHost::default()),
                text_inserter: Arc::new(FakeInserter),
                dictation_engine: Arc::new(FakeEngine),
                task_spawner: Arc::new(TokioTaskSpawner),
                credential_store: Arc::new(crate::credentials::InMemoryCredentialStore::default()),
                services: {
                    let mut services = crate::domains::BackendServices::unsupported();
                    services.host_context = Arc::clone(&host) as Arc<dyn HostContextAdapter>;
                    services
                },
                local_asr_runtime: None,
                selection_runtime: None,
                selection_polisher: None,
            },
        )
        .unwrap();
        backend.start().await.unwrap();

        // 默认关闭：host 一次屏幕观测都不该开始（也没有截图发生）。
        backend.capture_host_dictation_context(DictationStartOptions::default()).await.unwrap();
        assert_eq!(host.screen_starts.load(Ordering::SeqCst), 0);

        let mut preferences = backend.get_preferences();
        preferences.screen_ocr_context_enabled = true;
        backend.set_preferences(preferences).unwrap();

        // 第一个会话：启动一次观测；识别完躺进槽位的结果也要能被开关立即作废。
        backend.start_dictation().await.unwrap();
        assert_eq!(host.screen_starts.load(Ordering::SeqCst), 1);
        let first_slot = Arc::clone(&host.slots.lock().unwrap()[0]);
        *first_slot.ready.lock().unwrap() = Some("already recognized".to_string());
        let mut preferences = backend.get_preferences();
        preferences.screen_ocr_context_enabled = false;
        backend.set_preferences(preferences).unwrap();
        assert_eq!(
            first_slot.discards.load(Ordering::SeqCst),
            1,
            "会话进行中关掉开关 → 立刻作废"
        );
        assert_eq!(
            crate::ports::ScreenOcrSlot::try_take(first_slot.as_ref()),
            None,
            "作废后即使 ready 也不得被消费"
        );

        // 重新打开，验证 cancel 作废 + 新会话拿到全新槽位（无跨会话缓存）。
        let mut preferences = backend.get_preferences();
        preferences.screen_ocr_context_enabled = true;
        backend.set_preferences(preferences).unwrap();
        backend.cancel_dictation(None).await.unwrap();
        assert_eq!(first_slot.discards.load(Ordering::SeqCst), 2, "cancel 再作废一次");

        // 第二个会话：新会话拿到新的槽位，正常结束后同样被作废。
        backend.start_dictation().await.unwrap();
        assert_eq!(host.screen_starts.load(Ordering::SeqCst), 2);
        backend.stop_dictation().await.unwrap();
        let slots = host.slots.lock().unwrap();
        assert_eq!(slots.len(), 2, "每次听写恰好一次观测");
        assert!(!Arc::ptr_eq(&slots[0], &slots[1]), "两个会话是两个不同槽位");
        assert_eq!(slots[1].discards.load(Ordering::SeqCst), 1);
    }
}
