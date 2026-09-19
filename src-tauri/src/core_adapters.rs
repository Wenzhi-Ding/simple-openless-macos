//! Thin Tauri implementations of the framework-independent core ports.
use crate::recorder::{AudioConsumer as LegacyAudioConsumer, Recorder, RecorderError};
use futures_util::future::BoxFuture;
use openless_core::{
    ActiveRecording, AudioConsumer as CoreAudioConsumer, AudioRecorder, BackendError,
    BackendErrorCode, DictationContext, DictationEngine, HostAction, HostActions, InsertOutcome,
    RecordingArchive, RecordingProgressSink, SessionId,
    TextInserter as CoreTextInserter, TextInsertionSession, TextPolisher, TextStreamChunk,
    TextStreamSink, TranscriptOutput, TranscriptionEngine, TranscriptionSession,
};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
pub(crate) type AppHandleSlot = Arc<Mutex<Option<AppHandle>>>;
pub(crate) fn app_handle_slot() -> AppHandleSlot {
    Arc::new(Mutex::new(None))
}
/// Late-bound Core backend shared with adapters that are constructed before
/// `OpenLessBackend::new` returns. The weak reference keeps Core ownership
/// explicit without making an adapter query Tauri managed state through
/// `AppHandle` or creating a backend/adapter reference cycle.
pub(crate) type BackendSlot = Arc<Mutex<Option<std::sync::Weak<openless_core::OpenLessBackend>>>>;
pub(crate) fn backend_slot() -> BackendSlot {
    Arc::new(Mutex::new(None))
}
/// Native audio callbacks and resource destructors do not inherit Tokio's
/// thread-local context. Always enqueue on Tauri's shared executor instead of
/// asking the callback thread for a runtime and silently losing its cleanup.
struct TauriTaskSpawner;
impl openless_core::TaskSpawner for TauriTaskSpawner {
    fn spawn(&self, task: BoxFuture<'static, ()>) {
        tauri::async_runtime::spawn(task);
    }
}
#[derive(Clone)]
pub(crate) struct TauriNativeAsrDependencies {
    qwen3: Arc<crate::asr::local::qwen3_mlx::Qwen3MlxRuntime>,
}
impl TauriNativeAsrDependencies {
    pub(crate) fn new() -> Self {
        Self {
            qwen3: Arc::new(crate::asr::local::qwen3_mlx::Qwen3MlxRuntime::new()),
        }
    }
}
pub(crate) fn backend_dependencies(
    app: AppHandleSlot,
    backend: BackendSlot,
    native_asr_dependencies: TauriNativeAsrDependencies,
    local_polisher: Arc<crate::local_llm::LocalLlamaPolisher>,
    preferences: Arc<openless_core::PreferencesStore>,
    hotkey_status: Arc<Mutex<openless_core::HotkeyStatus>>,
    prearm: Arc<crate::prearm::PrearmRegistry>,
) -> openless_core::BackendDependencies {
    let task_spawner: Arc<dyn openless_core::TaskSpawner> = Arc::new(TauriTaskSpawner);
    let local_asr_runtime = Arc::new(TauriLocalAsrRuntimeAdapter::new(
        native_asr_dependencies.clone(),
    ));
    let _ = preferences;
    let transcription = Arc::new(openless_core::TranscriptionRouter::default());
    let native_asr: Arc<dyn TranscriptionEngine> =
        Arc::new(TauriNativeTranscriptionEngine::new(native_asr_dependencies));
    transcription
        .register(
            crate::asr::local::qwen3_mlx::PROVIDER_ID,
            Arc::clone(&native_asr),
        )
        .expect("native ASR provider ids are non-empty");
    let polisher = Arc::new(openless_core::TextPolisherRouter::default());
    polisher
        .register(crate::local_llm::LOCAL_LLM_PROVIDER_ID, local_polisher)
        .expect("local LLM provider id is valid");
    let polisher: Arc<dyn TextPolisher> = polisher;
    let auxiliary_transcription: Arc<dyn TranscriptionEngine> = transcription.clone();
    let auxiliary_polisher: Arc<dyn TextPolisher> = Arc::clone(&polisher);
    let host_recorder: Arc<dyn AudioRecorder> = Arc::new(TauriAudioRecorder {
        app: Arc::clone(&app),
        backend: Arc::clone(&backend),
        prearm,
    });
    let traditional = Arc::new(openless_core::PipelineDictationEngine::new(
        host_recorder,
        transcription,
        Arc::clone(&polisher),
    ));
    let dictation: Arc<dyn DictationEngine> =
        Arc::new(openless_core::DictationEngineRouter::new(traditional));
    let mut dependencies = openless_core::BackendDependencies::unsupported();
    dependencies
        .services
        .configure_auxiliary_runtime(auxiliary_polisher, auxiliary_transcription);
    dependencies.services.platform =
        Arc::new(TauriPlatformApi::new(Arc::clone(&app), hotkey_status));
    dependencies.services.host_context = Arc::new(TauriHostContextAdapter);
    dependencies.local_asr_runtime = Some(local_asr_runtime);
    dependencies.text_inserter = Arc::new(TauriTextInserter::new(Arc::clone(&app)));
    dependencies.host_actions = Arc::new(TauriHostActions::new(app));
    dependencies.dictation_engine = dictation;
    dependencies.task_spawner = task_spawner;
    dependencies
}
fn local_asr_backend_error(code: BackendErrorCode, error: impl std::fmt::Display) -> BackendError {
    BackendError::new(code, error.to_string())
}
struct TauriLocalAsrRuntimeAdapter {
    native: TauriNativeAsrDependencies,
}
impl TauriLocalAsrRuntimeAdapter {
    fn new(native: TauriNativeAsrDependencies) -> Self {
        Self { native }
    }
}
impl openless_core::ModelRuntimeAdapter for TauriLocalAsrRuntimeAdapter {
    fn runtime_status(
        &self,
        _model_dir: PathBuf,
        keep_loaded_secs: u32,
    ) -> BoxFuture<'static, Result<openless_core::LocalAsrRuntimeStatus, BackendError>> {
        let qwen3 = Arc::clone(&self.native.qwen3);
        Box::pin(async move {
            let status = qwen3
                .status_snapshot(crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID)
                .await;
            Ok(openless_core::LocalAsrRuntimeStatus {
                provider_id: crate::asr::local::qwen3_mlx::PROVIDER_ID.to_string(),
                available: status.available,
                loaded: status.runtime_ready,
                active_model: status.active_model,
                model_id: status.loaded_model_id,
                keep_loaded_secs,
                operation: status.loading.then(|| "loading".to_string()),
                error: status.error,
                last_error: status.last_error,
                last_prepare_ms: status.last_prepare_ms,
                last_transcribe_ms: status.last_transcribe_ms,
                last_audio_ms: status.last_audio_ms,
            })
        })
    }
    fn prepare(
        &self,
        model_dir: PathBuf,
        progress: openless_core::ModelPrepareProgressSink,
    ) -> BoxFuture<'static, Result<String, BackendError>> {
        let qwen3 = Arc::clone(&self.native.qwen3);
        Box::pin(async move {
            qwen3
                .ensure_loaded_with_progress(
                    crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID,
                    model_dir,
                    move |phase, percent, error| {
                        let (phase, label) = match phase {
                            "runtime" => (
                                openless_core::LocalAsrPreparePhase::Runtime,
                                "Start local Qwen3-ASR MLX worker",
                            ),
                            "load" => (
                                openless_core::LocalAsrPreparePhase::Load,
                                "Load Qwen3-ASR model",
                            ),
                            "finished" => (
                                openless_core::LocalAsrPreparePhase::Finished,
                                "Qwen3-ASR model ready",
                            ),
                            _ => (
                                openless_core::LocalAsrPreparePhase::Failed,
                                "Qwen3-ASR model failed to load",
                            ),
                        };
                        progress(openless_core::LocalAsrPrepareProgress {
                            runtime: openless_core::LocalAsrRuntimeKind::Qwen3Mlx,
                            phase,
                            model_alias: crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID
                                .to_string(),
                            label: label.to_string(),
                            percent,
                            error,
                        });
                    },
                )
                .await
                .map_err(|error| {
                    local_asr_backend_error(BackendErrorCode::Platform, format!("{error:#}"))
                })
        })
    }
    fn cancel_prepare(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.native.qwen3.request_cancel_prepare();
        Box::pin(async { Ok(()) })
    }
    fn release(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        let qwen3 = Arc::clone(&self.native.qwen3);
        Box::pin(async move {
            qwen3.release_now().await.map_err(|error| {
                local_asr_backend_error(BackendErrorCode::Platform, format!("{error:#}"))
            })
        })
    }
    fn preload(&self, _model_dir: PathBuf) -> BoxFuture<'static, Result<(), BackendError>> {
        let qwen3 = Arc::clone(&self.native.qwen3);
        Box::pin(async move {
            let status = qwen3
                .status_snapshot(crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID)
                .await;
            (status.runtime_ready
                && status.loaded_model_id.as_deref()
                    == Some(crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID))
            .then_some(())
            .ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "prepare the fixed Qwen3 MLX model before preload",
                )
            })
        })
    }
}
struct TauriPlatformApi {
    app: AppHandleSlot,
    hotkey_status: Arc<Mutex<openless_core::HotkeyStatus>>,
}
impl TauriPlatformApi {
    fn new(app: AppHandleSlot, hotkey_status: Arc<Mutex<openless_core::HotkeyStatus>>) -> Self {
        Self { app, hotkey_status }
    }
    fn app_handle(&self) -> Result<AppHandle, BackendError> {
        self.app.lock().clone().ok_or_else(|| {
            BackendError::new(
                BackendErrorCode::InvalidState,
                "Tauri app handle is not available",
            )
        })
    }
}
fn map_permission_state(
    status: crate::permissions::PermissionStatus,
) -> openless_core::PermissionState {
    match status {
        crate::permissions::PermissionStatus::Granted => openless_core::PermissionState::Granted,
        crate::permissions::PermissionStatus::Denied => openless_core::PermissionState::Denied,
        crate::permissions::PermissionStatus::NotDetermined => {
            openless_core::PermissionState::Unknown
        }
        crate::permissions::PermissionStatus::Restricted => {
            openless_core::PermissionState::Restricted
        }
        crate::permissions::PermissionStatus::NotApplicable => {
            openless_core::PermissionState::Unsupported
        }
        crate::permissions::PermissionStatus::NoDevice => openless_core::PermissionState::NoDevice,
    }
}
fn current_permission_snapshot() -> openless_core::PermissionSnapshot {
    openless_core::PermissionSnapshot {
        microphone: map_permission_state(crate::permissions::check_microphone()),
        accessibility: map_permission_state(crate::permissions::check_accessibility()),
    }
}
impl openless_core::PlatformApi for TauriPlatformApi {
    fn capabilities(
        &self,
    ) -> BoxFuture<'static, Result<openless_core::PlatformCapabilities, BackendError>> {
        Box::pin(async { Ok(openless_core::PlatformCapabilities::current()) })
    }
    fn microphone_devices(
        &self,
    ) -> BoxFuture<'static, Result<Vec<openless_core::MicrophoneDevice>, BackendError>> {
        Box::pin(async move {
            {
                let devices =
                    tauri::async_runtime::spawn_blocking(crate::recorder::list_input_devices)
                        .await
                        .map_err(map_tauri_error)?
                        .map_err(map_recorder_error)?;
                Ok(devices
                    .into_iter()
                    .map(|device| openless_core::MicrophoneDevice {
                        id: device.name.clone(),
                        name: device.name,
                        is_default: device.is_default,
                    })
                    .collect())
            }
        })
    }
    fn microphone_permission(
        &self,
    ) -> BoxFuture<'static, Result<openless_core::PermissionSnapshot, BackendError>> {
        Box::pin(async { Ok(current_permission_snapshot()) })
    }
    fn accessibility_permission(
        &self,
    ) -> BoxFuture<'static, Result<openless_core::PermissionSnapshot, BackendError>> {
        Box::pin(async { Ok(current_permission_snapshot()) })
    }
    fn request_microphone_permission(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        let app = self.app_handle();
        Box::pin(async move {
            let app = app?;
            let _ = crate::request_microphone_from_foreground(&app);
            Ok(())
        })
    }
    fn request_accessibility_permission(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        Box::pin(async {
            let _ = crate::permissions::request_accessibility();
            Ok(())
        })
    }
    fn hotkey_status(
        &self,
    ) -> BoxFuture<'static, Result<openless_core::HotkeyStatus, BackendError>> {
        {
            let hotkey_status = Arc::clone(&self.hotkey_status);
            Box::pin(async move { Ok(hotkey_status.lock().clone()) })
        }
    }
}
/// 本机转写超时策略(自原 core provider_rules 移植;纯本地计算)。
fn native_transcribe_timeout(provider_type: &str, duration_ms: u64) -> std::time::Duration {
    let _ = provider_type;
    let (numerator, denominator, extra, minimum) = (3_u64, 5_000_u64, 10, 30);
    let seconds = duration_ms
        .saturating_mul(numerator)
        .div_ceil(denominator)
        .saturating_add(extra)
        .max(minimum);
    std::time::Duration::from_secs(seconds)
}
struct TauriNativeTranscriptionEngine {
    dependencies: TauriNativeAsrDependencies,
}
impl TauriNativeTranscriptionEngine {
    fn new(dependencies: TauriNativeAsrDependencies) -> Self {
        Self { dependencies }
    }
}
#[derive(Clone)]
enum TauriNativeTranscriptionSessionKind {
    Qwen3Mlx {
        provider: Arc<crate::asr::local::qwen3_mlx::Qwen3MlxAsr>,
        runtime: Arc<crate::asr::local::qwen3_mlx::Qwen3MlxRuntime>,
    },
}
#[derive(Clone)]
struct TauriNativeTranscriptionSession {
    kind: TauriNativeTranscriptionSessionKind,
    asr_call_label: openless_core::AsrCallLabel,
    partials: Arc<dyn TextStreamSink>,
    next_offset: Arc<AtomicU64>,
    released: Arc<AtomicBool>,
    hotwords: Vec<String>,
}
impl TranscriptionEngine for TauriNativeTranscriptionEngine {
    fn start(
        &self,
        _session_id: SessionId,
        context: Arc<DictationContext>,
        partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, Result<Arc<dyn TranscriptionSession>, BackendError>> {
        let qwen3 = Arc::clone(&self.dependencies.qwen3);
        Box::pin(async move {
            if context.asr.provider_type != crate::asr::local::qwen3_mlx::PROVIDER_ID {
                return Err(BackendError::new(
                    BackendErrorCode::Unsupported,
                    format!(
                        "native ASR provider is unavailable: {}",
                        context.asr.provider_type
                    ),
                ));
            }
            let model =
                context.asr.model.clone().unwrap_or_else(|| {
                    crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID.to_string()
                });
            if model != crate::asr::local::qwen3_mlx::SUPPORTED_MODEL_ID {
                return Err(BackendError::new(
                    BackendErrorCode::Unsupported,
                    format!("Qwen3-ASR MLX supports qwen3-asr-1.7b only (requested {model})"),
                ));
            }
            let root = crate::persistence::models_root()
                .unwrap_or_else(|_| std::path::PathBuf::from("models"));
            let model_dir = openless_core::local_model_dir(&root);
            if !openless_core::directory_has_model_files(&model_dir) {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "Qwen3-ASR model directory is empty",
                ));
            }
            let provider = crate::asr::local::qwen3_mlx::Qwen3MlxAsr::new(
                Arc::clone(&qwen3),
                model.clone(),
                model_dir,
                context.asr.language.clone(),
            );
            Ok(Arc::new(TauriNativeTranscriptionSession {
                kind: TauriNativeTranscriptionSessionKind::Qwen3Mlx {
                    provider: Arc::new(provider),
                    runtime: qwen3,
                },
                asr_call_label: openless_core::AsrCallLabel::new(
                    context.asr.provider_type.clone(),
                    Some(model),
                ),
                partials,
                hotwords: context.enabled_hotwords().to_vec(),
                next_offset: Arc::new(AtomicU64::new(0)),
                released: Arc::new(AtomicBool::new(false)),
            }) as Arc<dyn TranscriptionSession>)
        })
    }
}
impl CoreAudioConsumer for TauriNativeTranscriptionSession {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        let TauriNativeTranscriptionSessionKind::Qwen3Mlx { provider, .. } = &self.kind;
        LegacyAudioConsumer::consume_pcm_chunk(provider.as_ref(), pcm);
    }
}
impl TranscriptionSession for TauriNativeTranscriptionSession {
    fn asr_call_label(&self) -> Option<openless_core::AsrCallLabel> {
        Some(self.asr_call_label.clone())
    }
    fn finish(&self) -> BoxFuture<'static, Result<TranscriptOutput, BackendError>> {
        let session = self.clone();
        let TauriNativeTranscriptionSessionKind::Qwen3Mlx { provider, .. } = self.kind.clone();
        let partials = Arc::clone(&self.partials);
        let next_offset = Arc::clone(&self.next_offset);
        let hotwords = self.hotwords.clone();
        Box::pin(async move {
            let timeout =
                native_transcribe_timeout("local-qwen3-mlx", provider.buffer_duration_ms());
            let result = provider
                .transcribe(timeout)
                .await
                .map_err(map_native_asr_error)
                .and_then(|output| {
                    let text = crate::hotwords::correct(&output.text, &hotwords);
                    let offset =
                        next_offset.fetch_add(text.chars().count() as u64, Ordering::AcqRel);
                    partials.publish(TextStreamChunk {
                        text: text.clone(),
                        offset,
                    })?;
                    Ok(TranscriptOutput {
                        text,
                        duration_ms: output.duration_ms,
                    })
                });
            if result.is_err() {
                session.cancel_native();
            }
            session.release_native();
            result
        })
    }
    fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        let session = self.clone();
        Box::pin(async move {
            session.cancel_native();
            session.release_native();
            Ok(())
        })
    }
}
impl TauriNativeTranscriptionSession {
    fn cancel_native(&self) {
        let TauriNativeTranscriptionSessionKind::Qwen3Mlx { provider, .. } = &self.kind;
        provider.cancel();
    }
    fn release_native(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let TauriNativeTranscriptionSessionKind::Qwen3Mlx { runtime, .. } = &self.kind;
        let _ = runtime;
    }
}
fn map_native_asr_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::new(
        BackendErrorCode::Provider,
        format!("native ASR provider failed: {error}"),
    )
}
pub(crate) struct TauriAudioRecorder {
    app: AppHandleSlot,
    backend: BackendSlot,
    /// 热键按下瞬间拉起的预置录音流；start 优先接管它，让按键到采集之间
    /// 只剩 core 会话链自身的耗时，而不是「会话链 + cpal 建流」串行相加。
    prearm: Arc<crate::prearm::PrearmRegistry>,
}
struct AudioConsumerBridge {
    inner: Arc<dyn CoreAudioConsumer>,
}
impl LegacyAudioConsumer for AudioConsumerBridge {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        self.inner.consume_pcm_chunk(pcm);
    }
}
struct TauriActiveRecording {
    /// 消费式停止：正常路径是 `Recorder::stop`，预置接管路径是预置流自己的停止闭包。
    stop: Option<Box<dyn FnOnce() + Send>>,
    archive: Option<Arc<TauriRecordingArchive>>,
    _mute: Option<crate::audio_mute::AudioMuteGuard>,
}
struct TauriRecordingArchive {
    path: PathBuf,
    available: Arc<AtomicBool>,
}
impl TauriRecordingArchive {
    fn new(path: PathBuf, available: bool) -> Self {
        Self {
            path,
            available: Arc::new(AtomicBool::new(available)),
        }
    }
}
impl RecordingArchive for TauriRecordingArchive {
    fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }
    fn read_pcm(&self) -> BoxFuture<'static, Result<Vec<u8>, BackendError>> {
        let path = self.path.clone();
        Box::pin(async move {
            let wav = tokio::fs::read(&path).await.map_err(|error| {
                BackendError::new(
                    BackendErrorCode::Persistence,
                    format!("read dictation recording archive: {error}"),
                )
            })?;
            if wav.len() <= 44
                || &wav[..4] != b"RIFF"
                || &wav[8..12] != b"WAVE"
                || !(wav.len() - 44).is_multiple_of(2)
            {
                return Err(BackendError::new(
                    BackendErrorCode::Persistence,
                    "dictation recording archive is not canonical PCM WAV",
                ));
            }
            Ok(wav[44..].to_vec())
        })
    }
    fn discard(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        let path = self.path.clone();
        let available = Arc::clone(&self.available);
        Box::pin(async move {
            if !available.load(Ordering::Acquire) {
                return Ok(());
            }
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {
                    available.store(false, Ordering::Release);
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    available.store(false, Ordering::Release);
                    Ok(())
                }
                Err(error) => {
                    log::warn!(
                        "[core-adapter] 清理成功口述的归档录音失败 {}: {error}",
                        path.display()
                    );
                    Err(BackendError::new(
                        BackendErrorCode::Persistence,
                        format!("discard dictation recording archive: {error}"),
                    ))
                }
            }
        })
    }
}
impl ActiveRecording for TauriActiveRecording {
    fn archive(&self) -> Option<Arc<dyn RecordingArchive>> {
        self.archive
            .as_ref()
            .map(|archive| archive.clone() as Arc<dyn RecordingArchive>)
    }
    fn stop(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BackendError>> {
        Box::pin(async move {
            let stop = self.stop.take().ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "Tauri recorder was already stopped",
                )
            })?;
            tauri::async_runtime::spawn_blocking(move || {
                stop();
                Ok(())
            })
            .await
            .map_err(|error| {
                BackendError::new(
                    BackendErrorCode::Internal,
                    format!("join Tauri recorder stop task: {error}"),
                )
            })?
        })
    }
}
/// 归档和系统输出静音沿用1.x的best-effort语义：目录不可写、没有默认扬声器或
/// macOS音量脚本失败，都不意味着输入麦克风不可用。分别告警并关闭失败的辅助项，
/// 让后面的Recorder::start独立报告真实采集错误；成功的静音guard仍由录音资源持有。
fn prepare_recording_options<M>(
    archive_path: Result<Option<PathBuf>, impl std::fmt::Display>,
    mute_enabled: bool,
    activate_mute: impl FnOnce() -> Result<M, String>,
) -> (Option<PathBuf>, Option<M>) {
    let archive_path = archive_path
        .map_err(|error| log::warn!("[recordings] archive unavailable; capture continues: {error}"))
        .ok()
        .flatten();
    let mute = mute_enabled.then(activate_mute).and_then(|result| {
        result
            .map_err(|error| {
                log::warn!("[audio-mute] failed to mute output; capture continues: {error}")
            })
            .ok()
    });
    (archive_path, mute)
}
impl AudioRecorder for TauriAudioRecorder {
    fn start(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        consumer: Arc<dyn CoreAudioConsumer>,
        progress: Arc<dyn RecordingProgressSink>,
    ) -> BoxFuture<'static, Result<Box<dyn ActiveRecording>, BackendError>> {
        let app = Arc::clone(&self.app);
        let backend = Arc::clone(&self.backend);
        let prearm = Arc::clone(&self.prearm);
        Box::pin(async move {
            if let Some(app) = app.lock().clone() {
                let state = app.state::<crate::commands::MicrophoneMonitorState>();
                let preview = state.lock().take();
                if let Some(preview) = preview {
                    preview.stop();
                }
            }
            let archive_path = context
                .recording
                .archive_enabled
                .then(|| crate::persistence::recording_path_for_session(&session_id.to_string()))
                .transpose();
            let microphone = context.recording.microphone_device_name.clone();
            let recording_plan = context.recording.clone();
            let fault_progress = Arc::clone(&progress);
            let (recording, runtime_errors) = tauri::async_runtime::spawn_blocking(move || {
                if recording_plan.archive_enabled {
                    if let Err(error) = crate::persistence::prune_recordings(
                        recording_plan.retention_days,
                        recording_plan.max_entries,
                    ) {
                        log::warn!("[recordings] prune before capture failed: {error:#}");
                    }
                }
                let (archive_path, mute) = prepare_recording_options(
                    archive_path,
                    recording_plan.mute_during_recording,
                    crate::audio_mute::AudioMuteGuard::activate,
                );
                let consumer: Arc<dyn LegacyAudioConsumer> =
                    Arc::new(AudioConsumerBridge { inner: consumer });

                // 优先接管热键按下瞬间预置的流：mic 匹配、还活着、未过期才用，
                // 否则回落到新建（行为与没有预置时一致）。adopt 可能等在建的流
                // 完工（condvar），必须留在 spawn_blocking 里。
                let prearmed =
                    prearm.adopt(crate::prearm::normalize_mic_name(microphone.as_deref().unwrap_or("")));
                // 接管时 elapsed 从按键瞬间（≈预置流创建）起算， capsule 计时与
                // 用户体感一致；新建路径维持「此刻起算」。
                let started_at = prearmed
                    .as_ref()
                    .map(|prearmed| prearmed.created_at)
                    .unwrap_or_else(Instant::now);
                let level_progress = Arc::clone(&progress);
                let level_handler: Arc<dyn Fn(f32) + Send + Sync> = Arc::new(move |level| {
                    let _ = level_progress
                        .publish_level(started_at.elapsed().as_millis() as u64, level);
                });

                let (stop, runtime_errors, archive_active) = match prearmed {
                    Some(prearmed) => {
                        // 归档器在接管时才创建：预置期间缓冲的音频只存内存，
                        // 接管这一刻连同实时流一起进归档（回放先行，顺序不乱）。
                        let archiver = archive_path.clone().and_then(|path| {
                            match crate::recorder::WavArchiver::create(&path) {
                                Ok(archiver) => Some(Arc::new(Mutex::new(archiver))),
                                Err(error) => {
                                    log::warn!(
                                        "[recorder] wav archive create failed at {path:?}: {error}"
                                    );
                                    None
                                }
                            }
                        });
                        let archive_active = archiver.is_some();
                        let replayed = prearmed.tap.attach(consumer, archiver, level_handler);
                        log::info!(
                            "[prearm] adopted stream {}ms after key press; replayed {} buffered bytes",
                            prearmed.created_at.elapsed().as_millis(),
                            replayed
                        );
                        (prearmed.stop, prearmed.error_rx, archive_active)
                    }
                    None => {
                        let recorder_archive_path = archive_path.clone();
                        let start_result = Recorder::start(
                            microphone,
                            consumer,
                            level_handler,
                            recorder_archive_path,
                        );
                        let (recorder, runtime_errors, archive_active) = match start_result {
                            Ok(started) => started,
                            Err(error) => {
                                if let Some(path) = &archive_path {
                                    let _ = std::fs::remove_file(path);
                                }
                                return Err(map_recorder_error(error));
                            }
                        };
                        (
                            Box::new(move || recorder.stop()) as Box<dyn FnOnce() + Send>,
                            runtime_errors,
                            archive_active,
                        )
                    }
                };
                let recording = Box::new(TauriActiveRecording {
                    stop: Some(stop),
                    archive: archive_path
                        .map(|path| Arc::new(TauriRecordingArchive::new(path, archive_active))),
                    _mute: mute,
                }) as Box<dyn ActiveRecording>;
                Ok((recording, runtime_errors))
            })
            .await
            .map_err(|error| {
                BackendError::new(
                    BackendErrorCode::Internal,
                    format!("join Tauri recorder start task: {error}"),
                )
            })??;
            tauri::async_runtime::spawn(async move {
                let runtime_error =
                    tauri::async_runtime::spawn_blocking(move || runtime_errors.recv()).await;
                let Ok(Ok(runtime_error)) = runtime_error else {
                    return;
                };
                let error = map_recorder_error(runtime_error);
                let _ = fault_progress.publish(openless_core::RecordingEvent::Fatal(error.clone()));
                let backend = backend.lock().as_ref().and_then(std::sync::Weak::upgrade);
                if let Some(backend) = backend {
                    if let Err(report_error) =
                        backend.report_recording_fault(session_id, error).await
                    {
                        if report_error.code != BackendErrorCode::InvalidState {
                            log::warn!("[recorder] report runtime fault failed: {report_error}");
                        }
                    }
                }
            });
            Ok(recording)
        })
    }
}
fn map_recorder_error(error: RecorderError) -> BackendError {
    let code = match error {
        RecorderError::PermissionDenied => BackendErrorCode::PermissionDenied,
        RecorderError::NoInputDevice | RecorderError::EngineFailed(_) => BackendErrorCode::Platform,
    };
    BackendError::new(code, error.user_message())
}
#[derive(Clone)]
pub(crate) struct TauriTextInserter {
    app: AppHandleSlot,
    insertion_target: Option<crate::focus_target::InsertionTarget>,
}
impl TauriTextInserter {
    fn new(app: AppHandleSlot) -> Self {
        Self {
            app,
            insertion_target: None,
        }
    }
}
impl CoreTextInserter for TauriTextInserter {
    fn capture_target(&self) -> Option<Arc<dyn CoreTextInserter>> {
        Some(Arc::new(Self {
            insertion_target: crate::focus_target::InsertionTarget::capture(),
            ..self.clone()
        }))
    }
    fn begin(
        &self,
        _session_id: SessionId,
        _context: Arc<DictationContext>,
    ) -> BoxFuture<'static, Result<Arc<dyn TextInsertionSession>, BackendError>> {
        let app = Arc::clone(&self.app);
        let insertion_target = self.insertion_target.clone();
        Box::pin(async move {
            let insertion_target =
                insertion_target.or_else(crate::focus_target::InsertionTarget::capture);
            let _app_handle = app.lock().clone().ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::InvalidState,
                    "Tauri AppHandle is not bound yet",
                )
            })?;
            Ok(Arc::new(TauriTextInsertionSession {
                insertion_target,
                finished: Arc::new(AtomicBool::new(false)),
            }) as Arc<dyn TextInsertionSession>)
        })
    }
}
#[derive(Clone)]
struct TauriTextInsertionSession {
    insertion_target: Option<crate::focus_target::InsertionTarget>,
    finished: Arc<AtomicBool>,
}
impl TauriTextInsertionSession {
    fn restore_insertion_target(&self) -> Result<(), BackendError> {
        self.insertion_target
            .is_some_and(crate::focus_target::InsertionTarget::reactivate)
            .then_some(())
            .ok_or_else(|| {
                BackendError::new(
                    BackendErrorCode::Platform,
                    "original text insertion target is unavailable",
                )
            })
    }
    async fn insert_final(&self, text: String) -> Result<InsertOutcome, BackendError> {
        if let Err(error) = self.restore_insertion_target() {
            return Err(error);
        }
        {
            tauri::async_runtime::spawn_blocking(move || {
                map_insert_status(crate::insertion::TextInserter::new().insert(&text))
            })
            .await
            .map_err(|error| {
                BackendError::new(
                    BackendErrorCode::Internal,
                    format!("join Tauri insertion task: {error}"),
                )
            })?
        }
    }
}
impl TextInsertionSession for TauriTextInsertionSession {
    fn finish(
        &self,
        final_text: String,
    ) -> BoxFuture<'static, Result<InsertOutcome, BackendError>> {
        let session = self.clone();
        Box::pin(async move {
            if session.finished.swap(true, Ordering::AcqRel) {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "text insertion session is already closed",
                ));
            }
            if final_text.is_empty() {
                return Ok(InsertOutcome::Inserted);
            }
            session.insert_final(final_text).await
        })
    }
    fn cancel(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        let session = self.clone();
        Box::pin(async move {
            session.finished.store(true, Ordering::Release);
            Ok(())
        })
    }
}
fn map_insert_status(status: crate::types::InsertStatus) -> Result<InsertOutcome, BackendError> {
    match status {
        crate::types::InsertStatus::Inserted => Ok(InsertOutcome::Inserted),
        crate::types::InsertStatus::CopiedFallback => Ok(InsertOutcome::CopiedFallback),
        // PasteSent 是持久化历史里兼容 Windows 时代的残留状态，macOS 运行时不产生。
        crate::types::InsertStatus::PasteSent
        | crate::types::InsertStatus::Failed
        | crate::types::InsertStatus::NotRequested => Err(BackendError::new(
            BackendErrorCode::Platform,
            "Tauri text insertion failed",
        )),
    }
}
struct TauriHostContextAdapter;
impl openless_core::HostContextAdapter for TauriHostContextAdapter {
    fn start_screen_ocr(&self) -> openless_core::ports::ScreenOcrStart {
        crate::screen_ocr::start()
    }
    fn capture(
        &self,
        include_cursor: bool,
    ) -> BoxFuture<'static, Result<openless_core::HostContextCapture, BackendError>> {
        Box::pin(async move {
            let front_app = crate::coordinator::capture_frontmost_app();
            let (cursor_context, cursor_status, cursor_reason) = if include_cursor {
                let probe = crate::host_document::probe_around_cursor(
                    crate::host_document::DEFAULT_BUDGET_CHARS,
                )
                .await;
                let status = host_document_status_string(probe.status);
                let reason = probe.reason.clone();
                let context = probe.window.as_ref().map(|window| {
                    openless_core::prompts::cursor_context_input(window.before(), window.after())
                });
                (context, Some(status), reason)
            } else {
                (None, None, None)
            };
            Ok(openless_core::HostContextCapture {
                front_app,
                cursor_context,
                cursor_status,
                cursor_reason,
            })
        })
    }
}
/// HostDocumentStatus → probe 用的机器可读字符串（与 HostDocumentReadResult 的
/// serde camelCase 序列化一致），core 侧 cursor_trace_from_capture 按此折叠。
fn host_document_status_string(status: crate::host_document::HostDocumentStatus) -> String {
    match status {
        crate::host_document::HostDocumentStatus::Ok => "ok",
        crate::host_document::HostDocumentStatus::Blocked => "blocked",
        crate::host_document::HostDocumentStatus::Unsupported => "unsupported",
        crate::host_document::HostDocumentStatus::Unavailable => "unavailable",
        crate::host_document::HostDocumentStatus::Timeout => "timeout",
    }
    .to_string()
}
pub(crate) struct TauriHostActions {
    app: AppHandleSlot,
}
impl TauriHostActions {
    pub(crate) fn new(app: AppHandleSlot) -> Self {
        Self { app }
    }
    fn app(&self) -> Result<AppHandle, BackendError> {
        self.app.lock().clone().ok_or_else(|| {
            BackendError::new(
                BackendErrorCode::InvalidState,
                "Tauri AppHandle is not bound yet",
            )
        })
    }
}
impl HostActions for TauriHostActions {
    fn request(&self, action: HostAction) -> Result<(), BackendError> {
        let app = self.app()?;
        match action {
            HostAction::ShowMain | HostAction::FocusMain => crate::show_main_window(&app),
            HostAction::ShowDictationFeedback | HostAction::HideDictationFeedback => {}
            HostAction::OpenSystemSettings(page) => {
                crate::commands::open_system_settings(page)
                    .map_err(|message| BackendError::new(BackendErrorCode::Platform, message))?;
            }
            HostAction::RequestRestart => app.restart(),
            HostAction::Notify(message) => {
                app.emit("core:notification", message)
                    .map_err(map_tauri_error)?;
            }
        }
        Ok(())
    }
}
fn map_tauri_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::new(BackendErrorCode::Platform, error.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_recording_preparation_failure_keeps_microphone_options_usable() {
        let mut options = Vec::new();
        for (archive_fails, mute_fails) in [(true, false), (false, true), (true, true)] {
            let archive_result = if archive_fails {
                Err("recordings directory cannot be created")
            } else {
                Ok(Some(PathBuf::from("fixture.wav")))
            };
            let (archive, mute) = prepare_recording_options(archive_result, true, || {
                if mute_fails {
                    Err("default render endpoint is unavailable".to_string())
                } else {
                    Ok(())
                }
            });
            options.push((archive.is_some(), mute.is_some()));
        }
        assert_eq!(
            options,
            vec![(false, true), (true, false), (false, false)],
            "optional effects must not prevent capture"
        );
    }
    #[test]
    fn optional_recording_preparation_preserves_guard_ownership_and_disabled_behavior() {
        struct MuteGuard(Arc<AtomicU64>);
        impl Drop for MuteGuard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let restored = Arc::new(AtomicU64::new(0));
        let path = PathBuf::from("fixture.wav");
        let (archive, mute) =
            prepare_recording_options(Ok::<_, String>(Some(path.clone())), true, || {
                Ok(MuteGuard(Arc::clone(&restored)))
            });
        assert_eq!(archive, Some(path.clone()));
        assert_eq!(restored.load(Ordering::SeqCst), 0);
        drop(mute);
        assert_eq!(restored.load(Ordering::SeqCst), 1);
        let (archive, mute) =
            prepare_recording_options(Ok::<_, String>(None), false, || -> Result<(), String> {
                panic!("disabled muting must not call the platform")
            });
        assert!(archive.is_none());
        assert!(mute.is_none());
        let (archive, mute) = prepare_recording_options(
            Ok::<_, String>(Some(path.clone())),
            false,
            || -> Result<(), String> { panic!("disabled muting must not call the platform") },
        );
        assert_eq!(archive, Some(path));
        assert!(mute.is_none());
    }
    #[test]
    fn native_callback_tasks_run_on_the_tauri_host_runtime() {
        use openless_core::TaskSpawner;
        let _host_runtime = tauri::async_runtime::handle();
        let (completed, completion) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            TauriTaskSpawner.spawn(Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                completed.send(()).unwrap();
            }));
        })
        .join()
        .unwrap();
        assert_eq!(
            completion.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(()),
            "native audio/cancellation work must not be dropped outside Tokio threads"
        );
    }
    struct IgnoreTextStreamSink;
    #[tokio::test]
    async fn fixed_qwen_runtime_reports_missing_model_files() {
        use openless_core::ModelRuntimeAdapter;
        let adapter = TauriLocalAsrRuntimeAdapter::new(TauriNativeAsrDependencies::new());
        let error = adapter
            .prepare(PathBuf::new(), Arc::new(|_| {}))
            .await
            .unwrap_err();
        assert!(
            error.message.contains("config.json"),
            "unexpected error: {}",
            error.message
        );
    }
    #[tokio::test]
    async fn qwen_session_audio_cancel_and_empty_finish_share_runtime() {
        let runtime = Arc::new(crate::asr::local::qwen3_mlx::Qwen3MlxRuntime::new());
        let provider = Arc::new(crate::asr::local::qwen3_mlx::Qwen3MlxAsr::new(
            Arc::clone(&runtime),
            "qwen3-asr-1.7b".into(),
            PathBuf::from("unused-fixture"),
            None,
        ));
        let session = TauriNativeTranscriptionSession {
            kind: TauriNativeTranscriptionSessionKind::Qwen3Mlx {
                provider: Arc::clone(&provider),
                runtime: Arc::clone(&runtime),
            },
            asr_call_label: openless_core::AsrCallLabel::new(
                "local-qwen3-mlx",
                Some("qwen3-asr-1.7b".into()),
            ),
            partials: Arc::new(IgnoreTextStreamSink),
            hotwords: Vec::new(),
            next_offset: Arc::new(AtomicU64::new(0)),
            released: Arc::new(AtomicBool::new(false)),
        };
        CoreAudioConsumer::consume_pcm_chunk(&session, &[0; 32]);
        assert_eq!(provider.buffer_duration_ms(), 1);
        session.cancel().await.unwrap();
        assert_eq!(provider.buffer_duration_ms(), 0);
        assert!(session.released.load(Ordering::Acquire));
        let output = session.finish().await.unwrap();
        assert_eq!(output.text, "");
        assert_eq!(output.duration_ms, 0);
        let TauriNativeTranscriptionSessionKind::Qwen3Mlx { runtime: held, .. } = &session.kind;
        assert!(Arc::ptr_eq(held, &runtime));
        assert!(
            !runtime
                .status_snapshot("qwen3-asr-1.7b")
                .await
                .runtime_ready
        );
    }
    impl TextStreamSink for IgnoreTextStreamSink {
        fn publish(&self, _chunk: TextStreamChunk) -> Result<(), BackendError> {
            Ok(())
        }
    }
    #[test]
    fn insertion_status_mapping_preserves_fallback_and_failure_semantics() {
        assert_eq!(
            map_insert_status(crate::types::InsertStatus::Inserted).unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            map_insert_status(crate::types::InsertStatus::CopiedFallback).unwrap(),
            InsertOutcome::CopiedFallback
        );
        assert_eq!(
            map_insert_status(crate::types::InsertStatus::Failed)
                .unwrap_err()
                .code,
            BackendErrorCode::Platform
        );
    }
    #[test]
    fn platform_permission_mapping_preserves_every_legacy_state() {
        let cases = [
            (
                crate::permissions::PermissionStatus::Granted,
                openless_core::PermissionState::Granted,
            ),
            (
                crate::permissions::PermissionStatus::Denied,
                openless_core::PermissionState::Denied,
            ),
            (
                crate::permissions::PermissionStatus::NotDetermined,
                openless_core::PermissionState::Unknown,
            ),
            (
                crate::permissions::PermissionStatus::Restricted,
                openless_core::PermissionState::Restricted,
            ),
            (
                crate::permissions::PermissionStatus::NotApplicable,
                openless_core::PermissionState::Unsupported,
            ),
            (
                crate::permissions::PermissionStatus::NoDevice,
                openless_core::PermissionState::NoDevice,
            ),
        ];
        for (legacy, core) in cases {
            assert_eq!(map_permission_state(legacy), core);
        }
    }
}
