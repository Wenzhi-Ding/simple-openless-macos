//! Fixed Qwen3-ASR runtime orchestration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::domains::{LocalAsrApi, LocalAsrRuntimeStatus};
use crate::errors::{BackendError, BackendErrorCode};
use crate::events::{BackendEventKind, BackendEventPublisher, LocalAsrPrepareProgress};
use crate::local_asr_catalog::LOCAL_QWEN3_MODEL_ID;
use crate::PreferencesStore;

pub type ModelPrepareProgressSink = Arc<dyn Fn(LocalAsrPrepareProgress) + Send + Sync + 'static>;

pub fn local_model_dir(root: &Path) -> PathBuf {
    root.join(LOCAL_QWEN3_MODEL_ID)
}

pub fn directory_has_model_files(dir: &Path) -> bool {
    dir.join("config.json").is_file()
        && std::fs::read_dir(dir)
            .map(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".safetensors"))
                })
            })
            .unwrap_or(false)
}

pub trait ModelRuntimeAdapter: Send + Sync {
    fn runtime_status(
        &self,
        model_dir: PathBuf,
        keep_loaded_secs: u32,
    ) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>>;

    fn prepare(
        &self,
        model_dir: PathBuf,
        progress: ModelPrepareProgressSink,
    ) -> BoxFuture<'static, Result<String, BackendError>>;

    fn cancel_prepare(&self) -> BoxFuture<'static, Result<(), BackendError>>;

    fn release(&self) -> BoxFuture<'static, Result<(), BackendError>>;

    fn preload(&self, model_dir: PathBuf) -> BoxFuture<'static, Result<(), BackendError>>;
}

pub(crate) struct LocalAsrService {
    preferences: Arc<PreferencesStore>,
    runtime: Arc<dyn ModelRuntimeAdapter>,
    default_models_root: PathBuf,
    events: BackendEventPublisher,
}

impl LocalAsrService {
    pub(crate) fn new(
        preferences: Arc<PreferencesStore>,
        runtime: Arc<dyn ModelRuntimeAdapter>,
        default_models_root: PathBuf,
        events: BackendEventPublisher,
        _preferences_revision: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            preferences,
            runtime,
            default_models_root,
            events,
        }
    }

    fn models_root(&self) -> PathBuf {
        let base = self.preferences.get().local_asr_models_base_dir;
        if base.trim().is_empty() {
            self.default_models_root.clone()
        } else {
            PathBuf::from(base).join("OpenLess").join("models")
        }
    }

    fn model_dir(&self) -> PathBuf {
        local_model_dir(&self.models_root())
    }
}

impl LocalAsrApi for LocalAsrService {
    fn runtime_status(&self) -> BoxFuture<'static, Result<LocalAsrRuntimeStatus, BackendError>> {
        self.runtime.runtime_status(
            self.model_dir(),
            self.preferences.get().local_asr_keep_loaded_secs,
        )
    }

    fn prepare(&self) -> BoxFuture<'static, Result<String, BackendError>> {
        let model_dir = self.model_dir();
        if !directory_has_model_files(&model_dir) {
            return Box::pin(async move {
                Err(BackendError::new(
                    BackendErrorCode::InvalidState,
                    format!(
                        "fixed Qwen3-ASR model is incomplete: {}",
                        model_dir.display()
                    ),
                ))
            });
        }
        let events = self.events.clone();
        let progress: ModelPrepareProgressSink = Arc::new(move |payload| {
            events.publish(None, BackendEventKind::LocalAsrPrepareProgress(payload));
        });
        let runtime = Arc::clone(&self.runtime);
        Box::pin(async move {
            let model = runtime.prepare(model_dir.clone(), progress).await?;
            runtime.preload(model_dir).await?;
            Ok(model)
        })
    }

    fn cancel_prepare(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.runtime.cancel_prepare()
    }

    fn release(&self) -> BoxFuture<'static, Result<(), BackendError>> {
        self.runtime.release()
    }
}
