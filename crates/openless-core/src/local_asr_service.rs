//! Shared Local ASR orchestration.
//!
//! Model/runtime policy and preference transactions live here. The host
//! Adapter only supplies native engines; model files are plain directories on
//! local disk (imported or copied by the user).
//!
//! 减法改造(纯本地):上游的 ModelStore 下载/镜像/远程目录能力已移除;
//! 模型通过本地导入放置,installed 状态由模型目录是否有文件判定。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::domains::{
    LocalAsrActivationRequest, LocalAsrActivationResult, LocalAsrApi, LocalAsrModel,
    LocalAsrRuntimeStatus, LocalAsrSettings, LocalAsrStorageSettings, LocalAsrTestResult,
};
use crate::errors::{BackendError, BackendErrorCode};
use crate::events::{BackendEventKind, BackendEventPublisher};
use crate::local_asr_catalog::{
    normalize_foundry_language_hint, normalize_sherpa_language_hint, FoundryRuntimeSource,
    LocalAsrRuntime, LocalAsrTarget,
};
use crate::types::PreferencesChange;
use crate::{PreferencesStore, UserPreferences};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeModelState {
    pub target: LocalAsrTarget,
    pub installed: bool,
    pub size_bytes: Option<u64>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageRebind {
    Applied,
    RestartRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAsrRuntimeLease {
    pub target: LocalAsrTarget,
    pub generation: u64,
}

pub type ModelPrepareProgressSink =
    Arc<dyn Fn(crate::events::LocalAsrPrepareProgress) + Send + Sync + 'static>;

fn unsupported<T>(operation: &'static str) -> BoxFuture<'static, Result<T, BackendError>> {
    Box::pin(async move {
        Err(BackendError::new(
            BackendErrorCode::Unsupported,
            format!("local ASR runtime does not support {operation}"),
        ))
    })
}

// ─────────────────────────── 纯本地模型目录布局 ───────────────────────────

/// Compute the on-disk directory of one local model from the models root.
pub fn local_model_dir(root: &Path, target: &LocalAsrTarget) -> PathBuf {
    match target.runtime {
        LocalAsrRuntime::Generic => root.join(target.model_id()),
        LocalAsrRuntime::Foundry => root.join("foundry-local"),
        LocalAsrRuntime::SherpaOnnx => root.join("sherpa-onnx").join(target.model_id()),
    }
}

fn directory_size_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| entry.metadata().ok())
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len())
                .sum()
        })
        .unwrap_or(0)
}

pub fn directory_has_model_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(|entry| entry.ok()).any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with('.'))
            })
        })
        .unwrap_or(false)
}

/// 静态本地模型目录(不含任何远程元数据;size 未知时为 None)。
fn catalog_models(runtime: LocalAsrRuntime) -> Vec<LocalAsrModel> {
    match runtime {
        LocalAsrRuntime::Generic => crate::LocalAsrModelId::all()
            .iter()
            .map(|model| LocalAsrModel {
                target: LocalAsrTarget::parse(runtime, model.as_str())
                    .expect("built-in generic model id"),
                display_name: model.as_str().to_string(),
                family: if model.is_qwen() { "qwen" } else { "whisper" }.to_string(),
                mode: None,
                languages: Vec::new(),
                installed: false,
                downloaded_bytes: 0,
                size_bytes: None,
            })
            .collect(),
        LocalAsrRuntime::Foundry => ["whisper-small", "whisper-medium", "whisper-large-v3-turbo"]
            .iter()
            .map(|alias| LocalAsrModel {
                target: LocalAsrTarget::parse(runtime, *alias).expect("built-in foundry alias"),
                display_name: (*alias).to_string(),
                family: "foundry".to_string(),
                mode: None,
                languages: Vec::new(),
                installed: false,
                downloaded_bytes: 0,
                size_bytes: None,
            })
            .collect(),
        LocalAsrRuntime::SherpaOnnx => [
            "sense-voice-small-zh",
            "paraformer-zh",
            "whisper-small-multi",
            "whisper-large-v3-multi",
            "qwen3-asr-0.6b-int8",
            "zipformer-bilingual-zh-en-streaming",
        ]
        .iter()
        .map(|alias| LocalAsrModel {
            target: LocalAsrTarget::parse(runtime, *alias).expect("built-in sherpa alias"),
            display_name: (*alias).to_string(),
            family: "sherpa".to_string(),
                mode: LocalAsrTarget::parse(runtime, *alias)
                    .ok()
                    .and_then(|target| target.sherpa_execution_mode())
                    .map(|mode| mode.as_str().to_string()),
            languages: Vec::new(),
            installed: false,
            downloaded_bytes: 0,
            size_bytes: None,
        })
        .collect(),
    }
}

/// Host seam for native Local ASR engines.
///
/// Defaults fail explicitly so a platform can implement only capabilities it
/// genuinely supports without reporting fake success.
pub trait ModelRuntimeAdapter: Send + Sync {
    fn engine_available(&self, _runtime: LocalAsrRuntime) -> bool {
        false
    }

    fn supports_model(&self, target: &LocalAsrTarget) -> bool {
        self.engine_available(target.runtime)
    }

    fn inspect_native_models(
        &self,
        _targets: Vec<LocalAsrTarget>,
    ) -> BoxFuture<'static, Result<Vec<NativeModelState>, BackendError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn native_model_dir(
        &self,
        _target: LocalAsrTarget,
        fallback: PathBuf,
    ) -> BoxFuture<'static, Result<PathBuf, BackendError>> {
        Box::pin(async move { Ok(fallback) })
    }

    fn delete_native_model(
        &self,
        _target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("native model deletion")
    }

    fn rebind_storage(
        &self,
        _models_root: PathBuf,
    ) -> BoxFuture<'static, Result<StorageRebind, BackendError>> {
        Box::pin(async { Ok(StorageRebind::Applied) })
    }

    fn runtime_status(
        &self,
        _settings: LocalAsrSettings,
        _model_dir: PathBuf,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        unsupported("runtime status")
    }

    fn prepare(
        &self,
        _target: LocalAsrTarget,
        _runtime_source: FoundryRuntimeSource,
        _model_dir: PathBuf,
        _progress: ModelPrepareProgressSink,
    ) -> BoxFuture<'static, Result<String, BackendError>> {
        unsupported("runtime preparation")
    }

    fn cancel_prepare(
        &self,
        _runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("prepare cancellation")
    }

    fn release(&self, _runtime: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("runtime release")
    }

    fn release_lease(
        &self,
        lease: LocalAsrRuntimeLease,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.release(lease.target.runtime)
    }

    /// Adopt an already loaded model before starting an activation transaction.
    /// A later ordinary preload/use must revoke this lease's cleanup authority.
    fn claim_lease(&self, _lease: LocalAsrRuntimeLease) {}

    fn preload(
        &self,
        _target: LocalAsrTarget,
        _model_dir: PathBuf,
        _provider_type: String,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        unsupported("runtime preload")
    }

    /// Activation-owned preload. Hosts with independent caches retain this
    /// identity so releasing an old lease cannot evict a newer use of the model.
    fn preload_lease(
        &self,
        lease: LocalAsrRuntimeLease,
        model_dir: PathBuf,
        provider_type: String,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.preload(lease.target, model_dir, provider_type)
    }

    fn test_model(
        &self,
        _target: LocalAsrTarget,
        _model_dir: PathBuf,
    ) -> BoxFuture<'static, Result<LocalAsrTestResult, BackendError>> {
        unsupported("model test")
    }

    fn invalidate_route(&self, _runtime: LocalAsrRuntime) {}
}

pub(crate) struct LocalAsrService {
    preferences: Arc<PreferencesStore>,
    runtime: Arc<dyn ModelRuntimeAdapter>,
    default_models_root: PathBuf,
    events: BackendEventPublisher,
    preferences_revision: Arc<AtomicU64>,
    activation_generation: Arc<AtomicU64>,
    activation_lock: Arc<tokio::sync::Mutex<()>>,
    active_lease: Arc<std::sync::Mutex<Option<LocalAsrRuntimeLease>>>,
}

impl LocalAsrService {
    pub(crate) fn new(
        preferences: Arc<PreferencesStore>,
        runtime: Arc<dyn ModelRuntimeAdapter>,
        default_models_root: PathBuf,
        events: BackendEventPublisher,
        preferences_revision: Arc<AtomicU64>,
        _credentials: Arc<dyn crate::credentials::CredentialStore>,
    ) -> Self {
        Self {
            preferences,
            runtime,
            default_models_root,
            events,
            preferences_revision,
            activation_generation: Arc::new(AtomicU64::new(0)),
            activation_lock: Arc::new(tokio::sync::Mutex::new(())),
            active_lease: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn publish_preferences(
        &self,
        update: impl FnOnce(&mut UserPreferences),
    ) -> Result<(), BackendError> {
        self.preferences.update(update)?;
        let revision = self.preferences_revision.fetch_add(1, Ordering::SeqCst) + 1;
        self.events.publish(
            None,
            BackendEventKind::PreferencesChanged(PreferencesChange { revision }),
        );
        Ok(())
    }

    fn effective_root(preferences: &UserPreferences, default_root: &Path) -> PathBuf {
        match Self::normalized_base_dir(Some(PathBuf::from(
            preferences.local_asr_models_base_dir.clone(),
        ))) {
            Ok(Some(base)) => base.join("OpenLess").join("models"),
            _ => default_root.to_path_buf(),
        }
    }

    fn active_model(preferences: &UserPreferences, runtime: LocalAsrRuntime) -> String {
        match runtime {
            LocalAsrRuntime::Generic => {
                if matches!(
                    preferences.active_asr_provider.as_str(),
                    "local-whisper" | "apple-whisper"
                ) {
                    preferences.local_whisper_active_model.clone()
                } else {
                    preferences.local_asr_active_model.clone()
                }
            }
            LocalAsrRuntime::Foundry => {
                LocalAsrTarget::parse(runtime, preferences.foundry_local_asr_model.clone())
                    .map(|target| target.model_id().to_string())
                    .unwrap_or_else(|_| runtime.default_model().to_string())
            }
            LocalAsrRuntime::SherpaOnnx => {
                LocalAsrTarget::parse(runtime, preferences.sherpa_onnx_model.clone())
                    .map(|target| target.model_id().to_string())
                    .unwrap_or_else(|_| runtime.default_model().to_string())
            }
        }
    }

    fn keep_loaded_secs(preferences: &UserPreferences, runtime: LocalAsrRuntime) -> u32 {
        match runtime {
            LocalAsrRuntime::Generic => preferences.local_asr_keep_loaded_secs,
            LocalAsrRuntime::Foundry => preferences.foundry_local_asr_keep_loaded_secs,
            LocalAsrRuntime::SherpaOnnx => preferences.sherpa_onnx_keep_loaded_secs,
        }
    }

    async fn runtime_status_snapshot(
        preferences: Arc<PreferencesStore>,
        adapter: Arc<dyn ModelRuntimeAdapter>,
        default_root: PathBuf,
        runtime: LocalAsrRuntime,
    ) -> Result<LocalAsrRuntimeStatus, BackendError> {
        let preferences = preferences.get();
        let active_model = Self::active_model(&preferences, runtime);
        let target = LocalAsrTarget::parse(runtime, active_model.clone())?;
        let root = Self::effective_root(&preferences, &default_root);
        let model_dir = local_model_dir(&root, &target);
        adapter
            .runtime_status(
                LocalAsrSettings {
                    runtime,
                    provider_id: runtime.provider_id().to_string(),
                    active_model,
                    models_base_dir: Self::normalized_base_dir(Some(PathBuf::from(
                        preferences.local_asr_models_base_dir.clone(),
                    )))?,
                    models_root_dir: root,
                    engine_available: adapter.engine_available(runtime),
                    language_hint: match runtime {
                        LocalAsrRuntime::Generic => None,
                        LocalAsrRuntime::Foundry => {
                            Some(preferences.foundry_local_asr_language_hint.clone())
                        }
                        LocalAsrRuntime::SherpaOnnx => {
                            Some(preferences.sherpa_onnx_language_hint.clone())
                        }
                    },
                    keep_loaded_secs: Self::keep_loaded_secs(&preferences, runtime),
                },
                model_dir,
            )
            .await
    }

    async fn publish_runtime_status(
        preferences: Arc<PreferencesStore>,
        adapter: Arc<dyn ModelRuntimeAdapter>,
        default_root: PathBuf,
        events: BackendEventPublisher,
        runtime: LocalAsrRuntime,
    ) {
        if let Ok(status) =
            Self::runtime_status_snapshot(preferences, adapter, default_root, runtime).await
        {
            events.publish(None, BackendEventKind::LocalAsrEngineChanged(status));
        }
    }

    fn normalized_base_dir(path: Option<PathBuf>) -> Result<Option<PathBuf>, BackendError> {
        match path {
            Some(path) if path.as_os_str().is_empty() => Ok(None),
            Some(path) if !path.is_absolute() => Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "local ASR model base directory must be absolute",
            )),
            path => Ok(path),
        }
    }

    fn storage_settings(&self) -> Result<LocalAsrStorageSettings, BackendError> {
        let preferences = self.preferences.get();
        let base_dir = Self::normalized_base_dir(Some(PathBuf::from(
            preferences.local_asr_models_base_dir.clone(),
        )))?;
        Ok(LocalAsrStorageSettings {
            is_default: base_dir.is_none(),
            models_base_dir: base_dir,
            models_root_dir: Self::effective_root(&preferences, &self.default_models_root),
            restart_required: false,
        })
    }

    fn prepared_model_dir(&self, target: &LocalAsrTarget) -> Result<PathBuf, BackendError> {
        let preferences = self.preferences.get();
        let root = Self::effective_root(&preferences, &self.default_models_root);
        let dir = local_model_dir(&root, target);
        if !directory_has_model_files(&dir) {
            return Err(BackendError::new(
                BackendErrorCode::InvalidState,
                "local ASR model directory is empty; import the model first",
            ));
        }
        Ok(dir)
    }

    fn target_for_active_preferences(preferences: &UserPreferences) -> Option<LocalAsrTarget> {
        let provider = preferences.active_asr_provider.as_str();
        let runtime = match provider {
            "local-qwen3" | "local-qwen3-c" | "local-qwen3-mlx" | "local-whisper"
            | "apple-whisper" => LocalAsrRuntime::Generic,
            "foundry-local" | "foundry-whisper" | "foundry-local-whisper" => {
                LocalAsrRuntime::Foundry
            }
            "sherpa-onnx" | "sherpa-onnx-local" => LocalAsrRuntime::SherpaOnnx,
            _ => return None,
        };
        let model = if matches!(provider, "local-whisper" | "apple-whisper") {
            preferences.local_whisper_active_model.clone()
        } else {
            Self::active_model(preferences, runtime)
        };
        LocalAsrTarget::parse(runtime, model).ok()
    }

    fn apply_activation(
        preferences: &mut UserPreferences,
        target: &LocalAsrTarget,
        provider_id: &str,
    ) {
        preferences.active_asr_provider = provider_id.to_string();
        match target.runtime {
            LocalAsrRuntime::Generic => {
                let model = crate::LocalAsrModelId::from_wire_id(target.model_id())
                    .expect("validated local ASR target");
                if model.is_whisper() {
                    preferences.local_whisper_active_model = target.model_id().to_string();
                } else {
                    preferences.local_asr_active_model = target.model_id().to_string();
                }
            }
            LocalAsrRuntime::Foundry => {
                preferences.foundry_local_asr_model = target.model_id().to_string();
            }
            LocalAsrRuntime::SherpaOnnx => {
                preferences.sherpa_onnx_model = target.model_id().to_string();
            }
        }
    }

    fn validate_activation_provider(
        target: &LocalAsrTarget,
        provider_type: &str,
    ) -> Result<(), BackendError> {
        let provider_matches = match target.runtime {
            LocalAsrRuntime::Generic => {
                let model = crate::LocalAsrModelId::from_wire_id(target.model_id())
                    .expect("validated local ASR target");
                if model.is_qwen() {
                    matches!(
                        provider_type,
                        "local-qwen3" | "local-qwen3-c" | "local-qwen3-mlx"
                    )
                } else {
                    matches!(provider_type, "local-whisper" | "apple-whisper")
                }
            }
            LocalAsrRuntime::Foundry => matches!(
                provider_type,
                "foundry-local" | "foundry-whisper" | "foundry-local-whisper"
            ),
            LocalAsrRuntime::SherpaOnnx => {
                matches!(provider_type, "sherpa-onnx" | "sherpa-onnx-local")
            }
        };
        if provider_matches {
            Ok(())
        } else {
            Err(BackendError::new(
                BackendErrorCode::InvalidArgument,
                "local ASR provider does not match the selected runtime and model",
            ))
        }
    }
}

async fn release_activation_lease(
    adapter: &Arc<dyn ModelRuntimeAdapter>,
    generation_clock: &Arc<AtomicU64>,
    operation_generation: u64,
    lease: LocalAsrRuntimeLease,
) -> Result<(), BackendError> {
    if generation_clock.load(Ordering::Acquire) != operation_generation {
        return Ok(());
    }
    adapter.release_lease(lease).await
}

async fn restore_activation_runtime(
    adapter: &Arc<dyn ModelRuntimeAdapter>,
    default_root: &Path,
    operation_generation: u64,
    requested: &LocalAsrTarget,
    previous: Option<&LocalAsrRuntimeLease>,
    previous_preferences: &UserPreferences,
    progress: ModelPrepareProgressSink,
) -> Vec<BackendError> {
    // Native Windows runtimes already replace their own model during prepare;
    // preserve their existing same-target rollback behavior. Generic cache
    // ownership must be restored even when only the activation owner changed.
    if requested.runtime != LocalAsrRuntime::Generic
        && previous.map(|lease| &lease.target) == Some(requested)
    {
        return Vec::new();
    }
    let mut errors = Vec::new();
    if let Err(error) = adapter
        .release_lease(LocalAsrRuntimeLease {
            target: requested.clone(),
            generation: operation_generation,
        })
        .await
    {
        errors.push(error);
    }
    if let Some(previous) = previous {
        let root = LocalAsrService::effective_root(previous_preferences, default_root);
        let model_dir = local_model_dir(&root, &previous.target);
        let source =
            FoundryRuntimeSource::from_legacy(&previous_preferences.foundry_local_runtime_source);
        match adapter
            .prepare(previous.target.clone(), source, model_dir.clone(), progress)
            .await
        {
            Ok(_) => {
                if let Err(error) = adapter
                    .preload_lease(
                        previous.clone(),
                        model_dir,
                        previous_preferences.active_asr_provider.clone(),
                    )
                    .await
                {
                    errors.push(error);
                }
            }
            Err(error) => errors.push(error),
        }
    }
    errors
}

fn activation_error(primary: BackendError, rollback: Vec<BackendError>) -> BackendError {
    if rollback.is_empty() {
        return primary;
    }
    BackendError::new(
        BackendErrorCode::Internal,
        format!(
            "{}; local ASR activation rollback failed: {}",
            primary.message,
            rollback
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ")
        ),
    )
}

fn restore_activation_preferences(
    preferences: &Arc<PreferencesStore>,
    previous_preferences: &UserPreferences,
    request: &LocalAsrActivationRequest,
    provider_type: &str,
) -> Vec<BackendError> {
    let mut errors = Vec::new();
    let mut applied = previous_preferences.clone();
    LocalAsrService::apply_activation(&mut applied, &request.target, provider_type);
    if let Err(error) = preferences.update(|current| {
        // Compensation owns only fields changed by this activation. A later
        // user edit (including to the same model field) must survive rollback.
        for (value, before, after) in [
            (
                &mut current.active_asr_provider,
                &previous_preferences.active_asr_provider,
                &applied.active_asr_provider,
            ),
            (
                &mut current.local_asr_active_model,
                &previous_preferences.local_asr_active_model,
                &applied.local_asr_active_model,
            ),
            (
                &mut current.local_whisper_active_model,
                &previous_preferences.local_whisper_active_model,
                &applied.local_whisper_active_model,
            ),
            (
                &mut current.foundry_local_asr_model,
                &previous_preferences.foundry_local_asr_model,
                &applied.foundry_local_asr_model,
            ),
            (
                &mut current.sherpa_onnx_model,
                &previous_preferences.sherpa_onnx_model,
                &applied.sherpa_onnx_model,
            ),
        ] {
            if before != after && value == after {
                *value = before.clone();
            }
        }
    }) {
        errors.push(error);
    }
    errors
}

impl LocalAsrApi for LocalAsrService {
    fn activate(
        &self,
        request: LocalAsrActivationRequest,
    ) -> BoxFuture<'static, Result<LocalAsrActivationResult, BackendError>> {
        if request.provider_id.trim().is_empty() {
            return Box::pin(async {
                Err(BackendError::new(
                    BackendErrorCode::InvalidArgument,
                    "local ASR provider id must not be blank",
                ))
            });
        }
        if !self.runtime.supports_model(&request.target) {
            return Box::pin(async {
                Err(BackendError::new(
                    BackendErrorCode::Unsupported,
                    "local ASR runtime does not support the selected model",
                ))
            });
        }
        let preferences = Arc::clone(&self.preferences);
        let adapter = Arc::clone(&self.runtime);
        let default_root = self.default_models_root.clone();
        let events = self.events.clone();
        let preferences_revision = Arc::clone(&self.preferences_revision);
        let generation_clock = Arc::clone(&self.activation_generation);
        let activation_lock = Arc::clone(&self.activation_lock);
        let active_lease = Arc::clone(&self.active_lease);
        Box::pin(async move {
            let _guard = activation_lock.lock().await;
            let previous_preferences = preferences.get();
            let provider_type = request.provider_id.clone();
            Self::validate_activation_provider(&request.target, &provider_type)?;
            let generation = generation_clock.fetch_add(1, Ordering::AcqRel) + 1;
            let previous_target = Self::target_for_active_preferences(&previous_preferences);
            let previous_lease = active_lease
                .lock()
                .expect("local ASR activation lease lock poisoned")
                .clone()
                .filter(|lease| previous_target.as_ref() == Some(&lease.target))
                .or_else(|| {
                    previous_target.map(|target| LocalAsrRuntimeLease {
                        target,
                        generation: generation.saturating_sub(1),
                    })
                });
            let root = Self::effective_root(&previous_preferences, &default_root);
            let model_dir = local_model_dir(&root, &request.target);
            if !directory_has_model_files(&model_dir) {
                return Err(BackendError::new(
                    BackendErrorCode::InvalidState,
                    "local ASR model directory is empty; import the model first",
                ));
            }
            if let Some(previous) = previous_lease.as_ref() {
                adapter.claim_lease(previous.clone());
            }
            let progress_events = events.clone();
            let progress: ModelPrepareProgressSink = Arc::new(move |progress| {
                progress_events.publish(None, BackendEventKind::LocalAsrPrepareProgress(progress));
            });
            let source =
                FoundryRuntimeSource::from_legacy(&previous_preferences.foundry_local_runtime_source);
            let prepared_model = match adapter
                .prepare(
                    request.target.clone(),
                    source,
                    model_dir.clone(),
                    Arc::clone(&progress),
                )
                .await
            {
                Ok(model) => model,
                Err(error) => {
                    let rollback = restore_activation_runtime(
                        &adapter,
                        &default_root,
                        generation,
                        &request.target,
                        previous_lease.as_ref(),
                        &previous_preferences,
                        progress,
                    )
                    .await;
                    return Err(activation_error(error, rollback));
                }
            };
            if let Err(error) = adapter
                .preload_lease(
                    LocalAsrRuntimeLease {
                        target: request.target.clone(),
                        generation,
                    },
                    model_dir.clone(),
                    provider_type.clone(),
                )
                .await
            {
                let rollback = restore_activation_runtime(
                    &adapter,
                    &default_root,
                    generation,
                    &request.target,
                    previous_lease.as_ref(),
                    &previous_preferences,
                    progress,
                )
                .await;
                return Err(activation_error(error, rollback));
            }

            // Finish every fallible native operation before committing the
            // selected provider to preferences.
            if let Some(previous_lease) = previous_lease.as_ref() {
                // Generic is a catalog group, not a single native cache: macOS
                // Qwen and Whisper have independent engines. Retire the old
                // model lease even when the replacement has the same runtime.
                if previous_lease.target.runtime != request.target.runtime
                    || (request.target.runtime == LocalAsrRuntime::Generic
                        && previous_lease.target != request.target)
                {
                    if let Err(error) = release_activation_lease(
                        &adapter,
                        &generation_clock,
                        generation,
                        previous_lease.clone(),
                    )
                    .await
                    {
                        let rollback = restore_activation_runtime(
                            &adapter,
                            &default_root,
                            generation,
                            &request.target,
                            Some(previous_lease),
                            &previous_preferences,
                            progress,
                        )
                        .await;
                        return Err(activation_error(error, rollback));
                    }
                }
            }

            let committed_previous = match preferences.update(|current| {
                let before = current.clone();
                Self::apply_activation(current, &request.target, &provider_type);
                before
            }) {
                Ok(before) => before,
                Err(error) => {
                    let rollback = restore_activation_runtime(
                        &adapter,
                        &default_root,
                        generation,
                        &request.target,
                        previous_lease.as_ref(),
                        &previous_preferences,
                        progress,
                    )
                    .await;
                    return Err(activation_error(error, rollback));
                }
            };
            let provider_id = match preferences.get().active_asr_provider.clone() {
                value if value == provider_type => value,
                current => {
                    // Another writer changed the provider while native
                    // preparation ran; roll the activation back.
                    let mut rollback = restore_activation_preferences(
                        &preferences,
                        &committed_previous,
                        &request,
                        &provider_type,
                    );
                    rollback.extend(
                        restore_activation_runtime(
                            &adapter,
                            &default_root,
                            generation,
                            &request.target,
                            previous_lease.as_ref(),
                            &previous_preferences,
                            progress,
                        )
                        .await,
                    );
                    return Err(activation_error(
                        BackendError::new(
                            BackendErrorCode::InvalidState,
                            format!(
                                "active ASR provider changed during activation: {current}"
                            ),
                        ),
                        rollback,
                    ));
                }
            };

            *active_lease
                .lock()
                .expect("local ASR activation lease lock poisoned") = Some(LocalAsrRuntimeLease {
                target: request.target.clone(),
                generation,
            });

            let revision = preferences_revision.fetch_add(1, Ordering::SeqCst) + 1;
            events.publish(
                None,
                BackendEventKind::PreferencesChanged(PreferencesChange { revision }),
            );
            adapter.invalidate_route(request.target.runtime);
            Self::publish_runtime_status(
                preferences,
                adapter,
                default_root,
                events,
                request.target.runtime,
            )
            .await;
            Ok(LocalAsrActivationResult {
                target: request.target,
                provider_id,
                generation,
                prepared_model,
            })
        })
    }

    fn settings(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<LocalAsrSettings, BackendError>> {
        let preferences = self.preferences.get();
        let root = Self::effective_root(&preferences, &self.default_models_root);
        let engine_available = self.runtime.engine_available(runtime);
        Box::pin(async move {
            let models_base_dir = Self::normalized_base_dir(Some(PathBuf::from(
                preferences.local_asr_models_base_dir.clone(),
            )))?;
            Ok(LocalAsrSettings {
                runtime,
                provider_id: runtime.provider_id().to_string(),
                active_model: Self::active_model(&preferences, runtime),
                models_base_dir,
                models_root_dir: root,
                engine_available,
                language_hint: match runtime {
                    LocalAsrRuntime::Generic => None,
                    LocalAsrRuntime::Foundry => {
                        Some(preferences.foundry_local_asr_language_hint.clone())
                    }
                    LocalAsrRuntime::SherpaOnnx => {
                        Some(preferences.sherpa_onnx_language_hint.clone())
                    }
                },
                keep_loaded_secs: Self::keep_loaded_secs(&preferences, runtime),
            })
        })
    }

    fn storage_settings(
        &self,
    ) -> BoxFuture<'static, Result<LocalAsrStorageSettings, BackendError>> {
        let result = self.storage_settings();
        Box::pin(async move { result })
    }

    fn list_models(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<Vec<LocalAsrModel>, BackendError>> {
        let adapter = Arc::clone(&self.runtime);
        let preferences = self.preferences.get();
        let root = Self::effective_root(&preferences, &self.default_models_root);
        Box::pin(async move {
            let mut models = catalog_models(runtime);
            models.retain(|model| adapter.supports_model(&model.target));
            for model in &mut models {
                let dir = local_model_dir(&root, &model.target);
                model.installed = directory_has_model_files(&dir);
                model.downloaded_bytes = if model.installed {
                    directory_size_bytes(&dir)
                } else {
                    0
                };
            }
            Ok(models)
        })
    }

    fn runtime_status(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        let preferences = Arc::clone(&self.preferences);
        let adapter = Arc::clone(&self.runtime);
        let default_root = self.default_models_root.clone();
        Box::pin(Self::runtime_status_snapshot(
            preferences,
            adapter,
            default_root,
            runtime,
        ))
    }

    fn set_models_base_dir(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<LocalAsrStorageSettings, BackendError>> {
        let next = match Self::normalized_base_dir(path) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let preferences = Arc::clone(&self.preferences);
        let events = self.events.clone();
        let revision = Arc::clone(&self.preferences_revision);
        let adapter = Arc::clone(&self.runtime);
        let default_models_root = self.default_models_root.clone();
        let activation_lock = Arc::clone(&self.activation_lock);
        Box::pin(async move {
            let _activation_guard = activation_lock.lock().await;
            // A second directory request may be queued behind an in-flight
            // relocation. Resolve the starting root only after owning the lock.
            let current_preferences = preferences.get();
            let current = Self::normalized_base_dir(Some(PathBuf::from(
                current_preferences.local_asr_models_base_dir.clone(),
            )))?;
            if current == next {
                let root = Self::effective_root(&current_preferences, &default_models_root);
                return Ok(LocalAsrStorageSettings {
                    is_default: next.is_none(),
                    models_base_dir: next,
                    models_root_dir: root,
                    restart_required: false,
                });
            }
            let next_root = next
                .as_ref()
                .map(|path| path.join("OpenLess").join("models"))
                .unwrap_or_else(|| default_models_root.clone());
            for runtime in [
                LocalAsrRuntime::Generic,
                LocalAsrRuntime::Foundry,
                LocalAsrRuntime::SherpaOnnx,
            ] {
                adapter.release(runtime).await?;
            }
            let next_preference = next
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Err(error) = preferences.update(|current| {
                current.local_asr_models_base_dir = next_preference.clone();
            }) {
                return Err(error);
            }
            let rebind = match adapter.rebind_storage(next_root).await {
                Ok(rebind) => rebind,
                Err(error) => {
                    if let Err(rollback) = preferences.update(|current| {
                        if current.local_asr_models_base_dir == next_preference {
                            current.local_asr_models_base_dir =
                                current_preferences.local_asr_models_base_dir.clone();
                        }
                    }) {
                        return Err(BackendError::new(
                            BackendErrorCode::Internal,
                            format!(
                                "rebind model storage failed: {}; preference rollback also failed: {}",
                                error.message, rollback.message
                            ),
                        ));
                    }
                    return Err(error);
                }
            };
            let revision = revision.fetch_add(1, Ordering::SeqCst) + 1;
            events.publish(
                None,
                BackendEventKind::PreferencesChanged(PreferencesChange { revision }),
            );
            let root = Self::effective_root(&preferences.get(), &default_models_root);
            Ok(LocalAsrStorageSettings {
                is_default: next.is_none(),
                models_base_dir: next,
                models_root_dir: root,
                restart_required: rebind == StorageRebind::RestartRequired,
            })
        })
    }

    fn set_active_model(
        &self,
        target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let runtime = target.runtime;
        let result = self.publish_preferences(|preferences| match runtime {
            LocalAsrRuntime::Generic => {
                let model = crate::LocalAsrModelId::from_wire_id(target.model_id())
                    .expect("validated target");
                if model.is_whisper() {
                    preferences.local_whisper_active_model = target.model_id().to_string();
                } else {
                    preferences.local_asr_active_model = target.model_id().to_string();
                }
            }
            LocalAsrRuntime::Foundry => {
                preferences.foundry_local_asr_model = target.model_id().to_string();
            }
            LocalAsrRuntime::SherpaOnnx => {
                preferences.sherpa_onnx_model = target.model_id().to_string();
            }
        });
        if result.is_ok() {
            self.runtime.invalidate_route(runtime);
        }
        let preferences = Arc::clone(&self.preferences);
        let adapter = Arc::clone(&self.runtime);
        let default_root = self.default_models_root.clone();
        let events = self.events.clone();
        Box::pin(async move {
            result?;
            Self::publish_runtime_status(preferences, adapter, default_root, events, runtime)
                .await;
            Ok(())
        })
    }

    fn set_language_hint(
        &self,
        runtime: LocalAsrRuntime,
        language_hint: String,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let normalized = match runtime {
            LocalAsrRuntime::Foundry => normalize_foundry_language_hint(&language_hint),
            LocalAsrRuntime::SherpaOnnx => normalize_sherpa_language_hint(&language_hint),
            LocalAsrRuntime::Generic => Err(BackendError::new(
                BackendErrorCode::Unsupported,
                "generic local ASR has no runtime language hint",
            )),
        };
        let normalized = match normalized {
            Ok(value) => value,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let result = self.publish_preferences(|preferences| match runtime {
            LocalAsrRuntime::Foundry => preferences.foundry_local_asr_language_hint = normalized,
            LocalAsrRuntime::SherpaOnnx => preferences.sherpa_onnx_language_hint = normalized,
            LocalAsrRuntime::Generic => unreachable!(),
        });
        Box::pin(async { result })
    }

    fn set_keep_loaded_secs(
        &self,
        runtime: LocalAsrRuntime,
        seconds: u32,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        let result = self.publish_preferences(|preferences| match runtime {
            LocalAsrRuntime::Generic => preferences.local_asr_keep_loaded_secs = seconds,
            LocalAsrRuntime::Foundry => preferences.foundry_local_asr_keep_loaded_secs = seconds,
            LocalAsrRuntime::SherpaOnnx => preferences.sherpa_onnx_keep_loaded_secs = seconds,
        });
        let preferences = Arc::clone(&self.preferences);
        let adapter = Arc::clone(&self.runtime);
        let default_root = self.default_models_root.clone();
        let events = self.events.clone();
        Box::pin(async move {
            result?;
            Self::publish_runtime_status(preferences, adapter, default_root, events, runtime)
                .await;
            Ok(())
        })
    }

    fn prepare(&self, target: LocalAsrTarget) -> BoxFuture<'static, Result<String, BackendError>> {
        let source =
            FoundryRuntimeSource::from_legacy(&self.preferences.get().foundry_local_runtime_source);
        let runtime = target.runtime;
        let adapter = Arc::clone(&self.runtime);
        let model_dir = match self.prepared_model_dir(&target) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let progress_events = self.events.clone();
        let progress: ModelPrepareProgressSink = Arc::new(move |progress| {
            progress_events.publish(None, BackendEventKind::LocalAsrPrepareProgress(progress));
        });
        let operation = adapter.prepare(target, source, model_dir, progress);
        let preferences = Arc::clone(&self.preferences);
        let default_root = self.default_models_root.clone();
        let events = self.events.clone();
        Box::pin(async move {
            let result = operation.await;
            // 成败都广播一次运行时状态：失败路径也要让 engine-changed 订阅方看到
            // 真实 error，而不是只靠轮询兜底。
            Self::publish_runtime_status(preferences, adapter, default_root, events, runtime)
                .await;
            result
        })
    }

    fn cancel_prepare(
        &self,
        runtime: LocalAsrRuntime,
    ) -> BoxFuture<'static, Result<(), BackendError>> {
        self.runtime.cancel_prepare(runtime)
    }

    fn release(&self, runtime: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        let adapter = Arc::clone(&self.runtime);
        let operation = adapter.release(runtime);
        let preferences = Arc::clone(&self.preferences);
        let events = self.events.clone();
        let default_root = self.default_models_root.clone();
        Box::pin(async move {
            operation.await?;
            Self::publish_runtime_status(preferences, adapter, default_root, events, runtime)
                .await;
            Ok(())
        })
    }

    fn preload(&self, runtime: LocalAsrRuntime) -> BoxFuture<'static, Result<(), BackendError>> {
        let preferences = self.preferences.get();
        let target = match LocalAsrTarget::parse(runtime, Self::active_model(&preferences, runtime))
        {
            Ok(target) => target,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let model_dir = match self.prepared_model_dir(&target) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.runtime
            .preload(target, model_dir, preferences.active_asr_provider)
    }

    fn delete_model(&self, target: LocalAsrTarget) -> BoxFuture<'static, Result<(), BackendError>> {
        let runtime = target.runtime;
        let adapter = Arc::clone(&self.runtime);
        let delete_target = target.clone();
        let preferences = self.preferences.get();
        let root = Self::effective_root(&preferences, &self.default_models_root);
        let dir = local_model_dir(&root, &delete_target);
        let operation = adapter.release(runtime);
        let preferences = Arc::clone(&self.preferences);
        let events = self.events.clone();
        let default_root = self.default_models_root.clone();
        Box::pin(async move {
            operation.await?;
            match adapter.delete_native_model(delete_target).await {
                Ok(()) => {}
                // 非原生管理的模型(如 sherpa 目录)由本地直接删除目录。
                Err(error) if error.code == BackendErrorCode::Unsupported => {}
                Err(error) => return Err(error),
            }
            std::fs::remove_dir_all(&dir).map_err(|error| {
                BackendError::new(
                    BackendErrorCode::Persistence,
                    format!("remove local ASR model directory failed: {error}"),
                )
            })?;
            Self::publish_runtime_status(preferences, adapter, default_root, events, runtime)
                .await;
            Ok(())
        })
    }

    fn model_dir(
        &self,
        target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<PathBuf, BackendError>> {
        let preferences = self.preferences.get();
        let root = Self::effective_root(&preferences, &self.default_models_root);
        let fallback = local_model_dir(&root, &target);
        self.runtime.native_model_dir(target, fallback)
    }

    fn test_model(
        &self,
        target: LocalAsrTarget,
    ) -> BoxFuture<'static, Result<LocalAsrTestResult, BackendError>> {
        let model_dir = match self.prepared_model_dir(&target) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.runtime.test_model(target, model_dir)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dir_layout_matches_runtime_family() {
        let root = Path::new("/data/models");
        let sherpa = LocalAsrTarget::parse(
            LocalAsrRuntime::SherpaOnnx,
            "sense-voice-small-zh",
        )
        .unwrap();
        assert_eq!(
            local_model_dir(root, &sherpa),
            PathBuf::from("/data/models/sherpa-onnx/sense-voice-small-zh")
        );
        let generic = LocalAsrTarget::parse(
            LocalAsrRuntime::Generic,
            "whisper-large-v3-turbo",
        )
        .unwrap();
        assert_eq!(
            local_model_dir(root, &generic),
            PathBuf::from("/data/models/whisper-large-v3-turbo")
        );
    }

    #[test]
    fn installed_detection_requires_non_hidden_files() {
        let dir = std::env::temp_dir().join(format!("openless-model-layout-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!directory_has_model_files(&dir));
        assert_eq!(directory_size_bytes(&dir), 0);
        std::fs::write(dir.join("model.onnx"), b"bytes").unwrap();
        assert!(directory_has_model_files(&dir));
        assert_eq!(directory_size_bytes(&dir), 5);
        // 隐藏的 sentinel/锁文件不算模型文件。
        std::fs::remove_file(dir.join("model.onnx")).unwrap();
        std::fs::write(dir.join(".openless-model-ready"), b"").unwrap();
        assert!(!directory_has_model_files(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_lists_every_sherpa_alias_as_offline_except_streaming() {
        let models = catalog_models(LocalAsrRuntime::SherpaOnnx);
        assert_eq!(models.len(), 6);
        for model in &models {
            let mode = model.mode.as_deref().expect("sherpa models carry a mode");
            let expected = if model.target.model_id() == "zipformer-bilingual-zh-en-streaming" {
                "online"
            } else {
                "offline"
            };
            assert_eq!(mode, expected);
        }
        assert!(models
            .iter()
            .any(|model| model.target.model_id() == "sense-voice-small-zh"));
    }
}
