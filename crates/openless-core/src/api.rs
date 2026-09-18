use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::activity::ActivityDay;
use crate::config::{BackendConfig, BackendDependencies, Clock, SystemClock, TaskSpawner};
use crate::dictation_context::{
    DictationContext, DictationProviderInvocations, DictationStartOptions,
};
use crate::errors::{BackendError, BackendErrorCode};
use crate::events::{
    BackendEventKind, BackendEventPublisher, EventBus, EventReplay, EventSubscription,
};
use crate::ports::{
    EngineFailureStage, EngineProgress, EngineProgressSink, EngineStage, HostAction, InsertOutcome,
    TextInserter, TextInsertionSession,
};
use crate::shared_types::UserPreferences;
use crate::types::{
    DictationPhase, DictationResult, DictationSession, DictationStateSnapshot, DictionaryEntry,
    HistoryChange, HistoryInsertStatus, HistorySource, PreferencesChange, SessionId,
    VocabularyChange,
};
use crate::vocabulary::DictionaryStore;
use crate::{ActivityStore, HistoryStore, PreferencesStore};

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
    pub preferences_revision: u64,
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictationContextPurpose {
    Dictation,
    AsrOnly,
}

struct VoiceCaptureLifecycle;

impl VoiceCaptureLifecycle {
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
}

impl BackendRepositories {
    pub fn open(data_dir: &std::path::Path) -> Result<Self, BackendError> {
        let preferences = Arc::new(PreferencesStore::open(data_dir.join("preferences.json"))?);
        Ok(Self {
            preferences,
            history: Arc::new(HistoryStore::at_data_dir(data_dir)),
            activity: Arc::new(
                ActivityStore::at_data_dir(data_dir).unwrap_or_else(|_| ActivityStore::in_memory()),
            ),
            vocabulary: Arc::new(DictionaryStore::at_data_dir(data_dir)),
        })
    }
}

struct MutableState {
    running: bool,
    dictation: DictationStateSnapshot,
    dictation_context: Option<Arc<DictationContext>>,
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
    task_spawner: Arc<dyn TaskSpawner>,
    _resources: Arc<crate::voice_session::VoiceResourceHold>,
    cancel_result: std::sync::OnceLock<Result<(), BackendError>>,
    /// 听写触发时冻结的光标前文本尾部。有值时对上屏文本做衔接调整
    /// (补空格/首字母大小写);只作用于插入层,历史与诊断保持润色原始输出。
    boundary_tail: Option<String>,
    // 0: open, 1: native finalization in flight, 2: cancellation owns cleanup,
    // 3: native finalization settled, 4: cancellation cleanup settled.
    // Cancellation must join state 1 or 2 rather
    // than release the next voice session while its committed input continues.
    terminal: AtomicU8,
    /// finish / cancel 的原生收尾都在独立任务里跑；两者通过 terminal 状态 +
    /// 本 Notify 会合，保证后来的取消能等到在途收尾结束。
    drained: tokio::sync::Notify,
}

impl ActiveTextInsertion {
    fn new(
        platform: Arc<dyn TextInsertionSession>,
        context: &DictationContext,
        task_spawner: Arc<dyn TaskSpawner>,
        resources: Arc<crate::voice_session::VoiceResourceHold>,
    ) -> Arc<Self> {
        Arc::new(Self {
            platform,
            task_spawner,
            _resources: resources,
            cancel_result: std::sync::OnceLock::new(),
            boundary_tail: crate::insertion_boundary::document_tail_before_cursor(
                context.polish.cursor_context.as_deref(),
            ),
            terminal: AtomicU8::new(0),
            drained: tokio::sync::Notify::new(),
        })
    }

    async fn finish(self: &Arc<Self>, final_text: String) -> Result<InsertOutcome, BackendError> {
        // 上屏文本做与冻结光标尾部同源的衔接调整(补空格/首字母大小写)。
        // 历史与诊断保存的是调整前的润色原始输出。
        let final_text = match &self.boundary_tail {
            Some(tail) => crate::insertion_boundary::adjust_for_boundary(&final_text, Some(tail)),
            None => final_text,
        };
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
        self.platform.finish(final_text).await
    }

    async fn cancel(self: &Arc<Self>) -> Result<(), BackendError> {
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
    vocabulary_revision: Arc<AtomicU64>,
    history: Arc<HistoryStore>,
    history_revision: Arc<AtomicU64>,
    activity: Arc<ActivityStore>,
    preferences: Arc<PreferencesStore>,
    preferences_revision: Arc<AtomicU64>,
    settings_write_gate: Mutex<()>,
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
        Self {
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
        let history_revision = Arc::new(AtomicU64::new(0));
        let vocabulary_revision = Arc::new(AtomicU64::new(0));
        if let Some(runtime) = deps.local_asr_runtime.take() {
            deps.services.local_asr = Arc::new(crate::local_asr_service::LocalAsrService::new(
                Arc::clone(&repositories.preferences),
                runtime,
                config.data_dir.join("models"),
                BackendEventPublisher::new(Arc::clone(&events)),
                Arc::clone(&preferences_revision),
            ));
        }
        if let Some((polisher, transcription)) = deps.services.take_auxiliary_runtime() {
            deps.services.auxiliary = Arc::new(crate::auxiliary::AuxiliaryService::new(
                Arc::clone(&repositories.preferences),
                Arc::clone(&repositories.vocabulary),
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
                transcripts: HashMap::new(),
                silence_monitor: None,
            })),
            phase_changed: Arc::new(tokio::sync::Notify::new()),
            hotkey: Mutex::new(crate::hotkey_interpreter::HotkeyInterpreter::default()),
            hotkey_dispatch_gate: tokio::sync::Mutex::new(()),
            vocabulary: repositories.vocabulary,
            vocabulary_revision,
            history: repositories.history,
            history_revision,
            activity: repositories.activity,
            preferences: repositories.preferences,
            preferences_revision,
            settings_write_gate: Mutex::new(()),
            text_insertions: Arc::new(Mutex::new(HashMap::new())),
            voice_sessions: Arc::new(crate::voice_session::VoiceSessionGate::default()),
        })
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
        })
    }

    pub async fn start(&self) -> Result<StartupSnapshot, BackendError> {
        let mut state = self.state.write().expect("backend state lock poisoned");
        if state.running {
            return Ok(StartupSnapshot {
                contract_version: crate::BACKEND_CONTRACT_VERSION.to_string(),
                backend: BackendSnapshot {
                    running: true,
                    dictation: state.dictation.clone(),
                    vocabulary_revision: self.vocabulary_revision.load(Ordering::Acquire),
                    history_revision: self.history_revision.load(Ordering::Acquire),
                    preferences_revision: self.preferences_revision.load(Ordering::Acquire),
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
                preferences_revision: self.preferences_revision.load(Ordering::Acquire),
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
        // Start native teardown before awaiting unrelated domain shutdown.
        // This also retains a cleanup hold if the shutdown caller disappears.
        let dictation_cleanup =
            active_session.map(|session_id| self.cancel_session_adapters(session_id));
        if let Some(cleanup) = dictation_cleanup {
            let cancel_result = cleanup.await;
            let host_result = active_session
                .map(|session_id| self.hide_dictation_feedback(session_id))
                .unwrap_or(Ok(()));
            cancel_result?;
            host_result?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> BackendSnapshot {
        let state = self.state.read().expect("backend state lock poisoned");
        BackendSnapshot {
            running: state.running,
            dictation: state.dictation.clone(),
            vocabulary_revision: self.vocabulary_revision.load(Ordering::Acquire),
            history_revision: self.history_revision.load(Ordering::Acquire),
            preferences_revision: self.preferences_revision.load(Ordering::Acquire),
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

    /// Apply physical dictation-key edges using the shared toggle hotkey rules.
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
    /// The options are only consumed when this edge actually starts a new session.
    pub async fn dispatch_dictation_hotkey_edge_with_options(
        &self,
        edge: DictationHotkeyEdge,
        options: DictationStartOptions,
    ) -> Result<CliDispatchOutcome, BackendError> {
        self.dispatch_dictation_hotkey_edge_with_session_options(
            edge,
            DictationHotkeyDispatchOptions { start: options },
        )
        .await
    }

    /// Apply a physical hotkey edge with host-captured start options.
    ///
    /// Start options are consumed only when the edge creates a session. This
    /// preserves desktop modifier semantics without making the host duplicate
    /// the toggle state machine.
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
                    hotkey.press(press_id, at, phase, modifier_only)
                }
                DictationHotkeyEdge::Released { press_id, at: _ } => hotkey.release(press_id),
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
                .stop_dictation()
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

    pub fn get_preferences(&self) -> UserPreferences {
        self.preferences.get()
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

        self.publish_preferences_changed();
        Ok(crate::SettingsUpdateOutcome {
            preferences,
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

    pub fn list_history(&self) -> Result<Vec<DictationSession>, BackendError> {
        self.history.list()
    }

    pub fn list_activity(&self) -> Result<Vec<ActivityDay>, BackendError> {
        self.activity.snapshot()
    }

    fn record_activity(
        &self,
        date: &str,
        chars: u64,
        duration_ms: u64,
    ) -> Result<(), BackendError> {
        self.activity.bump(date, chars, duration_ms)?;
        self.publish_history_changed();
        Ok(())
    }

    fn append_history(
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

    fn update_history_entry(&self, session: DictationSession) -> Result<bool, BackendError> {
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

    pub fn add_vocabulary(
        &self,
        phrase: String,
        note: Option<String>,
    ) -> Result<DictionaryEntry, BackendError> {
        let entry = self.vocabulary.add(phrase, note)?;
        self.publish_vocabulary_changed();
        Ok(entry)
    }

    fn record_vocabulary_hits(&self, text: &str) -> Result<u64, BackendError> {
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
        self.voice_sessions.acquire(session_id)?;
        let starting_resources = self.voice_sessions.hold_resources(session_id)?;
        // Freeze the destination before context/credential awaits or feedback
        // can change foreground focus. No native preparation happens yet.
        let inserter = insert_text
            .then(|| self.deps.text_inserter.capture_target())
            .flatten()
            .unwrap_or_else(|| Arc::clone(&self.deps.text_inserter)); // Context capture can await AX, a keyring, or another host service.
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
                        Some(
                            crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                                &context,
                                "insertion failed before recording started",
                            ),
                        ),
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
                    Some(
                        crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                            &context,
                            "recording failed before transcription started",
                        ),
                    ),
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
        self.stop_dictation_session_inner(None).await
    }

    pub async fn stop_dictation_session(
        &self,
        session_id: SessionId,
    ) -> Result<DictationResult, BackendError> {
        self.stop_dictation_session_inner(Some(session_id)).await
    }

    async fn stop_dictation_session_inner(
        &self,
        mut expected_session_id: Option<SessionId>,
    ) -> Result<DictationResult, BackendError> {
        let (session_id, context) = loop {
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
                        let context = state.dictation_context.clone().ok_or_else(|| {
                            BackendError::new(
                                BackendErrorCode::Internal,
                                "active dictation session has no captured context",
                            )
                        })?;
                        state.dictation.phase = DictationPhase::Transcribing;
                        self.events.publish(
                            Some(session_id),
                            BackendEventKind::DictationStateChanged(state.dictation.clone()),
                        );
                        self.phase_changed.notify_waiters();
                        Some((session_id, context))
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

        let progress = self.engine_progress_sink();
        let resources = self.voice_sessions.hold_resources(session_id)?;
        let finishing = self.deps.dictation_engine.finish(session_id, progress);
        // finish() has taken the recorder out of the engine's registry. Cancel
        // can no longer join that stop through engine.cancel(), so its original
        // owner must retain the shared lease even without a text insertion.
        let engine_result = match own_voice_effect(
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
            // 空录音（没说话或全程静音）不是故障：胶囊按取消态静默淡出，
            // 不弹红色错误。历史仍留 emptyTranscript 条目——空录音的归档
            // 音频靠该条目在历史页提供「重新转写」入口，删掉会孤立音频文件。
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
                Some(
                    crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                        &context,
                        "transcription returned an empty transcript",
                    ),
                ),
            );
            {
                let mut state = self.state.write().expect("backend state lock poisoned");
                if state.dictation.session_id == Some(session_id) {
                    state.dictation.phase = DictationPhase::Cancelled;
                    self.events.publish(
                        Some(session_id),
                        BackendEventKind::DictationStateChanged(state.dictation.clone()),
                    );
                    self.phase_changed.notify_waiters();
                }
            }
            let _ = self.cancel_text_insertion(session_id).await;
            let _ = self.hide_dictation_feedback(session_id);
            self.reset_dictation_session(session_id);
            return Err(BackendError::new(
                BackendErrorCode::Cancelled,
                "transcription returned an empty transcript",
            ));
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
            Some(InsertOutcome::CopiedFallback) => HistoryInsertStatus::CopiedFallback,
            None => HistoryInsertStatus::NotRequested,
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
            pipeline_mode: Some("traditional".to_string()),
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
            pipeline_mode: Some("traditional".to_string()),
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
            Some(
                crate::polish_diagnostics::PolishDiagnostics::not_called_from_context(
                    &context,
                    "recording failed before transcription started",
                ),
            ),
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
        let hotwords = self
            .vocabulary
            .list()?
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.phrase)
            .collect();
        // 固定本地 provider 直接从规范化 preferences 捕获。
        let _ = purpose;
        let active_asr_provider = crate::dictation_context::ProviderInvocation::for_provider(
            &preferences.active_asr_provider,
        );
        let active_llm_provider = crate::dictation_context::ProviderInvocation::for_provider(
            &preferences.active_llm_provider,
        );
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
            DictationProviderInvocations::new(active_asr_provider, active_llm_provider),
            hotwords,
            recent_history,
            options,
        );
        context.screen_ocr_pending = screen_ocr_pending.unwrap_or_default();
        context.polish.screen_ocr_trace = screen_ocr_trace;
        // 调用方自带正文（如 options.cursor_context 直传）保持 Included 候选；
        // 宿主刚读取成功也一样（Included），失败路径已在上面覆盖轨迹。
        if context.polish.cursor_context.is_some() {
            context.polish.cursor_context_trace =
                crate::polish_diagnostics::PolishContextTrace::included();
        } else if preferences.cursor_context_enabled {
            context.polish.cursor_context_trace = cursor_trace;
        }

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
